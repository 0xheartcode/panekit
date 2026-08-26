//! Full seam+PTY loop with NO tmux: spawn the `counter_tui` example in a
//! pseudo-terminal, drive it through `PtyBackend`, and observe the state via the
//! `paneview` snapshot file. This is the CI shape, spawn, drive, assert, exit.
//!
//! Run:  cargo run -p panedrive --features pty --example pty_drive_counter
//! (Build `counter_tui` first: cargo build -p panedrive --example counter_tui)

use panedrive::{Condition, PaneBackend, PtyBackend, driver, parse_keys};
use std::time::Duration;

fn main() {
    // counter_tui lands next to this example in target/<profile>/examples/.
    let here = std::env::current_exe().expect("current exe");
    let fixture = here.with_file_name("counter_tui");
    assert!(
        fixture.exists(),
        "build the fixture first: cargo build -p panedrive --example counter_tui"
    );

    let state = std::env::temp_dir().join(format!("pty-drive-{}.state.json", std::process::id()));
    let _ = std::fs::remove_file(&state);

    let backend = PtyBackend::spawn(
        fixture.to_str().unwrap(),
        &[state.to_str().unwrap()],
        30,
        100,
    )
    .expect("spawn counter_tui in a PTY");

    let check = |label: &str, spec: &str| {
        let cond = Condition::parse(spec).unwrap();
        let ok = driver::wait_until(
            &cond,
            Duration::from_millis(2000),
            Duration::from_millis(25),
            || driver::read_state_file(&state),
        )
        .is_satisfied();
        println!("  [{}] {label}: {spec}", if ok { "PASS" } else { "FAIL" });
        ok
    };

    let mut all = true;
    all &= check("initial", "count=0");
    backend
        .send_keys(&parse_keys("i n c Enter").unwrap())
        .unwrap();
    all &= check("after inc", "count=1");
    backend
        .send_keys(&parse_keys("i n c Enter").unwrap())
        .unwrap();
    all &= check("after 2nd inc", "count=2");
    backend
        .send_keys(&parse_keys("d e c Enter").unwrap())
        .unwrap();
    all &= check("after dec", "count=1");

    // Prove capture works off the parsed screen too (cat-style echo of input).
    let screen = backend.capture().unwrap();
    let echoed = screen.contains("inc");
    println!(
        "  [{}] screen shows typed input",
        if echoed { "PASS" } else { "FAIL" }
    );
    all &= echoed;

    let _ = std::fs::remove_file(&state);
    println!(
        "{}",
        if all {
            "PTY DRIVE: all passed"
        } else {
            "PTY DRIVE: FAILURES"
        }
    );
    std::process::exit(if all { 0 } else { 1 });
}
