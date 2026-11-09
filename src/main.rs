#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod doc;
mod document;
mod gfx;
mod print;
mod search;
mod ui;
mod view;

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
use view::{font_attrs, View};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
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
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pending {
    /// Close one tab.
    Close(usize),
    /// Close the current window.
    CloseWindow,
    /// Quit the app.
    Exit,
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
        Self { view, untitled, path: None, encoding: Encoding::Utf8, eol: Eol::platform(), mixed_eol: false, meta_dirty: false, discard: false }
    }

    fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| if self.untitled <= 1 { "Untitled".into() } else { format!("Untitled {}", self.untitled) })
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

    // ---------------------------------------------------------------- tabs & documents

    fn tab(&self) -> &Tab {
        &self.win.tabs[self.win.active]
    }

    fn doc_name(&self) -> String {
        self.tab().name()
    }

    fn activate(&mut self, i: usize) {
        if i >= self.win.tabs.len() {
            return;
        }
        if i != self.win.active {
            self.win.status_msg = None;
        }
        self.win.active = i;
        self.win.hover_url = None;
        self.win.drag = if self.win.drag == Drag::Tab { Drag::Tab } else { Drag::None };
        self.win.tabbar.reveal = true;
    }

    /// Opens an empty tab after the current one and switches to it.
    /// Lowest "Untitled N" number not used by an open untitled tab.
    fn next_untitled(&self) -> usize {
        (1..).find(|n| !self.win.tabs.iter().any(|t| t.path.is_none() && t.untitled == *n)).unwrap()
    }

    fn new_tab(&mut self) -> usize {
        let view = View::new(&mut self.gfx.fs, &self.settings.font_family, self.settings.wrap);
        let i = (self.win.active + 1).min(self.win.tabs.len());
        let n = self.next_untitled();
        self.win.tabs.insert(i, Tab::new(view, n));
        self.activate(i);
        i
    }

    /// Opens `path` in a tab: switches to it if already open, reuses an
    /// untouched empty tab, or opens a new one.
    fn open_path(&mut self, path: PathBuf) {
        let canon = std::fs::canonicalize(&path).ok();
        let same = |p: &PathBuf| *p == path || (canon.is_some() && std::fs::canonicalize(p).ok() == canon);
        if let Some(i) = self.win.tabs.iter().position(|t| t.path.as_ref().is_some_and(same)) {
            self.activate(i);
            return;
        }
        match doc::load(&path) {
            Ok(l) => {
                let i = if self.tab().is_pristine() { self.win.active } else { self.new_tab() };
                let t = &mut self.win.tabs[i];
                t.view.set_document(document::Document::from_rope(l.rope));
                t.encoding = l.encoding;
                t.eol = l.eol;
                t.mixed_eol = l.mixed_eol;
                t.meta_dirty = false;
                t.path = Some(path);
                self.activate(i);
                self.win.status_msg = l
                    .mixed_eol
                    .then(|| format!("Mixed line endings; will be saved as {}", l.eol.label()));
            }
            Err(e) => self.win.status_msg = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self.tab().path.as_ref().and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()) {
            d = d.set_directory(dir);
        }
        if let Some(w) = &self.win.window {
            d = d.set_parent(&**w);
        }
        d
    }

    fn open_dialog(&mut self) {
        for p in self.dialog().set_title("Open").pick_files().unwrap_or_default() {
            self.open_path(p);
        }
    }

    fn save(&mut self) -> bool {
        match self.tab().path.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let name = self.tab().path.as_ref().map_or("Untitled.txt".into(), |_| self.doc_name());
        match self.dialog().set_title("Save As").set_file_name(name).save_file() {
            Some(p) => self.save_to(p),
            None => false,
        }
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        let t = &mut self.win.tabs[self.win.active];
        let text = t.view.doc.to_string_eol(t.eol.as_str());
        let bytes = match doc::encode(&text, t.encoding) {
            Ok(b) => b,
            Err(c) => {
                self.win.status_msg =
                    Some(format!("'{c}' can't be stored as {}; saved as UTF-8 instead", t.encoding.label()));
                t.encoding = Encoding::Utf8;
                text.into_bytes()
            }
        };
        match doc::save(&path, &bytes) {
            Ok(()) => {
                t.path = Some(path);
                t.view.doc.mark_saved();
                t.meta_dirty = false;
                t.mixed_eol = false;
                true
            }
            Err(e) => {
                self.win.status_msg = Some(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn rename(&mut self) {
        let Some(old) = self.tab().path.clone().filter(|p| p.exists()) else {
            self.save_as();
            return;
        };
        let Some(new) = self.dialog().set_title("Rename").set_file_name(self.doc_name()).save_file() else {
            return;
        };
        if new == old {
            return;
        }
        match std::fs::rename(&old, &new) {
            Ok(()) => {
                self.win.tabs[self.win.active].path = Some(new);
                self.win.status_msg = Some(format!("Renamed to {}", self.doc_name()));
            }
            Err(e) => self.win.status_msg = Some(format!("Could not rename: {e}")),
        }
    }

    fn print(&mut self) {
        let title = self.doc_name();
        let doc = &self.win.tabs[self.win.active].view.doc;
        let line = |i: usize| doc.line(i).into_owned();
        let job = print::Job {
            lines: doc.len_lines(),
            line: &line,
            family: &self.settings.font_family,
            font_pt: self.settings.font_pt,
        };
        // A throwaway glyph cache: print-resolution glyphs would otherwise bloat the screen cache.
        let mut cache = cosmic_text::SwashCache::new();
        let r = print::print(&mut self.gfx.fs, &mut cache, &job, &title);
        self.win.status_msg = Some(r.unwrap_or_else(|e| e));
    }

    fn ask_save(&mut self, then: Pending) {
        self.win.prompt = Some(Prompt::new(format!("Do you want to save changes to {}?", self.doc_name())));
        self.win.pending = Some(then);
    }

    /// Removes tab `i` from the current window and returns it. If that empties
    /// the window, it gets a fresh tab, and if other windows exist it closes.
    fn take_tab(&mut self, i: usize) -> Tab {
        let t = self.win.tabs.remove(i);
        if self.win.tabs.is_empty() {
            let view = View::new(&mut self.gfx.fs, &self.settings.font_family, self.settings.wrap);
            self.win.tabs.push(Tab::new(view, 1));
            self.win.close = !self.others.is_empty();
        }
        let a = if i < self.win.active { self.win.active - 1 } else { self.win.active.min(self.win.tabs.len() - 1) };
        self.win.active = usize::MAX; // force activate() to treat this as a switch
        self.activate(a);
        t
    }

    /// Closes tab `i`, first asking to save it unless `force`. Returns true if it closed.
    fn close_tab(&mut self, i: usize, force: bool) -> bool {
        if !force && self.win.tabs[i].is_dirty() {
            self.activate(i);
            self.ask_save(Pending::Close(i));
            return false;
        }
        self.take_tab(i);
        true
    }

    /// Makes `others[k]` the current window (the old current one goes into `others`).
    fn swap_to(&mut self, k: usize) {
        std::mem::swap(&mut self.win, &mut self.others[k]);
        self.gfx.set_ui_scale(self.win.scale);
    }

    /// Makes the window with this id current. Returns false if it isn't one of ours.
    fn focus_win(&mut self, id: WindowId) -> bool {
        if self.win.id() == Some(id) {
            return true;
        }
        match self.others.iter().position(|w| w.id() == Some(id)) {
            Some(k) => {
                self.swap_to(k);
                true
            }
            None => false,
        }
    }

    /// Quits once every modified tab in every window has been saved or
    /// discarded, asking about each in turn (switching windows as needed).
    fn request_exit(&mut self) {
        if self.win.tabs.iter().all(|t| !t.needs_save())
            && let Some(k) = self.others.iter().position(|w| w.tabs.iter().any(Tab::needs_save))
        {
            self.swap_to(k);
            if let Some(w) = &self.win.window {
                w.focus_window();
            }
        }
        match self.win.tabs.iter().position(Tab::needs_save) {
            Some(i) => {
                self.activate(i);
                self.ask_save(Pending::Exit);
            }
            None => self.exit = true,
        }
    }

    /// Closes the current window once its modified tabs are saved or discarded.
    /// Closing the last window quits.
    fn request_close_window(&mut self) {
        match self.win.tabs.iter().position(Tab::needs_save) {
            Some(i) => {
                self.activate(i);
                self.ask_save(Pending::CloseWindow);
            }
            None if self.others.is_empty() => self.exit = true,
            None => self.win.close = true,
        }
    }

    fn choose(&mut self, c: Choice) {
        self.win.prompt = None;
        let Some(pending) = self.win.pending.take() else { return };
        let i = self.win.active;
        match c {
            Choice::Save if !self.save() => {}
            Choice::Save | Choice::Discard => {
                if c == Choice::Discard {
                    self.win.tabs[i].discard = true;
                }
                match pending {
                    Pending::Close(i) => {
                        self.close_tab(i, true);
                    }
                    Pending::CloseWindow => self.request_close_window(),
                    Pending::Exit => self.request_exit(),
                }
            }
            Choice::Cancel => {
                for t in self.win.tabs.iter_mut().chain(self.others.iter_mut().flat_map(|w| w.tabs.iter_mut())) {
                    t.discard = false;
                }
            }
        }
    }

    /// Finishes dragging a tab. Dropped well outside the tab bar, it moves into
    /// the window under the pointer, or into a new window there.
    fn finish_tab_drag(&mut self) {
        let (p, s, tabs) = (self.win.mouse, self.win.scale, self.win.layout.tabs);
        let size = self.win.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
        let outside = p.1 < tabs.y - 20.0 * s
            || p.1 > tabs.bottom() + 40.0 * s
            || p.0 < 0.0
            || p.0 > size.width as f32;
        if !outside {
            return;
        }
        let screen = self.win.screen_rect().map(|r| (r.x + p.0, r.y + p.1));
        let target = screen.and_then(|sp| self.others.iter().position(|w| w.screen_rect().is_some_and(|r| r.contains(sp))));
        if let Some(k) = target {
            let tab = self.take_tab(self.win.active);
            let w = &mut self.others[k];
            w.tabs.push(tab);
            w.active = w.tabs.len() - 1;
            w.tabbar.reveal = true;
            w.redraw_later();
            if let Some(win) = &w.window {
                win.focus_window();
            }
        } else if self.win.tabs.len() > 1 {
            let tab = self.take_tab(self.win.active);
            // Put the new window so the pointer sits over its first tab.
            let pos = screen.map(|(x, y)| PhysicalPosition::new((x - 60.0 * s) as i32, (y - 40.0 * s) as i32));
            self.spawn = Some(Spawn { tab, pos, size });
        }
    }

    /// Creates an OS window for `win`.
    fn open_window(&mut self, el: &ActiveEventLoop, win: &mut Win, pos: Option<PhysicalPosition<i32>>, size: Option<PhysicalSize<u32>>) {
        let mut attrs = Window::default_attributes().with_title(APP).with_min_inner_size(LogicalSize::new(320, 200));
        attrs = match size {
            Some(sz) => attrs.with_inner_size(sz),
            None => attrs.with_inner_size(LogicalSize::new(self.settings.win_w.max(320), self.settings.win_h.max(200))),
        };
        if let Some(p) = pos {
            attrs = attrs.with_position(p);
        }
        let w = Rc::new(el.create_window(attrs).expect("failed to create window"));
        let ctx = softbuffer::Context::new(w.clone()).expect("failed to create graphics context");
        let surface = softbuffer::Surface::new(&ctx, w.clone()).expect("failed to create surface");
        w.set_ime_allowed(true);
        win.scale = w.scale_factor() as f32;
        self.theme = if w.theme() == Some(winit::window::Theme::Dark) { Theme::DARK } else { Theme::LIGHT };
        win.window = Some(w.clone());
        win.surface = Some(surface);
        win.tabbar.reveal = true;
        w.request_redraw();
    }

    // ---------------------------------------------------------------- editing

    fn edited(&mut self) {
        self.win.status_msg = None;
        self.win.hover_url = None;
    }

    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    fn set_clipboard(&mut self, text: String) {
        let text = if cfg!(windows) { text.replace('\n', "\r\n") } else { text };
        if let Some(cb) = self.clipboard() {
            let _ = cb.set_text(text);
        }
    }

    fn get_clipboard(&mut self) -> Option<String> {
        self.clipboard().and_then(|cb| cb.get_text().ok())
    }

    fn copy(&mut self) {
        if let Some(t) = self.win.tabs[self.win.active].view.selected_text() {
            self.set_clipboard(t);
        }
    }

    // ---------------------------------------------------------------- find

    fn open_find(&mut self, replace: bool) {
        self.win.find.visible = true;
        self.win.find.replace |= replace;
        self.win.find.focus = true;
        self.win.find.field = 0;
        let sel = self.win.tabs[self.win.active].view.selection().filter(|(a, z)| a.line == z.line);
        if let Some(sel) = sel.and_then(|_| self.win.tabs[self.win.active].view.selected_text()) {
            self.win.find.find.set(&sel);
        } else {
            self.win.find.find.all = true;
        }
    }

    fn find_next(&mut self, forward: bool) -> bool {
        let needle = self.win.find.find.text.clone();
        if needle.is_empty() {
            self.open_find(false);
            return false;
        }
        let case = self.win.find.case;
        let cur = self.win.tabs[self.win.active].view.cursor;
        let (start, end) = self.win.tabs[self.win.active].view.selection().unwrap_or((cur, cur));
        let doc = &self.win.tabs[self.win.active].view.doc;
        let n = doc.len_lines();
        let mut found = None;
        for k in 0..=n {
            if forward {
                let li = (end.line + k) % n;
                let from = if k == 0 { end.index } else { 0 };
                if let Some(r) = search::find(&doc.line(li), &needle, case, from) {
                    found = Some((li, r));
                    break;
                }
            } else {
                let li = (start.line + n * 2 - k) % n;
                let hay = doc.line(li);
                let before = if k == 0 { start.index } else { hay.len() };
                if let Some(r) = search::rfind(&hay, &needle, case, before) {
                    found = Some((li, r));
                    break;
                }
            }
        }
        match found {
            Some((li, r)) => {
                self.win.tabs[self.win.active].view.set_selection(Cursor::new(li, r.start), Cursor::new(li, r.end));
                self.win.status_msg = None;
                true
            }
            None => {
                self.win.status_msg = Some(format!("Cannot find \"{needle}\""));
                false
            }
        }
    }

    fn replace_one(&mut self) {
        let needle = self.win.find.find.text.clone();
        let repl = self.win.find.repl.text.clone();
        let sel = self.win.tabs[self.win.active].view.selected_text().unwrap_or_default();
        if !sel.is_empty() && search::find(&sel, &needle, self.win.find.case, 0) == Some(0..sel.len()) {
            self.win.tabs[self.win.active].view.replace_selection(&repl, false);
            self.edited();
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        let needle = self.win.find.find.text.clone();
        let repl = self.win.find.repl.text.clone();
        if needle.is_empty() {
            return;
        }
        let case = self.win.find.case;
        let doc = &self.win.tabs[self.win.active].view.doc;
        let mut ranges = Vec::new();
        for li in 0..doc.len_lines() {
            let line = doc.line(li);
            let mut from = 0;
            while let Some(r) = search::find(&line, &needle, case, from) {
                from = r.end;
                ranges.push((Cursor::new(li, r.start), Cursor::new(li, r.end)));
            }
        }
        if ranges.is_empty() {
            self.win.status_msg = Some(format!("Cannot find \"{needle}\""));
            return;
        }
        self.win.tabs[self.win.active].view.replace_many(&ranges, &repl);
        self.edited();
        self.win.status_msg = Some(format!("Replaced {} occurrence(s)", ranges.len()));
    }

    fn find_action(&mut self, a: FindAction) {
        match a {
            FindAction::Next => {
                self.find_next(true);
            }
            FindAction::Prev => {
                self.find_next(false);
            }
            FindAction::Replace => self.replace_one(),
            FindAction::ReplaceAll => self.replace_all(),
            FindAction::ToggleCase => self.win.find.case = !self.win.find.case,
            FindAction::ToggleReplace => {
                self.win.find.replace = !self.win.find.replace;
                self.win.find.field = 0;
            }
            FindAction::Close => {
                self.win.find.visible = false;
                self.win.find.focus = false;
            }
            FindAction::FocusFind | FindAction::FocusReplace => {
                self.win.find.focus = true;
                self.win.find.field = (a == FindAction::FocusReplace) as usize;
            }
        }
    }

    // ---------------------------------------------------------------- commands

    fn run(&mut self, cmd: Cmd) {
        self.win.menu.close();
        if matches!(cmd, Cmd::WordWrap | Cmd::StatusBar | Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset) {
            // Shared settings: other windows pick them up when they next draw.
            for w in &self.others {
                w.redraw_later();
            }
        }
        match cmd {
            Cmd::New => {
                self.new_tab();
            }
            Cmd::Open => self.open_dialog(),
            Cmd::Exit => self.request_exit(),
            Cmd::CloseTab => {
                self.close_tab(self.win.active, false);
            }
            Cmd::NextTab => self.activate((self.win.active + 1) % self.win.tabs.len()),
            Cmd::PrevTab => self.activate((self.win.active + self.win.tabs.len() - 1) % self.win.tabs.len()),
            Cmd::SelectTab(n) => {
                let i = if n >= 9 { self.win.tabs.len() - 1 } else { n as usize - 1 };
                self.activate(i);
            }
            Cmd::Save => {
                self.save();
            }
            Cmd::SaveAs => {
                self.save_as();
            }
            Cmd::Rename => self.rename(),
            Cmd::Print => self.print(),
            Cmd::Undo => {
                self.win.tabs[self.win.active].view.undo();
                self.edited();
            }
            Cmd::Redo => {
                self.win.tabs[self.win.active].view.redo();
                self.edited();
            }
            Cmd::Cut => {
                self.copy();
                self.win.tabs[self.win.active].view.delete_selection();
                self.edited();
            }
            Cmd::Copy => self.copy(),
            Cmd::Paste => {
                if let Some(t) = self.get_clipboard() {
                    self.win.tabs[self.win.active].view.replace_selection(&t, false);
                    self.edited();
                }
            }
            Cmd::Delete => {
                self.win.tabs[self.win.active].view.delete();
                self.edited();
            }
            Cmd::SelectAll => self.win.tabs[self.win.active].view.select_all(),
            Cmd::Find => self.open_find(false),
            Cmd::Replace => self.open_find(true),
            Cmd::FindNext => {
                self.find_next(true);
            }
            Cmd::FindPrev => {
                self.find_next(false);
            }
            Cmd::WordWrap => {
                self.settings.wrap = !self.settings.wrap;
                self.settings.save();
            }
            Cmd::Font => {
                let mut fams: Vec<String> =
                    self.gfx.fs.db().faces().filter_map(|f| f.families.first().map(|(n, _)| n.clone())).collect();
                fams.sort_by_key(|f| f.to_lowercase());
                fams.dedup();
                self.win.font_dlg = Some(FontDialog::new(fams, &self.settings.font_family, self.settings.font_pt));
            }
            Cmd::ZoomIn => self.zoom = (self.zoom + 0.1).min(5.0),
            Cmd::ZoomOut => self.zoom = (self.zoom - 0.1).max(0.3),
            Cmd::ZoomReset => self.zoom = 1.0,
            Cmd::StatusBar => {
                self.settings.status_bar = !self.settings.status_bar;
                self.settings.save();
            }
            Cmd::SetEol(e) => {
                if e != self.win.tabs[self.win.active].eol || self.win.tabs[self.win.active].mixed_eol {
                    self.win.tabs[self.win.active].eol = e;
                    self.win.tabs[self.win.active].mixed_eol = false;
                    self.win.tabs[self.win.active].meta_dirty = true;
                }
            }
            Cmd::SetEncoding(e) => {
                if e != self.win.tabs[self.win.active].encoding {
                    self.win.tabs[self.win.active].encoding = e;
                    self.win.tabs[self.win.active].meta_dirty = true;
                }
            }
        }
    }

    fn checked(&self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::WordWrap => self.settings.wrap,
            Cmd::StatusBar => self.settings.status_bar,
            Cmd::SetEol(e) => e == self.win.tabs[self.win.active].eol,
            Cmd::SetEncoding(e) => e == self.win.tabs[self.win.active].encoding,
            _ => false,
        }
    }

    fn apply_font(&mut self) {
        let Some(d) = self.win.font_dlg.take() else { return };
        if let Some(f) = d.selected {
            self.settings.font_family = f;
        }
        if let Ok(pt) = d.size.text.trim().parse::<f32>() {
            self.settings.font_pt = pt.clamp(4.0, 200.0);
        }
        self.settings.save();
        for w in std::iter::once(&mut self.win).chain(self.others.iter_mut()) {
            for t in &mut w.tabs {
                t.view.set_font(&self.settings.font_family);
            }
            w.redraw_later();
        }
    }

    // ---------------------------------------------------------------- keyboard

    fn shortcut(&self, ev: &KeyEvent, c: Option<char>, primary: bool, shift: bool, alt: bool) -> Option<Cmd> {
        if ev.logical_key == Key::Named(NamedKey::F3) {
            return Some(if shift { Cmd::FindPrev } else { Cmd::FindNext });
        }
        // Ctrl+Tab uses the Control key on every platform, including macOS.
        if self.mods.control_key() && ev.logical_key == Key::Named(NamedKey::Tab) {
            return Some(if shift { Cmd::PrevTab } else { Cmd::NextTab });
        }
        if primary && !shift && !alt {
            match ev.logical_key {
                Key::Named(NamedKey::PageDown) => return Some(Cmd::NextTab),
                Key::Named(NamedKey::PageUp) => return Some(Cmd::PrevTab),
                _ => {}
            }
        }
        if primary && !alt {
            return Some(match c? {
                'n' | 't' => Cmd::New,
                'w' => Cmd::CloseTab,
                d @ '1'..='9' => Cmd::SelectTab(d as u8 - b'0'),
                'o' => Cmd::Open,
                's' if shift => Cmd::SaveAs,
                's' => Cmd::Save,
                'p' => Cmd::Print,
                'q' => Cmd::Exit,
                'z' if shift => Cmd::Redo,
                'z' => Cmd::Undo,
                'y' => Cmd::Redo,
                'x' => Cmd::Cut,
                'c' => Cmd::Copy,
                'v' => Cmd::Paste,
                'a' => Cmd::SelectAll,
                'f' => Cmd::Find,
                'h' => Cmd::Replace,
                'g' if shift => Cmd::FindPrev,
                'g' => Cmd::FindNext,
                '=' | '+' => Cmd::ZoomIn,
                '-' => Cmd::ZoomOut,
                '0' => Cmd::ZoomReset,
                _ => return None,
            });
        }
        if alt && !primary && !MAC && c == Some('z') {
            return Some(Cmd::WordWrap);
        }
        None
    }

    fn on_key(&mut self, ev: KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        self.win.needs_redraw = true;
        let primary = self.primary();
        let (shift, alt) = (self.mods.shift_key(), self.mods.alt_key());
        let c = shortcut_char(&ev);
        let named = match &ev.logical_key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        let text = ev.text.as_ref().map(|t| t.as_str()).filter(|t| !t.chars().any(char::is_control));
        // Text is typed unless a command modifier is held (AltGr = Ctrl+Alt on Windows).
        let typing = !primary || (alt && !MAC);

        if let Some(p) = &mut self.win.prompt {
            match named {
                Some(NamedKey::Escape) => self.choose(Choice::Cancel),
                Some(NamedKey::Enter | NamedKey::Space) => {
                    let c = ui::PROMPT_CHOICES[p.focus].1;
                    self.choose(c);
                }
                Some(NamedKey::Tab | NamedKey::ArrowRight) => p.focus = (p.focus + 1) % 3,
                Some(NamedKey::ArrowLeft) => p.focus = (p.focus + 2) % 3,
                _ => {}
            }
            return;
        }

        if let Some(d) = &mut self.win.font_dlg {
            match named {
                Some(NamedKey::Escape) => self.win.font_dlg = None,
                Some(NamedKey::Enter) => self.apply_font(),
                Some(NamedKey::ArrowUp) => d.move_sel(-1),
                Some(NamedKey::ArrowDown) => d.move_sel(1),
                Some(NamedKey::PageUp) => d.move_sel(-(d.visible_rows() as isize)),
                Some(NamedKey::PageDown) => d.move_sel(d.visible_rows() as isize),
                Some(NamedKey::Tab) => d.field = 1 - d.field,
                Some(n) => {
                    if let Some(k) = field_key(n) {
                        d.field_mut().key(k);
                    }
                }
                None if primary && c == Some('a') => d.field_mut().key(FieldKey::SelectAll),
                None if typing => {
                    if let Some(t) = text {
                        d.field_mut().key(FieldKey::Insert(t));
                    }
                }
                None => {}
            }
            if let Some(d) = &mut self.win.font_dlg
                && d.field == 0
            {
                let list = d.filtered();
                if !list.iter().any(|f| Some(*f) == d.selected.as_deref()) {
                    d.selected = list.first().map(|s| s.to_string());
                    d.scroll = 0;
                }
            }
            return;
        }

        if self.win.menu.open.is_some() {
            let nav = match named {
                Some(NamedKey::ArrowUp) => Some(NavKey::Up),
                Some(NamedKey::ArrowDown) => Some(NavKey::Down),
                Some(NamedKey::ArrowLeft) => Some(NavKey::Left),
                Some(NamedKey::ArrowRight) => Some(NavKey::Right),
                Some(NamedKey::Enter | NamedKey::Space) => Some(NavKey::Enter),
                Some(NamedKey::Escape | NamedKey::Alt) => {
                    self.win.menu.close();
                    None
                }
                _ => None,
            };
            if let Some(cmd) = nav.and_then(|n| self.win.menu.key(n)) {
                self.run(cmd);
            }
            return;
        }

        // Alt+letter opens a menu (not on macOS, where Option types characters).
        if alt
            && !primary
            && !MAC
            && let Some(i) = c.and_then(|c| "feov".find(c))
        {
            self.win.menu.open = Some(i);
            self.win.menu.hover = None;
            self.win.menu.key(NavKey::Down);
            return;
        }

        let in_find = self.win.find.visible && self.win.find.focus;
        if let Some(cmd) = self.shortcut(&ev, c, primary, shift, alt) {
            if in_find && matches!(cmd, Cmd::SelectAll | Cmd::Copy | Cmd::Cut | Cmd::Paste) {
                match cmd {
                    Cmd::SelectAll => self.win.find.field_mut().key(FieldKey::SelectAll),
                    Cmd::Paste => {
                        if let Some(t) = self.get_clipboard() {
                            let first = t.lines().next().unwrap_or("").to_owned();
                            self.win.find.field_mut().key(FieldKey::Insert(&first));
                        }
                    }
                    _ => {
                        let f = self.win.find.field_mut().clone();
                        if f.all {
                            self.set_clipboard(f.text);
                            if cmd == Cmd::Cut {
                                self.win.find.field_mut().key(FieldKey::Delete);
                            }
                        }
                    }
                }
            } else {
                self.run(cmd);
            }
            return;
        }

        if in_find {
            match named {
                Some(NamedKey::Escape) => self.find_action(FindAction::Close),
                Some(NamedKey::Enter) if self.win.find.field == 1 => self.replace_one(),
                Some(NamedKey::Enter) => {
                    self.find_next(!shift);
                }
                Some(NamedKey::Tab) if self.win.find.replace => self.win.find.field = 1 - self.win.find.field,
                Some(n) => {
                    if let Some(k) = field_key(n) {
                        self.win.find.field_mut().key(k);
                    } else if let (true, Some(t)) = (typing, text) {
                        self.win.find.field_mut().key(FieldKey::Insert(t));
                    }
                }
                None => {
                    if let (true, Some(t)) = (typing, text) {
                        self.win.find.field_mut().key(FieldKey::Insert(t));
                    }
                }
            }
            return;
        }

        // macOS: Option moves by word, Cmd by line/document. Elsewhere Ctrl moves by word.
        let word = if MAC { alt } else { primary };
        let line = MAC && primary;
        let motion = match named {
            Some(NamedKey::ArrowLeft) => {
                Some(if line { Motion::Home } else if word { Motion::LeftWord } else { Motion::Left })
            }
            Some(NamedKey::ArrowRight) => {
                Some(if line { Motion::End } else if word { Motion::RightWord } else { Motion::Right })
            }
            Some(NamedKey::ArrowUp) => Some(if line { Motion::BufferStart } else { Motion::Up }),
            Some(NamedKey::ArrowDown) => Some(if line { Motion::BufferEnd } else { Motion::Down }),
            Some(NamedKey::Home) => Some(if primary { Motion::BufferStart } else { Motion::Home }),
            Some(NamedKey::End) => Some(if primary { Motion::BufferEnd } else { Motion::End }),
            Some(NamedKey::PageUp) => Some(Motion::PageUp),
            Some(NamedKey::PageDown) => Some(Motion::PageDown),
            _ => None,
        };
        if let Some(m) = motion {
            self.win.tabs[self.win.active].view.motion(&mut self.gfx.fs, m, shift);
            return;
        }
        let edited = match named {
            Some(NamedKey::Backspace) if word => {
                self.win.tabs[self.win.active].view.delete_word(&mut self.gfx.fs, false);
                true
            }
            Some(NamedKey::Backspace) => {
                self.win.tabs[self.win.active].view.backspace();
                true
            }
            Some(NamedKey::Delete) if word => {
                self.win.tabs[self.win.active].view.delete_word(&mut self.gfx.fs, true);
                true
            }
            Some(NamedKey::Delete) => {
                self.win.tabs[self.win.active].view.delete();
                true
            }
            Some(NamedKey::Enter) => {
                self.win.tabs[self.win.active].view.replace_selection("\n", false);
                true
            }
            Some(NamedKey::Tab) => {
                if !primary {
                    self.win.tabs[self.win.active].view.type_text("\t");
                }
                true
            }
            Some(NamedKey::Escape) => {
                if self.win.find.visible {
                    self.find_action(FindAction::Close);
                } else {
                    self.win.tabs[self.win.active].view.clear_selection();
                }
                false
            }
            _ => match text {
                Some(t) if typing => {
                    self.win.tabs[self.win.active].view.type_text(t);
                    true
                }
                _ => false,
            },
        };
        if edited {
            self.edited();
        }
    }

    // ---------------------------------------------------------------- mouse

    /// Converts a window point to coordinates relative to the text origin.
    fn text_point(&self, p: (f32, f32)) -> (f32, f32) {
        let pad = (6.0 * self.win.scale).round();
        (p.0 - self.win.layout.text.x - pad + self.win.tabs[self.win.active].view.h_scroll(), p.1 - self.win.layout.text.y)
    }

    fn url_at(&self, p: (f32, f32)) -> Option<(usize, Range<usize>, String)> {
        if !self.win.layout.text.contains(p) {
            return None;
        }
        let (x, y) = self.text_point(p);
        self.win.tabs[self.win.active].view.url_at(x, y)
    }

    /// (thumb rect, track travel, total lines)
    fn scrollbar(&self) -> Option<(Rect, f32, usize)> {
        let (total, line) = self.win.tabs[self.win.active].view.scroll_info();
        let lh = self.win.tabs[self.win.active].view.line_h();
        let track = self.win.layout.vbar;
        let vis = (track.h / lh).max(1.0);
        if total as f32 <= vis && line == 0 {
            return None;
        }
        let th = (track.h * (vis / (total as f32 + vis)).min(1.0)).max(24.0 * self.win.scale).min(track.h);
        let y = track.y + (track.h - th) * line as f32 / (total.saturating_sub(1).max(1)) as f32;
        Some((Rect::new(track.x + 2.0 * self.win.scale, y, track.w - 4.0 * self.win.scale, th), track.h - th, total))
    }

    fn update_cursor_icon(&mut self) {
        let p = self.win.mouse;
        let icon = if self.win.drag == Drag::Tab {
            if self.win.layout.tabs.contains(p) { CursorIcon::Grab } else { CursorIcon::Grabbing }
        } else if self.win.prompt.is_some() || self.win.font_dlg.is_some() || self.win.menu.open.is_some() {
            CursorIcon::Default
        } else if self.win.layout.text.contains(p) {
            if self.primary() && self.win.hover_url.is_some() { CursorIcon::Pointer } else { CursorIcon::Text }
        } else if self.win.find.visible
            && self.win.find.hit(p).is_some_and(|a| matches!(a, FindAction::FocusFind | FindAction::FocusReplace))
        {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if icon != self.win.cursor_icon {
            self.win.cursor_icon = icon;
            if let Some(w) = &self.win.window {
                w.set_cursor(icon);
            }
        }
    }

    fn on_mouse_move(&mut self, p: (f32, f32)) {
        self.win.mouse_prev = self.win.mouse;
        self.win.mouse = p;
        if self.win.menu.open.is_some() && self.win.menu.hover_at(p) {
            self.win.needs_redraw = true;
        }
        match self.win.drag {
            Drag::Text => {
                let t = self.win.layout.text;
                if p.1 < t.y {
                    self.win.tabs[self.win.active].view.scroll_px(-(t.y - p.1));
                } else if p.1 > t.bottom() {
                    self.win.tabs[self.win.active].view.scroll_px(p.1 - t.bottom());
                }
                self.win.tabs[self.win.active].view.prepare(&mut self.gfx.fs);
                let (x, y) = self.text_point(p);
                self.win.tabs[self.win.active].view.drag(x, y.clamp(0.0, t.h - 1.0));
                self.win.needs_redraw = true;
            }
            Drag::Thumb(grab) => {
                if let Some((_, range, total)) = self.scrollbar() {
                    let frac = ((p.1 - grab - self.win.layout.vbar.y) / range.max(1.0)).clamp(0.0, 1.0);
                    self.win.tabs[self.win.active].view.scroll_to_line((frac * (total - 1) as f32).round() as usize);
                    self.win.needs_redraw = true;
                }
            }
            Drag::Tab => {
                if let Some(TabHit::Tab(j) | TabHit::Close(j)) = self.win.tabbar.hit(p)
                    && j != self.win.active
                {
                    let t = self.win.tabs.remove(self.win.active);
                    self.win.tabs.insert(j, t);
                    self.win.active = j;
                    self.win.tabbar.reveal = true;
                }
                self.win.needs_redraw = true;
            }
            Drag::None => {
                if self.win.layout.tabs.contains(p) || self.win.layout.tabs.contains(self.win.mouse_prev) {
                    self.win.needs_redraw = true; // tab hover highlight
                }
                let hover = self.url_at(p).map(|(l, r, _)| (l, r));
                if hover != self.win.hover_url {
                    self.win.hover_url = hover;
                    self.win.needs_redraw = true;
                }
            }
        }
        self.update_cursor_icon();
    }

    fn on_mouse_button(&mut self, state: ElementState, button: MouseButton) {
        self.win.needs_redraw = true;
        let p = self.win.mouse;
        if state == ElementState::Released {
            if self.win.drag == Drag::Tab {
                self.finish_tab_drag();
            }
            self.win.drag = Drag::None;
            self.update_cursor_icon();
            return;
        }
        // Click counting for double/triple click.
        let now = Instant::now();
        let (t, lp, n) = self.win.clicks;
        let n = if now - t < Duration::from_millis(450) && (p.0 - lp.0).abs() < 5.0 && (p.1 - lp.1).abs() < 5.0 {
            n % 3 + 1
        } else {
            1
        };
        self.win.clicks = (now, p, n);

        if let Some(pr) = &self.win.prompt {
            if let Some(c) = pr.hit(p) {
                self.choose(c);
            }
            return;
        }
        if let Some(d) = &mut self.win.font_dlg {
            match d.hit(p) {
                Some(DialogAction::Ok) => self.apply_font(),
                Some(DialogAction::Cancel) => self.win.font_dlg = None,
                Some(DialogAction::FocusFilter) => d.field = 0,
                Some(DialogAction::FocusSize) => d.field = 1,
                None if n == 2 && d.in_list(p) => self.apply_font(),
                None => {}
            }
            return;
        }
        if button == MouseButton::Middle
            && self.win.menu.open.is_none()
            && let Some(TabHit::Tab(i) | TabHit::Close(i)) = self.win.tabbar.hit(p)
        {
            self.close_tab(i, false);
            return;
        }
        if button != MouseButton::Left {
            if self.win.menu.open.is_some() {
                self.win.menu.close();
            }
            return;
        }
        match self.win.menu.hit(p) {
            MenuHit::Title(i) => {
                self.win.menu.open = if self.win.menu.open == Some(i) { None } else { Some(i) };
                self.win.menu.hover = None;
                return;
            }
            MenuHit::Item(cmd) => return self.run(cmd),
            MenuHit::Inside => return,
            MenuHit::Outside if self.win.menu.open.is_some() => return self.win.menu.close(),
            MenuHit::Outside => {}
        }
        if self.win.layout.tabs.contains(p) {
            match self.win.tabbar.hit(p) {
                Some(TabHit::Close(i)) => {
                    self.close_tab(i, false);
                }
                Some(TabHit::Tab(i)) => {
                    self.activate(i);
                    self.win.drag = Drag::Tab;
                }
                Some(TabHit::New) => {
                    self.new_tab();
                }
                Some(TabHit::Strip) if n == 2 => {
                    self.new_tab();
                }
                _ => {}
            }
            return;
        }
        if self.win.find.visible && self.win.layout.find.contains(p) {
            if let Some(a) = self.win.find.hit(p) {
                self.find_action(a);
            }
            return;
        }
        if self.win.layout.vbar.contains(p) {
            if let Some((thumb, _, _)) = self.scrollbar() {
                if p.1 >= thumb.y && p.1 < thumb.bottom() {
                    self.win.drag = Drag::Thumb(p.1 - thumb.y);
                } else {
                    let page = self.win.layout.text.h * 0.9;
                    self.win.tabs[self.win.active].view.scroll_px(if p.1 < thumb.y { -page } else { page });
                }
            }
            return;
        }
        if self.win.layout.text.contains(p) {
            self.win.find.focus = false;
            if self.primary()
                && let Some((_, _, url)) = self.url_at(p)
            {
                let target = search::url_target(&url);
                if let Err(e) = open::that_detached(&target) {
                    self.win.status_msg = Some(format!("Could not open {target}: {e}"));
                }
                return;
            }
            let (x, y) = self.text_point(p);
            self.win.tabs[self.win.active].view.click(x, y, n, self.mods.shift_key());
            self.win.drag = Drag::Text;
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let lh = self.win.tabs[self.win.active].view.line_h();
        let (mut dx, mut dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * 3.0 * lh, y * 3.0 * lh),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        if let Some(d) = &mut self.win.font_dlg {
            d.scroll_by(if dy > 0.0 { -3 } else { 3 });
            self.win.needs_redraw = true;
            return;
        }
        if self.win.prompt.is_some() {
            return;
        }
        if self.win.layout.tabs.contains(self.win.mouse) {
            self.win.tabbar.scroll(-(if dx != 0.0 { dx } else { dy }));
            self.win.needs_redraw = true;
            return;
        }
        if self.primary() {
            if dy != 0.0 {
                self.run(if dy > 0.0 { Cmd::ZoomIn } else { Cmd::ZoomOut });
                self.win.needs_redraw = true;
            }
            return;
        }
        if self.mods.shift_key() && dx == 0.0 {
            (dx, dy) = (dy, 0.0);
        }
        if dy != 0.0 {
            self.win.tabs[self.win.active].view.scroll_px(-dy);
        }
        if dx != 0.0 && !self.settings.wrap {
            self.win.tabs[self.win.active].view.scroll_h(-dx);
        }
        self.win.hover_url = None;
        self.win.needs_redraw = true;
    }

    // ---------------------------------------------------------------- drawing

    fn compute_layout(&mut self, w: f32, h: f32) {
        let s = self.win.scale;
        let lh = self.gfx.ui_line_h();
        let menu_h = (lh + 8.0 * s).round();
        let tabs_h = TabBar::height(&self.gfx, s);
        let status_h = if self.settings.status_bar { (lh + 6.0 * s).round() } else { 0.0 };
        let find_h = self.win.find.height(&self.gfx, s).round();
        let vbar_w = (12.0 * s).round();
        let text_h = (h - menu_h - tabs_h - find_h - status_h).max(1.0);
        let top = menu_h + tabs_h;
        self.win.layout = Layout {
            menu: Rect::new(0.0, 0.0, w, menu_h),
            tabs: Rect::new(0.0, menu_h, w, tabs_h),
            text: Rect::new(0.0, top, (w - vbar_w).max(1.0), text_h),
            vbar: Rect::new(w - vbar_w, top, vbar_w, text_h),
            find: Rect::new(0.0, top + text_h, w, find_h),
            status: Rect::new(0.0, h - status_h, w, status_h),
        };
        let pad = (6.0 * s).round();
        let font_px = (self.settings.font_pt * self.zoom * 4.0 / 3.0 * s).max(4.0);
        let line_h = (font_px * 1.3).ceil();
        let want = ((self.win.layout.text.w - 2.0 * pad).max(1.0), text_h, font_px, line_h, self.settings.wrap);
        self.win.tabs[self.win.active].view.set_geometry(font_px, line_h, want.0, want.1, want.4);
    }

    fn draw_editor(&mut self, cv: &mut Canvas) {
        let th = self.theme;
        let s = self.win.scale;
        let area = self.win.layout.text;
        let pad = (6.0 * s).round();
        let show_caret =
            self.win.focused && !(self.win.find.visible && self.win.find.focus) && self.win.prompt.is_none() && self.win.font_dlg.is_none();
        self.win.tabs[self.win.active].view.prepare(&mut self.gfx.fs);
        let caret =
            self.win.tabs[self.win.active].view.draw(&mut self.gfx.fs, &mut self.gfx.cache, cv, area, pad, &th, s, self.win.hover_url.as_ref());
        if let Some((x, y, h)) = caret {
            cv.set_clip(area);
            if !self.win.preedit.is_empty() {
                let w = self.gfx.text_w(&self.win.preedit);
                let r = Rect::new(x, y, w + 4.0 * s, h);
                cv.fill(r, th.field);
                self.gfx.text_in(cv, &self.win.preedit, Rect::new(x + 2.0 * s, y, w, h), th.fg);
                cv.fill(Rect::new(x, r.bottom() - 2.0 * s, r.w, s.max(1.0)), th.fg);
            } else if show_caret {
                cv.fill(Rect::new(x, y, s.max(1.0).round(), h), th.fg);
            }
            cv.reset_clip();
            if let Some(w) = &self.win.window {
                w.set_ime_cursor_area(PhysicalPosition::new(x as f64, y as f64), PhysicalSize::new(1.0, h as f64));
            }
        }

        // Scrollbar.
        cv.fill(self.win.layout.vbar, th.bg);
        if let Some((thumb, _, _)) = self.scrollbar() {
            cv.fill(thumb, if matches!(self.win.drag, Drag::Thumb(_)) { th.dim } else { th.border });
        }
    }

    fn draw_status(&mut self, cv: &mut Canvas) {
        let r = self.win.layout.status;
        if r.h <= 0.0 {
            return;
        }
        let (th, s) = (self.theme, self.win.scale);
        cv.fill(r, th.chrome);
        cv.fill(Rect::new(r.x, r.y, r.w, s.max(1.0)), th.border);
        let cur = self.win.tabs[self.win.active].view.cursor;
        let doc = &self.win.tabs[self.win.active].view.doc;
        let col = doc.line(cur.line)[..cur.index].chars().count() + 1;
        let selected = self.win.tabs[self.win.active].view.selection().map(|(a, z)| doc.chars_between(a, z));
        let left = match &self.win.status_msg {
            Some(m) => m.clone(),
            None => match selected {
                Some(n) => format!("Ln {}, Col {}   ({n} selected)", cur.line + 1, col),
                None => format!("Ln {}, Col {}", cur.line + 1, col),
            },
        };
        let row = Rect::new(r.x + 10.0 * s, r.y, r.w, r.h);
        cv.set_clip(r);
        let eol = if self.win.tabs[self.win.active].mixed_eol { format!("{} (mixed)", self.win.tabs[self.win.active].eol.label()) } else { self.win.tabs[self.win.active].eol.label().to_owned() };
        let segs = [format!("{}%", (self.zoom * 100.0).round()), eol, self.win.tabs[self.win.active].encoding.label().to_owned()];
        let mut x = r.right() - 10.0 * s;
        for seg in segs.iter().rev() {
            let w = self.gfx.text_w(seg);
            x -= w;
            self.gfx.text_in(cv, seg, Rect::new(x, r.y, w, r.h), th.chrome_fg);
            x -= 12.0 * s;
            cv.fill(Rect::new(x, r.y + 4.0 * s, s.max(1.0), r.h - 8.0 * s), th.border);
            x -= 12.0 * s;
        }
        cv.set_clip(Rect::new(r.x, r.y, (x - r.x).max(0.0), r.h));
        self.gfx.text_in(cv, &left, row, th.chrome_fg);
        cv.reset_clip();
    }

    fn redraw(&mut self) {
        let Some(win) = self.win.window.clone() else { return };
        self.gfx.set_ui_scale(self.win.scale);
        let Some(mut surface) = self.win.surface.take() else { return };
        let size = win.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
            && surface.resize(w, h).is_ok()
        {
            self.compute_layout(size.width as f32, size.height as f32);
            if let Ok(mut buf) = surface.buffer_mut() {
                let mut cv = Canvas::new(&mut buf, size.width, size.height);
                self.paint(&mut cv);
                let _ = buf.present();
            }
        }
        self.win.surface = Some(surface);
        let title = format!("{}{} - {APP}", if self.tab().is_dirty() { "*" } else { "" }, self.doc_name());
        if title != self.win.title {
            win.set_title(&title);
            self.win.title = title;
        }
    }

    fn paint(&mut self, cv: &mut Canvas) {
        let (th, s) = (self.theme, self.win.scale);
        cv.fill(Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32), th.bg);
        self.draw_editor(cv);
        if self.win.find.visible {
            self.win.find.draw(&mut self.gfx, cv, &th, self.win.layout.find, s);
        }
        self.draw_status(cv);
        let items: Vec<(String, bool)> = self.win.tabs.iter().map(|t| (t.name(), t.is_dirty())).collect();
        self.win.tabbar.draw(&mut self.gfx, cv, &th, self.win.layout.tabs, s, &items, self.win.active, self.win.mouse);
        self.win.menu.draw_bar(&mut self.gfx, cv, &th, self.win.layout.menu, s);
        if self.win.menu.open.is_some() {
            let states: Vec<(Cmd, bool)> = ui::all_menu_cmds().map(|c| (c, self.checked(c))).collect();
            self.win.menu.draw_dropdown(&mut self.gfx, cv, &th, s, |c| states.iter().any(|(k, v)| *k == c && *v));
        }
        if let Some(d) = &mut self.win.font_dlg {
            d.draw(&mut self.gfx, cv, &th, s, &mut |g, cv, r, fam, pt| draw_preview(g, cv, r, fam, pt, s, th.fg));
        }
        if let Some(p) = &mut self.win.prompt {
            p.draw(&mut self.gfx, cv, &th, s);
        }
    }
}

fn draw_preview(g: &mut Gfx, cv: &mut Canvas, r: Rect, family: &str, pt: f32, s: f32, color: cosmic_text::Color) {
    let px = (pt.clamp(4.0, 200.0) * 4.0 / 3.0 * s).min(r.h * 0.8);
    let mut b = Buffer::new(&mut g.fs, Metrics::new(px, px * 1.25));
    b.set_size(Some(r.w), Some(r.h));
    b.set_wrap(Wrap::None);
    b.set_text("AaBbYyZz 0123 — Ωж مرحبا 你好", &font_attrs(family), Shaping::Advanced, None);
    b.shape_until_scroll(&mut g.fs, false);
    let oy = r.y + (r.h - px * 1.25) / 2.0;
    for run in b.layout_runs() {
        for gl in run.glyphs {
            let pg = gl.physical((r.x, oy + run.line_y), 1.0);
            g.cache.with_pixels(&mut g.fs, pg.cache_key, color, |x, y, c| cv.blend(pg.x + x, pg.y + y, c));
        }
    }
}

fn field_key(n: NamedKey) -> Option<FieldKey<'static>> {
    Some(match n {
        NamedKey::Backspace => FieldKey::Backspace,
        NamedKey::Delete => FieldKey::Delete,
        NamedKey::ArrowLeft => FieldKey::Left,
        NamedKey::ArrowRight => FieldKey::Right,
        NamedKey::Home => FieldKey::Home,
        NamedKey::End => FieldKey::End,
        NamedKey::Space => FieldKey::Insert(" "),
        _ => return None,
    })
}

/// The ASCII character a shortcut refers to, falling back to the physical key
/// so shortcuts work on non-Latin keyboard layouts.
fn shortcut_char(ev: &KeyEvent) -> Option<char> {
    if let Key::Character(s) = ev.key_without_modifiers() {
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.next())
            && c.is_ascii()
        {
            return Some(c.to_ascii_lowercase());
        }
    }
    let PhysicalKey::Code(code) = ev.physical_key else { return None };
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyC => 'c',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyW => 'w',
        KeyCode::KeyV => 'v',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Equal | KeyCode::NumpadAdd => '=',
        KeyCode::Minus | KeyCode::NumpadSubtract => '-',
        KeyCode::Digit0 | KeyCode::Numpad0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        _ => return None,
    })
}

impl ApplicationHandler for App {
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

fn main() {
    let settings = Settings::load();
    let files = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let el = EventLoop::new().expect("failed to create event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(settings, files);
    el.run_app(&mut app).expect("event loop error");
}
