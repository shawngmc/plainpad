//! Editing, find/replace and menu commands.

use super::*;

/// How long to wait for another app to hand over pasted text.
pub(crate) const PASTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Shown when the app that owns the clipboard doesn't hand over its text in time.
pub(crate) const PASTE_TIMED_OUT: &str = "Paste timed out: the app that owns the clipboard didn't respond";

impl App {
    // ---------------------------------------------------------------- editing

    pub(crate) fn edited(&mut self) {
        self.win.status_msg = None;
        self.win.hover_url = None;
    }

    pub(crate) fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    /// Puts text on the system clipboard: natively on Wayland, through arboard
    /// elsewhere (X11, Windows, macOS).
    pub(crate) fn set_clipboard(&mut self, text: String) {
        let text = if cfg!(windows) { text.replace('\n', "\r\n") } else { text };
        if let Some(wl) = &mut self.wl {
            if !wl.set_clipboard(text) {
                self.win.status_msg = Some("Couldn't copy: no recent input to authorise it".into());
            }
            return;
        }
        let ok = self.clipboard().is_some_and(|cb| cb.set_text(text).is_ok());
        if !ok {
            self.win.status_msg = Some("Couldn't copy to the clipboard".into());
        }
    }

    /// Starts a paste into `target`. Reading the clipboard can mean waiting on
    /// another app, so it happens off the main thread and the text is inserted
    /// when it arrives (see `finish_pastes`); the editor never blocks on it.
    pub(crate) fn start_paste(&mut self, target: PasteTarget) {
        let token = self.next_paste;
        self.next_paste += 1;
        let results = Arc::clone(&self.paste_results);
        let proxy = self.proxy.clone();
        let deliver = move |r: Result<String, String>| {
            results.lock().unwrap_or_else(|e| e.into_inner()).push((token, r));
            let _ = proxy.send_event(UserEvent::Wake);
        };
        let start = match &mut self.wl {
            Some(wl) => wl.request_paste(deliver),
            None => {
                // X11, Windows, macOS: arboard on a thread, since the app that owns
                // the clipboard may be asked to produce the data on demand.
                let spawned = std::thread::Builder::new().name("clipboard-read".into()).spawn(move || {
                    let r = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                        Ok(t) => Ok(t),
                        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
                        Err(e) => Err(format!("Couldn't read the clipboard: {e}")),
                    };
                    deliver(r);
                });
                if spawned.is_ok() { wayland::PasteStart::Pending } else { wayland::PasteStart::Empty }
            }
        };
        match start {
            wayland::PasteStart::Ready(text) => self.apply_paste(target, text),
            wayland::PasteStart::Empty => {}
            wayland::PasteStart::Pending => {
                // Given up on if the other app never answers: the event loop
                // wakes at the deadline (see `expire_pastes`).
                self.pastes.push(PendingPaste { token, target, deadline: Instant::now() + PASTE_TIMEOUT });
                self.win.needs_redraw = true;
            }
        }
    }

    /// When the next waiting paste gives up, if any.
    pub(crate) fn paste_deadline(&self) -> Option<Instant> {
        self.pastes.iter().map(|p| p.deadline).min()
    }

    /// Abandons pastes whose deadline has passed, reporting it in their window.
    /// A late answer is then ignored by `finish_pastes`.
    pub(crate) fn expire_pastes(&mut self, now: Instant) {
        let late: Vec<u64> = self.pastes.iter().filter(|p| now >= p.deadline).map(|p| p.token).collect();
        if late.is_empty() {
            return;
        }
        let mut results = self.paste_results.lock().unwrap_or_else(|e| e.into_inner());
        results.extend(late.into_iter().map(|t| (t, Err(PASTE_TIMED_OUT.to_string()))));
        drop(results);
        self.finish_pastes();
    }

    /// Applies pastes whose text has arrived (or that failed or timed out).
    pub(crate) fn finish_pastes(&mut self) {
        let done = std::mem::take(&mut *self.paste_results.lock().unwrap_or_else(|e| e.into_inner()));
        for (token, result) in done {
            // Ignore late arrivals for pastes already finished (by timeout, or the reverse).
            let Some(pos) = self.pastes.iter().position(|p| p.token == token) else { continue };
            let target = self.pastes.remove(pos).target;
            let found = match target {
                PasteTarget::Editor { tab } => self.focus_tab_window(tab),
                PasteTarget::Find { win, .. } => self.focus_win(win),
            };
            if !found {
                continue; // the tab or window has gone
            }
            match result {
                Ok(text) => self.apply_paste(target, text),
                Err(e) => self.win.status_msg = Some(e),
            }
            self.win.needs_redraw = true;
        }
    }

    /// Whether a paste into the current window is waiting for its text.
    pub(crate) fn paste_pending_here(&self) -> bool {
        let id = self.win.id();
        self.pastes.iter().any(|p| match p.target {
            PasteTarget::Editor { tab } => self.win.tab_pos(tab).is_some(),
            PasteTarget::Find { win, .. } => Some(win) == id,
        })
    }

    /// Makes the window holding the tab with this id current.
    fn focus_tab_window(&mut self, tab: u64) -> bool {
        if self.win.tab_pos(tab).is_some() {
            return true;
        }
        match self.others.iter().position(|w| w.tab_pos(tab).is_some()) {
            Some(k) => {
                self.swap_to(k);
                true
            }
            None => false,
        }
    }

    fn apply_paste(&mut self, target: PasteTarget, text: String) {
        if text.is_empty() {
            return;
        }
        match target {
            PasteTarget::Editor { tab } => {
                let Some(i) = self.win.tab_pos(tab) else { return };
                self.win.tabs[i].view.replace_selection(&text, false);
                self.edited();
            }
            PasteTarget::Find { field, .. } => {
                let first = text.lines().next().unwrap_or("");
                self.win.find.field = field;
                self.win.find.field_mut().key(FieldKey::Insert(first));
            }
        }
        self.win.needs_redraw = true;
    }

    pub(crate) fn copy(&mut self) {
        if let Some(t) = self.win.tabs[self.win.active].view.selected_text() {
            self.set_clipboard(t);
        }
    }

    // ---------------------------------------------------------------- find

    pub(crate) fn open_find(&mut self, replace: bool) {
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

    pub(crate) fn replace_one(&mut self) {
        let needle = self.win.find.find.text.clone();
        let repl = self.win.find.repl.text.clone();
        let sel = self.win.tabs[self.win.active].view.selected_text().unwrap_or_default();
        if !sel.is_empty() && search::find(&sel, &needle, self.win.find.case, 0) == Some(0..sel.len()) {
            self.win.tabs[self.win.active].view.replace_selection(&repl, false);
            self.edited();
        }
        self.find_next(true);
    }

    pub(crate) fn find_action(&mut self, a: FindAction) {
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

    pub(crate) fn run(&mut self, cmd: Cmd) {
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
                self.save(None);
            }
            Cmd::SaveAs => {
                self.save_as(None);
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
                let tab = self.win.tabs[self.win.active].id;
                self.start_paste(PasteTarget::Editor { tab });
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
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                let z = match cmd {
                    Cmd::ZoomIn => (self.zoom + 0.1).min(5.0),
                    Cmd::ZoomOut => (self.zoom - 0.1).max(0.3),
                    _ => 1.0,
                };
                if z != self.zoom {
                    self.zoom = z;
                    self.gfx.clear_glyphs(); // glyphs at the old size won't be drawn again
                }
            }
            Cmd::About => self.show_about(),
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

    pub(crate) fn checked(&self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::WordWrap => self.settings.wrap,
            Cmd::StatusBar => self.settings.status_bar,
            Cmd::SetEol(e) => e == self.win.tabs[self.win.active].eol,
            Cmd::SetEncoding(e) => e == self.win.tabs[self.win.active].encoding,
            _ => false,
        }
    }

    pub(crate) fn apply_font(&mut self) {
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
        self.gfx.clear_glyphs();
    }
}
