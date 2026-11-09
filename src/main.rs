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
use ui::{Choice, Cmd, DialogAction, FieldKey, FindAction, FindBar, FontDialog, MenuHit, MenuState, NavKey, Prompt};
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
}

struct App {
    gfx: Gfx,
    view: View,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    settings: Settings,
    theme: Theme,
    scale: f32,
    zoom: f32,
    mods: ModifiersState,
    mouse: (f32, f32),
    drag: Drag,
    clicks: (Instant, (f32, f32), u8),
    path: Option<PathBuf>,
    dropped: Option<PathBuf>,
    encoding: Encoding,
    eol: Eol,
    mixed_eol: bool,
    meta_dirty: bool,
    menu: MenuState,
    find: FindBar,
    font_dlg: Option<FontDialog>,
    prompt: Option<Prompt>,
    pending: Option<Cmd>,
    status_msg: Option<String>,
    hover_url: Option<(usize, Range<usize>)>,
    preedit: String,
    clipboard: Option<arboard::Clipboard>,
    layout: Layout,
    applied: (f32, f32, f32, f32, bool),
    title: String,
    cursor_icon: CursorIcon,
    focused: bool,
    needs_redraw: bool,
    exit: bool,
}

impl App {
    fn new(settings: Settings, file: Option<PathBuf>) -> Self {
        let mut gfx = Gfx::new();
        let view = View::new(&mut gfx.fs, &settings.font_family, settings.wrap);
        let mut app = App {
            gfx,
            view,
            window: None,
            surface: None,
            settings,
            theme: Theme::LIGHT,
            scale: 1.0,
            zoom: 1.0,
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            drag: Drag::None,
            clicks: (Instant::now(), (0.0, 0.0), 0),
            path: None,
            dropped: None,
            encoding: Encoding::Utf8,
            eol: Eol::platform(),
            mixed_eol: false,
            meta_dirty: false,
            menu: MenuState::default(),
            find: FindBar::default(),
            font_dlg: None,
            prompt: None,
            pending: None,
            status_msg: None,
            hover_url: None,
            preedit: String::new(),
            clipboard: None,
            layout: Layout::default(),
            applied: (0.0, 0.0, 0.0, 0.0, false),
            title: String::new(),
            cursor_icon: CursorIcon::Default,
            focused: true,
            needs_redraw: true,
            exit: false,
        };
        if let Some(p) = file {
            if p.exists() {
                app.open_path(p);
            } else {
                app.path = Some(p);
            }
        }
        app
    }

    fn primary(&self) -> bool {
        if MAC { self.mods.super_key() } else { self.mods.control_key() }
    }

    // ---------------------------------------------------------------- document

    fn doc_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    fn is_dirty(&self) -> bool {
        self.meta_dirty || self.view.doc.is_dirty()
    }

    fn set_doc(&mut self, doc: document::Document) {
        self.view.set_document(doc);
        self.meta_dirty = false;
        self.hover_url = None;
    }

    fn new_doc(&mut self) {
        self.set_doc(document::Document::new(""));
        self.path = None;
        self.encoding = Encoding::Utf8;
        self.eol = Eol::platform();
        self.mixed_eol = false;
        self.status_msg = None;
    }

    fn open_path(&mut self, path: PathBuf) {
        match doc::load(&path) {
            Ok(l) => {
                self.set_doc(document::Document::from_rope(l.rope));
                self.encoding = l.encoding;
                self.eol = l.eol;
                self.mixed_eol = l.mixed_eol;
                self.path = Some(path);
                self.status_msg = l
                    .mixed_eol
                    .then(|| format!("Mixed line endings; will be saved as {}", l.eol.label()));
            }
            Err(e) => self.status_msg = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()) {
            d = d.set_directory(dir);
        }
        if let Some(w) = &self.window {
            d = d.set_parent(&**w);
        }
        d
    }

    fn open_dialog(&mut self) {
        if let Some(p) = self.dialog().set_title("Open").pick_file() {
            self.open_path(p);
        }
    }

    fn save(&mut self) -> bool {
        match self.path.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let name = self.path.as_ref().map_or("Untitled.txt".into(), |_| self.doc_name());
        match self.dialog().set_title("Save As").set_file_name(name).save_file() {
            Some(p) => self.save_to(p),
            None => false,
        }
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        let text = self.view.doc.to_string_eol(self.eol.as_str());
        let bytes = match doc::encode(&text, self.encoding) {
            Ok(b) => b,
            Err(c) => {
                self.status_msg =
                    Some(format!("'{c}' can't be stored as {}; saved as UTF-8 instead", self.encoding.label()));
                self.encoding = Encoding::Utf8;
                text.into_bytes()
            }
        };
        match doc::save(&path, &bytes) {
            Ok(()) => {
                self.path = Some(path);
                self.view.doc.mark_saved();
                self.meta_dirty = false;
                self.mixed_eol = false;
                true
            }
            Err(e) => {
                self.status_msg = Some(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn rename(&mut self) {
        let Some(old) = self.path.clone().filter(|p| p.exists()) else {
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
                self.path = Some(new);
                self.status_msg = Some(format!("Renamed to {}", self.doc_name()));
            }
            Err(e) => self.status_msg = Some(format!("Could not rename: {e}")),
        }
    }

    fn print(&mut self) {
        let title = self.doc_name();
        let doc = &self.view.doc;
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
        self.status_msg = Some(r.unwrap_or_else(|e| e));
    }

    /// Returns true if `cmd` may proceed now; otherwise asks to save first.
    fn guard(&mut self, cmd: Cmd) -> bool {
        if !self.is_dirty() {
            return true;
        }
        self.prompt = Some(Prompt::new(format!("Do you want to save changes to {}?", self.doc_name())));
        self.pending = Some(cmd);
        false
    }

    fn run_unguarded(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::New => self.new_doc(),
            Cmd::Open => match self.dropped.take() {
                Some(p) => self.open_path(p),
                None => self.open_dialog(),
            },
            Cmd::Exit => self.exit = true,
            _ => self.run(cmd),
        }
    }

    fn choose(&mut self, c: Choice) {
        self.prompt = None;
        let pending = self.pending.take();
        match c {
            Choice::Save => {
                if self.save()
                    && let Some(p) = pending
                {
                    self.run_unguarded(p);
                }
            }
            Choice::Discard => {
                if let Some(p) = pending {
                    self.run_unguarded(p);
                }
            }
            Choice::Cancel => self.dropped = None,
        }
    }

    // ---------------------------------------------------------------- editing

    fn edited(&mut self) {
        self.status_msg = None;
        self.hover_url = None;
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
        if let Some(t) = self.view.selected_text() {
            self.set_clipboard(t);
        }
    }

    // ---------------------------------------------------------------- find

    fn open_find(&mut self, replace: bool) {
        self.find.visible = true;
        self.find.replace |= replace;
        self.find.focus = true;
        self.find.field = 0;
        let sel = self.view.selection().filter(|(a, z)| a.line == z.line);
        if let Some(sel) = sel.and_then(|_| self.view.selected_text()) {
            self.find.find.set(&sel);
        } else {
            self.find.find.all = true;
        }
    }

    fn find_next(&mut self, forward: bool) -> bool {
        let needle = self.find.find.text.clone();
        if needle.is_empty() {
            self.open_find(false);
            return false;
        }
        let case = self.find.case;
        let cur = self.view.cursor;
        let (start, end) = self.view.selection().unwrap_or((cur, cur));
        let doc = &self.view.doc;
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
                self.view.set_selection(Cursor::new(li, r.start), Cursor::new(li, r.end));
                self.status_msg = None;
                true
            }
            None => {
                self.status_msg = Some(format!("Cannot find \"{needle}\""));
                false
            }
        }
    }

    fn replace_one(&mut self) {
        let needle = self.find.find.text.clone();
        let repl = self.find.repl.text.clone();
        let sel = self.view.selected_text().unwrap_or_default();
        if !sel.is_empty() && search::find(&sel, &needle, self.find.case, 0) == Some(0..sel.len()) {
            self.view.replace_selection(&repl, false);
            self.edited();
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        let needle = self.find.find.text.clone();
        let repl = self.find.repl.text.clone();
        if needle.is_empty() {
            return;
        }
        let case = self.find.case;
        let doc = &self.view.doc;
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
            self.status_msg = Some(format!("Cannot find \"{needle}\""));
            return;
        }
        self.view.replace_many(&ranges, &repl);
        self.edited();
        self.status_msg = Some(format!("Replaced {} occurrence(s)", ranges.len()));
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
            FindAction::ToggleCase => self.find.case = !self.find.case,
            FindAction::ToggleReplace => {
                self.find.replace = !self.find.replace;
                self.find.field = 0;
            }
            FindAction::Close => {
                self.find.visible = false;
                self.find.focus = false;
            }
            FindAction::FocusFind | FindAction::FocusReplace => {
                self.find.focus = true;
                self.find.field = (a == FindAction::FocusReplace) as usize;
            }
        }
    }

    // ---------------------------------------------------------------- commands

    fn run(&mut self, cmd: Cmd) {
        self.menu.close();
        match cmd {
            Cmd::New | Cmd::Open | Cmd::Exit => {
                if self.guard(cmd) {
                    self.run_unguarded(cmd);
                }
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
                self.view.undo();
                self.edited();
            }
            Cmd::Redo => {
                self.view.redo();
                self.edited();
            }
            Cmd::Cut => {
                self.copy();
                self.view.delete_selection();
                self.edited();
            }
            Cmd::Copy => self.copy(),
            Cmd::Paste => {
                if let Some(t) = self.get_clipboard() {
                    self.view.replace_selection(&t, false);
                    self.edited();
                }
            }
            Cmd::Delete => {
                self.view.delete();
                self.edited();
            }
            Cmd::SelectAll => self.view.select_all(),
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
                self.font_dlg = Some(FontDialog::new(fams, &self.settings.font_family, self.settings.font_pt));
            }
            Cmd::ZoomIn => self.zoom = (self.zoom + 0.1).min(5.0),
            Cmd::ZoomOut => self.zoom = (self.zoom - 0.1).max(0.3),
            Cmd::ZoomReset => self.zoom = 1.0,
            Cmd::StatusBar => {
                self.settings.status_bar = !self.settings.status_bar;
                self.settings.save();
            }
            Cmd::SetEol(e) => {
                if e != self.eol || self.mixed_eol {
                    self.eol = e;
                    self.mixed_eol = false;
                    self.meta_dirty = true;
                }
            }
            Cmd::SetEncoding(e) => {
                if e != self.encoding {
                    self.encoding = e;
                    self.meta_dirty = true;
                }
            }
        }
    }

    fn checked(&self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::WordWrap => self.settings.wrap,
            Cmd::StatusBar => self.settings.status_bar,
            Cmd::SetEol(e) => e == self.eol,
            Cmd::SetEncoding(e) => e == self.encoding,
            _ => false,
        }
    }

    fn apply_font(&mut self) {
        let Some(d) = self.font_dlg.take() else { return };
        if let Some(f) = d.selected {
            self.settings.font_family = f;
        }
        if let Ok(pt) = d.size.text.trim().parse::<f32>() {
            self.settings.font_pt = pt.clamp(4.0, 200.0);
        }
        self.settings.save();
        self.view.set_font(&self.settings.font_family);
        self.applied.2 = 0.0; // force metrics refresh
    }

    // ---------------------------------------------------------------- keyboard

    fn shortcut(&self, ev: &KeyEvent, c: Option<char>, primary: bool, shift: bool, alt: bool) -> Option<Cmd> {
        if ev.logical_key == Key::Named(NamedKey::F3) {
            return Some(if shift { Cmd::FindPrev } else { Cmd::FindNext });
        }
        if primary && !alt {
            return Some(match c? {
                'n' => Cmd::New,
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
        self.needs_redraw = true;
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

        if let Some(p) = &mut self.prompt {
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

        if let Some(d) = &mut self.font_dlg {
            match named {
                Some(NamedKey::Escape) => self.font_dlg = None,
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
            if let Some(d) = &mut self.font_dlg
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

        if self.menu.open.is_some() {
            let nav = match named {
                Some(NamedKey::ArrowUp) => Some(NavKey::Up),
                Some(NamedKey::ArrowDown) => Some(NavKey::Down),
                Some(NamedKey::ArrowLeft) => Some(NavKey::Left),
                Some(NamedKey::ArrowRight) => Some(NavKey::Right),
                Some(NamedKey::Enter | NamedKey::Space) => Some(NavKey::Enter),
                Some(NamedKey::Escape | NamedKey::Alt) => {
                    self.menu.close();
                    None
                }
                _ => None,
            };
            if let Some(cmd) = nav.and_then(|n| self.menu.key(n)) {
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
            self.menu.open = Some(i);
            self.menu.hover = None;
            self.menu.key(NavKey::Down);
            return;
        }

        let in_find = self.find.visible && self.find.focus;
        if let Some(cmd) = self.shortcut(&ev, c, primary, shift, alt) {
            if in_find && matches!(cmd, Cmd::SelectAll | Cmd::Copy | Cmd::Cut | Cmd::Paste) {
                match cmd {
                    Cmd::SelectAll => self.find.field_mut().key(FieldKey::SelectAll),
                    Cmd::Paste => {
                        if let Some(t) = self.get_clipboard() {
                            let first = t.lines().next().unwrap_or("").to_owned();
                            self.find.field_mut().key(FieldKey::Insert(&first));
                        }
                    }
                    _ => {
                        let f = self.find.field_mut().clone();
                        if f.all {
                            self.set_clipboard(f.text);
                            if cmd == Cmd::Cut {
                                self.find.field_mut().key(FieldKey::Delete);
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
                Some(NamedKey::Enter) if self.find.field == 1 => self.replace_one(),
                Some(NamedKey::Enter) => {
                    self.find_next(!shift);
                }
                Some(NamedKey::Tab) if self.find.replace => self.find.field = 1 - self.find.field,
                Some(n) => {
                    if let Some(k) = field_key(n) {
                        self.find.field_mut().key(k);
                    } else if let (true, Some(t)) = (typing, text) {
                        self.find.field_mut().key(FieldKey::Insert(t));
                    }
                }
                None => {
                    if let (true, Some(t)) = (typing, text) {
                        self.find.field_mut().key(FieldKey::Insert(t));
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
            self.view.motion(&mut self.gfx.fs, m, shift);
            return;
        }
        let edited = match named {
            Some(NamedKey::Backspace) if word => {
                self.view.delete_word(&mut self.gfx.fs, false);
                true
            }
            Some(NamedKey::Backspace) => {
                self.view.backspace();
                true
            }
            Some(NamedKey::Delete) if word => {
                self.view.delete_word(&mut self.gfx.fs, true);
                true
            }
            Some(NamedKey::Delete) => {
                self.view.delete();
                true
            }
            Some(NamedKey::Enter) => {
                self.view.replace_selection("\n", false);
                true
            }
            Some(NamedKey::Tab) => {
                if !primary {
                    self.view.type_text("\t");
                }
                true
            }
            Some(NamedKey::Escape) => {
                if self.find.visible {
                    self.find_action(FindAction::Close);
                } else {
                    self.view.clear_selection();
                }
                false
            }
            _ => match text {
                Some(t) if typing => {
                    self.view.type_text(t);
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
        let pad = (6.0 * self.scale).round();
        (p.0 - self.layout.text.x - pad + self.view.h_scroll(), p.1 - self.layout.text.y)
    }

    fn url_at(&self, p: (f32, f32)) -> Option<(usize, Range<usize>, String)> {
        if !self.layout.text.contains(p) {
            return None;
        }
        let (x, y) = self.text_point(p);
        self.view.url_at(x, y)
    }

    /// (thumb rect, track travel, total lines)
    fn scrollbar(&self) -> Option<(Rect, f32, usize)> {
        let (total, line) = self.view.scroll_info();
        let lh = self.view.line_h();
        let track = self.layout.vbar;
        let vis = (track.h / lh).max(1.0);
        if total as f32 <= vis && line == 0 {
            return None;
        }
        let th = (track.h * (vis / (total as f32 + vis)).min(1.0)).max(24.0 * self.scale).min(track.h);
        let y = track.y + (track.h - th) * line as f32 / (total.saturating_sub(1).max(1)) as f32;
        Some((Rect::new(track.x + 2.0 * self.scale, y, track.w - 4.0 * self.scale, th), track.h - th, total))
    }

    fn update_cursor_icon(&mut self) {
        let p = self.mouse;
        let icon = if self.prompt.is_some() || self.font_dlg.is_some() || self.menu.open.is_some() {
            CursorIcon::Default
        } else if self.layout.text.contains(p) {
            if self.primary() && self.hover_url.is_some() { CursorIcon::Pointer } else { CursorIcon::Text }
        } else if self.find.visible
            && self.find.hit(p).is_some_and(|a| matches!(a, FindAction::FocusFind | FindAction::FocusReplace))
        {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            if let Some(w) = &self.window {
                w.set_cursor(icon);
            }
        }
    }

    fn on_mouse_move(&mut self, p: (f32, f32)) {
        self.mouse = p;
        if self.menu.open.is_some() && self.menu.hover_at(p) {
            self.needs_redraw = true;
        }
        match self.drag {
            Drag::Text => {
                let t = self.layout.text;
                if p.1 < t.y {
                    self.view.scroll_px(-(t.y - p.1));
                } else if p.1 > t.bottom() {
                    self.view.scroll_px(p.1 - t.bottom());
                }
                self.view.prepare(&mut self.gfx.fs);
                let (x, y) = self.text_point(p);
                self.view.drag(x, y.clamp(0.0, t.h - 1.0));
                self.needs_redraw = true;
            }
            Drag::Thumb(grab) => {
                if let Some((_, range, total)) = self.scrollbar() {
                    let frac = ((p.1 - grab - self.layout.vbar.y) / range.max(1.0)).clamp(0.0, 1.0);
                    self.view.scroll_to_line((frac * (total - 1) as f32).round() as usize);
                    self.needs_redraw = true;
                }
            }
            Drag::None => {
                let hover = self.url_at(p).map(|(l, r, _)| (l, r));
                if hover != self.hover_url {
                    self.hover_url = hover;
                    self.needs_redraw = true;
                }
            }
        }
        self.update_cursor_icon();
    }

    fn on_mouse_button(&mut self, state: ElementState, button: MouseButton) {
        self.needs_redraw = true;
        let p = self.mouse;
        if state == ElementState::Released {
            self.drag = Drag::None;
            return;
        }
        // Click counting for double/triple click.
        let now = Instant::now();
        let (t, lp, n) = self.clicks;
        let n = if now - t < Duration::from_millis(450) && (p.0 - lp.0).abs() < 5.0 && (p.1 - lp.1).abs() < 5.0 {
            n % 3 + 1
        } else {
            1
        };
        self.clicks = (now, p, n);

        if let Some(pr) = &self.prompt {
            if let Some(c) = pr.hit(p) {
                self.choose(c);
            }
            return;
        }
        if let Some(d) = &mut self.font_dlg {
            match d.hit(p) {
                Some(DialogAction::Ok) => self.apply_font(),
                Some(DialogAction::Cancel) => self.font_dlg = None,
                Some(DialogAction::FocusFilter) => d.field = 0,
                Some(DialogAction::FocusSize) => d.field = 1,
                None if n == 2 && d.in_list(p) => self.apply_font(),
                None => {}
            }
            return;
        }
        if button != MouseButton::Left {
            if self.menu.open.is_some() {
                self.menu.close();
            }
            return;
        }
        match self.menu.hit(p) {
            MenuHit::Title(i) => {
                self.menu.open = if self.menu.open == Some(i) { None } else { Some(i) };
                self.menu.hover = None;
                return;
            }
            MenuHit::Item(cmd) => return self.run(cmd),
            MenuHit::Inside => return,
            MenuHit::Outside if self.menu.open.is_some() => return self.menu.close(),
            MenuHit::Outside => {}
        }
        if self.find.visible && self.layout.find.contains(p) {
            if let Some(a) = self.find.hit(p) {
                self.find_action(a);
            }
            return;
        }
        if self.layout.vbar.contains(p) {
            if let Some((thumb, _, _)) = self.scrollbar() {
                if p.1 >= thumb.y && p.1 < thumb.bottom() {
                    self.drag = Drag::Thumb(p.1 - thumb.y);
                } else {
                    let page = self.layout.text.h * 0.9;
                    self.view.scroll_px(if p.1 < thumb.y { -page } else { page });
                }
            }
            return;
        }
        if self.layout.text.contains(p) {
            self.find.focus = false;
            if self.primary()
                && let Some((_, _, url)) = self.url_at(p)
            {
                let target = search::url_target(&url);
                if let Err(e) = open::that_detached(&target) {
                    self.status_msg = Some(format!("Could not open {target}: {e}"));
                }
                return;
            }
            let (x, y) = self.text_point(p);
            self.view.click(x, y, n, self.mods.shift_key());
            self.drag = Drag::Text;
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let lh = self.view.line_h();
        let (mut dx, mut dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * 3.0 * lh, y * 3.0 * lh),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        if let Some(d) = &mut self.font_dlg {
            d.scroll_by(if dy > 0.0 { -3 } else { 3 });
            self.needs_redraw = true;
            return;
        }
        if self.prompt.is_some() {
            return;
        }
        if self.primary() {
            if dy != 0.0 {
                self.run(if dy > 0.0 { Cmd::ZoomIn } else { Cmd::ZoomOut });
                self.needs_redraw = true;
            }
            return;
        }
        if self.mods.shift_key() && dx == 0.0 {
            (dx, dy) = (dy, 0.0);
        }
        if dy != 0.0 {
            self.view.scroll_px(-dy);
        }
        if dx != 0.0 && !self.settings.wrap {
            self.view.scroll_h(-dx);
        }
        self.hover_url = None;
        self.needs_redraw = true;
    }

    // ---------------------------------------------------------------- drawing

    fn compute_layout(&mut self, w: f32, h: f32) {
        let s = self.scale;
        let lh = self.gfx.ui_line_h();
        let menu_h = (lh + 8.0 * s).round();
        let status_h = if self.settings.status_bar { (lh + 6.0 * s).round() } else { 0.0 };
        let find_h = self.find.height(&self.gfx, s).round();
        let vbar_w = (12.0 * s).round();
        let text_h = (h - menu_h - find_h - status_h).max(1.0);
        self.layout = Layout {
            menu: Rect::new(0.0, 0.0, w, menu_h),
            text: Rect::new(0.0, menu_h, (w - vbar_w).max(1.0), text_h),
            vbar: Rect::new(w - vbar_w, menu_h, vbar_w, text_h),
            find: Rect::new(0.0, menu_h + text_h, w, find_h),
            status: Rect::new(0.0, h - status_h, w, status_h),
        };
        let pad = (6.0 * s).round();
        let font_px = (self.settings.font_pt * self.zoom * 4.0 / 3.0 * s).max(4.0);
        let line_h = (font_px * 1.3).ceil();
        let want = ((self.layout.text.w - 2.0 * pad).max(1.0), text_h, font_px, line_h, self.settings.wrap);
        if want != self.applied {
            self.applied = want;
            self.view.set_geometry(font_px, line_h, want.0, want.1, want.4);
        }
    }

    fn draw_editor(&mut self, cv: &mut Canvas) {
        let th = self.theme;
        let s = self.scale;
        let area = self.layout.text;
        let pad = (6.0 * s).round();
        let show_caret =
            self.focused && !(self.find.visible && self.find.focus) && self.prompt.is_none() && self.font_dlg.is_none();
        self.view.prepare(&mut self.gfx.fs);
        let caret =
            self.view.draw(&mut self.gfx.fs, &mut self.gfx.cache, cv, area, pad, &th, s, self.hover_url.as_ref());
        if let Some((x, y, h)) = caret {
            cv.set_clip(area);
            if !self.preedit.is_empty() {
                let w = self.gfx.text_w(&self.preedit);
                let r = Rect::new(x, y, w + 4.0 * s, h);
                cv.fill(r, th.field);
                self.gfx.text_in(cv, &self.preedit, Rect::new(x + 2.0 * s, y, w, h), th.fg);
                cv.fill(Rect::new(x, r.bottom() - 2.0 * s, r.w, s.max(1.0)), th.fg);
            } else if show_caret {
                cv.fill(Rect::new(x, y, s.max(1.0).round(), h), th.fg);
            }
            cv.reset_clip();
            if let Some(w) = &self.window {
                w.set_ime_cursor_area(PhysicalPosition::new(x as f64, y as f64), PhysicalSize::new(1.0, h as f64));
            }
        }

        // Scrollbar.
        cv.fill(self.layout.vbar, th.bg);
        if let Some((thumb, _, _)) = self.scrollbar() {
            cv.fill(thumb, if matches!(self.drag, Drag::Thumb(_)) { th.dim } else { th.border });
        }
    }

    fn draw_status(&mut self, cv: &mut Canvas) {
        let r = self.layout.status;
        if r.h <= 0.0 {
            return;
        }
        let (th, s) = (self.theme, self.scale);
        cv.fill(r, th.chrome);
        cv.fill(Rect::new(r.x, r.y, r.w, s.max(1.0)), th.border);
        let cur = self.view.cursor;
        let doc = &self.view.doc;
        let col = doc.line(cur.line)[..cur.index].chars().count() + 1;
        let selected = self.view.selection().map(|(a, z)| doc.chars_between(a, z));
        let left = match &self.status_msg {
            Some(m) => m.clone(),
            None => match selected {
                Some(n) => format!("Ln {}, Col {}   ({n} selected)", cur.line + 1, col),
                None => format!("Ln {}, Col {}", cur.line + 1, col),
            },
        };
        let row = Rect::new(r.x + 10.0 * s, r.y, r.w, r.h);
        cv.set_clip(r);
        let eol = if self.mixed_eol { format!("{} (mixed)", self.eol.label()) } else { self.eol.label().to_owned() };
        let segs = [format!("{}%", (self.zoom * 100.0).round()), eol, self.encoding.label().to_owned()];
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
        let Some(win) = self.window.clone() else { return };
        let Some(mut surface) = self.surface.take() else { return };
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
        self.surface = Some(surface);
        let title = format!("{}{} - {APP}", if self.is_dirty() { "*" } else { "" }, self.doc_name());
        if title != self.title {
            win.set_title(&title);
            self.title = title;
        }
    }

    fn paint(&mut self, cv: &mut Canvas) {
        let (th, s) = (self.theme, self.scale);
        cv.fill(Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32), th.bg);
        self.draw_editor(cv);
        if self.find.visible {
            self.find.draw(&mut self.gfx, cv, &th, self.layout.find, s);
        }
        self.draw_status(cv);
        self.menu.draw_bar(&mut self.gfx, cv, &th, self.layout.menu, s);
        if self.menu.open.is_some() {
            let states: Vec<(Cmd, bool)> = ui::MENUS
                .iter()
                .flat_map(|(_, items)| items.iter().filter_map(|i| i.cmd))
                .map(|c| (c, self.checked(c)))
                .collect();
            self.menu.draw_dropdown(&mut self.gfx, cv, &th, s, |c| states.iter().any(|(k, v)| *k == c && *v));
        }
        if let Some(d) = &mut self.font_dlg {
            d.draw(&mut self.gfx, cv, &th, s, &mut |g, cv, r, fam, pt| draw_preview(g, cv, r, fam, pt, s, th.fg));
        }
        if let Some(p) = &mut self.prompt {
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
        KeyCode::KeyV => 'v',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Equal | KeyCode::NumpadAdd => '=',
        KeyCode::Minus | KeyCode::NumpadSubtract => '-',
        KeyCode::Digit0 | KeyCode::Numpad0 => '0',
        _ => return None,
    })
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(APP)
            .with_inner_size(LogicalSize::new(self.settings.win_w.max(320), self.settings.win_h.max(200)))
            .with_min_inner_size(LogicalSize::new(320, 200));
        let win = Rc::new(el.create_window(attrs).expect("failed to create window"));
        let ctx = softbuffer::Context::new(win.clone()).expect("failed to create graphics context");
        let surface = softbuffer::Surface::new(&ctx, win.clone()).expect("failed to create surface");
        win.set_ime_allowed(true);
        self.scale = win.scale_factor() as f32;
        self.gfx.set_ui_scale(self.scale);
        self.theme = if win.theme() == Some(winit::window::Theme::Dark) { Theme::DARK } else { Theme::LIGHT };
        self.window = Some(win.clone());
        self.surface = Some(surface);
        win.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                self.redraw();
                return;
            }
            WindowEvent::CloseRequested => self.run(Cmd::Exit),
            WindowEvent::Resized(sz) => {
                let l = sz.to_logical::<f64>(self.scale as f64);
                self.settings.win_w = l.width.round() as u32;
                self.settings.win_h = l.height.round() as u32;
                self.needs_redraw = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor as f32;
                self.gfx.set_ui_scale(self.scale);
                self.needs_redraw = true;
            }
            WindowEvent::ThemeChanged(t) => {
                self.theme = if t == winit::window::Theme::Dark { Theme::DARK } else { Theme::LIGHT };
                self.needs_redraw = true;
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                self.needs_redraw = true;
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                self.update_cursor_icon();
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => self.on_key(event),
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _) => self.preedit = text,
                    Ime::Commit(text) => {
                        self.preedit.clear();
                        if self.prompt.is_none() && self.font_dlg.is_none() {
                            if self.find.visible && self.find.focus {
                                self.find.field_mut().key(FieldKey::Insert(&text));
                            } else {
                                self.view.type_text(&text);
                                self.edited();
                            }
                        }
                    }
                    Ime::Enabled | Ime::Disabled => self.preedit.clear(),
                }
                self.needs_redraw = true;
            }
            WindowEvent::CursorMoved { position, .. } => self.on_mouse_move((position.x as f32, position.y as f32)),
            WindowEvent::CursorLeft { .. } => {
                if self.hover_url.take().is_some() {
                    self.needs_redraw = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::DroppedFile(path) => {
                self.dropped = Some(path);
                self.run(Cmd::Open);
                self.needs_redraw = true;
            }
            _ => {}
        }
        if self.exit {
            self.settings.save();
            el.exit();
            return;
        }
        if std::mem::take(&mut self.needs_redraw)
            && let Some(w) = &self.window
        {
            w.request_redraw();
        }
    }
}

fn main() {
    let settings = Settings::load();
    let file = std::env::args_os().nth(1).map(PathBuf::from);
    let el = EventLoop::new().expect("failed to create event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(settings, file);
    el.run_app(&mut app).expect("event loop error");
}
