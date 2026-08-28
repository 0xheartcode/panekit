//! Embed the git short-SHA and commit date into `panedrive --version`, e.g.
//! `panedrive 0.1.1 (a1b2c3d4e5 2026-08-28)`. Falls back to the bare crate
//! version when git is unavailable (for example, a crates.io tarball has no
//! `.git`), so a published build still reports a clean version.

use std::process::Command;

fn main() {
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };

    let sha = git(&["rev-parse", "--short=10", "HEAD"]);
    let date = git(&["show", "-s", "--format=%cd", "--date=short", "HEAD"]);

    let version = match (sha, date) {
        (Some(sha), Some(date)) => format!("{pkg} ({sha} {date})"),
        (Some(sha), None) => format!("{pkg} ({sha})"),
        _ => pkg,
    };
    println!("cargo:rustc-env=PANEDRIVE_VERSION={version}");

    // Refresh the embedded SHA when HEAD moves; harmless if .git is absent.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}
