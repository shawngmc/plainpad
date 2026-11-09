//! Tabs, windows and documents: opening, saving, closing, and moving tabs between windows.

use super::*;

impl App {
    // ---------------------------------------------------------------- tabs & documents

    pub(crate) fn tab(&self) -> &Tab {
        &self.win.tabs[self.win.active]
    }

    pub(crate) fn doc_name(&self) -> String {
        self.tab().name()
    }

    pub(crate) fn activate(&mut self, i: usize) {
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
    pub(crate) fn next_untitled(&self) -> usize {
        (1..).find(|n| !self.win.tabs.iter().any(|t| t.path.is_none() && t.untitled == *n)).unwrap()
    }

    pub(crate) fn new_tab(&mut self) -> usize {
        let view = View::new(&mut self.gfx.fs, &self.settings.font_family, self.settings.wrap);
        let i = (self.win.active + 1).min(self.win.tabs.len());
        let n = self.next_untitled();
        self.win.tabs.insert(i, Tab::new(view, n));
        self.activate(i);
        i
    }

    /// Opens `path` in a tab: switches to it if already open, reuses an
    /// untouched empty tab, or opens a new one.
    pub(crate) fn open_path(&mut self, path: PathBuf) {
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
                self.win.status_msg =
                    l.mixed_eol.then(|| format!("Mixed line endings; will be saved as {}", l.eol.label()));
            }
            Err(e) => self.win.status_msg = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    pub(crate) fn dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self.tab().path.as_ref().and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()) {
            d = d.set_directory(dir);
        }
        if let Some(w) = &self.win.window {
            d = d.set_parent(&**w);
        }
        d
    }

    pub(crate) fn open_dialog(&mut self) {
        for p in self.dialog().set_title("Open").pick_files().unwrap_or_default() {
            self.open_path(p);
        }
    }

    pub(crate) fn save(&mut self) -> bool {
        match self.tab().path.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    pub(crate) fn save_as(&mut self) -> bool {
        let name = self.tab().path.as_ref().map_or("Untitled.txt".into(), |_| self.doc_name());
        match self.dialog().set_title("Save As").set_file_name(name).save_file() {
            Some(p) => self.save_to(p),
            None => false,
        }
    }

    pub(crate) fn save_to(&mut self, path: PathBuf) -> bool {
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

    pub(crate) fn rename(&mut self) {
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

    pub(crate) fn print(&mut self) {
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

    pub(crate) fn ask_save(&mut self, then: Pending) {
        self.win.prompt = Some(Prompt::save(format!("Do you want to save changes to {}?", self.doc_name())));
        self.win.pending = Some(then);
    }

    /// Removes tab `i` from the current window and returns it. If that empties
    /// the window, it gets a fresh tab, and if other windows exist it closes.
    pub(crate) fn take_tab(&mut self, i: usize) -> Tab {
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
    pub(crate) fn close_tab(&mut self, i: usize, force: bool) -> bool {
        if !force && self.win.tabs[i].is_dirty() {
            self.activate(i);
            self.ask_save(Pending::Close(i));
            return false;
        }
        self.take_tab(i);
        true
    }

    /// Makes `others[k]` the current window (the old current one goes into `others`).
    pub(crate) fn swap_to(&mut self, k: usize) {
        std::mem::swap(&mut self.win, &mut self.others[k]);
        self.gfx.set_ui_scale(self.win.scale);
    }

    /// Makes the window with this id current. Returns false if it isn't one of ours.
    pub(crate) fn focus_win(&mut self, id: WindowId) -> bool {
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
    pub(crate) fn request_exit(&mut self) {
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
    pub(crate) fn request_close_window(&mut self) {
        match self.win.tabs.iter().position(Tab::needs_save) {
            Some(i) => {
                self.activate(i);
                self.ask_save(Pending::CloseWindow);
            }
            None if self.others.is_empty() => self.exit = true,
            None => self.win.close = true,
        }
    }

    /// Opens a Ctrl+clicked link, first warning about it if it may be disguised.
    pub(crate) fn open_link(&mut self, text: &str) {
        let info = linkcheck::analyze(text);
        if info.suspicious() {
            let mut lines = vec!["Warning: this link may be disguised as a different address.".to_string()];
            lines.extend(info.warnings.iter().map(|w| format!("• {w}")));
            lines.push(format!("It really goes to: {}", info.target));
            self.win.prompt = Some(Prompt::link(lines));
            self.win.pending = Some(Pending::OpenLink(info.target));
        } else if let Err(e) = launch::open_url(&info.target) {
            self.win.status_msg = Some(e);
        }
    }

    pub(crate) fn choose(&mut self, c: Choice) {
        self.win.prompt = None;
        let Some(pending) = self.win.pending.take() else { return };
        if let Pending::OpenLink(url) = pending {
            if c == Choice::Open
                && let Err(e) = launch::open_url(&url)
            {
                self.win.status_msg = Some(e);
            }
            return;
        }
        let i = self.win.active;
        match c {
            Choice::Open => {}
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
                    Pending::OpenLink(_) => {}
                }
            }
            Choice::Cancel => {
                for t in self.win.tabs.iter_mut().chain(self.others.iter_mut().flat_map(|w| w.tabs.iter_mut())) {
                    t.discard = false;
                }
            }
        }
    }

    /// Whether a dragged tab is far enough outside the tab bar to detach it.
    pub(crate) fn tab_drag_outside(&self) -> bool {
        let (p, s, tabs) = (self.win.mouse, self.win.scale, self.win.layout.tabs);
        let size = self.win.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
        p.1 < tabs.y - 20.0 * s || p.1 > tabs.bottom() + 40.0 * s || p.0 < 0.0 || p.0 > size.width as f32
    }

    /// Moves tab `i` of the current window into `others[k]`. `at` is a point in
    /// that window's pixels: dropped on its tab bar, the tab goes in there.
    pub(crate) fn move_tab_to(&mut self, i: usize, k: usize, at: Option<(f32, f32)>) {
        let tab = self.take_tab(i);
        let w = &mut self.others[k];
        let pos = match at.and_then(|p| w.tabbar.hit(p)) {
            Some(TabHit::Tab(j) | TabHit::Close(j)) => j,
            _ => w.tabs.len(),
        };
        w.tabs.insert(pos, tab);
        w.active = pos;
        w.tabbar.reveal = true;
        w.redraw_later();
        if let Some(win) = &w.window {
            win.focus_window();
        }
    }

    /// Finishes dragging a tab (X11, Windows, macOS). Dropped well outside the
    /// tab bar, it moves into the window under the pointer, or into a new window there.
    pub(crate) fn finish_tab_drag(&mut self) {
        if !self.tab_drag_outside() {
            return;
        }
        let (p, s) = (self.win.mouse, self.win.scale);
        let size = self.win.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
        let screen = self.win.screen_rect().map(|r| (r.x + p.0, r.y + p.1));
        let target = screen.and_then(|sp| {
            self.others.iter().position(|w| w.screen_rect().is_some_and(|r| r.contains(sp))).map(|k| (k, sp))
        });
        if let Some((k, (sx, sy))) = target {
            let local = self.others[k].screen_rect().map(|r| (sx - r.x, sy - r.y));
            self.move_tab_to(self.win.active, k, local);
        } else if self.win.tabs.len() > 1 {
            let tab = self.take_tab(self.win.active);
            // Put the new window so the pointer sits over its first tab.
            let pos = screen.map(|(x, y)| PhysicalPosition::new((x - 60.0 * s) as i32, (y - 40.0 * s) as i32));
            self.spawn = Some(Spawn { tab, pos, size });
        }
    }

    /// Starts a Wayland drag-and-drop for the active tab, if on Wayland and
    /// the tab has somewhere to go. Returns true if it started.
    pub(crate) fn start_wl_drag(&mut self) -> bool {
        let movable = self.win.tabs.len() > 1 || !self.others.is_empty();
        let (Some(wl), Some(id)) = (&mut self.wl, self.win.id()) else { return false };
        if !movable || self.win.wl_surface == 0 || !wl.start(self.win.wl_surface as *mut std::ffi::c_void) {
            return false;
        }
        self.wl_src = Some((id, self.win.active));
        self.win.drag = Drag::None;
        true
    }

    /// Handles the end of a Wayland tab drag.
    pub(crate) fn finish_wl_drag(&mut self, outcome: wl_drag::Outcome) {
        let Some((src, i)) = self.wl_src.take() else { return };
        if !self.focus_win(src) || i >= self.win.tabs.len() {
            return;
        }
        self.win.needs_redraw = true;
        match outcome {
            wl_drag::Outcome::Dropped { surface, x, y } => {
                if let Some(k) = self.others.iter().position(|w| w.wl_surface == surface) {
                    let sc = self.others[k].scale;
                    self.move_tab_to(i, k, Some((x as f32 * sc, y as f32 * sc)));
                    return;
                }
                // Dropped back on its own window: over the tab bar cancels, elsewhere detaches.
                let p = (x as f32 * self.win.scale, y as f32 * self.win.scale);
                if surface != self.win.wl_surface || self.win.layout.tabs.contains(p) {
                    return;
                }
            }
            wl_drag::Outcome::Elsewhere => {}
        }
        if self.win.tabs.len() > 1 {
            let size = self.win.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
            let tab = self.take_tab(i);
            self.spawn = Some(Spawn { tab, pos: None, size });
        }
    }

    /// Creates an OS window for `win`.
    pub(crate) fn open_window(
        &mut self,
        el: &ActiveEventLoop,
        win: &mut Win,
        pos: Option<PhysicalPosition<i32>>,
        size: Option<PhysicalSize<u32>>,
    ) {
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
        if let Ok(h) = w.window_handle()
            && let RawWindowHandle::Wayland(wh) = h.as_raw()
        {
            win.wl_surface = wh.surface.as_ptr() as usize;
        }
        if self.wl.is_none()
            && let Ok(d) = w.display_handle()
            && let RawDisplayHandle::Wayland(dh) = d.as_raw()
        {
            self.wl = wl_drag::WlDrag::new(dh.display.as_ptr());
        }
        win.window = Some(w.clone());
        win.surface = Some(surface);
        win.tabbar.reveal = true;
        w.request_redraw();
    }
}
