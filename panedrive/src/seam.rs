//! Introspection over a state-seam JSON document.
//!
//! The seam contract itself lives in [`paneview`], the crate that *defines* the
//! seam, so the rule has one owner and an app can unit-test its own snapshots
//! with [`paneview::check`] without pulling in this driver. This module
//! re-exports that introspection surface and adds the [`SeamReport`] shape the
//! `validate-seam` command prints.
//!
//! - [`leaves`] enumerates every *assertable* dot-path (what `state --paths`
//!   prints), so you can see the surface a [`Condition`](crate::condition)
//!   addresses before writing one.
//! - [`validate`] checks a document against the seam contract and reports a
//!   [`SeamReport`] (ok flag, human-readable problems, path count).

use serde_json::Value;

pub use paneview::{Leaf, Violation, check, leaves};

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

/// Check `root` against the seam contract (see [`paneview::check`]) and present
/// the outcome as a [`SeamReport`] for the `validate-seam` command: an `ok`
/// flag, the human-readable problems, and how many assertable paths the document
/// exposes. The contract constrains only the root shape (a non-empty object);
/// nesting and arrays are fine.
pub fn validate(root: &Value) -> SeamReport {
    let problems = check(root)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.message)
        .collect::<Vec<_>>();
    SeamReport {
        ok: problems.is_empty(),
        paths: leaves(root).len(),
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_accepts_an_object_with_leaves() {
        let report = validate(&json!({ "focus": "fleet" }));
        assert!(report.ok);
        assert_eq!(report.paths, 1);
        assert!(report.problems.is_empty());
    }

    #[test]
    fn validate_counts_paths_through_arrays() {
        // The report's path count reflects the reused leaf walk, arrays and all.
        let report = validate(&json!({
            "rows": [ { "status": "ok" }, { "status": "down" } ],
            "count": 2,
        }));
        assert!(report.ok);
        assert_eq!(report.paths, 3);
    }

    #[test]
    fn validate_rejects_non_object_roots_and_empty_objects() {
        // An array root and a scalar root are both rejected, but with *distinct*
        // messages, so neither arm can be dropped without a test noticing.
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
