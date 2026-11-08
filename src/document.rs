//! The document: a rope holding the full text (with `\n` line breaks) plus
//! undo/redo history. Positions are `cosmic_text::Cursor`s whose `line` is a
//! document line and `index` a byte offset within that line.

use cosmic_text::Cursor;
use ropey::Rope;
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

/// One reversible replacement: `removed` was replaced by `inserted` at `start`.
struct Op {
    start: Cursor,
    removed: String,
    inserted: String,
}

/// Which document lines an edit touched, for updating the layout window.
#[derive(Clone, Copy, Debug)]
pub struct EditResult {
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
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

pub struct Document {
    rope: Rope,
    undo: Vec<Vec<Op>>,
    redo: Vec<Vec<Op>>,
    /// Undo depth at the last save; `None` if that state is unreachable.
    saved: Option<usize>,
    can_merge: bool,
}

impl Document {
    pub fn new(text: &str) -> Self {
        Self { rope: Rope::from_str(&normalize(text)), undo: Vec::new(), redo: Vec::new(), saved: Some(0), can_merge: false }
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
        (removed, EditResult { first: a.line, old_last: b.line, new_last: end.line, end })
    }

    fn record(&mut self, ops: Vec<Op>, merge: bool) {
        if self.saved.is_some_and(|s| s > self.undo.len()) {
            self.saved = None;
        }
        self.redo.clear();
        let at_save = self.saved == Some(self.undo.len());
        match self.undo.last_mut() {
            Some(g) if merge && self.can_merge && !at_save => g.extend(ops),
            _ => self.undo.push(ops),
        }
        self.can_merge = merge;
    }

    /// Replaces the range `a..b` with `text`, recording it for undo. With
    /// `merge`, it joins the previous undo step if that was also mergeable.
    pub fn replace(&mut self, a: Cursor, b: Cursor, text: &str, merge: bool) -> EditResult {
        let (a, b) = order(a, b);
        let text = normalize(text);
        let (removed, res) = self.replace_raw(a, b, &text);
        if !(removed.is_empty() && text.is_empty()) {
            self.record(vec![Op { start: a, removed, inserted: text.into_owned() }], merge);
        }
        res
    }

    /// Replaces many non-overlapping ranges (sorted ascending) as one undo step.
    pub fn replace_many(&mut self, ranges: &[(Cursor, Cursor)], text: &str) {
        let text = normalize(text);
        let mut ops = Vec::with_capacity(ranges.len());
        for &(a, b) in ranges.iter().rev() {
            let (removed, _) = self.replace_raw(a, b, &text);
            ops.push(Op { start: a, removed, inserted: text.to_string() });
        }
        if !ops.is_empty() {
            self.record(ops, false);
        }
    }

    /// Ends the current typing group so the next edit starts a new undo step.
    pub fn break_merge(&mut self) {
        self.can_merge = false;
    }

    pub fn undo(&mut self) -> Option<Cursor> {
        let group = self.undo.pop()?;
        let mut cursor = Cursor::default();
        for op in group.iter().rev() {
            let end = end_of(op.start, &op.inserted);
            self.replace_raw(op.start, end, &op.removed);
            cursor = end_of(op.start, &op.removed);
        }
        self.redo.push(group);
        self.can_merge = false;
        Some(cursor)
    }

    pub fn redo(&mut self) -> Option<Cursor> {
        let group = self.redo.pop()?;
        let mut cursor = Cursor::default();
        for op in &group {
            let end = end_of(op.start, &op.removed);
            self.replace_raw(op.start, end, &op.inserted);
            cursor = end_of(op.start, &op.inserted);
        }
        self.undo.push(group);
        self.can_merge = false;
        Some(cursor)
    }

    pub fn is_dirty(&self) -> bool {
        Some(self.undo.len()) != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
        self.can_merge = false;
    }

    /// Previous code point (Backspace deletes one code point, as most editors do).
    pub fn prev_char(&self, c: Cursor) -> Cursor {
        if c.index == 0 {
            return if c.line == 0 { c } else { Cursor::new(c.line - 1, self.line_len(c.line - 1)) };
        }
        let line = self.line(c.line);
        Cursor::new(c.line, line[..c.index].char_indices().next_back().map_or(0, |(i, _)| i))
    }

    /// Next grapheme cluster boundary (Delete removes a whole cluster).
    pub fn next_grapheme(&self, c: Cursor) -> Cursor {
        let line = self.line(c.line);
        if c.index >= line.len() {
            return if c.line + 1 < self.len_lines() { Cursor::new(c.line + 1, 0) } else { c };
        }
        let len = line[c.index..].graphemes(true).next().map_or(0, str::len);
        Cursor::new(c.line, c.index + len)
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
    fn replace_many_is_one_step() {
        let mut d = Document::new("a b a\na");
        d.replace_many(&[(c(0, 0), c(0, 1)), (c(0, 4), c(0, 5)), (c(1, 0), c(1, 1))], "xyz");
        assert_eq!(d.to_string_eol("\n"), "xyz b xyz\nxyz");
        d.undo();
        assert_eq!(d.to_string_eol("\n"), "a b a\na");
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
