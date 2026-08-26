//! Live smoke test for the tmux backend: spawn a detached pane running `cat`,
//! type into it, and read the echo back. Skips gracefully when tmux is not
//! installed so `cargo test` stays green on hosts without it.

use panedrive::{PaneBackend, TmuxBackend, parse_keys};
use std::process::Command;
use std::time::Duration;

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn send_keys_then_capture_sees_the_echo() {
    if !tmux_available() {
        eprintln!("skipping tmux_roundtrip: tmux not installed");
        return;
    }

    let session = format!("panedrive-it-{}", std::process::id());
    // Run `cat` as the pane's process so the pane echoes exactly what we type,
    // with no shell prompt noise to match against. `-P -F '#{pane_id}'` prints
    // the real pane id, so we don't assume a window/pane base-index.
    let out = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "24",
            "cat",
        ])
        .output()
        .expect("spawn tmux");
    assert!(out.status.success(), "could not start tmux session");
    let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(pane.starts_with('%'), "unexpected pane id: {pane:?}");

    let backend = TmuxBackend::new(pane);
    let keys = parse_keys("h e l l o Enter").unwrap();
    let send = backend.send_keys(&keys);

    // Give cat a moment to echo the line back into the pane.
    std::thread::sleep(Duration::from_millis(200));
    let captured = backend.capture();

    // Tear the session down before asserting, so a failure never leaks it.
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();

    send.expect("send_keys");
    let screen = captured.expect("capture");
    assert!(
        screen.contains("hello"),
        "expected echoed text, screen was: {screen:?}"
    );
}

#[test]
fn type_from_env_via_paste_reaches_the_pane_without_argv() {
    if !tmux_available() {
        eprintln!("skipping type_from_env_via_paste: tmux not installed");
        return;
    }

    let session = format!("panedrive-paste-{}", std::process::id());
    let out = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "24",
            "cat",
        ])
        .output()
        .expect("spawn tmux");
    assert!(out.status.success(), "could not start tmux session");
    let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // The secret lives only in the env var, never on the command line, and the
    // --paste transport routes it through a tmux buffer, not send-keys argv.
    let status = Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .env("PANEDRIVE_SECRET", "hunter2xyz")
        .args([
            "type",
            "--from-env",
            "PANEDRIVE_SECRET",
            "--paste",
            "--pane",
            &pane,
        ])
        .status()
        .expect("run panedrive");

    std::thread::sleep(Duration::from_millis(250));
    let captured = TmuxBackend::new(pane).capture();
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();

    assert!(status.success(), "panedrive type --paste should succeed");
    assert!(
        captured.expect("capture").contains("hunter2xyz"),
        "the pasted secret should reach the pane"
    );
}
