//! The document: a rope holding the full text (with `\n` line breaks) plus
//! undo/redo history. Positions are `cosmic_text::Cursor`s whose `line` is a
//! document line and `index` a byte offset within that line.

use cosmic_text::Cursor;
use ropey::Rope;
use std::borrow::Cow;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

/// Undo and redo history may use about this much memory together; beyond it the
/// oldest undo steps are dropped (the most recent step is always kept).
const UNDO_BUDGET: usize = 64 * 1024 * 1024;

/// One reversible change.
enum Op {
    /// `removed` was replaced by `inserted` at `start`.
    Edit { start: Cursor, removed: String, inserted: String },
    /// Replace All: many single-line matches replaced by the same text.
    Many(Many),
}

/// Replace All, stored compactly: a few bytes per match instead of two strings
/// each. Neither the matches nor the replacement contain line breaks.
struct Many {
    /// Match positions (line, byte index) before the edit, ascending.
    starts: Vec<(usize, usize)>,
    /// Each match's original text, concatenated (they can differ in case).
    removed: String,
    removed_lens: Vec<u32>,
    inserted: String,
}

impl Op {
    /// Approximate memory use, for the undo budget.
    fn bytes(&self) -> usize {
        std::mem::size_of::<Op>()
            + match self {
                Op::Edit { removed, inserted, .. } => removed.capacity() + inserted.capacity(),
                Op::Many(m) => {
                    m.starts.capacity() * std::mem::size_of::<(usize, usize)>()
                        + m.removed.capacity()
                        + m.removed_lens.capacity() * 4
                        + m.inserted.capacity()
                }
            }
    }
}

/// One undo step: the changes it made, in the order they were made.
struct Group {
    ops: Vec<Op>,
    bytes: usize,
}

impl Group {
    fn new(ops: Vec<Op>) -> Self {
        let bytes = ops.iter().map(Op::bytes).sum();
        Self { ops, bytes }
    }
}

/// Which document lines an edit touched, for updating the layout window.
#[derive(Clone, Copy, Debug)]
pub struct EditResult {
    /// Start of the edit, and the end of the replaced text (before the edit).
    pub start: Cursor,
    pub old_end: Cursor,
    pub first: usize,
    /// Last line of the edited range before the edit.
    pub old_last: usize,
    /// Last line of the edited range after the edit.
    pub new_last: usize,
    /// End of the inserted text.
    pub end: Cursor,
}

pub fn order(a: Cursor, b: Cursor) -> (Cursor, Cursor) {
    if (a.line, a.index) <= (b.line, b.index) { (a, b) } else { (b, a) }
}

/// Position just past `text` when inserted at `start`.
fn end_of(start: Cursor, text: &str) -> Cursor {
    match text.rfind('\n') {
        None => Cursor::new(start.line, start.index + text.len()),
        Some(i) => Cursor::new(start.line + text.matches('\n').count(), text.len() - i - 1),
    }
}

fn normalize(text: &str) -> Cow<'_, str> {
    if text.contains('\r') { Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n")) } else { Cow::Borrowed(text) }
}

pub struct Document {
    rope: Rope,
    undo: VecDeque<Group>,
    redo: Vec<Group>,
    /// Memory used by `undo` and `redo` together (approximate).
    history_bytes: usize,
    /// Undo steps dropped from the front to stay within the budget.
    evicted: usize,
    /// History depth (`evicted + undo.len()`) at the last save; `None` if that
    /// state is unreachable.
    saved: Option<usize>,
    can_merge: bool,
}

impl Document {
    pub fn new(text: &str) -> Self {
        Self::from_rope(Rope::from_str(&normalize(text)))
    }

    /// Takes an already-normalised (`\n`-only) rope, e.g. from the streaming loader.
    pub fn from_rope(rope: Rope) -> Self {
        Self {
            rope,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_bytes: 0,
            evicted: 0,
            saved: Some(0),
            can_merge: false,
        }
    }

    fn depth(&self) -> usize {
        self.evicted + self.undo.len()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Line text without its trailing newline. Borrowed when contiguous in the rope.
    pub fn line(&self, i: usize) -> Cow<'_, str> {
        let mut s: Cow<str> = self.rope.line(i).into();
        if s.ends_with('\n') {
            match &mut s {
                Cow::Borrowed(b) => *b = &b[..b.len() - 1],
                Cow::Owned(o) => {
                    o.pop();
                }
            }
        }
        s
    }

    pub fn line_len(&self, i: usize) -> usize {
        let l = self.rope.line(i);
        l.len_bytes() - (l.len_chars() > 0 && l.char(l.len_chars() - 1) == '\n') as usize
    }

    pub fn end(&self) -> Cursor {
        let l = self.len_lines() - 1;
        Cursor::new(l, self.line_len(l))
    }

    fn char_idx(&self, c: Cursor) -> usize {
        self.rope.byte_to_char(self.rope.line_to_byte(c.line) + c.index)
    }

    /// Number of chars between two positions (O(log n)).
    pub fn chars_between(&self, a: Cursor, b: Cursor) -> usize {
        let (a, b) = order(a, b);
        self.char_idx(b) - self.char_idx(a)
    }

    pub fn text_range(&self, a: Cursor, b: Cursor) -> String {
        let (a, b) = order(a, b);
        self.rope.slice(self.char_idx(a)..self.char_idx(b)).to_string()
    }

    /// Whole text with `\n` replaced by `eol`.
    pub fn to_string_eol(&self, eol: &str) -> String {
        let mut s = String::with_capacity(self.rope.len_bytes() + self.len_lines() * (eol.len() - 1));
        for chunk in self.rope.chunks() {
            if eol == "\n" {
                s.push_str(chunk);
            } else {
                for (i, part) in chunk.split('\n').enumerate() {
                    if i > 0 {
                        s.push_str(eol);
                    }
                    s.push_str(part);
                }
            }
        }
        s
    }

    /// Clamps a position to the document and to a char boundary.
    pub fn clamp(&self, c: Cursor) -> Cursor {
        let line = c.line.min(self.len_lines() - 1);
        let text = self.line(line);
        let mut index = c.index.min(text.len());
        while !text.is_char_boundary(index) {
            index -= 1;
        }
        Cursor { line, index, affinity: c.affinity }
    }

    fn replace_raw(&mut self, a: Cursor, b: Cursor, text: &str) -> (String, EditResult) {
        let (ca, cb) = (self.char_idx(a), self.char_idx(b));
        let removed = self.rope.slice(ca..cb).to_string();
        self.rope.remove(ca..cb);
        self.rope.insert(ca, text);
        let end = end_of(a, text);
        (removed, EditResult { start: a, old_end: b, first: a.line, old_last: b.line, new_last: end.line, end })
    }

    fn record(&mut self, ops: Vec<Op>, merge: bool) {
        if self.saved.is_some_and(|s| s > self.depth()) {
            self.saved = None;
        }
        for g in self.redo.drain(..) {
            self.history_bytes -= g.bytes;
        }
        let at_save = self.saved == Some(self.depth());
        let group = Group::new(ops);
        self.history_bytes += group.bytes;
        match self.undo.back_mut() {
            Some(g) if merge && self.can_merge && !at_save => {
                g.bytes += group.bytes;
                g.ops.extend(group.ops);
            }
            _ => self.undo.push_back(group),
        }
        self.can_merge = merge;
        self.enforce_budget();
    }

    /// Drops the oldest undo steps while history exceeds the budget.
    fn enforce_budget(&mut self) {
        let mut dropped = false;
        while self.history_bytes > UNDO_BUDGET && self.undo.len() > 1 {
            let g = self.undo.pop_front().expect("len > 1");
            self.history_bytes -= g.bytes;
            self.evicted += 1;
            dropped = true;
        }
        if dropped {
            crate::mem::trim();
        }
    }

    /// Memory used by undo/redo history (approximate).
    #[cfg(test)]
    fn history_bytes(&self) -> usize {
        self.history_bytes
    }

    /// Replaces the range `a..b` with `text`, recording it for undo. With
    /// `merge`, it joins the previous undo step if that was also mergeable.
    pub fn replace(&mut self, a: Cursor, b: Cursor, text: &str, merge: bool) -> EditResult {
        let (a, b) = order(a, b);
        let text = normalize(text);
        let (removed, res) = self.replace_raw(a, b, &text);
        if !(removed.is_empty() && text.is_empty()) {
            self.record(vec![Op::Edit { start: a, removed, inserted: text.into_owned() }], merge);
        }
        res
    }

    /// Replaces many non-overlapping single-line ranges (sorted ascending) as one
    /// undo step, stored compactly.
    pub fn replace_many(&mut self, ranges: &[(Cursor, Cursor)], text: &str) {
        if ranges.is_empty() {
            return;
        }
        let text = normalize(text);
        let single_line = !text.contains('\n') && ranges.iter().all(|(a, b)| a.line == b.line);
        if !single_line {
            // Not expected from Replace All, but keep it correct: one plain op per range.
            let mut ops = Vec::with_capacity(ranges.len());
            for &(a, b) in ranges.iter().rev() {
                let (removed, _) = self.replace_raw(a, b, &text);
                ops.push(Op::Edit { start: a, removed, inserted: text.to_string() });
            }
            self.record(ops, false);
            return;
        }
        let mut lens = Vec::with_capacity(ranges.len());
        let mut parts = Vec::with_capacity(ranges.len());
        for &(a, b) in ranges.iter().rev() {
            let (removed, _) = self.replace_raw(a, b, &text);
            lens.push(removed.len() as u32);
            parts.push(removed);
        }
        lens.reverse();
        parts.reverse();
        let many = Many {
            starts: ranges.iter().map(|(a, _)| (a.line, a.index)).collect(),
            removed: parts.concat(),
            removed_lens: lens,
            inserted: text.into_owned(),
        };
        self.record(vec![Op::Many(many)], false);
    }

    /// Reverts one op, returning where the cursor should go.
    fn revert(&mut self, op: &Op) -> Cursor {
        match op {
            Op::Edit { start, removed, inserted } => {
                self.replace_raw(*start, end_of(*start, inserted), removed);
                end_of(*start, removed)
            }
            Op::Many(m) => {
                // Restore left to right: everything before each match is then back to
                // its original text, so the match sits at its original position.
                let mut off = 0;
                for (&(l, i), &rl) in m.starts.iter().zip(&m.removed_lens) {
                    let rl = rl as usize;
                    let a = Cursor::new(l, i);
                    self.replace_raw(a, Cursor::new(l, i + m.inserted.len()), &m.removed[off..off + rl]);
                    off += rl;
                }
                let (l, i) = m.starts[0];
                Cursor::new(l, i + m.removed_lens[0] as usize)
            }
        }
    }

    /// Re-applies one op, returning where the cursor should go.
    fn reapply(&mut self, op: &Op) -> Cursor {
        match op {
            Op::Edit { start, removed, inserted } => {
                self.replace_raw(*start, end_of(*start, removed), inserted);
                end_of(*start, inserted)
            }
            Op::Many(m) => {
                for (&(l, i), &rl) in m.starts.iter().zip(&m.removed_lens).rev() {
                    self.replace_raw(Cursor::new(l, i), Cursor::new(l, i + rl as usize), &m.inserted);
                }
                let (l, i) = m.starts[0];
                Cursor::new(l, i + m.inserted.len())
            }
        }
    }

    /// Ends the current typing group so the next edit starts a new undo step.
    pub fn break_merge(&mut self) {
        self.can_merge = false;
    }

    pub fn undo(&mut self) -> Option<Cursor> {
        let group = self.undo.pop_back()?;
        let mut cursor = Cursor::default();
        for op in group.ops.iter().rev() {
            cursor = self.revert(op);
        }
        let big = group.bytes > 1024 * 1024;
        self.redo.push(group);
        self.can_merge = false;
        if big {
            crate::mem::trim(); // a large step allocated and freed temporary text
        }
        Some(cursor)
    }

    pub fn redo(&mut self) -> Option<Cursor> {
        let group = self.redo.pop()?;
        let mut cursor = Cursor::default();
        for op in &group.ops {
            cursor = self.reapply(op);
        }
        let big = group.bytes > 1024 * 1024;
        self.undo.push_back(group);
        self.can_merge = false;
        if big {
            crate::mem::trim();
        }
        Some(cursor)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        Some(self.depth()) != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.depth());
        self.can_merge = false;
    }

    /// Previous code point (Backspace deletes one code point, as most editors do).
    pub fn prev_char(&self, c: Cursor) -> Cursor {
        if c.index == 0 {
            return if c.line == 0 { c } else { Cursor::new(c.line - 1, self.line_len(c.line - 1)) };
        }
        // Straight from the rope: no copy of the (possibly huge) line.
        let ci = self.char_idx(c);
        let prev = self.rope.char_to_byte(ci - 1) - self.rope.line_to_byte(c.line);
        Cursor::new(c.line, prev)
    }

    /// Next grapheme cluster boundary (Delete removes a whole cluster).
    pub fn next_grapheme(&self, c: Cursor) -> Cursor {
        let len = self.line_len(c.line);
        if c.index >= len {
            return if c.line + 1 < self.len_lines() { Cursor::new(c.line + 1, 0) } else { c };
        }
        // Look at a small window after the cursor rather than copying the whole line.
        // A cluster longer than the window is cut at the window's end, which still
        // deletes valid characters.
        let ci = self.char_idx(c);
        let line_end = self.char_idx(Cursor::new(c.line, len));
        let ahead: String = self.rope.slice(ci..line_end.min(ci + 64)).to_string();
        let g = ahead.graphemes(true).next().map_or(0, str::len);
        Cursor::new(c.line, c.index + g)
    }

    /// Text of line `line` between byte offsets `a` and `b` (char boundaries).
    pub fn line_slice(&self, line: usize, a: usize, b: usize) -> String {
        let base = self.rope.line_to_byte(line);
        let (ca, cb) = (self.rope.byte_to_char(base + a), self.rope.byte_to_char(base + b));
        self.rope.slice(ca..cb).to_string()
    }

    /// 1-based column (in characters) of a position, without copying the line.
    pub fn column(&self, c: Cursor) -> usize {
        self.chars_between(Cursor::new(c.line, 0), c) + 1
    }

    /// The word (or run of whitespace/punctuation) containing `c`.
    pub fn word_at(&self, c: Cursor) -> (Cursor, Cursor) {
        let line = self.line(c.line);
        let mut found = (line.len(), line.len());
        for (i, w) in line.split_word_bound_indices() {
            if c.index < i + w.len() {
                found = (i, i + w.len());
                break;
            }
        }
        (Cursor::new(c.line, found.0), Cursor::new(c.line, found.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(l: usize, i: usize) -> Cursor {
        Cursor::new(l, i)
    }

    #[test]
    fn edits_and_undo() {
        let mut d = Document::new("hello\r\nworld");
        assert_eq!(d.len_lines(), 2);
        assert_eq!(d.line(1), "world");
        let r = d.replace(c(0, 5), c(0, 5), "!", true);
        assert_eq!(r.end, c(0, 6));
        d.replace(c(0, 6), c(0, 6), "?", true);
        assert_eq!(d.line(0), "hello!?");
        let r = d.replace(c(0, 2), c(1, 2), "XX\nYY", false);
        assert_eq!((r.first, r.old_last, r.new_last, r.end), (0, 1, 1, c(1, 2)));
        assert_eq!(d.to_string_eol("\r\n"), "heXX\r\nYYrld");
        assert!(d.is_dirty());
        d.undo();
        assert_eq!(d.to_string_eol("\n"), "hello!?\nworld");
        d.undo(); // typing "!?" merged into one step
        assert_eq!(d.to_string_eol("\n"), "hello\nworld");
        assert!(!d.is_dirty());
        d.redo();
        d.redo();
        assert_eq!(d.to_string_eol("\n"), "heXX\nYYrld");
    }

    #[test]
    fn replace_many_compact_undo_redo() {
        // Several matches per line, removed texts differing in case and length
        // from the replacement, and an untouched line in between.
        let text = "Ab ab AB\nnone here\nab-ab\nlast AB";
        let mut d = Document::new(text);
        let ranges = [
            (c(0, 0), c(0, 2)),
            (c(0, 3), c(0, 5)),
            (c(0, 6), c(0, 8)),
            (c(2, 0), c(2, 2)),
            (c(2, 3), c(2, 5)),
            (c(3, 5), c(3, 7)),
        ];
        d.replace_many(&ranges, "xyz");
        let after = "xyz xyz xyz\nnone here\nxyz-xyz\nlast xyz";
        assert_eq!(d.to_string_eol("\n"), after);
        d.undo();
        assert_eq!(d.to_string_eol("\n"), text);
        assert!(!d.is_dirty());
        d.redo();
        assert_eq!(d.to_string_eol("\n"), after);
        // Shorter replacement too.
        let mut d = Document::new("aaa aaa\naaa");
        d.replace_many(&[(c(0, 0), c(0, 3)), (c(0, 4), c(0, 7)), (c(1, 0), c(1, 3))], "b");
        assert_eq!(d.to_string_eol("\n"), "b b\nb");
        d.undo();
        assert_eq!(d.to_string_eol("\n"), "aaa aaa\naaa");
    }

    #[test]
    fn replace_many_is_compact() {
        let line = "lorem ipsum ".repeat(10);
        let text = vec![line.as_str(); 10_000].join("\n");
        let mut d = Document::new(&text);
        let mut ranges = Vec::new();
        for l in 0..10_000 {
            for k in 0..10 {
                ranges.push((c(l, k * 12), c(l, k * 12 + 5)));
            }
        }
        d.replace_many(&ranges, "LOREM");
        // About 30 bytes per match (it used to be ~140).
        assert!(d.history_bytes() < ranges.len() * 40, "{} bytes", d.history_bytes());
    }

    #[test]
    fn history_budget_drops_oldest_steps() {
        let mut d = Document::new("");
        let chunk = "x".repeat(4 * 1024 * 1024);
        for _ in 0..40 {
            d.replace(d.end(), d.end(), &chunk, false);
        }
        assert!(d.history_bytes() <= UNDO_BUDGET + chunk.len() * 2);
        // Undo works for the steps that remain, then stops; the saved (empty)
        // state is no longer reachable, so the document stays modified.
        let mut steps = 0;
        while d.undo().is_some() {
            steps += 1;
        }
        assert!((10..40).contains(&steps), "{steps} steps kept");
        assert!(d.is_dirty());
        assert_eq!(d.len_lines(), 1);
        assert_eq!(d.line_len(0), (40 - steps) * chunk.len());
    }

    #[test]
    fn replace_many_is_one_step() {
        let mut d = Document::new("a b a\na");
        d.replace_many(&[(c(0, 0), c(0, 1)), (c(0, 4), c(0, 5)), (c(1, 0), c(1, 1))], "xyz");
        assert_eq!(d.to_string_eol("\n"), "xyz b xyz\nxyz");
        d.undo();
        assert_eq!(d.to_string_eol("\n"), "a b a\na");
    }

    #[test]
    fn navigation_on_long_lines() {
        let text = format!("{}é\u{301}x", "a".repeat(100_000));
        let d = Document::new(&text);
        let at = c(0, 100_000);
        assert_eq!(d.next_grapheme(at), c(0, 100_004)); // é + combining accent
        assert_eq!(d.prev_char(c(0, 100_004)), c(0, 100_002));
        assert_eq!(d.line_slice(0, 99_998, 100_004), "aaé\u{301}");
        assert_eq!(d.column(c(0, 100_004)), 100_003);
    }

    #[test]
    fn navigation() {
        let d = Document::new("e\u{301}x\nab cd");
        assert_eq!(d.next_grapheme(c(0, 0)), c(0, 3));
        assert_eq!(d.prev_char(c(0, 3)), c(0, 1));
        assert_eq!(d.prev_char(c(1, 0)), c(0, 4));
        assert_eq!(d.word_at(c(1, 4)), (c(1, 3), c(1, 5)));
        assert_eq!(d.chars_between(c(0, 0), c(1, 2)), 6);
        assert_eq!(d.clamp(c(9, 99)), c(1, 5));
    }
}
