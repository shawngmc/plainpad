//! The document: a rope holding the full text (with `\n` line breaks) plus
//! undo/redo history. Positions are `cosmic_text::Cursor`s whose `line` is a
//! document line and `index` a byte offset within that line.

use cosmic_text::Cursor;
use ropey::Rope;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
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

/// One change, for the crash-recovery journal: `remove` bytes at byte offset
/// `at` were replaced by `insert`.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalOp {
    pub at: usize,
    pub remove: usize,
    pub insert: String,
}

/// Changes since the journal was last taken; see [`Document::take_journal`].
pub struct Journal {
    /// Identifies the state the journal started from. It changes when the
    /// document is saved, or the journal restarted.
    pub epoch: u64,
    /// The journal started from the saved text (so the saved file is a valid base).
    pub from_saved: bool,
    /// None if changes were too big to journal; the whole text must be written.
    pub ops: Option<Vec<JournalOp>>,
}

/// Applies journal entries to `rope`, checking each lands inside the text and
/// on character boundaries (a damaged journal must not panic).
pub fn replay(rope: &mut Rope, ops: &[JournalOp]) -> Result<(), String> {
    for op in ops {
        let end = op.at.checked_add(op.remove).filter(|&e| e <= rope.len_bytes()).ok_or("edit past the end")?;
        let (ca, cb) = (rope.byte_to_char(op.at), rope.byte_to_char(end));
        if rope.char_to_byte(ca) != op.at || rope.char_to_byte(cb) != end {
            return Err("edit inside a character".into());
        }
        rope.remove(ca..cb);
        rope.insert(ca, &op.insert);
    }
    Ok(())
}

/// Unwritten journal entries beyond this give up on journaling until the next full snapshot.
const JOURNAL_CAP: usize = 16 * 1024 * 1024;

fn next_epoch() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
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
    /// Bumped on every change to the text, so autosave knows what's new.
    version: u64,
    /// Edits not yet handed to crash recovery, and how much memory they use.
    journal: Vec<JournalOp>,
    journal_bytes: usize,
    /// A change was too big to journal (Replace All, or too much unwritten).
    journal_broken: bool,
    epoch: u64,
    epoch_from_saved: bool,
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
            version: 0,
            journal: Vec::new(),
            journal_bytes: 0,
            journal_broken: false,
            epoch: next_epoch(),
            epoch_from_saved: true,
        }
    }

    /// Changes whenever the text does.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// A cheap copy of the text (ropes share their contents), for writing a
    /// snapshot on another thread.
    pub fn snapshot(&self) -> Rope {
        self.rope.clone()
    }

    /// The text, for searching and saving (on this thread; use [`Self::snapshot`] for another).
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Absolute byte offset of a position.
    pub fn offset(&self, c: Cursor) -> usize {
        self.rope.line_to_byte(c.line) + c.index
    }

    /// The position at an absolute byte offset.
    pub fn cursor_at(&self, byte: usize) -> Cursor {
        let line = self.rope.byte_to_line(byte);
        Cursor::new(line, byte - self.rope.line_to_byte(line))
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
    #[cfg(test)]
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
        self.version += 1;
        let (ca, cb) = (self.char_idx(a), self.char_idx(b));
        let removed = self.rope.slice(ca..cb).to_string();
        let at = self.offset(a);
        self.rope.remove(ca..cb);
        self.rope.insert(ca, text);
        self.journal_push(JournalOp { at, remove: removed.len(), insert: text.to_owned() });
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
    #[cfg(test)]
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
        let abs: Vec<Range<usize>> = ranges.iter().map(|&(a, b)| self.offset(a)..self.offset(b)).collect();
        let p = prepare_replace_all(&self.rope, &abs, &text);
        self.apply_prepared(p);
    }

    /// Installs a Replace All computed by [`prepare_replace_all`] (possibly on
    /// another thread, from a snapshot of this exact text).
    pub fn apply_prepared(&mut self, p: Prepared) {
        self.version += 1;
        self.journal_break();
        self.rope = p.rope;
        self.record(vec![Op::Many(p.many)], false);
    }

    /// Reverts one op, returning where the cursor should go.
    fn revert(&mut self, op: &Op) -> Cursor {
        match op {
            Op::Edit { start, removed, inserted } => {
                self.replace_raw(*start, end_of(*start, inserted), removed);
                end_of(*start, removed)
            }
            Op::Many(m) if rebuild_pays(m, &self.rope) => {
                self.version += 1;
                self.journal_break();
                self.rope = splice_many(&self.rope, m, false);
                let (l, i) = m.starts[0];
                Cursor::new(l, i + m.removed_lens[0] as usize)
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
            Op::Many(m) if rebuild_pays(m, &self.rope) => {
                self.version += 1;
                self.journal_break();
                self.rope = splice_many(&self.rope, m, true);
                let (l, i) = m.starts[0];
                Cursor::new(l, i + m.inserted.len())
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
        self.restart_journal();
    }

    // ------------------------------------------------------------ journal

    fn journal_push(&mut self, op: JournalOp) {
        if self.journal_broken {
            return;
        }
        self.journal_bytes += op.insert.len() + std::mem::size_of::<JournalOp>();
        self.journal.push(op);
        if self.journal_bytes > JOURNAL_CAP {
            self.journal_break();
        }
    }

    fn journal_break(&mut self) {
        self.journal_broken = true;
        self.journal = Vec::new();
        self.journal_bytes = 0;
    }

    /// Starts the journal afresh from the current text.
    pub fn restart_journal(&mut self) {
        self.journal = Vec::new();
        self.journal_bytes = 0;
        self.journal_broken = false;
        self.epoch = next_epoch();
        self.epoch_from_saved = !self.is_dirty();
    }

    /// Whether there are journal entries waiting (or the journal broke).
    pub fn journal_pending(&self) -> bool {
        self.journal_broken || !self.journal.is_empty()
    }

    /// Hands over the changes since the last call (or since the journal
    /// started). Applied in order to the text as it was then, they give the
    /// text now. If they couldn't be kept, the journal restarts from the
    /// current text and `ops` is None: the caller must record the whole text.
    pub fn take_journal(&mut self) -> Journal {
        if self.journal_broken {
            self.restart_journal();
            return Journal { epoch: self.epoch, from_saved: self.epoch_from_saved, ops: None };
        }
        self.journal_bytes = 0;
        let ops = std::mem::take(&mut self.journal);
        Journal { epoch: self.epoch, from_saved: self.epoch_from_saved, ops: Some(ops) }
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

/// A Replace All ready to install: the new text and its undo record.
pub struct Prepared {
    rope: Rope,
    many: Many,
}

impl Prepared {
    pub fn count(&self) -> usize {
        self.many.starts.len()
    }

    /// Where the first replacement starts.
    pub fn first(&self) -> Cursor {
        let (l, i) = self.many.starts[0];
        Cursor::new(l, i)
    }
}

/// Walks forward through a rope once, handing on the text it passes over and
/// keeping count of lines. Replace All and its undo use it to visit every
/// match in a single pass, instead of looking each one up in the tree.
struct Walk<'a> {
    chunks: ropey::iter::Chunks<'a>,
    /// The not yet visited part of the current chunk.
    rest: &'a str,
    /// Byte offset of `rest`, its line, and where that line starts.
    pos: usize,
    line: usize,
    line_start: usize,
}

impl<'a> Walk<'a> {
    fn new(rope: &'a Rope) -> Self {
        Self { chunks: rope.chunks(), rest: "", pos: 0, line: 0, line_start: 0 }
    }

    /// The next non-empty piece of text, or None at the end.
    fn fill(&mut self) -> bool {
        while self.rest.is_empty() {
            match self.chunks.next() {
                Some(c) => self.rest = c,
                None => return false,
            }
        }
        true
    }

    /// Moves to byte `to` (a char boundary), handing each piece passed over to `out`.
    fn advance(&mut self, to: usize, out: &mut dyn FnMut(&'a str)) {
        while self.pos < to && self.fill() {
            let (piece, rest) = self.rest.split_at((to - self.pos).min(self.rest.len()));
            if let Some(k) = piece.rfind('\n') {
                self.line += piece.bytes().filter(|&b| b == b'\n').count();
                self.line_start = self.pos + k + 1;
            }
            out(piece);
            self.pos += piece.len();
            self.rest = rest;
        }
    }

    /// Moves to the start of line `l` (unless already on it).
    fn skip_to_line(&mut self, l: usize, out: &mut dyn FnMut(&'a str)) {
        while self.line < l && self.fill() {
            let n = self.rest.find('\n').map_or(self.rest.len(), |k| k + 1);
            let pos = self.pos + n;
            self.advance(pos, out);
        }
    }
}

/// Builds the result of replacing every range in `matches` (absolute byte
/// ranges, ascending, non-overlapping, none containing a line break) with
/// `text` (no line breaks either), in one pass over the rope. This can run on
/// a background thread with a snapshot. `matches` must not be empty.
pub fn prepare_replace_all(rope: &Rope, matches: &[Range<usize>], text: &str) -> Prepared {
    debug_assert!(!text.contains('\n') && !matches.is_empty());
    let mut starts = Vec::with_capacity(matches.len());
    let mut removed = String::new();
    let mut removed_lens = Vec::with_capacity(matches.len());
    let mut b = ropey::RopeBuilder::new();
    let mut w = Walk::new(rope);
    for r in matches {
        w.advance(r.start, &mut |p| b.append(p));
        starts.push((w.line, r.start - w.line_start));
        w.advance(r.end, &mut |p| removed.push_str(p));
        b.append(text);
        removed_lens.push((r.end - r.start) as u32);
    }
    w.advance(rope.len_bytes(), &mut |p| b.append(p));
    removed.shrink_to_fit();
    let many = Many { starts, removed, removed_lens, inserted: text.to_owned() };
    Prepared { rope: b.finish(), many }
}

/// Rebuilding the whole rope beats editing it match by match (each edit is
/// O(log n) but slow in absolute terms) once there are many matches.
fn rebuild_pays(m: &Many, rope: &Rope) -> bool {
    m.starts.len() * 1000 > rope.len_bytes()
}

/// Applies (`forward`) or reverts a Replace All in one pass, copying the text
/// between matches into a new rope.
fn splice_many(rope: &Rope, m: &Many, forward: bool) -> Rope {
    let mut b = ropey::RopeBuilder::new();
    let mut w = Walk::new(rope);
    let ins = m.inserted.len();
    let (mut off, mut prev_line, mut shift) = (0, usize::MAX, 0isize);
    for (&(l, i), &rl) in m.starts.iter().zip(&m.removed_lens) {
        let rl = rl as usize;
        w.skip_to_line(l, &mut |p| b.append(p));
        if l != prev_line {
            (prev_line, shift) = (l, 0);
        }
        // Positions are in the original text. In the replaced text (when
        // reverting), earlier matches on the same line have shifted this one.
        let at = (w.line_start + i).wrapping_add_signed(shift);
        let (old_len, new) = if forward { (rl, &m.inserted[..]) } else { (ins, &m.removed[off..off + rl]) };
        w.advance(at, &mut |p| b.append(p));
        b.append(new);
        w.advance(at + old_len, &mut |_| {});
        if !forward {
            shift += ins as isize - rl as isize;
        }
        off += rl;
    }
    w.advance(rope.len_bytes(), &mut |p| b.append(p));
    b.finish()
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
    fn replace_all_undo_both_ways() {
        // Few matches in a long text are undone match by match; many in a short
        // one by rebuilding the rope. Both must restore the text exactly.
        for filler in ["", &"-".repeat(20_000)] {
            let text = format!("Ab ab{filler}\nx AB ab ab\n{filler}ab");
            let mut d = Document::new(&text);
            let abs: Vec<_> = text.match_indices(['a', 'A']).map(|(i, _)| i..i + 2).collect();
            let p = prepare_replace_all(d.rope(), &abs, "Q");
            assert_eq!(p.count(), 6);
            d.apply_prepared(p);
            let after = text.replace("Ab", "Q").replace("ab", "Q").replace("AB", "Q");
            assert_eq!(d.to_string_eol("\n"), after);
            assert_eq!(d.undo(), Some(c(0, 2)));
            assert_eq!(d.to_string_eol("\n"), text);
            d.redo();
            assert_eq!(d.to_string_eol("\n"), after);
        }
    }

    #[test]
    fn journal_replays_to_the_same_text() {
        let base = "héllo wörld\nsecond line\n日本語";
        let mut d = Document::new(base);
        let mut copy = Rope::from_str(base);
        let check = |d: &mut Document, copy: &mut Rope| {
            let j = d.take_journal();
            replay(copy, &j.ops.expect("journal intact")).unwrap();
            assert_eq!(*copy, d.rope);
        };
        d.replace(c(0, 1), c(0, 3), "E", false);
        d.replace(c(2, 3), c(2, 3), "\nnew", false);
        check(&mut d, &mut copy);
        d.undo();
        d.undo();
        d.redo();
        d.replace(c(1, 0), c(1, 6), "", false);
        check(&mut d, &mut copy);
        assert!(d.take_journal().ops.unwrap().is_empty());
        // Saving starts a new journal from the saved text.
        let e = d.epoch;
        d.mark_saved();
        let j = d.take_journal();
        assert!(j.epoch != e && j.from_saved && j.ops.unwrap().is_empty());
        // Replace All can't be journaled: the caller is told to write everything.
        let abs = vec![0..1];
        let p = prepare_replace_all(d.rope(), &abs, "Z");
        d.apply_prepared(p);
        let j = d.take_journal();
        assert!(j.ops.is_none() && !j.from_saved);
        assert!(d.take_journal().ops.unwrap().is_empty(), "journaling resumes after that");
        // A damaged journal is refused, not applied.
        let mut r = Rope::from_str("日本");
        assert!(replay(&mut r, &[JournalOp { at: 1, remove: 0, insert: "x".into() }]).is_err());
        assert!(replay(&mut r, &[JournalOp { at: 3, remove: 9, insert: "x".into() }]).is_err());
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
