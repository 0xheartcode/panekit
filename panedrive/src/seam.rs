//! Introspection over a state-seam JSON document.
//!
//! Two jobs, both pure functions over a [`serde_json::Value`]:
//!
//! - [`leaves`] enumerates every *assertable* dot-path (what `state --paths`
//!   prints), so you can see the surface a [`Condition`](crate::condition)
//!   addresses before writing one.
//! - [`validate`] checks a document against the seam contract, an object with
//!   scalar leaves, what `validate-seam` reports when bringing up a new
//!   (often non-Rust) adapter.

use serde_json::Value;

/// One assertable leaf: its dot-path, the scalar's textual form, and its JSON
/// type name (`string`, `number`, `bool`, `null`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub path: String,
    pub value: String,
    pub kind: &'static str,
}

/// Every scalar leaf in `root`, as dot-paths. Array elements use numeric
/// segments (`rows.0.name`); objects and arrays are traversed, not emitted, so
/// the result is exactly the set of paths a condition can target.
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

/// The result of checking a document against the seam contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeamReport {
    /// True when there are no problems.
    pub ok: bool,
    /// Human-readable contract violations (empty when `ok`).
    pub problems: Vec<String>,
    /// How many assertable paths the document exposes.
    pub paths: usize,
}

/// Check `root` against the seam contract: it must be a JSON **object** (so
/// conditions have named paths to address) and expose at least one assertable
/// leaf. Nesting and arrays are fine; only the shape of the root is constrained.
pub fn validate(root: &Value) -> SeamReport {
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
    SeamReport {
        ok: problems.is_empty(),
        problems,
        paths: leaves(root).len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // objects/arrays themselves are not emitted, only their scalar leaves
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn validate_accepts_an_object_with_leaves() {
        let report = validate(&json!({ "focus": "fleet" }));
        assert!(report.ok);
        assert_eq!(report.paths, 1);
        assert!(report.problems.is_empty());
    }

    #[test]
    fn validate_rejects_non_object_roots_and_empty_objects() {
        // An array root and a scalar root are both rejected, but with *distinct*
        // messages, so the array arm cannot be deleted without a test noticing.
        let array = validate(&json!([1, 2, 3]));
        assert!(!array.ok);
        assert!(
            array.problems[0].contains("array"),
            "array root problem should name the array shape, got {:?}",
            array.problems[0]
        );
        assert!(
            !array.problems[0].contains("scalar"),
            "array root must not be reported as a scalar, got {:?}",
            array.problems[0]
        );

        let scalar = validate(&json!("scalar"));
        assert!(!scalar.ok);
        assert!(
            scalar.problems[0].contains("scalar"),
            "scalar root problem should name the scalar shape, got {:?}",
            scalar.problems[0]
        );
        assert!(
            !scalar.problems[0].contains("array"),
            "scalar root must not be reported as an array, got {:?}",
            scalar.problems[0]
        );

        let empty = validate(&json!({}));
        assert!(!empty.ok);
        assert!(
            empty.problems[0].contains("empty"),
            "empty-object problem should say it is empty, got {:?}",
            empty.problems[0]
        );
        assert_eq!(empty.paths, 0);
    }
}
