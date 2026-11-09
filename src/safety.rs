//! Keeping your work safe: autosave snapshots for crash recovery, and noticing
//! when an open file is changed on disk by something else.

use super::*;
use std::time::SystemTime;

/// How long after an edit a crash-recovery snapshot is written.
const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);

/// What we last knew about a file on disk: (modified time, size).
pub(crate) type Stamp = (SystemTime, u64);

pub(crate) fn stamp(path: &std::path::Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// What a tab's snapshot reflects, so we know when it's out of date.
#[derive(Clone, PartialEq)]
pub(crate) struct SnapKey {
    version: u64,
    meta: recovery::Meta,
}

fn snap_key(t: &Tab) -> SnapKey {
    SnapKey {
        version: t.view.doc.version(),
        meta: recovery::Meta { path: t.path.clone(), encoding: t.encoding, eol: t.eol },
    }
}

impl App {
    fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        std::iter::once(&self.win).chain(self.others.iter()).flat_map(|w| w.tabs.iter())
    }

    /// Called after every event: deletes snapshots that are no longer wanted
    /// straight away (a tab saved, reverted or closed), and arms the autosave
    /// timer if some modified tab's snapshot is out of date.
    pub(crate) fn autosave_check(&mut self) {
        let Some(rec) = &self.recovery else { return };
        // Tabs that are clean again: their snapshot goes now.
        for t in std::iter::once(&mut self.win).chain(self.others.iter_mut()).flat_map(|w| w.tabs.iter_mut()) {
            if t.snap.is_some() && !t.is_dirty() {
                rec.remove(t.id);
                self.snapshots.remove(&t.id);
                t.snap = None;
            }
        }
        // Tabs that were closed: likewise, so a crash can't bring back discarded text.
        let live: std::collections::HashSet<u64> = self.all_tabs().map(|t| t.id).collect();
        let gone: Vec<u64> = self.snapshots.iter().copied().filter(|id| !live.contains(id)).collect();
        for id in gone {
            rec.remove(id);
            self.snapshots.remove(&id);
        }
        let stale = self.all_tabs().any(|t| t.is_dirty() && t.snap.as_ref() != Some(&snap_key(t)));
        if stale && self.autosave_at.is_none() {
            self.autosave_at = Some(Instant::now() + AUTOSAVE_DELAY);
        }
    }

    /// The autosave timer fired: snapshot every modified tab that changed since
    /// its last snapshot. Writing happens on the recovery thread.
    pub(crate) fn autosave_now(&mut self) {
        self.autosave_at = None;
        let Some(rec) = self.recovery.as_ref() else { return };
        let mut written = Vec::new();
        for t in std::iter::once(&mut self.win).chain(self.others.iter_mut()).flat_map(|w| w.tabs.iter_mut()) {
            if !t.is_dirty() {
                continue;
            }
            let key = snap_key(t);
            if t.snap.as_ref() != Some(&key) {
                rec.save(t.id, key.meta.clone(), t.view.doc.snapshot());
                written.push(t.id);
                t.snap = Some(key);
            }
        }
        self.snapshots.extend(written);
    }

    /// Reopens documents left unsaved by an instance that crashed.
    pub(crate) fn restore_recovered(&mut self) {
        let Some(rec) = &self.recovery else { return };
        let found = rec.take_orphans();
        if found.is_empty() {
            return;
        }
        let n = found.len();
        for r in found {
            let i = if self.tab().is_pristine() { self.win.active } else { self.new_tab() };
            let t = &mut self.win.tabs[i];
            t.view.set_document(document::Document::new(&r.text));
            t.encoding = r.meta.encoding;
            t.eol = r.meta.eol;
            t.disk = r.meta.path.as_deref().and_then(stamp);
            t.path = r.meta.path;
            // Not saved anywhere yet: show it as modified so it gets saved (and re-snapshotted).
            t.meta_dirty = true;
        }
        self.win.status_msg = Some(format!(
            "Recovered {n} unsaved document{} from a session that ended unexpectedly",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Checks this window's tabs for files changed or deleted on disk by
    /// something else. Unmodified tabs reload silently; for a modified one, asks
    /// (one prompt at a time; answering it checks again).
    pub(crate) fn check_disk(&mut self) {
        if self.win.modal() {
            return;
        }
        for i in 0..self.win.tabs.len() {
            let t = &self.win.tabs[i];
            let (Some(path), Some(known)) = (t.path.clone(), t.disk) else { continue };
            if self.busy(t.id) {
                continue; // being read or written by us right now
            }
            match stamp(&path) {
                Some(now) if now == known => {}
                Some(_) => {
                    if !self.win.tabs[i].is_dirty() {
                        self.reload_tab(i);
                    } else {
                        self.activate(i);
                        let name = self.doc_name();
                        self.win.prompt = Some(Prompt::changed(format!(
                            "{name} has been changed by another program. Reload it and lose your changes, or keep your version?"
                        )));
                        self.win.pending = Some(Pending::Reload(self.win.tabs[i].id));
                        return;
                    }
                }
                None if !path.exists() => {
                    let t = &mut self.win.tabs[i];
                    t.disk = None;
                    t.meta_dirty = true; // saving will recreate it
                    self.win.status_msg = Some(format!(
                        "{} was deleted or moved by another program; save to recreate it",
                        self.win.tabs[i].name()
                    ));
                }
                None => {}
            }
        }
    }

    /// Before saving over the tab's file: if it changed on disk since we loaded
    /// or saved it, ask first. Returns true if saving may go ahead now.
    pub(crate) fn confirm_overwrite(&mut self) -> bool {
        let t = &self.win.tabs[self.win.active];
        let (Some(path), Some(known)) = (&t.path, t.disk) else { return true };
        match stamp(path) {
            Some(now) if now != known => {
                let id = t.id;
                let name = t.name();
                self.win.prompt = Some(Prompt::overwrite(format!(
                    "{name} has been changed by another program since you opened it. Overwrite it with your version?"
                )));
                self.win.pending = Some(Pending::Overwrite(id));
                false
            }
            _ => true,
        }
    }

    /// Handles the answer to a reload or overwrite prompt. Returns false if the
    /// pending action was something else.
    pub(crate) fn choose_disk(&mut self, c: Choice, pending: &Pending) -> bool {
        match pending {
            Pending::Reload(id) => {
                if let Some(i) = self.win.tab_pos(*id) {
                    if c == Choice::Reload {
                        self.reload_tab(i);
                    } else if let Some(p) = self.win.tabs[i].path.clone() {
                        // Keep mine: remember the new disk state so we don't ask again.
                        self.win.tabs[i].disk = stamp(&p);
                    }
                }
                self.check_disk();
                true
            }
            Pending::Overwrite(id) => {
                if c == Choice::Overwrite
                    && let Some(i) = self.win.tab_pos(*id)
                {
                    self.activate(i);
                    if let Some(p) = self.win.tabs[i].path.clone() {
                        self.win.tabs[i].disk = stamp(&p);
                    }
                    self.save(None);
                }
                true
            }
            _ => false,
        }
    }
}
