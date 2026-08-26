//! Live smoke test for the PTY backend, no tmux, no multiplexer. Spawns `cat`
//! in a pseudo-terminal, types into it, and reads the echo back off the parsed
//! screen. Only built with `--features pty`.

#![cfg(feature = "pty")]

use panedrive::{PaneBackend, PtyBackend, parse_keys};
use std::time::Duration;

#[test]
fn pty_spawns_a_program_and_captures_its_echo() {
    let backend = PtyBackend::spawn("cat", &[], 24, 80).expect("spawn cat in a PTY");
    assert!(backend.is_alive(), "cat should be running");

    backend
        .send_keys(&parse_keys("h e l l o Enter").unwrap())
        .expect("send keys to the PTY");

    // Let cat echo the line back; the reader thread folds it into the screen.
    std::thread::sleep(Duration::from_millis(250));
    let screen = backend.capture().expect("capture the screen");

    assert!(
        screen.contains("hello"),
        "expected the typed line on screen, got: {screen:?}"
    );
    // Dropping the backend kills cat.
}
