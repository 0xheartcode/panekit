//! A minimal interactive fixture for exercising the *full* panekit loop by
//! hand: read a command line, mutate state, emit a `paneview` snapshot. This is
//! NOT part of the library, it stands in for a real TUI so `panedrive` can
//! drive something that actually changes state.
//!
//! Run it inside a tmux pane:
//!   cargo run -p panedrive --example counter_tui -- /tmp/counter.state.json
//! Then from another shell:
//!   panedrive press inc Enter        --pane <pane>
//!   panedrive wait-until count=1     --state /tmp/counter.state.json
//!
//! Commands (one per line): `inc`, `dec`, `quit`.

use paneview::{DumpState, write_snapshot};
use serde_json::{Value, json};
use std::io::BufRead;
use std::path::PathBuf;

struct Counter {
    count: i64,
    last: String,
    ready: bool,
}

impl DumpState for Counter {
    fn dump_state(&self) -> Value {
        json!({ "count": self.count, "last": self.last, "ready": self.ready })
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("counter.state.json"));

    let mut c = Counter {
        count: 0,
        last: "start".to_string(),
        ready: true,
    };
    write_snapshot(&c, &path).expect("write initial snapshot");

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "inc" => {
                c.count += 1;
                c.last = "inc".to_string();
            }
            "dec" => {
                c.count -= 1;
                c.last = "dec".to_string();
            }
            "quit" => break,
            other => c.last = format!("unknown:{other}"),
        }
        write_snapshot(&c, &path).expect("write snapshot");
    }
}
