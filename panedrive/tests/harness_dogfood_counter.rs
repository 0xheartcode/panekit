//! Dogfood: run the **real, on-disk** `examples/counter.pds` script in-process
//! through the [`Harness`], against a model that mirrors the `counter_tui`
//! example's command loop. `tests/run_script_pty.rs` drives the same kind of
//! flow over a real PTY; this proves the harness's headline promise — the same
//! `.pds` script runs identically in-process — using the very file shipped as
//! the example, so the two paths cannot silently diverge.

use panedrive::{Harness, InProcessUi, Key, RunResult};
use paneview::DumpState;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Mirrors `examples/counter_tui.rs`: a line-oriented command loop. Typed
/// characters accumulate into a line; Enter submits it as a command
/// (`inc`/`dec`/`quit`), exactly as the example's `match line.trim()` does.
struct Counter {
    count: i64,
    last: String,
    ready: bool,
    line: String,
}

impl Counter {
    fn new() -> Self {
        Self {
            count: 0,
            last: "start".into(),
            ready: true,
            line: String::new(),
        }
    }

    fn submit(&mut self) {
        match self.line.trim() {
            "inc" => {
                self.count += 1;
                self.last = "inc".into();
            }
            "dec" => {
                self.count -= 1;
                self.last = "dec".into();
            }
            "quit" => {}
            other => self.last = format!("unknown:{other}"),
        }
        self.line.clear();
    }
}

impl DumpState for Counter {
    fn dump_state(&self) -> Value {
        json!({ "count": self.count, "last": self.last, "ready": self.ready })
    }
}

impl InProcessUi for Counter {
    fn apply_key(&mut self, key: &Key) {
        match key {
            Key::Enter => self.submit(),
            Key::Char(c) => self.line.push(*c),
            _ => {}
        }
    }
}

#[test]
fn the_shipped_counter_pds_passes_in_process() {
    // Load the exact script the example ships and the PTY test drives.
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("counter.pds");
    let script = std::fs::read_to_string(&script_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", script_path.display()));

    let mut ui = Harness::new(Counter::new());
    let outcome = ui.run(&script).expect("run counter.pds in-process");

    assert_eq!(
        outcome,
        RunResult::Passed,
        "the shipped counter.pds must pass in-process just as it does over PTY"
    );
    // And the seam ended where the script asserted it would.
    assert_eq!(
        ui.state(),
        json!({ "count": 2, "last": "inc", "ready": true })
    );
}
