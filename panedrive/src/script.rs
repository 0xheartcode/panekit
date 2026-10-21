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
//! | `expect-screen 1/2/3 day`              | the visible text must contain it  |
//! | `expect-no-screen panicked`            | the visible text must NOT         |
//! | `capture`                              | emit the pane's visible text      |
//! | `sleep 200ms`                          | pause (`200ms`, `1s`, or bare ms) |
//!
//! `assert` / `wait-until` read the structured JSON seam; `expect-screen` /
//! `expect-no-screen` match the *rendered* text, catching layout or clipping
//! bugs a correct seam cannot see. Both take a literal substring (no regex yet).
//!
//! Exit contract, preserved from the single-shot commands: a failing `assert`,
//! a timed-out `wait-until`, or an unmet `expect-screen` / `expect-no-screen`
//! stops the run and yields [`RunResult::Failed`] (exit 1); a backend or usage
//! error is an `Err` (exit 2); otherwise the run passes (exit 0). Independently,
//! a run also fails when the program the PTY backend owns exits unexpectedly
//! (before the script ends, or non-zero) or when its output matches a
//! [`RunOptions::fail_on`] pattern, so a crash after the last assertion is not
//! reported as a pass.

use crate::backend::{ChildExit, PaneBackend};
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
    /// Assert the pane's currently visible text contains this literal substring.
    ExpectScreen(String),
    /// Assert the pane's currently visible text does NOT contain this substring.
    ExpectNoScreen(String),
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
    /// The failing verb: `"assert"`, `"wait-until"`, `"expect-screen"`,
    /// `"expect-no-screen"`, `"child-exit"`, or `"fail-on"`.
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
    /// An `expect-screen` / `expect-no-screen` step did not hold against the
    /// pane's rendered text.
    ScreenMismatch {
        /// The literal substring the step looked for.
        pattern: String,
        /// `true` for `expect-no-screen` (the pattern must be absent), so the
        /// failure is "it was present"; `false` for `expect-screen`.
        negate: bool,
    },
    /// The program the backend owns exited unexpectedly: before the script
    /// finished, or with a non-zero status.
    ChildExited {
        /// The exit code, or `None` if the platform did not report one.
        code: Option<i32>,
        /// `true` when it exited while steps still remained (vs. a non-zero
        /// exit at the end of the run).
        early: bool,
    },
    /// The program's output matched a [`RunOptions::fail_on`] pattern.
    OutputMatched {
        /// The pattern that matched the output.
        pattern: String,
    },
}

impl StepFailure {
    /// Whether this failure was a `wait-until` timeout, the only failure a slow
    /// or loaded environment can cause spuriously and so the only one
    /// [`run_with_retries`] retries. An assertion mismatch, a crash, or an
    /// output match is deterministic and is never retried.
    pub fn is_timeout(&self) -> bool {
        matches!(self.reason, FailReason::TimedOut { .. })
    }

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
            FailReason::ScreenMismatch { pattern, negate } => {
                if *negate {
                    format!(
                        "step {}: {} {pattern:?} but the screen contained it",
                        self.step, self.kind
                    )
                } else {
                    format!(
                        "step {}: {} {pattern:?} but the screen did not contain it",
                        self.step, self.kind
                    )
                }
            }
            FailReason::ChildExited { code, early } => {
                let code = code.map_or_else(|| "unknown".to_string(), |c| c.to_string());
                if *early {
                    format!(
                        "step {}: the program exited before the script finished (exit {code})",
                        self.step
                    )
                } else {
                    format!(
                        "step {}: the program exited with a non-zero status (exit {code})",
                        self.step
                    )
                }
            }
            FailReason::OutputMatched { pattern } => format!(
                "step {}: the program output matched --fail-on {pattern:?}",
                self.step
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
            FailReason::ScreenMismatch { pattern, negate } => {
                m.insert("error".into(), "screen mismatch".into());
                m.insert("pattern".into(), pattern.clone().into());
                m.insert("negate".into(), (*negate).into());
            }
            FailReason::ChildExited { code, early } => {
                m.insert("error".into(), "child exited".into());
                if let Some(c) = code {
                    m.insert("exit_code".into(), (*c).into());
                }
                m.insert("early".into(), (*early).into());
            }
            FailReason::OutputMatched { pattern } => {
                m.insert("error".into(), "output matched".into());
                m.insert("pattern".into(), pattern.clone().into());
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
    /// The verb: `press`, `type`, `assert`, `wait-until`, `expect-screen`,
    /// `expect-no-screen`, `capture`, `sleep`, or a failure kind (`child-exit`,
    /// `fail-on`) for a run that failed outside a step.
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
            "expect-screen" => Step::ExpectScreen(parse_pattern(rest, "expect-screen")?),
            "expect-no-screen" => Step::ExpectNoScreen(parse_pattern(rest, "expect-no-screen")?),
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
    /// When `Some(pattern)`, the run fails if the program's output ever contains
    /// `pattern` (e.g. `panicked`). Only a backend that retains the child's
    /// output stream (the PTY backend) can match; others never do.
    pub fail_on: Option<String>,
    /// When `false` (the default), a backend-owned child that exits before the
    /// script finishes, or exits with a non-zero status, fails the run, so a
    /// crash after the last assertion is not read as a pass. Set `true` for a
    /// script that deliberately quits the program. Attach backends own no child,
    /// so this never affects them.
    pub allow_exit: bool,
    /// When `Some(factor)`, every `wait-until` timeout and the `settle` wait are
    /// multiplied by `factor`, so one script runs on a slow or loaded box
    /// without editing each step. `None` means a factor of 1 (no scaling).
    pub timeout_scale: Option<f64>,
}

impl RunOptions {
    /// Default options: no settling, no output guard, crashes fail the run.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable settling, waiting up to `timeout` after each mutating step.
    pub fn with_settle(mut self, timeout: Duration) -> Self {
        self.settle = Some(timeout);
        self
    }

    /// Fail the run if the program's output ever contains `pattern`.
    pub fn with_fail_on(mut self, pattern: impl Into<String>) -> Self {
        self.fail_on = Some(pattern.into());
        self
    }

    /// Allow the program to exit without failing the run (for scripts that
    /// deliberately quit it).
    pub fn with_allow_exit(mut self, allow: bool) -> Self {
        self.allow_exit = allow;
        self
    }

    /// Scale every `wait-until` timeout and the settle wait by `factor`.
    pub fn with_timeout_scale(mut self, factor: f64) -> Self {
        self.timeout_scale = Some(factor);
        self
    }
}

/// Multiply a duration by a scale factor (used for `--timeout-scale`). A
/// non-positive or non-finite factor is ignored (treated as 1), so a bad value
/// never makes a timeout zero or negative.
fn scale_duration(d: Duration, factor: Option<f64>) -> Duration {
    match factor {
        Some(f) if f.is_finite() && f > 0.0 => d.mul_f64(f),
        _ => d,
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
    let settle = opts.settle.map(|d| scale_duration(d, opts.timeout_scale));
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
            Step::ExpectScreen(pattern) | Step::ExpectNoScreen(pattern) => {
                let negate = matches!(step, Step::ExpectNoScreen(_));
                let kind = if negate {
                    "expect-no-screen"
                } else {
                    "expect-screen"
                };
                let present = backend.capture()?.contains(pattern.as_str());
                let ok = present != negate;
                sink.on_event(&RunEvent {
                    t_ms: t_ms(run_start),
                    step: n,
                    kind,
                    detail: pattern.clone(),
                    ok: Some(ok),
                    actual: None,
                });
                if !ok {
                    return Ok(RunResult::Failed(StepFailure {
                        step: n,
                        kind,
                        cond: pattern.clone(),
                        path: "screen".into(),
                        reason: FailReason::ScreenMismatch {
                            pattern: pattern.clone(),
                            negate,
                        },
                    }));
                }
            }
            Step::WaitUntil {
                cond,
                timeout,
                interval,
            } => {
                let timeout = scale_duration(*timeout, opts.timeout_scale);
                let outcome = wait_until(cond, timeout, *interval, || sink.probe());
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
        // After every step: a program that crashed/exited unexpectedly, or
        // output that matched `--fail-on`, fails the run. Attach backends own
        // no child and retain no output, so this is a no-op for them. A clean
        // exit here fails only if a later step still needs to send input; a
        // clean quit followed only by read-only steps (reading the last
        // snapshot) is fine.
        let input_remains = steps[n..]
            .iter()
            .any(|s| matches!(s, Step::Press(_) | Step::Type { .. }));
        if let Some(failure) = child_or_output_failure(backend, opts, n, input_remains) {
            sink.on_event(&failure_event(t_ms(run_start), &failure));
            return Ok(RunResult::Failed(failure));
        }
    }
    // A crash can land in the brief window just after the final step. Only wait
    // for it when that step sent input (the only thing that can trigger one), so
    // a script ending on a read-only step pays no latency.
    if !steps.is_empty() {
        let last_was_input = matches!(steps.last(), Some(Step::Press(_) | Step::Type { .. }));
        if let Some(failure) = final_child_check(backend, opts, steps.len(), last_was_input) {
            sink.on_event(&failure_event(t_ms(run_start), &failure));
            return Ok(RunResult::Failed(failure));
        }
    }
    Ok(RunResult::Passed)
}

/// Run a script attempt up to `1 + retries` times, retrying only a timeout-class
/// failure (see [`StepFailure::is_timeout`]). `attempt` performs one whole run
/// and returns its [`RunResult`]; a caller re-spawns a fresh program and clears
/// the seam inside it, so each attempt is independent. Returns the terminal
/// result and how many attempts were made (at least 1).
///
/// Only a `wait-until` timeout is retried: it is the one failure a slow or
/// loaded environment can cause spuriously. An assertion mismatch, a crash, or
/// an output match is deterministic and returns on the first attempt, so a real
/// failure is never masked by retrying.
pub fn run_with_retries(
    retries: u32,
    mut attempt: impl FnMut() -> anyhow::Result<RunResult>,
) -> anyhow::Result<(RunResult, u32)> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let result = attempt()?;
        let retryable = matches!(&result, RunResult::Failed(f) if f.is_timeout());
        if retryable && attempts <= retries {
            continue;
        }
        return Ok((result, attempts));
    }
}

/// Build the `--events` line for a child-exit / `--fail-on` failure, so every
/// failure emits an event and the event stream stays coherent (not just the
/// per-step assertions).
fn failure_event(t_ms: u64, failure: &StepFailure) -> RunEvent {
    RunEvent {
        t_ms,
        step: failure.step,
        kind: failure.kind,
        detail: failure.cond.clone(),
        ok: Some(false),
        actual: None,
    }
}

/// How long [`final_child_check`] waits after the last step for a program to
/// flush a crash before the run is declared passed, so a panic right after the
/// final assertion still fails. Paid once, only on a child-owning backend, and
/// it returns the instant the child exits.
const CHILD_EXIT_GRACE_MS: u64 = 150;
/// Poll cadence for [`final_child_check`]'s grace wait.
const CHILD_EXIT_POLL_MS: u64 = 10;

/// Fail the run if the program the backend owns exited unexpectedly or its
/// output matched `--fail-on`. Returns `None` for attach backends (no owned
/// child, no retained output) and when all is well.
///
/// `input_remains` is whether a later step would still send input (a `press` or
/// `type`) to the program. A non-zero exit always fails (a crash). A clean exit
/// (code 0) fails only when input remains: exiting while we still need to drive
/// it is a real problem, but exiting when only read-only steps are left (an
/// `assert` or `wait-until` reading the last snapshot) is a clean quit and the
/// run can still pass.
fn child_or_output_failure(
    backend: &dyn PaneBackend,
    opts: &RunOptions,
    step: usize,
    input_remains: bool,
) -> Option<StepFailure> {
    if let Some(pattern) = &opts.fail_on {
        if backend.output_contains(pattern) {
            return Some(StepFailure {
                step,
                kind: "fail-on",
                cond: pattern.clone(),
                path: "output".into(),
                reason: FailReason::OutputMatched {
                    pattern: pattern.clone(),
                },
            });
        }
    }
    if !opts.allow_exit {
        if let Some(ChildExit::Exited(code)) = backend.child_exit() {
            let early = input_remains;
            let nonzero = code != Some(0);
            if early || nonzero {
                return Some(StepFailure {
                    step,
                    kind: "child-exit",
                    cond: "exit".into(),
                    path: String::new(),
                    reason: FailReason::ChildExited { code, early },
                });
            }
        }
    }
    None
}

/// After the last step, give a child-owning backend a brief grace to surface a
/// crash (a non-zero exit, or a `--fail-on` match) that lands just after the
/// final input. A clean exit, or no owned child, ends the wait at once.
///
/// `wait` is whether the last step sent input: only input can trigger a crash,
/// so a script that ends on a read-only step takes a single immediate check and
/// pays no latency. The grace is a best-effort window; a crash that takes longer
/// than [`CHILD_EXIT_GRACE_MS`] to surface after the last keystroke is not caught
/// here (the per-step checks and `--fail-on` remain the primary guards).
fn final_child_check(
    backend: &dyn PaneBackend,
    opts: &RunOptions,
    step: usize,
    wait: bool,
) -> Option<StepFailure> {
    // Nothing to guard: exits are allowed and there is no output pattern.
    if opts.allow_exit && opts.fail_on.is_none() {
        return None;
    }
    // Nothing to wait for: no owned child, or the last step sent no input that
    // could trigger a crash. A single immediate check (a no-op for attach
    // backends) suffices.
    if !wait || backend.child_exit().is_none() {
        return child_or_output_failure(backend, opts, step, false);
    }
    let start = Instant::now();
    loop {
        if let Some(failure) = child_or_output_failure(backend, opts, step, false) {
            return Some(failure);
        }
        if matches!(backend.child_exit(), Some(ChildExit::Exited(_))) {
            // The child has exited. The check above read the status a moment
            // earlier and found it clean, but re-vet once here to close the gap
            // between the two reads (a crash landing between them), then stop.
            return child_or_output_failure(backend, opts, step, false);
        }
        if start.elapsed() >= Duration::from_millis(CHILD_EXIT_GRACE_MS) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(CHILD_EXIT_POLL_MS));
    }
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

/// The literal substring operand of an `expect-screen` / `expect-no-screen`
/// step: the rest of the line with surrounding whitespace trimmed. Errors when
/// empty so a bare `expect-screen` is a usage error, not a match-everything.
fn parse_pattern(rest: &str, verb: &str) -> anyhow::Result<String> {
    let pattern = rest.trim();
    if pattern.is_empty() {
        anyhow::bail!("{verb} needs a pattern to match");
    }
    Ok(pattern.to_string())
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
        assert_eq!(
            Step::parse("expect-screen 1/2/3 day/week")
                .unwrap()
                .unwrap(),
            Step::ExpectScreen("1/2/3 day/week".to_string())
        );
        assert_eq!(
            Step::parse("expect-no-screen panicked").unwrap().unwrap(),
            Step::ExpectNoScreen("panicked".to_string())
        );
        // The pattern is the whole remainder (spaces kept), surrounding
        // whitespace trimmed.
        assert_eq!(
            Step::parse("expect-screen   padded  ").unwrap().unwrap(),
            Step::ExpectScreen("padded".to_string())
        );
    }

    #[test]
    fn expect_screen_needs_a_pattern() {
        assert!(Step::parse("expect-screen").is_err());
        assert!(Step::parse("expect-screen    ").is_err());
        assert!(Step::parse("expect-no-screen").is_err());
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
        mock_with_screen("SCREEN")
    }

    fn mock_with_screen(screen: &str) -> MockBackend {
        MockBackend {
            sent: RefCell::new(Vec::new()),
            pasted: RefCell::new(Vec::new()),
            screen: screen.to_string(),
        }
    }

    /// A backend that stands in for the PTY backend in child-exit / output
    /// tests: it reports a scripted [`ChildExit`] and whether its output has
    /// contained a needle, without spawning anything.
    struct ProgramBackend {
        exit: Option<ChildExit>,
        output: String,
    }

    impl PaneBackend for ProgramBackend {
        fn send_keys(&self, _keys: &[Key]) -> io::Result<()> {
            Ok(())
        }
        fn capture(&self) -> io::Result<String> {
            Ok(String::new())
        }
        fn child_exit(&self) -> Option<ChildExit> {
            self.exit
        }
        fn output_contains(&self, needle: &str) -> bool {
            self.output.contains(needle)
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

        let screen_missing = StepFailure {
            step: 4,
            kind: "expect-screen",
            cond: "Ready".into(),
            path: "screen".into(),
            reason: FailReason::ScreenMismatch {
                pattern: "Ready".into(),
                negate: false,
            },
        };
        let j = screen_missing.to_json();
        assert_eq!(j["error"], json!("screen mismatch"));
        assert_eq!(j["pattern"], json!("Ready"));
        assert_eq!(j["negate"], json!(false));
        assert!(
            screen_missing
                .human()
                .contains("expect-screen \"Ready\" but the screen did not contain it")
        );

        let screen_present = StepFailure {
            step: 5,
            kind: "expect-no-screen",
            cond: "panicked".into(),
            path: "screen".into(),
            reason: FailReason::ScreenMismatch {
                pattern: "panicked".into(),
                negate: true,
            },
        };
        assert_eq!(screen_present.to_json()["negate"], json!(true));
        assert!(
            screen_present
                .human()
                .contains("expect-no-screen \"panicked\" but the screen contained it")
        );

        let crashed = StepFailure {
            step: 6,
            kind: "child-exit",
            cond: "exit".into(),
            path: String::new(),
            reason: FailReason::ChildExited {
                code: Some(101),
                early: false,
            },
        };
        let j = crashed.to_json();
        assert_eq!(j["error"], json!("child exited"));
        assert_eq!(j["exit_code"], json!(101));
        assert_eq!(j["early"], json!(false));
        assert!(
            crashed
                .human()
                .contains("exited with a non-zero status (exit 101)")
        );

        // An early exit, and one with no reported code (killed by a signal).
        let early_signal = StepFailure {
            step: 2,
            kind: "child-exit",
            cond: "exit".into(),
            path: String::new(),
            reason: FailReason::ChildExited {
                code: None,
                early: true,
            },
        };
        let j = early_signal.to_json();
        assert!(
            j.get("exit_code").is_none(),
            "an unknown code is omitted from json"
        );
        assert_eq!(j["early"], json!(true));
        assert!(
            early_signal
                .human()
                .contains("exited before the script finished (exit unknown)")
        );

        let matched = StepFailure {
            step: 7,
            kind: "fail-on",
            cond: "panicked".into(),
            path: "output".into(),
            reason: FailReason::OutputMatched {
                pattern: "panicked".into(),
            },
        };
        let j = matched.to_json();
        assert_eq!(j["error"], json!("output matched"));
        assert_eq!(j["pattern"], json!("panicked"));
        assert!(
            matched
                .human()
                .contains("output matched --fail-on \"panicked\"")
        );
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
    fn expect_screen_checks_rendered_text() {
        // Present -> passes; absent -> fails, naming the pattern.
        let backend = mock_with_screen("Loading...\n1/2/3 day/week/month\nReady");
        let steps = parse_script("expect-screen 1/2/3 day/week/month").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);

        let steps = parse_script("expect-screen NoSuchLegend").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap() else {
            panic!("a missing substring must fail");
        };
        assert_eq!(f.kind, "expect-screen");
        assert!(matches!(
            f.reason,
            FailReason::ScreenMismatch { negate: false, .. }
        ));
    }

    #[test]
    fn expect_no_screen_checks_absence() {
        // Absent -> passes; present -> fails. This is the bug class a correct
        // seam cannot see (a stack trace rendered on screen).
        let clean = mock_with_screen("all good here");
        let steps = parse_script("expect-no-screen panicked").unwrap();
        assert_eq!(
            run_script(&steps, &clean, &RunOptions::new(), &mut probe_sink(|| None)).unwrap(),
            RunResult::Passed
        );

        let crashed = mock_with_screen("thread 'main' panicked at src/x.rs");
        let RunResult::Failed(f) = run_script(
            &steps,
            &crashed,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap() else {
            panic!("a present substring must fail expect-no-screen");
        };
        assert_eq!(f.kind, "expect-no-screen");
        assert!(matches!(
            f.reason,
            FailReason::ScreenMismatch { negate: true, .. }
        ));
    }

    #[test]
    fn a_non_zero_child_exit_fails_the_run() {
        // The program crashes after the last (passing) step; the seam is fine,
        // but the run must still fail.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(101))),
            output: String::new(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap() else {
            panic!("a non-zero exit must fail the run even when the seam passed");
        };
        assert_eq!(f.kind, "child-exit");
        assert!(matches!(
            f.reason,
            FailReason::ChildExited {
                code: Some(101),
                early: false
            }
        ));
    }

    #[test]
    fn a_clean_exit_before_read_only_steps_passes() {
        // The quit-app flow: press the quit key, then read the saved state to
        // confirm it quit. The program exits cleanly (code 0) after the press,
        // and the only remaining step is a read-only assert against the last
        // snapshot. That is a clean quit, not an early exit, so the run passes.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(0))),
            output: String::new(),
        };
        let steps = parse_script("press a\nassert ready=true").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(
            out,
            RunResult::Passed,
            "a clean exit followed only by read-only steps is a clean quit, not an early exit"
        );
    }

    #[test]
    fn a_clean_exit_before_a_later_input_step_fails() {
        // A clean exit is still a failure when a later step would send input to
        // the now-dead program: we cannot drive something that has quit.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(0))),
            output: String::new(),
        };
        let steps = parse_script("press a\npress b").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap() else {
            panic!("exiting while an input step still remains must fail");
        };
        assert!(matches!(
            f.reason,
            FailReason::ChildExited { early: true, .. }
        ));
        assert_eq!(
            f.step, 1,
            "the failure is reported at the step after which it was seen"
        );
    }

    #[test]
    fn a_nonzero_exit_before_read_only_steps_still_fails() {
        // A crash (non-zero exit) is always a failure, even if only read-only
        // steps remain: the refined clean-quit rule must not swallow a crash.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(3))),
            output: String::new(),
        };
        let steps = parse_script("press a\nassert ready=true").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap() else {
            panic!("a non-zero exit must fail even with only read-only steps left");
        };
        assert!(matches!(
            f.reason,
            FailReason::ChildExited {
                code: Some(3),
                early: false
            }
        ));
    }

    fn timeout_failure() -> RunResult {
        RunResult::Failed(StepFailure {
            step: 1,
            kind: "wait-until",
            cond: "ready=true".into(),
            path: "ready".into(),
            reason: FailReason::TimedOut {
                timeout_ms: 100,
                observed: Observed::Missing,
            },
        })
    }

    fn assert_failure() -> RunResult {
        RunResult::Failed(StepFailure {
            step: 1,
            kind: "assert",
            cond: "x=1".into(),
            path: "x".into(),
            reason: FailReason::Unmet {
                observed: Observed::Missing,
            },
        })
    }

    #[test]
    fn run_with_retries_retries_a_timeout_then_passes() {
        let mut n = 0;
        let (res, attempts) = run_with_retries(2, || {
            n += 1;
            Ok(if n < 3 {
                timeout_failure()
            } else {
                RunResult::Passed
            })
        })
        .unwrap();
        assert_eq!(attempts, 3, "two timeouts then a pass is three attempts");
        assert_eq!(res, RunResult::Passed);
    }

    #[test]
    fn run_with_retries_does_not_retry_a_non_timeout_failure() {
        let mut n = 0;
        let (res, attempts) = run_with_retries(5, || {
            n += 1;
            Ok(assert_failure())
        })
        .unwrap();
        assert_eq!(attempts, 1, "a deterministic failure is never retried");
        assert!(matches!(res, RunResult::Failed(f) if !f.is_timeout()));
    }

    #[test]
    fn run_with_retries_exhausts_then_returns_the_last_timeout() {
        let (res, attempts) = run_with_retries(1, || Ok(timeout_failure())).unwrap();
        assert_eq!(attempts, 2, "one retry allows at most two attempts");
        assert!(matches!(res, RunResult::Failed(f) if f.is_timeout()));
    }

    #[test]
    fn run_with_retries_zero_runs_once() {
        let (_res, attempts) = run_with_retries(0, || Ok(timeout_failure())).unwrap();
        assert_eq!(attempts, 1, "no retries means a single attempt");
    }

    #[test]
    fn scale_duration_scales_and_ignores_bad_factors() {
        let base = Duration::from_millis(100);
        assert_eq!(scale_duration(base, Some(2.0)), Duration::from_millis(200));
        assert_eq!(scale_duration(base, None), base);
        // A non-positive or non-finite factor is ignored, never zeroing a wait.
        assert_eq!(scale_duration(base, Some(0.0)), base);
        assert_eq!(scale_duration(base, Some(-1.0)), base);
        assert_eq!(scale_duration(base, Some(f64::NAN)), base);
        assert_eq!(scale_duration(base, Some(f64::INFINITY)), base);
    }

    #[test]
    fn timeout_scale_reaches_the_wait() {
        // A wait-until that cannot be satisfied reports the SCALED timeout,
        // proving the factor reaches the wait, not just scale_duration alone.
        let steps = parse_script("wait-until ready=true --timeout-ms 20 --interval-ms 5").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &mock(),
            &RunOptions::new().with_timeout_scale(3.0),
            &mut probe_sink(|| Some(json!({ "ready": false }))),
        )
        .unwrap() else {
            panic!("an unsatisfiable wait-until must time out");
        };
        assert!(
            matches!(f.reason, FailReason::TimedOut { timeout_ms: 60, .. }),
            "the reported timeout must be the scaled 60 ms, got {:?}",
            f.reason
        );
    }

    #[test]
    fn a_clean_exit_on_the_last_step_passes() {
        // A script that deliberately quits the app on its final step is a pass:
        // a zero exit at the end is normal, not an early/non-zero crash.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(0))),
            output: String::new(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);
    }

    #[test]
    fn allow_exit_suppresses_the_child_exit_failure() {
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(1))),
            output: String::new(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new().with_allow_exit(true),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(
            out,
            RunResult::Passed,
            "--allow-exit must tolerate the exit"
        );
    }

    #[test]
    fn fail_on_matches_the_child_output() {
        let backend = ProgramBackend {
            exit: Some(ChildExit::Running),
            output: "booting\nthread panicked at lib.rs\n".to_string(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new().with_fail_on("panicked"),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap() else {
            panic!("--fail-on must fail a run whose output matched");
        };
        assert_eq!(f.kind, "fail-on");
        assert!(matches!(f.reason, FailReason::OutputMatched { .. }));
    }

    #[test]
    fn fail_on_passes_when_the_output_is_clean() {
        let backend = ProgramBackend {
            exit: Some(ChildExit::Running),
            output: "booting\nall good\n".to_string(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new().with_fail_on("panicked"),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);
    }

    /// A backend whose child reports `Running` for the first `flip_after`
    /// status reads, then a sticky non-zero `Exited` — to drive
    /// `final_child_check`'s grace loop deterministically by call count, not
    /// wall-clock time. Sticky-once-exited mirrors a real `try_wait`.
    struct ToggleExitBackend {
        calls: RefCell<u32>,
        flip_after: u32,
        code: i32,
    }

    impl PaneBackend for ToggleExitBackend {
        fn send_keys(&self, _: &[Key]) -> io::Result<()> {
            Ok(())
        }
        fn capture(&self) -> io::Result<String> {
            Ok(String::new())
        }
        fn child_exit(&self) -> Option<ChildExit> {
            let mut n = self.calls.borrow_mut();
            let exited = *n >= self.flip_after;
            *n += 1;
            if exited {
                Some(ChildExit::Exited(Some(self.code)))
            } else {
                Some(ChildExit::Running)
            }
        }
    }

    #[test]
    fn the_grace_loop_catches_a_crash_just_after_the_last_input() {
        // The child is still Running at the checks right after the last
        // keystroke, then exits non-zero while the grace loop polls. Because the
        // last step sent input, the grace is armed, and the loop must catch it.
        let backend = ToggleExitBackend {
            calls: RefCell::new(0),
            flip_after: 2,
            code: 101,
        };
        let steps = parse_script("press x").unwrap();
        let RunResult::Failed(f) = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| None),
        )
        .unwrap() else {
            panic!("a crash landing within the grace window must fail the run");
        };
        assert_eq!(f.kind, "child-exit");
        assert!(matches!(
            f.reason,
            FailReason::ChildExited { early: false, .. }
        ));
    }

    #[test]
    fn a_read_only_last_step_skips_the_grace_wait() {
        // When the last step is read-only (no input to trigger a crash), the
        // final check is immediate: a child that is still Running is not waited
        // on, so the run passes without the grace latency.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Running),
            output: String::new(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &backend,
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);
    }

    #[test]
    fn child_exit_failure_emits_an_event() {
        // The event stream must stay coherent: a child-exit failure emits an
        // event, like every assertion failure, not just an abrupt Failed result.
        let backend = ProgramBackend {
            exit: Some(ChildExit::Exited(Some(1))),
            output: String::new(),
        };
        let steps = parse_script("assert ready=true").unwrap();
        let mut sink = TestSink::new(|| Some(json!({ "ready": true })));
        let out = run_script(&steps, &backend, &RunOptions::new(), &mut sink).unwrap();
        assert!(matches!(out, RunResult::Failed(_)));
        let last = sink.events.last().expect("an event for the child-exit");
        assert_eq!(last.kind, "child-exit");
        assert_eq!(last.ok, Some(false));
    }

    #[test]
    fn fail_on_never_matches_an_attach_backend() {
        // MockBackend does not override output_contains, so it uses the trait
        // default (`false`). A `--fail-on` run against it must pass: an attach
        // backend retains no output to match. (If the default were `true`, this
        // would spuriously fail.)
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new().with_fail_on("panicked"),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);
    }

    #[test]
    fn attach_backends_never_trip_the_child_exit_guard() {
        // A backend that owns no child (child_exit None) must pass even though
        // --allow-exit is off: the guard is for owned processes only.
        let steps = parse_script("assert ready=true").unwrap();
        let out = run_script(
            &steps,
            &mock(),
            &RunOptions::new(),
            &mut probe_sink(|| Some(json!({ "ready": true }))),
        )
        .unwrap();
        assert_eq!(out, RunResult::Passed);
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
