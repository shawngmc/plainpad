#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod commands;
mod doc;
mod document;
mod gfx;
mod input;
mod launch;
mod linkcheck;
mod paint;
mod print;
mod search;
mod tabs;
mod ui;
mod view;
#[cfg(all(unix, not(target_os = "macos")))]
mod wl_drag;
/// Wayland-only; a no-op stand-in elsewhere.
#[cfg(not(all(unix, not(target_os = "macos"))))]
mod wl_drag {
    #[allow(dead_code)]
    pub enum Outcome {
        Dropped { surface: usize, x: f64, y: f64 },
        Elsewhere,
    }
    pub struct WlDrag;
    impl WlDrag {
        pub fn new(_: *mut std::ffi::c_void) -> Option<Self> {
            None
        }
        pub fn start(&mut self, _: *mut std::ffi::c_void) -> bool {
            false
        }
        pub fn poll(&mut self) -> Option<Outcome> {
            None
        }
    }
}

use cosmic_text::{Buffer, Cursor, Metrics, Motion, Shaping, Wrap};
use doc::{Encoding, Eol, Settings};
use gfx::{Canvas, Gfx, Rect, Theme};
use std::num::NonZeroU32;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use ui::{
    Choice, Cmd, DialogAction, FieldKey, FindAction, FindBar, FontDialog, MenuHit, MenuState, NavKey, Prompt, TabBar,
    TabHit,
};
use view::{View, font_attrs};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::{CursorIcon, Window, WindowId};

const APP: &str = "Plainpad";
const MAC: bool = cfg!(target_os = "macos");

#[derive(Default, Clone, Copy)]
struct Layout {
    menu: Rect,
    tabs: Rect,
    text: Rect,
    vbar: Rect,
    find: Rect,
    status: Rect,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Text,
    Thumb(f32),
    Tab,
}

/// What to do once the user answers a "save changes?" prompt.
#[derive(Clone, PartialEq, Debug)]
enum Pending {
    /// Close one tab.
    Close(usize),
    /// Close the current window.
    CloseWindow,
    /// Quit the app.
    Exit,
    /// Open this (suspicious) link if the user confirms.
    OpenLink(String),
}

/// One open document.
struct Tab {
    view: View,
    /// Shown as "Untitled N" until the tab has a path.
    untitled: usize,
    path: Option<PathBuf>,
    encoding: Encoding,
    eol: Eol,
    mixed_eol: bool,
    /// Encoding or line endings changed since the last save.
    meta_dirty: bool,
    /// "Don't Save" was chosen for this tab while closing its window or exiting.
    discard: bool,
}

impl Tab {
    fn new(view: View, untitled: usize) -> Self {
        Self {
            view,
            untitled,
            path: None,
            encoding: Encoding::Utf8,
            eol: Eol::platform(),
            mixed_eol: false,
            meta_dirty: false,
            discard: false,
        }
    }

    fn name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| {
            if self.untitled <= 1 { "Untitled".into() } else { format!("Untitled {}", self.untitled) }
        })
    }

    fn is_dirty(&self) -> bool {
        self.meta_dirty || self.view.doc.is_dirty()
    }

    /// Modified and not yet saved or explicitly discarded.
    fn needs_save(&self) -> bool {
        self.is_dirty() && !self.discard
    }

    /// An empty, unsaved, unmodified tab that opening a file may reuse.
    fn is_pristine(&self) -> bool {
        self.path.is_none() && !self.is_dirty() && self.view.doc.len_lines() == 1 && self.view.doc.line_len(0) == 0
    }
}

/// Per-window state. The window an event is for is swapped into `App::win`
/// before handling it, so most code works on `self.win` without knowing
/// how many windows there are.
struct Win {
    tabs: Vec<Tab>,
    active: usize,
    tabbar: TabBar,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    scale: f32,
    mouse: (f32, f32),
    mouse_prev: (f32, f32),
    drag: Drag,
    clicks: (Instant, (f32, f32), u8),
    menu: MenuState,
    find: FindBar,
    font_dlg: Option<FontDialog>,
    prompt: Option<Prompt>,
    pending: Option<Pending>,
    status_msg: Option<String>,
    hover_url: Option<(usize, Range<usize>)>,
    preedit: String,
    layout: Layout,
    title: String,
    cursor_icon: CursorIcon,
    focused: bool,
    needs_redraw: bool,
    /// Close this window once the current event has been handled.
    close: bool,
    /// The window's `wl_surface` pointer on Wayland (0 elsewhere), to match drop targets.
    wl_surface: usize,
}

impl Win {
    fn new(tabs: Vec<Tab>) -> Self {
        Win {
            tabs,
            active: 0,
            tabbar: TabBar::default(),
            window: None,
            surface: None,
            scale: 1.0,
            mouse: (0.0, 0.0),
            mouse_prev: (0.0, 0.0),
            drag: Drag::None,
            clicks: (Instant::now(), (0.0, 0.0), 0),
            menu: MenuState::default(),
            find: FindBar::default(),
            font_dlg: None,
            prompt: None,
            pending: None,
            status_msg: None,
            hover_url: None,
            preedit: String::new(),
            layout: Layout::default(),
            title: String::new(),
            cursor_icon: CursorIcon::Default,
            focused: true,
            needs_redraw: true,
            close: false,
            wl_surface: 0,
        }
    }

    fn id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|w| w.id())
    }

    fn redraw_later(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// The window's client area in screen coordinates, where the platform reports it (not on Wayland).
    fn screen_rect(&self) -> Option<Rect> {
        let w = self.window.as_ref()?;
        let p = w.inner_position().ok()?;
        let sz = w.inner_size();
        Some(Rect::new(p.x as f32, p.y as f32, sz.width as f32, sz.height as f32))
    }
}

/// A tab dragged out of its window, waiting for a new window to be created for it.
struct Spawn {
    tab: Tab,
    /// Where to put the new window's top-left corner, in screen pixels.
    pos: Option<PhysicalPosition<i32>>,
    size: PhysicalSize<u32>,
}

struct App {
    gfx: Gfx,
    settings: Settings,
    theme: Theme,
    zoom: f32,
    mods: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    /// The window currently being handled.
    win: Win,
    /// All other windows.
    others: Vec<Win>,
    spawn: Option<Spawn>,
    /// Wayland drag-and-drop, when running on Wayland.
    wl: Option<wl_drag::WlDrag>,
    /// The window and tab of a Wayland drag in progress.
    wl_src: Option<(WindowId, usize)>,
    exit: bool,
}

impl App {
    fn new(settings: Settings, files: Vec<PathBuf>) -> Self {
        let mut gfx = Gfx::new();
        let view = View::new(&mut gfx.fs, &settings.font_family, settings.wrap);
        let mut app = App {
            gfx,
            settings,
            theme: Theme::LIGHT,
            zoom: 1.0,
            mods: ModifiersState::empty(),
            clipboard: None,
            win: Win::new(vec![Tab::new(view, 1)]),
            others: Vec::new(),
            spawn: None,
            wl: None,
            wl_src: None,
            exit: false,
        };
        for p in files {
            if p.exists() {
                app.open_path(p);
            } else {
                // A new file to be created on first save, as Notepad does.
                let i = if app.win.tabs[app.win.active].is_pristine() { app.win.active } else { app.new_tab() };
                app.win.tabs[i].path = Some(p);
            }
        }
        app
    }

    fn primary(&self) -> bool {
        if MAC { self.mods.super_key() } else { self.mods.control_key() }
    }
}

impl App {
    /// Work that follows any event: quitting, creating a window for a
    /// detached tab, repainting, and dropping closed windows.
    fn after_event(&mut self, el: &ActiveEventLoop) {
        if self.exit {
            self.settings.save();
            el.exit();
            return;
        }
        if let Some(sp) = self.spawn.take() {
            let mut win = Win::new(vec![sp.tab]);
            self.open_window(el, &mut win, sp.pos, Some(sp.size));
            self.others.push(win);
        }
        if std::mem::take(&mut self.win.needs_redraw) {
            self.win.redraw_later();
        }
        // Drop closed windows; if the current one closed, another becomes current.
        self.others.retain(|w| !w.close);
        if self.win.close {
            match self.others.pop() {
                Some(w) => {
                    self.win = w;
                    self.gfx.set_ui_scale(self.win.scale);
                    self.win.redraw_later();
                }
                None => {
                    self.settings.save();
                    el.exit();
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    /// Releases everything that holds on to the windowing system's connection
    /// while it is still open. On Wayland, winit closes the display when the
    /// event loop ends, and the drag-and-drop queue and the drawing surfaces
    /// use that connection; dropping them afterwards would be a use-after-free.
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.wl = None;
        for w in std::iter::once(&mut self.win).chain(self.others.iter_mut()) {
            w.surface = None;
            w.window = None;
        }
        self.others.clear();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let Some(outcome) = self.wl.as_mut().and_then(|w| w.poll()) {
            self.finish_wl_drag(outcome);
            self.after_event(el);
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.window.is_some() {
            return;
        }
        let mut win = std::mem::replace(&mut self.win, Win::new(Vec::new()));
        self.open_window(el, &mut win, None, None);
        self.win = win;
        self.gfx.set_ui_scale(self.win.scale);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !self.focus_win(id) {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => {
                self.redraw();
                return;
            }
            WindowEvent::CloseRequested => self.request_close_window(),
            WindowEvent::Resized(sz) => {
                let l = sz.to_logical::<f64>(self.win.scale as f64);
                self.settings.win_w = l.width.round() as u32;
                self.settings.win_h = l.height.round() as u32;
                self.win.needs_redraw = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.win.scale = scale_factor as f32;
                self.gfx.set_ui_scale(self.win.scale);
                self.win.needs_redraw = true;
            }
            WindowEvent::ThemeChanged(t) => {
                self.theme = if t == winit::window::Theme::Dark { Theme::DARK } else { Theme::LIGHT };
                self.win.needs_redraw = true;
            }
            WindowEvent::Focused(f) => {
                self.win.focused = f;
                self.win.needs_redraw = true;
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                self.update_cursor_icon();
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => self.on_key(event),
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _) => self.win.preedit = text,
                    Ime::Commit(text) => {
                        self.win.preedit.clear();
                        if self.win.prompt.is_none() && self.win.font_dlg.is_none() {
                            if self.win.find.visible && self.win.find.focus {
                                self.win.find.field_mut().key(FieldKey::Insert(&text));
                            } else {
                                self.win.tabs[self.win.active].view.type_text(&text);
                                self.edited();
                            }
                        }
                    }
                    Ime::Enabled | Ime::Disabled => self.win.preedit.clear(),
                }
                self.win.needs_redraw = true;
            }
            WindowEvent::CursorMoved { position, .. } => self.on_mouse_move((position.x as f32, position.y as f32)),
            WindowEvent::CursorLeft { .. } => {
                if self.win.hover_url.take().is_some() {
                    self.win.needs_redraw = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::DroppedFile(path) => {
                self.open_path(path);
                self.win.needs_redraw = true;
            }
            _ => {}
        }
        self.after_event(el);
    }
}

fn main() {
    let settings = Settings::load();
    let files = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let el = EventLoop::new().expect("failed to create event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(settings, files);
    el.run_app(&mut app).expect("event loop error");
}
