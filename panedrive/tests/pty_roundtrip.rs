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

#[test]
fn a_cleanly_exited_tui_keeps_its_last_frame_for_capture() {
    // The shape of a TUI quitting: enter the alternate screen, draw a frame, then
    // leave the alternate screen (restoring the blank primary) and exit. After it
    // exits the live screen is blank, but capture must still return the final
    // frame so a post-exit expect-screen can assert the UI's last state.
    // The draw and the teardown must land in separate reads (as they do for a
    // real TUI, where a keypress round-trip separates the last render from the
    // quit), so the reader snapshots the non-blank frame before the primary
    // screen is restored. The sleep guarantees that split.
    let backend = PtyBackend::spawn(
        "sh",
        &[
            "-c",
            "printf '\\033[?1049hLAST-FRAME-XYZ'; sleep 0.3; printf '\\033[?1049l'",
        ],
        24,
        80,
    )
    .expect("spawn the alt-screen program");

    // Wait for the child to exit and the reader thread to drain its output.
    for _ in 0..40 {
        if !backend.is_alive() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(150));
    assert!(!backend.is_alive(), "the program should have exited");

    let screen = backend.capture().expect("capture after exit");
    assert!(
        screen.contains("LAST-FRAME-XYZ"),
        "post-exit capture should retain the last rendered frame, got: {screen:?}"
    );
}

#[test]
fn a_running_childs_blank_screen_is_not_masked_by_the_retained_frame() {
    // Draw a frame (so a non-blank frame is retained), then clear the screen to
    // blank while STILL running. The post-exit fallback is gated on the child
    // having exited, so a live blank screen must read as blank, not as the
    // retained frame. This pins that the fallback needs *both* blank and exited.
    let backend = PtyBackend::spawn(
        "sh",
        &[
            "-c",
            "printf 'XYZ-VISIBLE'; sleep 0.3; printf '\\033[2J\\033[H'; sleep 5",
        ],
        24,
        80,
    )
    .expect("spawn the draw-then-clear program");

    // Past the draw and the clear, while the child is still sleeping.
    std::thread::sleep(Duration::from_millis(700));
    assert!(backend.is_alive(), "the program should still be running");
    let screen = backend.capture().expect("capture while running");
    assert!(
        screen.trim().is_empty(),
        "a live blank screen must not be masked by the retained frame, got: {screen:?}"
    );
    // Dropping the backend kills it.
}
