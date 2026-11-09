//! The editing view. The full text lives in a [`Document`] (a rope). Only a
//! window of lines around the viewport is loaded into a cosmic-text
//! [`Buffer`] for shaping and layout, so memory for layout is bounded by the
//! window size rather than the document size. Cursor, selection and undo are
//! kept in document coordinates.
//!
//! Very long lines (minified files, logs) are split into display segments of
//! at most [`SEG`] bytes, each its own layout line. cosmic-text shapes a line as
//! a whole, so without this a multi-megabyte line took seconds to show and every
//! keystroke re-shaped all of it. `segs` maps each layout line back to its
//! document line and starting byte.

use crate::document::{Document, EditResult, order};
use crate::gfx::{Canvas, Rect, Theme};
use crate::linkcheck;
use crate::search;
use cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Cursor, Family, FontSystem, LineEnding, Metrics, Motion, Scroll, Shaping,
    SwashCache, Wrap,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Lines longer than this many bytes are split into display segments.
const SEG: usize = 2048;

/// Byte offsets where a line's display segments start (always begins with 0).
/// Splits after a space or punctuation in the last quarter of a segment where
/// possible, otherwise at a grapheme boundary, so no character is broken.
fn split_line(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut s = 0;
    while text.len() - s > SEG {
        let limit = s + SEG;
        let soft = text.as_bytes()[s + SEG * 3 / 4..limit]
            .iter()
            .rposition(|b| b" ,;})]>".contains(b))
            .map(|p| s + SEG * 3 / 4 + p + 1);
        let cut = soft.unwrap_or_else(|| {
            let window_end = (limit + 64).min(text.len());
            let mut end = window_end;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[s..end].grapheme_indices(true).map(|(i, _)| s + i).take_while(|&i| i <= limit).last().unwrap_or(s)
        });
        // Always make progress, even through one enormous grapheme cluster.
        let cut = if cut > s {
            cut
        } else {
            let mut c = limit;
            while !text.is_char_boundary(c) {
                c += 1;
            }
            c
        };
        starts.push(cut);
        s = cut;
    }
    starts
}

pub fn font_attrs(family: &str) -> Attrs<'_> {
    if family.is_empty() { Attrs::new().family(Family::Monospace) } else { Attrs::new().family(Family::Name(family)) }
}

pub struct View {
    pub doc: Document,
    buf: Buffer,
    /// Document line held in `buf.lines[0]`.
    win: usize,
    /// For each line in `buf.lines`: (document line, byte offset where it starts).
    segs: Vec<(usize, usize)>,
    pub cursor: Cursor,
    pub anchor: Option<Cursor>,
    cursor_x: Option<i32>,
    reveal: bool,
    attrs: AttrsList,
    vis_rows: usize,
    geom: Option<(f32, f32, f32, f32, bool)>,
    /// Refuse edits (while the document is loaded, saved or replaced in the background).
    pub read_only: bool,
}

impl View {
    pub fn new(fs: &mut FontSystem, family: &str, wrap: bool) -> Self {
        let mut buf = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buf.set_wrap(if wrap { Wrap::WordOrGlyph } else { Wrap::None });
        let mut v = View {
            doc: Document::new(""),
            buf,
            win: 0,
            segs: Vec::new(),
            cursor: Cursor::default(),
            anchor: None,
            cursor_x: None,
            reveal: false,
            attrs: AttrsList::new(&font_attrs(family)),
            vis_rows: 40,
            geom: None,
            read_only: false,
        };
        v.load_window((0, 0), 0.0, false);
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
        self.load_window((0, 0), 0.0, false);
    }

    /// Replaces the document (e.g. reloaded from disk), keeping the scroll
    /// position and cursor as far as the new text allows.
    pub fn reload_document(&mut self, doc: Document) {
        let (cursor, top) = (self.cursor, self.top_line());
        self.doc = doc;
        self.anchor = None;
        self.cursor_x = None;
        let top = top.min(self.doc.len_lines() - 1);
        let vert = self.buf.scroll().vertical;
        self.load_window((top, 0), vert, false);
        self.cursor = self.doc.clamp(cursor);
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

    /// One past the last document line in the window.
    fn win_end(&self) -> usize {
        self.segs.last().map_or(self.win, |s| s.0 + 1)
    }

    /// The first visible segment: (document line, byte offset).
    fn top_pos(&self) -> (usize, usize) {
        self.segs.get(self.buf.scroll().line).copied().unwrap_or((self.win, 0))
    }

    fn top_line(&self) -> usize {
        self.top_pos().0
    }

    fn in_window(&self, c: Cursor) -> bool {
        c.line >= self.win && c.line < self.win_end()
    }

    /// Whether the whole document is loaded (no window shifting needed).
    fn whole_doc(&self) -> bool {
        self.win == 0 && self.win_end() == self.doc.len_lines()
    }

    /// Index of the segment containing a document position. A position exactly
    /// on a boundary belongs to the segment that starts there.
    fn seg_of(&self, line: usize, index: usize) -> usize {
        self.segs.partition_point(|&s| s <= (line, index)).saturating_sub(1)
    }

    fn doc_to_win(&self, c: Cursor) -> Cursor {
        let k = self.seg_of(c.line, c.index);
        Cursor { line: k, index: c.index.saturating_sub(self.segs[k].1), ..c }
    }

    fn win_to_doc(&self, c: Cursor) -> Cursor {
        let (line, start) = self.segs[c.line];
        Cursor { line, index: start + c.index, ..c }
    }

    /// Layout lines (and their `segs` entries) for document line `i`.
    fn make_lines(&self, i: usize, out: &mut Vec<BufferLine>, segs: &mut Vec<(usize, usize)>) {
        let text = self.doc.line(i);
        let starts = split_line(&text);
        for (k, &a) in starts.iter().enumerate() {
            let b = starts.get(k + 1).copied().unwrap_or(text.len());
            out.push(BufferLine::new(&text[a..b], LineEnding::Lf, self.attrs.clone(), Shaping::Advanced));
            segs.push((i, a));
        }
    }

    /// Forces the buffer to re-run layout. cosmic-text only notices freshly
    /// inserted (never-shaped) lines when some buffer state changed, and a
    /// `set_scroll` to the current value is ignored, so nudge it and restore it.
    fn touch(&mut self) {
        let s = self.buf.scroll();
        self.buf.set_scroll(Scroll::new(s.line, s.vertical + 1.0, s.horizontal));
        self.buf.set_scroll(s);
    }

    /// Loads doc lines around `top` (document line, byte offset) into the layout
    /// buffer, scrolled so that the segment containing `top` is the first visible
    /// line. With `reuse`, lines already loaded (and their shaping) are kept; only
    /// valid if the window mirrors the doc.
    fn load_window(&mut self, top: (usize, usize), vert: f32, reuse: bool) {
        let n = self.doc.len_lines();
        let m = self.margin();
        let top = if top.0 >= n { (n - 1, 0) } else { top };
        let start = top.0.saturating_sub(m);
        let end = (top.0 + self.vis_rows + m).min(n).max(start + 1);
        let old_segs = std::mem::take(&mut self.segs);
        let mut old: Vec<Option<BufferLine>> =
            if reuse { std::mem::take(&mut self.buf.lines).into_iter().map(Some).collect() } else { Vec::new() };
        let mut lines = Vec::with_capacity(end - start);
        let mut segs = Vec::with_capacity(end - start);
        for i in start..end {
            let a = old_segs.partition_point(|s| s.0 < i);
            let b = old_segs.partition_point(|s| s.0 <= i);
            if reuse && a < b && old[a..b].iter().all(Option::is_some) {
                for k in a..b {
                    lines.push(old[k].take().expect("checked"));
                    segs.push(old_segs[k]);
                }
            } else {
                self.make_lines(i, &mut lines, &mut segs);
            }
        }
        self.buf.lines = lines;
        self.segs = segs;
        self.win = start;
        let h = self.buf.scroll().horizontal;
        let k = self.seg_of(top.0, top.1);
        self.buf.set_scroll(Scroll::new(k, vert, h));
        self.touch();
    }

    /// Re-centres the window if scrolling has brought the view near its edge.
    fn sync_window(&mut self) -> bool {
        let s = self.buf.scroll();
        let (len, m, n) = (self.buf.lines.len(), self.margin(), self.doc.len_lines());
        let near_top = self.win > 0 && s.line < m / 2;
        let near_bottom = self.win_end() < n && s.line + self.vis_rows + m / 2 > len;
        if near_top || near_bottom {
            self.load_window(self.top_pos(), s.vertical, true);
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
            self.load_window(self.top_pos(), s.vertical, true);
        } else {
            self.load_window((c.saturating_sub(self.vis_rows / 2), 0), 0.0, true);
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
        if lines.unsigned_abs() > self.margin() / 2 && !self.whole_doc() {
            let top = (self.top_line() as isize + lines).max(0) as usize;
            self.load_window((top, 0), 0.0, true);
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

    /// (total, first visible) in scroll units: document lines, or segments when
    /// the whole document is loaded (so one huge line still scrolls smoothly).
    pub fn scroll_info(&self) -> (usize, usize) {
        if self.whole_doc() {
            (self.segs.len(), self.buf.scroll().line)
        } else {
            (self.doc.len_lines(), self.top_line())
        }
    }

    /// Scrolls to a position in the units of [`View::scroll_info`].
    pub fn scroll_to_line(&mut self, unit: usize) {
        if self.whole_doc() {
            let h = self.buf.scroll().horizontal;
            self.buf.set_scroll(Scroll::new(unit.min(self.segs.len() - 1), 0.0, h));
            self.touch();
        } else {
            self.load_window((unit, 0), 0.0, true);
        }
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

    /// Fast path for an edit within one segment of one line (typing, Backspace,
    /// Delete): re-lay out just that segment and shift the later segments' offsets.
    fn edit_segment(&mut self, r: &EditResult) -> bool {
        let line = r.first;
        if r.old_last != line || r.new_last != line || !self.in_window(r.start) {
            return false;
        }
        let k = self.seg_of(line, r.start.index);
        let delta = r.end.index as isize - r.old_end.index as isize;
        let seg_start = self.segs[k].1;
        let next = self.segs.get(k + 1).filter(|s| s.0 == line).map(|s| s.1);
        let old_line_len = (self.doc.line_len(line) as isize - delta) as usize;
        let old_seg_end = next.unwrap_or(old_line_len);
        if r.old_end.index > old_seg_end {
            return false; // the edit spans a segment boundary
        }
        let new_end = (old_seg_end as isize + delta) as usize;
        let len = new_end - seg_start;
        // Re-split once a segment grows well past the limit, or empties out.
        if len > 2 * SEG || (len == 0 && (k > 0 && self.segs[k - 1].0 == line || next.is_some())) {
            return false;
        }
        let text = self.doc.line_slice(line, seg_start, new_end);
        self.buf.lines[k].set_text(text, LineEnding::Lf, self.attrs.clone());
        for s in self.segs[k + 1..].iter_mut().take_while(|s| s.0 == line) {
            s.1 = (s.1 as isize + delta) as usize;
        }
        self.touch();
        true
    }

    fn after_edit(&mut self, r: EditResult) {
        let (ws, we) = (self.win, self.win_end());
        if self.edit_segment(&r) {
            // done
        } else if r.first >= ws && r.old_last < we && r.new_last - r.first <= 4 * self.margin() {
            // Rebuild the edited lines' segments in place.
            let a = self.segs.partition_point(|s| s.0 < r.first);
            let b = self.segs.partition_point(|s| s.0 <= r.old_last);
            let (mut lines, mut segs) = (Vec::new(), Vec::new());
            for i in r.first..=r.new_last {
                self.make_lines(i, &mut lines, &mut segs);
            }
            self.buf.lines.splice(a..b, lines);
            let shift = r.new_last as isize - r.old_last as isize;
            let added = segs.len();
            self.segs.splice(a..b, segs);
            for s in &mut self.segs[a + added..] {
                s.0 = (s.0 as isize + shift) as usize;
            }
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
        let top = self.top_pos();
        let vert = self.buf.scroll().vertical;
        self.load_window(top, vert, false);
    }

    pub fn replace_selection(&mut self, text: &str, merge: bool) {
        if self.read_only {
            return;
        }
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
        if self.read_only {
            return;
        }
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
        if self.read_only {
            return;
        }
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
        if self.read_only {
            return;
        }
        if self.selection().is_some() {
            return self.delete_selection();
        }
        self.ensure_cursor_window();
        let m = if forward { Motion::NextWord } else { Motion::PreviousWord };
        if let Some((t, _)) = self.seg_motion(fs, m, None)
            && (t.line, t.index) != (self.cursor.line, self.cursor.index)
        {
            let r = self.doc.replace(self.cursor, t, "", false);
            self.after_edit(r);
        }
    }

    pub fn undo(&mut self) {
        if self.read_only {
            return;
        }
        if let Some(c) = self.doc.undo() {
            self.reload();
            self.place_cursor(c);
        }
    }

    pub fn redo(&mut self) {
        if self.read_only {
            return;
        }
        if let Some(c) = self.doc.redo() {
            self.reload();
            self.place_cursor(c);
        }
    }

    /// Installs a Replace All result.
    pub fn apply_prepared(&mut self, p: crate::document::Prepared) {
        let first = p.first();
        self.doc.apply_prepared(p);
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
                self.seg_motion(fs, m, self.cursor_x).unwrap_or((self.cursor, self.cursor_x))
            }
        };
        self.cursor = new;
        self.cursor_x = x;
        self.reveal = true;
    }

    /// Applies a cosmic-text motion to the cursor, in document coordinates. The
    /// end of one segment and the start of the next are the same document
    /// position, so a motion that lands on that boundary is applied again.
    fn seg_motion(&mut self, fs: &mut FontSystem, m: Motion, x: Option<i32>) -> Option<(Cursor, Option<i32>)> {
        let from = (self.cursor.line, self.cursor.index);
        let mut wc = self.doc_to_win(self.cursor);
        let mut x = x;
        for _ in 0..2 {
            let (c, nx) = self.buf.cursor_motion(fs, wc, x, m)?;
            let d = self.win_to_doc(c);
            if (d.line, d.index) != from || c == wc {
                return Some((d, nx));
            }
            (wc, x) = (c, nx);
        }
        Some((self.win_to_doc(wc), x))
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
        let (line, off) = self.segs[run.line_i];
        Some((line, r.start + off..r.end + off, run.text[r].to_owned()))
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
                let (dl, off) = self.segs[li];
                let gs = g.start + off;
                let hovered = hover.is_some_and(|(hl, hr)| *hl == dl && gs >= hr.start && gs < hr.end);
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
        assert_mirrors(&v);
        v.undo();
        v.undo();
        v.prepare(&mut fs);
        assert_eq!(v.doc.line(10), "line 10");
        assert_mirrors(&v);
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
        assert_mirrors(&v);
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

    /// The layout window must mirror the document exactly: each segment's text
    /// matches its slice of the document line, and segments cover whole lines.
    fn assert_mirrors(v: &View) {
        for (k, l) in v.buf.lines.iter().enumerate() {
            let (line, start) = v.segs[k];
            let end = match v.segs.get(k + 1) {
                Some(&(nl, ns)) if nl == line => ns,
                _ => v.doc.line_len(line),
            };
            if k == 0 || v.segs[k - 1].0 != line {
                assert_eq!(start, 0, "segment {k} should start its line");
            }
            assert_eq!(l.text(), v.doc.line_slice(line, start, end), "segment {k} of line {line}");
        }
    }

    #[test]
    fn split_line_respects_limits_and_characters() {
        assert_eq!(split_line("short"), vec![0]);
        let text = "aé😀 ".repeat(5000); // multi-byte characters throughout
        let starts = split_line(&text);
        assert!(starts.len() > 1);
        for w in starts.windows(2) {
            assert!(w[1] > w[0] && w[1] - w[0] <= SEG + 64);
            assert!(text.is_char_boundary(w[1]));
        }
        // One enormous grapheme cluster still makes progress.
        let mut combining = String::from("a");
        combining.push_str(&"\u{301}".repeat(3000));
        let starts = split_line(&combining);
        assert!(starts.len() > 1 && starts.iter().all(|&i| combining.is_char_boundary(i)));
    }

    #[test]
    fn long_line_is_segmented_and_lazy() {
        let text = "abc def; ".repeat(600_000); // one 5.4 MB line
        let t = std::time::Instant::now();
        let (_fs, v) = view(&text);
        let elapsed = t.elapsed();
        assert!(v.window_len() > 2000, "{} segments", v.window_len());
        assert_mirrors(&v);
        // Only the visible segments are shaped.
        let shaped = v.buf.lines.iter().filter(|l| l.shape_opt().is_some()).count();
        assert!(shaped < 50, "{shaped} segments shaped");
        assert!(elapsed < std::time::Duration::from_secs(2), "took {elapsed:?}");
    }

    #[test]
    fn editing_a_long_line() {
        let text = format!("{}\nnext", "word ".repeat(2000)); // 10 KB line + a short one
        let (mut fs, mut v) = view(&text);
        let segs_before = v.window_len();
        // Type in the middle of a segment: only that segment changes.
        v.cursor = Cursor::new(0, 3000);
        v.type_text("X");
        v.prepare(&mut fs);
        assert_eq!(v.window_len(), segs_before);
        assert_mirrors(&v);
        // Backspace and Delete across a segment boundary.
        let boundary = v.segs[1].1;
        v.cursor = Cursor::new(0, boundary);
        v.backspace();
        v.prepare(&mut fs);
        assert_mirrors(&v);
        v.cursor = Cursor::new(0, v.segs[1].1);
        v.delete();
        v.prepare(&mut fs);
        assert_mirrors(&v);
        // A newline in the middle re-splits the line and shifts later lines.
        v.cursor = Cursor::new(0, 5000);
        v.replace_selection("\n", false);
        v.prepare(&mut fs);
        assert_mirrors(&v);
        assert_eq!(v.doc.line(2), "next");
        // Undo everything: back to the original text.
        while v.doc.can_undo() {
            v.undo();
        }
        v.prepare(&mut fs);
        assert_mirrors(&v);
        assert_eq!(v.doc.to_string_eol("\n"), text);
    }

    #[test]
    fn motion_crosses_segment_boundaries() {
        let text = "x".repeat(5000);
        let (mut fs, mut v) = view(&text);
        let boundary = v.segs[1].1;
        v.cursor = Cursor::new(0, boundary + 1);
        v.motion(&mut fs, Motion::Left, false);
        v.prepare(&mut fs);
        assert_eq!(v.cursor.index, boundary);
        v.motion(&mut fs, Motion::Left, false); // must not get stuck on the boundary
        v.prepare(&mut fs);
        assert_eq!(v.cursor.index, boundary - 1);
        v.motion(&mut fs, Motion::Right, false);
        v.motion(&mut fs, Motion::Right, false);
        v.prepare(&mut fs);
        assert_eq!(v.cursor.index, boundary + 1);
        // Scrolling a single huge line works in segment units.
        let (total, _) = v.scroll_info();
        assert_eq!(total, v.window_len());
    }
}
