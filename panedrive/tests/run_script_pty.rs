//! End-to-end runner test over the PTY backend: spawn the `counter_tui`
//! example in a pseudo-terminal, drive it with a script, and confirm the state
//! seam moved. Proves the `run` model that PTY needs, one process spawns the
//! program and executes every step, since a PTY-spawned child dies the moment
//! the process exits. Only built with `--features pty`.

#![cfg(feature = "pty")]

use panedrive::{PtyBackend, RunResult, parse_script, read_state_file, run_script};
use std::path::PathBuf;

/// The `counter_tui` example binary sits next to the `panedrive` test binary,
/// under `examples/`. `cargo test` builds examples, but skip gracefully if it
/// is somehow absent so this never hard-fails a run.
fn counter_bin() -> Option<PathBuf> {
    let here = PathBuf::from(env!("CARGO_BIN_EXE_panedrive"));
    let candidate = here.parent()?.join("examples").join("counter_tui");
    candidate.exists().then_some(candidate)
}

#[test]
fn run_drives_a_pty_spawned_tui_and_the_seam_moves() {
    let Some(counter) = counter_bin() else {
        eprintln!("skipping run_script_pty: counter_tui example not built");
        return;
    };
    let state = std::env::temp_dir().join(format!(
        "panedrive-runpty-{}.state.json",
        std::process::id()
    ));
    let state_str = state.to_string_lossy().into_owned();

    // Spawn the counter in a PTY it will write its snapshots from.
    let backend = PtyBackend::spawn(&counter.to_string_lossy(), &[&state_str], 24, 80)
        .expect("spawn counter");

    // Two `inc` lines then wait for the seam to reach count=2.
    let script = "\
        type inc\n\
        press Enter\n\
        type inc\n\
        press Enter\n\
        wait-until count=2 --timeout-ms 3000 --interval-ms 20\n\
        assert last=inc\n";
    let steps = parse_script(script).expect("parse script");

    let outcome =
        run_script(&steps, &backend, || read_state_file(&state), |_| {}).expect("run script");

    // Quit and clean up before asserting so a failure never leaks the file.
    let _ = backend.kill();
    let seen = read_state_file(&state);
    std::fs::remove_file(&state).ok();

    assert_eq!(
        outcome,
        RunResult::Passed,
        "the script should pass; state was {seen:?}"
    );
}
