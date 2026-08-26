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
