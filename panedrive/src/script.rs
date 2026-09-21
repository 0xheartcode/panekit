//! A line-oriented script of driving steps, run in one process against one
//! backend. This is the only way to drive the PTY backend from the CLI: the PTY
//! backend *spawns and owns* the target program, so it dies the moment the
//! process exits. A single `run` invocation spawns the program, executes every
//! step in order, and exits, instead of the tmux model of one attach per
//! command. The same runner also batches steps for the attach backends.
//!
//! Grammar (one step per line; blank lines and `#` comments are skipped):
//!
//! | line                                   | step                              |
//! |----------------------------------------|-----------------------------------|
//! | `press 2 Down Enter`                   | send those keys                   |
//! | `type hello world`                     | type the literal rest of the line |
//! | `type --from-env VAULT_PASS`           | type a secret from an env var     |
//! | `type --paste --from-env VAULT_PASS`   | same, via the paste transport     |
//! | `wait-until count=1 --timeout-ms 2000` | poll the seam until it holds      |
//! | `assert focus=fleet`                   | check the seam once               |
//! | `capture`                              | emit the pane's visible text      |
//! | `sleep 200ms`                          | pause (`200ms`, `1s`, or bare ms) |
//!
//! Exit contract, preserved from the single-shot commands: a failing `assert`
//! or a timed-out `wait-until` stops the run and yields [`RunResult::Failed`]
//! (exit 1); a backend or usage error is an `Err` (exit 2); otherwise the run
//! passes (exit 0).

use crate::backend::PaneBackend;
use crate::condition::{Condition, Observed};
use crate::driver::wait_until;
use crate::key::{self, Key, TmuxKey};
use serde_json::Value;
use std::time::{Duration, Instant};

/// How long the `settle` poll waits between seam reads.
const SETTLE_INTERVAL_MS: u64 = 20;

const DEFAULT_TIMEOUT_MS: u64 = 5000;
const DEFAULT_INTERVAL_MS: u64 = 50;

/// One parsed step of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send a sequence of key presses.
    Press(Vec<Key>),
    /// Type text into the pane, optionally via the paste transport.
    Type {
        /// Where the text to type comes from.
        source: TypeSource,
        /// Route through the backend's paste transport (tmux buffer) instead of
        /// keystrokes, so a secret never transits `send-keys` argv.
        paste: bool,
    },
    /// Poll the seam until a condition holds or the timeout elapses.
    WaitUntil {
        /// The condition to wait for.
        cond: Condition,
        /// How long to keep polling before giving up.
        timeout: Duration,
        /// How long to sleep between seam reads.
        interval: Duration,
    },
    /// Check a condition against the seam once.
    Assert(Condition),
    /// Emit the pane's currently visible text.
    Capture,
    /// Pause for the given duration.
    Sleep(Duration),
}

/// Where a `type` step gets its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSource {
    /// A literal string from the script line.
    Literal(String),
    /// The value of an environment variable, resolved at run time so the secret
    /// is never baked into the parsed script.
    FromEnv(String),
}

/// The result of running a whole script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// Every step passed.
    Passed,
    /// An `assert`/`wait-until` step did not hold; the failure explains which
    /// step, the condition, and what the state actually held.
    Failed(StepFailure),
}

/// A structured description of the step that ended a run, carrying enough detail
/// to explain the failure (and serialize it) without re-reading the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    /// 1-based step number in the script.
    pub step: usize,
    /// The failing verb, `"assert"` or `"wait-until"`.
    pub kind: &'static str,
    /// The condition spec that failed, in canonical form (`bag.count=2`).
    pub cond: String,
    /// The dot-path the condition addressed.
    pub path: String,
    /// Why it failed.
    pub reason: FailReason,
}

/// Why a step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailReason {
    /// The condition did not hold; `observed` is what the path resolved to.
    Unmet {
        /// What the condition's path resolved to.
        observed: Observed,
    },
    /// There was no readable state seam to check against.
    NoState,
    /// A `wait-until` timed out; `observed` is the last thing the path held.
    TimedOut {
        /// The timeout that elapsed, in milliseconds.
        timeout_ms: u64,
        /// The last value the path held before the timeout.
        observed: Observed,
    },
}

impl StepFailure {
    /// A one-line human explanation, e.g.
    /// `step 2: assert bag.count=2 did not hold (bag.count was 1)`.
    pub fn human(&self) -> String {
        match &self.reason {
            FailReason::NoState => format!(
                "step {}: {} {} has no readable state",
                self.step, self.kind, self.cond
            ),
            FailReason::Unmet { observed } => format!(
                "step {}: {} {} did not hold ({})",
                self.step,
                self.kind,
                self.cond,
                observed.describe(&self.path)
            ),
            FailReason::TimedOut {
                timeout_ms,
                observed,
            } => format!(
                "step {}: {} {} timed out after {timeout_ms} ms ({})",
                self.step,
                self.kind,
                self.cond,
                observed.describe(&self.path)
            ),
        }
    }

    /// The machine-readable form for `--json`.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("step".into(), self.step.into());
        m.insert("kind".into(), self.kind.into());
        m.insert("cond".into(), self.cond.clone().into());
        m.insert("path".into(), self.path.clone().into());
        match &self.reason {
            FailReason::NoState => {
                m.insert("error".into(), "no readable state".into());
            }
            FailReason::Unmet { observed } => {
                m.insert("actual".into(), observed.to_json());
            }
            FailReason::TimedOut {
                timeout_ms,
                observed,
            } => {
                m.insert("timeout_ms".into(), (*timeout_ms).into());
                m.insert("actual".into(), observed.to_json());
            }
        }
        Value::Object(m)
    }
}

/// One recorded step during a run, the unit of the `--events` track and the
/// `steps` array of the `--json` summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvent {
    /// Milliseconds since the run started.
    pub t_ms: u64,
    /// 1-based step number.
    pub step: usize,
    /// The verb: `press`, `type`, `assert`, `wait-until`, `capture`, `sleep`.
    pub kind: &'static str,
    /// A short description (keys pressed, condition spec, duration). Never a
    /// secret: `type --from-env VAR` records the variable name, not its value.
    pub detail: String,
    /// For `assert`/`wait-until`, whether it held; `None` for input steps.
    pub ok: Option<bool>,
    /// For condition steps, the scalar the path held, if any.
    pub actual: Option<String>,
}

impl RunEvent {
    /// The machine-readable form, one JSONL line in the `--events` file.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("t_ms".into(), self.t_ms.into());
        m.insert("step".into(), self.step.into());
        m.insert("kind".into(), self.kind.into());
        m.insert("detail".into(), self.detail.clone().into());
        if let Some(ok) = self.ok {
            m.insert("ok".into(), ok.into());
        }
        if let Some(a) = &self.actual {
            m.insert("actual".into(), a.clone().into());
        }
        Value::Object(m)
    }
}

/// A readable spelling of a key run for an event `detail`, e.g. `Down Down Enter`.
fn keys_detail(keys: &[Key]) -> String {
    let mut out = String::new();
    for k in keys {
        if !out.is_empty() {
            out.push(' ');
        }
        match k.to_tmux() {
            TmuxKey::Literal(c) => out.push(c),
            TmuxKey::Named(name) => out.push_str(&name),
        }
    }
    out
}

impl Step {
    /// Parse one script line. Blank lines and `#` comments return `None`.
    pub fn parse(line: &str) -> anyhow::Result<Option<Step>> {
        // Allow leading indentation, but keep the rest of the line intact so
        // `type` can carry literal leading and trailing whitespace. The other
        // verbs tokenize their operand, so surrounding spaces do not matter.
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            return Ok(None);
        }
        let (verb, rest) = match content.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r),
            None => (content, ""),
        };
        let step = match verb {
            "press" => {
                let keys = key::parse_keys(rest)?;
                if keys.is_empty() {
                    anyhow::bail!("press needs at least one key");
                }
                Step::Press(keys)
            }
            "type" => parse_type(rest)?,
            "capture" => Step::Capture,
            "sleep" => Step::Sleep(parse_duration(rest)?),
            "wait-until" => parse_wait_until(rest)?,
            "assert" => Step::Assert(Condition::parse(first_token(rest)?)?),
            other => anyhow::bail!("unknown step {other:?}"),
        };
        Ok(Some(step))
    }
}

/// Parse a whole script into its steps, reporting the 1-based line number on the
/// first bad line.
pub fn parse_script(text: &str) -> anyhow::Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (i, line) in text.lines().enumerate() {
        match Step::parse(line) {
            Ok(Some(step)) => steps.push(step),
            Ok(None) => {}
            Err(e) => anyhow::bail!("line {}: {e}", i + 1),
        }
    }
    Ok(steps)
}

/// Options that control how a script runs.
#[derive(Debug, Default, Clone)]
pub struct RunOptions {
    /// When `Some(timeout)`, each key/`type` step waits up to `timeout` for the
    /// seam to change before the next step runs. This absorbs the asynchronous
    /// gap between a keypress and the UI writing its next snapshot, so a
    /// following `assert` does not race a stale seam. `None` disables settling.
    pub settle: Option<Duration>,
}

impl RunOptions {
    /// Default options: no settling.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable settling, waiting up to `timeout` after each mutating step.
    pub fn with_settle(mut self, timeout: Duration) -> Self {
        self.settle = Some(timeout);
        self
    }
}

/// The channels a running script reads and reports through: the state `probe`
/// (the source of seam values), the `emit` sink that receives `capture` output,
/// and the `on_event` sink that observes every executed [`RunEvent`]. Only
/// `probe` is required; the two reporting hooks default to no-ops, so a caller
/// that just wants a pass/fail verdict implements a single method.
pub trait RunSink {
    /// Read the current state seam, or `None` when it is not readable yet.
    fn probe(&mut self) -> Option<Value>;
    /// Receive the visible pane text produced by a `capture` step.
    fn emit(&mut self, _screen: &str) {}
    /// Observe a completed step, in order, including the step that fails.
    fn on_event(&mut self, _event: &RunEvent) {}
}

/// A [`RunSink`] assembled from closures, for callers that would rather not
/// define their own type. Use [`ClosureSink::new`] to wire all three channels,
/// or [`probe_sink`] when only the state probe matters.
pub struct ClosureSink<P, E, V> {
    probe: P,
    emit: E,
    on_event: V,
}

impl<P, E, V> ClosureSink<P, E, V>
where
    P: FnMut() -> Option<Value>,
    E: FnMut(&str),
    V: FnMut(&RunEvent),
{
    /// Wire a sink from a state probe, a capture emitter, and an event observer.
    pub fn new(probe: P, emit: E, on_event: V) -> Self {
        Self {
            probe,
            emit,
            on_event,
        }
    }
}

impl<P, E, V> RunSink for ClosureSink<P, E, V>
where
    P: FnMut() -> Option<Value>,
    E: FnMut(&str),
    V: FnMut(&RunEvent),
{
    fn probe(&mut self) -> Option<Value> {
        (self.probe)()
    }
    fn emit(&mut self, screen: &str) {
        (self.emit)(screen)
    }
    fn on_event(&mut self, event: &RunEvent) {
        (self.on_event)(event)
    }
}

/// A [`RunSink`] that only reads state through `probe`, ignoring capture output
/// and events, for a plain pass/fail run.
pub fn probe_sink<P>(probe: P) -> ClosureSink<P, impl FnMut(&str), impl FnMut(&RunEvent)>
where
    P: FnMut() -> Option<Value>,
{
    ClosureSink::new(probe, |_screen: &str| {}, |_event: &RunEvent| {})
}

/// Run every step in order against `backend`, reading state and reporting
/// progress through `sink`, under `opts`. Stops at the first failing assertion
/// or timed-out `wait-until` with [`RunResult::Failed`] (exit 1); a backend or
/// usage error is returned as `Err` (exit 2); otherwise [`RunResult::Passed`].
pub fn run_script(
    steps: &[Step],
    backend: &dyn PaneBackend,
    opts: &RunOptions,
    sink: &mut dyn RunSink,
) -> anyhow::Result<RunResult> {
    let settle = opts.settle;
    let run_start = Instant::now();
    let t_ms = |start: Instant| start.elapsed().as_millis() as u64;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        match step {
            Step::Press(keys) => {
                let before = if settle.is_some() { sink.probe() } else { None };
                backend.send_keys(keys)?;
                settle_after(settle, &before, sink);
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "press",
                    detail: keys_detail(keys),
                    ok: None,
                    actual: None,
                });
            }
            Step::Type { source, paste } => {
                let text = match source {
                    TypeSource::Literal(s) => s.clone(),
                    TypeSource::FromEnv(var) => std::env::var(var).map_err(|_| {
                        anyhow::anyhow!("step {n}: environment variable {var} is not set")
                    })?,
                };
                let before = if settle.is_some() { sink.probe() } else { None };
                if *paste {
                    backend.paste_text(&text)?;
                } else {
                    let keys: Vec<Key> = text.chars().map(Key::Char).collect();
                    backend.send_keys(&keys)?;
                }
                settle_after(settle, &before, sink);
                // Never log the secret itself: record the source, not the value.
                let detail = match source {
                    TypeSource::Literal(s) => s.clone(),
                    TypeSource::FromEnv(var) => format!("--from-env {var}"),
                };
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "type",
                    detail,
                    ok: None,
                    actual: None,
                });
            }
            Step::Sleep(d) => {
                std::thread::sleep(*d);
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "sleep",
                    detail: format!("{} ms", d.as_millis()),
                    ok: None,
                    actual: None,
                });
            }
            Step::Capture => {
                sink.emit(&backend.capture()?);
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "capture",
                    detail: String::new(),
                    ok: None,
                    actual: None,
                });
            }
            Step::Assert(cond) => {
                let (ok, observed, has_state) = match sink.probe() {
                    Some(v) => (cond.eval(&v), cond.observed(&v), true),
                    None => (false, Observed::Missing, false),
                };
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "assert",
                    detail: cond.to_spec(),
                    ok: Some(ok),
                    actual: observed.scalar().map(str::to_string),
                });
                if !ok {
                    let reason = if has_state {
                        FailReason::Unmet { observed }
                    } else {
                        FailReason::NoState
                    };
                    return Ok(RunResult::Failed(StepFailure {
                        step: n,
                        kind: "assert",
                        cond: cond.to_spec(),
                        path: cond.path().to_string(),
                        reason,
                    }));
                }
            }
            Step::WaitUntil {
                cond,
                timeout,
                interval,
            } => {
                let outcome = wait_until(cond, *timeout, *interval, || sink.probe());
                let ok = outcome.is_satisfied();
                // A final probe gives the event/failure the last observed value.
                let observed = sink
                    .probe()
                    .map(|v| cond.observed(&v))
                    .unwrap_or(Observed::Missing);
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind: "wait-until",
                    detail: cond.to_spec(),
                    ok: Some(ok),
                    actual: observed.scalar().map(str::to_string),
                });
                if !ok {
                    return Ok(RunResult::Failed(StepFailure {
                        step: n,
                        kind: "wait-until",
                        cond: cond.to_spec(),
                        path: cond.path().to_string(),
                        reason: FailReason::TimedOut {
                            timeout_ms: timeout.as_millis() as u64,
                            observed,
                        },
                    }));
                }
            }
        }
    }
    Ok(RunResult::Passed)
}

/// After a mutating step, poll the seam until it differs from `before` (or the
/// settle timeout elapses). A no-op when `settle` is `None`.
fn settle_after(settle: Option<Duration>, before: &Option<Value>, sink: &mut dyn RunSink) {
    let Some(timeout) = settle else { return };
    let start = Instant::now();
    loop {
        if sink.probe().as_ref() != before.as_ref() {
            return;
        }
        if start.elapsed() >= timeout {
            return;
        }
        std::thread::sleep(Duration::from_millis(SETTLE_INTERVAL_MS));
    }
}

/// `type <literal...>` | `type [--paste] --from-env VAR` | `type --paste <literal...>`.
///
/// Flags are recognized only when the first token is `--paste` or `--from-env`;
/// otherwise the whole remainder (leading and trailing whitespace included) is
/// literal text, so ordinary `type` keeps its verbatim behavior.
fn parse_type(rest: &str) -> anyhow::Result<Step> {
    let trimmed = rest.trim_start();
    let first = trimmed.split_whitespace().next().unwrap_or("");
    if first != "--paste" && first != "--from-env" {
        return Ok(Step::Type {
            source: TypeSource::Literal(rest.to_string()),
            paste: false,
        });
    }

    let mut paste = false;
    let mut from_env: Option<String> = None;
    let mut tokens = trimmed.split_whitespace().peekable();
    while let Some(&tok) = tokens.peek() {
        match tok {
            "--paste" => {
                paste = true;
                tokens.next();
            }
            "--from-env" => {
                tokens.next();
                let var = tokens
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("type --from-env needs a variable name"))?;
                from_env = Some(var.to_string());
            }
            _ => break,
        }
    }
    let remainder: Vec<&str> = tokens.collect();
    let source = match from_env {
        Some(var) => {
            if !remainder.is_empty() {
                anyhow::bail!("type --from-env takes no literal text");
            }
            TypeSource::FromEnv(var)
        }
        None => {
            if remainder.is_empty() {
                anyhow::bail!("type --paste needs literal text or --from-env VAR");
            }
            TypeSource::Literal(remainder.join(" "))
        }
    };
    Ok(Step::Type { source, paste })
}

/// `wait-until <cond> [--timeout-ms N] [--interval-ms N]`.
fn parse_wait_until(rest: &str) -> anyhow::Result<Step> {
    let mut tokens = rest.split_whitespace();
    let cond_spec = tokens
        .next()
        .ok_or_else(|| anyhow::anyhow!("wait-until needs a condition"))?;
    let cond = Condition::parse(cond_spec)?;
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    let mut interval = Duration::from_millis(DEFAULT_INTERVAL_MS);
    while let Some(flag) = tokens.next() {
        match flag {
            "--timeout-ms" => {
                timeout = Duration::from_millis(parse_flag_ms(&mut tokens, "--timeout-ms")?)
            }
            "--interval-ms" => {
                interval = Duration::from_millis(parse_flag_ms(&mut tokens, "--interval-ms")?)
            }
            _ => anyhow::bail!("unexpected token {flag:?} in wait-until"),
        }
    }
    Ok(Step::WaitUntil {
        cond,
        timeout,
        interval,
    })
}

fn parse_flag_ms<'a, I: Iterator<Item = &'a str>>(
    tokens: &mut I,
    flag: &str,
) -> anyhow::Result<u64> {
    let raw = tokens
        .next()
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))?;
    raw.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{flag} value {raw:?} is not a number"))
}

fn first_token(rest: &str) -> anyhow::Result<&str> {
    rest.split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected a condition"))
}

/// Parse `200ms`, `1s`, or a bare millisecond count.
fn parse_duration(raw: &str) -> anyhow::Result<Duration> {
    let raw = raw.trim();
    if let Some(ms) = raw.strip_suffix("ms") {
        return Ok(Duration::from_millis(parse_u64(ms.trim())?));
    }
    if let Some(s) = raw.strip_suffix('s') {
        return Ok(Duration::from_secs(parse_u64(s.trim())?));
    }
    Ok(Duration::from_millis(parse_u64(raw)?))
}

fn parse_u64(s: &str) -> anyhow::Result<u64> {
    s.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("expected a number, got {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::io;

    #[test]
    fn parse_skips_blanks_and_comments() {
        assert_eq!(Step::parse("").unwrap(), None);
        assert_eq!(Step::parse("   ").unwrap(), None);
        assert_eq!(Step::parse("# a comment").unwrap(), None);
    }

    #[test]
    fn parse_each_verb() {
        assert_eq!(
            Step::parse("press 2 Down Enter").unwrap().unwrap(),
            Step::Press(vec![Key::Char('2'), Key::Down, Key::Enter])
        );
        assert_eq!(
            Step::parse("type hello world").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::Literal("hello world".to_string()),
                paste: false,
            }
        );
        assert_eq!(Step::parse("capture").unwrap().unwrap(), Step::Capture);
        assert_eq!(
            Step::parse("assert focus=fleet").unwrap().unwrap(),
            Step::Assert(Condition::parse("focus=fleet").unwrap())
        );
    }

    #[test]
    fn parse_sleep_forms() {
        assert_eq!(
            Step::parse("sleep 200ms").unwrap().unwrap(),
            Step::Sleep(Duration::from_millis(200))
        );
        assert_eq!(
            Step::parse("sleep 1s").unwrap().unwrap(),
            Step::Sleep(Duration::from_secs(1))
        );
        assert_eq!(
            Step::parse("sleep 50").unwrap().unwrap(),
            Step::Sleep(Duration::from_millis(50))
        );
        assert!(Step::parse("sleep nope").is_err());
    }

    #[test]
    fn parse_wait_until_defaults_and_flags() {
        assert_eq!(
            Step::parse("wait-until count=1").unwrap().unwrap(),
            Step::WaitUntil {
                cond: Condition::parse("count=1").unwrap(),
                timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
                interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            }
        );
        assert_eq!(
            Step::parse("wait-until count=1 --timeout-ms 2000 --interval-ms 10")
                .unwrap()
                .unwrap(),
            Step::WaitUntil {
                cond: Condition::parse("count=1").unwrap(),
                timeout: Duration::from_millis(2000),
                interval: Duration::from_millis(10),
            }
        );
        assert!(Step::parse("wait-until").is_err());
        assert!(Step::parse("wait-until count=1 --timeout-ms").is_err());
        assert!(Step::parse("wait-until count=1 --timeout-ms nope").is_err());
        assert!(Step::parse("wait-until count=1 --bogus 1").is_err());
    }

    #[test]
    fn parse_rejects_unknown_verbs() {
        assert!(Step::parse("frobnicate x").is_err());
    }

    #[test]
    fn press_needs_at_least_one_key() {
        assert!(Step::parse("press").is_err());
        assert!(Step::parse("press   ").is_err());
        assert!(Step::parse("press ,").is_err());
    }

    #[test]
    fn type_preserves_leading_and_trailing_whitespace() {
        assert_eq!(
            Step::parse("type   x").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::Literal("  x".to_string()),
                paste: false,
            }
        );
        assert_eq!(
            Step::parse("type foo ").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::Literal("foo ".to_string()),
                paste: false,
            }
        );
    }

    #[test]
    fn type_parses_secret_and_paste_flags() {
        assert_eq!(
            Step::parse("type --from-env VAULT_PASS").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::FromEnv("VAULT_PASS".to_string()),
                paste: false,
            }
        );
        assert_eq!(
            Step::parse("type --paste --from-env VAULT_PASS")
                .unwrap()
                .unwrap(),
            Step::Type {
                source: TypeSource::FromEnv("VAULT_PASS".to_string()),
                paste: true,
            }
        );
        assert_eq!(
            Step::parse("type --paste hi there").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::Literal("hi there".to_string()),
                paste: true,
            }
        );
        // A literal that merely starts like a flag word is still literal.
        assert_eq!(
            Step::parse("type --pastel colour").unwrap().unwrap(),
            Step::Type {
                source: TypeSource::Literal("--pastel colour".to_string()),
                paste: false,
            }
        );
        assert!(Step::parse("type --from-env").is_err());
        assert!(Step::parse("type --from-env VAR trailing").is_err());
        assert!(Step::parse("type --paste").is_err());
    }

    #[test]
    fn leading_indentation_is_allowed() {
        assert_eq!(Step::parse("    capture").unwrap().unwrap(), Step::Capture);
        assert_eq!(Step::parse("   # comment").unwrap(), None);
    }

    #[test]
    fn parse_script_reports_the_bad_line_number() {
        let text = "press Enter\n\n# ok\nfrobnicate\n";
        let err = parse_script(text).unwrap_err().to_string();
        assert!(err.contains("line 4"), "got: {err}");
    }

    /// Records the keys, pastes, and captures it was asked for; returns scripted
    /// screens.
    struct MockBackend {
        sent: RefCell<Vec<Key>>,
        pasted: RefCell<Vec<String>>,
        screen: String,
    }

    impl PaneBackend for MockBackend {
        fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
            self.sent.borrow_mut().extend_from_slice(keys);
            Ok(())
        }
        fn capture(&self) -> io::Result<String> {
            Ok(self.screen.clone())
        }
        fn paste_text(&self, text: &str) -> io::Result<()> {
            self.pasted.borrow_mut().push(text.to_string());
            Ok(())
        }
    }

    fn mock() -> MockBackend {
        MockBackend {
            sent: RefCell::new(Vec::new()),
            pasted: RefCell::new(Vec::new()),
            screen: "SCREEN".to_string(),
        }
    }

    /// A `RunSink` that reads state through a probe closure and keeps every
    /// capture line and event it is handed, so a test can inspect them after the
    /// run (unlike bare closures, it owns its buffers past the borrow).
    struct TestSink<P: FnMut() -> Option<Value>> {
        probe: P,
        captured: Vec<String>,
        events: Vec<RunEvent>,
    }

    impl<P: FnMut() -> Option<Value>> TestSink<P> {
        fn new(probe: P) -> Self {
            Self {
                probe,
                captured: Vec::new(),
                events: Vec::new(),
            }
        }
    }

    impl<P: FnMut() -> Option<Value>> RunSink for TestSink<P> {
        fn probe(&mut self) -> Option<Value> {
            (self.probe)()
        }
        fn emit(&mut self, screen: &str) {
            self.captured.push(screen.to_string());
        }
        fn on_event(&mut self, event: &RunEvent) {
            self.events.push(event.clone());
        }
    }

    #[test]
    fn run_passes_when_every_step_holds() {
        let steps = parse_script("press a\ntype hi\nassert ready=true\ncapture").unwrap();
        let backend = mock();
        let mut sink = TestSink::new(|| Some(json!({ "ready": true })));
        let out = run_script(&steps, &backend, &RunOptions::new(), &mut sink).unwrap();
        assert_eq!(out, RunResult::Passed);
        assert_eq!(
            *backend.sent.borrow(),
            vec![Key::Char('a'), Key::Char('h'), Key::Char('i')]
        );
        assert_eq!(sink.captured, vec!["SCREEN".to_string()]);
    }

    #[test]
    fn run_types_a_secret_from_env_and_can_paste_it() {
        // SAFETY: single-threaded test; the var is set and read within it.
        unsafe {
            std::env::set_var("PANEDRIVE_TEST_SECRET", "s3cr3t");
        }
        // keystroke path: chars are sent one by one
        let keyed = mock();
        let steps = parse_script("type --from-env PANEDRIVE_TEST_SECRET").unwrap();
        run_script(&steps, &keyed, &RunOptions::new(), &mut probe_sink(|| None)).unwrap();
        assert_eq!(
            *keyed.sent.borrow(),
            "s3cr3t".chars().map(Key::Char).collect::<Vec<_>>()
        );
        assert!(keyed.pasted.borrow().is_empty());

        // paste path: routed through paste_text, not send_keys
        let pasted = mock();
        let steps = parse_script("type --paste --from-env PANEDRIVE_TEST_SECRET").unwrap();
        run_script(
            &steps,
            &pasted,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap();
        assert_eq!(*pasted.pasted.borrow(), vec!["s3cr3t".to_string()]);
        assert!(pasted.sent.borrow().is_empty());

        unsafe {
            std::env::remove_var("PANEDRIVE_TEST_SECRET");
        }
    }

    #[test]
    fn run_errors_when_the_secret_env_var_is_unset() {
        let steps = parse_script("type --from-env PANEDRIVE_DEFINITELY_UNSET").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| None),
        );
        assert!(
            out.is_err(),
            "an unset env var must be an error, not a pass"
        );
    }

    #[test]
    fn settle_waits_for_the_seam_to_change_before_the_next_step() {
        use std::cell::Cell;
        // The seam holds v=0 for the pre-press snapshot AND the first couple of
        // settle polls, only flipping to v=1 on the fourth read. This models the
        // app writing its update several polls late, and — crucially — it makes
        // the test able to tell a real settle loop from a broken one: a settle
        // that never polls, or that treats "unchanged" as done, or that exits
        // before the change, leaves the following `assert` reading the stale
        // v=0 and the run Fails. Only a genuine poll-until-changed reaches v=1.
        // (A probe that advanced on every call could not distinguish these.)
        let make_probe = || {
            let n = Cell::new(0);
            move || {
                let i = n.get();
                n.set(i + 1);
                Some(json!({ "v": if i < 3 { 0 } else { 1 } }))
            }
        };
        let steps = parse_script("press a\nassert v=1").unwrap();

        // Without settle, the assert races and reads the stale v=0.
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(make_probe()),
        )
        .unwrap();
        assert!(matches!(out, RunResult::Failed(_)), "no settle should race");

        // With settle, the press polls until v changes, so the assert holds.
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new().with_settle(Duration::from_secs(1)),
            &mut probe_sink(make_probe()),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed, "settle should absorb the async gap");
    }

    #[test]
    fn run_fails_on_assert_mismatch() {
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": false }))),
        )
        .unwrap();
        assert!(matches!(out, RunResult::Failed(_)));
    }

    #[test]
    fn failed_assert_carries_the_observed_value() {
        let steps = parse_script("assert count=2").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "count": 1 }))),
        )
        .unwrap();
        let RunResult::Failed(f) = out else {
            panic!("expected a failure");
        };
        assert_eq!(f.step, 1);
        assert_eq!(f.kind, "assert");
        assert_eq!(f.cond, "count=2");
        assert_eq!(
            f.reason,
            FailReason::Unmet {
                observed: Observed::Scalar("1".into())
            }
        );
        assert!(
            f.human().contains("count=2 did not hold (count was 1)"),
            "{}",
            f.human()
        );
        assert_eq!(f.to_json()["actual"], json!("1"));
    }

    #[test]
    fn recording_emits_one_event_per_step_with_outcomes() {
        let steps = parse_script("press Down\nassert count=1").unwrap();
        let mut sink = TestSink::new(|| Some(json!({ "count": 1 })));
        let out = run_script(&steps, &mock(), &RunOptions::new(), &mut sink).unwrap();
        assert_eq!(out, RunResult::Passed);
        let events = &sink.events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "press");
        assert_eq!(events[0].detail, "Down");
        assert_eq!(events[1].kind, "assert");
        assert_eq!(events[1].ok, Some(true));
        assert_eq!(events[1].actual.as_deref(), Some("1"));
    }

    #[test]
    fn recording_never_logs_a_secret_value() {
        // SAFETY: single-threaded test; the var is set and read within it.
        unsafe {
            std::env::set_var("PANEDRIVE_TEST_SECRET2", "hunter2");
        }
        let steps = parse_script("type --from-env PANEDRIVE_TEST_SECRET2").unwrap();
        let mut sink = TestSink::new(|| None);
        run_script(&steps, &mock(), &RunOptions::new(), &mut sink).unwrap();
        unsafe {
            std::env::remove_var("PANEDRIVE_TEST_SECRET2");
        }
        let events = &sink.events;
        assert_eq!(events[0].kind, "type");
        assert_eq!(events[0].detail, "--from-env PANEDRIVE_TEST_SECRET2");
        assert!(
            !events[0].detail.contains("hunter2"),
            "the secret value must never reach the event track"
        );
    }

    #[test]
    fn run_fails_on_assert_without_state() {
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap();
        assert!(matches!(
            out,
            RunResult::Failed(f) if matches!(f.reason, FailReason::NoState)
        ));
    }

    #[test]
    fn step_failure_serializes_and_explains_each_reason() {
        let unmet = StepFailure {
            step: 2,
            kind: "assert",
            cond: "count=2".into(),
            path: "count".into(),
            reason: FailReason::Unmet {
                observed: Observed::Scalar("1".into()),
            },
        };
        assert_eq!(unmet.to_json()["actual"], json!("1"));
        assert!(
            unmet
                .human()
                .contains("step 2: assert count=2 did not hold (count was 1)")
        );

        let no_state = StepFailure {
            step: 1,
            kind: "assert",
            cond: "x=1".into(),
            path: "x".into(),
            reason: FailReason::NoState,
        };
        assert_eq!(no_state.to_json()["error"], json!("no readable state"));
        assert!(no_state.human().contains("has no readable state"));

        let timed_out = StepFailure {
            step: 3,
            kind: "wait-until",
            cond: "y=1".into(),
            path: "y".into(),
            reason: FailReason::TimedOut {
                timeout_ms: 500,
                observed: Observed::Missing,
            },
        };
        let j = timed_out.to_json();
        assert_eq!(j["timeout_ms"], json!(500));
        assert_eq!(j["actual"], json!(null));
        assert!(timed_out.human().contains("timed out after 500 ms"));
    }

    #[test]
    fn run_event_json_omits_absent_ok_and_actual() {
        let assertion = RunEvent {
            t_ms: 5,
            step: 1,
            kind: "assert",
            detail: "count=1".into(),
            ok: Some(true),
            actual: Some("1".into()),
        };
        let j = assertion.to_json();
        assert_eq!(j["ok"], json!(true));
        assert_eq!(j["actual"], json!("1"));

        let press = RunEvent {
            t_ms: 0,
            step: 2,
            kind: "press",
            detail: "Enter".into(),
            ok: None,
            actual: None,
        };
        let j2 = press.to_json();
        assert!(j2.get("ok").is_none());
        assert!(j2.get("actual").is_none());
    }

    #[test]
    fn recording_reports_timeout_with_last_observed() {
        let steps = parse_script("wait-until ready=true --timeout-ms 5 --interval-ms 1").unwrap();
        let mut sink = TestSink::new(|| Some(json!({ "ready": false })));
        let out = run_script(&steps, &mock(), &RunOptions::new(), &mut sink).unwrap();
        let RunResult::Failed(f) = out else {
            panic!("expected a timeout failure");
        };
        assert!(matches!(f.reason, FailReason::TimedOut { .. }));
        let last = sink.events.last().unwrap();
        assert_eq!(last.kind, "wait-until");
        assert_eq!(last.ok, Some(false));
    }

    #[test]
    fn run_fails_when_wait_until_times_out() {
        let steps = parse_script("wait-until ready=true --timeout-ms 5 --interval-ms 1").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": false }))),
        )
        .unwrap();
        assert!(matches!(
            out,
            RunResult::Failed(f) if matches!(f.reason, FailReason::TimedOut { .. })
        ));
    }

    #[test]
    fn run_stops_at_the_first_failure() {
        // The second step fails, so the third (capture) never runs.
        let steps = parse_script("press a\nassert ready=true\ncapture").unwrap();
        let backend = mock();
        let mut sink = TestSink::new(|| Some(json!({ "ready": false })));
        let out = run_script(&steps, &backend, &RunOptions::new(), &mut sink).unwrap();
        assert!(matches!(out, RunResult::Failed(_)));
        assert!(
            sink.captured.is_empty(),
            "capture after the failure must not run"
        );
    }

    #[test]
    fn run_surfaces_a_backend_error_as_err() {
        struct Broken;
        impl PaneBackend for Broken {
            fn send_keys(&self, _: &[Key]) -> io::Result<()> {
                Err(io::Error::other("boom"))
            }
            fn capture(&self) -> io::Result<String> {
                Ok(String::new())
            }
        }
        let steps = parse_script("press a").unwrap();
        let err = run_script(
            &steps,
            &Broken,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        );
        assert!(err.is_err());
    }
}
