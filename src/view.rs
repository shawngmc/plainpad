//! The editing view. The full text lives in a [`Document`] (a rope). Only a
//! window of lines around the viewport is loaded into a cosmic-text
//! [`Buffer`] for shaping and layout, so memory for layout is bounded by the
//! window size rather than the document size. Cursor, selection and undo are
//! kept in document coordinates.

use crate::document::{Document, EditResult, order};
use crate::gfx::{Canvas, Rect, Theme};
use crate::linkcheck;
use crate::search;
use cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Cursor, Family, FontSystem, LineEnding, Metrics, Motion, Scroll, Shaping,
    SwashCache, Wrap,
};
use std::ops::Range;

pub fn font_attrs(family: &str) -> Attrs<'_> {
    if family.is_empty() { Attrs::new().family(Family::Monospace) } else { Attrs::new().family(Family::Name(family)) }
}

pub struct View {
    pub doc: Document,
    buf: Buffer,
    /// Document line held in `buf.lines[0]`.
    win: usize,
    pub cursor: Cursor,
    pub anchor: Option<Cursor>,
    cursor_x: Option<i32>,
    reveal: bool,
    attrs: AttrsList,
    vis_rows: usize,
    geom: Option<(f32, f32, f32, f32, bool)>,
}

impl View {
    pub fn new(fs: &mut FontSystem, family: &str, wrap: bool) -> Self {
        let mut buf = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buf.set_wrap(if wrap { Wrap::WordOrGlyph } else { Wrap::None });
        let mut v = View {
            doc: Document::new(""),
            buf,
            win: 0,
            cursor: Cursor::default(),
            anchor: None,
            cursor_x: None,
            reveal: false,
            attrs: AttrsList::new(&font_attrs(family)),
            vis_rows: 40,
            geom: None,
        };
        v.load_window(0, 0.0, false);
        v
    }

    #[cfg(test)]
    pub fn set_text(&mut self, text: &str) {
        self.set_document(Document::new(text));
    }

    pub fn set_document(&mut self, doc: Document) {
        self.doc = doc;
        self.cursor = Cursor::default();
        self.anchor = None;
        self.cursor_x = None;
        self.buf.set_scroll(Scroll::default());
        self.load_window(0, 0.0, false);
    }

    pub fn set_font(&mut self, family: &str) {
        self.attrs = AttrsList::new(&font_attrs(family));
        for line in &mut self.buf.lines {
            line.set_attrs_list(self.attrs.clone());
        }
        self.buf.set_redraw(true);
    }

    /// Applies the text area's size, font metrics and wrap mode. Does nothing if
    /// they're unchanged, so switching back to a tab keeps its scroll position.
    /// On a real change the top visible line stays put rather than jumping to the cursor.
    pub fn set_geometry(&mut self, font_px: f32, line_h: f32, w: f32, h: f32, wrap: bool) {
        let g = (font_px, line_h, w, h, wrap);
        if self.geom == Some(g) {
            return;
        }
        let first = self.geom.is_none();
        self.geom = Some(g);
        self.buf.set_metrics_and_size(Metrics::new(font_px, line_h), Some(w), Some(h));
        self.buf.set_wrap(if wrap { Wrap::WordOrGlyph } else { Wrap::None });
        if wrap {
            let mut s = self.buf.scroll();
            s.horizontal = 0.0;
            self.buf.set_scroll(s);
        }
        self.vis_rows = ((h / line_h).ceil() as usize).max(1);
        self.reveal |= first;
    }

    pub fn line_h(&self) -> f32 {
        self.buf.metrics().line_height
    }

    pub fn h_scroll(&self) -> f32 {
        self.buf.scroll().horizontal
    }

    // ------------------------------------------------------------ window

    fn margin(&self) -> usize {
        (self.vis_rows * 2).max(64)
    }

    fn win_end(&self) -> usize {
        self.win + self.buf.lines.len()
    }

    fn top_line(&self) -> usize {
        self.win + self.buf.scroll().line
    }

    fn in_window(&self, c: Cursor) -> bool {
        c.line >= self.win && c.line < self.win_end()
    }

    fn doc_to_win(&self, c: Cursor) -> Cursor {
        Cursor { line: c.line - self.win, ..c }
    }

    fn win_to_doc(&self, c: Cursor) -> Cursor {
        Cursor { line: c.line + self.win, ..c }
    }

    /// Forces the buffer to re-run layout. cosmic-text only notices freshly
    /// inserted (never-shaped) lines when some buffer state changed, and a
    /// `set_scroll` to the current value is ignored, so nudge it and restore it.
    fn touch(&mut self) {
        let s = self.buf.scroll();
        self.buf.set_scroll(Scroll::new(s.line, s.vertical + 1.0, s.horizontal));
        self.buf.set_scroll(s);
    }

    fn new_line(&self, i: usize) -> BufferLine {
        BufferLine::new(self.doc.line(i), LineEnding::Lf, self.attrs.clone(), Shaping::Advanced)
    }

    /// Loads doc lines around `top` into the layout buffer, scrolled so that
    /// `top` is the first visible line. With `reuse`, lines already loaded
    /// (and their shaping) are kept; only valid if the window mirrors the doc.
    fn load_window(&mut self, top: usize, vert: f32, reuse: bool) {
        let n = self.doc.len_lines();
        let m = self.margin();
        let top = top.min(n - 1);
        let start = top.saturating_sub(m);
        let end = (top + self.vis_rows + m).min(n).max(start + 1);
        let mut old: Vec<Option<BufferLine>> =
            if reuse { std::mem::take(&mut self.buf.lines).into_iter().map(Some).collect() } else { Vec::new() };
        let os = self.win;
        let mut lines = Vec::with_capacity(end - start);
        for i in start..end {
            let reused = if i >= os { old.get_mut(i - os).and_then(Option::take) } else { None };
            lines.push(reused.unwrap_or_else(|| self.new_line(i)));
        }
        self.buf.lines = lines;
        self.win = start;
        let h = self.buf.scroll().horizontal;
        self.buf.set_scroll(Scroll::new(top - start, vert, h));
        self.touch();
    }

    /// Re-centres the window if scrolling has brought the view near its edge.
    fn sync_window(&mut self) -> bool {
        let s = self.buf.scroll();
        let (len, m, n) = (self.buf.lines.len(), self.margin(), self.doc.len_lines());
        let near_top = self.win > 0 && s.line < m / 2;
        let near_bottom = self.win_end() < n && s.line + self.vis_rows + m / 2 > len;
        if near_top || near_bottom {
            self.load_window(self.top_line(), s.vertical, true);
            return true;
        }
        false
    }

    /// Makes sure the cursor is inside the window with room around it for motions.
    fn ensure_cursor_window(&mut self) {
        let (c, m, n) = (self.cursor.line, self.margin(), self.doc.len_lines());
        let ok_top = self.win == 0 || c >= self.win + m / 2;
        let ok_bottom = self.win_end() == n || c + m / 2 < self.win_end();
        if self.in_window(self.cursor) && ok_top && ok_bottom {
            return;
        }
        let top = self.top_line();
        let s = self.buf.scroll();
        if c + m / 2 >= top && c < top + self.vis_rows + m / 2 {
            self.load_window(top, s.vertical, true);
        } else {
            self.load_window(c.saturating_sub(self.vis_rows / 2), 0.0, true);
        }
    }

    /// Lays out what's visible; scrolls to the cursor if it moved. Call before drawing or hit-testing.
    pub fn prepare(&mut self, fs: &mut FontSystem) {
        if std::mem::take(&mut self.reveal) {
            self.ensure_cursor_window();
            let wc = self.doc_to_win(self.cursor);
            self.buf.shape_until_cursor(fs, wc, true);
        } else {
            self.buf.shape_until_scroll(fs, true);
        }
        if self.sync_window() {
            self.buf.shape_until_scroll(fs, true);
        }
        self.clamp_top(fs);
    }

    /// cosmic-text pulls the view back when there's empty space below the last
    /// line, but not once it reaches the first line, which can leave the top
    /// partly hidden after text is deleted. Clamp that case here.
    fn clamp_top(&mut self, fs: &mut FontSystem) {
        let s = self.buf.scroll();
        if self.win != 0 || s.line != 0 || s.vertical <= 0.0 || self.win_end() != self.doc.len_lines() {
            return;
        }
        let view_h = self.buf.size().1.unwrap_or(0.0);
        let lh = self.line_h();
        let mut total = 0.0;
        for i in 0..self.buf.lines.len() {
            let Some(layout) = self.buf.line_layout(fs, i) else { break };
            total += layout.iter().map(|l| l.line_height_opt.unwrap_or(lh)).sum::<f32>();
            if total >= s.vertical + view_h {
                return; // content fills the view: nothing to fix
            }
        }
        let v = (total - view_h).max(0.0);
        self.buf.set_scroll(Scroll::new(0, v, s.horizontal));
        self.buf.shape_until_scroll(fs, true);
    }

    pub fn scroll_px(&mut self, px: f32) {
        let lh = self.line_h();
        let lines = (px / lh) as isize;
        if lines.unsigned_abs() > self.margin() / 2 {
            let top = (self.top_line() as isize + lines).max(0) as usize;
            self.load_window(top, 0.0, true);
            return;
        }
        let mut s = self.buf.scroll();
        s.vertical += px;
        self.buf.set_scroll(s);
    }

    pub fn scroll_h(&mut self, dx: f32) {
        let mut s = self.buf.scroll();
        s.horizontal = (s.horizontal + dx).max(0.0);
        self.buf.set_scroll(s);
    }

    /// (total doc lines, first visible doc line)
    pub fn scroll_info(&self) -> (usize, usize) {
        (self.doc.len_lines(), self.top_line())
    }

    pub fn scroll_to_line(&mut self, line: usize) {
        self.load_window(line, 0.0, true);
    }

    // ------------------------------------------------------------ selection

    pub fn selection(&self) -> Option<(Cursor, Cursor)> {
        let a = self.anchor.filter(|a| (a.line, a.index) != (self.cursor.line, self.cursor.index))?;
        Some(order(a, self.cursor))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(a, z)| self.doc.text_range(a, z))
    }

    pub fn set_selection(&mut self, anchor: Cursor, cursor: Cursor) {
        self.anchor = Some(anchor);
        self.cursor = cursor;
        self.cursor_x = None;
        self.reveal = true;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Cursor::default());
        self.cursor = self.doc.end();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    // ------------------------------------------------------------ editing

    fn after_edit(&mut self, r: EditResult) {
        let (ws, we) = (self.win, self.win_end());
        let grown = self.buf.lines.len() + r.new_last.saturating_sub(r.old_last);
        if r.first >= ws && r.old_last < we && grown <= self.vis_rows + 4 * self.margin() {
            let new: Vec<BufferLine> = (r.first..=r.new_last).map(|i| self.new_line(i)).collect();
            self.buf.lines.splice(r.first - ws..=r.old_last - ws, new);
            self.touch();
        } else {
            self.reload();
        }
        self.cursor = r.end;
        self.anchor = None;
        self.cursor_x = None;
        self.reveal = true;
    }

    /// Rebuilds the window from the document, keeping the scroll position.
    fn reload(&mut self) {
        let top = self.top_line().min(self.doc.len_lines() - 1);
        let vert = self.buf.scroll().vertical;
        self.load_window(top, vert, false);
    }

    pub fn replace_selection(&mut self, text: &str, merge: bool) {
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        let r = self.doc.replace(a, b, text, merge);
        self.after_edit(r);
    }

    pub fn type_text(&mut self, s: &str) {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => {
                self.replace_selection(s, true);
                if c.is_whitespace() {
                    self.doc.break_merge();
                }
            }
            (Some(_), Some(_)) => self.replace_selection(s, false),
            _ => {}
        }
    }

    pub fn delete_selection(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("", false);
        }
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let a = self.doc.prev_char(self.cursor);
        if a != self.cursor {
            let r = self.doc.replace(a, self.cursor, "", false);
            self.after_edit(r);
        }
    }

    pub fn delete(&mut self) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let b = self.doc.next_grapheme(self.cursor);
        if b != self.cursor {
            let r = self.doc.replace(self.cursor, b, "", false);
            self.after_edit(r);
        }
    }

    pub fn delete_word(&mut self, fs: &mut FontSystem, forward: bool) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        self.ensure_cursor_window();
        let m = if forward { Motion::NextWord } else { Motion::PreviousWord };
        let wc = self.doc_to_win(self.cursor);
        if let Some((t, _)) = self.buf.cursor_motion(fs, wc, None, m) {
            let t = self.win_to_doc(t);
            if t != self.cursor {
                let r = self.doc.replace(self.cursor, t, "", false);
                self.after_edit(r);
            }
        }
    }

    pub fn undo(&mut self) {
        if let Some(c) = self.doc.undo() {
            self.reload();
            self.place_cursor(c);
        }
    }

    pub fn redo(&mut self) {
        if let Some(c) = self.doc.redo() {
            self.reload();
            self.place_cursor(c);
        }
    }

    pub fn replace_many(&mut self, ranges: &[(Cursor, Cursor)], text: &str) {
        let Some(&(first, _)) = ranges.first() else { return };
        self.doc.replace_many(ranges, text);
        self.reload();
        self.place_cursor(first);
    }

    fn place_cursor(&mut self, c: Cursor) {
        self.cursor = self.doc.clamp(c);
        self.anchor = None;
        self.cursor_x = None;
        self.reveal = true;
    }

    // ------------------------------------------------------------ navigation

    pub fn motion(&mut self, fs: &mut FontSystem, m: Motion, extend: bool) {
        self.doc.break_merge();
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((a, z)) = self.selection() {
            self.anchor = None;
            match m {
                Motion::Left => return self.place_cursor(a),
                Motion::Right => return self.place_cursor(z),
                _ => {}
            }
        } else {
            self.anchor = None;
        }
        let (new, x) = match m {
            Motion::BufferStart => (Cursor::default(), None),
            Motion::BufferEnd => (self.doc.end(), None),
            _ => {
                self.ensure_cursor_window();
                let wc = self.doc_to_win(self.cursor);
                match self.buf.cursor_motion(fs, wc, self.cursor_x, m) {
                    Some((c, x)) => (self.win_to_doc(c), x),
                    None => (self.cursor, self.cursor_x),
                }
            }
        };
        self.cursor = new;
        self.cursor_x = x;
        self.reveal = true;
    }

    /// `x`/`y` are relative to the text origin (horizontal scroll already added).
    fn hit(&self, x: f32, y: f32) -> Option<Cursor> {
        self.buf.hit(x, y).map(|c| self.win_to_doc(c))
    }

    pub fn click(&mut self, x: f32, y: f32, count: u8, extend: bool) {
        let Some(c) = self.hit(x, y) else { return };
        self.doc.break_merge();
        self.cursor_x = None;
        match count {
            2 => {
                let (a, b) = self.doc.word_at(c);
                self.anchor = Some(a);
                self.cursor = b;
            }
            3 => {
                self.anchor = Some(Cursor::new(c.line, 0));
                self.cursor = if c.line + 1 < self.doc.len_lines() {
                    Cursor::new(c.line + 1, 0)
                } else {
                    Cursor::new(c.line, self.doc.line_len(c.line))
                };
            }
            _ => {
                if !extend || self.anchor.is_none() {
                    self.anchor = Some(if extend { self.cursor } else { c });
                }
                self.cursor = c;
            }
        }
        self.reveal = true;
    }

    /// Whether the point (relative to the text origin) is inside the selection.
    pub fn point_in_selection(&self, x: f32, y: f32) -> bool {
        let (Some(c), Some((a, z))) = (self.hit(x, y), self.selection()) else { return false };
        (a.line, a.index) <= (c.line, c.index) && (c.line, c.index) < (z.line, z.index)
    }

    pub fn drag(&mut self, x: f32, y: f32) {
        if let Some(c) = self.hit(x, y) {
            self.cursor = c;
            self.reveal = true;
        }
    }

    /// URL under a point: (doc line, byte range, text).
    pub fn url_at(&self, x: f32, y: f32) -> Option<(usize, Range<usize>, String)> {
        let run = self.buf.layout_runs().find(|r| y >= r.line_top && y < r.line_top + r.line_height)?;
        let g = run.glyphs.iter().find(|g| x >= g.x && x < g.x + g.w)?;
        let r = search::find_urls(run.text).into_iter().find(|r| g.start >= r.start && g.start < r.end)?;
        Some((run.line_i + self.win, r.clone(), run.text[r].to_owned()))
    }

    // ------------------------------------------------------------ drawing

    /// Draws the visible text into `area` (clip already set by the caller is
    /// not assumed). Returns the caret rectangle (x, y, h) if it is visible.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        fs: &mut FontSystem,
        cache: &mut SwashCache,
        cv: &mut Canvas,
        area: Rect,
        pad: f32,
        th: &Theme,
        s: f32,
        hover: Option<&(usize, Range<usize>)>,
    ) -> Option<(f32, f32, f32)> {
        let (ws, we) = (self.win, self.win_end());
        let sel = self.selection().filter(|(a, z)| z.line >= ws && a.line < we).map(|(a, z)| {
            let a = if a.line < ws { Cursor::default() } else { self.doc_to_win(a) };
            let z = if z.line >= we {
                let l = self.buf.lines.len() - 1;
                Cursor::new(l, self.buf.lines[l].text().len())
            } else {
                self.doc_to_win(z)
            };
            (a, z)
        });
        let wc = self.in_window(self.cursor).then(|| self.doc_to_win(self.cursor));
        let ox = area.x + pad - self.buf.scroll().horizontal;
        let oy = area.y;
        let px1 = s.max(1.0).round();
        let mut url_line = usize::MAX;
        let mut urls: Vec<(Range<usize>, bool)> = Vec::new();
        let mut caret = None;
        cv.set_clip(area);
        for run in self.buf.layout_runs() {
            let li = run.line_i;
            let top = oy + run.line_top;
            if let Some((a, z)) = sel.filter(|(a, z)| li >= a.line && li <= z.line) {
                let hl: Vec<(f32, f32)> = run.highlight(a, z).collect();
                if hl.is_empty() && run.glyphs.is_empty() && z.line > li {
                    cv.fill(Rect::new(area.x, top, area.w, run.line_height), th.sel);
                }
                let n = hl.len();
                for (k, (x, w)) in hl.into_iter().enumerate() {
                    let (mut x0, mut x1) = (ox + x, ox + x + w);
                    if k + 1 == n && z.line > li {
                        if run.rtl { x0 = area.x } else { x1 = area.right() }
                    }
                    cv.fill(Rect::new(x0, top, x1 - x0, run.line_height), th.sel);
                }
            }
            if li != url_line {
                url_line = li;
                // (range, suspicious) for each link on this line.
                urls = search::find_urls(run.text)
                    .into_iter()
                    .map(|r| {
                        let bad = linkcheck::analyze(&run.text[r.clone()]).suspicious();
                        (r, bad)
                    })
                    .collect();
            }
            for g in run.glyphs {
                let link = urls.iter().find(|(r, _)| g.start >= r.start && g.start < r.end).map(|(_, bad)| *bad);
                let in_url = link.is_some();
                let suspicious = link == Some(true);
                let selected = sel.is_some_and(|(a, z)| {
                    li >= a.line
                        && li <= z.line
                        && (a.line != li || g.end > a.index)
                        && (z.line != li || g.start < z.index)
                });
                let color = if selected {
                    th.sel_fg
                } else if suspicious {
                    th.danger
                } else if in_url {
                    th.link
                } else {
                    th.fg
                };
                let pg = g.physical((ox, oy + run.line_y), 1.0);
                cache.with_pixels(fs, pg.cache_key, color, |x, y, c| cv.blend(pg.x + x, pg.y + y, c));
                let hovered = hover.is_some_and(|(hl, hr)| *hl == li + ws && g.start >= hr.start && g.start < hr.end);
                // Suspicious links are always underlined (doubly when hovered), so they stand out unprompted.
                if hovered || suspicious {
                    let c = if suspicious { th.danger } else { th.link };
                    cv.fill(Rect::new(ox + g.x, oy + run.line_y + 2.0 * s, g.w, px1), c);
                    if suspicious && hovered {
                        cv.fill(Rect::new(ox + g.x, oy + run.line_y + 2.0 * s + 2.0 * px1, g.w, px1), c);
                    }
                }
            }
            if let Some(wc) = wc.filter(|wc| wc.line == li)
                && let Some(x) = run.cursor_position(&wc)
            {
                caret = Some(((ox + x).round(), top, run.line_height));
            }
        }
        cv.reset_clip();
        caret
    }

    /// Number of lines currently held in the layout window (for diagnostics/tests).
    #[cfg(test)]
    pub fn window_len(&self) -> usize {
        self.buf.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str) -> (FontSystem, View) {
        let mut fs = FontSystem::new();
        let mut v = View::new(&mut fs, "", true);
        v.set_geometry(14.0, 18.0, 400.0, 18.0 * 20.0, true);
        v.set_text(text);
        v.prepare(&mut fs);
        (fs, v)
    }

    fn big(n: usize) -> String {
        (0..n).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn window_is_bounded() {
        let (_fs, v) = view(&big(100_000));
        assert!(v.window_len() < 200, "window holds {} lines", v.window_len());
    }

    #[test]
    fn navigation_across_window() {
        let (mut fs, mut v) = view(&big(100_000));
        v.motion(&mut fs, Motion::BufferEnd, false);
        v.prepare(&mut fs);
        assert_eq!(v.cursor.line, 99_999);
        for _ in 0..500 {
            v.motion(&mut fs, Motion::Up, true);
            v.prepare(&mut fs);
        }
        assert_eq!(v.cursor.line, 99_499);
        assert_eq!(v.selection().unwrap().1.line, 99_999);
        v.motion(&mut fs, Motion::BufferStart, false);
        v.prepare(&mut fs);
        assert_eq!(v.cursor.line, 0);
    }

    #[test]
    fn edits_mirror_into_window() {
        let (mut fs, mut v) = view(&big(1000));
        for _ in 0..10 {
            v.motion(&mut fs, Motion::Down, false);
        }
        v.type_text("X");
        v.replace_selection("\nnew\n", false);
        v.prepare(&mut fs);
        assert_eq!(
            (v.doc.line(10).as_ref(), v.doc.line(11).as_ref(), v.doc.line(12).as_ref()),
            ("X", "new", "line 10")
        );
        // Window lines must match the document exactly.
        for (k, l) in v.buf.lines.iter().enumerate() {
            assert_eq!(l.text(), v.doc.line(v.win + k), "window line {k}");
        }
        v.undo();
        v.undo();
        v.prepare(&mut fs);
        assert_eq!(v.doc.line(10), "line 10");
        for (k, l) in v.buf.lines.iter().enumerate() {
            assert_eq!(l.text(), v.doc.line(v.win + k));
        }
    }

    #[test]
    fn scrolling_far_keeps_window_consistent() {
        let (mut fs, mut v) = view(&big(50_000));
        for _ in 0..300 {
            v.scroll_px(18.0 * 3.0);
            v.prepare(&mut fs);
        }
        let (_, top) = v.scroll_info();
        assert!((850..=950).contains(&top), "top = {top}");
        for (k, l) in v.buf.lines.iter().enumerate() {
            assert_eq!(l.text(), v.doc.line(v.win + k));
        }
        assert!(v.window_len() < 200);
    }

    fn runs(v: &View) -> usize {
        v.buf.layout_runs().count()
    }

    #[test]
    fn big_jumps_still_lay_out() {
        let (mut fs, mut v) = view(&big(2000));
        for _ in 0..10 {
            v.scroll_px(18.0 * 300.0);
            v.prepare(&mut fs);
            assert!(runs(&v) >= 20, "blank view at top {:?}", v.scroll_info());
        }
        v.scroll_to_line(500);
        v.prepare(&mut fs);
        assert_eq!(v.scroll_info().1, 500);
        assert!(runs(&v) >= 20);
    }

    #[test]
    fn multi_line_insert_is_laid_out() {
        let (mut fs, mut v) = view("a\nb");
        v.replace_selection(&"x\n".repeat(30), false);
        v.prepare(&mut fs);
        assert!(runs(&v) >= 20);
    }

    #[test]
    fn shrinking_to_fit_scrolls_to_top() {
        let (mut fs, mut v) = view(&big(100));
        v.motion(&mut fs, Motion::BufferEnd, false);
        v.prepare(&mut fs);
        assert!(v.scroll_info().1 > 50);
        // Select from line 5 to the end and delete: the rest now fits in the view.
        v.anchor = Some(Cursor::new(5, 0));
        v.delete_selection();
        v.prepare(&mut fs);
        assert_eq!(v.scroll_info().1, 0);
        assert_eq!(v.buf.scroll().vertical, 0.0);
        assert_eq!(runs(&v), 6);
    }

    #[test]
    fn deleting_to_fit_shows_the_top() {
        let mut fs = FontSystem::new();
        let mut v = View::new(&mut fs, "", true);
        // A view height that isn't a whole number of lines, as in a real window.
        v.set_geometry(14.0, 18.0, 400.0, 18.0 * 20.0 + 9.0, true);
        v.set_text(&big(40));
        v.prepare(&mut fs);
        v.motion(&mut fs, Motion::BufferEnd, false);
        v.prepare(&mut fs);
        for _ in 0..200 {
            v.backspace();
            v.prepare(&mut fs);
        }
        assert_eq!(v.scroll_info().1, 0);
        assert_eq!(v.buf.scroll().vertical, 0.0);
    }

    #[test]
    fn unchanged_geometry_keeps_scroll() {
        let (mut fs, mut v) = view(&big(1000));
        v.scroll_px(18.0 * 300.0);
        v.prepare(&mut fs);
        let top = v.scroll_info().1;
        assert!(top > 200);
        // Re-applying the same geometry (as on a tab switch) must not move the view,
        // even though the cursor is far away at the start.
        v.set_geometry(14.0, 18.0, 400.0, 18.0 * 20.0, true);
        v.prepare(&mut fs);
        assert_eq!(v.scroll_info().1, top);
        // A real change (e.g. a resize) keeps the top line too.
        v.set_geometry(14.0, 18.0, 300.0, 18.0 * 15.0, true);
        v.prepare(&mut fs);
        assert_eq!(v.scroll_info().1, top);
    }
}
