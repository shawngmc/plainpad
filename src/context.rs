//! Right-click (context) menus for the text area and the tab bar.

use super::*;
use ui::{ContextMenu, CtxAction, CtxItem};

impl App {
    /// The menu for the text area. `link` is the link under the pointer, if any.
    pub(crate) fn open_text_menu(&mut self, at: (f32, f32), link: Option<String>) {
        let view = &self.win.tabs[self.win.active].view;
        let has_sel = view.selection().is_some();
        let (can_undo, can_redo) = (view.doc.can_undo(), view.doc.can_redo());
        let mut items = Vec::new();
        if let Some(url) = link {
            items.push(CtxItem::new("Open Link", "Ctrl+Click", CtxAction::OpenLink(url.clone()), true));
            items.push(CtxItem::new("Copy Link Address", "", CtxAction::CopyLink(url), true));
            items.push(CtxItem::SEP);
        }
        items.extend([
            CtxItem::new("Undo", "Ctrl+Z", CtxAction::Cmd(Cmd::Undo), can_undo),
            CtxItem::new("Redo", "Ctrl+Y", CtxAction::Cmd(Cmd::Redo), can_redo),
            CtxItem::SEP,
            CtxItem::new("Cut", "Ctrl+X", CtxAction::Cmd(Cmd::Cut), has_sel),
            CtxItem::new("Copy", "Ctrl+C", CtxAction::Cmd(Cmd::Copy), has_sel),
            CtxItem::new("Paste", "Ctrl+V", CtxAction::Cmd(Cmd::Paste), true),
            CtxItem::new("Delete", "Del", CtxAction::Cmd(Cmd::Delete), has_sel),
            CtxItem::SEP,
            CtxItem::new("Select All", "Ctrl+A", CtxAction::Cmd(Cmd::SelectAll), true),
        ]);
        self.win.menu.close();
        self.win.ctx = Some(ContextMenu::new(items, at));
    }

    /// The menu for tab `i` in the tab bar.
    pub(crate) fn open_tab_menu(&mut self, i: usize, at: (f32, f32)) {
        let n = self.win.tabs.len();
        let has_path = self.win.tabs[i].path.is_some();
        let id = self.win.tabs[i].id;
        let items = vec![
            CtxItem::new("Close Tab", "Ctrl+W", CtxAction::CloseTab(id), true),
            CtxItem::new("Close Other Tabs", "", CtxAction::CloseOtherTabs(id), n > 1),
            CtxItem::new("Close Tabs to the Right", "", CtxAction::CloseTabsRight(id), i + 1 < n),
            CtxItem::SEP,
            CtxItem::new("Move to New Window", "", CtxAction::MoveToNewWindow(id), n > 1),
            CtxItem::SEP,
            CtxItem::new("Copy File Path", "", CtxAction::CopyPath(id), has_path),
        ];
        self.win.menu.close();
        self.win.ctx = Some(ContextMenu::new(items, at));
    }

    /// The link containing the text cursor, if any (for opening the menu from the keyboard).
    pub(crate) fn link_at_cursor(&self) -> Option<String> {
        let view = &self.win.tabs[self.win.active].view;
        let c = view.cursor;
        let line = view.doc.line(c.line);
        let r = search::find_urls(&line).into_iter().find(|r| c.index >= r.start && c.index < r.end)?;
        line.get(r).map(str::to_owned)
    }

    pub(crate) fn run_ctx(&mut self, a: CtxAction) {
        self.win.ctx = None;
        match a {
            CtxAction::Cmd(cmd) => self.run(cmd),
            CtxAction::OpenLink(url) => self.open_link(&url),
            CtxAction::CopyLink(url) => self.set_clipboard(url),
            CtxAction::CloseTab(id) => {
                if let Some(i) = self.win.tab_pos(id) {
                    self.close_tab(i, false);
                }
            }
            CtxAction::CloseOtherTabs(id) => {
                if let Some(i) = self.win.tab_pos(id) {
                    self.close_tabs_where(i, |j| j != i);
                }
            }
            CtxAction::CloseTabsRight(id) => {
                if let Some(i) = self.win.tab_pos(id) {
                    self.close_tabs_where(i, |j| j > i);
                }
            }
            CtxAction::MoveToNewWindow(id) => {
                let Some(i) = self.win.tab_pos(id) else { return };
                if self.win.tabs.len() > 1 {
                    let size = self.win.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                    // Offset from this window where the platform reports positions (not Wayland).
                    let pos = self.win.screen_rect().map(|r| PhysicalPosition::new(r.x as i32 + 40, r.y as i32 + 40));
                    let tab = self.take_tab(i);
                    self.spawn = Some(Spawn { tab, pos, size });
                }
            }
            CtxAction::CopyPath(id) => {
                if let Some(p) = self.win.tab_pos(id).and_then(|i| self.win.tabs[i].path.as_ref()) {
                    self.set_clipboard(p.display().to_string());
                }
            }
        }
    }

    /// Closes every tab whose index matches `close`, keeping tab `keep` active
    /// afterwards. Modified tabs are asked about one at a time.
    fn close_tabs_where(&mut self, keep: usize, close: impl Fn(usize) -> bool) {
        for (j, t) in self.win.tabs.iter_mut().enumerate() {
            t.closing = close(j);
        }
        self.win.keep_tab = Some(self.win.tabs[keep].id);
        self.close_marked();
    }

    /// Continues closing the tabs marked `closing`: asks about the next modified
    /// one, or, once none is left to ask about, removes them all.
    pub(crate) fn close_marked(&mut self) {
        if let Some(i) = self.win.tabs.iter().position(|t| t.closing && t.needs_save()) {
            self.activate(i);
            self.ask_save(Pending::CloseMarked);
            return;
        }
        for i in (0..self.win.tabs.len()).rev() {
            if self.win.tabs[i].closing {
                drop(self.take_tab(i));
            }
        }
        crate::mem::trim();
        if let Some(id) = self.win.keep_tab.take()
            && let Some(i) = self.win.tabs.iter().position(|t| t.id == id)
        {
            self.activate(i);
        }
    }
}
