//! File loading/saving, text encodings, line endings and persisted settings.

use ropey::{Rope, RopeBuilder};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
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
    pub rope: Rope,
    pub encoding: Encoding,
    pub eol: Eol,
    pub mixed_eol: bool,
}

// Windows-1252 bytes 0x80..=0x9F. Undefined slots map to the C1 control of
// the same value so that decoding and re-encoding round-trips every byte.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D,
    0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
    0x0153, 0x009D, 0x017E, 0x0178,
];

fn cp1252(b: u8) -> char {
    match b {
        0x80..=0x9F => char::from_u32(CP1252_HIGH[(b - 0x80) as usize] as u32).unwrap(),
        _ => b as char,
    }
}

/// Encodes text. Fails (returning the first offending char) if the encoding
/// cannot represent it.
/// The byte order mark written at the start of a file in this encoding.
fn bom(enc: Encoding) -> &'static [u8] {
    match enc {
        Encoding::Utf8Bom => &[0xEF, 0xBB, 0xBF],
        Encoding::Utf16Le => &[0xFF, 0xFE],
        Encoding::Utf16Be => &[0xFE, 0xFF],
        Encoding::Utf8 | Encoding::Windows1252 => &[],
    }
}

fn to_cp1252(c: char) -> Option<u8> {
    let cp = c as u32;
    if cp < 0x80 || (0xA0..=0xFF).contains(&cp) {
        Some(cp as u8)
    } else {
        CP1252_HIGH.iter().position(|&u| u as u32 == cp).map(|i| 0x80 + i as u8)
    }
}

/// Appends `text` in encoding `enc` (without a BOM), or returns the first
/// character the encoding can't represent.
fn encode_into(out: &mut Vec<u8>, text: &str, enc: Encoding) -> Result<(), char> {
    match enc {
        Encoding::Utf8 | Encoding::Utf8Bom => out.extend_from_slice(text.as_bytes()),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let le = enc == Encoding::Utf16Le;
            out.reserve(text.len() * 2);
            for u in text.encode_utf16() {
                out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
        }
        Encoding::Windows1252 => {
            out.reserve(text.len());
            for c in text.chars() {
                out.push(to_cp1252(c).ok_or(c)?);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub fn encode(text: &str, enc: Encoding) -> Result<Vec<u8>, char> {
    let mut v = bom(enc).to_vec();
    encode_into(&mut v, text, enc)?;
    Ok(v)
}

/// The first character in `rope` that `enc` can't store, if any.
pub fn unencodable(rope: &Rope, enc: Encoding) -> Option<char> {
    match enc {
        Encoding::Windows1252 => rope.chunks().flat_map(str::chars).find(|&c| to_cp1252(c).is_none()),
        _ => None,
    }
}

/// Writes `rope` with `\n` turned into `eol`, encoded as `enc` (which must be
/// able to store every character; see [`unencodable`]). Reports progress in bytes of text.
pub fn write_text(w: &mut dyn Write, rope: &Rope, eol: Eol, enc: Encoding, progress: &dyn Fn(u64)) -> io::Result<()> {
    w.write_all(bom(enc))?;
    let eol = eol.as_str();
    let mut out = Vec::new();
    let mut done = 0u64;
    for chunk in rope.chunks() {
        let replaced;
        let text = if eol == "\n" || !chunk.contains('\n') {
            chunk
        } else {
            replaced = chunk.replace('\n', eol);
            &replaced
        };
        if matches!(enc, Encoding::Utf8 | Encoding::Utf8Bom) {
            w.write_all(text.as_bytes())?;
        } else {
            out.clear();
            encode_into(&mut out, text, enc).map_err(|c| io::Error::other(format!("'{c}' can't be encoded")))?;
            w.write_all(&out)?;
        }
        done += chunk.len() as u64;
        progress(done);
    }
    Ok(())
}

/// What happened when a document was saved.
pub struct SaveOutcome {
    pub result: io::Result<()>,
    /// The encoding actually used.
    pub encoding: Encoding,
    /// A character the requested encoding couldn't store, so UTF-8 was used instead.
    pub fell_back: Option<char>,
}

/// Saves a document's text to `path` (see [`save_with`]).
pub fn save_rope(path: &Path, rope: &Rope, eol: Eol, enc: Encoding, progress: &dyn Fn(u64)) -> SaveOutcome {
    let fell_back = unencodable(rope, enc);
    let encoding = if fell_back.is_some() { Encoding::Utf8 } else { enc };
    let result = save_with(path, &mut |w| write_text(w, rope, eol, encoding, progress));
    SaveOutcome { result, encoding, fell_back }
}

/// Receives decoded text in pieces, counts and normalises line endings
/// (a `\r` at the end of one piece may pair with a `\n` at the start of the
/// next), and appends the result to a rope.
struct Sink {
    rope: RopeBuilder,
    crlf: usize,
    lf: usize,
    cr: usize,
    pending_cr: bool,
}

impl Sink {
    fn new() -> Self {
        Self { rope: RopeBuilder::new(), crlf: 0, lf: 0, cr: 0, pending_cr: false }
    }

    fn plain(&mut self, s: &str) {
        self.lf += s.bytes().filter(|&b| b == b'\n').count();
        self.rope.append(s);
    }

    fn push(&mut self, mut s: &str) {
        if s.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_cr) {
            if let Some(rest) = s.strip_prefix('\n') {
                self.crlf += 1;
                s = rest;
            } else {
                self.cr += 1;
            }
            self.rope.append("\n");
        }
        while let Some(i) = s.find('\r') {
            self.plain(&s[..i]);
            s = &s[i + 1..];
            if s.is_empty() {
                self.pending_cr = true;
                return;
            }
            if let Some(rest) = s.strip_prefix('\n') {
                self.crlf += 1;
                s = rest;
            } else {
                self.cr += 1;
            }
            self.rope.append("\n");
        }
        self.plain(s);
    }

    fn finish(mut self, encoding: Encoding) -> Loaded {
        if self.pending_cr {
            self.cr += 1;
            self.rope.append("\n");
        }
        let (crlf, lf, cr) = (self.crlf, self.lf, self.cr);
        let eol = if crlf == 0 && lf == 0 && cr == 0 {
            Eol::platform()
        } else if crlf >= lf && crlf >= cr {
            Eol::CrLf
        } else if lf >= cr {
            Eol::Lf
        } else {
            Eol::Cr
        };
        let kinds = (crlf > 0) as u8 + (lf > 0) as u8 + (cr > 0) as u8;
        Loaded { rope: self.rope.finish(), encoding, eol, mixed_eol: kinds > 1 }
    }
}

const CHUNK: usize = 64 * 1024;

/// Streams UTF-8. Returns false on invalid input unless `lossy` (then U+FFFD is substituted).
fn utf8(r: &mut impl Read, sink: &mut Sink, lossy: bool) -> io::Result<bool> {
    let mut buf = vec![0u8; CHUNK];
    let mut carry: Vec<u8> = Vec::with_capacity(CHUNK + 4);
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let mut start = 0;
        loop {
            match std::str::from_utf8(&carry[start..]) {
                Ok(s) => {
                    sink.push(s);
                    start = carry.len();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    sink.push(std::str::from_utf8(&carry[start..start + valid]).unwrap());
                    start += valid;
                    match e.error_len() {
                        None => break, // incomplete sequence at the end: wait for more bytes
                        Some(len) if lossy => {
                            sink.push("\u{FFFD}");
                            start += len;
                        }
                        Some(_) => return Ok(false),
                    }
                }
            }
        }
        carry.drain(..start);
    }
    if !carry.is_empty() {
        if !lossy {
            return Ok(false);
        }
        sink.push("\u{FFFD}");
    }
    Ok(true)
}

fn utf16(r: &mut impl Read, sink: &mut Sink, le: bool) -> io::Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    let mut units: Vec<u16> = Vec::new();
    let mut text = String::new();
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let even = carry.len() & !1;
        units.extend(
            carry[..even]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| if le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) }),
        );
        carry.drain(..even);
        // Keep a trailing high surrogate for the next chunk.
        let held = units.last().filter(|u| (0xD800..0xDC00).contains(*u)).copied();
        if held.is_some() {
            units.pop();
        }
        text.clear();
        text.extend(char::decode_utf16(units.drain(..)).map(|c| c.unwrap_or('\u{FFFD}')));
        sink.push(&text);
        units.extend(held);
    }
    if !units.is_empty() {
        sink.push("\u{FFFD}");
    }
    Ok(())
}

fn windows1252(r: &mut impl Read, sink: &mut Sink) -> io::Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut text = String::with_capacity(CHUNK * 2);
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        text.clear();
        text.extend(buf[..n].iter().map(|&b| cp1252(b)));
        sink.push(&text);
    }
}

/// Decodes a byte stream straight into a rope. Needs `Seek` to restart as
/// Windows-1252 if the text turns out not to be valid UTF-8.
pub fn load_from<R: Read + Seek>(mut r: R) -> io::Result<Loaded> {
    let mut bom = [0u8; 3];
    let mut got = 0;
    while got < 3 {
        match r.read(&mut bom[got..])? {
            0 => break,
            n => got += n,
        }
    }
    let bom = &bom[..got];
    let mut sink = Sink::new();
    let (skip, enc) = if bom.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (3, Encoding::Utf8Bom)
    } else if bom.starts_with(&[0xFF, 0xFE]) {
        (2, Encoding::Utf16Le)
    } else if bom.starts_with(&[0xFE, 0xFF]) {
        (2, Encoding::Utf16Be)
    } else {
        (0, Encoding::Utf8)
    };
    r.seek(SeekFrom::Start(skip))?;
    match enc {
        Encoding::Utf16Le | Encoding::Utf16Be => utf16(&mut r, &mut sink, enc == Encoding::Utf16Le)?,
        Encoding::Utf8Bom => {
            utf8(&mut r, &mut sink, true)?;
        }
        _ => {
            if !utf8(&mut r, &mut sink, false)? {
                r.seek(SeekFrom::Start(0))?;
                sink = Sink::new();
                windows1252(&mut r, &mut sink)?;
                return Ok(sink.finish(Encoding::Windows1252));
            }
        }
    }
    Ok(sink.finish(enc))
}

pub fn load(path: &Path) -> io::Result<Loaded> {
    load_from(open_regular(path)?)
}

/// Opens a file for reading, refusing anything but a regular file: opening a
/// FIFO would block forever, and a device such as /dev/zero never ends.
pub fn open_regular(path: &Path) -> io::Result<fs::File> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    fs::File::open(path)
}

/// A hard-to-guess number for temporary file names. Not cryptographic; the
/// exclusive create in [`create_private`] is what makes temp files safe.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one((std::process::id(), std::time::SystemTime::now()))
}

/// Creates a brand-new file in `dir` named `{prefix}{random}{suffix}`.
/// It fails rather than opening an existing file or following a symlink
/// someone planted (O_CREAT|O_EXCL), and on Unix only the owner can read it.
pub fn create_private(dir: &Path, prefix: &str, suffix: &str) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let path = dir.join(format!("{prefix}{:016x}{suffix}", random_u64()));
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not create a temporary file"))
}

/// Produces a file's contents.
type Writer<'a> = dyn FnMut(&mut dyn Write) -> io::Result<()> + 'a;

/// Runs `write` into `file` through a buffer, then flushes it all to disk.
fn fill(file: &mut fs::File, write: &mut Writer) -> io::Result<()> {
    let mut w = io::BufWriter::with_capacity(256 * 1024, &mut *file);
    write(&mut w)?;
    w.flush()?;
    drop(w);
    file.sync_all()
}

fn write_in_place(target: &Path, write: &mut Writer) -> io::Result<()> {
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(target)?;
    fill(&mut f, write)
}

/// Replacing the file by renaming over it would change something about it:
/// it has other hard links, or (on Unix) it belongs to another user.
#[cfg(unix)]
fn must_write_in_place(target: &fs::Metadata, tmp: &fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    target.nlink() > 1 || tmp.metadata().is_ok_and(|m| m.uid() != target.uid())
}

#[cfg(not(unix))]
fn must_write_in_place(_: &fs::Metadata, _: &fs::File) -> bool {
    false
}

#[cfg(test)]
pub fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    save_with(path, &mut |w| w.write_all(bytes))
}

/// Saves what `write` produces to `path`. An existing file is replaced
/// atomically: the data goes to a private temp file next to it, which is
/// flushed to disk and then renamed over the original, so a failed save (disk
/// full, crash) never leaves a truncated file. Symlinks are resolved so the
/// link itself is kept, and the original's permissions are copied. Files with
/// extra hard links or another owner are written in place instead, so they
/// keep their identity.
pub fn save_with(path: &Path, write: &mut Writer) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Ok(meta) = fs::metadata(&target) else {
        // New file: create it directly so it gets the usual default permissions.
        return write_in_place(&target, write);
    };
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Ok((tmp, mut file)) = create_private(dir, &format!(".{name}."), ".plainpad-tmp") else {
        // The directory isn't writable (but the file may be).
        return write_in_place(&target, write);
    };
    if must_write_in_place(&meta, &file) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return write_in_place(&target, write);
    }
    let written = fill(&mut file, write);
    drop(file);
    if let Err(e) = written {
        // Never fall back to writing in place here: that would truncate the original.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = fs::set_permissions(&tmp, meta.permissions());
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
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

/// The app's own directory: settings and crash-recovery snapshots live here.
pub fn app_dir() -> Option<PathBuf> {
    config_path().and_then(|p| p.parent().map(Path::to_path_buf))
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

    fn load_str(bytes: &[u8]) -> Loaded {
        load_from(std::io::Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn eol_detection() {
        let l = load_str(b"a\r\nb\r\nc");
        assert_eq!((l.eol, l.mixed_eol, l.rope.to_string()), (Eol::CrLf, false, "a\nb\nc".into()));
        assert_eq!(load_str(b"a\nb\n").eol, Eol::Lf);
        let l = load_str(b"a\rb\r");
        assert_eq!((l.eol, l.rope.to_string()), (Eol::Cr, "a\nb\n".into()));
        let l = load_str(b"a\r\nb\nc\n");
        assert_eq!((l.eol, l.mixed_eol), (Eol::Lf, true));
    }

    #[test]
    fn chunk_boundaries() {
        // A CRLF and a multi-byte char straddling the 64 KiB read boundary.
        let mut v = vec![b'x'; CHUNK - 1];
        v.extend_from_slice(b"\r\n");
        v.extend(std::iter::repeat_n(b'y', CHUNK - 2));
        v.extend_from_slice("é€😀".as_bytes());
        let l = load_str(&v);
        assert_eq!(l.encoding, Encoding::Utf8);
        assert_eq!((l.eol, l.mixed_eol), (Eol::CrLf, false));
        assert_eq!(l.rope.len_lines(), 2);
        assert!(l.rope.to_string().ends_with("yé€😀"));
        // UTF-16 with a surrogate pair split across the boundary.
        let mut text = "a".repeat(CHUNK / 2 - 2);
        text.push('😀');
        text.push_str("\r\nz");
        let bytes = encode(&text, Encoding::Utf16Le).unwrap();
        let l = load_str(&bytes);
        assert_eq!(l.encoding, Encoding::Utf16Le);
        assert_eq!(l.rope.to_string(), text.replace("\r\n", "\n"));
        // Invalid UTF-8 late in a large file falls back to Windows-1252.
        let mut v = vec![b'q'; CHUNK * 3];
        v.push(0x80);
        let l = load_str(&v);
        assert_eq!(l.encoding, Encoding::Windows1252);
        assert!(l.rope.to_string().ends_with("q€"));
    }

    #[test]
    fn encodings_roundtrip() {
        let s = "héllo – €";
        for enc in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Windows1252] {
            let bytes = encode(s, enc).unwrap();
            let l = load_str(&bytes);
            assert_eq!(l.rope.to_string(), s, "{enc:?}");
            assert_eq!(l.encoding, enc);
        }
        assert_eq!(encode("日本", Encoding::Windows1252), Err('日'));
        // Every single byte survives a cp1252 round-trip.
        let all: Vec<u8> = (0x80..=0xFF).collect();
        let l = load_str(&all);
        assert_eq!(l.encoding, Encoding::Windows1252);
        assert_eq!(encode(&l.rope.to_string(), l.encoding).unwrap(), all);
    }

    fn tmpdir() -> PathBuf {
        let (p, f) = create_private(&std::env::temp_dir(), "plainpad-test-", "").unwrap();
        drop(f);
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap();
        p
    }

    #[test]
    fn save_streams_the_rope() {
        let d = tmpdir();
        let f = d.join("s.txt");
        // Many chunks, so line endings are converted across chunk boundaries.
        let text: String = (0..5000).map(|i| format!("line {i} é€\n")).collect();
        let rope = Rope::from_str(&text);
        for (eol, enc) in [(Eol::CrLf, Encoding::Utf8), (Eol::Lf, Encoding::Utf16Be), (Eol::Cr, Encoding::Windows1252)]
        {
            let last = std::cell::Cell::new(0);
            let out = save_rope(&f, &rope, eol, enc, &|d| last.set(d));
            out.result.unwrap();
            assert_eq!((out.encoding, out.fell_back), (enc, None));
            assert_eq!(last.get(), text.len() as u64);
            let want = encode(&text.replace('\n', eol.as_str()), enc).unwrap();
            assert_eq!(fs::read(&f).unwrap(), want, "{eol:?} {enc:?}");
            let back = load(&f).unwrap();
            assert_eq!(back.rope, rope);
            assert_eq!(back.eol, eol);
        }
        // A character Windows-1252 can't store: saved as UTF-8 instead, and said so.
        let rope = Rope::from_str("ok 日本");
        let out = save_rope(&f, &rope, Eol::Lf, Encoding::Windows1252, &|_| {});
        out.result.unwrap();
        assert_eq!((out.encoding, out.fell_back), (Encoding::Utf8, Some('日')));
        assert_eq!(fs::read_to_string(&f).unwrap(), "ok 日本");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn save_replaces_and_keeps_links() {
        let d = tmpdir();
        let f = d.join("a.txt");
        fs::write(&f, "old").unwrap();
        save(&f, b"new").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        // No temp files left behind.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            // Saving through a symlink keeps the link and updates its target.
            let l = d.join("link.txt");
            symlink(&f, &l).unwrap();
            save(&l, b"via link").unwrap();
            assert!(fs::symlink_metadata(&l).unwrap().file_type().is_symlink());
            assert_eq!(fs::read_to_string(&f).unwrap(), "via link");
            // Hard links stay linked.
            let h = d.join("hard.txt");
            fs::hard_link(&f, &h).unwrap();
            save(&f, b"both").unwrap();
            assert_eq!(fs::read_to_string(&h).unwrap(), "both");
            // Permissions are kept.
            fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
            fs::remove_file(&h).unwrap();
            save(&f, b"perm").unwrap();
            assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o640);
        }
        // New files are created.
        save(&d.join("new.txt"), b"x").unwrap();
        assert_eq!(fs::read_to_string(d.join("new.txt")).unwrap(), "x");
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_exclusive_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir();
        let (p, _f) = create_private(&d, "x-", ".tmp").unwrap();
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_special_files() {
        assert!(load(Path::new("/dev/zero")).is_err());
        assert!(load(Path::new("/")).is_err());
    }
}
