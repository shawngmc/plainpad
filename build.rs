//! Records what the binary was built from, for Help → About and `--version`:
//! the git commit and its date, whether the working tree had uncommitted
//! changes, the target and the compiler. There's deliberately no build
//! timestamp, so building the same commit twice gives the same binary.
//! Outside a git checkout (a source tarball) the commit is "unknown".

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    let commit = git(&["rev-parse", "--short=10", "HEAD"]);
    let date = git(&["log", "-1", "--format=%cs"]);
    let dirty = commit.is_some() && git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();

    println!("cargo:rustc-env=PLAINPAD_COMMIT={}", commit.as_deref().unwrap_or("unknown"));
    println!("cargo:rustc-env=PLAINPAD_COMMIT_DATE={}", date.as_deref().unwrap_or(""));
    println!("cargo:rustc-env=PLAINPAD_DIRTY={}", if dirty { "1" } else { "" });
    println!("cargo:rustc-env=PLAINPAD_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rustc-env=PLAINPAD_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());
    println!("cargo:rustc-env=PLAINPAD_RUSTC={rustc}");

    // Run again when the commit or the sources change (a new commit, a checkout,
    // staging, or an edit that makes the tree dirty or clean).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        let dir = Path::new(&dir);
        for f in ["HEAD", "index", "packed-refs"] {
            println!("cargo:rerun-if-changed={}", dir.join(f).display());
        }
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", dir.join(r).display());
        }
    }
}
