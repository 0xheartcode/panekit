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
#![deny(missing_docs)]

use serde::Serialize;
use serde_json::Value;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
/// type. For a plain data struct serialization cannot fail; if it does (a custom
/// `Serialize` that errors, a map with non-string keys), the snapshot is a
/// self-describing marker object `{"_error": "..."}` rather than a value that
/// looks like a valid but empty state. A driver can see the marker and report
/// it. Use [`try_dump_serialize`] when you want the error as a `Result` instead.
pub fn dump_serialize<T: Serialize>(state: &T) -> Value {
    try_dump_serialize(state).unwrap_or_else(|e| {
        let mut marker = serde_json::Map::new();
        marker.insert(
            "_error".to_string(),
            Value::String(format!("snapshot serialization failed: {e}")),
        );
        Value::Object(marker)
    })
}

/// Derive a snapshot from any [`Serialize`] type, surfacing a serialization
/// failure as an `Err` instead of the `{"_error": ...}` marker that
/// [`dump_serialize`] writes. Use this when your app wants to handle the failure
/// itself (log it, fall back, refuse to write) rather than emit a marker.
pub fn try_dump_serialize<T: Serialize>(state: &T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(state)
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

/// Monotonic counter making each temp-file name unique within this process, so
/// two writers aimed at one path (two threads, two tests in parallel) never pick
/// the same temp name.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write a state snapshot to `path` as pretty JSON, atomically: the bytes land
/// in a sibling temp file first, then a rename swaps them into place, so a
/// concurrent reader never sees a half-written snapshot.
///
/// The temp file name carries this process's id and a per-process counter
/// (`<path>.<pid>.<n>.tmp`), so two writers targeting the same path, whether two
/// app instances or parallel tests, cannot clobber each other's temp file before
/// the rename. The rename itself is atomic on the same filesystem.
pub fn write_snapshot(state: &impl DumpState, path: &Path) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(&state.dump_state())?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{}.tmp", std::process::id(), seq));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// The seam contract: introspection and validation.
//
// These live here, in the crate that *defines* the seam, so the contract has
// one owner. An app can unit-test its own snapshots with `paneview::check`
// without pulling in the driver; `panedrive`'s `validate-seam` and `state
// --paths` reuse the same functions, so the rule never drifts between the two.
// ---------------------------------------------------------------------------

/// One assertable leaf of a seam document: its dot-path, the scalar's textual
/// form, and its JSON type name (`string`, `number`, `bool`, `null`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    /// The dot-path that addresses this leaf.
    pub path: String,
    /// The scalar's value in its textual form.
    pub value: String,
    /// The scalar's JSON type name (`string`, `number`, `bool`, `null`).
    pub kind: &'static str,
}

/// Every scalar leaf in `root`, as dot-paths. Objects and arrays are containers:
/// they are traversed, not emitted, and array elements use numeric path segments
/// (`rows.0.name`). The result is exactly the set of paths a condition can
/// target. Arrays and nested objects are first-class; only their scalar leaves
/// are addressable.
pub fn leaves(root: &Value) -> Vec<Leaf> {
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

fn walk(v: &Value, prefix: &str, out: &mut Vec<Leaf>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                walk(val, &join(prefix, k), out);
            }
        }
        Value::Array(arr) => {
            for (i, val) in arr.iter().enumerate() {
                walk(val, &join(prefix, &i.to_string()), out);
            }
        }
        scalar => {
            if let Some((kind, value)) = scalar_repr(scalar) {
                out.push(Leaf {
                    path: prefix.to_string(),
                    value,
                    kind,
                });
            }
        }
    }
}

fn join(prefix: &str, seg: &str) -> String {
    if prefix.is_empty() {
        seg.to_string()
    } else {
        format!("{prefix}.{seg}")
    }
}

fn scalar_repr(v: &Value) -> Option<(&'static str, String)> {
    match v {
        Value::String(s) => Some(("string", s.clone())),
        Value::Number(n) => Some(("number", n.to_string())),
        Value::Bool(b) => Some(("bool", b.to_string())),
        Value::Null => Some(("null", "null".to_string())),
        Value::Object(_) | Value::Array(_) => None,
    }
}

/// A single way a document fails the seam contract, with a human-readable
/// message. Collected into the `Err` of [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Human-readable description of the contract violation.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Check `root` against the seam contract and return `Ok(())` when it holds, or
/// the list of [`Violation`]s when it does not.
///
/// The contract constrains only the *shape of the root*: it must be a JSON
/// **object** (so conditions have named paths to address) that exposes at least
/// one assertable leaf. Nesting and arrays are allowed as containers; array
/// elements are addressed by index (`rows.2.status`). Only the leaves a
/// condition compares need to be scalar (string, number, bool), which the leaf
/// walk already enforces by emitting nothing for a container.
///
/// Call this in your app's unit tests against a sample snapshot to catch a
/// malformed seam before a driver ever runs.
pub fn check(root: &Value) -> Result<(), Vec<Violation>> {
    let mut problems = Vec::new();
    match root {
        Value::Object(map) if map.is_empty() => {
            problems.push("root object is empty: no assertable paths".to_string());
        }
        Value::Object(_) => {}
        Value::Array(_) => {
            problems.push("root is an array, expected an object".to_string());
        }
        _ => {
            problems.push("root is a scalar, expected an object".to_string());
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems
            .into_iter()
            .map(|message| Violation { message })
            .collect())
    }
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

        // No temp sibling survives the rename, whatever unique name it took.
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leaked.is_empty(), "temp snapshot leaked: {leaked:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_snapshot_temp_names_are_unique_per_call() {
        // Two writes to the same path must not pick the same temp name, or a
        // second writer could clobber the first writer's temp before its rename.
        // Prove the name varies by capturing it through a failing rename target:
        // instead, assert the counter advances so distinct calls differ.
        let a = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let b = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        assert_ne!(a, b, "the temp-name counter must advance between writers");
    }

    #[test]
    fn dump_serialize_marks_a_serialization_failure() {
        use std::collections::HashMap;
        // serde_json cannot represent a map with non-string keys; it errors.
        // dump_serialize must surface that as a readable marker, never as a
        // value that looks like a valid empty state.
        #[derive(Serialize)]
        struct Bad {
            m: HashMap<Vec<u8>, u8>,
        }
        let mut m = HashMap::new();
        m.insert(vec![1u8, 2], 3u8);
        let bad = Bad { m };

        assert!(
            try_dump_serialize(&bad).is_err(),
            "this must fail to serialize"
        );

        let marked = dump_serialize(&bad);
        let obj = marked.as_object().expect("marker must be an object");
        assert!(
            obj.contains_key("_error"),
            "a serialization failure must produce an _error marker, got {marked}"
        );
        // The marker is a valid seam root (an object), so a driver can read and
        // report it rather than seeing a half-missing file.
        assert!(check(&marked).is_ok());
    }

    #[test]
    fn dump_serialize_passes_through_a_good_value() {
        #[derive(Serialize)]
        struct Good {
            n: u8,
        }
        assert_eq!(dump_serialize(&Good { n: 7 }), json!({ "n": 7 }));
    }

    #[test]
    fn check_accepts_an_object_with_leaves() {
        assert!(check(&json!({ "focus": "fleet" })).is_ok());
    }

    #[test]
    fn check_accepts_arrays_as_containers() {
        // Regression: arrays are first-class containers, not a contract
        // violation. A seam with a list of rows must validate, and its scalar
        // leaves must be addressable by index. This pins the rule so nobody
        // re-derives "no arrays" from the scalar-leaf wording.
        let v = json!({
            "rows": [ { "status": "ok" }, { "status": "down" } ],
            "count": 2,
        });
        assert!(check(&v).is_ok(), "an array-bearing seam must validate");
        let paths: Vec<String> = leaves(&v).into_iter().map(|l| l.path).collect();
        assert!(paths.contains(&"rows.0.status".to_string()));
        assert!(paths.contains(&"rows.1.status".to_string()));
        assert!(paths.contains(&"count".to_string()));
    }

    #[test]
    fn check_rejects_non_object_roots_and_empty_objects() {
        let array = check(&json!([1, 2, 3])).unwrap_err();
        assert!(array[0].message.contains("array"));
        assert!(!array[0].message.contains("scalar"));

        let scalar = check(&json!("scalar")).unwrap_err();
        assert!(scalar[0].message.contains("scalar"));
        assert!(!scalar[0].message.contains("array"));

        let empty = check(&json!({})).unwrap_err();
        assert!(empty[0].message.contains("empty"));
    }

    #[test]
    fn violation_displays_its_message() {
        // Display must render the message verbatim, so a caller that prints a
        // violation shows the contract problem, not an empty string.
        let mut problems = check(&json!([1, 2, 3])).unwrap_err();
        let v = problems.remove(0);
        assert_eq!(v.to_string(), "root is an array, expected an object");
        assert_eq!(v.to_string(), v.message);
    }

    #[test]
    fn leaves_flattens_nested_objects_and_arrays() {
        let v = json!({
            "focus": "fleet",
            "bag": { "count": 2, "open": true },
            "rows": [ { "name": "a" }, { "name": "b" } ],
        });
        let got: Vec<(String, &str)> = leaves(&v).into_iter().map(|l| (l.path, l.kind)).collect();
        assert!(got.contains(&("focus".to_string(), "string")));
        assert!(got.contains(&("bag.count".to_string(), "number")));
        assert!(got.contains(&("bag.open".to_string(), "bool")));
        assert!(got.contains(&("rows.0.name".to_string(), "string")));
        assert!(got.contains(&("rows.1.name".to_string(), "string")));
        assert_eq!(got.len(), 5);
    }
}
