//! Keeping your work safe: autosave snapshots for crash recovery, and noticing
//! when an open file is changed on disk by something else.

use super::*;
pub(crate) use doc::{Stamp, stamp};

/// How long after an edit a crash-recovery snapshot is written.
const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);

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

/// A sanitized one-line description of a recovered document for the summary
/// dialog: its original path (if any), flagged when restoring it reads a file.
fn recovered_label(r: &recovery::Recovered) -> String {
    match r.origin() {
        Some(p) => {
            let name = ui::sanitize_label(&p.to_string_lossy());
            if r.reads_file() { format!("{name}   (unsaved edits will be rebuilt from this file)") } else { name }
        }
        None => "an untitled document".to_string(),
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
            if t.is_dirty() {
                continue;
            }
            if t.snap.is_some() {
                rec.remove(t.id);
                self.snapshots.remove(&t.id);
                t.snap = None;
            }
            // Its next snapshot, if any, builds on the saved text again; and
            // edits that were undone needn't stay in memory.
            if t.view.doc.journal_pending() {
                t.view.doc.restart_journal();
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
                let journal = t.view.doc.take_journal();
                // The file can stand in for the saved text, while it holds it.
                let file = t.path.clone().zip(t.disk).filter(|_| t.file_base);
                rec.save(t.id, key.meta.clone(), t.view.doc.snapshot(), journal, file);
                written.push(t.id);
                t.snap = Some(key);
            }
        }
        self.snapshots.extend(written);
    }

    /// Finds documents left unsaved by a crashed instance and shows the summary
    /// dialog. Nothing is opened yet: the documents are materialised only when
    /// the user dismisses the dialog (see [`App::finish_recovery`]), so a
    /// file-based snapshot's file isn't read until its path has been shown, and
    /// every recovered document then opens as an *untitled* tab — never pointed
    /// at a path a snapshot named. A planted snapshot therefore can't make the
    /// editor silently read an arbitrary file or arm a silent write.
    pub(crate) fn restore_recovered(&mut self) {
        let Some(rec) = &self.recovery else { return };
        let (found, lost) = rec.take_orphans();
        if found.is_empty() && lost.is_empty() {
            return;
        }
        let n = found.len();
        let mut lines = vec![
            format!(
                "Recovered {n} unsaved document{} from a session that ended unexpectedly.",
                if n == 1 { "" } else { "s" }
            ),
            "They open as untitled tabs — use Save As to write each one back.".to_string(),
        ];
        lines.extend(found.iter().map(|r| format!("•  {}", recovered_label(r))));
        if !lost.is_empty() {
            lines.push("Could not recover:".to_string());
            lines.extend(lost.iter().map(|l| format!("•  {}", ui::sanitize_label(l))));
        }
        self.recovery_items = found;
        self.win.prompt = Some(Prompt::recovery(lines));
    }

    /// Materialises the recovered documents into untitled, modified tabs once
    /// the user has seen the summary. With `list`, also opens a tab holding the
    /// list of recovered names, as a reference while saving several back.
    pub(crate) fn finish_recovery(&mut self, list: bool) {
        let items = std::mem::take(&mut self.recovery_items);
        if items.is_empty() {
            return;
        }
        let mut names: Vec<(String, Option<String>)> = Vec::new();
        let mut errors = Vec::new();
        for r in items {
            let origin = r.origin().map(|p| ui::sanitize_label(&p.to_string_lossy()));
            // Reads the base file now, if the snapshot needs one — its path was
            // shown in the dialog the user just dismissed.
            match r.restore() {
                Ok((meta, text)) => {
                    let i = if self.tab().is_pristine() { self.win.active } else { self.new_tab() };
                    let t = &mut self.win.tabs[i];
                    t.view.set_document(document::Document::from_rope(text));
                    t.encoding = meta.encoding;
                    t.eol = meta.eol;
                    // Deliberately untitled: no `path`, no `disk` stamp. The content
                    // is marked modified so it prompts to be saved (and gets
                    // re-snapshotted), and a Save routes through Save As.
                    t.meta_dirty = true;
                    t.recovered_from = origin.clone();
                    names.push((t.name(), origin));
                }
                Err(e) => errors.push(e),
            }
        }
        if list && !names.is_empty() {
            let mut body = String::from("Recovered unsaved documents (originally):\n\n");
            for (tab, origin) in &names {
                match origin {
                    Some(p) => body.push_str(&format!("{tab}\twas  {p}\n")),
                    None => body.push_str(&format!("{tab}\twas an untitled document\n")),
                }
            }
            body.push_str("\nEach opened as an untitled tab above; use File \u{2192} Save As to write it back.\n");
            let i = self.new_tab();
            // A plain reference note: unmodified, so it won't prompt to save or
            // get snapshotted, and it has no path.
            self.win.tabs[i].view.set_document(document::Document::new(&body));
        }
        if !errors.is_empty() {
            self.win.status_msg = Some(errors.join(". "));
        }
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
                    t.file_base = false;
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
                        // Keep mine: remember the new disk state so we don't ask again. The
                        // file no longer holds this tab's saved text.
                        self.win.tabs[i].disk = stamp(&p);
                        self.win.tabs[i].file_base = false;
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
