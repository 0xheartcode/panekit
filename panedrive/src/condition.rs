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
//! | `line=~Re*dy`     | the scalar matches a glob (`*` any run, `?` one)  |
//! | `rows#=3`         | the array/string/object length (`#=` `#>` `#<` …) |
//! | `rows.2=x`        | array indices are path segments                  |
//!
//! Scalars (string/bool/number/null) compare by their textual form; objects and
//! arrays are not scalar and never equal a bare value.

use serde_json::Value;

/// A numeric comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumOp {
    /// Greater than (`>`).
    Gt,
    /// Less than (`<`).
    Lt,
    /// Greater than or equal (`>=`).
    Ge,
    /// Less than or equal (`<=`).
    Le,
}

impl NumOp {
    /// The operator's source symbol, for reconstructing a condition spec.
    pub fn symbol(&self) -> &'static str {
        match self {
            NumOp::Gt => ">",
            NumOp::Lt => "<",
            NumOp::Ge => ">=",
            NumOp::Le => "<=",
        }
    }

    /// Apply the comparison of `a` against `b`.
    fn apply(&self, a: f64, b: f64) -> bool {
        match self {
            NumOp::Gt => a > b,
            NumOp::Lt => a < b,
            NumOp::Ge => a >= b,
            NumOp::Le => a <= b,
        }
    }
}

/// A length comparison operator (the `#`-prefixed family): compares the *length*
/// of an array, string, or object at the path, rather than its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenOp {
    /// Length equals (`#=`).
    Eq,
    /// Length greater than (`#>`).
    Gt,
    /// Length less than (`#<`).
    Lt,
    /// Length greater than or equal (`#>=`).
    Ge,
    /// Length less than or equal (`#<=`).
    Le,
}

impl LenOp {
    /// The operator's source symbol, for reconstructing a condition spec.
    pub fn symbol(&self) -> &'static str {
        match self {
            LenOp::Eq => "#=",
            LenOp::Gt => "#>",
            LenOp::Lt => "#<",
            LenOp::Ge => "#>=",
            LenOp::Le => "#<=",
        }
    }

    /// Apply the comparison of a length `a` against `b`.
    fn apply(&self, a: f64, b: f64) -> bool {
        match self {
            LenOp::Eq => a == b,
            LenOp::Gt => a > b,
            LenOp::Lt => a < b,
            LenOp::Ge => a >= b,
            LenOp::Le => a <= b,
        }
    }
}

/// What a condition's dot-path resolved to, used to explain a result without
/// re-reading the state seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// A scalar leaf, rendered as its textual form.
    Scalar(String),
    /// The path resolved to an object or array (not comparable to a bare value).
    NonScalar,
    /// The path did not resolve.
    Missing,
}

impl Observed {
    /// A human phrase describing what was seen at the path, e.g. `was 1`,
    /// `not present`, `is not a scalar`.
    pub fn describe(&self, path: &str) -> String {
        match self {
            Observed::Scalar(s) => format!("{path} was {s}"),
            Observed::NonScalar => format!("{path} is not a scalar"),
            Observed::Missing => format!("{path} not present"),
        }
    }

    /// The scalar text if the path held one, else `None` (missing/non-scalar).
    pub fn scalar(&self) -> Option<&str> {
        match self {
            Observed::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// Render as JSON for `--json` output: the scalar text as a string, or
    /// `null` when the path was absent or resolved to a non-scalar.
    pub fn to_json(&self) -> Value {
        match self.scalar() {
            Some(s) => Value::String(s.to_string()),
            None => Value::Null,
        }
    }
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
    /// The scalar at the dot-path matches the given glob (`*` any run, `?` one).
    Glob(String, String),
    /// The numeric scalar at the dot-path compares to the given number.
    Compare(String, NumOp, String),
    /// The length of the array/string/object at the dot-path compares to the
    /// given number.
    Len(String, LenOp, String),
}

impl Condition {
    /// Parse a condition spec. Operators are matched longest-first so `>=` binds
    /// before `>` and `!=` / `~=` before `=`; a trailing `?` (or no operator at
    /// all) means existence.
    pub fn parse(spec: &str) -> anyhow::Result<Condition> {
        let s = spec.trim();
        // Length operators (`#`-prefixed) first: they embed `=`/`>`/`<`, so they
        // must be matched before the value operators below split on those
        // symbols. Longest-first within the family (`#>=` before `#>`).
        if let Some((path, want)) = s.split_once("#>=") {
            return Ok(Condition::Len(
                clean_path(path),
                LenOp::Ge,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("#<=") {
            return Ok(Condition::Len(
                clean_path(path),
                LenOp::Le,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("#=") {
            return Ok(Condition::Len(
                clean_path(path),
                LenOp::Eq,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("#>") {
            return Ok(Condition::Len(
                clean_path(path),
                LenOp::Gt,
                want.trim().into(),
            ));
        }
        if let Some((path, want)) = s.split_once("#<") {
            return Ok(Condition::Len(
                clean_path(path),
                LenOp::Lt,
                want.trim().into(),
            ));
        }
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
        // `=~` (glob) before the single-char `=`, since it embeds it.
        if let Some((path, want)) = s.split_once("=~") {
            return Ok(Condition::Glob(clean_path(path), want.trim().into()));
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

    /// The dot-path this condition addresses.
    pub fn path(&self) -> &str {
        match self {
            Condition::Exists(p)
            | Condition::Equals(p, _)
            | Condition::NotEquals(p, _)
            | Condition::Contains(p, _)
            | Condition::Glob(p, _)
            | Condition::Compare(p, _, _)
            | Condition::Len(p, _, _) => p,
        }
    }

    /// Reconstruct the canonical spec (`bag.count>=2`), for messages that should
    /// echo the user's condition rather than the internal `Debug` form.
    pub fn to_spec(&self) -> String {
        match self {
            Condition::Exists(p) => p.clone(),
            Condition::Equals(p, w) => format!("{p}={w}"),
            Condition::NotEquals(p, w) => format!("{p}!={w}"),
            Condition::Contains(p, w) => format!("{p}~={w}"),
            Condition::Glob(p, w) => format!("{p}=~{w}"),
            Condition::Compare(p, op, w) => format!("{p}{}{w}", op.symbol()),
            Condition::Len(p, op, w) => format!("{p}{}{w}", op.symbol()),
        }
    }

    /// What the condition's path resolved to in `root`.
    pub fn observed(&self, root: &Value) -> Observed {
        // A length condition reports the measured length, so a failure reads
        // `rows#=3 (rows was 2)` rather than `rows is not a scalar`.
        if let Condition::Len(path, _, _) = self {
            return match value_at(root, path) {
                None => Observed::Missing,
                Some(v) => match len_of(v) {
                    Some(n) => Observed::Scalar(n.to_string()),
                    None => Observed::NonScalar,
                },
            };
        }
        match value_at(root, self.path()) {
            None => Observed::Missing,
            Some(Value::Object(_) | Value::Array(_)) => Observed::NonScalar,
            Some(v) => match scalar_text(v) {
                Some(s) => Observed::Scalar(s),
                None => Observed::NonScalar,
            },
        }
    }

    /// A one-line failure explanation: the spec plus what was actually seen,
    /// e.g. `bag.count=2 (bag.count was 1)`.
    pub fn explain(&self, root: &Value) -> String {
        format!(
            "{} ({})",
            self.to_spec(),
            self.observed(root).describe(self.path())
        )
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
            Condition::Glob(path, pat) => match scalar_at(root, path) {
                Some(got) => glob_match(&got, pat),
                None => false,
            },
            // Both sides must be numeric and the path must exist; otherwise the
            // comparison is unsatisfied rather than erroring.
            Condition::Compare(path, op, want) => {
                match (
                    scalar_at(root, path).and_then(|g| g.parse::<f64>().ok()),
                    want.parse::<f64>(),
                ) {
                    (Some(got), Ok(want)) => op.apply(got, want),
                    _ => false,
                }
            }
            // The path must resolve to something with a length (array/string/
            // object) and the bound must be numeric; otherwise it is unsatisfied.
            Condition::Len(path, op, want) => {
                match (value_at(root, path).and_then(len_of), want.parse::<f64>()) {
                    (Some(len), Ok(want)) => op.apply(len as f64, want),
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
    scalar_text(value_at(root, path)?)
}

/// The textual form of a scalar JSON value, or `None` for objects/arrays.
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Null => Some("null".to_string()),
        Value::Object(_) | Value::Array(_) => None,
    }
}

/// The length of a value that has one: an array's element count, a string's
/// character count, or an object's key count. A number, bool, or null has no
/// length, so a length condition on one is unsatisfied rather than an error.
fn len_of(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) => Some(a.len()),
        Value::String(s) => Some(s.chars().count()),
        Value::Object(o) => Some(o.len()),
        Value::Number(_) | Value::Bool(_) | Value::Null => None,
    }
}

/// Match `text` against a glob `pat`, anchored to the whole value: `*` matches
/// any run of characters (including none) and `?` matches exactly one. All other
/// characters are literal (there is no escaping in this form). This is a small,
/// dependency-free matcher, not a full regular expression.
fn glob_match(text: &str, pat: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    // `star` remembers the last `*` seen so we can backtrack and let it absorb
    // one more character; `mark` is where that `*` started matching in `t`.
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
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
        // `=~` (glob) must not be split by the single-char `=`.
        assert_eq!(
            Condition::parse("focus=~fl*t").unwrap(),
            Condition::Glob("focus".into(), "fl*t".into())
        );
        // The `#` length family embeds `=`/`>`/`<` and must win over them, and
        // `#>=`/`#<=` must bind before `#>`/`#<`.
        assert_eq!(
            Condition::parse("rows#=2").unwrap(),
            Condition::Len("rows".into(), LenOp::Eq, "2".into())
        );
        assert_eq!(
            Condition::parse("rows#>=2").unwrap(),
            Condition::Len("rows".into(), LenOp::Ge, "2".into())
        );
        assert_eq!(
            Condition::parse("rows#<=2").unwrap(),
            Condition::Len("rows".into(), LenOp::Le, "2".into())
        );
        assert_eq!(
            Condition::parse("rows#>1").unwrap(),
            Condition::Len("rows".into(), LenOp::Gt, "1".into())
        );
        assert_eq!(
            Condition::parse("rows#<9").unwrap(),
            Condition::Len("rows".into(), LenOp::Lt, "9".into())
        );
    }

    #[test]
    fn glob_matches_anchored_wildcards() {
        // Anchored: the whole value must match, not a substring.
        assert!(Condition::parse("focus=~fleet").unwrap().eval(&state()));
        assert!(Condition::parse("focus=~fl*t").unwrap().eval(&state()));
        assert!(Condition::parse("focus=~f????").unwrap().eval(&state()));
        assert!(Condition::parse("focus=~*").unwrap().eval(&state()));
        assert!(Condition::parse("focus=~fle*").unwrap().eval(&state()));
        // substring without anchors does not match the whole value
        assert!(!Condition::parse("focus=~fle").unwrap().eval(&state()));
        assert!(!Condition::parse("focus=~*x*").unwrap().eval(&state()));
        assert!(!Condition::parse("focus=~f???").unwrap().eval(&state()));
        // non-scalar / missing is unsatisfied, never a panic
        assert!(!Condition::parse("bag=~*").unwrap().eval(&state()));
        assert!(!Condition::parse("missing=~*").unwrap().eval(&state()));
    }

    #[test]
    fn len_compares_array_string_and_object_length() {
        // array length
        assert!(Condition::parse("rows#=2").unwrap().eval(&state()));
        assert!(Condition::parse("rows#>1").unwrap().eval(&state()));
        assert!(Condition::parse("rows#>=2").unwrap().eval(&state()));
        assert!(Condition::parse("rows#<3").unwrap().eval(&state()));
        assert!(!Condition::parse("rows#=3").unwrap().eval(&state()));
        // string length (characters)
        assert!(Condition::parse("focus#=5").unwrap().eval(&state()));
        assert!(Condition::parse("focus#>4").unwrap().eval(&state()));
        // object length (keys)
        assert!(Condition::parse("bag#=1").unwrap().eval(&state()));
        // a scalar number/bool has no length → unsatisfied, not a panic
        assert!(!Condition::parse("bag.count#=1").unwrap().eval(&state()));
        assert!(!Condition::parse("open#=1").unwrap().eval(&state()));
        assert!(!Condition::parse("missing#=0").unwrap().eval(&state()));
        // a non-numeric bound is unsatisfied
        assert!(!Condition::parse("rows#=two").unwrap().eval(&state()));
    }

    #[test]
    fn len_observed_and_explain_report_the_length() {
        let c = Condition::parse("rows#=3").unwrap();
        assert_eq!(c.observed(&state()), Observed::Scalar("2".into()));
        assert_eq!(c.explain(&state()), "rows#=3 (rows was 2)");
        // a value with no length reports non-scalar
        let c = Condition::parse("bag.count#=1").unwrap();
        assert_eq!(c.observed(&state()), Observed::NonScalar);
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
    fn to_spec_reconstructs_the_canonical_form() {
        for spec in [
            "focus",
            "focus=fleet",
            "bag.count!=0",
            "focus~=fle",
            "focus=~fl*t",
            "bag.count>=2",
            "bag.count<9",
            "rows#=2",
            "rows#>=2",
            "rows#<9",
        ] {
            assert_eq!(Condition::parse(spec).unwrap().to_spec(), spec);
        }
        // a trailing `?` and a leading `.` normalize away
        assert_eq!(Condition::parse(".focus?").unwrap().to_spec(), "focus");
    }

    #[test]
    fn observed_reports_scalar_missing_and_non_scalar() {
        let c = Condition::parse("bag.count=9").unwrap();
        assert_eq!(c.observed(&state()), Observed::Scalar("2".into()));
        let c = Condition::parse("missing=1").unwrap();
        assert_eq!(c.observed(&state()), Observed::Missing);
        // an object path is non-scalar
        let c = Condition::parse("bag=x").unwrap();
        assert_eq!(c.observed(&state()), Observed::NonScalar);
    }

    #[test]
    fn explain_echoes_spec_and_actual_value() {
        let c = Condition::parse("bag.count=9").unwrap();
        assert_eq!(c.explain(&state()), "bag.count=9 (bag.count was 2)");
        let c = Condition::parse("missing=1").unwrap();
        assert_eq!(c.explain(&state()), "missing=1 (missing not present)");
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
