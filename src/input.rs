//! Keyboard and mouse handling.

use super::*;

impl App {
    // ---------------------------------------------------------------- keyboard

    pub(crate) fn shortcut(
        &self,
        ev: &KeyEvent,
        c: Option<char>,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> Option<Cmd> {
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

    pub(crate) fn on_key(&mut self, ev: KeyEvent) {
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
            Some(NamedKey::ArrowLeft) => Some(if line {
                Motion::Home
            } else if word {
                Motion::LeftWord
            } else {
                Motion::Left
            }),
            Some(NamedKey::ArrowRight) => Some(if line {
                Motion::End
            } else if word {
                Motion::RightWord
            } else {
                Motion::Right
            }),
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
    pub(crate) fn text_point(&self, p: (f32, f32)) -> (f32, f32) {
        let pad = (6.0 * self.win.scale).round();
        (
            p.0 - self.win.layout.text.x - pad + self.win.tabs[self.win.active].view.h_scroll(),
            p.1 - self.win.layout.text.y,
        )
    }

    pub(crate) fn url_at(&self, p: (f32, f32)) -> Option<(usize, Range<usize>, String)> {
        if !self.win.layout.text.contains(p) {
            return None;
        }
        let (x, y) = self.text_point(p);
        self.win.tabs[self.win.active].view.url_at(x, y)
    }

    /// (thumb rect, track travel, total lines)
    pub(crate) fn scrollbar(&self) -> Option<(Rect, f32, usize)> {
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

    pub(crate) fn update_cursor_icon(&mut self) {
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

    pub(crate) fn on_mouse_move(&mut self, p: (f32, f32)) {
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
                if self.tab_drag_outside() && self.start_wl_drag() {
                    self.update_cursor_icon();
                    return;
                }
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

    pub(crate) fn on_mouse_button(&mut self, state: ElementState, button: MouseButton) {
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
                if let Err(e) = launch::open_url(&search::url_target(&url)) {
                    self.win.status_msg = Some(e);
                }
                return;
            }
            let (x, y) = self.text_point(p);
            self.win.tabs[self.win.active].view.click(x, y, n, self.mods.shift_key());
            self.win.drag = Drag::Text;
        }
    }

    pub(crate) fn on_wheel(&mut self, delta: MouseScrollDelta) {
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
