//! Crash recovery: snapshots of unsaved documents, restored after a crash.
//!
//! Each running instance keeps its snapshots in its own folder under
//! `<app dir>/recovery/`, holding an exclusive lock on a file there for as long
//! as it runs. At startup, any folder whose lock can be taken belonged to an
//! instance that is no longer running (it crashed or was killed), so its
//! snapshots are offered back. Instances never touch each other's live folders.
//!
//! Snapshots are written by one background thread, in order. A small document
//! is written whole each time, to a private temporary file that is then
//! renamed into place, so a crash mid-write never leaves a torn snapshot. The
//! text comes from a rope clone, which is cheap.
//!
//! Rewriting a big document after every pause in typing would be costly (a
//! 500 MB file, 500 MB per pause), so from 1 MB up only the first snapshot
//! records the text, and later ones append just the edits to a log. When the
//! file on disk still holds the document's saved text, even the first one
//! records no text: it names the file (with its modified time and size), and
//! the log starts from there. The text is written whole again only after a
//! change too big to log (Replace All), when the log has grown to a quarter of
//! the document, or after a failed write.
//!
//! A snapshot's header names a generation, and its log is `<id>-<generation>.log`, so a
//! new full snapshot and a stale log can never be combined.

use crate::doc::{self, Encoding, Eol, Stamp};
use crate::document::{Journal, JournalOp};
use ropey::Rope;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

const MAGIC: &str = "plainpad-recovery 2";
/// The first format: always the whole text, no log.
const MAGIC_V1: &str = "plainpad-recovery 1";
/// Smaller documents are always written whole: it's cheap, and simplest.
const JOURNAL_MIN: usize = 1 << 20;

/// What a snapshot needs besides the text to restore a tab faithfully.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub eol: Eol,
}

/// A document left unsaved by a crashed instance, ready to restore.
///
/// The text is not materialised up front: a snapshot that records only a log
/// on top of a file on disk (`Source::FromFile`) is read only when the caller
/// asks, via [`Recovered::restore`], so the user can be shown which path will
/// be read before any external file is touched. A planted snapshot therefore
/// can't make the editor silently read an arbitrary path at startup.
pub struct Recovered {
    pub meta: Meta,
    src: Source,
    /// The edit log replayed on top of the base (empty for a v1 snapshot).
    log: Vec<u8>,
    v1: bool,
}

/// How a recovered document's text is obtained.
enum Source {
    /// Fully reconstructed from the snapshot's own data: nothing external to read.
    Ready(Rope),
    /// The text is this file (unchanged since the snapshot) with the log on top.
    /// The read is deferred until the user has been shown the path.
    FromFile { path: PathBuf, stamp: Stamp },
}

/// A human name for a recovered document, for messages.
fn recovered_name(meta: &Meta) -> String {
    meta.path
        .as_deref()
        .and_then(Path::file_name)
        .map_or("An untitled document".into(), |n| n.to_string_lossy().into_owned())
}

impl Recovered {
    /// The path this document had before the crash, if any. Shown to the user
    /// before restoring (so they know what each untitled tab was, and — for a
    /// file-based snapshot — which file will be read).
    pub fn origin(&self) -> Option<&Path> {
        self.meta.path.as_deref()
    }

    /// Whether restoring it reads an external file (its path is shown first).
    pub fn reads_file(&self) -> bool {
        matches!(self.src, Source::FromFile { .. })
    }

    /// If this is a file-based snapshot whose file is gone or changed, the
    /// message to show instead of restoring it (its edits can't be rebuilt).
    fn unrecoverable(&self) -> Option<String> {
        match &self.src {
            Source::FromFile { path, stamp } if doc::stamp(path) != Some(*stamp) => Some(format!(
                "{} was changed by another program, so its unsaved edits can't be rebuilt",
                recovered_name(&self.meta)
            )),
            _ => None,
        }
    }

    /// Reconstructs the text, reading the base file now if the snapshot needs
    /// it. The caller must already have shown the user the path (see
    /// [`Recovered::origin`]). Returns the final metadata and text.
    pub fn restore(self) -> Result<(Meta, Rope), String> {
        let Recovered { mut meta, src, log, v1 } = self;
        let text = match src {
            Source::Ready(r) => r,
            Source::FromFile { path, stamp } => {
                if doc::stamp(&path) != Some(stamp) {
                    return Err(format!(
                        "{} was changed by another program, so its unsaved edits can't be rebuilt",
                        recovered_name(&meta)
                    ));
                }
                let mut t =
                    doc::load(&path).map_err(|e| format!("{} could not be read: {e}", recovered_name(&meta)))?.rope;
                if !v1 {
                    replay_log(&log, &mut t, &mut meta)
                        .map_err(|e| format!("{}: its edit log is damaged ({e})", recovered_name(&meta)))?;
                }
                t
            }
        };
        Ok((meta, text))
    }
}

enum Job {
    Write { id: u64, meta: Meta, text: Rope, journal: Journal, file: Option<(PathBuf, Stamp)> },
    Remove(u64),
}

/// What a snapshot's text is built on.
#[derive(Clone)]
enum Base {
    /// The text is in the snapshot.
    Text,
    /// The file at this path, as long as it's unchanged.
    File(PathBuf, Stamp),
}

/// What's on disk for one document, as far as the writer knows.
struct OnDisk {
    generation: u64,
    base: Base,
    log_bytes: u64,
    meta: Meta,
}

struct Snap {
    /// The document journal epoch the snapshot follows.
    epoch: u64,
    /// None after a failed write: nothing on disk can be built on.
    disk: Option<OnDisk>,
}

/// The writer thread's side: decides, for each snapshot, whether to append
/// to the log, start one on top of the saved file, or write the whole text.
struct Writer {
    dir: PathBuf,
    snaps: HashMap<u64, Snap>,
    next_gen: u64,
}

pub struct Recovery {
    dir: PathBuf,
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Held for the life of the process; marks this folder as in use.
    _lock: fs::File,
}

fn recovery_root() -> Option<PathBuf> {
    doc::app_dir().map(|d| d.join("recovery"))
}

fn create_dir_private(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// Whether `path` is a real directory private to the effective user: owned by
/// it, with no group or other write access, and not a symlink. The recovery
/// code trusts a directory only when this holds, so a snapshot planted by a
/// *different* user (a world-writable config dir, or a root process that
/// inherited another user's `$HOME`) is never adopted. A peer with the *same*
/// uid still passes — that case is handled by recovery being low-capability
/// (untitled tabs, consent-gated reads), not by ownership.
#[cfg(unix)]
fn is_private_dir(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid only reads the process's effective uid; always sound.
    let euid = unsafe { libc::geteuid() };
    // symlink_metadata (lstat) so a symlink in place of the directory fails here.
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.uid() == euid && m.mode() & 0o022 == 0)
}

/// Windows analogue: a real directory (not a reparse point / symlink) that we
/// can't positively tell is owned by a *different, non-privileged* user. Default
/// `%APPDATA%` ACLs already keep other standard users out, and the dangerous
/// outcomes are prevented by recovery being low-capability, so an inconclusive
/// ownership query fails *open* (allows the directory) rather than disabling
/// recovery; only a definite foreign owner is refused.
#[cfg(windows)]
fn is_private_dir(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else { return false };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return false;
    }
    !owned_by_other_user(path).unwrap_or(false)
}

/// `Some(true)` only when the directory's owner is a valid SID that is neither
/// the current process's user nor a privileged principal (Administrators /
/// SYSTEM, as an elevated process would create). `None` on any query failure.
#[cfg(windows)]
fn owned_by_other_user(path: &Path) -> Option<bool> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; the out-params are valid; on success the
    // returned security descriptor must be released with LocalFree (done below).
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS || owner.is_null() {
        return None;
    }
    // SAFETY: `owner` is a valid SID pointer into `sd` until it is freed.
    let is_privileged = unsafe {
        IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0 || IsWellKnownSid(owner, WinLocalSystemSid) != 0
    };
    let same_as_current = (|| {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess is a pseudo-handle; `token` is an out-param.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut len = 0u32;
        // SAFETY: first call just asks for the required buffer length.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` bytes; filled with a TOKEN_USER on success.
        let ok =
            len > 0 && unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } != 0;
        // SAFETY: `token` came from OpenProcessToken.
        unsafe { CloseHandle(token) };
        if !ok {
            return None;
        }
        // SAFETY: on success `buf` begins with a TOKEN_USER whose User.Sid is valid.
        let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        // SAFETY: both SIDs are valid for the duration of this call.
        Some(unsafe { EqualSid(owner, tu.User.Sid) } != 0)
    })();
    // SAFETY: `sd` was allocated by GetNamedSecurityInfoW and is freed exactly once.
    unsafe { LocalFree(sd as _) };

    match same_as_current {
        Some(true) => Some(false),         // owned by us
        _ if is_privileged => Some(false), // owned by Administrators / SYSTEM
        Some(false) => Some(true),         // a different, non-privileged owner
        None => None,                      // couldn't determine the current user
    }
}

#[cfg(not(any(unix, windows)))]
fn is_private_dir(path: &Path) -> bool {
    path.is_dir()
}

/// Opens a file without following a final symlink, so a planted link can't
/// redirect a read (or the lock) out of the recovery directory.
fn open_nofollow(path: &Path, write: bool) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.read(!write).write(write);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut o, libc::O_NOFOLLOW);
    o.open(path)
}

impl Recovery {
    /// Creates this instance's snapshot folder and locks it.
    pub fn start() -> Option<Self> {
        Self::start_in(&recovery_root()?)
    }

    fn start_in(root: &Path) -> Option<Self> {
        create_dir_private(root).ok()?;
        // Fail closed: if the recovery directory isn't private to us (e.g. it
        // already existed owned by someone else, or is group/other-writable),
        // don't adopt from it or write snapshots into it.
        if !is_private_dir(root) {
            return None;
        }
        let (dir, _) = doc::create_private(root, &format!("{}-", std::process::id()), ".d").ok()?;
        // create_private made a file; we want a folder by that unique name.
        fs::remove_file(&dir).ok()?;
        create_dir_private(&dir).ok()?;
        let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock")).ok()?;
        lock.try_lock().ok()?;
        let (tx, rx) = mpsc::channel::<Job>();
        let wdir = dir.clone();
        let thread = std::thread::Builder::new()
            .name("autosave".into())
            .spawn(move || {
                let mut w = Writer { dir: wdir, snaps: HashMap::new(), next_gen: 1 };
                for job in rx {
                    match job {
                        Job::Write { id, meta, text, journal, file } => w.write(id, meta, &text, journal, file),
                        Job::Remove(id) => w.remove(id),
                    }
                }
            })
            .ok()?;
        Some(Self { dir, jobs: Some(tx), thread: Some(thread), _lock: lock })
    }

    /// Queues a snapshot of a document (replacing any earlier one for `id`):
    /// its text, its journal since the last snapshot, and the file holding its
    /// saved text, if that's what the journal started from.
    pub fn save(&self, id: u64, meta: Meta, text: Rope, journal: Journal, file: Option<(PathBuf, Stamp)>) {
        if let Some(tx) = &self.jobs {
            let _ = tx.send(Job::Write { id, meta, text, journal, file });
        }
    }

    /// Queues removal of a document's snapshot (it was saved, closed, or reverted).
    pub fn remove(&self, id: u64) {
        if let Some(tx) = &self.jobs {
            let _ = tx.send(Job::Remove(id));
        }
    }

    /// Test helper: behave like a crash, releasing the folder lock without the
    /// clean-exit cleanup.
    #[cfg(test)]
    fn crash(self) {
        let _ = self._lock.unlock();
        std::mem::forget(self);
    }

    /// Snapshots left by instances that are no longer running, and
    /// descriptions of any that couldn't be restored. Their folders are deleted
    /// once read; the caller re-snapshots what it restores.
    pub fn take_orphans(&self) -> (Vec<Recovered>, Vec<String>) {
        let Some(root) = self.dir.parent() else { return Default::default() };
        take_orphans_in(root, &self.dir)
    }
}

impl Drop for Recovery {
    /// On a normal exit: finish queued work, then delete this instance's folder.
    /// Every tab was saved or discarded by then, so nothing in it is needed.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn take_orphans_in(root: &Path, mine: &Path) -> (Vec<Recovered>, Vec<String>) {
    let (mut out, mut lost) = (Vec::new(), Vec::new());
    // Only adopt from a recovery tree that's private to us (see is_private_dir).
    if !is_private_dir(root) {
        return (out, lost);
    }
    let Ok(entries) = fs::read_dir(root) else { return (out, lost) };
    for e in entries.flatten() {
        let dir = e.path();
        // Skip our own folder, and anything that isn't a directory private to us
        // (a symlink, or a folder owned by another user, is never trusted).
        if dir == mine || !is_private_dir(&dir) {
            continue;
        }
        // Still locked: another running instance's live folder.
        let Ok(lock) = open_nofollow(&dir.join("lock"), true) else { continue };
        if lock.try_lock().is_err() {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|x| x == "snap") {
                match read_snapshot(&p) {
                    // A file-based snapshot whose file is gone or changed can't be
                    // rebuilt; report it rather than offering an empty recovery.
                    Ok(Some(r)) => match r.unrecoverable() {
                        Some(msg) => lost.push(msg),
                        None => out.push(r),
                    },
                    Ok(None) => {}
                    Err(e) => lost.push(e),
                }
            }
        }
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }
    (out, lost)
}

fn snapshot_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id}.snap"))
}

fn eol_name(e: Eol) -> &'static str {
    match e {
        Eol::Lf => "lf",
        Eol::CrLf => "crlf",
        Eol::Cr => "cr",
    }
}

fn encoding_name(e: Encoding) -> &'static str {
    match e {
        Encoding::Utf8 => "utf8",
        Encoding::Utf8Bom => "utf8bom",
        Encoding::Utf16Le => "utf16le",
        Encoding::Utf16Be => "utf16be",
        Encoding::Windows1252 => "cp1252",
    }
}

/// Paths are stored percent-encoded on one line, so any byte sequence survives.
fn encode_path(p: &Path) -> String {
    let bytes = p.as_os_str().as_encoded_bytes();
    let mut s = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'%' {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn decode_path(s: &str) -> Option<PathBuf> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    path_from_encoded_bytes(out)
}

/// Rebuilds a path from the bytes [`encode_path`] produced. The bytes come from
/// a snapshot that may be corrupt or planted, so they are validated rather than
/// handed to `OsString::from_encoded_bytes_unchecked`, whose safety contract the
/// untrusted bytes need not meet — on Windows that call is undefined behaviour
/// for anything that isn't valid WTF-8.
#[cfg(unix)]
fn path_from_encoded_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    // On Unix every byte sequence is a valid `OsStr`, so this is lossless (it
    // round-trips non-UTF-8 file names) and sound.
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn path_from_encoded_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    // Elsewhere (Windows, WASI) accept only valid UTF-8 instead of the unchecked
    // constructor. A path that isn't valid UTF-8 (e.g. a Windows name with an
    // unpaired surrogate) simply isn't restored — recovery falls back to an
    // untitled document rather than risking UB.
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

fn log_path(dir: &Path, id: u64, generation: u64) -> PathBuf {
    dir.join(format!("{id}-{generation}.log"))
}

fn parse_encoding(s: &str) -> Encoding {
    match s {
        "utf8bom" => Encoding::Utf8Bom,
        "utf16le" => Encoding::Utf16Le,
        "utf16be" => Encoding::Utf16Be,
        "cp1252" => Encoding::Windows1252,
        _ => Encoding::Utf8,
    }
}

fn parse_eol(s: &str) -> Eol {
    match s {
        "crlf" => Eol::CrLf,
        "cr" => Eol::Cr,
        _ => Eol::Lf,
    }
}

fn stamp_text((t, size): Stamp) -> Option<String> {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(format!("{} {} {size}", d.as_secs(), d.subsec_nanos()))
}

fn parse_stamp(s: &str) -> Option<Stamp> {
    let mut it = s.split(' ').map(str::parse::<u64>);
    let (secs, nanos, size) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    Some((SystemTime::UNIX_EPOCH + Duration::new(secs, u32::try_from(nanos).ok()?), size))
}

impl Base {
    /// Whether what it names is still there, unchanged.
    fn valid(&self) -> bool {
        match self {
            Base::Text => true,
            Base::File(p, st) => doc::stamp(p) == Some(*st),
        }
    }
}

impl Writer {
    fn write(&mut self, id: u64, meta: Meta, text: &Rope, journal: Journal, file: Option<(PathBuf, Stamp)>) {
        let epoch = journal.epoch;
        let result = self.try_write(id, &meta, text, journal, file);
        let disk = result.ok();
        let old = self.snaps.insert(id, Snap { epoch, disk });
        // A new generation replaces the old one's log.
        if let Some(old) = old.and_then(|s| s.disk)
            && self.snaps[&id].disk.as_ref().is_none_or(|d| d.generation != old.generation)
        {
            let _ = fs::remove_file(log_path(&self.dir, id, old.generation));
        }
    }

    fn try_write(
        &mut self,
        id: u64,
        meta: &Meta,
        text: &Rope,
        journal: Journal,
        file: Option<(PathBuf, Stamp)>,
    ) -> io::Result<OnDisk> {
        if let Some(ops) = journal.ops.filter(|_| text.len_bytes() >= JOURNAL_MIN) {
            let snap = self.snaps.get_mut(&id);
            // Same journal as last time, all written: just add the new edits.
            if let Some(Snap { epoch, disk: Some(d) }) = snap
                && *epoch == journal.epoch
                && d.log_bytes < (text.len_bytes() as u64 / 4).max(4 << 20)
                && d.base.valid()
            {
                let changed = (d.meta != *meta).then_some(meta);
                let n = append_log(&log_path(&self.dir, id, d.generation), &ops, changed)?;
                return Ok(OnDisk {
                    generation: d.generation,
                    base: d.base.clone(),
                    log_bytes: d.log_bytes + n,
                    meta: meta.clone(),
                });
            }
            // A journal we haven't seen, starting from the saved file: build on the file.
            // (Seen before means some of its edits went into a write that's gone.)
            let unseen = snap.is_none_or(|s| s.epoch != journal.epoch);
            if unseen
                && journal.from_saved
                && let Some((path, st)) = file
                && doc::stamp(&path) == Some(st)
            {
                let generation = self.next_generation();
                let base = Base::File(path, st);
                let n = append_log(&log_path(&self.dir, id, generation), &ops, None)?;
                write_header(&self.dir, id, meta, generation, &base, None)?;
                return Ok(OnDisk { generation, base, log_bytes: n, meta: meta.clone() });
            }
        }
        let generation = self.next_generation();
        write_header(&self.dir, id, meta, generation, &Base::Text, Some(text))?;
        Ok(OnDisk { generation, base: Base::Text, log_bytes: 0, meta: meta.clone() })
    }

    fn next_generation(&mut self) -> u64 {
        self.next_gen += 1;
        self.next_gen
    }

    fn remove(&mut self, id: u64) {
        let _ = fs::remove_file(snapshot_path(&self.dir, id));
        if let Some(d) = self.snaps.remove(&id).and_then(|s| s.disk) {
            let _ = fs::remove_file(log_path(&self.dir, id, d.generation));
        }
    }
}

/// Appends edits (and the new metadata, if it changed) to a log. Returns the bytes written.
///
/// Records: `E <at> <remove> <len>`, a newline, `len` bytes of inserted text and
/// a newline; or `M <encoding> <eol> <path>` (path percent-encoded, `-` for none).
fn append_log(path: &Path, ops: &[JournalOp], meta: Option<&Meta>) -> io::Result<u64> {
    let mut opts = fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut w = io::BufWriter::new(opts.open(path)?);
    let mut n = 0u64;
    if let Some(m) = meta {
        let p = m.path.as_deref().map_or("-".into(), |p| format!("p{}", encode_path(p)));
        let line = format!("M {} {} {p}\n", encoding_name(m.encoding), eol_name(m.eol));
        w.write_all(line.as_bytes())?;
        n += line.len() as u64;
    }
    for op in ops {
        let head = format!("E {} {} {}\n", op.at, op.remove, op.insert.len());
        w.write_all(head.as_bytes())?;
        w.write_all(op.insert.as_bytes())?;
        w.write_all(b"\n")?;
        n += (head.len() + op.insert.len() + 1) as u64;
    }
    w.flush()?;
    Ok(n)
}

/// Writes a snapshot header (and the text, for a text base) via a temporary file.
fn write_header(dir: &Path, id: u64, meta: &Meta, generation: u64, base: &Base, text: Option<&Rope>) -> io::Result<()> {
    let (tmp, file) = doc::create_private(dir, ".", ".tmp")?;
    let result = (|| {
        let mut w = io::BufWriter::new(file);
        writeln!(w, "{MAGIC}")?;
        writeln!(w, "path:{}", meta.path.as_deref().map(encode_path).unwrap_or_default())?;
        writeln!(w, "encoding:{}", encoding_name(meta.encoding))?;
        writeln!(w, "eol:{}", eol_name(meta.eol))?;
        writeln!(w, "generation:{generation}")?;
        match base {
            Base::Text => writeln!(w, "base:text")?,
            Base::File(p, st) => {
                let st = stamp_text(*st).ok_or_else(|| io::Error::other("file time out of range"))?;
                writeln!(w, "base:file {st} {}", encode_path(p))?;
            }
        }
        writeln!(w)?;
        for chunk in text.iter().flat_map(|t| t.chunks()) {
            w.write_all(chunk.as_bytes())?;
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&tmp, snapshot_path(dir, id))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Reads a snapshot and replays its log. `Ok(None)` for something that isn't
/// a snapshot; `Err` describes a document that couldn't be restored.
fn read_snapshot(p: &Path) -> Result<Option<Recovered>, String> {
    let Ok(f) = open_nofollow(p, false) else { return Ok(None) };
    let mut r = io::BufReader::new(f);
    let mut line = || {
        let mut l = String::new();
        r.read_line(&mut l).ok().filter(|&n| n > 0)?;
        l.pop();
        Some(l)
    };
    let v1 = match line().as_deref() {
        Some(MAGIC) => false,
        Some(MAGIC_V1) => true,
        _ => return Ok(None),
    };
    let header = (|| {
        let path = line()?.strip_prefix("path:")?.to_owned();
        let path = if path.is_empty() { None } else { Some(decode_path(&path)?) };
        let encoding = parse_encoding(line()?.strip_prefix("encoding:")?);
        let eol = parse_eol(line()?.strip_prefix("eol:")?);
        let (generation, base) = if v1 {
            (0, "text".to_owned())
        } else {
            let generation = line()?.strip_prefix("generation:")?.parse::<u64>().ok()?;
            (generation, line()?.strip_prefix("base:")?.to_owned())
        };
        line()?.is_empty().then_some((Meta { path, encoding, eol }, generation, base))
    })();
    let Some((mut meta, generation, base)) = header else { return Ok(None) };
    let name = recovered_name(&meta);
    // The edit log (our own file, not an external path) is safe to read now.
    let log = if v1 {
        Vec::new()
    } else {
        let lp = p.with_file_name(format!("{}-{generation}.log", p.file_stem().unwrap_or_default().to_string_lossy()));
        // O_NOFOLLOW: a symlink planted at the log name can't redirect the read.
        let mut buf = Vec::new();
        if let Ok(mut f) = open_nofollow(&lp, false) {
            use std::io::Read;
            let _ = f.read_to_end(&mut buf);
        }
        buf
    };
    if base == "text" {
        // The text is in the snapshot itself: reconstruct it now, nothing external to read.
        let mut text = Rope::from_reader(r).map_err(|_| format!("{name}: its snapshot is damaged"))?;
        if !v1 {
            replay_log(&log, &mut text, &mut meta).map_err(|e| format!("{name}: its edit log is damaged ({e})"))?;
        }
        Ok(Some(Recovered { meta, src: Source::Ready(text), log: Vec::new(), v1 }))
    } else {
        // "file <secs> <nanos> <size> <path>": record the path and stamp, but
        // defer reading the file until the user has been shown that path.
        let (st, file) = base
            .strip_prefix("file ")
            .and_then(|b| b.rsplit_once(' '))
            .and_then(|(st, p)| Some((parse_stamp(st)?, decode_path(p)?)))
            .ok_or_else(|| format!("{name}: its snapshot is damaged"))?;
        Ok(Some(Recovered { meta, src: Source::FromFile { path: file, stamp: st }, log, v1 }))
    }
}

/// Applies a log's records. A record cut short (a crash while writing it) ends the log.
fn replay_log(mut data: &[u8], text: &mut Rope, meta: &mut Meta) -> Result<(), String> {
    while let Some(nl) = data.iter().position(|&b| b == b'\n') {
        let head = std::str::from_utf8(&data[..nl]).map_err(|_| "bad record")?;
        let rest = &data[nl + 1..];
        let mut f = head.split(' ');
        match f.next() {
            Some("E") => {
                let mut num = || f.next().and_then(|n| n.parse::<usize>().ok()).ok_or("bad edit");
                let (at, remove, len) = (num()?, num()?, num()?);
                if rest.len() <= len {
                    return Ok(()); // cut short
                }
                let insert = std::str::from_utf8(&rest[..len]).map_err(|_| "bad text")?.to_owned();
                crate::document::replay(text, &[JournalOp { at, remove, insert }])?;
                data = &rest[len + 1..];
            }
            Some("M") => {
                let (enc, eol, path) = (f.next().ok_or("bad meta")?, f.next().ok_or("bad meta")?, f.next());
                meta.encoding = parse_encoding(enc);
                meta.eol = parse_eol(eol);
                meta.path = match path.and_then(|p| p.strip_prefix('p')) {
                    Some(p) => Some(decode_path(p).ok_or("bad path")?),
                    None => None,
                };
                data = rest;
            }
            _ => return Err("unknown record".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use cosmic_text::Cursor;

    fn tmp_root() -> PathBuf {
        let (p, f) = doc::create_private(&std::env::temp_dir(), "plainpad-rec-test-", "").unwrap();
        drop(f);
        fs::remove_file(&p).unwrap();
        p
    }

    fn wait_for(p: &Path, present: bool) {
        for _ in 0..200 {
            if p.exists() == present {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{} never became {}", p.display(), if present { "present" } else { "absent" });
    }

    #[test]
    fn snapshots_survive_a_crash_but_not_a_clean_exit() {
        let root = tmp_root();
        let meta =
            Meta { path: Some(PathBuf::from("/tmp/odd name\n%.txt")), encoding: Encoding::Utf16Le, eol: Eol::CrLf };
        // A running instance writes snapshots...
        let a = Recovery::start_in(&root).unwrap();
        let mut d = Document::new("unsaved text\nline 2");
        a.save(1, meta.clone(), d.snapshot(), d.take_journal(), None);
        let none = Meta { path: None, encoding: Encoding::Utf8, eol: Eol::Lf };
        a.save(2, none, Rope::from_str("gone"), Document::new("").take_journal(), None);
        a.remove(2);
        wait_for(&snapshot_path(&a.dir, 1), true);
        wait_for(&snapshot_path(&a.dir, 2), false);
        // ...and a second instance must not take them while the first is alive.
        let b = Recovery::start_in(&root).unwrap();
        assert!(b.take_orphans().0.is_empty());
        // Simulate a crash: the first instance's lock is released without cleanup.
        let a_dir = a.dir.clone();
        a.crash();
        let (rec, lost) = b.take_orphans();
        assert_eq!((rec.len(), lost.len()), (1, 0));
        let (m, text) = rec.into_iter().next().unwrap().restore().unwrap();
        assert_eq!(m, meta);
        assert_eq!(text, "unsaved text\nline 2");
        assert!(!a_dir.exists(), "orphan folder is removed once recovered");
        // A clean exit removes the instance's folder.
        let b_dir = b.dir.clone();
        drop(b);
        assert!(!b_dir.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovered_items_expose_origin_and_defer_file_reads() {
        let root = tmp_root();
        fs::create_dir_all(&root).unwrap();
        let file = root.join("doc.txt");
        let original = "saved line\n".repeat(200_000); // >1 MB: logs on top of the file
        fs::write(&file, &original).unwrap();
        let meta = Meta { path: Some(file.clone()), encoding: Encoding::Utf8, eol: Eol::Lf };
        let base = Some((file.clone(), doc::stamp(&file).unwrap()));
        let c = |l, i| Cursor::new(l, i);

        let a = Recovery::start_in(&root).unwrap();
        // A file-based snapshot (big doc, unchanged file) and a text-based one.
        let mut big = Document::from_rope(doc::load(&file).unwrap().rope);
        big.replace(c(0, 0), c(0, 0), "EDIT ", false);
        a.save(1, meta.clone(), big.snapshot(), big.take_journal(), base);
        let note = Meta { path: Some(root.join("note.txt")), encoding: Encoding::Utf8, eol: Eol::Lf };
        let mut small = Document::new("just a note");
        a.save(2, note.clone(), small.snapshot(), small.take_journal(), None);
        flush(&a);
        a.crash();

        let b = Recovery::start_in(&root).unwrap();
        let (mut rec, lost) = b.take_orphans();
        assert!(lost.is_empty(), "{lost:?}");
        rec.sort_by_key(|r| r.origin().map(|p| p.to_path_buf()));
        // One reads an external file (the big doc); the other is self-contained.
        let big_item = rec.iter().find(|r| r.origin() == Some(file.as_path())).unwrap();
        assert!(big_item.reads_file(), "big doc rebuilds from its file");
        let note_item = rec.iter().find(|r| r.origin() == Some(root.join("note.txt").as_path())).unwrap();
        assert!(!note_item.reads_file(), "text-based snapshot needs no external read");
        // restore() yields the expected text for both.
        let texts: std::collections::HashMap<_, _> = rec
            .into_iter()
            .map(|r| {
                let origin = r.origin().map(Path::to_path_buf);
                (origin, r.restore().unwrap().1.to_string())
            })
            .collect();
        assert_eq!(texts[&Some(file.clone())], format!("EDIT {original}"));
        assert_eq!(texts[&Some(root.join("note.txt"))], "just a note");
        drop(b);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn non_private_recovery_dirs_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp_root();
        // A crashed instance leaves an orphan folder with a snapshot.
        let a = Recovery::start_in(&root).unwrap();
        let meta = Meta { path: None, encoding: Encoding::Utf8, eol: Eol::Lf };
        a.save(1, meta, Rope::from_str("secret"), Document::new("").take_journal(), None);
        wait_for(&snapshot_path(&a.dir, 1), true);
        let a_dir = a.dir.clone();
        a.crash();

        // Made group/other-writable (as a planted folder would be): not adopted.
        fs::set_permissions(&a_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let b = Recovery::start_in(&root).unwrap();
        let (rec, lost) = b.take_orphans();
        assert!(rec.is_empty() && lost.is_empty(), "a group/other-writable folder must be skipped");
        drop(b);

        // A recovery root that isn't private to us: recovery refuses to start.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Recovery::start_in(&root).is_none(), "a world-writable recovery root must be refused");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn path_encoding_round_trips_and_rejects_garbage() {
        for p in ["/tmp/a b.txt", "/weird/na me\n%.txt", "relative.txt", ""] {
            let enc = encode_path(Path::new(p));
            assert_eq!(decode_path(&enc).as_deref(), Some(Path::new(p)), "round-trip {p:?}");
        }
        // Malformed percent-encoding is rejected rather than decoded into garbage.
        assert_eq!(decode_path("%G1"), None);
        assert_eq!(decode_path("%0"), None);
        assert_eq!(decode_path("ab%"), None);
        #[cfg(unix)]
        {
            // A non-UTF-8 byte round-trips losslessly on Unix.
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(decode_path("%FF").unwrap().as_os_str().as_bytes(), b"\xFF");
        }
    }

    /// Waits until the writer thread has handled everything queued so far.
    fn flush(r: &Recovery) {
        r.save(
            u64::MAX,
            Meta { path: None, encoding: Encoding::Utf8, eol: Eol::Lf },
            Rope::new(),
            Journal { epoch: 0, from_saved: false, ops: None },
            None,
        );
        wait_for(&snapshot_path(&r.dir, u64::MAX), true);
        r.remove(u64::MAX);
        wait_for(&snapshot_path(&r.dir, u64::MAX), false);
    }

    #[test]
    fn big_documents_log_their_edits() {
        let root = tmp_root();
        fs::create_dir_all(&root).unwrap();
        let file = root.join("big.txt");
        let original = "0123456789 abcdé\n".repeat(80_000); // ~1.4 MB
        fs::write(&file, &original).unwrap();
        let meta = Meta { path: Some(file.clone()), encoding: Encoding::Utf8, eol: Eol::Lf };
        let base = Some((file.clone(), doc::stamp(&file).unwrap()));
        let c = |l, i| Cursor::new(l, i);

        let a = Recovery::start_in(&root).unwrap();
        let mut d = Document::from_rope(doc::load(&file).unwrap().rope);
        d.replace(c(0, 0), c(0, 3), "START", false);
        a.save(7, meta.clone(), d.snapshot(), d.take_journal(), base.clone());
        d.replace(c(5, 2), c(5, 2), "é\nmore", false);
        d.undo();
        d.replace(c(79_999, 0), c(80_000, 0), "", false);
        let changed = Meta { eol: Eol::CrLf, ..meta.clone() };
        a.save(7, changed.clone(), d.snapshot(), d.take_journal(), base.clone());
        flush(&a);
        // Nothing but a header and a small log: the text came from the file.
        let snap = snapshot_path(&a.dir, 7);
        assert!(fs::metadata(&snap).unwrap().len() < 300, "header only");
        let logs: u64 = fs::read_dir(&a.dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(logs < 300, "{logs} bytes of log");
        let want = d.snapshot();
        let a_dir = a.dir.clone();
        a.crash();
        let b = Recovery::start_in(&root).unwrap();
        let (rec, lost) = b.take_orphans();
        assert!(lost.is_empty(), "{lost:?}");
        let (m, text) = rec.into_iter().next().unwrap().restore().unwrap();
        assert_eq!(text, want);
        assert_eq!(m, changed);
        assert!(!a_dir.exists());

        // Replace All can't be logged: the text is written whole, then logging resumes on top.
        let p = crate::document::prepare_replace_all(d.rope(), &[0..5], "S");
        d.apply_prepared(p);
        b.save(8, meta.clone(), d.snapshot(), d.take_journal(), base.clone());
        d.replace(c(1, 0), c(1, 0), "after", false);
        b.save(8, meta.clone(), d.snapshot(), d.take_journal(), base.clone());
        flush(&b);
        assert!(fs::metadata(snapshot_path(&b.dir, 8)).unwrap().len() > 1_000_000, "full text");
        // A crash while writing a log record leaves it cut short: what came before still counts.
        let log = fs::read_dir(&b.dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "log"));
        let mut f = fs::OpenOptions::new().append(true).open(log.unwrap()).unwrap();
        f.write_all(b"E 0 0 100\npartial").unwrap();
        let want = d.snapshot();
        // A document built on the file, which then changes: reported, not rebuilt wrongly.
        let mut e = Document::from_rope(doc::load(&file).unwrap().rope);
        e.replace(c(0, 0), c(0, 0), "x", false);
        b.save(9, meta.clone(), e.snapshot(), e.take_journal(), base.clone());
        flush(&b);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&file, "changed by someone else").unwrap();
        b.crash();
        let c3 = Recovery::start_in(&root).unwrap();
        let (rec, lost) = c3.take_orphans();
        assert_eq!(rec.len(), 1);
        let (_m, text) = rec.into_iter().next().unwrap().restore().unwrap();
        assert_eq!(text, want);
        assert_eq!(lost.len(), 1);
        assert!(lost[0].contains("big.txt was changed by another program"), "{}", lost[0]);
        drop(c3);
        let _ = fs::remove_dir_all(&root);
    }
}
