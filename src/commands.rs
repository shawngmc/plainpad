//! Editing, find/replace and menu commands.

use super::*;

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

    pub(crate) fn get_clipboard(&mut self) -> Option<String> {
        if let Some(wl) = &mut self.wl {
            return wl.get_clipboard();
        }
        self.clipboard().and_then(|cb| cb.get_text().ok())
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

    pub(crate) fn find_next(&mut self, forward: bool) -> bool {
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

    pub(crate) fn replace_all(&mut self) {
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
    }
}
