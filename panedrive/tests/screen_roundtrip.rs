//! Live smoke test for the GNU screen backend: start a detached session running
//! `cat`, type into it via `stuff`, and read the echo back via `hardcopy`.
//! Skips gracefully when screen is not installed so `cargo test` stays green on
//! hosts without it.

use panedrive::{PaneBackend, ScreenBackend, parse_keys};
use std::process::Command;
use std::time::Duration;

fn screen_available() -> bool {
    // `screen --version` exits non-zero but prints the version, so probe with a
    // command that succeeds when screen exists: listing sessions.
    Command::new("screen")
        .arg("-ls")
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains("Socket") || s.contains("No Sockets")
        })
        .unwrap_or(false)
}

#[test]
fn stuff_then_hardcopy_sees_the_echo() {
    if !screen_available() {
        eprintln!("skipping screen_roundtrip: screen not installed");
        return;
    }

    let session = format!("panedrive-it-{}", std::process::id());
    // Run `cat` as the session's program so it echoes exactly what we type,
    // with no shell prompt to race against.
    let started = Command::new("screen")
        .args(["-dmS", &session, "cat"])
        .status()
        .expect("start screen")
        .success();
    assert!(started, "could not start a detached screen session");
    std::thread::sleep(Duration::from_millis(300));

    let backend = ScreenBackend::new(&session);
    backend
        .send_keys(&parse_keys("h e l l o Enter").unwrap())
        .expect("send_keys");

    // Poll the screen until the echo lands rather than racing a fixed sleep, so
    // the test stays green under parallel load.
    let mut screen = String::new();
    for _ in 0..40 {
        screen = backend.capture().unwrap_or_default();
        if screen.contains("hello") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Tear the session down before asserting so a failure never leaks it.
    let _ = Command::new("screen")
        .args(["-S", &session, "-X", "quit"])
        .status();

    assert!(
        screen.contains("hello"),
        "expected the echoed text, screen was: {screen:?}"
    );
}
