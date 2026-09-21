//! End-to-end CLI contract: the `assert` and `wait-until` subcommands must map
//! their result to the documented exit codes (0 held, 1 failed/timed-out) so
//! they work as shell gates. These paths read a JSON state file and need no
//! terminal, so they run everywhere.

use std::fs;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_panedrive"))
}

fn write_state(name: &str, json: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("panedrive-cli-{}-{name}", std::process::id()));
    fs::write(&path, json).unwrap();
    path
}

fn write_script(name: &str, body: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("panedrive-cli-{}-{name}.pds", std::process::id()));
    fs::write(&path, body).unwrap();
    path
}

fn code(mut cmd: Command) -> i32 {
    cmd.status().unwrap().code().unwrap()
}

#[test]
fn assert_exit_codes_follow_the_contract() {
    let state = write_state("assert", r#"{ "focus": "fleet", "bag": { "count": 2 } }"#);

    let mut ok = bin();
    ok.args(["assert", "focus=fleet", "--state"]).arg(&state);
    assert_eq!(code(ok), 0, "true condition should exit 0");

    let mut bad = bin();
    bad.args(["assert", "focus=nope", "--state"]).arg(&state);
    assert_eq!(code(bad), 1, "false condition should exit 1");

    let mut missing = bin();
    missing.args(["assert", "focus", "--state", "/no/such/state.json"]);
    assert_eq!(code(missing), 1, "missing state should exit 1");

    fs::remove_file(&state).ok();
}

#[test]
fn wait_until_returns_on_hold_and_on_timeout() {
    let state = write_state("wait", r#"{ "bag": { "count": 2 } }"#);

    let mut held = bin();
    held.args([
        "wait-until",
        "bag.count=2",
        "--timeout-ms",
        "500",
        "--state",
    ])
    .arg(&state);
    assert_eq!(code(held), 0, "already-true should exit 0 quickly");

    let mut timeout = bin();
    timeout
        .args([
            "wait-until",
            "bag.count=9",
            "--timeout-ms",
            "80",
            "--interval-ms",
            "10",
            "--state",
        ])
        .arg(&state);
    assert_eq!(code(timeout), 1, "never-true should time out with exit 1");

    fs::remove_file(&state).ok();
}

#[test]
fn bad_condition_is_a_usage_error_exit_2() {
    let state = write_state("usage", r#"{ "a": 1 }"#);
    let mut cmd = bin();
    cmd.args(["assert", "", "--state"]).arg(&state); // empty condition
    assert_eq!(code(cmd), 2, "unparseable condition should exit 2");
    fs::remove_file(&state).ok();
}

#[test]
fn run_maps_the_script_result_to_exit_codes() {
    // `assert`/empty-step scripts read the state seam and never contact the
    // backend, so a dummy `--pane` lets us exercise the whole exit contract
    // headlessly, with no tmux pane in sight.
    let state = write_state("run", r#"{ "focus": "fleet" }"#);

    let empty = write_script("empty", "# nothing to do\n");
    let mut pass_empty = bin();
    pass_empty
        .args(["run"])
        .arg(&empty)
        .args(["--backend", "tmux", "--pane", "dummy"]);
    assert_eq!(code(pass_empty), 0, "an empty script passes");

    let holds = write_script("holds", "assert focus=fleet\n");
    let mut pass = bin();
    pass.args(["run"])
        .arg(&holds)
        .args(["--backend", "tmux", "--pane", "dummy", "--state"])
        .arg(&state);
    assert_eq!(code(pass), 0, "a holding assert exits 0");

    let fails = write_script("fails", "assert focus=nope\n");
    let mut fail = bin();
    fail.args(["run"])
        .arg(&fails)
        .args(["--backend", "tmux", "--pane", "dummy", "--state"])
        .arg(&state);
    assert_eq!(code(fail), 1, "a failing assert exits 1");

    let bad = write_script("bad", "frobnicate\n");
    let mut usage = bin();
    usage
        .args(["run"])
        .arg(&bad)
        .args(["--backend", "tmux", "--pane", "dummy"]);
    assert_eq!(code(usage), 2, "an unparseable script is a usage error");

    for f in [&state, &empty, &holds, &fails, &bad] {
        fs::remove_file(f).ok();
    }
}

#[test]
fn run_reports_backend_selection_errors_as_exit_2() {
    let script = write_script("sel", "press Enter\n");

    // No --pane for an attach backend.
    let mut no_pane = bin();
    no_pane
        .args(["run"])
        .arg(&script)
        .args(["--backend", "tmux"]);
    assert_eq!(code(no_pane), 2, "tmux without --pane is a usage error");

    // Without the `pty` feature the pty backend is not compiled in, so
    // selecting it is a usage error. With the feature it actually spawns, so
    // only assert the no-feature contract here.
    if !cfg!(feature = "pty") {
        let mut pty = bin();
        pty.args(["run"])
            .arg(&script)
            .args(["--backend", "pty", "--", "true"]);
        assert_eq!(code(pty), 2, "pty without the feature is a usage error");
    }

    // A missing script file cannot be read.
    let mut missing = bin();
    missing.args([
        "run",
        "/no/such/script.pds",
        "--backend",
        "tmux",
        "--pane",
        "x",
    ]);
    assert_eq!(code(missing), 2, "a missing script is a usage error");

    fs::remove_file(&script).ok();
}

#[test]
fn pty_backend_is_rejected_for_one_shot_commands() {
    // The pty backend spawns a program, so it is only valid via `run`.
    let mut cmd = bin();
    cmd.args(["press", "Enter", "--pane", "x", "--backend", "pty"]);
    assert_eq!(code(cmd), 2, "pty on a one-shot command is a usage error");
}

#[test]
fn type_requires_exactly_one_text_source() {
    // These fail during source resolution, before any backend is touched.
    let mut none = bin();
    none.args(["type", "--pane", "x"]);
    assert_eq!(code(none), 2, "no text source should be a usage error");

    let mut two = bin();
    two.args(["type", "hello", "--stdin", "--pane", "x"]);
    assert_eq!(code(two), 2, "literal + --stdin should be a usage error");

    let mut env_missing = bin();
    env_missing
        .args([
            "type",
            "--from-env",
            "PANEDRIVE_DEFINITELY_UNSET",
            "--pane",
            "x",
        ])
        .env_remove("PANEDRIVE_DEFINITELY_UNSET");
    assert_eq!(
        code(env_missing),
        2,
        "unset env var should be a usage error"
    );
}

/// Run a command, returning its exit code and captured stdout.
fn output(mut cmd: Command) -> (i32, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn assert_json_reports_the_observed_value() {
    let state = write_state("assert-json", r#"{"count":1}"#);

    let mut fail = bin();
    fail.args(["assert", "count=2", "--state"])
        .arg(&state)
        .arg("--json");
    let (c, out) = output(fail);
    assert_eq!(c, 1);
    assert!(out.contains(r#""ok":false"#), "stdout: {out}");
    assert!(out.contains(r#""actual":"1""#), "stdout: {out}");

    let mut pass = bin();
    pass.args(["assert", "count=1", "--state"])
        .arg(&state)
        .arg("--json");
    let (c2, out2) = output(pass);
    assert_eq!(c2, 0);
    assert!(out2.contains(r#""ok":true"#), "stdout: {out2}");

    fs::remove_file(&state).ok();
}

#[test]
fn assert_json_without_state_reports_an_error() {
    let mut cmd = bin();
    cmd.args(["assert", "x=1", "--state", "/no/such/state.json", "--json"]);
    let (c, out) = output(cmd);
    assert_eq!(c, 1);
    assert!(
        out.contains(r#""error":"no readable state""#),
        "stdout: {out}"
    );
}

#[test]
fn wait_until_json_times_out_and_succeeds() {
    let miss = write_state("wait-miss", r#"{"ready":false}"#);
    let mut to = bin();
    to.args(["wait-until", "ready=true", "--state"])
        .arg(&miss)
        .args(["--timeout-ms", "20", "--interval-ms", "5", "--json"]);
    let (c, out) = output(to);
    assert_eq!(c, 1);
    assert!(
        out.contains(r#""ok":false"#) && out.contains(r#""timeout_ms":20"#),
        "stdout: {out}"
    );

    let hit = write_state("wait-hit", r#"{"ready":true}"#);
    let mut ok = bin();
    ok.args(["wait-until", "ready=true", "--state"])
        .arg(&hit)
        .arg("--json");
    let (c2, out2) = output(ok);
    assert_eq!(c2, 0);
    assert!(out2.contains(r#""ok":true"#), "stdout: {out2}");

    fs::remove_file(&miss).ok();
    fs::remove_file(&hit).ok();
}

#[test]
fn state_command_prints_paths_and_pretty_json() {
    let state = write_state("state-cmd", r#"{"focus":"fleet","bag":{"count":2}}"#);

    let mut paths = bin();
    paths.args(["state", "--state"]).arg(&state).arg("--paths");
    let (c, out) = output(paths);
    assert_eq!(c, 0);
    assert!(
        out.contains("bag.count") && out.contains("focus"),
        "stdout: {out}"
    );

    let mut pretty = bin();
    pretty.args(["state", "--state"]).arg(&state);
    let (c2, out2) = output(pretty);
    assert_eq!(c2, 0);
    assert!(out2.contains("\"focus\""), "stdout: {out2}");

    fs::remove_file(&state).ok();
}

#[test]
fn state_defaults_from_the_env_var() {
    let state = write_state("state-env", r#"{"a":1}"#);
    let mut cmd = bin();
    cmd.args(["assert", "a=1", "--json"])
        .env("PANEDRIVE_STATE", &state);
    assert_eq!(code(cmd), 0, "assert should read $PANEDRIVE_STATE");
    fs::remove_file(&state).ok();
}

#[test]
fn validate_seam_accepts_objects_and_rejects_the_rest() {
    let good = write_state("seam-ok", r#"{"focus":"fleet"}"#);
    let mut c1 = bin();
    c1.arg("validate-seam").arg(&good);
    assert_eq!(code(c1), 0);

    let bad = write_state("seam-bad", r#"[1,2,3]"#);
    let mut c2 = bin();
    c2.arg("validate-seam").arg(&bad).arg("--json");
    let (code2, out) = output(c2);
    assert_eq!(code2, 1);
    assert!(out.contains(r#""ok":false"#), "stdout: {out}");

    let junk = write_state("seam-junk", "not json at all");
    let mut c3 = bin();
    c3.arg("validate-seam").arg(&junk);
    assert_eq!(code(c3), 1);

    fs::remove_file(&good).ok();
    fs::remove_file(&bad).ok();
    fs::remove_file(&junk).ok();
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn run_over_tmux_emits_json_events_and_a_cast() {
    if !tmux_available() {
        eprintln!("skipping run_over_tmux_emits_json_events_and_a_cast: tmux not installed");
        return;
    }
    let sess = format!("pd-cli-cast-{}", std::process::id());
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &sess])
        .status();
    let started = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &sess,
            "-x",
            "80",
            "-y",
            "24",
            "cat",
        ])
        .status()
        .unwrap();
    assert!(started.success(), "could not start tmux session");
    std::thread::sleep(std::time::Duration::from_millis(200));

    let script = write_script("tmux-cast", "type hello\npress Enter\nsleep 200ms\n");
    let events = std::env::temp_dir().join(format!("pd-events-{}.jsonl", std::process::id()));
    let cast = std::env::temp_dir().join(format!("pd-cast-{}.cast", std::process::id()));

    let mut cmd = bin();
    cmd.arg("run")
        .arg(&script)
        .args(["--backend", "tmux", "--pane", &sess, "--json", "--events"])
        .arg(&events)
        .arg("--cast")
        .arg(&cast);
    let (c, out) = output(cmd);
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &sess])
        .status();

    assert_eq!(c, 0, "run should pass; stdout: {out}");
    assert!(out.contains(r#""ok":true"#), "stdout: {out}");
    let ev = fs::read_to_string(&events).unwrap();
    assert!(ev.lines().count() >= 3, "expected >=3 events, got: {ev}");
    let cast_txt = fs::read_to_string(&cast).unwrap();
    assert!(
        cast_txt.lines().next().unwrap().contains(r#""version":2"#),
        "cast header: {:?}",
        cast_txt.lines().next()
    );

    fs::remove_file(&script).ok();
    fs::remove_file(&events).ok();
    fs::remove_file(&cast).ok();
}
