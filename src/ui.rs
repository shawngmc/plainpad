//! Custom-drawn chrome: menu bar, find/replace bar, font dialog, prompts.

use crate::doc::{Encoding, Eol};
use crate::gfx::{Canvas, Gfx, Rect, Theme};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    Rename,
    Print,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Find,
    FindNext,
    FindPrev,
    Replace,
    WordWrap,
    Font,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    StatusBar,
    SetEol(Eol),
    SetEncoding(Encoding),
    CloseTab,
    NextTab,
    PrevTab,
    /// 1-8 select that tab, 9 the last one.
    SelectTab(u8),
}

pub struct Item {
    pub label: &'static str,
    pub key: &'static str,
    pub cmd: Option<Cmd>,
    pub sub: Option<&'static [Item]>,
}

impl Item {
    fn is_sep(&self) -> bool {
        self.cmd.is_none() && self.sub.is_none()
    }
}

const fn it(label: &'static str, key: &'static str, cmd: Cmd) -> Item {
    Item { label, key, cmd: Some(cmd), sub: None }
}
const fn submenu(label: &'static str, items: &'static [Item]) -> Item {
    Item { label, key: "", cmd: None, sub: Some(items) }
}
const SEP: Item = Item { label: "", key: "", cmd: None, sub: None };

pub const MENUS: &[(&str, &[Item])] = &[
    (
        "File",
        &[
            it("New Tab", "Ctrl+N", Cmd::New),
            it("Open…", "Ctrl+O", Cmd::Open),
            it("Save", "Ctrl+S", Cmd::Save),
            it("Save As…", "Ctrl+Shift+S", Cmd::SaveAs),
            it("Rename…", "", Cmd::Rename),
            it("Close Tab", "Ctrl+W", Cmd::CloseTab),
            SEP,
            it("Print…", "Ctrl+P", Cmd::Print),
            SEP,
            it("Exit", "Ctrl+Q", Cmd::Exit),
        ],
    ),
    (
        "Edit",
        &[
            it("Undo", "Ctrl+Z", Cmd::Undo),
            it("Redo", "Ctrl+Y", Cmd::Redo),
            SEP,
            it("Cut", "Ctrl+X", Cmd::Cut),
            it("Copy", "Ctrl+C", Cmd::Copy),
            it("Paste", "Ctrl+V", Cmd::Paste),
            it("Delete", "Del", Cmd::Delete),
            SEP,
            it("Find…", "Ctrl+F", Cmd::Find),
            it("Find Next", "F3", Cmd::FindNext),
            it("Find Previous", "Shift+F3", Cmd::FindPrev),
            it("Replace…", "Ctrl+H", Cmd::Replace),
            SEP,
            it("Select All", "Ctrl+A", Cmd::SelectAll),
        ],
    ),
    (
        "Format",
        &[
            it("Word Wrap", "Alt+Z", Cmd::WordWrap),
            it("Font…", "", Cmd::Font),
            SEP,
            submenu(
                "Line Endings",
                &[
                    it("Windows (CRLF)", "", Cmd::SetEol(Eol::CrLf)),
                    it("Unix (LF)", "", Cmd::SetEol(Eol::Lf)),
                    it("Classic Mac (CR)", "", Cmd::SetEol(Eol::Cr)),
                ],
            ),
            submenu(
                "Encoding",
                &[
                    it("UTF-8", "", Cmd::SetEncoding(Encoding::Utf8)),
                    it("UTF-8 with BOM", "", Cmd::SetEncoding(Encoding::Utf8Bom)),
                    it("UTF-16 LE", "", Cmd::SetEncoding(Encoding::Utf16Le)),
                    it("UTF-16 BE", "", Cmd::SetEncoding(Encoding::Utf16Be)),
                    it("Windows-1252", "", Cmd::SetEncoding(Encoding::Windows1252)),
                ],
            ),
        ],
    ),
    (
        "View",
        &[
            it("Zoom In", "Ctrl++", Cmd::ZoomIn),
            it("Zoom Out", "Ctrl+-", Cmd::ZoomOut),
            it("Restore Default Zoom", "Ctrl+0", Cmd::ZoomReset),
            SEP,
            it("Status Bar", "", Cmd::StatusBar),
            SEP,
            it("Next Tab", "Ctrl+Tab", Cmd::NextTab),
            it("Previous Tab", "Ctrl+Shift+Tab", Cmd::PrevTab),
        ],
    ),
];

/// Every command reachable from the menus, including submenus.
pub fn all_menu_cmds() -> impl Iterator<Item = Cmd> {
    MENUS.iter().flat_map(|(_, items)| {
        items.iter().flat_map(|i| i.cmd.into_iter().chain(i.sub.into_iter().flatten().filter_map(|s| s.cmd)))
    })
}

/// Shortcut text as shown on this platform.
pub fn key_label(k: &str) -> String {
    if cfg!(target_os = "macos") && k.ends_with("Tab") {
        // Tab switching uses the Control key on macOS too.
        k.replace("Ctrl+Shift+", "⌃⇧").replace("Ctrl+", "⌃")
    } else if cfg!(target_os = "macos") {
        k.replace("Ctrl+Shift+", "⇧⌘").replace("Ctrl+", "⌘").replace("Shift+", "⇧").replace("Alt+", "⌥")
    } else {
        k.to_owned()
    }
}

/// A drawn dropdown panel and the screen rect of each selectable row.
#[derive(Default)]
struct Panel {
    rect: Rect,
    rows: Vec<(Rect, usize)>,
}

impl Panel {
    fn row_at(&self, p: (f32, f32)) -> Option<usize> {
        self.rows.iter().find(|(r, _)| r.contains(p)).map(|(_, i)| *i)
    }

    fn row_rect(&self, idx: usize) -> Option<Rect> {
        self.rows.iter().find(|(_, i)| *i == idx).map(|(r, _)| *r)
    }
}

#[derive(Default)]
pub struct MenuState {
    pub open: Option<usize>,
    /// Highlighted row in the open menu.
    pub hover: Option<usize>,
    /// Row of the open menu whose submenu is showing.
    pub sub: Option<usize>,
    /// Highlighted row in the submenu; `Some` also means keyboard focus is in the submenu.
    pub sub_hover: Option<usize>,
    titles: Vec<Rect>,
    main: Panel,
    side: Panel,
}

pub enum MenuHit {
    Title(usize),
    Item(Cmd),
    Inside,
    Outside,
}

/// One row of a dropdown panel, as drawn.
struct Row {
    label: String,
    key: String,
    checked: bool,
    submenu: bool,
    enabled: bool,
    sep: bool,
}

impl Row {
    fn from_item(i: &Item, checked: &dyn Fn(Cmd) -> bool) -> Self {
        Row {
            label: i.label.to_owned(),
            key: key_label(i.key),
            checked: i.cmd.is_some_and(checked),
            submenu: i.sub.is_some(),
            enabled: true,
            sep: i.is_sep(),
        }
    }
}

/// Size of a dropdown panel: (width, height, label column width, key column width).
fn panel_size(g: &mut Gfx, rows: &[Row], s: f32) -> (f32, f32, f32, f32) {
    let row = (g.ui_line_h() + 8.0 * s).round();
    let sep = 9.0 * s;
    let mut lw: f32 = 0.0;
    let mut kw: f32 = g.text_w("▸");
    for r in rows {
        lw = lw.max(g.text_w(&r.label));
        kw = kw.max(g.text_w(&r.key));
    }
    let w = 28.0 * s + lw + 32.0 * s + kw + 16.0 * s;
    let h = rows.iter().map(|r| if r.sep { sep } else { row }).sum::<f32>() + 8.0 * s;
    (w, h, lw, kw)
}

/// Draws a dropdown panel with its top-left at (x, y). Disabled rows are drawn
/// dimmed and left out of the hit-test list, so they can't be hovered or chosen.
#[allow(clippy::too_many_arguments)]
fn draw_panel(
    g: &mut Gfx,
    cv: &mut Canvas,
    th: &Theme,
    s: f32,
    items: &[Row],
    x: f32,
    y: f32,
    highlight: &[usize],
) -> Panel {
    let (w, h, lw, kw) = panel_size(g, items, s);
    let row = (g.ui_line_h() + 8.0 * s).round();
    let sep = 9.0 * s;
    let pad = 28.0 * s;
    let rect = Rect::new(x, y, w, h);
    cv.fill(Rect::new(x + 2.0 * s, y + 2.0 * s, w, h), th.shade);
    cv.fill(rect, th.chrome);
    cv.outline(rect, s, th.border);
    let mut rows = Vec::new();
    let mut yy = y + 4.0 * s;
    for (idx, i) in items.iter().enumerate() {
        if i.sep {
            cv.fill(Rect::new(x + 8.0 * s, yy + (sep / 2.0).floor(), w - 16.0 * s, s), th.border);
            yy += sep;
            continue;
        }
        let r = Rect::new(x + 4.0 * s, yy, w - 8.0 * s, row);
        let fg = if i.enabled { th.chrome_fg } else { th.border };
        if i.enabled && highlight.contains(&idx) {
            cv.fill(r, th.hover);
        }
        if i.checked {
            g.text_in(cv, "✓", Rect::new(x + 10.0 * s, yy, pad, row), fg);
        }
        g.text_in(cv, &i.label, Rect::new(x + pad, yy, lw, row), fg);
        let kl = if i.submenu { "▸" } else { i.key.as_str() };
        let kx = x + w - 16.0 * s - g.text_w(kl);
        g.text_in(cv, kl, Rect::new(kx, yy, kw, row), if i.enabled { th.dim } else { th.border });
        if i.enabled {
            rows.push((r, idx));
        }
        yy += row;
    }
    Panel { rect, rows }
}

impl MenuState {
    pub fn draw_bar(&mut self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, bar: Rect, s: f32) {
        cv.fill(bar, th.chrome);
        cv.fill(Rect::new(bar.x, bar.bottom() - s, bar.w, s), th.border);
        self.titles.clear();
        let mut x = bar.x + 4.0 * s;
        for (i, (title, _)) in MENUS.iter().enumerate() {
            let w = g.text_w(title) + 20.0 * s;
            let r = Rect::new(x, bar.y, w, bar.h - s);
            if self.open == Some(i) {
                cv.fill(r, th.hover);
            }
            g.text_in(cv, title, Rect::new(x + 10.0 * s, r.y, w, r.h), th.chrome_fg);
            self.titles.push(r);
            x += w;
        }
    }

    pub fn draw_dropdown(&mut self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, s: f32, checked: impl Fn(Cmd) -> bool) {
        self.main = Panel::default();
        self.side = Panel::default();
        let Some(m) = self.open else { return };
        let items = MENUS[m].1;
        let rows: Vec<Row> = items.iter().map(|i| Row::from_item(i, &checked)).collect();
        let (w, _, _, _) = panel_size(g, &rows, s);
        let t = self.titles[m];
        let x = t.x.min(cv.w as f32 - w).max(0.0);
        let hl: Vec<usize> = self.hover.into_iter().chain(self.sub).collect();
        self.main = draw_panel(g, cv, th, s, &rows, x, t.bottom() + s, &hl);
        let Some(si) = self.sub else { return };
        let (Some(sub), Some(row)) = (items[si].sub, self.main.row_rect(si)) else { return };
        let sub_rows: Vec<Row> = sub.iter().map(|i| Row::from_item(i, &checked)).collect();
        let (sw, sh, _, _) = panel_size(g, &sub_rows, s);
        let mut sx = self.main.rect.right() - 4.0 * s;
        if sx + sw > cv.w as f32 {
            sx = (self.main.rect.x - sw + 4.0 * s).max(0.0);
        }
        let sy = (row.y - 4.0 * s).min(cv.h as f32 - sh).max(0.0);
        let hl: Vec<usize> = self.sub_hover.into_iter().collect();
        self.side = draw_panel(g, cv, th, s, &sub_rows, sx, sy, &hl);
    }

    fn sub_items(&self) -> Option<&'static [Item]> {
        MENUS[self.open?].1[self.sub?].sub
    }

    pub fn hit(&mut self, p: (f32, f32)) -> MenuHit {
        if let Some(i) = self.titles.iter().position(|r| r.contains(p)) {
            return MenuHit::Title(i);
        }
        let Some(m) = self.open else { return MenuHit::Outside };
        if let (Some(sub), Some(idx)) = (self.sub_items(), self.side.row_at(p)) {
            return sub[idx].cmd.map_or(MenuHit::Inside, MenuHit::Item);
        }
        if self.side.rect.contains(p) {
            return MenuHit::Inside;
        }
        if let Some(idx) = self.main.row_at(p) {
            let item = &MENUS[m].1[idx];
            if item.sub.is_some() {
                self.sub = Some(idx);
                return MenuHit::Inside;
            }
            return item.cmd.map_or(MenuHit::Inside, MenuHit::Item);
        }
        if self.main.rect.contains(p) { MenuHit::Inside } else { MenuHit::Outside }
    }

    pub fn hover_at(&mut self, p: (f32, f32)) -> bool {
        let old = (self.open, self.hover, self.sub, self.sub_hover);
        if let Some(i) = self.titles.iter().position(|r| r.contains(p))
            && self.open.is_some()
            && self.open != Some(i)
        {
            self.open = Some(i);
            self.hover = None;
            self.sub = None;
            self.sub_hover = None;
        }
        if self.side.rect.contains(p) {
            self.sub_hover = self.side.row_at(p);
        } else if let Some(m) = self.open {
            self.hover = self.main.row_at(p);
            self.sub_hover = None;
            // Hovering another row closes the submenu; moving off the panel keeps it open.
            if let Some(h) = self.hover {
                self.sub = MENUS[m].1[h].sub.map(|_| h);
            }
        }
        old != (self.open, self.hover, self.sub, self.sub_hover)
    }

    /// Keyboard navigation inside an open menu. Returns a command on Enter.
    pub fn key(&mut self, k: NavKey) -> Option<Cmd> {
        let m = self.open?;
        let items = MENUS[m].1;
        let step = |items: &[Item], from: Option<usize>, d: isize| -> Option<usize> {
            let n = items.len() as isize;
            let mut i = from.map_or(if d > 0 { -1 } else { n }, |v| v as isize);
            for _ in 0..n {
                i = (i + d).rem_euclid(n);
                if !items[i as usize].is_sep() {
                    return Some(i as usize);
                }
            }
            None
        };
        let switch_menu = |me: &mut Self, d: usize| {
            me.open = Some((m + d) % MENUS.len());
            me.hover = None;
            me.sub = None;
            me.sub_hover = None;
        };
        // Keyboard focus inside a submenu.
        if let (Some(sub), Some(sh)) = (self.sub_items(), self.sub_hover) {
            match k {
                NavKey::Up => self.sub_hover = step(sub, Some(sh), -1),
                NavKey::Down => self.sub_hover = step(sub, Some(sh), 1),
                NavKey::Left => self.sub_hover = None,
                NavKey::Right => switch_menu(self, 1),
                NavKey::Enter => return sub[sh].cmd,
            }
            return None;
        }
        let opens_sub = self.hover.and_then(|h| items[h].sub.map(|s| (h, s)));
        match k {
            NavKey::Up => self.hover = step(items, self.hover, -1),
            NavKey::Down => self.hover = step(items, self.hover, 1),
            NavKey::Left => switch_menu(self, MENUS.len() - 1),
            NavKey::Right | NavKey::Enter if opens_sub.is_some() => {
                let (h, sub) = opens_sub.unwrap();
                self.sub = Some(h);
                self.sub_hover = step(sub, None, 1);
            }
            NavKey::Right => switch_menu(self, 1),
            NavKey::Enter => return self.hover.and_then(|i| items[i].cmd),
        }
        if !matches!(k, NavKey::Right | NavKey::Enter) {
            self.sub = self.hover.filter(|&h| items[h].sub.is_some());
        }
        None
    }

    pub fn close(&mut self) {
        self.open = None;
        self.hover = None;
        self.sub = None;
        self.sub_hover = None;
    }
}

/// What a right-click menu item does.
#[derive(Clone, Debug, PartialEq)]
pub enum CtxAction {
    Cmd(Cmd),
    OpenLink(String),
    CopyLink(String),
    CloseTab(usize),
    CloseOtherTabs(usize),
    CloseTabsRight(usize),
    MoveToNewWindow(usize),
    CopyPath(usize),
}

pub struct CtxItem {
    pub label: &'static str,
    pub key: &'static str,
    /// `None` is a separator.
    pub action: Option<CtxAction>,
    pub enabled: bool,
}

impl CtxItem {
    pub fn new(label: &'static str, key: &'static str, action: CtxAction, enabled: bool) -> Self {
        Self { label, key, action: Some(action), enabled }
    }

    pub const SEP: CtxItem = CtxItem { label: "", key: "", action: None, enabled: false };
}

/// A right-click (context) menu, opened at a point.
pub struct ContextMenu {
    items: Vec<CtxItem>,
    at: (f32, f32),
    pub hover: Option<usize>,
    panel: Panel,
}

pub enum CtxHit {
    Item(CtxAction),
    /// On the menu but not on a usable item (a separator or disabled item).
    Inside,
    Outside,
}

impl ContextMenu {
    pub fn new(items: Vec<CtxItem>, at: (f32, f32)) -> Self {
        Self { items, at, hover: None, panel: Panel::default() }
    }

    fn rows(&self) -> Vec<Row> {
        self.items
            .iter()
            .map(|i| Row {
                label: i.label.to_owned(),
                key: key_label(i.key),
                checked: false,
                submenu: false,
                enabled: i.enabled,
                sep: i.action.is_none(),
            })
            .collect()
    }

    pub fn draw(&mut self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, s: f32) {
        let rows = self.rows();
        let (w, h, _, _) = panel_size(g, &rows, s);
        // Open down-right of the pointer, flipping to stay on screen.
        let (cw, ch) = (cv.w as f32, cv.h as f32);
        let x = if self.at.0 + w > cw { (self.at.0 - w).max(0.0) } else { self.at.0 };
        let y = if self.at.1 + h > ch { (self.at.1 - h).max(0.0) } else { self.at.1 };
        let hl: Vec<usize> = self.hover.into_iter().collect();
        self.panel = draw_panel(g, cv, th, s, &rows, x, y, &hl);
    }

    pub fn hover_at(&mut self, p: (f32, f32)) -> bool {
        let old = self.hover;
        self.hover = self.panel.row_at(p);
        old != self.hover
    }

    pub fn hit(&self, p: (f32, f32)) -> CtxHit {
        match self.panel.row_at(p) {
            Some(i) => self.items[i].action.clone().map_or(CtxHit::Inside, CtxHit::Item),
            None if self.panel.rect.contains(p) => CtxHit::Inside,
            None => CtxHit::Outside,
        }
    }

    /// Up/Down move between enabled items; Enter returns the chosen action.
    pub fn key(&mut self, k: NavKey) -> Option<CtxAction> {
        let usable: Vec<usize> =
            (0..self.items.len()).filter(|&i| self.items[i].enabled && self.items[i].action.is_some()).collect();
        if usable.is_empty() {
            return None;
        }
        let pos = self.hover.and_then(|h| usable.iter().position(|&i| i == h));
        match k {
            NavKey::Down => self.hover = Some(usable[pos.map_or(0, |p| (p + 1) % usable.len())]),
            NavKey::Up => {
                self.hover = Some(usable[pos.map_or(usable.len() - 1, |p| (p + usable.len() - 1) % usable.len())])
            }
            NavKey::Enter => return self.hover.and_then(|h| self.items[h].action.clone()),
            NavKey::Left | NavKey::Right => {}
        }
        None
    }
}

#[derive(Clone, Copy)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
}

/// Minimal single-line text field.
#[derive(Default, Clone)]
pub struct TextField {
    pub text: String,
    pub cursor: usize,
    /// Whole text is selected (typing replaces it).
    pub all: bool,
}

pub enum FieldKey<'a> {
    Insert(&'a str),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    SelectAll,
}

impl TextField {
    pub fn set(&mut self, t: &str) {
        self.text = t.to_owned();
        self.cursor = self.text.len();
        self.all = true;
    }

    pub fn key(&mut self, k: FieldKey) {
        let prev = |s: &str, i: usize| s[..i].char_indices().next_back().map_or(0, |(j, _)| j);
        let next = |s: &str, i: usize| s[i..].chars().next().map_or(i, |c| i + c.len_utf8());
        if self.all && matches!(k, FieldKey::Insert(_) | FieldKey::Backspace | FieldKey::Delete) {
            self.text.clear();
            self.cursor = 0;
            self.all = false;
            if !matches!(k, FieldKey::Insert(_)) {
                return;
            }
        }
        match k {
            FieldKey::Insert(s) => {
                let s: String = s.chars().filter(|c| !c.is_control()).collect();
                self.text.insert_str(self.cursor, &s);
                self.cursor += s.len();
            }
            FieldKey::Backspace if self.cursor > 0 => {
                let p = prev(&self.text, self.cursor);
                self.text.replace_range(p..self.cursor, "");
                self.cursor = p;
            }
            FieldKey::Delete if self.cursor < self.text.len() => {
                let n = next(&self.text, self.cursor);
                self.text.replace_range(self.cursor..n, "");
            }
            FieldKey::Left => self.cursor = if self.all { 0 } else { prev(&self.text, self.cursor) },
            FieldKey::Right => self.cursor = if self.all { self.text.len() } else { next(&self.text, self.cursor) },
            FieldKey::Home => self.cursor = 0,
            FieldKey::End => self.cursor = self.text.len(),
            FieldKey::SelectAll => {
                self.all = true;
                return;
            }
            _ => {}
        }
        self.all = false;
    }

    pub fn draw(&self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, r: Rect, focused: bool, s: f32) {
        cv.fill(r, th.field);
        cv.outline(r, s, if focused { th.accent } else { th.border });
        let inner = Rect::new(r.x + 6.0 * s, r.y, r.w - 12.0 * s, r.h);
        cv.set_clip(inner);
        // Keep the caret visible for long text by scrolling left.
        let caret_x = g.text_w(&self.text[..self.cursor]);
        let shift = (caret_x - inner.w + 2.0 * s).max(0.0);
        if self.all && focused && !self.text.is_empty() {
            let w = g.text_w(&self.text);
            cv.fill(Rect::new(inner.x - shift, r.y + 3.0 * s, w, r.h - 6.0 * s), th.sel);
        }
        g.text_in(cv, &self.text, Rect::new(inner.x - shift, r.y, inner.w + shift, r.h), th.fg);
        if focused {
            cv.fill(Rect::new(inner.x - shift + caret_x, r.y + 4.0 * s, s.max(1.0), r.h - 8.0 * s), th.fg);
        }
        cv.reset_clip();
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum FindAction {
    Next,
    Prev,
    Replace,
    ReplaceAll,
    ToggleCase,
    ToggleReplace,
    Close,
    FocusFind,
    FocusReplace,
}

#[derive(Default)]
pub struct FindBar {
    pub visible: bool,
    pub replace: bool,
    pub case: bool,
    pub focus: bool,
    /// 0 = find field, 1 = replace field.
    pub field: usize,
    pub find: TextField,
    pub repl: TextField,
    buttons: Vec<(Rect, FindAction)>,
}

impl FindBar {
    pub fn height(&self, g: &Gfx, s: f32) -> f32 {
        if !self.visible {
            return 0.0;
        }
        let row = (g.ui_line_h() + 14.0 * s).round();
        row * if self.replace { 2.0 } else { 1.0 } + 6.0 * s
    }

    pub fn draw(&mut self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, r: Rect, s: f32) {
        self.buttons.clear();
        cv.fill(r, th.chrome);
        cv.fill(Rect::new(r.x, r.y, r.w, s), th.border);
        let row = (g.ui_line_h() + 14.0 * s).round();
        let label_w = g.text_w("Replace").max(g.text_w("Find")) + 16.0 * s;
        let field_w = (r.w * 0.4).clamp(120.0 * s, 420.0 * s);
        let fh = row - 8.0 * s;
        let rows: &[(usize, &str)] = if self.replace { &[(0, "Find"), (1, "Replace")] } else { &[(0, "Find")] };
        for &(i, label) in rows {
            let y = r.y + 3.0 * s + i as f32 * row;
            g.text_in(cv, label, Rect::new(r.x + 10.0 * s, y, label_w, row), th.chrome_fg);
            let fr = Rect::new(r.x + 10.0 * s + label_w, y + 4.0 * s, field_w, fh);
            let field = if i == 0 { &self.find } else { &self.repl };
            field.draw(g, cv, th, fr, self.focus && self.field == i, s);
            self.buttons.push((fr, if i == 0 { FindAction::FocusFind } else { FindAction::FocusReplace }));
            let mut x = fr.right() + 8.0 * s;
            let btns: &[(&str, FindAction, bool)] = if i == 0 {
                &[
                    ("Next", FindAction::Next, false),
                    ("Previous", FindAction::Prev, false),
                    ("Match case", FindAction::ToggleCase, self.case),
                    (if self.replace { "Hide replace" } else { "Replace…" }, FindAction::ToggleReplace, false),
                ]
            } else {
                &[("Replace", FindAction::Replace, false), ("Replace all", FindAction::ReplaceAll, false)]
            };
            for &(t, a, on) in btns {
                let w = g.text_w(t) + 16.0 * s;
                let br = Rect::new(x, y + 4.0 * s, w, fh);
                if br.right() > r.right() - 30.0 * s {
                    break;
                }
                cv.fill(br, if on { th.accent } else { th.bg });
                cv.outline(br, s, th.border);
                g.text_in(cv, t, Rect::new(br.x + 8.0 * s, br.y, w, fh), if on { th.accent_fg } else { th.chrome_fg });
                self.buttons.push((br, a));
                x += w + 6.0 * s;
            }
        }
        let cr = Rect::new(r.right() - 28.0 * s, r.y + 3.0 * s + 4.0 * s, 22.0 * s, fh);
        g.text_in(cv, "✕", Rect::new(cr.x + 5.0 * s, cr.y, cr.w, cr.h), th.dim);
        self.buttons.push((cr, FindAction::Close));
    }

    pub fn hit(&self, p: (f32, f32)) -> Option<FindAction> {
        self.buttons.iter().find(|(r, _)| r.contains(p)).map(|(_, a)| *a)
    }

    pub fn field_mut(&mut self) -> &mut TextField {
        if self.field == 0 { &mut self.find } else { &mut self.repl }
    }
}

pub struct FontDialog {
    pub families: Vec<String>,
    pub filter: TextField,
    pub size: TextField,
    pub selected: Option<String>,
    pub scroll: usize,
    /// 0 = filter field, 1 = size field.
    pub field: usize,
    list: Rect,
    row_h: f32,
    rows: Vec<(Rect, String)>,
    buttons: Vec<(Rect, DialogAction)>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DialogAction {
    Ok,
    Cancel,
    FocusFilter,
    FocusSize,
}

impl FontDialog {
    pub fn new(families: Vec<String>, current: &str, pt: f32) -> Self {
        let mut size = TextField::default();
        size.set(&format!("{pt}"));
        let selected = Some(current.to_owned());
        let mut d = Self {
            families,
            filter: TextField::default(),
            size,
            selected,
            scroll: 0,
            field: 0,
            list: Rect::default(),
            row_h: 1.0,
            rows: Vec::new(),
            buttons: Vec::new(),
        };
        if let Some(i) = d.filtered().iter().position(|f| *f == current) {
            d.scroll = i.saturating_sub(3);
        }
        d
    }

    pub fn filtered(&self) -> Vec<&str> {
        let q = self.filter.text.to_lowercase();
        std::iter::once("")
            .chain(self.families.iter().map(String::as_str))
            .filter(|f| q.is_empty() || display_name(f).to_lowercase().contains(&q))
            .collect()
    }

    pub fn visible_rows(&self) -> usize {
        ((self.list.h / self.row_h).floor() as usize).max(1)
    }

    pub fn move_sel(&mut self, d: isize) {
        let list = self.filtered();
        if list.is_empty() {
            return;
        }
        let cur = self.selected.as_deref().and_then(|s| list.iter().position(|f| *f == s));
        let i = match cur {
            Some(i) => (i as isize + d).clamp(0, list.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(list[i].to_owned());
        let vis = self.visible_rows();
        if i < self.scroll {
            self.scroll = i;
        } else if i >= self.scroll + vis {
            self.scroll = i + 1 - vis;
        }
    }

    pub fn scroll_by(&mut self, d: isize) {
        let n = self.filtered().len();
        let max = n.saturating_sub(self.visible_rows());
        self.scroll = (self.scroll as isize + d).clamp(0, max as isize) as usize;
    }

    pub fn draw(
        &mut self,
        g: &mut Gfx,
        cv: &mut Canvas,
        th: &Theme,
        s: f32,
        preview: &mut dyn FnMut(&mut Gfx, &mut Canvas, Rect, &str, f32),
    ) {
        cv.fill(Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32), th.shade);
        let w = (440.0 * s).min(cv.w as f32 - 20.0 * s);
        let h = (460.0 * s).min(cv.h as f32 - 20.0 * s);
        let r = Rect::new(((cv.w as f32 - w) / 2.0).round(), ((cv.h as f32 - h) / 2.0).round(), w, h);
        cv.fill(r, th.chrome);
        cv.outline(r, s, th.border);
        self.buttons.clear();
        self.rows.clear();
        let lh = g.ui_line_h();
        let pad = 14.0 * s;
        let fh = lh + 10.0 * s;
        let mut y = r.y + pad;
        g.text(cv, "Font", r.x + pad, y, th.chrome_fg);
        y += lh + 8.0 * s;
        let fr = Rect::new(r.x + pad, y, r.w - 2.0 * pad, fh);
        if self.filter.text.is_empty() && self.field != 0 {
            g.text_in(cv, "Search fonts", Rect::new(fr.x + 6.0 * s, fr.y, fr.w, fr.h), th.dim);
            cv.outline(fr, s, th.border);
        } else {
            self.filter.draw(g, cv, th, fr, self.field == 0, s);
        }
        self.buttons.push((fr, DialogAction::FocusFilter));
        y += fh + 6.0 * s;
        let bottom_h = fh + 8.0 * s + 48.0 * s + 8.0 * s + fh + pad;
        self.list = Rect::new(r.x + pad, y, r.w - 2.0 * pad, (r.bottom() - bottom_h - y).max(lh));
        self.row_h = lh + 6.0 * s;
        cv.fill(self.list, th.field);
        cv.outline(self.list, s, th.border);
        cv.set_clip(self.list);
        let list: Vec<String> = self.filtered().into_iter().map(String::from).collect();
        let vis = self.visible_rows();
        for (k, f) in list.iter().enumerate().skip(self.scroll).take(vis + 1) {
            let rr =
                Rect::new(self.list.x, self.list.y + (k - self.scroll) as f32 * self.row_h, self.list.w, self.row_h);
            let sel = self.selected.as_deref() == Some(f.as_str());
            if sel {
                cv.fill(rr, th.accent);
            }
            g.text_in(
                cv,
                &display_name(f),
                Rect::new(rr.x + 8.0 * s, rr.y, rr.w, rr.h),
                if sel { th.accent_fg } else { th.fg },
            );
            self.rows.push((rr, f.to_string()));
        }
        cv.reset_clip();
        y = self.list.bottom() + 8.0 * s;
        let sw = g.text_w("Size (pt)") + 12.0 * s;
        g.text_in(cv, "Size (pt)", Rect::new(r.x + pad, y, sw, fh), th.chrome_fg);
        let sr = Rect::new(r.x + pad + sw, y, 70.0 * s, fh);
        self.size.draw(g, cv, th, sr, self.field == 1, s);
        self.buttons.push((sr, DialogAction::FocusSize));
        y += fh + 8.0 * s;
        let pr = Rect::new(r.x + pad, y, r.w - 2.0 * pad, 48.0 * s);
        cv.fill(pr, th.bg);
        cv.outline(pr, s, th.border);
        let pt: f32 = self.size.text.trim().parse().unwrap_or(11.0);
        cv.set_clip(pr);
        preview(
            g,
            cv,
            Rect::new(pr.x + 8.0 * s, pr.y, pr.w - 16.0 * s, pr.h),
            self.selected.as_deref().unwrap_or(""),
            pt,
        );
        cv.reset_clip();
        y += 48.0 * s + 8.0 * s;
        let mut x = r.right() - pad;
        for (t, a) in [("Cancel", DialogAction::Cancel), ("OK", DialogAction::Ok)] {
            let bw = g.text_w(t).max(50.0 * s) + 20.0 * s;
            x -= bw;
            let br = Rect::new(x, y, bw, fh);
            let primary = a == DialogAction::Ok;
            cv.fill(br, if primary { th.accent } else { th.bg });
            cv.outline(br, s, th.border);
            let tw = g.text_w(t);
            g.text_in(
                cv,
                t,
                Rect::new(br.x + (bw - tw) / 2.0, br.y, tw, fh),
                if primary { th.accent_fg } else { th.chrome_fg },
            );
            self.buttons.push((br, a));
            x -= 8.0 * s;
        }
    }

    pub fn hit(&mut self, p: (f32, f32)) -> Option<DialogAction> {
        if let Some((_, f)) = self.rows.iter().find(|(r, _)| r.contains(p)) {
            self.selected = Some(f.clone());
            return None;
        }
        self.buttons.iter().find(|(r, _)| r.contains(p)).map(|(_, a)| *a)
    }

    pub fn in_list(&self, p: (f32, f32)) -> bool {
        self.list.contains(p)
    }

    pub fn field_mut(&mut self) -> &mut TextField {
        if self.field == 0 { &mut self.filter } else { &mut self.size }
    }
}

pub fn display_name(f: &str) -> String {
    if f.is_empty() { "(Default monospace)".to_owned() } else { f.to_owned() }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Choice {
    Save,
    Discard,
    Cancel,
    /// Open a link despite the warning.
    Open,
}

/// A modal question with a row of buttons.
pub struct Prompt {
    /// Lines of text; long lines are wrapped to fit.
    pub lines: Vec<String>,
    choices: &'static [(&'static str, Choice)],
    pub focus: usize,
    /// Draw the first line as a warning.
    warn: bool,
    buttons: Vec<(Rect, Choice)>,
}

const SAVE_CHOICES: &[(&str, Choice)] =
    &[("Save", Choice::Save), ("Don't Save", Choice::Discard), ("Cancel", Choice::Cancel)];
const LINK_CHOICES: &[(&str, Choice)] = &[("Open Anyway", Choice::Open), ("Cancel", Choice::Cancel)];

/// Greedy word wrap of `text` to `max_w` pixels.
fn wrap(g: &mut Gfx, text: &str, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if !line.is_empty() && g.text_w(&candidate) > max_w {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    out.push(line);
    out
}

impl Prompt {
    /// "Save changes?" with Save / Don't Save / Cancel; Save is the default.
    pub fn save(msg: String) -> Self {
        Self { lines: vec![msg], choices: SAVE_CHOICES, focus: 0, warn: false, buttons: Vec::new() }
    }

    /// A warning about a suspicious link, with Open Anyway / Cancel; Cancel is the default.
    pub fn link(lines: Vec<String>) -> Self {
        Self { lines, choices: LINK_CHOICES, focus: 1, warn: true, buttons: Vec::new() }
    }

    pub fn focused(&self) -> Choice {
        self.choices[self.focus].1
    }

    pub fn cycle(&mut self, forward: bool) {
        let n = self.choices.len();
        self.focus = if forward { (self.focus + 1) % n } else { (self.focus + n - 1) % n };
    }

    pub fn draw(&mut self, g: &mut Gfx, cv: &mut Canvas, th: &Theme, s: f32) {
        cv.fill(Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32), th.shade);
        let lh = g.ui_line_h();
        let pad = 18.0 * s;
        let fh = lh + 12.0 * s;
        let bws: Vec<f32> = self.choices.iter().map(|(t, _)| g.text_w(t).max(60.0 * s) + 24.0 * s).collect();
        let btn_total: f32 = bws.iter().sum::<f32>() + 8.0 * s * (bws.len() as f32 - 1.0);
        let max_text = (560.0 * s).min(cv.w as f32 - 40.0 * s - 2.0 * pad).max(100.0 * s);
        let wrapped: Vec<(usize, String)> = self
            .lines
            .iter()
            .enumerate()
            .flat_map(|(i, l)| wrap(g, l, max_text).into_iter().map(move |w| (i, w)))
            .collect();
        let text_w = wrapped.iter().map(|(_, l)| g.text_w(l)).fold(0.0, f32::max);
        let w = (text_w + 2.0 * pad).max(btn_total + 2.0 * pad).min(cv.w as f32 - 20.0 * s);
        let h = pad + lh * wrapped.len() as f32 + pad + fh + pad;
        let r = Rect::new(((cv.w as f32 - w) / 2.0).round(), ((cv.h as f32 - h) / 2.5).round().max(0.0), w, h);
        cv.fill(r, th.chrome);
        cv.outline(r, s, if self.warn { th.danger } else { th.border });
        cv.set_clip(r);
        for (k, (i, l)) in wrapped.iter().enumerate() {
            let color = if self.warn && *i == 0 { th.danger } else { th.chrome_fg };
            g.text(cv, l, r.x + pad, r.y + pad + k as f32 * lh, color);
        }
        cv.reset_clip();
        self.buttons.clear();
        let mut x = r.right() - pad - btn_total;
        let y = r.bottom() - pad - fh;
        for (i, ((t, c), bw)) in self.choices.iter().zip(&bws).enumerate() {
            let br = Rect::new(x, y, *bw, fh);
            let f = i == self.focus;
            cv.fill(br, if f { th.accent } else { th.bg });
            cv.outline(br, s, th.border);
            let tw = g.text_w(t);
            g.text_in(cv, t, Rect::new(br.x + (bw - tw) / 2.0, y, tw, fh), if f { th.accent_fg } else { th.chrome_fg });
            self.buttons.push((br, *c));
            x += bw + 8.0 * s;
        }
    }

    pub fn hit(&self, p: (f32, f32)) -> Option<Choice> {
        self.buttons.iter().find(|(r, _)| r.contains(p)).map(|(_, c)| *c)
    }
}

pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
    Strip,
}

/// The strip of document tabs under the menu bar.
#[derive(Default)]
pub struct TabBar {
    /// (tab rect, close-button rect) for each tab, in screen coordinates.
    rects: Vec<(Rect, Rect)>,
    new_btn: Rect,
    strip: Rect,
    /// Horizontal scroll of the strip when tabs overflow.
    offset: f32,
    max_offset: f32,
    /// Scroll the active tab into view on the next draw.
    pub reveal: bool,
}

impl TabBar {
    pub fn height(g: &Gfx, s: f32) -> f32 {
        (g.ui_line_h() + 14.0 * s).round()
    }

    /// `items` holds (name, modified) for each tab.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        g: &mut Gfx,
        cv: &mut Canvas,
        th: &Theme,
        r: Rect,
        s: f32,
        items: &[(String, bool)],
        active: usize,
        mouse: (f32, f32),
    ) {
        cv.fill(r, th.chrome);
        cv.fill(Rect::new(r.x, r.bottom() - s, r.w, s), th.border);
        self.strip = r;
        let btn = r.h - 6.0 * s;
        let avail = (r.w - btn - 12.0 * s).max(0.0);
        let close_w = 26.0 * s;
        let (min_w, max_w) = (90.0 * s, 220.0 * s);
        let mut widths: Vec<f32> =
            items.iter().map(|(n, _)| (g.text_w(n) + 14.0 * s + close_w).clamp(min_w, max_w)).collect();
        let total: f32 = widths.iter().sum();
        if total > avail && !items.is_empty() {
            let w = (avail / items.len() as f32).max(min_w);
            for x in &mut widths {
                *x = x.min(w);
            }
        }
        let total: f32 = widths.iter().sum();
        self.max_offset = (total - avail).max(0.0);
        if std::mem::take(&mut self.reveal) && active < widths.len() {
            let x0: f32 = widths[..active].iter().sum();
            let x1 = x0 + widths[active];
            if x0 < self.offset {
                self.offset = x0;
            } else if x1 > self.offset + avail {
                self.offset = x1 - avail;
            }
        }
        self.offset = self.offset.clamp(0.0, self.max_offset);

        self.rects.clear();
        let strip = Rect::new(r.x, r.y, avail + 4.0 * s, r.h - s);
        let top = r.y + 4.0 * s;
        let h = r.bottom() - s - top;
        let mut x = r.x + 4.0 * s - self.offset;
        for (i, ((name, dirty), w)) in items.iter().zip(&widths).enumerate() {
            let tr = Rect::new(x, top, *w, h);
            let cr = Rect::new(tr.right() - close_w + 4.0 * s, top + (h - 18.0 * s) / 2.0, 18.0 * s, 18.0 * s);
            x += w;
            self.rects.push((tr, cr));
            let hovered = tr.contains(mouse) && strip.contains(mouse);
            cv.set_clip(strip);
            if i == active {
                cv.fill(Rect::new(tr.x, tr.y, tr.w, tr.h + s), th.bg);
                cv.fill(Rect::new(tr.x, tr.y, tr.w, (2.0 * s).round()), th.accent);
            } else {
                if hovered {
                    cv.fill(tr, th.hover);
                }
                if i + 1 != active {
                    cv.fill(Rect::new(tr.right() - s, tr.y + h * 0.25, s.max(1.0), h * 0.5), th.border);
                }
            }
            let color = if i == active { th.fg } else { th.dim };
            cv.set_clip(Rect::new(tr.x, tr.y, (tr.w - close_w).max(0.0), tr.h));
            g.text_in(cv, name, Rect::new(tr.x + 10.0 * s, tr.y, tr.w, tr.h), color);
            cv.set_clip(strip);
            let over_close = cr.contains(mouse);
            let glyph = if *dirty && !over_close {
                Some("●")
            } else if i == active || hovered {
                Some("×")
            } else {
                None
            };
            if over_close {
                cv.fill(cr, th.hover);
            }
            if let Some(gl) = glyph {
                let gw = g.text_w(gl);
                g.text_in(cv, gl, Rect::new(cr.x + (cr.w - gw) / 2.0, cr.y, gw, cr.h), color);
            }
            cv.reset_clip();
        }
        let bx = (r.x + 4.0 * s + total - self.offset).min(r.x + avail + 4.0 * s) + 4.0 * s;
        self.new_btn = Rect::new(bx, r.y + 3.0 * s, btn, btn);
        if self.new_btn.contains(mouse) {
            cv.fill(self.new_btn, th.hover);
        }
        let pw = g.text_w("+");
        g.text_in(cv, "+", Rect::new(bx + (btn - pw) / 2.0, self.new_btn.y, pw, btn), th.chrome_fg);
        // Keep hit-testing to the visible part of the strip.
        for (tr, cr) in &mut self.rects {
            let x0 = tr.x.max(strip.x);
            let x1 = tr.right().min(strip.right());
            *tr = Rect::new(x0, tr.y, (x1 - x0).max(0.0), tr.h);
            if !strip.contains((cr.x + cr.w / 2.0, cr.y)) {
                *cr = Rect::default();
            }
        }
    }

    pub fn hit(&self, p: (f32, f32)) -> Option<TabHit> {
        if let Some(i) = self.rects.iter().position(|(_, c)| c.contains(p)) {
            return Some(TabHit::Close(i));
        }
        if let Some(i) = self.rects.iter().position(|(t, _)| t.contains(p)) {
            return Some(TabHit::Tab(i));
        }
        if self.new_btn.contains(p) {
            return Some(TabHit::New);
        }
        self.strip.contains(p).then_some(TabHit::Strip)
    }

    pub fn scroll(&mut self, d: f32) {
        self.offset = (self.offset + d).clamp(0.0, self.max_offset);
    }
}
