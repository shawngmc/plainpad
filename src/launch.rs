//! Opening links (and, for printing, a PDF) with the system's default handler.
//!
//! Uses the platform's own mechanism directly: `ShellExecuteW` on Windows,
//! `/usr/bin/open` on macOS and `xdg-open` elsewhere. Arguments are passed as
//! a single argv entry, never through a shell.

use std::ffi::OsStr;
use std::path::Path;

/// Schemes Ctrl+click may open. Anything else (`file:`, `javascript:`,
/// custom app protocols, ...) is refused.
const ALLOWED: [&str; 4] = ["http://", "https://", "ftp://", "mailto:"];

pub fn is_openable_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    ALLOWED.iter().any(|p| lower.starts_with(p)) && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub fn open_url(url: &str) -> Result<(), String> {
    if !is_openable_url(url) {
        return Err(format!("Not a web or mail link: {url}"));
    }
    open(OsStr::new(url))
}

#[cfg_attr(windows, allow(dead_code))]
pub fn open_file(path: &Path) -> Result<(), String> {
    open(path.as_os_str())
}

#[cfg(windows)]
fn open(target: &OsStr) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = target.encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value greater than 32 on success.
    if r as isize > 32 { Ok(()) } else { Err(format!("Could not open {}", target.to_string_lossy())) }
}

#[cfg(not(windows))]
fn open(target: &OsStr) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") { "/usr/bin/open" } else { "xdg-open" };
    let mut child = std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not run {program}: {e}"))?;
    // Reap the launcher when it exits so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links() {
        assert!(is_openable_url("https://example.com/a?b"));
        assert!(is_openable_url("HTTP://EXAMPLE.COM"));
        assert!(is_openable_url("mailto:a@b.c"));
        assert!(!is_openable_url("file:///etc/passwd"));
        assert!(!is_openable_url("javascript:alert(1)"));
        assert!(!is_openable_url(r"C:\Windows\System32\calc.exe"));
        assert!(!is_openable_url("https://a.com\nrm"));
        assert!(!is_openable_url("-https://x"));
    }
}
