//! `paneview`, the **state seam** a terminal UI exposes so an agent (or a test)
//! can read its state as structured JSON instead of scraping the rendered
//! screen.
//!
//! A terminal UI implements [`DumpState`]; the companion `panedrive` crate
//! drives that UI (presses keys, waits, asserts) against the JSON it emits.
//! The seam lives in this tiny, near-zero-dependency crate on purpose: every UI
//! can depend on the shared contract without pulling in the driver's machinery
//! (tmux, PTY, CLI). See `docs/ARCHITECTURE.md`.
//!
//! # Example
//!
//! ```
//! use paneview::{dump_serialize, DumpState};
//! use serde::Serialize;
//! use serde_json::json;
//!
//! #[derive(Serialize)]
//! struct App {
//!     focus: &'static str,
//!     rows: usize,
//! }
//!
//! impl DumpState for App {
//!     fn dump_state(&self) -> serde_json::Value {
//!         // Emit machine-first keys, not a mirror of the screen layout.
//!         dump_serialize(self)
//!     }
//! }
//!
//! let app = App { focus: "fleet", rows: 3 };
//! assert_eq!(app.dump_state(), json!({ "focus": "fleet", "rows": 3 }));
//! ```

use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A terminal UI that can report its current state as a JSON value.
///
/// Implement this on your app/model. The returned value is the single source of
/// truth a driver reads, so keep it **machine-first**: flat, stable keys and
/// enums rendered as strings, rather than a mirror of the on-screen layout.
pub trait DumpState {
    /// Snapshot the current state as a JSON value.
    fn dump_state(&self) -> Value;
}

/// Convenience for the common case: derive the snapshot from any [`Serialize`]
/// type. Returns [`Value::Null`] only if serialization fails, which for a plain
/// data struct cannot happen.
pub fn dump_serialize<T: Serialize>(state: &T) -> Value {
    serde_json::to_value(state).unwrap_or(Value::Null)
}

/// Print a state snapshot as pretty JSON to stdout, wire this to a
/// `--dump-state` flag so a UI can report state without a running screen.
///
/// This is IO-only glue over the same serialization that [`write_snapshot`]
/// uses and its tests cover; stdout is not observable in-process.
pub fn print_snapshot(state: &impl DumpState) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(&state.dump_state())?;
    let mut out = io::stdout().lock();
    out.write_all(&json)?;
    out.write_all(b"\n")
}

/// Write a state snapshot to `path` as pretty JSON, atomically: the bytes land
/// in a sibling `*.tmp` file first, then a rename swaps them into place, so a
/// concurrent reader never sees a half-written snapshot.
pub fn write_snapshot(state: &impl DumpState, path: &Path) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(&state.dump_state())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        focus: &'static str,
        open: bool,
    }

    impl DumpState for Fixture {
        fn dump_state(&self) -> Value {
            json!({ "focus": self.focus, "open": self.open })
        }
    }

    #[test]
    fn dump_state_returns_the_json() {
        let f = Fixture {
            focus: "bag",
            open: true,
        };
        assert_eq!(f.dump_state(), json!({ "focus": "bag", "open": true }));
    }

    #[test]
    fn dump_serialize_maps_a_struct() {
        #[derive(Serialize)]
        struct S {
            n: u8,
        }
        assert_eq!(dump_serialize(&S { n: 7 }), json!({ "n": 7 }));
    }

    #[test]
    fn write_snapshot_roundtrips_and_leaves_no_tmp() {
        let dir = std::env::temp_dir().join(format!("paneview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let f = Fixture {
            focus: "fleet",
            open: false,
        };
        write_snapshot(&f, &path).unwrap();

        let read: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, json!({ "focus": "fleet", "open": false }));

        // The atomic temp file must not survive the rename.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!Path::new(&tmp).exists(), "temp snapshot leaked");

        std::fs::remove_dir_all(&dir).ok();
    }
}
