//! End-to-end tests over the PTY backend for the 0.1.4 "stop lying" features:
//! screen-text assertions (`expect-screen` / `expect-no-screen`), and failing a
//! run when the program crashes or its output matches a `--fail-on` pattern.
//! Drives the `exit_tui` example in a real pseudo-terminal. Only built with
//! `--features pty`.

#![cfg(feature = "pty")]

use panedrive::{PtyBackend, RunOptions, RunResult, parse_script, probe_sink, run_script};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The `exit_tui` example binary sits next to the `panedrive` test binary under
/// `examples/`. Skip gracefully if it is somehow absent so this never hard-fails.
fn exit_bin() -> Option<PathBuf> {
    let here = PathBuf::from(env!("CARGO_BIN_EXE_panedrive"));
    let candidate = here.parent()?.join("examples").join("exit_tui");
    candidate.exists().then_some(candidate)
}

/// Wait until the backend's captured screen contains `needle`, so a screen
/// assertion does not race the program's startup render. Returns whether it
/// appeared within the timeout.
fn wait_for_screen(backend: &PtyBackend, needle: &str, timeout: Duration) -> bool {
    use panedrive::PaneBackend;
    let start = Instant::now();
    loop {
        if backend.capture().unwrap_or_default().contains(needle) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn() -> Option<PtyBackend> {
    let bin = exit_bin()?;
    Some(PtyBackend::spawn(&bin.to_string_lossy(), &[], 24, 80).expect("spawn exit_tui"))
}

#[test]
fn expect_screen_matches_the_rendered_banner() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    // The banner must render before we assert on it.
    assert!(
        wait_for_screen(&backend, "1/2/3 day/week/month", Duration::from_secs(3)),
        "the banner never rendered"
    );

    // output_contains must answer honestly in both directions: the banner it
    // printed is present, an absent needle is not. (Pins the `--fail-on` scan.)
    use panedrive::PaneBackend;
    assert!(backend.output_contains("READY banner"));
    assert!(!backend.output_contains("this-string-was-never-printed"));

    // A substring that is on screen passes; one that is absent (and the
    // crash banner, which has not been triggered) is correctly absent.
    let script = "\
        expect-screen 1/2/3 day/week/month\n\
        expect-no-screen panicked\n";
    let steps = parse_script(script).unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new(),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    assert_eq!(outcome, RunResult::Passed);
}

#[test]
fn expect_screen_fails_when_the_text_is_absent() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    assert!(wait_for_screen(
        &backend,
        "READY banner",
        Duration::from_secs(3)
    ));
    let steps = parse_script("expect-screen definitely-not-on-screen").unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new(),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    assert!(
        matches!(outcome, RunResult::Failed(f) if f.kind == "expect-screen"),
        "a missing on-screen substring must fail"
    );
}

#[test]
fn a_crash_after_the_last_step_fails_the_run() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    // The seam probe is absent (None); every scripted step "passes", and the
    // program then crashes (exit 101). The run must still fail — the crash must
    // not read as a pass.
    let script = "\
        type crash\n\
        press Enter\n";
    let steps = parse_script(script).unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new(),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    let RunResult::Failed(f) = outcome else {
        panic!("a non-zero crash must fail the run");
    };
    assert_eq!(f.kind, "child-exit");
}

#[test]
fn fail_on_catches_the_crash_output() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    // With --allow-exit the exit itself is tolerated, isolating the --fail-on
    // output guard: the crash prints "panicked", which must fail the run.
    let script = "\
        type crash\n\
        press Enter\n";
    let steps = parse_script(script).unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new()
            .with_allow_exit(true)
            .with_fail_on("panicked"),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    assert!(
        matches!(outcome, RunResult::Failed(f) if f.kind == "fail-on"),
        "--fail-on must catch the panic output"
    );
}

#[test]
fn a_leftover_seam_file_does_not_cause_a_false_pass() {
    // Regression for the stale-seam false pass (issue #3): a leftover state file
    // from a previous run must not let `wait-until` pass before the freshly
    // spawned program has written anything. Driven through the real CLI so it
    // exercises the pty pre-spawn seam clear in cmd_run. `exit_tui` never writes
    // the seam, so once the leftover is cleared the `wait-until` must time out
    // (exit 1), not pass on stale state (exit 0).
    let Some(bin) = exit_bin() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    let dir = std::env::temp_dir();
    let state = dir.join(format!("panedrive-stale-{}.state.json", std::process::id()));
    let script = dir.join(format!("panedrive-stale-{}.pds", std::process::id()));
    std::fs::write(&state, br#"{"ready":true}"#).unwrap();
    std::fs::write(
        &script,
        b"wait-until ready=true --timeout-ms 400 --interval-ms 20\n",
    )
    .unwrap();

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .arg("run")
        .arg(&script)
        .args(["--backend", "pty", "--state"])
        .arg(&state)
        .arg("--")
        .arg(bin)
        .status()
        .expect("spawn panedrive");

    std::fs::remove_file(&state).ok();
    std::fs::remove_file(&script).ok();
    assert_eq!(
        status.code(),
        Some(1),
        "a leftover seam file must not produce a false pass; wait-until should time out"
    );
}

#[test]
fn a_clean_quit_on_the_last_step_passes() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    // The script deliberately quits the program on its final step; a zero exit
    // at the end is normal and must not fail, even without --allow-exit.
    let script = "\
        type bye\n\
        press Enter\n";
    let steps = parse_script(script).unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new(),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    assert_eq!(
        outcome,
        RunResult::Passed,
        "a clean quit on the last step is a pass"
    );
}

#[test]
fn a_clean_quit_then_a_read_only_check_passes() {
    let Some(backend) = spawn() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    // The quit-app flow: quit the program, then a read-only step checks the
    // final rendered screen. The program exits cleanly (code 0) after the quit,
    // and only a read-only step remains, so this is a clean quit, not an early
    // exit, and the run passes. Regression for the 0.1.4 false-fail where a
    // read-only step after a clean exit was reported as "exited before the
    // script finished".
    assert!(wait_for_screen(
        &backend,
        "READY banner",
        Duration::from_secs(3)
    ));
    let script = "\
        type bye\n\
        press Enter\n\
        expect-no-screen panicked\n";
    let steps = parse_script(script).unwrap();
    let outcome = run_script(
        &steps,
        &backend,
        &RunOptions::new(),
        &mut probe_sink(|| None),
    )
    .expect("run");
    let _ = backend.kill();
    assert_eq!(
        outcome,
        RunResult::Passed,
        "a clean quit followed only by a read-only check must pass"
    );
}

#[test]
fn the_json_summary_carries_schema_and_outcome() {
    // The run summary envelope: a passed run reports schema, outcome, and ok.
    let Some(bin) = exit_bin() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    let dir = std::env::temp_dir();
    let script = dir.join(format!("panedrive-sum-{}.pds", std::process::id()));
    std::fs::write(&script, b"type bye\npress Enter\n").unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .arg("run")
        .arg(&script)
        .args(["--backend", "pty", "--json", "--"])
        .arg(&bin)
        .output()
        .expect("spawn panedrive");
    std::fs::remove_file(&script).ok();

    assert_eq!(out.status.code(), Some(0), "a clean quit passes");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("\"schema\""))
        .expect("a JSON summary line");
    let v: serde_json::Value = serde_json::from_str(line).expect("summary is valid JSON");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["outcome"], "passed");
}

#[test]
fn retry_reruns_on_timeout_and_reports_retried() {
    // A wait-until that can never be satisfied (no seam) times out; with
    // --retry-timeouts 1 the whole run is attempted twice, and the summary
    // reports outcome "retried" with the attempt count. Driven through the CLI
    // so the re-spawn path in cmd_run is exercised.
    let Some(bin) = exit_bin() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    let dir = std::env::temp_dir();
    let script = dir.join(format!("panedrive-retry-{}.pds", std::process::id()));
    std::fs::write(
        &script,
        b"wait-until ready=true --timeout-ms 150 --interval-ms 20\n",
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .arg("run")
        .arg(&script)
        .args(["--backend", "pty", "--json", "--retry-timeouts", "1", "--"])
        .arg(&bin)
        .output()
        .expect("spawn panedrive");
    std::fs::remove_file(&script).ok();

    assert_eq!(
        out.status.code(),
        Some(1),
        "an unsatisfiable wait-until still fails"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("\"schema\""))
        .expect("a JSON summary line");
    let v: serde_json::Value = serde_json::from_str(line).expect("summary is valid JSON");
    assert_eq!(v["outcome"], "retried");
    assert_eq!(v["attempts"], 2, "one retry means two attempts");
    assert_eq!(v["ok"], false);
}

#[test]
fn artifacts_are_written_on_failure() {
    // A crashing run with --artifacts writes a bundle and the summary points at
    // it. The manifest must exist and the JSON must carry the artifacts pointer.
    let Some(bin) = exit_bin() else {
        eprintln!("skipping: exit_tui example not built");
        return;
    };
    let dir = std::env::temp_dir();
    let script = dir.join(format!("panedrive-art-{}.pds", std::process::id()));
    let art_dir = dir.join(format!("panedrive-art-bundle-{}", std::process::id()));
    std::fs::write(&script, b"type crash\npress Enter\n").unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .arg("run")
        .arg(&script)
        .args(["--backend", "pty", "--json", "--artifacts"])
        .arg(&art_dir)
        .arg("--")
        .arg(&bin)
        .output()
        .expect("spawn panedrive");
    std::fs::remove_file(&script).ok();

    assert_eq!(out.status.code(), Some(1), "a crash fails the run");
    assert!(
        art_dir.join("manifest.json").exists(),
        "the artifact manifest must be written on failure"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("\"schema\""))
        .expect("a JSON summary line");
    let v: serde_json::Value = serde_json::from_str(line).expect("summary is valid JSON");
    assert!(
        v.get("artifacts").is_some(),
        "the summary must point at the artifact bundle"
    );
    std::fs::remove_dir_all(&art_dir).ok();
}
