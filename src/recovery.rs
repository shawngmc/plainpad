//! Crash recovery: snapshots of unsaved documents, restored after a crash.
//!
//! Each running instance keeps its snapshots in its own folder under
//! `<app dir>/recovery/`, holding an exclusive lock on a file there for as long
//! as it runs. At startup, any folder whose lock can be taken belonged to an
//! instance that is no longer running (it crashed or was killed), so its
//! snapshots are offered back. Instances never touch each other's live folders.
//!
//! Snapshots are written by one background thread, in order, each to a private
//! temporary file that is then renamed into place, so a crash mid-write never
//! leaves a torn snapshot. The text comes from a rope clone, which is cheap.

use crate::doc::{self, Encoding, Eol};
use ropey::Rope;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const MAGIC: &str = "plainpad-recovery 1";

/// What a snapshot needs besides the text to restore a tab faithfully.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub eol: Eol,
}

/// A recovered document.
pub struct Recovered {
    pub meta: Meta,
    pub text: String,
}

enum Job {
    Write { id: u64, meta: Meta, text: Rope },
    Remove(u64),
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

impl Recovery {
    /// Creates this instance's snapshot folder and locks it.
    pub fn start() -> Option<Self> {
        Self::start_in(&recovery_root()?)
    }

    fn start_in(root: &Path) -> Option<Self> {
        create_dir_private(root).ok()?;
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
                for job in rx {
                    match job {
                        Job::Write { id, meta, text } => {
                            let _ = write_snapshot(&wdir, id, &meta, &text);
                        }
                        Job::Remove(id) => {
                            let _ = fs::remove_file(snapshot_path(&wdir, id));
                        }
                    }
                }
            })
            .ok()?;
        Some(Self { dir, jobs: Some(tx), thread: Some(thread), _lock: lock })
    }

    /// Queues a snapshot of a document (replacing any earlier one for `id`).
    pub fn save(&self, id: u64, meta: Meta, text: Rope) {
        if let Some(tx) = &self.jobs {
            let _ = tx.send(Job::Write { id, meta, text });
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

    /// Snapshots left by instances that are no longer running. Their folders
    /// are deleted once read; the caller re-snapshots what it restores.
    pub fn take_orphans(&self) -> Vec<Recovered> {
        let Some(root) = self.dir.parent() else { return Vec::new() };
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

fn take_orphans_in(root: &Path, mine: &Path) -> Vec<Recovered> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else { return out };
    for e in entries.flatten() {
        let dir = e.path();
        if dir == mine || !dir.is_dir() {
            continue;
        }
        // Still locked: another running instance's live folder.
        let Ok(lock) = fs::OpenOptions::new().write(true).open(dir.join("lock")) else { continue };
        if lock.try_lock().is_err() {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|x| x == "snap")
                && let Some(r) = read_snapshot(&p)
            {
                out.push(r);
            }
        }
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }
    out
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
    // SAFETY: these bytes came from `as_encoded_bytes` of an OsStr on this platform.
    Some(PathBuf::from(unsafe { std::ffi::OsString::from_encoded_bytes_unchecked(out) }))
}

fn write_snapshot(dir: &Path, id: u64, meta: &Meta, text: &Rope) -> io::Result<()> {
    let (tmp, file) = doc::create_private(dir, ".", ".tmp")?;
    let result = (|| {
        let mut w = io::BufWriter::new(file);
        writeln!(w, "{MAGIC}")?;
        writeln!(w, "path:{}", meta.path.as_deref().map(encode_path).unwrap_or_default())?;
        writeln!(w, "encoding:{}", encoding_name(meta.encoding))?;
        writeln!(w, "eol:{}", eol_name(meta.eol))?;
        writeln!(w)?;
        for chunk in text.chunks() {
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

fn read_snapshot(p: &Path) -> Option<Recovered> {
    let data = fs::read_to_string(p).ok()?;
    let mut rest = data.as_str();
    let mut next = || {
        let (line, r) = rest.split_once('\n')?;
        rest = r;
        Some(line)
    };
    if next()? != MAGIC {
        return None;
    }
    let path = next()?.strip_prefix("path:")?;
    let path = if path.is_empty() { None } else { Some(decode_path(path)?) };
    let encoding = match next()?.strip_prefix("encoding:")? {
        "utf8bom" => Encoding::Utf8Bom,
        "utf16le" => Encoding::Utf16Le,
        "utf16be" => Encoding::Utf16Be,
        "cp1252" => Encoding::Windows1252,
        _ => Encoding::Utf8,
    };
    let eol = match next()?.strip_prefix("eol:")? {
        "crlf" => Eol::CrLf,
        "cr" => Eol::Cr,
        _ => Eol::Lf,
    };
    if !next()?.is_empty() {
        return None;
    }
    Some(Recovered { meta: Meta { path, encoding, eol }, text: rest.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        a.save(1, meta.clone(), Rope::from_str("unsaved text\nline 2"));
        a.save(2, Meta { path: None, encoding: Encoding::Utf8, eol: Eol::Lf }, Rope::from_str("gone"));
        a.remove(2);
        wait_for(&snapshot_path(&a.dir, 1), true);
        wait_for(&snapshot_path(&a.dir, 2), false);
        // ...and a second instance must not take them while the first is alive.
        let b = Recovery::start_in(&root).unwrap();
        assert!(b.take_orphans().is_empty());
        // Simulate a crash: the first instance's lock is released without cleanup.
        let a_dir = a.dir.clone();
        a.crash();
        let rec = b.take_orphans();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].meta, meta);
        assert_eq!(rec[0].text, "unsaved text\nline 2");
        assert!(!a_dir.exists(), "orphan folder is removed once recovered");
        // A clean exit removes the instance's folder.
        let b_dir = b.dir.clone();
        drop(b);
        assert!(!b_dir.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
