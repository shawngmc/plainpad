//! Checks a link for tricks that make it look like a different address
//! (homograph and other social-engineering attacks), and works out the plain
//! ASCII address it really goes to.
//!
//! The host-name rules follow what browsers do, simplified from Unicode UTS #39
//! ("highly restrictive" profile) and the IDNA bidi rule:
//!
//! - a label may mix scripts only in the combinations used by Chinese, Japanese
//!   and Korean (with Latin); anything else, such as Latin with Cyrillic, is flagged
//! - a label written entirely in Cyrillic or Greek letters that look Latin, under
//!   a non-Cyrillic/Greek top-level domain, is flagged ("whole-script confusable")
//! - Latin lookalikes such as dotless ı or the IPA ɡ, combining marks, fullwidth
//!   forms, and characters that look like `/` or `.` are flagged
//! - a label mixing right-to-left and left-to-right letters is flagged
//! - `user@host` authorities and IP addresses written as plain or hex numbers are flagged
//!
//! Non-ASCII host labels are converted to punycode (`xn--…`), which is what the
//! browser is given and what the user is shown when a link is suspicious.

use crate::search::url_target;

#[derive(Debug, Clone, PartialEq)]
pub struct LinkInfo {
    /// The link as written (with `www.` links given an `http://` scheme).
    pub url: String,
    /// What to hand to the OS: the link with its host in ASCII (punycode) form.
    pub target: String,
    /// The host as written (lower-cased), for display.
    pub host: String,
    /// The host in ASCII form.
    pub ascii_host: String,
    /// Reasons this link may be disguised. Empty means it looks honest.
    pub warnings: Vec<String>,
}

impl LinkInfo {
    pub fn suspicious(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Whether a link may be disguised (see [`analyze`]). Drawing asks this for
/// every visible link on every frame, so answers are remembered per link text:
/// up to 256 of them, for links up to 512 bytes (about 128 KB at most).
pub fn is_suspicious(text: &str) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local!(static SEEN: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new()));
    if text.len() > 512 {
        return analyze(text).suspicious();
    }
    SEEN.with_borrow_mut(|seen| {
        if let Some(&bad) = seen.get(text) {
            return bad;
        }
        if seen.len() >= 256 {
            seen.clear();
        }
        let bad = analyze(text).suspicious();
        seen.insert(text.to_owned(), bad);
        bad
    })
}

pub fn analyze(text: &str) -> LinkInfo {
    let url = url_target(text);
    let lower = url.to_ascii_lowercase();
    let mut warnings = Vec::new();

    if lower.starts_with("mailto:") {
        // mailto:local@domain?query — the domain is what identifies the recipient.
        let rest = &url["mailto:".len()..];
        let addr_end = rest.find('?').unwrap_or(rest.len());
        let addr = &rest[..addr_end];
        let Some(at) = addr.rfind('@') else {
            return LinkInfo {
                url: url.clone(),
                target: url.clone(),
                host: String::new(),
                ascii_host: String::new(),
                warnings,
            };
        };
        let host = addr[at + 1..].to_lowercase();
        let ascii_host = check_host(&host, &mut warnings);
        let target = format!("mailto:{}@{}{}", &addr[..at], ascii_host, &rest[addr_end..]);
        return LinkInfo { url, target, host, ascii_host, warnings };
    }

    let Some(scheme_end) = url.find("://") else {
        return LinkInfo {
            url: url.clone(),
            target: url.clone(),
            host: String::new(),
            ascii_host: String::new(),
            warnings,
        };
    };
    let after = &url[scheme_end + 3..];
    // The authority ends at the first real '/', '?' or '#'. Lookalike slashes
    // don't end it, so they stay in the host and get flagged below.
    let auth_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..auth_end];
    let (userinfo, host_port) = match authority.rfind('@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
        None => (None, authority),
    };
    let (host, port) = split_port(host_port);
    let host = host.to_lowercase();
    if let Some(u) = userinfo.filter(|u| !u.is_empty()) {
        warnings.push(format!("The text before \"@\" (\"{u}\") is not the site. This link goes to {host}."));
    }
    let ascii_host = check_host(&host, &mut warnings);
    let mut target = String::with_capacity(url.len() + 16);
    target.push_str(&url[..scheme_end + 3]);
    if let Some(u) = userinfo {
        target.push_str(u);
        target.push('@');
    }
    target.push_str(&ascii_host);
    target.push_str(port);
    target.push_str(&after[auth_end..]);
    LinkInfo { url, target, host, ascii_host, warnings }
}

/// Splits `host:port`, leaving IPv6 literals (`[::1]:80`) intact.
fn split_port(hp: &str) -> (&str, &str) {
    if let Some(i) = hp.rfind(':')
        && hp[i + 1..].bytes().all(|b| b.is_ascii_digit())
        && !hp[..i].contains(':')
    {
        return (&hp[..i], &hp[i..]);
    }
    (hp, "")
}

/// Characters that look like a slash, dot, colon or other URL syntax but aren't.
fn is_syntax_lookalike(c: char) -> bool {
    matches!(
        c,
        '\u{2044}' | '\u{2215}' | '\u{29F8}' | '\u{FF0F}' | '\u{0338}' | '\u{2571}' // slashes
        | '\u{3002}' | '\u{FF0E}' | '\u{FF61}' | '\u{2024}' | '\u{0701}' | '\u{0702}' | '\u{A60E}' | '\u{10A50}' // dots
        | '\u{0589}' | '\u{05C3}' | '\u{2236}' | '\u{A789}' | '\u{FE13}' | '\u{FF1A}' // colons
        | '\u{FF20}' | '\u{FE6B}' // at signs
        | '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{30FC}' // dashes, minus, katakana long vowel
    )
}

/// Checks a host name, adding warnings, and returns its ASCII form.
fn check_host(host: &str, warnings: &mut Vec<String>) -> String {
    if host.is_empty() {
        return String::new();
    }
    if host.starts_with('[') {
        return host.to_string(); // IPv6 literal
    }
    if let Some(c) = host.chars().find(|&c| is_syntax_lookalike(c)) {
        warnings.push(format!(
            "The address contains \"{c}\" (U+{:04X}), which looks like URL punctuation but isn't. The real site is the whole name \"{host}\".",
            c as u32
        ));
    }
    if host.chars().any(|c| ('\u{FF01}'..='\u{FF5E}').contains(&c)) {
        warnings.push("The address uses fullwidth letters that imitate ordinary ones.".into());
    }
    if host.contains('%') {
        warnings.push("The site name contains percent-encoded characters.".into());
    }
    if is_obfuscated_ip(host) {
        warnings.push(format!("\"{host}\" is an IP address written in an unusual form, which can hide where it goes."));
    }
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last().copied().unwrap_or("");
    let mut ascii = Vec::with_capacity(labels.len());
    for label in &labels {
        if label.is_ascii() {
            ascii.push(label.to_string());
            continue;
        }
        if let Some(w) = check_label(label, tld) {
            warnings.push(w);
        }
        ascii.push(punycode(label).map_or_else(|| label.to_string(), |p| format!("xn--{p}")));
    }
    ascii.join(".")
}

/// IP addresses written as one decimal/hex/octal number, or with hex/octal parts.
fn is_obfuscated_ip(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    let numeric = |p: &str| {
        !p.is_empty()
            && (p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 2
                    && p.as_bytes()[..2].eq_ignore_ascii_case(b"0x")
                    && p.as_bytes()[2..].iter().all(|b| b.is_ascii_hexdigit())))
    };
    if !parts.iter().all(|p| numeric(p)) {
        return false;
    }
    // A normal dotted quad: four plain decimal parts, no leading zeros.
    let normal = parts.len() == 4
        && parts.iter().all(|p| {
            !p.starts_with("0x") && (p == &"0" || !p.starts_with('0')) && p.parse::<u16>().is_ok_and(|v| v <= 255)
        });
    !normal
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Script {
    Common,
    Inherited,
    Latin,
    Greek,
    Cyrillic,
    Armenian,
    Hebrew,
    Arabic,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Bopomofo,
    /// Some other script, identified by its 128-code-point block.
    Other(u32),
}

fn script(c: char) -> Script {
    use Script::*;
    let u = c as u32;
    match u {
        0x30..=0x39 | 0x2D | 0x5F => Common,
        0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x250..=0x2AF | 0x1D00..=0x1DBF | 0x1E00..=0x1EFF => Latin,
        0x2C60..=0x2C7F | 0xA720..=0xA7FF | 0xAB30..=0xAB6F => Latin,
        0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F => Inherited,
        0x370..=0x3FF | 0x1F00..=0x1FFF => Greek,
        0x400..=0x52F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Cyrillic,
        0x530..=0x58F => Armenian,
        0x590..=0x5FF | 0xFB1D..=0xFB4F => Hebrew,
        0x600..=0x6FF | 0x750..=0x77F | 0x8A0..=0x8FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Arabic,
        0x3005..=0x3007 | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF => Han,
        0x3040..=0x309F => Hiragana,
        0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Katakana,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Hangul,
        0x3100..=0x312F | 0x31A0..=0x31BF => Bopomofo,
        _ => Other(u >> 7),
    }
}

fn is_rtl(s: Script) -> bool {
    matches!(s, Script::Hebrew | Script::Arabic)
}

/// Latin-script letters that imitate plain ASCII letters.
const LATIN_LOOKALIKES: &[char] = &[
    'ı', 'ȷ', 'ɑ', 'ɡ', 'ɢ', 'ɩ', 'ɪ', 'ɴ', 'ɵ', 'ʀ', 'ʏ', 'ᴀ', 'ᴄ', 'ᴅ', 'ᴇ', 'ᴋ', 'ᴍ', 'ᴏ', 'ᴘ', 'ᴛ', 'ᴜ', 'ᴠ', 'ᴡ',
    'ᴢ', 'ꞵ', 'ſ', 'ƅ', 'ǀ',
];
/// Cyrillic and Greek letters that look like Latin ones (after lower-casing).
const CYRILLIC_LOOKALIKES: &str = "аысԁеԍһіюјӏорԗԛѕԝхуъьҽпгѵѡвкмнтӌԃ";
const GREEK_LOOKALIKES: &str = "αβεικνορτυχωηζ";
/// Top-level domains where an all-Cyrillic or all-Greek name is expected.
const CYRILLIC_TLDS: &[&str] = &[
    "ru",
    "su",
    "ua",
    "by",
    "bg",
    "kz",
    "kg",
    "mk",
    "rs",
    "mn",
    "tj",
    "uz",
    "рф",
    "укр",
    "бел",
    "срб",
    "мкд",
    "қаз",
    "мон",
    "бг",
    "рус",
    "москва",
    "орг",
    "ком",
    "сайт",
    "онлайн",
];
const GREEK_TLDS: &[&str] = &["gr", "cy", "ελ", "ευ"];

fn check_label(label: &str, tld: &str) -> Option<String> {
    let scripts: Vec<Script> = label.chars().map(script).collect();
    if let Some(c) = label.chars().find(|c| script(*c) == Script::Inherited) {
        return Some(format!("\"{label}\" contains a combining mark (U+{:04X}) that can disguise a letter.", c as u32));
    }
    if let Some(c) = label.chars().find(|c| LATIN_LOOKALIKES.contains(c)) {
        return Some(format!(
            "\"{label}\" contains \"{c}\" (U+{:04X}), which imitates a plain Latin letter.",
            c as u32
        ));
    }
    let mut used: Vec<Script> = Vec::new();
    for s in scripts.iter().copied().filter(|s| *s != Script::Common) {
        if !used.contains(&s) {
            used.push(s);
        }
    }
    let allowed_mix = |set: &[Script]| used.iter().all(|s| set.contains(s));
    use Script::*;
    let ok = used.len() <= 1
        || allowed_mix(&[Latin, Han, Hiragana, Katakana])
        || allowed_mix(&[Latin, Han, Bopomofo])
        || allowed_mix(&[Latin, Han, Hangul]);
    if !ok {
        let names: Vec<String> = used.iter().map(|s| script_name(*s)).collect();
        // Name the letters from the less-used script(s): they're the likely impostors,
        // and they look identical to the real thing, so quoting the label alone shows nothing.
        let count = |s: Script| scripts.iter().filter(|x| **x == s).count();
        let main = used.iter().copied().max_by_key(|s| count(*s)).unwrap_or(Latin);
        let mut odd: Vec<String> = Vec::new();
        for c in label.chars() {
            let sc = script(c);
            let desc = format!("'{c}' (U+{:04X}, {})", c as u32, script_name(sc));
            if sc != main && sc != Common && !odd.contains(&desc) && odd.len() < 3 {
                odd.push(desc);
            }
        }
        return Some(format!(
            "\"{label}\" mixes {} letters: {} among {} ones. Lookalike letters from another alphabet are a common way to imitate a real site.",
            names.join(" and "),
            odd.join(", "),
            script_name(main)
        ));
    }
    if used.iter().any(|s| is_rtl(*s)) && label.chars().any(|c| script(c) == Latin) {
        return Some(format!(
            "\"{label}\" mixes right-to-left and left-to-right letters, which can reorder how it displays."
        ));
    }
    // Whole-script confusables: every letter is a Cyrillic (or Greek) lookalike of a Latin letter.
    let letters = || label.chars().filter(|c| script(*c) != Common);
    for (s, set, tlds) in [(Cyrillic, CYRILLIC_LOOKALIKES, CYRILLIC_TLDS), (Greek, GREEK_LOOKALIKES, GREEK_TLDS)] {
        if used == [s] && letters().all(|c| set.contains(c)) && !tlds.contains(&tld) {
            return Some(format!(
                "\"{label}\" is written entirely in {} letters that look like Latin ones, so it can pass for a different name.",
                script_name(s)
            ));
        }
    }
    None
}

fn script_name(s: Script) -> String {
    match s {
        Script::Latin => "Latin".into(),
        Script::Greek => "Greek".into(),
        Script::Cyrillic => "Cyrillic".into(),
        Script::Armenian => "Armenian".into(),
        Script::Hebrew => "Hebrew".into(),
        Script::Arabic => "Arabic".into(),
        Script::Han => "Chinese".into(),
        Script::Hiragana | Script::Katakana => "Japanese".into(),
        Script::Hangul => "Korean".into(),
        Script::Bopomofo => "Bopomofo".into(),
        Script::Common | Script::Inherited => "common".into(),
        Script::Other(b) => format!("U+{:04X}-block", b << 7),
    }
}

/// Punycode encoding (RFC 3492) of one label, without the `xn--` prefix.
fn punycode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    fn adapt(mut delta: u32, points: u32, first: bool) -> u32 {
        delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + SKEW)
    }
    fn digit(d: u32) -> char {
        if d < 26 { (b'a' + d as u8) as char } else { (b'0' + (d - 26) as u8) as char }
    }
    let cps: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut out: String = input.chars().filter(char::is_ascii).collect();
    let basic = out.len() as u32;
    let mut handled = basic;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (handled as usize) < cps.len() {
        let m = cps.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;
        for &c in &cps {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warned(url: &str) -> bool {
        analyze(url).suspicious()
    }

    #[test]
    fn remembered_answers_match() {
        let long = format!("https://example.com/{}", "a".repeat(600));
        for _ in 0..2 {
            for url in ["https://example.com/", "https://аpple.com/", "https://google.com@evil.example/", &long] {
                assert_eq!(is_suspicious(url), warned(url), "{url}");
            }
        }
        // Many different links: the remembered answers stay bounded, and still right.
        for i in 0..1000 {
            assert!(!is_suspicious(&format!("https://site{i}.example/")));
        }
        assert!(is_suspicious("https://аpple.com/"));
    }

    #[test]
    fn punycode_vectors() {
        assert_eq!(punycode("bücher").as_deref(), Some("bcher-kva"));
        assert_eq!(punycode("münchen").as_deref(), Some("mnchen-3ya"));
        assert_eq!(punycode("аpple").as_deref(), Some("pple-43d"));
        assert_eq!(punycode("中国").as_deref(), Some("fiqs8s"));
        assert_eq!(punycode("παράδειγμα").as_deref(), Some("hxajbheg2az3al"));
    }

    #[test]
    fn honest_links_pass() {
        for u in [
            "https://example.com/path?q=1#x",
            "www.rust-lang.org",
            "https://bücher.de/",
            "https://中国.cn",
            "https://例え.テスト/",
            "https://한국.kr",
            "https://пример.рф",
            "https://παράδειγμα.ελ",
            "https://example.com/ünïcode/路径",
            "http://192.168.1.10:8080/",
            "http://[::1]:80/",
            "mailto:someone@example.com?subject=hi",
        ] {
            assert!(!warned(u), "{u}: {:?}", analyze(u).warnings);
        }
        assert_eq!(analyze("https://bücher.de/x").target, "https://xn--bcher-kva.de/x");
    }

    #[test]
    fn homographs_are_flagged() {
        // Cyrillic 'а' mixed into Latin.
        let i = analyze("https://аpple.com/login");
        assert!(i.suspicious());
        assert!(i.warnings[0].contains("U+0430, Cyrillic"), "{:?}", i.warnings);
        assert_eq!(i.ascii_host, "xn--pple-43d.com");
        assert_eq!(i.target, "https://xn--pple-43d.com/login");
        // All-Cyrillic lookalike under .com, but fine under .ru.
        assert!(warned("https://аррӏе.com"));
        assert!(!warned("https://аррӏе.ru"));
        // Greek lookalikes.
        assert!(warned("https://ορεnai.com"));
        // Latin lookalikes: IPA g, dotless i.
        assert!(warned("https://ɡoogle.com"));
        assert!(warned("https://lınkedin.com"));
        // Combining mark over a letter.
        assert!(warned("https://g\u{0300}oogle.com"));
        // Fullwidth letters.
        assert!(warned("https://ｇｏｏｇｌｅ.com"));
        // Mixed directions.
        assert!(warned("https://paypalשלום.com"));
        // In a mailto: domain.
        assert!(warned("mailto:ceo@exаmple.com"));
    }

    #[test]
    fn structural_tricks_are_flagged() {
        let i = analyze("https://accounts.google.com@evil.example/login");
        assert!(i.suspicious());
        assert_eq!(i.host, "evil.example");
        // Lookalike slash keeps everything in the host.
        let i = analyze("https://paypal.com\u{2044}signin.evil.example/");
        assert!(i.suspicious());
        assert!(i.host.ends_with("evil.example"));
        // Ideographic full stop as a dot.
        assert!(warned("https://google\u{3002}com.evil.example"));
        // Numeric IP tricks.
        assert!(warned("http://3627734734/"));
        assert!(warned("http://0xd8.0x3a.0xd6.0xce/"));
        assert!(warned("http://0330.072.0326.0316/"));
        assert!(warned("https://g%6Fogle.com"));
    }

    /// Hostile input must never panic: build many links from tricky pieces.
    #[test]
    fn never_panics() {
        let pieces = [
            "https://",
            "http://",
            "mailto:",
            "www.",
            "@",
            ":",
            "::",
            "[",
            "]",
            ".",
            "..",
            "/",
            "?",
            "#",
            "%",
            "0x",
            "0",
            "255",
            "99999",
            "a",
            "é",
            "ü",
            "ı",
            "а",
            "ο",
            "ש",
            "ال",
            "中",
            "テ",
            "한",
            "\u{300}",
            "\u{2044}",
            "\u{3002}",
            "ｇ",
            "\u{1F600}",
            "-",
            "xn--",
            "\u{10FFFF}",
            "ß",
            "İ",
        ];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            let mut s = String::new();
            let n = (seed % 9) as usize + 1;
            for _ in 0..n {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(pieces[(seed % pieces.len() as u64) as usize]);
            }
            let info = analyze(&s);
            assert!(
                info.ascii_host.is_ascii()
                    || info.ascii_host.starts_with('[')
                    || info.suspicious()
                    || !info.host.is_ascii()
            );
        }
    }
}
