#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod commands;
mod context;
mod doc;
mod document;
mod gfx;
mod input;
mod jobs;
mod launch;
mod linkcheck;
mod mem;
mod paint;
mod print;
mod recovery;
mod safety;
mod search;
mod tabs;
mod ui;
mod view;
#[cfg(all(unix, not(target_os = "macos")))]
mod wayland;
/// Wayland-only; a no-op stand-in elsewhere.
#[cfg(not(all(unix, not(target_os = "macos"))))]
mod wayland {
    pub struct Framebuffer;
    impl Framebuffer {
        pub fn draw(&mut self, _: u32, _: u32, _: impl FnOnce(&mut [u32])) -> bool {
            false
        }
    }
    #[allow(dead_code)]
    pub enum PasteStart {
        Ready(String),
        Pending,
        Empty,
    }
    #[allow(dead_code)]
    pub enum Outcome {
        Dropped { surface: usize, x: f64, y: f64 },
        Elsewhere,
    }
    pub struct Wayland;
    impl Wayland {
        pub fn new(_: *mut std::ffi::c_void, _: impl Fn() + Send + 'static) -> Option<Self> {
            None
        }
        pub fn start(&mut self, _: *mut std::ffi::c_void) -> bool {
            false
        }
        pub fn poll(&mut self) -> Option<Outcome> {
            None
        }
        pub fn set_clipboard(&mut self, _: String) -> bool {
            false
        }
        pub fn framebuffer(&self, _: *mut std::ffi::c_void) -> Option<Framebuffer> {
            None
        }
        pub fn request_paste(&mut self, _: impl FnOnce(Result<String, String>) + Send + 'static) -> PasteStart {
            PasteStart::Empty
        }
    }
}

use cosmic_text::{Buffer, Metrics, Motion, Shaping, Wrap};
use doc::{Encoding, Eol, Settings};
use gfx::{Canvas, Gfx, Rect, Theme};
use std::num::NonZeroU32;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::{
    Choice, Cmd, CtxHit, DialogAction, FieldKey, FindAction, FindBar, FontDialog, MenuHit, MenuState, NavKey, Prompt,
    TabBar, TabHit,
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
    /// Close the tab with this id.
    Close(u64),
    /// Close the current window.
    CloseWindow,
    /// Quit the app.
    Exit,
    /// Open this (suspicious) link if the user confirms.
    OpenLink(String),
    /// Close the tabs marked `closing` (Close Other Tabs, Close Tabs to the Right).
    CloseMarked,
    /// The tab's file changed on disk while it had unsaved changes: reload or keep?
    Reload(u64),
    /// The tab's file changed on disk since it was loaded: overwrite it when saving?
    Overwrite(u64),
    /// Open this very large file if the user confirms.
    OpenLarge(PathBuf),
}

fn next_tab_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
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
    /// Marked for closing by Close Other Tabs / Close Tabs to the Right.
    closing: bool,
    /// Stable identity, unaffected by tabs moving around.
    id: u64,
    /// The file's state on disk when last loaded or saved, to notice outside changes.
    disk: Option<safety::Stamp>,
    /// What this tab's crash-recovery snapshot contains, if it has one.
    snap: Option<safety::SnapKey>,
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
            closing: false,
            id: next_tab_id(),
            disk: None,
            snap: None,
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
    /// How frames reach the screen: our own low-memory presenter on Wayland,
    /// softbuffer elsewhere.
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    fb: Option<wayland::Framebuffer>,
    /// A frame was skipped because the compositor still held every buffer.
    redraw_deferred: bool,
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
    /// The open right-click menu.
    ctx: Option<ui::ContextMenu>,
    /// Where the text caret was last drawn (x, bottom), for opening the menu from the keyboard.
    caret: Option<(f32, f32)>,
    /// The tab to make active once a multi-tab close finishes.
    keep_tab: Option<u64>,
    /// The tab the open save prompt is about. Tabs are identified by id, not
    /// position: a tab dragged in from another window while the prompt is open
    /// shifts positions and changes the active tab.
    prompt_tab: Option<u64>,
}

impl Win {
    /// Whether a modal dialog is open (tabs shouldn't move in or out meanwhile).
    fn modal(&self) -> bool {
        self.prompt.is_some() || self.font_dlg.is_some()
    }

    /// Position of the tab with this id.
    fn tab_pos(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn new(tabs: Vec<Tab>) -> Self {
        Win {
            tabs,
            active: 0,
            tabbar: TabBar::default(),
            window: None,
            surface: None,
            fb: None,
            redraw_deferred: false,
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
            ctx: None,
            caret: None,
            keep_tab: None,
            prompt_tab: None,
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

/// Wakes the event loop from another thread: a Wayland tab drag has ended,
/// or a paste has finished (or timed out).
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Wake,
}

/// Where pasted text should go when it arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PasteTarget {
    /// At the cursor of the tab with this id, wherever that tab is by then.
    Editor { tab: u64 },
    /// Into a find-bar field (0 = find, 1 = replace) of this window.
    Find { win: WindowId, field: usize },
}

/// A paste waiting for its text.
struct PendingPaste {
    token: u64,
    target: PasteTarget,
}

/// Finished paste reads, by token, filled in from background threads.
type PasteResults = Arc<Mutex<Vec<(u64, Result<String, String>)>>>;

struct App {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    gfx: Gfx,
    settings: Settings,
    theme: Theme,
    zoom: f32,
    mods: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    /// Pastes waiting for their text, and results delivered by background threads.
    pastes: Vec<PendingPaste>,
    paste_results: PasteResults,
    next_paste: u64,
    /// Crash-recovery snapshots of unsaved documents.
    recovery: Option<recovery::Recovery>,
    /// Ids of tabs that have a snapshot on disk.
    snapshots: std::collections::HashSet<u64>,
    /// When the next autosave snapshot is due (the event loop sleeps until then).
    autosave_at: Option<Instant>,
    /// Loading, saving and searching big documents in the background.
    jobs: jobs::Jobs,
    /// The window currently being handled.
    win: Win,
    /// All other windows.
    others: Vec<Win>,
    spawn: Option<Spawn>,
    /// Native Wayland drag-and-drop and clipboard, when running on Wayland.
    wl: Option<wayland::Wayland>,
    /// The window and tab of a Wayland drag in progress.
    wl_src: Option<(WindowId, usize)>,
    exit: bool,
}

impl App {
    fn new(settings: Settings, files: Vec<PathBuf>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) -> Self {
        let mut gfx = Gfx::new();
        let view = View::new(&mut gfx.fs, &settings.font_family, settings.wrap);
        let mut app = App {
            proxy,
            gfx,
            settings,
            theme: Theme::LIGHT,
            zoom: 1.0,
            mods: ModifiersState::empty(),
            clipboard: None,
            recovery: recovery::Recovery::start(),
            snapshots: Default::default(),
            autosave_at: None,
            jobs: Default::default(),
            pastes: Vec::new(),
            paste_results: PasteResults::default(),
            next_paste: 0,
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
        app.restore_recovered();
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
        self.autosave_check();
        if let Some(sp) = self.spawn.take() {
            let mut win = Win::new(vec![sp.tab]);
            self.open_window(el, &mut win, sp.pos, Some(sp.size));
            self.others.push(win);
        }
        self.cancel_orphan_jobs();
        // While background work runs, redraw its progress now and then.
        if self.jobs.is_empty() {
            self.jobs.tick_at = None;
        } else if self.jobs.tick_at.is_none() {
            self.jobs.tick_at = Some(Instant::now() + jobs::TICK);
        }
        // Sleep until the next timer, or indefinitely if nothing is pending.
        el.set_control_flow(match self.autosave_at.into_iter().chain(self.jobs.tick_at).min() {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
        if std::mem::take(&mut self.win.needs_redraw) {
            self.win.redraw_later();
        }
        // Drop closed windows; if the current one closed, another becomes current.
        let before = self.others.len();
        self.others.retain(|w| !w.close);
        if self.others.len() != before || self.win.close {
            crate::mem::trim();
        }
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

impl ApplicationHandler<UserEvent> for App {
    /// Releases everything that holds on to the windowing system's connection
    /// while it is still open. On Wayland, winit closes the display when the
    /// event loop ends, and the drag-and-drop queue and the drawing surfaces
    /// use that connection; dropping them afterwards would be a use-after-free.
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Frame buffers first (they send requests), then the Wayland module (stops its threads).
        for w in std::iter::once(&mut self.win).chain(self.others.iter_mut()) {
            w.surface = None;
            w.fb = None;
            w.window = None;
        }
        self.others.clear();
        self.wl = None;
        // A normal exit: every tab was saved or discarded, so the snapshots go.
        self.recovery = None;
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            let autosave = self.autosave_at.is_some_and(|t| now >= t);
            let tick = self.jobs.tick_at.is_some_and(|t| now >= t);
            if autosave {
                self.autosave_now();
            }
            if tick {
                self.tick();
            }
            if autosave || tick {
                self.after_event(el);
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _: UserEvent) {
        if let Some(outcome) = self.wl.as_mut().and_then(|w| w.poll()) {
            self.finish_wl_drag(outcome);
        }
        self.finish_pastes();
        self.finish_jobs();
        // A frame skipped for lack of a free buffer can be drawn now.
        for w in std::iter::once(&mut self.win).chain(self.others.iter_mut()) {
            if std::mem::take(&mut w.redraw_deferred) {
                w.redraw_later();
            }
        }
        self.after_event(el);
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
                if f {
                    self.check_disk();
                }
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
    let el = EventLoop::<UserEvent>::with_user_event().build().expect("failed to create event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(settings, files, el.create_proxy());
    el.run_app(&mut app).expect("event loop error");
}
