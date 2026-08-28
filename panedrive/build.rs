//! Embed the git short-SHA and commit timestamp into `panedrive --version`, e.g.
//! `panedrive 0.1.1 (a1b2c3d4e5 2026-08-28T09:39:01Z)`. The timestamp is the
//! commit time (not the build time), so the string stays deterministic. Falls
//! back to the bare crate version when git is unavailable (for example, a
//! crates.io tarball has no `.git`), so a published build still reports cleanly.

use std::process::Command;

fn main() {
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();

    let git = |args: &[&str]| -> Option<String> {
        // TZ=UTC so `format-local` renders the commit time in UTC for the `Z`.
        let out = Command::new("git").env("TZ", "UTC").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };

    let sha = git(&["rev-parse", "--short=10", "HEAD"]);
    let date = git(&[
        "show",
        "-s",
        "--format=%cd",
        "--date=format-local:%Y-%m-%dT%H:%M:%SZ",
        "HEAD",
    ]);

    let version = match (sha, date) {
        (Some(sha), Some(date)) => format!("{pkg} ({sha} {date})"),
        (Some(sha), None) => format!("{pkg} ({sha})"),
        _ => pkg,
    };
    println!("cargo:rustc-env=PANEDRIVE_VERSION={version}");

    // Refresh the embedded SHA when the checked-out commit changes. On a branch,
    // `.git/HEAD` is a `ref:` pointer that stays put across commits, so also
    // watch the branch ref file it points at. All harmless if `.git` is absent.
    let git_dir = std::path::Path::new("../.git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(reference) = head.strip_prefix("ref:").map(str::trim) {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
