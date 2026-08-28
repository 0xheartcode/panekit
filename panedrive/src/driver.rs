//! Backend-agnostic driving: read the state seam, and wait until a condition
//! over it holds. The polling loop is generic over a *probe* closure so it is
//! testable without any real IO or sleeping in tests.

use crate::condition::Condition;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

/// Read a JSON state snapshot from `path`. Returns `None` if the file is
/// missing or not yet valid JSON (e.g. mid-write), callers treat that as
/// "state not ready", which is exactly what a poll loop wants.
pub fn read_state_file(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Wrap a captured screen as a structured state value so conditions can run
/// against an *uninstrumented* app: the whole text is at `screen`, and each row
/// is at `lines.<n>`. Pairs with the `~=` (contains) condition, e.g.
/// `screen~=Ready` or `lines.0~=Loading`.
pub fn screen_state(text: &str) -> Value {
    let lines: Vec<Value> = text.lines().map(|l| Value::String(l.to_string())).collect();
    serde_json::json!({ "screen": text, "lines": lines })
}

/// Outcome of a [`wait_until`] loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The condition held; the value is how long it took to first hold.
    Satisfied(Duration),
    /// The timeout elapsed without the condition ever holding.
    TimedOut,
}

impl WaitOutcome {
    pub fn is_satisfied(&self) -> bool {
        matches!(self, WaitOutcome::Satisfied(_))
    }
}

/// Poll `probe` until `cond` holds or `timeout` elapses, sleeping `interval`
/// between attempts. The condition is checked once up front, so a
/// already-satisfied state returns immediately without sleeping.
pub fn wait_until<F>(
    cond: &Condition,
    timeout: Duration,
    interval: Duration,
    mut probe: F,
) -> WaitOutcome
where
    F: FnMut() -> Option<Value>,
{
    let start = Instant::now();
    loop {
        if let Some(state) = probe() {
            if cond.eval(&state) {
                return WaitOutcome::Satisfied(start.elapsed());
            }
        }
        if start.elapsed() >= timeout {
            return WaitOutcome::TimedOut;
        }
        std::thread::sleep(interval);
    }
}

/// Sample `probe` every `interval` for `duration`, handing each observed state
/// to `emit` along with the elapsed time. With `distinct`, only states that
/// differ from the previously emitted one are passed on, turning the sampling
/// into a compact transition log. Returns how many states were emitted.
///
/// This catches transients a single [`wait_until`]/assert would miss (states the
/// UI emitted between checks); it cannot recover states the UI never wrote.
pub fn watch<P, E>(
    duration: Duration,
    interval: Duration,
    distinct: bool,
    mut probe: P,
    mut emit: E,
) -> usize
where
    P: FnMut() -> Option<Value>,
    E: FnMut(Duration, &Value),
{
    let start = Instant::now();
    let mut last: Option<Value> = None;
    let mut count = 0;
    loop {
        if let Some(v) = probe() {
            if !distinct || last.as_ref() != Some(&v) {
                emit(start.elapsed(), &v);
                count += 1;
                last = Some(v);
            }
        }
        if start.elapsed() >= duration {
            break;
        }
        std::thread::sleep(interval);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    #[test]
    fn screen_state_exposes_the_text_and_its_lines() {
        let v = screen_state("Loading...\nReady");
        assert_eq!(v["screen"], "Loading...\nReady");
        assert_eq!(v["lines"][0], "Loading...");
        assert_eq!(v["lines"][1], "Ready");
        // conditions run against it, so `screen~=Ready` and `lines.1=Ready` hold
        assert!(Condition::parse("screen~=Ready").unwrap().eval(&v));
        assert!(Condition::parse("lines.1=Ready").unwrap().eval(&v));
    }

    #[test]
    fn satisfied_on_first_probe_does_not_wait() {
        let cond = Condition::parse("ready=true").unwrap();
        let out = wait_until(
            &cond,
            Duration::from_secs(5),
            Duration::from_millis(10),
            || Some(json!({ "ready": true })),
        );
        assert!(out.is_satisfied());
    }

    #[test]
    fn becomes_satisfied_after_a_few_probes() {
        let n = Cell::new(0);
        let cond = Condition::parse("ready=true").unwrap();
        let out = wait_until(
            &cond,
            Duration::from_secs(5),
            Duration::from_millis(1),
            || {
                let i = n.get();
                n.set(i + 1);
                Some(json!({ "ready": i >= 3 }))
            },
        );
        assert!(out.is_satisfied());
        assert!(n.get() >= 4, "should have polled until ready");
    }

    #[test]
    fn times_out_when_never_satisfied() {
        let cond = Condition::parse("ready=true").unwrap();
        let out = wait_until(
            &cond,
            Duration::from_millis(5),
            Duration::from_millis(1),
            || Some(json!({ "ready": false })),
        );
        assert_eq!(out, WaitOutcome::TimedOut);
    }

    #[test]
    fn is_satisfied_distinguishes_the_two_outcomes() {
        assert!(WaitOutcome::Satisfied(Duration::from_millis(1)).is_satisfied());
        assert!(!WaitOutcome::TimedOut.is_satisfied());
    }

    #[test]
    fn watch_distinct_records_only_transitions() {
        let n = Cell::new(0);
        let mut emitted: Vec<i64> = Vec::new();
        // Values: 0,0,1,1,2 → distinct should emit 0,1,2 (three transitions).
        let seq = [0, 0, 1, 1, 2];
        let count = watch(
            Duration::from_millis(20),
            Duration::from_millis(1),
            true,
            || {
                let i = n.get().min(seq.len() - 1);
                n.set(n.get() + 1);
                Some(json!({ "c": seq[i] }))
            },
            |_, v| emitted.push(v["c"].as_i64().unwrap()),
        );
        assert_eq!(emitted, vec![0, 1, 2]);
        assert_eq!(count, 3);
    }

    #[test]
    fn watch_non_distinct_emits_every_sample() {
        let mut samples = 0;
        let count = watch(
            Duration::from_millis(10),
            Duration::from_millis(2),
            false,
            || Some(json!({ "x": 1 })),
            |_, _| samples += 1,
        );
        assert!(count >= 2, "expected multiple samples, got {count}");
        assert_eq!(count, samples);
    }

    #[test]
    fn missing_state_is_treated_as_not_ready_then_times_out() {
        let cond = Condition::parse("ready").unwrap();
        let out = wait_until(
            &cond,
            Duration::from_millis(5),
            Duration::from_millis(1),
            || None,
        );
        assert_eq!(out, WaitOutcome::TimedOut);
    }
}
