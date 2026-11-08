//! Plain-text search and URL detection within a single line.

use std::ops::Range;

/// Finds the first match of `needle` in `hay` starting at byte `from`.
pub fn find(hay: &str, needle: &str, case: bool, from: usize) -> Option<Range<usize>> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    if case {
        return hay[from..].find(needle).map(|i| from + i..from + i + needle.len());
    }
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    hay[from..]
        .char_indices()
        .find_map(|(i, _)| match_at(&hay[from + i..], &n).map(|len| from + i..from + i + len))
}

/// Finds the last match of `needle` in `hay` that ends at or before byte `before`.
pub fn rfind(hay: &str, needle: &str, case: bool, before: usize) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let before = before.min(hay.len());
    if case {
        return hay[..before].rfind(needle).map(|i| i..i + needle.len());
    }
    let n: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    hay[..before]
        .char_indices()
        .rev()
        .find_map(|(i, _)| match_at(&hay[i..before], &n).map(|len| i..i + len))
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

const PREFIXES: [&str; 6] = ["https://", "http://", "ftp://", "file://", "mailto:", "www."];

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
            if c.is_whitespace() || c.is_control() || "<>\"`{}|\\^".contains(c) {
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
    if text.len() >= 4 && text[..4].eq_ignore_ascii_case("www.") {
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
        assert_eq!(urls("(https://en.wikipedia.org/wiki/Rust_(language))"), ["https://en.wikipedia.org/wiki/Rust_(language)"]);
        assert_eq!(urls("go to www.rust-lang.org, or mailto:a@b.c"), ["www.rust-lang.org", "mailto:a@b.c"]);
        assert_eq!(urls("nohttp://x xhttps://y http://"), Vec::<&str>::new());
        assert_eq!(urls("مرحبا https://مثال.com/ص ok"), ["https://مثال.com/ص"]);
        assert_eq!(urls("<http://a.b/c>"), ["http://a.b/c"]);
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
    }
}
