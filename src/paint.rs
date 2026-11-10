//! Layout and drawing of a window.

use super::*;

impl App {
    // ---------------------------------------------------------------- drawing

    pub(crate) fn compute_layout(&mut self, w: f32, h: f32) {
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

    pub(crate) fn draw_editor(&mut self, cv: &mut Canvas) {
        let th = self.theme;
        let s = self.win.scale;
        let area = self.win.layout.text;
        let pad = (6.0 * s).round();
        let show_caret = self.win.focused
            && !(self.win.find.visible && self.win.find.focus)
            && self.win.prompt.is_none()
            && self.win.font_dlg.is_none()
            && self.opening(self.win.tabs[self.win.active].id).is_none();
        self.win.tabs[self.win.active].view.prepare(&mut self.gfx.fs);
        let caret = self.win.tabs[self.win.active].view.draw(
            &mut self.gfx.fs,
            &mut self.gfx.cache,
            cv,
            area,
            pad,
            &th,
            s,
            self.win.hover_url.as_ref(),
        );
        self.win.caret = caret.map(|(x, y, h)| (x, y + h));
        // A file still being read: say so where its text will appear.
        let tab = &self.win.tabs[self.win.active];
        if let Some(p) = self.opening(tab.id) {
            let msg = format!("Opening {}… {p}%", tab.name());
            cv.set_clip(area);
            self.gfx.text_once(cv, &msg, area.x + pad, area.y + pad, th.dim);
            cv.reset_clip();
        }
        if let Some((x, y, h)) = caret {
            cv.set_clip(area);
            if !self.win.preedit.is_empty() {
                let w = self.gfx.text_w_once(&self.win.preedit);
                let r = Rect::new(x, y, w + 4.0 * s, h);
                cv.fill(r, th.field);
                self.gfx.text_in_once(cv, &self.win.preedit, Rect::new(x + 2.0 * s, y, w, h), th.fg);
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

    pub(crate) fn draw_status(&mut self, cv: &mut Canvas) {
        let r = self.win.layout.status;
        if r.h <= 0.0 {
            return;
        }
        let (th, s) = (self.theme, self.win.scale);
        cv.fill(r, th.chrome);
        cv.fill(Rect::new(r.x, r.y, r.w, s.max(1.0)), th.border);
        let cur = self.win.tabs[self.win.active].view.cursor;
        let doc = &self.win.tabs[self.win.active].view.doc;
        let col = doc.column(cur); // no copy of the (possibly huge) line
        let selected = self.win.tabs[self.win.active].view.selection().map(|(a, z)| doc.chars_between(a, z));
        let recovered = self.win.tabs[self.win.active].recovered_from.clone();
        // Just the link's bytes: copying its line cost 5 MB a frame on a 5 MB line.
        let link = self.win.hover_url.as_ref().and_then(|(l, r)| doc.line_get(*l, r.clone()));
        let hovered = link.map(|u| linkcheck::analyze(&u));
        let mut warn = false;
        let key = if MAC { "Cmd" } else { "Ctrl" };
        let job = self.job_status(self.win.tabs[self.win.active].id);
        let left = match (&self.win.status_msg, hovered) {
            _ if job.is_some() => job.unwrap_or_default(),
            (Some(m), _) => m.clone(),
            (None, _) if self.paste_pending_here() => "Pasting…".to_owned(),
            (None, Some(info)) if info.suspicious() => {
                warn = true;
                format!("⚠ Suspicious link: really goes to {}. {}", info.ascii_host, info.warnings[0])
            }
            (None, Some(info)) if info.host != info.ascii_host => {
                format!("{}   [{}]   ({key}+click to open)", info.url, info.ascii_host)
            }
            (None, Some(info)) => format!("{}   ({key}+click to open)", info.url),
            (None, None) => match (selected, &recovered) {
                (Some(n), _) => format!("Ln {}, Col {}   ({n} selected)", cur.line + 1, col),
                (None, Some(p)) => format!("Recovered — was {p}; use Save As to write it back"),
                (None, None) => format!("Ln {}, Col {}", cur.line + 1, col),
            },
        };
        let row = Rect::new(r.x + 10.0 * s, r.y, r.w, r.h);
        cv.set_clip(r);
        let eol = if self.win.tabs[self.win.active].mixed_eol {
            format!("{} (mixed)", self.win.tabs[self.win.active].eol.label())
        } else {
            self.win.tabs[self.win.active].eol.label().to_owned()
        };
        let segs = [
            format!("{}%", (self.zoom * 100.0).round()),
            eol,
            self.win.tabs[self.win.active].encoding.label().to_owned(),
        ];
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
        self.gfx.text_in_once(cv, &left, row, if warn { th.danger } else { th.chrome_fg });
        cv.reset_clip();
    }

    pub(crate) fn redraw(&mut self) {
        let Some(win) = self.win.window.clone() else { return };
        self.gfx.set_ui_scale(self.win.scale);
        self.gfx.bound_glyphs();
        let size = win.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if let Some(mut fb) = self.win.fb.take() {
            self.compute_layout(size.width as f32, size.height as f32);
            let drawn = fb.draw(size.width, size.height, |px| {
                let mut cv = Canvas::new(px, size.width, size.height);
                self.paint(&mut cv);
            });
            self.win.fb = Some(fb);
            // Every buffer is still with the compositor: draw once it hands one back.
            self.win.redraw_deferred |= !drawn;
        } else if let Some(mut surface) = self.win.surface.take() {
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
        }
        let title = format!("{}{} - {APP}", if self.tab().is_dirty() { "*" } else { "" }, self.doc_name());
        if title != self.win.title {
            win.set_title(&title);
            self.win.title = title;
        }
    }

    pub(crate) fn paint(&mut self, cv: &mut Canvas) {
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
        if let Some(c) = &mut self.win.ctx {
            c.draw(&mut self.gfx, cv, &th, s);
        }
        if let Some(p) = &mut self.win.prompt {
            p.draw(&mut self.gfx, cv, &th, s);
        }
    }
}

pub(crate) fn draw_preview(
    g: &mut Gfx,
    cv: &mut Canvas,
    r: Rect,
    family: &str,
    pt: f32,
    s: f32,
    color: cosmic_text::Color,
) {
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
