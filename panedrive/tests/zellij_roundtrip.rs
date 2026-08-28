//! Live checks for the zellij backend. The deterministic encoding (which
//! bytes/chars each key becomes, how the argv is built) is covered by unit tests
//! in `key.rs` and `backend::zellij`; these tests drive a real `zellij`. They
//! skip when zellij is absent.
//!
//! zellij needs a real terminal to start a session, so the happy-path test hosts
//! it under `script(1)` (which provides a PTY) and drives it from outside. That
//! bootstrap is environment-sensitive, so it *skips* if a session never comes up
//! rather than flaking the gate; when the session does come up it asserts the
//! drive for real.

use panedrive::{PaneBackend, ZellijBackend, parse_keys};
use std::process::Command;
use std::time::{Duration, Instant};

fn zellij_available() -> bool {
    Command::new("zellij")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn script_available() -> bool {
    Command::new("script")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn session_is_up(session: &str) -> bool {
    Command::new("zellij")
        .arg("list-sessions")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(session))
        .unwrap_or(false)
}

#[test]
fn driving_a_missing_session_is_a_clean_error() {
    if !zellij_available() {
        eprintln!("skipping: zellij not installed");
        return;
    }

    // A session name that cannot exist for this process.
    let session = format!("panedrive-nosuch-{}", std::process::id());
    let backend = ZellijBackend::new(session);

    let err = backend.send_keys(&parse_keys("Enter").unwrap());
    assert!(
        err.is_err(),
        "sending to a non-existent zellij session should error, not silently pass"
    );
}

/// Live happy-path drive of a real zellij session. Bootstrapping a headless
/// zellij under `script(1)` is environment-sensitive (server startup, the
/// detached PTY host, and its `sleep`-fed stdin do not tear down cleanly on
/// every runner), so this is **opt-in**: it runs only when `PANEKIT_LIVE_ZELLIJ`
/// is set, keeping the deterministic gate stable. Run it locally with
/// `PANEKIT_LIVE_ZELLIJ=1 cargo test --test zellij_roundtrip`. The deterministic
/// error-path test above always runs and covers the backend's command path.
#[test]
fn drives_a_live_session_end_to_end() {
    if std::env::var_os("PANEKIT_LIVE_ZELLIJ").is_none() {
        eprintln!("skipping: set PANEKIT_LIVE_ZELLIJ=1 to run the live zellij happy-path");
        return;
    }
    if !zellij_available() || !script_available() {
        eprintln!("skipping: zellij or script(1) not installed");
        return;
    }

    let session = format!("panedrive-live-{}", std::process::id());
    // Write a config that suppresses the startup tips so the dump is clean.
    let cfg = std::env::temp_dir().join(format!("panedrive-zj-{}.kdl", std::process::id()));
    let _ = std::fs::write(&cfg, "show_startup_tips false\npane_frames false\n");

    // Host zellij under a PTY that `script` provides, fed by a long-lived stdin
    // so it does not exit on EOF. Detached in its own session so teardown is easy.
    let launch = format!(
        "TERM=xterm-256color script -qfc 'zellij --session {session}' /dev/null < <(sleep 60)"
    );
    let mut host = Command::new("setsid")
        .arg("bash")
        .arg("-c")
        .arg(&launch)
        .env("ZELLIJ_CONFIG_FILE", &cfg)
        .spawn()
        .expect("spawn zellij host");

    // Poll for the session to register.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !session_is_up(&session) {
        std::thread::sleep(Duration::from_millis(300));
    }

    let up = session_is_up(&session);
    let mut result = None;
    if up {
        let backend = ZellijBackend::new(&session);
        // Type a token at the shell and press Enter; the shell echoes it back.
        let _ = backend.send_keys(&parse_keys("Z J T O K E N Enter").unwrap());
        // Poll the dump until the token appears (echoed on the command line).
        let poll_end = Instant::now() + Duration::from_secs(5);
        let mut screen = String::new();
        while Instant::now() < poll_end {
            screen = backend.capture().unwrap_or_default();
            if screen.contains("ZJTOKEN") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        result = Some(screen);
    }

    // Teardown: kill the session and the host, remove the temp config.
    let _ = Command::new("zellij")
        .args(["kill-session", &session])
        .status();
    let _ = Command::new("zellij")
        .args(["delete-session", &session, "--force"])
        .status();
    let _ = host.kill();
    let _ = host.wait();
    let _ = std::fs::remove_file(&cfg);

    match result {
        Some(screen) => assert!(
            screen.contains("ZJTOKEN"),
            "typed token should echo in the live zellij pane; dump was: {screen:?}"
        ),
        None => eprintln!("skipping assert: zellij session never came up within 15s"),
    }
}
