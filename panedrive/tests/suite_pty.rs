//! End-to-end tests for the suite runner (matrix and parameter sweep) over the
//! PTY backend, driving the `counter_tui` example through the `panedrive` CLI.
//! A suite emits one NDJSON result line per cell. Only built with `--features
//! pty`.

#![cfg(feature = "pty")]

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// The `counter_tui` example binary sits next to the test binary under
/// `examples/`; skip gracefully if it was not built.
fn counter_bin() -> Option<PathBuf> {
    let here = PathBuf::from(env!("CARGO_BIN_EXE_panedrive"));
    let candidate = here.parent()?.join("examples").join("counter_tui");
    candidate.exists().then_some(candidate)
}

/// Run the `panedrive` CLI with the given args plus `-- <counter_tui>`, returning
/// the exit code and the parsed NDJSON lines (those carrying a schema).
fn run_suite(args: &[&str], counter: &PathBuf) -> (i32, Vec<Value>) {
    let out = Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .arg("run")
        .args(args)
        .arg("--")
        .arg(counter)
        .output()
        .expect("spawn panedrive");
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("\"schema\""))
        .map(|l| serde_json::from_str::<Value>(l).expect("each line is valid JSON"))
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

#[test]
fn sweep_emits_one_ndjson_line_per_value_and_passes() {
    let Some(counter) = counter_bin() else {
        eprintln!("skipping: counter_tui example not built");
        return;
    };
    let (code, lines) = run_suite(
        &[
            "-e",
            "type inc",
            "-e",
            "press Enter",
            "-e",
            "wait-until count=1 --timeout-ms 2000",
            "--backend",
            "pty",
            "--sweep",
            "SEED=1..3",
            "--json",
        ],
        &counter,
    );
    assert_eq!(code, 0, "every cell passes");
    assert_eq!(lines.len(), 3, "one line per sweep value");
    let mut seeds: Vec<String> = lines
        .iter()
        .map(|l| {
            assert_eq!(l["schema"], 1);
            assert_eq!(l["ok"], true);
            l["id"]["SEED"].as_str().expect("id.SEED").to_string()
        })
        .collect();
    seeds.sort();
    assert_eq!(seeds, vec!["1", "2", "3"]);
}

#[test]
fn matrix_header_expands_into_one_line_per_size() {
    let Some(counter) = counter_bin() else {
        eprintln!("skipping: counter_tui example not built");
        return;
    };
    let (code, lines) = run_suite(
        &[
            "-e",
            "#@size: 24x80, 30x100",
            "-e",
            "type inc",
            "-e",
            "press Enter",
            "-e",
            "wait-until count=1 --timeout-ms 2000",
            "--backend",
            "pty",
            "--json",
        ],
        &counter,
    );
    assert_eq!(code, 0, "both sizes pass");
    assert_eq!(lines.len(), 2, "one line per size cell");
    let mut sizes: Vec<String> = lines
        .iter()
        .map(|l| l["id"]["size"].as_str().expect("id.size").to_string())
        .collect();
    sizes.sort();
    assert_eq!(sizes, vec!["24x80", "30x100"]);
}

#[test]
fn a_failing_cell_makes_the_suite_exit_nonzero() {
    let Some(counter) = counter_bin() else {
        eprintln!("skipping: counter_tui example not built");
        return;
    };
    // count never reaches 2 (only one inc), so every cell times out and fails.
    let (code, lines) = run_suite(
        &[
            "-e",
            "type inc",
            "-e",
            "press Enter",
            "-e",
            "wait-until count=2 --timeout-ms 500",
            "--backend",
            "pty",
            "--sweep",
            "SEED=1..2",
            "--json",
        ],
        &counter,
    );
    assert_eq!(code, 1, "a failing cell fails the suite");
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l["ok"] == false));
}

#[test]
fn a_suite_writes_a_failure_bundle_per_failing_cell() {
    let Some(counter) = counter_bin() else {
        eprintln!("skipping: counter_tui example not built");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "panedrive-suite-art-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir_arg = dir.to_string_lossy().into_owned();
    // count never reaches 2, so both cells fail and each writes a bundle.
    let (code, lines) = run_suite(
        &[
            "-e",
            "type inc",
            "-e",
            "press Enter",
            "-e",
            "wait-until count=2 --timeout-ms 500",
            "--backend",
            "pty",
            "--sweep",
            "SEED=1..2",
            "--artifacts",
            &dir_arg,
            "--json",
        ],
        &counter,
    );
    assert_eq!(code, 1);
    assert_eq!(lines.len(), 2);
    for l in &lines {
        assert_eq!(l["ok"], false);
        let adir = l["artifacts"]["dir"]
            .as_str()
            .expect("a failing cell points at its bundle");
        let p = std::path::Path::new(adir);
        assert!(p.is_dir(), "the bundle dir exists on disk: {adir}");
        assert!(p.starts_with(&dir), "the bundle lives under --artifacts");
        assert!(
            p.join("manifest.json").is_file(),
            "the bundle has a manifest"
        );
    }
    let subdirs = std::fs::read_dir(&dir).expect("artifacts dir").count();
    assert_eq!(subdirs, 2, "one bundle subdirectory per failing cell");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sweep_is_rejected_on_attach_backends() {
    // A suite needs to own and configure a child, which only the pty backend
    // does; on tmux it is a usage error (exit 2) before anything runs.
    let out = Command::new(env!("CARGO_BIN_EXE_panedrive"))
        .args([
            "run",
            "-e",
            "assert ready=true",
            "--backend",
            "tmux",
            "--pane",
            "dummy",
            "--sweep",
            "SEED=1..2",
        ])
        .output()
        .expect("spawn panedrive");
    assert_eq!(out.status.code(), Some(2), "sweep on tmux is a usage error");
}
