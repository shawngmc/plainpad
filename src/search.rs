//! Plain-text search and URL detection within a single line.

use ropey::Rope;
use std::borrow::Cow;
use std::ops::Range;

/// Finds the first match of `needle` in `hay` starting at byte `from`.
pub fn find(hay: &str, needle: &str, case: bool, from: usize) -> Option<Range<usize>> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    let from = floor_boundary(hay, from);
    if case {
        return hay[from..].find(needle).map(|i| from + i..from + i + needle.len());
    }
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    find_nocase(hay, &n, from)
}

/// Case-insensitive search. Only positions whose character could start a match
/// are tried: for an ASCII first letter that is a quick byte comparison, plus a
/// check of each non-ASCII character (a few, such as the Kelvin sign, lowercase
/// to ASCII letters).
fn find_nocase(hay: &str, lower: &[char], from: usize) -> Option<Range<usize>> {
    let first = lower[0];
    let try_at = |i: usize| match_at(&hay[i..], lower).map(|len| i..i + len);
    if first.is_ascii() {
        let (lo, up) = (first as u8, first.to_ascii_uppercase() as u8);
        let b = hay.as_bytes();
        let mut i = from;
        while i < b.len() {
            let c = b[i];
            if (c == lo || c == up || (c >= 0xC0 && starts_lower(&hay[i..], first)))
                && let Some(r) = try_at(i)
            {
                return Some(r);
            }
            i += 1;
        }
        None
    } else {
        hay[from..].char_indices().map(|(i, _)| from + i).filter(|&i| starts_lower(&hay[i..], first)).find_map(try_at)
    }
}

/// Whether the first character of `s` lowercases to something starting with `first`.
fn starts_lower(s: &str, first: char) -> bool {
    s.chars().next().and_then(|c| c.to_lowercase().next()) == Some(first)
}

/// Finds the last match of `needle` in `hay` that ends at or before byte `before`.
pub fn rfind(hay: &str, needle: &str, case: bool, before: usize) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let before = floor_boundary(hay, before.min(hay.len()));
    if case {
        return hay[..before].rfind(needle).map(|i| i..i + needle.len());
    }
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    hay[..before]
        .char_indices()
        .rev()
        .filter(|&(i, _)| starts_lower(&hay[i..], n[0]))
        .find_map(|(i, _)| match_at(&hay[i..before], &n).map(|len| i..i + len))
}

// ---------------------------------------------------------------- whole documents

/// The document is searched in windows of about this many bytes.
#[cfg(not(test))]
fn window() -> usize {
    1 << 20
}

// Tests can make windows tiny, to cross many window boundaries.
#[cfg(test)]
thread_local!(static TEST_WINDOW: std::cell::Cell<usize> = const { std::cell::Cell::new(1 << 20) });
#[cfg(test)]
fn window() -> usize {
    TEST_WINDOW.with(|w| w.get())
}

/// A search over a whole rope, in absolute byte offsets. Matches never span
/// lines: the find field can't contain a line break.
pub struct Query {
    needle: String,
    case: bool,
    /// The longest a match can be in the text, in bytes, so windows overlap enough.
    max_len: usize,
}

impl Query {
    pub fn new(needle: &str, case: bool) -> Self {
        let max_len = if case { needle.len() } else { needle.chars().flat_map(char::to_lowercase).count() * 4 };
        Self { needle: needle.to_owned(), case, max_len }
    }
}

/// Rounds a byte offset down to a char boundary.
fn floor_char(rope: &Rope, i: usize) -> usize {
    rope.char_to_byte(rope.byte_to_char(i.min(rope.len_bytes())))
}

/// The text from `a` to `b` (both char boundaries), borrowed when it's one rope chunk.
fn text(rope: &Rope, a: usize, b: usize) -> Cow<'_, str> {
    rope.byte_slice(a..b).into()
}

/// The first match starting in `from..to`. `more(done)` reports progress (bytes
/// searched) and returns false to give up.
pub fn find_forward(
    rope: &Rope,
    q: &Query,
    from: usize,
    to: usize,
    more: &mut dyn FnMut(usize) -> bool,
) -> Option<Range<usize>> {
    let len = rope.len_bytes();
    let mut pos = floor_char(rope, from);
    while pos < to {
        let end = floor_char(rope, (pos + window()).min(to)).max(floor_char(rope, pos + 4).min(to));
        let stop = floor_char(rope, (end + q.max_len).min(len));
        let t = text(rope, pos, stop);
        if let Some(r) = find(&t, &q.needle, q.case, 0)
            && pos + r.start < end
        {
            return Some(pos + r.start..pos + r.end);
        }
        pos = end;
        if !more(pos) {
            return None;
        }
    }
    None
}

/// The last match that ends at or before `before`.
pub fn find_backward(
    rope: &Rope,
    q: &Query,
    before: usize,
    more: &mut dyn FnMut(usize) -> bool,
) -> Option<Range<usize>> {
    let mut end = floor_char(rope, before);
    let mut done = 0;
    while end > 0 {
        let start = floor_char(rope, end.saturating_sub(window()));
        // Reach back far enough to see a match that crosses `start`.
        let from = floor_char(rope, start.saturating_sub(q.max_len));
        let t = text(rope, from, end);
        if let Some(r) = rfind(&t, &q.needle, q.case, t.len()) {
            return Some(from + r.start..from + r.end);
        }
        done += end - start;
        end = start;
        if !more(done) {
            return None;
        }
    }
    None
}

/// Find Next / Find Previous from byte `at`, wrapping around the end of the document.
pub fn find_wrapping(
    rope: &Rope,
    q: &Query,
    at: usize,
    forward: bool,
    more: &mut dyn FnMut(usize) -> bool,
) -> Option<Range<usize>> {
    let len = rope.len_bytes();
    if forward {
        let mut wrapped = |d: usize| more(d);
        find_forward(rope, q, at, len, &mut wrapped).or_else(|| {
            let mut wrapped = |d: usize| more(len - at + d);
            find_forward(rope, q, 0, at, &mut wrapped)
        })
    } else {
        find_backward(rope, q, at, more).or_else(|| {
            let mut wrapped = |d: usize| more(at + d);
            find_backward(rope, q, len, &mut wrapped).filter(|r| r.end > at)
        })
    }
}

/// Every match, left to right and not overlapping. None if `more` gave up.
pub fn find_all(rope: &Rope, q: &Query, more: &mut dyn FnMut(usize) -> bool) -> Option<Vec<Range<usize>>> {
    let len = rope.len_bytes();
    let mut found = Vec::new();
    let (mut pos, mut next) = (0, 0);
    while pos < len {
        let end = floor_char(rope, (pos + window()).min(len)).max(floor_char(rope, pos + 4).min(len));
        let stop = floor_char(rope, (end + q.max_len).min(len));
        let t = text(rope, pos, stop);
        let mut from = next.max(pos) - pos;
        while let Some(r) = find(&t, &q.needle, q.case, from) {
            if pos + r.start >= end {
                break;
            }
            from = r.end;
            next = pos + r.end;
            found.push(pos + r.start..pos + r.end);
        }
        pos = end;
        if !more(pos) {
            return None;
        }
    }
    Some(found)
}

/// Rounds a byte offset down to a char boundary, so a stale offset can't panic.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Case-insensitive prefix match; returns the matched byte length in `h`.
fn match_at(h: &str, needle_lower: &[char]) -> Option<usize> {
    let mut need = needle_lower.iter().peekable();
    for (i, ch) in h.char_indices() {
        for lc in ch.to_lowercase() {
            if need.next() != Some(&lc) {
                return None;
            }
        }
        if need.peek().is_none() {
            return Some(i + ch.len_utf8());
        }
    }
    None
}

/// Recognised link prefixes. `file://` is deliberately absent: a text file
/// shouldn't be able to make Ctrl+click open or run a local program.
const PREFIXES: [&str; 5] = ["https://", "http://", "ftp://", "mailto:", "www."];

/// Invisible formatting characters (zero-width and bidi controls). A link
/// ends at one, so text can't hide or visually reorder part of a link target.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{00AD}')
}

/// Byte ranges of URLs in a line of text.
pub fn find_urls(line: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] >= 0x80 || b[i - 1] == b'.');
        let prefix = if boundary && b[i].is_ascii_alphabetic() {
            PREFIXES.iter().find(|p| b.len() - i > p.len() && b[i..i + p.len()].eq_ignore_ascii_case(p.as_bytes()))
        } else {
            None
        };
        let Some(prefix) = prefix else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = start + prefix.len();
        for (off, c) in line[end..].char_indices() {
            if c.is_whitespace() || c.is_control() || is_invisible(c) || "<>\"`{}|\\^".contains(c) {
                break;
            }
            end = start + prefix.len() + off + c.len_utf8();
        }
        // Trim trailing punctuation, keeping a ')' that closes a '(' inside the URL.
        while end > start + prefix.len() {
            let c = line[..end].chars().next_back().unwrap();
            let trim = match c {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | ']' | '}' => true,
                ')' => line[start..end].matches('(').count() < line[start..end].matches(')').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= c.len_utf8();
        }
        if end > start + prefix.len() {
            out.push(start..end);
        }
        i = end.max(i + 1);
    }
    out
}

/// Turns detected text into something the OS can open.
pub fn url_target(text: &str) -> String {
    if text.as_bytes().get(..4).is_some_and(|b| b.eq_ignore_ascii_case(b"www.")) {
        format!("http://{text}")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(s: &str) -> Vec<&str> {
        find_urls(s).into_iter().map(|r| &s[r]).collect()
    }

    #[test]
    fn detects_urls() {
        assert_eq!(urls("see https://example.com/a?b=c."), ["https://example.com/a?b=c"]);
        assert_eq!(
            urls("(https://en.wikipedia.org/wiki/Rust_(language))"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(urls("go to www.rust-lang.org, or mailto:a@b.c"), ["www.rust-lang.org", "mailto:a@b.c"]);
        assert_eq!(urls("nohttp://x xhttps://y http://"), Vec::<&str>::new());
        assert_eq!(urls("مرحبا https://مثال.com/ص ok"), ["https://مثال.com/ص"]);
        assert_eq!(urls("<http://a.b/c>"), ["http://a.b/c"]);
        // Local file links are not links.
        assert_eq!(urls("file:///etc/passwd"), Vec::<&str>::new());
        // Invisible bidi/zero-width characters end a link.
        assert_eq!(urls("https://good.com\u{202E}moc.live"), ["https://good.com"]);
        assert_eq!(urls("https://a.com\u{200B}.evil"), ["https://a.com"]);
    }

    #[test]
    fn finds_text() {
        assert_eq!(find("Hello hello", "hello", true, 0), Some(6..11));
        assert_eq!(find("Hello hello", "hello", false, 0), Some(0..5));
        assert_eq!(find("Hello hello", "hello", false, 1), Some(6..11));
        assert_eq!(rfind("Hello hello", "HELLO", false, 11), Some(6..11));
        assert_eq!(rfind("Hello hello", "hello", false, 10), Some(0..5));
        assert_eq!(find("ÄBC äbc", "äbc", false, 2), Some(5..9));
        assert_eq!(find("abc", "", false, 0), None);
        // Offsets inside a multi-byte char are rounded down instead of panicking.
        assert_eq!(find("éa", "a", true, 1), Some(2..3));
        assert_eq!(rfind("éa", "é", true, 1), None);
    }

    /// Every match by brute force, per line (the old way), as absolute byte ranges.
    fn naive_all(text: &str, needle: &str, case: bool) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut base = 0;
        for line in text.split('\n') {
            let mut from = 0;
            while let Some(r) = find(line, needle, case, from) {
                from = r.end;
                out.push(base + r.start..base + r.end);
            }
            base += line.len() + 1;
        }
        out
    }

    fn sample() -> String {
        // Mixed scripts, multi-byte characters and case variants, across many 64-byte windows.
        let parts = ["fox ", "FOX ", "Fox\n", "ÄBC äbc ", "\u{212A}elvin ", "日本語 ", "x", "İstanbul ", "fo", "x\n"];
        (0..400).map(|i| parts[(i * 7 + i / 3) % parts.len()]).collect()
    }

    #[test]
    fn whole_document_search_matches_line_search() {
        TEST_WINDOW.with(|w| w.set(64));
        let text = sample();
        let rope = Rope::from_str(&text);
        let mut go = |_: usize| true;
        let cases =
            [("fox", false), ("fox", true), ("äbc", false), ("kelvin", false), ("日本", true), ("i\u{307}stan", false)];
        for (needle, case) in cases {
            let q = Query::new(needle, case);
            let want = naive_all(&text, needle, case);
            assert!(!want.is_empty(), "{needle}");
            assert_eq!(find_all(&rope, &q, &mut go).unwrap(), want, "{needle} {case}");
            // Find Next from each match lands on the next one, wrapping; Find Previous likewise.
            for (k, r) in want.iter().enumerate() {
                let next = &want[(k + 1) % want.len()];
                assert_eq!(find_wrapping(&rope, &q, r.end, true, &mut go).as_ref(), Some(next), "{needle} next");
                let prev = &want[(k + want.len() - 1) % want.len()];
                assert_eq!(find_wrapping(&rope, &q, r.start, false, &mut go).as_ref(), Some(prev), "{needle} prev");
            }
        }
        // A cancelled search reports nothing; a missing needle finds nothing.
        assert_eq!(find_all(&rope, &Query::new("fox", false), &mut |_| false), None);
        assert_eq!(find_wrapping(&rope, &Query::new("zzz", false), 10, true, &mut go), None);
        assert_eq!(find_wrapping(&rope, &Query::new("zzz", false), 10, false, &mut go), None);
    }
}
