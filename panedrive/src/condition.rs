//! Conditions evaluated against the UI's JSON state seam.
//!
//! A condition is a tiny expression over a dot-path into the state JSON:
//!
//! | spec              | true when                                        |
//! |-------------------|--------------------------------------------------|
//! | `focus`           | the path exists                                  |
//! | `focus?`          | the path exists (explicit form)                  |
//! | `focus=fleet`     | the scalar at the path equals `fleet`            |
//! | `open=true`       | booleans/numbers compare by their text form      |
//! | `bag.count!=0`    | the path exists and its scalar differs           |
//! | `bag.count>2`     | numeric compare (`>`, `<`, `>=`, `<=`)           |
//! | `line~=Ready`     | the scalar at the path contains the substring    |
//! | `rows.2=x`        | array indices are path segments                  |
//!
//! Scalars (string/bool/number/null) compare by their textual form; objects and
//! arrays are not scalar and never equal a bare value.

use serde_json::Value;

/// A numeric comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumOp {
    Gt,
    Lt,
    Ge,
    Le,
}

/// A parsed condition over the state JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// The dot-path resolves to any value.
    Exists(String),
    /// The scalar at the dot-path equals the given text.
    Equals(String, String),
    /// The dot-path resolves to a scalar that differs from the given text.
    NotEquals(String, String),
    /// The scalar at the dot-path contains the given substring.
    Contains(String, String),
    /// The numeric scalar at the dot-path compares to the given number.
    Compare(String, NumOp, String),
}

impl Condition {
    /// Parse a condition spec. Operators are matched longest-first so `>=` binds
    /// before `>` and `!=` / `~=` before `=`; a trailing `?` (or no operator at
    /// all) means existence.
    pub fn parse(spec: &str) -> anyhow::Result<Condition> {
        let s = spec.trim();
        // Order matters: two-char operators before their one-char prefixes.
        if let Some((path, want)) = s.split_once(">=") {
            return Ok(Condition::Compare(
                clean_path(path),
                NumOp::Ge,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("<=") {
            return Ok(Condition::Compare(
                clean_path(path),
                NumOp::Le,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("!=") {
            return Ok(Condition::NotEquals(clean_path(path), want.trim().into()));
        }
        if let Some((path, want)) = s.split_once("~=") {
            return Ok(Condition::Contains(clean_path(path), want.trim().into()));
        }
        if let Some((path, want)) = s.split_once('>') {
            return Ok(Condition::Compare(
                clean_path(path),
                NumOp::Gt,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once('<') {
            return Ok(Condition::Compare(
                clean_path(path),
                NumOp::Lt,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once('=') {
            return Ok(Condition::Equals(clean_path(path), want.trim().into()));
        }
        let path = clean_path(s.strip_suffix('?').unwrap_or(s));
        if path.is_empty() {
            anyhow::bail!("empty condition");
        }
        Ok(Condition::Exists(path))
    }

    /// Evaluate against a state value.
    pub fn eval(&self, root: &Value) -> bool {
        match self {
            Condition::Exists(path) => value_at(root, path).is_some(),
            Condition::Equals(path, want) => match scalar_at(root, path) {
                Some(got) => scalar_eq(&got, want),
                None => false,
            },
            // Missing path is *not* an inequality: we require the value to exist
            // and differ, so `wait-until x!=0` doesn't pass on absent state.
            Condition::NotEquals(path, want) => match scalar_at(root, path) {
                Some(got) => !scalar_eq(&got, want),
                None => false,
            },
            Condition::Contains(path, want) => match scalar_at(root, path) {
                Some(got) => got.contains(want.as_str()),
                None => false,
            },
            // Both sides must be numeric and the path must exist; otherwise the
            // comparison is unsatisfied rather than erroring.
            Condition::Compare(path, op, want) => {
                match (
                    scalar_at(root, path).and_then(|g| g.parse::<f64>().ok()),
                    want.parse::<f64>(),
                ) {
                    (Some(got), Ok(want)) => match op {
                        NumOp::Gt => got > want,
                        NumOp::Lt => got < want,
                        NumOp::Ge => got >= want,
                        NumOp::Le => got <= want,
                    },
                    _ => false,
                }
            }
        }
    }
}

/// Compare two scalar texts. If both look numeric, compare by value so `2`
/// matches a JSON `2.0` (and `1e3` matches `1000`); otherwise compare as text.
fn scalar_eq(got: &str, want: &str) -> bool {
    match (got.parse::<f64>(), want.parse::<f64>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => got == want,
    }
}

fn clean_path(p: &str) -> String {
    let p = p.trim();
    p.strip_prefix('.').unwrap_or(p).to_string()
}

/// Resolve a dot-path (`a.b.2`) into the JSON tree.
fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(map) => map.get(seg)?,
            Value::Array(arr) => arr.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// The scalar text at a path, or `None` if missing or non-scalar.
fn scalar_at(root: &Value, path: &str) -> Option<String> {
    match value_at(root, path)? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Null => Some("null".to_string()),
        Value::Object(_) | Value::Array(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> Value {
        json!({
            "focus": "fleet",
            "open": true,
            "bag": { "count": 2 },
            "rows": ["a", "b"]
        })
    }

    #[test]
    fn parse_picks_the_right_operator() {
        assert_eq!(
            Condition::parse("focus").unwrap(),
            Condition::Exists("focus".into())
        );
        assert_eq!(
            Condition::parse(".focus?").unwrap(),
            Condition::Exists("focus".into())
        );
        assert_eq!(
            Condition::parse("focus=fleet").unwrap(),
            Condition::Equals("focus".into(), "fleet".into())
        );
        assert_eq!(
            Condition::parse("bag.count!=0").unwrap(),
            Condition::NotEquals("bag.count".into(), "0".into())
        );
        assert!(Condition::parse("").is_err());
        assert!(Condition::parse("?").is_err());
    }

    #[test]
    fn exists_checks_presence_including_nested_and_array() {
        assert!(Condition::parse("focus").unwrap().eval(&state()));
        assert!(Condition::parse("bag.count").unwrap().eval(&state()));
        assert!(Condition::parse("rows.1").unwrap().eval(&state()));
        assert!(!Condition::parse("missing").unwrap().eval(&state()));
        assert!(!Condition::parse("rows.9").unwrap().eval(&state()));
    }

    #[test]
    fn equals_compares_scalars_by_text() {
        assert!(Condition::parse("focus=fleet").unwrap().eval(&state()));
        assert!(Condition::parse("open=true").unwrap().eval(&state()));
        assert!(Condition::parse("bag.count=2").unwrap().eval(&state()));
        assert!(!Condition::parse("bag.count=3").unwrap().eval(&state()));
        // objects/arrays are not scalars → never equal a bare value
        assert!(!Condition::parse("bag=x").unwrap().eval(&state()));
    }

    #[test]
    fn equals_is_numeric_aware_across_int_float_and_exponent() {
        let s = json!({ "a": 2, "b": 2.0, "c": 1000.0 });
        // int state vs plain want, and float state vs int-looking want
        assert!(Condition::parse("a=2").unwrap().eval(&s));
        assert!(Condition::parse("a=2.0").unwrap().eval(&s));
        assert!(Condition::parse("b=2").unwrap().eval(&s));
        assert!(Condition::parse("c=1e3").unwrap().eval(&s));
        assert!(!Condition::parse("a=3").unwrap().eval(&s));
        // non-numeric still compares as text
        let t = json!({ "focus": "fleet" });
        assert!(Condition::parse("focus=fleet").unwrap().eval(&t));
        assert!(!Condition::parse("focus=bag").unwrap().eval(&t));
    }

    #[test]
    fn not_equals_requires_present_and_different() {
        assert!(Condition::parse("bag.count!=0").unwrap().eval(&state()));
        assert!(!Condition::parse("bag.count!=2").unwrap().eval(&state()));
        // absent path is unsatisfied, not "not-equal"
        assert!(!Condition::parse("missing!=0").unwrap().eval(&state()));
    }

    #[test]
    fn parse_matches_operators_longest_first() {
        assert_eq!(
            Condition::parse("bag.count>=2").unwrap(),
            Condition::Compare("bag.count".into(), NumOp::Ge, "2".into())
        );
        assert_eq!(
            Condition::parse("bag.count<=2").unwrap(),
            Condition::Compare("bag.count".into(), NumOp::Le, "2".into())
        );
        assert_eq!(
            Condition::parse("bag.count>1").unwrap(),
            Condition::Compare("bag.count".into(), NumOp::Gt, "1".into())
        );
        assert_eq!(
            Condition::parse("bag.count<9").unwrap(),
            Condition::Compare("bag.count".into(), NumOp::Lt, "9".into())
        );
        assert_eq!(
            Condition::parse("focus~=fle").unwrap(),
            Condition::Contains("focus".into(), "fle".into())
        );
    }

    #[test]
    fn numeric_comparisons_need_both_sides_numeric_and_present() {
        assert!(Condition::parse("bag.count>1").unwrap().eval(&state()));
        assert!(Condition::parse("bag.count>=2").unwrap().eval(&state()));
        assert!(!Condition::parse("bag.count>2").unwrap().eval(&state()));
        assert!(Condition::parse("bag.count<3").unwrap().eval(&state()));
        assert!(Condition::parse("bag.count<=2").unwrap().eval(&state()));
        assert!(!Condition::parse("bag.count<2").unwrap().eval(&state()));
        // non-numeric scalar or absent path is unsatisfied, never a panic
        assert!(!Condition::parse("focus>1").unwrap().eval(&state()));
        assert!(!Condition::parse("missing>1").unwrap().eval(&state()));
    }

    #[test]
    fn contains_matches_substrings_of_the_scalar_text() {
        assert!(Condition::parse("focus~=fle").unwrap().eval(&state()));
        assert!(Condition::parse("focus~=fleet").unwrap().eval(&state()));
        assert!(!Condition::parse("focus~=xyz").unwrap().eval(&state()));
        // objects/arrays are not scalars, so contains is unsatisfied
        assert!(!Condition::parse("bag~=count").unwrap().eval(&state()));
        assert!(!Condition::parse("missing~=x").unwrap().eval(&state()));
    }
}
