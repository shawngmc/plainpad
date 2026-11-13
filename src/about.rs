//! Help → About and `--version`: what this build is, for bug reports.

use super::*;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const COMMIT: &str = env!("PLAINPAD_COMMIT");
const COMMIT_DATE: &str = env!("PLAINPAD_COMMIT_DATE");
const DIRTY: &str = env!("PLAINPAD_DIRTY");
const TARGET: &str = env!("PLAINPAD_TARGET");
const PROFILE: &str = env!("PLAINPAD_PROFILE");
const RUSTC: &str = env!("PLAINPAD_RUSTC");
pub const REPO: &str = "https://github.com/shawngmc/plainpad";

/// "commit 1a2b3c4d5e (2026-11-13)", noting uncommitted changes.
fn commit() -> String {
    let mut s = format!("commit {COMMIT}");
    if !COMMIT_DATE.is_empty() {
        s += &format!(" ({COMMIT_DATE})");
    }
    if !DIRTY.is_empty() {
        s += ", with uncommitted changes";
    }
    s
}

/// One line for `--version`.
pub fn version_line() -> String {
    format!("{APP} {VERSION} ({})", commit())
}

/// The details shown in the dialog (and copied by Copy Details).
pub fn details(display: &str) -> Vec<String> {
    let build = if PROFILE == "release" { String::new() } else { format!(", {PROFILE} build") };
    vec![
        format!("{APP} {VERSION}"),
        format!("Built from {}", commit()),
        format!("For {TARGET}{build}, with {RUSTC}"),
        format!("Running on {display}"),
        REPO.to_owned(),
    ]
}

impl App {
    /// The windowing system in use, as far as it matters for bug reports.
    fn display_name(&self) -> &'static str {
        if self.wl.is_some() {
            "Wayland"
        } else if cfg!(windows) {
            "Windows"
        } else if MAC {
            "macOS"
        } else {
            "X11"
        }
    }

    pub(crate) fn show_about(&mut self) {
        let mut lines = details(self.display_name());
        lines.insert(1, "A plain text editor: fast, small, and nothing else.".to_owned());
        self.win.prompt = Some(Prompt::about(lines));
        self.win.pending = Some(Pending::About);
    }

    /// Copy Details: the build information, for pasting into a bug report.
    pub(crate) fn copy_about(&mut self) {
        let text = details(self.display_name()).join("\n");
        self.set_clipboard(text);
        self.win.status_msg = Some("Copied version details to the clipboard".into());
    }
}
