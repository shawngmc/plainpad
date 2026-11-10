//! Work on whole documents that can take seconds on a big file: opening,
//! reloading, saving, Find and Replace All. For documents of a few megabytes
//! or more it runs on a background thread, with progress in the status bar,
//! so the window never stops responding. Smaller documents are handled
//! straight away, as before.
//!
//! While a document is being loaded, saved or replaced it is read-only, so the
//! result always matches what's on screen. A search doesn't lock it: if the
//! text changes meanwhile, the result is simply dropped.

use super::*;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;

/// Documents at least this big are handled in the background.
pub(crate) const BACKGROUND_BYTES: usize = 4 << 20;
/// Files at least this big need confirming before they're opened.
pub(crate) const HUGE_BYTES: u64 = 1 << 30;
/// How often progress is redrawn.
pub(crate) const TICK: Duration = Duration::from_millis(100);

/// Shared between a job's thread and the window: how far it has got, and
/// whether it should stop.
#[derive(Default)]
pub(crate) struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

impl Progress {
    pub(crate) fn set(&self, done: u64) {
        self.done.store(done, Relaxed);
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    fn percent(&self) -> u64 {
        let total = self.total.load(Relaxed).max(1);
        self.done.load(Relaxed).min(total) * 100 / total
    }
}

/// What a job is doing, and what's needed to finish it on the window's thread.
pub(crate) enum Work {
    Load { path: PathBuf, reload: bool },
    Save { path: PathBuf, encoding: Encoding, eol: Eol, then: Option<Pending> },
    Find { version: u64, needle: String },
    ReplaceAll { version: u64, needle: String },
}

/// A job's result.
pub(crate) enum Done {
    Loaded(io::Result<doc::Loaded>),
    Saved(doc::SaveOutcome),
    Found(Option<Range<usize>>),
    Replaced(Option<document::Prepared>),
}

pub(crate) struct Job {
    id: u64,
    tab: u64,
    work: Work,
    progress: Arc<Progress>,
}

impl Job {
    /// Whether the job's tab must not change until it's done.
    fn locks(&self) -> bool {
        !matches!(self.work, Work::Find { .. })
    }
}

pub(crate) struct Jobs {
    list: Vec<Job>,
    tx: mpsc::Sender<(u64, Done)>,
    rx: mpsc::Receiver<(u64, Done)>,
    next: u64,
    /// Quit once the saves in progress have finished.
    exit_after_saves: bool,
    /// When progress is next redrawn.
    pub(crate) tick_at: Option<Instant>,
}

impl Default for Jobs {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { list: Vec::new(), tx, rx, next: 0, exit_after_saves: false, tick_at: None }
    }
}

impl Jobs {
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// How a save went.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Saving {
    Done,
    Failed,
    /// Running in the background; any follow-up (closing the tab, quitting)
    /// happens when it finishes.
    Started,
}

/// Counts bytes read, for progress, and stops reading once cancelled.
struct Counted<R> {
    inner: R,
    pos: u64,
    progress: Arc<Progress>,
}

impl<R: io::Read> io::Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.progress.cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        self.progress.set(self.pos);
        Ok(n)
    }
}

impl<R: io::Seek> io::Seek for Counted<R> {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        self.progress.set(self.pos);
        Ok(self.pos)
    }
}

/// "1.4 GB" and so on.
fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.0} MB", n as f64 / (1u64 << 20) as f64),
        n => format!("{} KB", n.div_ceil(1024)),
    }
}

impl App {
    // ---------------------------------------------------------------- running jobs

    /// Runs `run` on a new thread for the tab with this id. Returns false (and
    /// shows why) if the thread couldn't be started.
    fn spawn_job(
        &mut self,
        tab: u64,
        work: Work,
        total: usize,
        run: impl FnOnce(Arc<Progress>) -> Done + Send + 'static,
    ) -> bool {
        let id = self.jobs.next;
        self.jobs.next += 1;
        let progress = Arc::new(Progress::default());
        progress.total.store(total as u64, Relaxed);
        let (tx, proxy, p) = (self.jobs.tx.clone(), self.proxy.clone(), progress.clone());
        let started = std::thread::Builder::new().name("plainpad-job".into()).spawn(move || {
            let done = run(p);
            let _ = tx.send((id, done));
            let _ = proxy.send_event(UserEvent::Wake);
        });
        if let Err(e) = started {
            self.win.status_msg = Some(format!("Could not start background work: {e}"));
            return false;
        }
        self.jobs.list.push(Job { id, tab, work, progress });
        self.win.redraw_later();
        true
    }

    /// Whether the tab is being loaded, saved or replaced (and so read-only).
    pub(crate) fn busy(&self, tab: u64) -> bool {
        self.jobs.list.iter().any(|j| j.tab == tab && j.locks())
    }

    pub(crate) fn saving(&self, tab: u64) -> bool {
        self.jobs.list.iter().any(|j| j.tab == tab && matches!(j.work, Work::Save { .. }))
    }

    /// Tells the tab's searches to stop (their results would be out of date).
    fn cancel_finds(&mut self, tab: u64) {
        for j in &self.jobs.list {
            if j.tab == tab && matches!(j.work, Work::Find { .. }) {
                j.progress.cancel.store(true, Relaxed);
            }
        }
    }

    /// Stops the work of tabs that have been closed.
    pub(crate) fn cancel_orphan_jobs(&mut self) {
        if self.jobs.list.is_empty() {
            return;
        }
        let live: Vec<u64> = std::iter::once(&self.win)
            .chain(self.others.iter())
            .flat_map(|w| w.tabs.iter().map(|t| t.id))
            .chain(self.spawn.as_ref().map(|s| s.tab.id))
            .collect();
        for j in &self.jobs.list {
            if !live.contains(&j.tab) {
                j.progress.cancel.store(true, Relaxed);
            }
        }
    }

    /// What the tab's background work is up to, for the status bar.
    pub(crate) fn job_status(&self, tab: u64) -> Option<String> {
        let j = self.jobs.list.iter().find(|j| j.tab == tab)?;
        let what = match j.work {
            Work::Load { reload: false, .. } => "Opening",
            Work::Load { reload: true, .. } => "Reloading",
            Work::Save { .. } => "Saving",
            Work::Find { .. } => "Searching",
            Work::ReplaceAll { .. } => "Replacing",
        };
        Some(format!("{what}… {}%", j.progress.percent()))
    }

    /// How far the tab's file has been read, while it's being opened (its text isn't there yet).
    pub(crate) fn opening(&self, tab: u64) -> Option<u64> {
        let j = self.jobs.list.iter().find(|j| j.tab == tab && matches!(j.work, Work::Load { reload: false, .. }))?;
        Some(j.progress.percent())
    }

    /// Redraws the windows showing progress (called on a timer while jobs run).
    pub(crate) fn tick(&mut self) {
        self.jobs.tick_at = None;
        for w in std::iter::once(&self.win).chain(self.others.iter()) {
            if w.tabs.get(w.active).is_some_and(|t| self.jobs.list.iter().any(|j| j.tab == t.id)) {
                w.redraw_later();
            }
        }
    }

    /// Makes the window holding this tab current. False if no window has it.
    fn focus_tab(&mut self, tab: u64) -> bool {
        if self.win.tab_pos(tab).is_some() {
            return true;
        }
        match self.others.iter().position(|w| w.tab_pos(tab).is_some()) {
            Some(k) => {
                self.swap_to(k);
                true
            }
            None => false,
        }
    }

    /// Applies the results of finished jobs.
    pub(crate) fn finish_jobs(&mut self) {
        while let Ok((id, done)) = self.jobs.rx.try_recv() {
            let Some(k) = self.jobs.list.iter().position(|j| j.id == id) else { continue };
            let job = self.jobs.list.remove(k);
            if !self.focus_tab(job.tab) {
                continue; // the tab was closed meanwhile
            }
            let i = self.win.tab_pos(job.tab).expect("focus_tab found it");
            if job.locks() {
                self.win.tabs[i].view.read_only = false;
            }
            let fresh = |app: &Self, v: u64| app.win.tabs[i].view.doc.version() == v;
            match (job.work, done) {
                (Work::Load { path, reload }, Done::Loaded(r)) => self.loaded(i, path, reload, r),
                (Work::Save { path, encoding, eol, then }, Done::Saved(out)) => {
                    let ok = self.saved(i, path, encoding, eol, out);
                    match then {
                        Some(p) if ok => {
                            self.activate(i);
                            self.continue_pending(p);
                        }
                        Some(_) => self.abandon_close(),
                        None => {}
                    }
                }
                (Work::Find { version, needle }, Done::Found(r)) if fresh(self, version) => {
                    self.show_found(i, r, &needle)
                }
                (Work::ReplaceAll { version, needle }, Done::Replaced(p)) if fresh(self, version) => {
                    self.replaced(i, p, &needle)
                }
                _ => {}
            }
            self.win.needs_redraw = true;
            self.win.redraw_later();
        }
        if self.jobs.exit_after_saves && !self.jobs.list.iter().any(|j| matches!(j.work, Work::Save { .. })) {
            self.jobs.exit_after_saves = false;
            self.request_exit();
        }
    }

    // ---------------------------------------------------------------- opening

    /// Opens a file in a new (or reused empty) tab, asking first if it's enormous.
    pub(crate) fn open_new(&mut self, path: PathBuf, confirmed: bool) {
        let meta = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            Ok(_) => {
                return self.win.status_msg = Some(format!("Could not open {}: not a regular file", path.display()));
            }
            Err(e) => return self.win.status_msg = Some(format!("Could not open {}: {e}", path.display())),
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if meta.len() >= HUGE_BYTES && !confirmed {
            self.win.prompt = Some(Prompt::large(format!(
                "{name} is {}. Opening it needs at least that much memory, and the computer may slow down. Open it anyway?",
                human_size(meta.len())
            )));
            self.win.pending = Some(Pending::OpenLarge(path));
            return;
        }
        let disk = safety::stamp(&path); // before reading, so a change during the read is noticed
        if (meta.len() as usize) < BACKGROUND_BYTES {
            match doc::load(&path) {
                Ok(l) => {
                    let i = if self.tab().is_pristine() { self.win.active } else { self.new_tab() };
                    self.win.tabs[i].disk = disk;
                    self.win.tabs[i].path = Some(path.clone());
                    self.activate(i);
                    self.loaded(i, path, false, Ok(l));
                }
                Err(e) => self.win.status_msg = Some(format!("Could not open {}: {e}", path.display())),
            }
            return;
        }
        let i = if self.tab().is_pristine() { self.win.active } else { self.new_tab() };
        let t = &mut self.win.tabs[i];
        t.disk = disk;
        t.path = Some(path.clone());
        self.activate(i);
        self.load_in_background(i, path, false, meta.len());
    }

    /// Reads tab `i`'s file again (it changed on disk and the tab has no unsaved changes).
    pub(crate) fn reload_tab(&mut self, i: usize) {
        let Some(path) = self.win.tabs[i].path.clone() else { return };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.win.tabs[i].disk = safety::stamp(&path);
        if (size as usize) < BACKGROUND_BYTES {
            let r = doc::load(&path);
            self.loaded(i, path, true, r);
        } else {
            self.load_in_background(i, path, true, size);
        }
        self.win.needs_redraw = true;
    }

    fn load_in_background(&mut self, i: usize, path: PathBuf, reload: bool, size: u64) {
        let id = self.win.tabs[i].id;
        self.win.tabs[i].view.read_only = true;
        let p = path.clone();
        if !self.spawn_job(id, Work::Load { path, reload }, size as usize, move |progress| {
            Done::Loaded(doc::open_regular(&p).and_then(|f| doc::load_from(Counted { inner: f, pos: 0, progress })))
        }) {
            self.win.tabs[i].view.read_only = false;
        }
    }

    /// Fills tab `i` with a loaded file (or reports why it couldn't be read).
    fn loaded(&mut self, i: usize, path: PathBuf, reload: bool, r: io::Result<doc::Loaded>) {
        let active = i == self.win.active;
        match r {
            Ok(l) => {
                let t = &mut self.win.tabs[i];
                let doc = document::Document::from_rope(l.rope);
                if reload {
                    t.view.reload_document(doc);
                } else {
                    t.view.set_document(doc);
                }
                t.encoding = l.encoding;
                t.eol = l.eol;
                t.mixed_eol = l.mixed_eol;
                t.meta_dirty = false;
                t.file_base = true;
                let name = t.name();
                if reload {
                    self.win.status_msg = Some(format!("Reloaded {name} (changed by another program)"));
                } else if active {
                    self.win.status_msg =
                        l.mixed_eol.then(|| format!("Mixed line endings; will be saved as {}", l.eol.label()));
                }
            }
            Err(e) => {
                let what = if reload { "reload" } else { "open" };
                self.win.status_msg = Some(format!("Could not {what} {}: {e}", path.display()));
                if !reload {
                    self.close_tab(i, true); // it never got its text
                }
            }
        }
    }

    // ---------------------------------------------------------------- saving

    /// Saves the active tab to `path`; `then` continues a close or exit once it's done.
    pub(crate) fn save_to(&mut self, path: PathBuf, then: Option<Pending>) -> Saving {
        let i = self.win.active;
        let t = &self.win.tabs[i];
        if self.busy(t.id) {
            self.win.status_msg = Some(format!("{} is still being opened or saved", t.name()));
            return Saving::Failed;
        }
        let (rope, encoding, eol, id) = (t.view.doc.snapshot(), t.encoding, t.eol, t.id);
        if rope.len_bytes() < BACKGROUND_BYTES {
            let out = doc::save_rope(&path, &rope, eol, encoding, &|_| {});
            return if self.saved(i, path, encoding, eol, out) { Saving::Done } else { Saving::Failed };
        }
        self.win.tabs[i].view.read_only = true;
        self.win.status_msg = None;
        let total = rope.len_bytes();
        let p = path.clone();
        if self.spawn_job(id, Work::Save { path, encoding, eol, then }, total, move |progress| {
            Done::Saved(doc::save_rope(&p, &rope, eol, encoding, &|d| progress.set(d)))
        }) {
            Saving::Started
        } else {
            self.win.tabs[i].view.read_only = false;
            Saving::Failed
        }
    }

    /// Records a finished save of tab `i`. Returns whether it succeeded.
    fn saved(&mut self, i: usize, path: PathBuf, encoding: Encoding, eol: Eol, out: doc::SaveOutcome) -> bool {
        let t = &mut self.win.tabs[i];
        match out.result {
            Ok(()) => {
                t.disk = safety::stamp(&path);
                t.path = Some(path);
                t.file_base = true;
                t.recovered_from = None; // it now has a real path; the hint is done
                // The text couldn't change meanwhile (the tab was read-only), but its
                // encoding or line endings could have been switched from the menu.
                t.view.doc.mark_saved();
                if (t.encoding, t.eol) == (encoding, eol) {
                    t.encoding = out.encoding;
                    t.meta_dirty = false;
                    t.mixed_eol = false;
                }
                if let Some(c) = out.fell_back {
                    self.win.status_msg =
                        Some(format!("'{c}' can't be stored as {}; saved as UTF-8 instead", encoding.label()));
                }
                true
            }
            Err(e) => {
                self.win.status_msg = Some(format!("Could not save {}: {e}", t.name()));
                false
            }
        }
    }

    /// Before quitting: waits for saves in progress. Returns false if any are running.
    pub(crate) fn saves_finished(&mut self) -> bool {
        let n = self.jobs.list.iter().filter(|j| matches!(j.work, Work::Save { .. })).count();
        if n > 0 {
            self.jobs.exit_after_saves = true;
            self.win.status_msg = Some(format!("Waiting for {n} save{} to finish…", if n == 1 { "" } else { "s" }));
        }
        n == 0
    }

    // ---------------------------------------------------------------- find and replace

    /// Find Next / Find Previous in the active tab, from the selection.
    pub(crate) fn find_next(&mut self, forward: bool) {
        let needle = self.win.find.find.text.clone();
        if needle.is_empty() {
            return self.open_find(false);
        }
        let i = self.win.active;
        let view = &self.win.tabs[i].view;
        let (start, end) = view.selection().unwrap_or((view.cursor, view.cursor));
        let at = view.doc.offset(if forward { end } else { start });
        let q = search::Query::new(&needle, self.win.find.case);
        if view.doc.rope().len_bytes() < BACKGROUND_BYTES {
            let r = search::find_wrapping(view.doc.rope(), &q, at, forward, &mut |_| true);
            return self.show_found(i, r, &needle);
        }
        let (rope, version, id) = (view.doc.snapshot(), view.doc.version(), self.win.tabs[i].id);
        self.cancel_finds(id);
        let total = rope.len_bytes();
        self.spawn_job(id, Work::Find { version, needle }, total, move |p| {
            Done::Found(search::find_wrapping(&rope, &q, at, forward, &mut |d| {
                p.set(d as u64);
                !p.cancelled()
            }))
        });
    }

    fn show_found(&mut self, i: usize, r: Option<Range<usize>>, needle: &str) {
        match r {
            Some(r) => {
                let view = &mut self.win.tabs[i].view;
                let (a, z) = (view.doc.cursor_at(r.start), view.doc.cursor_at(r.end));
                view.set_selection(a, z);
                if i == self.win.active {
                    self.win.status_msg = None;
                }
            }
            None => self.win.status_msg = Some(format!("Cannot find \"{needle}\"")),
        }
    }

    pub(crate) fn replace_all(&mut self) {
        let needle = self.win.find.find.text.clone();
        let repl = self.win.find.repl.text.clone();
        let i = self.win.active;
        let id = self.win.tabs[i].id;
        if needle.is_empty() || self.busy(id) {
            return;
        }
        let q = search::Query::new(&needle, self.win.find.case);
        let doc = &self.win.tabs[i].view.doc;
        let run = move |rope: &ropey::Rope, more: &mut dyn FnMut(usize) -> bool| {
            let found = search::find_all(rope, &q, more)?;
            (!found.is_empty()).then(|| document::prepare_replace_all(rope, &found, &repl))
        };
        if doc.rope().len_bytes() < BACKGROUND_BYTES {
            let p = run(doc.rope(), &mut |_| true);
            return self.replaced(i, p, &needle);
        }
        let (rope, version) = (doc.snapshot(), doc.version());
        self.cancel_finds(id);
        self.win.tabs[i].view.read_only = true;
        let total = rope.len_bytes();
        if !self.spawn_job(id, Work::ReplaceAll { version, needle }, total, move |p| {
            Done::Replaced(run(&rope, &mut |d| {
                p.set(d as u64);
                !p.cancelled()
            }))
        }) {
            self.win.tabs[i].view.read_only = false;
        }
    }

    fn replaced(&mut self, i: usize, p: Option<document::Prepared>, needle: &str) {
        let Some(p) = p else {
            self.win.status_msg = Some(format!("Cannot find \"{needle}\""));
            return;
        };
        let n = p.count();
        self.win.tabs[i].view.apply_prepared(p);
        crate::mem::trim(); // the old text and the match list
        self.edited();
        self.win.status_msg = Some(format!("Replaced {n} occurrence{}", if n == 1 { "" } else { "s" }));
    }
}
