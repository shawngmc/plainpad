//! File loading/saving, text encodings, line endings and persisted settings.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 with BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Windows1252 => "Windows-1252",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::Cr => "\r",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::Cr => "CR",
        }
    }
    pub fn platform() -> Eol {
        if cfg!(windows) { Eol::CrLf } else { Eol::Lf }
    }
}

pub struct Loaded {
    /// Text with all line endings normalised to `\n`.
    pub text: String,
    pub encoding: Encoding,
    pub eol: Eol,
    pub mixed_eol: bool,
}

// Windows-1252 bytes 0x80..=0x9F. Undefined slots map to the C1 control of
// the same value so that decoding and re-encoding round-trips every byte.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

pub fn decode(bytes: &[u8]) -> (String, Encoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(rest).into_owned(), Encoding::Utf8Bom);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (utf16(rest, u16::from_le_bytes), Encoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (utf16(rest, u16::from_be_bytes), Encoding::Utf16Be);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), Encoding::Utf8),
        Err(_) => {
            let s = bytes
                .iter()
                .map(|&b| match b {
                    0x80..=0x9F => char::from_u32(CP1252_HIGH[(b - 0x80) as usize] as u32).unwrap(),
                    _ => b as char,
                })
                .collect();
            (s, Encoding::Windows1252)
        }
    }
}

fn utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| f([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Encodes text. Fails (returning the first offending char) if the encoding
/// cannot represent it.
pub fn encode(text: &str, enc: Encoding) -> Result<Vec<u8>, char> {
    Ok(match enc {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let le = enc == Encoding::Utf16Le;
            let mut v = Vec::with_capacity(text.len() * 2 + 2);
            v.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            for u in text.encode_utf16() {
                v.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
            v
        }
        Encoding::Windows1252 => {
            let mut v = Vec::with_capacity(text.len());
            for c in text.chars() {
                let cp = c as u32;
                let b = if cp < 0x80 || (0xA0..=0xFF).contains(&cp) {
                    cp as u8
                } else if let Some(i) = CP1252_HIGH.iter().position(|&u| u as u32 == cp) {
                    0x80 + i as u8
                } else {
                    return Err(c);
                };
                v.push(b);
            }
            v
        }
    })
}

/// Returns the dominant line ending and whether the text mixes several kinds.
pub fn detect_eol(text: &str) -> (Eol, bool) {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    let kinds = (crlf > 0) as u8 + (lf > 0) as u8 + (cr > 0) as u8;
    let eol = if crlf == 0 && lf == 0 && cr == 0 {
        Eol::platform()
    } else if crlf >= lf && crlf >= cr {
        Eol::CrLf
    } else if lf >= cr {
        Eol::Lf
    } else {
        Eol::Cr
    };
    (eol, kinds > 1)
}

pub fn normalize_eol(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn load(path: &Path) -> io::Result<Loaded> {
    let bytes = fs::read(path)?;
    let (text, encoding) = decode(&bytes);
    drop(bytes);
    let (eol, mixed_eol) = detect_eol(&text);
    Ok(Loaded { text: normalize_eol(text), encoding, eol, mixed_eol })
}

/// Writes via a temporary file + rename so a failed save never truncates the
/// original. Symlinks are resolved so the link itself is preserved.
pub fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.plainpad-{}.tmp", std::process::id()));
    let result = (|| {
        fs::write(&tmp, bytes)?;
        if let Ok(meta) = fs::metadata(&target) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        // Fall back to an in-place write (e.g. directory not writable).
        return fs::write(&target, bytes);
    }
    result
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Empty means the system monospace font.
    pub font_family: String,
    pub font_pt: f32,
    pub wrap: bool,
    pub status_bar: bool,
    pub win_w: u32,
    pub win_h: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { font_family: String::new(), font_pt: 11.0, wrap: true, status_bar: true, win_w: 900, win_h: 640 }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("plainpad").join("settings.conf"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(text) = config_path().and_then(|p| fs::read_to_string(p).ok()) else {
            return s;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "font_family" => s.font_family = v.to_owned(),
                "font_pt" => s.font_pt = v.parse().unwrap_or(s.font_pt),
                "wrap" => s.wrap = v == "true",
                "status_bar" => s.status_bar = v == "true",
                "win_w" => s.win_w = v.parse().unwrap_or(s.win_w),
                "win_h" => s.win_h = v.parse().unwrap_or(s.win_h),
                _ => {}
            }
        }
        s.font_pt = s.font_pt.clamp(4.0, 200.0);
        s
    }

    pub fn save(&self) {
        let Some(p) = config_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(
            p,
            format!(
                "font_family={}\nfont_pt={}\nwrap={}\nstatus_bar={}\nwin_w={}\nwin_h={}\n",
                self.font_family, self.font_pt, self.wrap, self.status_bar, self.win_w, self.win_h
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_detection() {
        assert_eq!(detect_eol("a\r\nb\r\nc"), (Eol::CrLf, false));
        assert_eq!(detect_eol("a\nb\n"), (Eol::Lf, false));
        assert_eq!(detect_eol("a\rb\r"), (Eol::Cr, false));
        assert_eq!(detect_eol("a\r\nb\nc\n"), (Eol::Lf, true));
        assert_eq!(normalize_eol("a\r\nb\rc\n".into()), "a\nb\nc\n");
    }

    #[test]
    fn encodings_roundtrip() {
        let s = "héllo – €";
        for enc in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Windows1252] {
            let bytes = encode(s, enc).unwrap();
            let (back, detected) = decode(&bytes);
            assert_eq!(back, s, "{enc:?}");
            assert_eq!(detected, enc);
        }
        assert_eq!(encode("日本", Encoding::Windows1252), Err('日'));
        // Every single byte survives a cp1252 round-trip.
        let all: Vec<u8> = (0x80..=0xFF).collect();
        let (s, enc) = decode(&all);
        assert_eq!(enc, Encoding::Windows1252);
        assert_eq!(encode(&s, enc).unwrap(), all);
    }
}
