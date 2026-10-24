//! `panedrive` CLI, drive a terminal UI from a shell or an agent.
//!
//! ```text
//! panedrive press   "2 Down Down Enter" --pane mysession:0.0
//! panedrive capture --pane mysession:0.0
//! panedrive wait-until "focus=fleet"     --state run.state.json --timeout-ms 5000
//! panedrive assert     "bag.count=2"     --state run.state.json
//! ```
//!
//! Exit codes: `0` success / condition held, `1` condition failed or timed out,
//! `2` usage or backend error. That makes `assert` and `wait-until` usable as
//! shell gates and in CI.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use panedrive::{
    ClosureSink, Key, Observed, PaneBackend, RunEvent, RunOptions, RunResult, ScreenBackend,
    TmuxBackend, WaitOutcome, ZellijBackend, condition::Condition, driver, key, parse_script,
    run_script, run_with_retries, seam,
};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "panedrive",
    version = env!("PANEDRIVE_VERSION"),
    about = "Drive and verify terminal UIs headlessly (panekit)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
// `Run` carries the large `RunArgs`; the others are small. This enum is built
// once per process from argv, so the size difference does not matter, and clap's
// derive cannot flatten a boxed `Args` variant.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Send keys to a pane, e.g. `2 Down Down Enter` or `C-c`. Accepts the keys
    /// as one quoted spec ("2 Down Enter") or as separate args (2 Down Enter).
    Press {
        /// Key spec: names/chars, space- or comma-separated (`Enter Down C-c q`).
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
        #[arg(long)]
        pane: String,
        #[arg(long, value_enum, default_value_t = Backend::Tmux)]
        backend: Backend,
    },
    /// Type a literal string into the pane (e.g. a value into a text field).
    ///
    /// For secrets, read the value from `--stdin` or `--from-env` so it never
    /// lands in argv or shell history, and prefer the PTY backend or tmux
    /// `--paste` so it does not transit `tmux send-keys` argv either.
    Type {
        /// The literal text to type. Omit when using --stdin or --from-env.
        text: Option<String>,
        /// Read the text from stdin (covers files, fifos, and `read -s` pipes).
        #[arg(long)]
        stdin: bool,
        /// Read the text from the named environment variable.
        #[arg(long, value_name = "VAR")]
        from_env: Option<String>,
        #[arg(long)]
        pane: String,
        #[arg(long, value_enum, default_value_t = Backend::Tmux)]
        backend: Backend,
        /// tmux only: deliver via a tmux buffer (load-buffer + paste-buffer) so
        /// the text never transits `tmux send-keys` argv. The PTY backend is
        /// already argv-safe, so this is a no-op there.
        #[arg(long)]
        paste: bool,
    },
    /// Print the pane's visible text (fallback when no JSON seam exists).
    Capture {
        #[arg(long)]
        pane: String,
        #[arg(long, value_enum, default_value_t = Backend::Tmux)]
        backend: Backend,
        /// Preserve colour and text attributes as an ANSI escape stream instead
        /// of flattening to plain text (pty backend; others fall back to plain).
        #[arg(long)]
        ansi: bool,
    },
    /// Poll the state seam until a condition holds (or time out).
    WaitUntil {
        /// Condition over the state JSON, e.g. `focus=fleet` or `bag.count!=0`.
        cond: String,
        #[arg(long, env = "PANEDRIVE_STATE")]
        state: PathBuf,
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 50)]
        interval_ms: u64,
        /// Emit the result as a JSON object on stdout (exit code unchanged).
        #[arg(long)]
        json: bool,
    },
    /// Show the state seam: pretty-print it, or (with `--paths`) list every
    /// assertable dot-path so you can see what conditions can target.
    State {
        #[arg(long, env = "PANEDRIVE_STATE")]
        state: PathBuf,
        /// List each assertable dot-path with its value and type, instead of
        /// pretty-printing the whole document.
        #[arg(long)]
        paths: bool,
    },
    /// Check a JSON file against the state-seam contract (an object with scalar
    /// leaves). Useful when bringing up a non-Rust seam adapter. Exit `0` if
    /// clean, `1` on a violation.
    ValidateSeam {
        /// Path to the JSON seam file to check.
        file: PathBuf,
        /// Emit the report as a JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate a condition against the state seam once (no waiting).
    Assert {
        /// Condition over the state JSON.
        cond: String,
        #[arg(long, env = "PANEDRIVE_STATE")]
        state: PathBuf,
        /// Emit the result as a JSON object on stdout (the exit code is
        /// unchanged), e.g. `{"ok":false,"cond":"count=2","actual":"1"}`.
        #[arg(long)]
        json: bool,
    },
    /// Record the state seam over a window, printing each observed state as a
    /// JSONL line (`{"t_ms":..,"state":..}`), catches transitions a single
    /// assert would miss. Pair with `press` to record what the UI does.
    Watch {
        #[arg(long, env = "PANEDRIVE_STATE")]
        state: PathBuf,
        /// How long to record for.
        #[arg(long, default_value_t = 5000)]
        for_ms: u64,
        /// Sampling interval.
        #[arg(long, default_value_t = 100)]
        interval_ms: u64,
        /// Only print a line when the state changed from the previous one.
        #[arg(long)]
        distinct: bool,
    },
    /// Run a script of steps (one per line) against one backend in a single
    /// process. This is the only way to drive the PTY backend, which spawns and
    /// owns the target program, from the CLI.
    ///
    /// tmux/zellij attach to a running pane (pass `--pane`); pty spawns the
    /// program given after `--`. Steps: `press`, `type`, `wait-until`,
    /// `assert`, `capture`, `sleep`. Exit `0` if all pass, `1` if an assert or
    /// wait-until fails, `2` on a usage or backend error.
    Run(RunArgs),
    /// Record an interactive session into a script: spawn a program in a PTY,
    /// forward your keystrokes to it, and write what you pressed as a `.pds`.
    /// Press Ctrl-] to stop. Requires building with `--features pty`.
    Record {
        /// Where to write the recorded script.
        #[arg(long)]
        script: PathBuf,
        /// Also record an asciinema v2 cast of the session to this path.
        #[arg(long)]
        cast: Option<PathBuf>,
        /// Rows of the spawned pseudo-terminal.
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Columns of the spawned pseudo-terminal.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// The program to spawn and its args, given after `--`.
        #[arg(last = true)]
        program: Vec<String>,
    },
}

/// Arguments to the `run` subcommand.
#[derive(clap::Args)]
struct RunArgs {
    /// Path to the script file (one step per line; `#` comments allowed). Omit it
    /// when passing steps inline with `-e`.
    script: Option<PathBuf>,
    /// Inline script step, repeatable: `-e "press Enter" -e "assert ready=true"`.
    /// Each `-e` is one script line. Mutually exclusive with the script path.
    #[arg(short = 'e', long = "inline", value_name = "STEP")]
    inline: Vec<String>,
    #[arg(long, value_enum, default_value_t = Backend::Tmux)]
    backend: Backend,
    /// State seam path that `assert` / `wait-until` steps read (defaults to
    /// $PANEDRIVE_STATE). Point the app's snapshot writer at the same path.
    #[arg(long, env = "PANEDRIVE_STATE")]
    state: Option<PathBuf>,
    /// Target for the attach backends: a tmux pane, or a zellij session.
    #[arg(long)]
    pane: Option<String>,
    /// pty only: the program to spawn and its args, given after `--`.
    #[arg(last = true)]
    program: Vec<String>,
    /// pty only: rows of the spawned pseudo-terminal.
    #[arg(long, default_value_t = 24)]
    rows: u16,
    /// pty only: columns of the spawned pseudo-terminal.
    #[arg(long, default_value_t = 80)]
    cols: u16,
    /// After each key/type step, wait up to ~1s for the seam to change
    /// before the next step, so an `assert` does not race a stale snapshot.
    #[arg(long)]
    settle: bool,
    /// After each key/type step, wait up to ~1s for the app to acknowledge the
    /// input by advancing `_meta.seq` (the paneview SnapshotWriter frame
    /// counter), a precise alternative to `--settle`. Falls back to `--settle`'s
    /// change detection when the seam has no `_meta.seq`. Not combined with
    /// `--settle`.
    #[arg(long)]
    ack: bool,
    /// Evaluate conditions against the captured screen text (`screen`,
    /// `lines.<n>`) instead of a JSON state file, for apps with no seam.
    #[arg(long)]
    from_capture: bool,
    /// Emit a JSON run summary on stdout: `{ok, failed_step?, steps:[..]}`.
    #[arg(long)]
    json: bool,
    /// Write a JSONL event track (one line per step, timestamped) to this
    /// path, for aligning a `--cast` recording or feeding CI/an agent.
    #[arg(long)]
    events: Option<PathBuf>,
    /// Record an asciinema v2 cast to this path. Supported on the pty and
    /// tmux backends (which expose a byte stream); not screen or zellij.
    #[arg(long)]
    cast: Option<PathBuf>,
    /// pty only: fail the run if the program's output ever contains this
    /// literal substring (e.g. `panicked`), catching a crash after the last
    /// assertion that the seam would otherwise miss.
    #[arg(long, value_name = "PATTERN")]
    fail_on: Option<String>,
    /// pty only: do not fail the run when the program exits. By default a
    /// program that exits before the script ends, or exits non-zero, fails the
    /// run; set this for a script that deliberately quits the program.
    #[arg(long)]
    allow_exit: bool,
    /// Multiply every `wait-until` timeout and the `--settle` wait by this
    /// factor, so one script runs on a slow or loaded box without editing each
    /// step. For example `--timeout-scale 2` doubles every wait.
    #[arg(long, value_name = "FACTOR")]
    timeout_scale: Option<f64>,
    /// pty only: if the run fails because a `wait-until` timed out, re-run the
    /// whole script up to this many more times. Only a timeout is retried; an
    /// assertion mismatch or a crash is deterministic and never retried.
    #[arg(long, default_value_t = 0, value_name = "N")]
    retry_timeouts: u32,
    /// On failure, write a small bundle (final screen, last seam snapshot, event
    /// log, and a manifest) to this directory so a failure can be inspected
    /// without a re-run. Secrets from `type --from-env` are never recorded.
    #[arg(long, value_name = "DIR")]
    artifacts: Option<PathBuf>,
    /// Record or refresh goldens instead of asserting them: each
    /// `expect-golden` step writes the current screen to its file and passes.
    #[arg(long, env = "PANEDRIVE_UPDATE_GOLDENS")]
    update_goldens: bool,
    /// Run the script once per value of a variable (a parameter sweep):
    /// `--sweep SEED=1..8` or `--sweep MODE=a,b,c`. The value is set as a child
    /// environment variable. Emits one JSON result line per run. pty backend.
    #[arg(long, value_name = "VAR=RANGE")]
    sweep: Option<String>,
    /// Run up to N cells of a suite (matrix or sweep) in parallel. Requires a
    /// multi-cell run. Defaults to 1 (sequential).
    #[arg(long, default_value_t = 1, value_name = "N")]
    jobs: usize,
}

#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum Backend {
    /// Attach to a running tmux pane.
    Tmux,
    /// Attach to a running zellij session (the `--pane` value is the session
    /// name).
    Zellij,
    /// Attach to a running GNU screen session (the `--pane` value is the session
    /// name).
    Screen,
    /// Spawn the program in an owned PTY. Only valid for `run` (which supplies
    /// the program after `--`), and requires building with `--features pty`.
    Pty,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("panedrive: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.cmd {
        Cmd::Press {
            keys,
            pane,
            backend,
        } => cmd_press(keys, pane, backend),
        Cmd::Type {
            text,
            stdin,
            from_env,
            pane,
            backend,
            paste,
        } => cmd_type(text, stdin, from_env, pane, backend, paste),
        Cmd::Capture {
            pane,
            backend,
            ansi,
        } => cmd_capture(pane, backend, ansi),
        Cmd::WaitUntil {
            cond,
            state,
            timeout_ms,
            interval_ms,
            json,
        } => cmd_wait_until(cond, state, timeout_ms, interval_ms, json),
        Cmd::State { state, paths } => cmd_state(state, paths),
        Cmd::ValidateSeam { file, json } => cmd_validate_seam(file, json),
        Cmd::Assert { cond, state, json } => cmd_assert(cond, state, json),
        Cmd::Watch {
            state,
            for_ms,
            interval_ms,
            distinct,
        } => cmd_watch(state, for_ms, interval_ms, distinct),
        Cmd::Run(args) => cmd_run(args),
        Cmd::Record {
            script,
            cast,
            rows,
            cols,
            program,
        } => spawn_record(script, cast, rows, cols, program),
    }
}

/// `press`: send a key spec to a pane.
fn cmd_press(keys: Vec<String>, pane: String, backend: Backend) -> anyhow::Result<ExitCode> {
    // Join so a quoted spec ("2 Down Enter") and separate args
    // (2 Down Enter) both parse identically.
    let keys = key::parse_keys(&keys.join(" "))?;
    backend_for(backend, pane)?.send_keys(&keys)?;
    Ok(ExitCode::SUCCESS)
}

/// `type`: type literal or secret text into a pane.
fn cmd_type(
    text: Option<String>,
    stdin: bool,
    from_env: Option<String>,
    pane: String,
    backend: Backend,
    paste: bool,
) -> anyhow::Result<ExitCode> {
    let text = resolve_type_text(text, stdin, from_env)?;
    let backend = backend_for(backend, pane)?;
    if paste {
        backend.paste_text(&text)?;
    } else {
        let keys: Vec<Key> = text.chars().map(Key::Char).collect();
        backend.send_keys(&keys)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `capture`: print a pane's visible text, optionally with ANSI styling.
fn cmd_capture(pane: String, backend: Backend, ansi: bool) -> anyhow::Result<ExitCode> {
    let b = backend_for(backend, pane)?;
    let text = if ansi {
        b.capture_styled()?
    } else {
        b.capture()?
    };
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// `wait-until`: poll the seam until the condition holds or time out.
fn cmd_wait_until(
    cond: String,
    state: PathBuf,
    timeout_ms: u64,
    interval_ms: u64,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let cond = Condition::parse(&cond)?;
    let outcome = driver::wait_until(
        &cond,
        Duration::from_millis(timeout_ms),
        Duration::from_millis(interval_ms),
        || driver::read_state_file(&state),
    );
    match outcome {
        WaitOutcome::Satisfied(took) => {
            let took = took.as_millis() as u64;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true, "cond": cond.to_spec(), "waited_ms": took,
                    })
                );
            } else {
                eprintln!("held after {took} ms");
            }
            Ok(ExitCode::SUCCESS)
        }
        WaitOutcome::TimedOut => {
            // Read once more so the message/JSON can report what the seam last
            // held, not just that it timed out.
            let observed = driver::read_state_file(&state)
                .map(|v| cond.observed(&v))
                .unwrap_or(Observed::Missing);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false, "cond": cond.to_spec(), "path": cond.path(),
                        "timeout_ms": timeout_ms, "actual": observed.to_json(),
                    })
                );
            } else {
                eprintln!(
                    "timed out after {timeout_ms} ms ({})",
                    observed.describe(cond.path())
                );
            }
            Ok(ExitCode::from(1))
        }
    }
}

/// `state`: pretty-print the seam, or list its assertable dot-paths.
fn cmd_state(state: PathBuf, paths: bool) -> anyhow::Result<ExitCode> {
    let value = driver::read_state_file(&state)
        .ok_or_else(|| anyhow::anyhow!("no readable JSON state at {}", state.display()))?;
    if paths {
        let leaves = seam::leaves(&value);
        if leaves.is_empty() {
            eprintln!("(no assertable paths)");
        }
        let width = leaves.iter().map(|l| l.path.len()).max().unwrap_or(0);
        for l in &leaves {
            println!("{:<width$} = {}  ({})", l.path, l.value, l.kind);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(ExitCode::SUCCESS)
}

/// `validate-seam`: check a JSON file against the seam contract.
fn cmd_validate_seam(file: PathBuf, json: bool) -> anyhow::Result<ExitCode> {
    let bytes =
        std::fs::read(&file).map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
    let report = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => seam::validate(&value),
        Err(e) => seam::SeamReport {
            ok: false,
            problems: vec![format!("invalid JSON: {e}")],
            paths: 0,
        },
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": report.ok, "paths": report.paths,
                "problems": report.problems,
            })
        );
    } else if report.ok {
        println!("seam ok: {} assertable path(s)", report.paths);
    } else {
        for p in &report.problems {
            eprintln!("seam problem: {p}");
        }
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `assert`: evaluate a condition against the seam once.
fn cmd_assert(cond: String, state: PathBuf, json: bool) -> anyhow::Result<ExitCode> {
    let cond = Condition::parse(&cond)?;
    match driver::read_state_file(&state) {
        Some(value) => {
            let ok = cond.eval(&value);
            if json {
                let actual = cond.observed(&value).to_json();
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": ok, "cond": cond.to_spec(),
                        "path": cond.path(), "actual": actual,
                    })
                );
            } else if !ok {
                eprintln!("assert failed: {}", cond.explain(&value));
            }
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        None => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false, "cond": cond.to_spec(),
                        "error": "no readable state",
                    })
                );
            } else {
                eprintln!("no readable state at {}", state.display());
            }
            Ok(ExitCode::from(1))
        }
    }
}

/// `watch`: record the seam over a window as JSONL transitions.
fn cmd_watch(
    state: PathBuf,
    for_ms: u64,
    interval_ms: u64,
    distinct: bool,
) -> anyhow::Result<ExitCode> {
    let n = driver::watch(
        Duration::from_millis(for_ms),
        Duration::from_millis(interval_ms),
        distinct,
        || driver::read_state_file(&state),
        |t, v| {
            let line = serde_json::json!({ "t_ms": t.as_millis() as u64, "state": v });
            println!("{line}");
        },
    );
    eprintln!("recorded {n} sample(s)");
    Ok(ExitCode::SUCCESS)
}

/// `run`: execute a script of steps against one backend in one process.
fn cmd_run(args: RunArgs) -> anyhow::Result<ExitCode> {
    let RunArgs {
        script,
        inline,
        backend,
        state,
        pane,
        program,
        rows,
        cols,
        settle,
        ack,
        from_capture,
        json,
        events,
        cast,
        fail_on,
        allow_exit,
        timeout_scale,
        retry_timeouts,
        artifacts,
        update_goldens,
        sweep,
        jobs,
    } = args;
    if cast.is_some() && !matches!(backend, Backend::Pty | Backend::Tmux) {
        anyhow::bail!("--cast is supported on the pty and tmux backends only");
    }
    // --fail-on / --allow-exit observe a program the backend owns; only the pty
    // backend owns its child, so reject them elsewhere rather than silently
    // ignoring them.
    if (fail_on.is_some() || allow_exit) && !matches!(backend, Backend::Pty) {
        anyhow::bail!("--fail-on and --allow-exit are supported on the pty backend only");
    }
    // Retry re-spawns the program for each attempt, so it needs a backend that
    // owns its child. Attach backends drive a process they do not own.
    if retry_timeouts > 0 && !matches!(backend, Backend::Pty) {
        anyhow::bail!("--retry-timeouts is supported on the pty backend only");
    }
    // A cast records one spawned session; re-spawning for a retry would clobber
    // it, so the two are not combined.
    if retry_timeouts > 0 && cast.is_some() {
        anyhow::bail!("--retry-timeouts cannot be combined with --cast");
    }
    if let Some(f) = timeout_scale {
        if !(f.is_finite() && f > 0.0) {
            anyhow::bail!("--timeout-scale must be a positive number");
        }
    }
    // Both wait after each mutating step; running both is redundant and their
    // precedence would be surprising, so take exactly one.
    if settle && ack {
        anyhow::bail!("--settle and --ack both wait after each step; pass only one");
    }
    // Exactly one source of steps: a script-file path, or one or more inline `-e`
    // lines. Both is ambiguous; neither leaves nothing to run. Either way the
    // text is fed to the same parser, so inline needs no separate grammar.
    let text = match (&script, inline.is_empty()) {
        (Some(_), false) => {
            anyhow::bail!("pass a script path or inline steps with -e, not both");
        }
        (None, true) => {
            anyhow::bail!("no script: pass a script path or at least one -e <step>");
        }
        (Some(path), true) => std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading script {}: {e}", path.display()))?,
        (None, false) => inline.join("\n"),
    };
    let is_pty = matches!(backend, Backend::Pty);
    let steps = parse_script(&text)?;

    // A run is a *suite* when the script declares a `#@size:`/`#@env:` directive or
    // a `--sweep` is given: the script runs once per cell (the cartesian product
    // of the axes). This is a separate path; a plain single run falls through to
    // the existing machinery below untouched.
    let axes = panedrive::parse_suite_header(&text)?;
    let sweep_axis = match &sweep {
        Some(spec) => Some(panedrive::parse_sweep(spec)?),
        None => None,
    };
    let is_suite = !axes.is_empty() || sweep_axis.is_some();
    if jobs == 0 {
        anyhow::bail!("--jobs must be at least 1");
    }
    if is_suite {
        return run_suite(SuiteRun {
            steps: &steps,
            opts: RunOptions {
                settle: settle.then(|| Duration::from_millis(1000)),
                ack: ack.then(|| Duration::from_millis(1000)),
                fail_on,
                allow_exit,
                timeout_scale,
                update_goldens,
            },
            backend,
            program,
            base_rows: rows,
            base_cols: cols,
            state: state.clone(),
            from_capture,
            json,
            jobs,
            axes,
            sweep_axis,
            cast: cast.is_some(),
            events: events.is_some(),
            artifacts: artifacts.clone(),
            retry_timeouts,
        });
    }
    if jobs > 1 {
        anyhow::bail!(
            "--jobs is only meaningful for a suite (pass --sweep or a `#@size:`/`#@env:` directive)"
        );
    }
    // The pty backend records the cast via an output tap; tmux records via a
    // pipe-pane recorder. Route the one cast path to whichever the backend can
    // stream from.
    let pty_cast = matches!(backend, Backend::Pty)
        .then(|| cast.clone())
        .flatten();
    let tmux_cast = matches!(backend, Backend::Tmux)
        .then(|| cast.clone())
        .flatten();
    let mut tmux_recorder = start_tmux_cast(pane.clone(), tmux_cast)?;
    let opts = RunOptions {
        settle: settle.then(|| Duration::from_millis(1000)),
        ack: ack.then(|| Duration::from_millis(1000)),
        fail_on,
        allow_exit,
        timeout_scale,
        update_goldens,
    };
    let state_ref = state.as_deref();

    // One attempt: clear a stale seam (pty only), spawn the program, run the
    // script, and record this attempt's events / final screen / last snapshot so
    // the terminal attempt's data is available for artefacts and the summary.
    // `run_with_retries` re-runs this only on a timeout-class failure.
    let mut last_events: Vec<RunEvent> = Vec::new();
    let mut last_screen: Option<String> = None;
    let mut last_seam: Option<Value> = None;
    let attempt = || -> anyhow::Result<RunResult> {
        // The pty backend spawns and owns the program, so a leftover seam file
        // from a previous run (or a previous attempt) would let a `wait-until`
        // pass immediately on stale state before the fresh program has written
        // anything (a false pass). Clear it first. Attach backends drive a
        // process they do not own, so their seam file may be live; leave it.
        if is_pty && !from_capture {
            if let Some(p) = state_ref {
                let _ = std::fs::remove_file(p);
            }
        }
        let backend_box = run_backend(
            backend,
            pane.clone(),
            program.clone(),
            rows,
            cols,
            &[],
            pty_cast.clone(),
        )?;
        let b: &dyn PaneBackend = backend_box.as_ref();
        let mut collected: Vec<RunEvent> = Vec::new();
        let mut probe: Box<dyn FnMut() -> Option<Value>> = if from_capture {
            Box::new(|| b.capture().ok().map(|text| driver::screen_state(&text)))
        } else {
            Box::new(move || state_ref.and_then(driver::read_state_file))
        };
        let outcome = {
            let mut sink = ClosureSink::new(
                &mut probe,
                |screen: &str| print!("{screen}"),
                |e: &RunEvent| collected.push(e.clone()),
            );
            run_script(&steps, b, &opts, &mut sink)?
        };
        // Capture this attempt's artefact inputs before the backend (and its
        // child) drops at the end of the closure.
        last_screen = b.capture().ok();
        last_seam = if from_capture {
            last_screen.as_deref().map(driver::screen_state)
        } else {
            state_ref.and_then(driver::read_state_file)
        };
        last_events = collected;
        Ok(outcome)
    };
    let (outcome, attempts) = run_with_retries(retry_timeouts, attempt)?;

    // Flush and finalize the tmux cast (drops the pipe, joins the tail).
    if let Some(r) = tmux_recorder.as_mut() {
        r.stop();
    }

    // Persist the terminal attempt's events to the --events file, if any.
    if let Some(p) = &events {
        let mut f = std::fs::File::create(p)
            .map_err(|e| anyhow::anyhow!("creating events file {}: {e}", p.display()))?;
        for e in &last_events {
            writeln!(f, "{}", e.to_json())?;
        }
    }

    let ok = matches!(outcome, RunResult::Passed);
    let retried = attempts > 1;
    let outcome_label = if retried {
        "retried"
    } else if ok {
        "passed"
    } else {
        "failed"
    };

    // On failure, write the artefact bundle and point the summary at it.
    let artifact_paths = match (&outcome, &artifacts) {
        (RunResult::Failed(failure), Some(dir)) => Some(
            panedrive::write_bundle(
                dir,
                last_screen.as_deref(),
                last_seam.as_ref(),
                &last_events,
                &failure.to_json(),
            )
            .map_err(|e| anyhow::anyhow!("writing artifacts to {}: {e}", dir.display()))?,
        ),
        _ => None,
    };

    let steps_json: Vec<Value> = last_events.iter().map(RunEvent::to_json).collect();
    let mut summary = serde_json::Map::new();
    summary.insert("schema".into(), serde_json::json!(OUTCOME_SCHEMA));
    summary.insert("ok".into(), serde_json::json!(ok));
    summary.insert("outcome".into(), serde_json::json!(outcome_label));
    if retried {
        summary.insert("attempts".into(), serde_json::json!(attempts));
    }
    if let RunResult::Failed(failure) = &outcome {
        summary.insert("failed_step".into(), serde_json::json!(failure.step));
        summary.insert("failure".into(), failure.to_json());
    }
    if let Some(ap) = &artifact_paths {
        summary.insert("artifacts".into(), ap.to_json());
    }
    summary.insert("steps".into(), serde_json::json!(steps_json));

    if json {
        println!("{}", Value::Object(summary));
    } else {
        if let RunResult::Failed(failure) = &outcome {
            eprintln!("{}", failure.human());
        }
        if retried {
            eprintln!(
                "note: run {} after {attempts} attempts",
                if ok { "passed" } else { "failed" }
            );
        }
    }
    if ok {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// The inputs to a suite run (a matrix and/or parameter sweep), collected so
/// `cmd_run` can hand off without a long argument list.
struct SuiteRun<'a> {
    steps: &'a [panedrive::Step],
    opts: RunOptions,
    backend: Backend,
    program: Vec<String>,
    base_rows: u16,
    base_cols: u16,
    state: Option<PathBuf>,
    from_capture: bool,
    json: bool,
    jobs: usize,
    axes: panedrive::SuiteAxes,
    sweep_axis: Option<(String, Vec<String>)>,
    cast: bool,
    events: bool,
    artifacts: Option<PathBuf>,
    retry_timeouts: u32,
}

/// One cell's result, ready to emit: the pass flag, the NDJSON line, and a human
/// one-liner.
struct CellReport {
    ok: bool,
    line: String,
    human: String,
}

/// Run a suite: expand the axes into cells, run the script once per cell with a
/// fresh backend and an isolated seam, and emit one result line per cell.
fn run_suite(run: SuiteRun) -> anyhow::Result<ExitCode> {
    let SuiteRun {
        steps,
        opts,
        backend,
        program,
        base_rows,
        base_cols,
        state: _state,
        from_capture,
        json,
        jobs,
        axes,
        sweep_axis,
        cast,
        events,
        artifacts,
        retry_timeouts,
    } = run;

    // A suite sets each cell's terminal size and child environment and owns a
    // child per cell, which only the pty backend does.
    if !matches!(backend, Backend::Pty) {
        anyhow::bail!(
            "a suite (--sweep or a `#@size:`/`#@env:` directive) runs on the pty backend only"
        );
    }
    // These each write a single destination and cannot represent many cells.
    if cast {
        anyhow::bail!("--cast writes one recording and cannot be combined with a suite");
    }
    if events {
        anyhow::bail!("--events writes one file and cannot be combined with a suite");
    }
    if retry_timeouts > 0 {
        anyhow::bail!("--retry-timeouts cannot be combined with a suite");
    }
    // Unlike the single-destination flags above, `--artifacts` extends cleanly to
    // a suite: each failing cell writes its own bundle under `<dir>/<NNN-id>/`, so
    // the directory holds one subdirectory per failure.
    let artifacts = artifacts.as_deref();

    let cells = panedrive::build_cells(&axes, sweep_axis.as_ref(), base_rows, base_cols);
    let total = cells.len();

    // Shared across workers: the next cell to claim, the pass/fail tally, and a
    // lock that serializes each complete output line so lines never interleave.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let passed = std::sync::atomic::AtomicUsize::new(0);
    let emit = std::sync::Mutex::new(());

    let emit_report = |r: &CellReport| {
        if r.ok {
            passed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let _g = emit.lock().unwrap_or_else(|e| e.into_inner());
        if json {
            println!("{}", r.line);
        } else {
            eprintln!("{}", r.human);
        }
    };

    let ctx = CellCtx {
        steps,
        opts: &opts,
        backend,
        program: &program,
        from_capture,
        artifacts,
    };

    if jobs <= 1 {
        for (i, cell) in cells.iter().enumerate() {
            let report = run_cell(i, cell, &ctx);
            emit_report(&report);
        }
    } else {
        let workers = jobs.min(total).max(1);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= total {
                            break;
                        }
                        let report = run_cell(i, &cells[i], &ctx);
                        emit_report(&report);
                    }
                });
            }
        });
    }

    let passed = passed.load(std::sync::atomic::Ordering::Relaxed);
    if !json {
        eprintln!("suite: {passed}/{total} cells passed");
    }
    if passed == total {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// The per-run inputs every cell shares (only `index` and the cell itself vary),
/// grouped so `run_cell` takes a short argument list and the parallel workers can
/// hand each cell one reference.
struct CellCtx<'a> {
    steps: &'a [panedrive::Step],
    opts: &'a RunOptions,
    backend: Backend,
    program: &'a [String],
    from_capture: bool,
    /// When set, a failing cell writes its bundle under `<artifacts>/<NNN-id>/`.
    artifacts: Option<&'a Path>,
}

/// Run one cell: spawn a fresh backend with the cell's size and environment and
/// an isolated per-cell seam, run the script, and build the result line. A spawn
/// or run error becomes a failed report rather than aborting the whole suite.
/// When `ctx.artifacts` is set and the cell fails, its failure bundle is written
/// under `<artifacts>/<NNN-id>/` and pointed at from the cell's envelope.
fn run_cell(index: usize, cell: &panedrive::CellSpec, ctx: &CellCtx) -> CellReport {
    let steps = ctx.steps;
    let opts = ctx.opts;
    let backend = ctx.backend;
    let program = ctx.program;
    let from_capture = ctx.from_capture;
    let artifacts_dir = ctx.artifacts;
    // A unique seam path per cell, so parallel cells never read, write, or clear
    // each other's snapshot. The child is pointed at it via PANEDRIVE_STATE.
    let seam = std::env::temp_dir().join(format!(
        "panedrive-cell-{}-{}.state.json",
        std::process::id(),
        index
    ));
    let mut env = cell.env.clone();
    if !from_capture {
        let _ = std::fs::remove_file(&seam);
        env.push((
            "PANEDRIVE_STATE".to_string(),
            seam.to_string_lossy().into_owned(),
        ));
    }

    let backend_box = match run_backend(
        backend,
        None,
        program.to_vec(),
        cell.rows,
        cell.cols,
        &env,
        None,
    ) {
        Ok(b) => b,
        Err(e) => return CellReport::error(cell, &e.to_string()),
    };
    let b: &dyn PaneBackend = backend_box.as_ref();
    let mut collected: Vec<RunEvent> = Vec::new();
    let seam_probe = seam.clone();
    let mut probe: Box<dyn FnMut() -> Option<Value>> = if from_capture {
        Box::new(|| b.capture().ok().map(|text| driver::screen_state(&text)))
    } else {
        Box::new(move || driver::read_state_file(&seam_probe))
    };
    let outcome = {
        let mut sink = ClosureSink::new(
            &mut probe,
            |_screen: &str| {},
            |e: &RunEvent| collected.push(e.clone()),
        );
        match run_script(steps, b, opts, &mut sink) {
            Ok(o) => o,
            Err(e) => {
                if !from_capture {
                    let _ = std::fs::remove_file(&seam);
                }
                return CellReport::error(cell, &e.to_string());
            }
        }
    };
    // On failure, write this cell's bundle under `<dir>/<NNN-id>/`, capturing the
    // final screen and seam *before* the seam file below is removed.
    let artifacts_json = match (artifacts_dir, &outcome) {
        (Some(dir), RunResult::Failed(failure)) => {
            let screen = b.capture().ok();
            let last_seam = if from_capture {
                screen.as_deref().map(driver::screen_state)
            } else {
                driver::read_state_file(&seam)
            };
            let cell_dir = dir.join(cell_slug(index, cell));
            panedrive::write_bundle(
                &cell_dir,
                screen.as_deref(),
                last_seam.as_ref(),
                &collected,
                &failure.to_json(),
            )
            .ok()
            .map(|ap| ap.to_json())
        }
        _ => None,
    };
    if !from_capture {
        let _ = std::fs::remove_file(&seam);
    }
    // `backend_box` drops here at end of scope (after `probe`), killing the
    // child; declaration order makes that drop order correct, so no manual drop.
    CellReport::from_outcome(cell, &outcome, &collected, artifacts_json)
}

/// A filesystem-safe subdirectory name for a cell's artifact bundle: the cell
/// index (so bundles sort in run order and never collide) then the cell's axis
/// bindings, with any character outside `[A-Za-z0-9._-]` replaced by `_`.
fn cell_slug(index: usize, cell: &panedrive::CellSpec) -> String {
    let safe: String = cell
        .id
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        format!("{index:03}")
    } else {
        format!("{index:03}-{safe}")
    }
}

impl CellReport {
    /// The axis bindings of a cell as a JSON object (the `id` field).
    fn id_object(cell: &panedrive::CellSpec) -> Value {
        let mut m = serde_json::Map::new();
        for (k, v) in &cell.id {
            m.insert(k.clone(), Value::String(v.clone()));
        }
        Value::Object(m)
    }

    /// A human label for a cell, e.g. `size=24x80 SEED=1`, or `cell 0` when the
    /// cell carries no bindings.
    fn id_label(cell: &panedrive::CellSpec) -> String {
        if cell.id.is_empty() {
            return "cell".to_string();
        }
        cell.id
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Build the result for a finished run: a single-run outcome envelope plus an
    /// `id`. Byte-identical to the `run --json` summary, so a consumer parses each
    /// line with the code it already has.
    fn from_outcome(
        cell: &panedrive::CellSpec,
        outcome: &RunResult,
        events: &[RunEvent],
        artifacts: Option<Value>,
    ) -> Self {
        let ok = matches!(outcome, RunResult::Passed);
        let steps_json: Vec<Value> = events.iter().map(RunEvent::to_json).collect();
        let mut m = serde_json::Map::new();
        m.insert("schema".into(), serde_json::json!(OUTCOME_SCHEMA));
        m.insert("ok".into(), serde_json::json!(ok));
        m.insert(
            "outcome".into(),
            serde_json::json!(if ok { "passed" } else { "failed" }),
        );
        if let RunResult::Failed(failure) = outcome {
            m.insert("failed_step".into(), serde_json::json!(failure.step));
            m.insert("failure".into(), failure.to_json());
        }
        if let Some(ap) = artifacts {
            m.insert("artifacts".into(), ap);
        }
        m.insert("steps".into(), serde_json::json!(steps_json));
        m.insert("id".into(), Self::id_object(cell));
        CellReport {
            ok,
            line: Value::Object(m).to_string(),
            human: format!(
                "{}: {}",
                Self::id_label(cell),
                if ok { "passed" } else { "failed" }
            ),
        }
    }

    /// Build a failed result for a cell that could not be spawned or run.
    fn error(cell: &panedrive::CellSpec, err: &str) -> Self {
        let mut m = serde_json::Map::new();
        m.insert("schema".into(), serde_json::json!(OUTCOME_SCHEMA));
        m.insert("ok".into(), serde_json::json!(false));
        m.insert("outcome".into(), serde_json::json!("failed"));
        m.insert("error".into(), serde_json::json!(err));
        m.insert("steps".into(), serde_json::json!(Vec::<Value>::new()));
        m.insert("id".into(), Self::id_object(cell));
        CellReport {
            ok: false,
            line: Value::Object(m).to_string(),
            human: format!("{}: error: {err}", Self::id_label(cell)),
        }
    }
}

/// The `schema` version stamped on every `run` summary. Bumped only on a
/// breaking change to the summary envelope (see `docs/design/outcome-model.md`).
const OUTCOME_SCHEMA: u32 = 1;

/// Start a tmux `--cast` recorder when a cast path is set. Encapsulates the
/// `--pane` requirement and file creation so `cmd_run` never branches on the
/// recorder's internals. `cast` is already gated to the tmux backend by the
/// caller, so `None` simply means "no tmux cast to record".
fn start_tmux_cast(
    pane: Option<String>,
    cast: Option<PathBuf>,
) -> anyhow::Result<Option<panedrive::TmuxCastRecorder>> {
    let Some(path) = cast else { return Ok(None) };
    let pane = pane.ok_or_else(|| anyhow::anyhow!("--pane is required to record a tmux cast"))?;
    let file = std::fs::File::create(&path)
        .map_err(|e| anyhow::anyhow!("creating cast file {}: {e}", path.display()))?;
    Ok(Some(panedrive::TmuxCastRecorder::start(pane, file)?))
}

/// Record an interactive PTY session into a `.pds` script (and optionally a
/// cast): spawn the program, mirror its output to our terminal, forward the
/// user's keystrokes to it, and decode those keystrokes into script steps.
#[cfg(feature = "pty")]
fn spawn_record(
    script_path: PathBuf,
    cast: Option<PathBuf>,
    rows: u16,
    cols: u16,
    program: Vec<String>,
) -> anyhow::Result<ExitCode> {
    use panedrive::backend::pty::OutputTap;

    let (prog, args) = program.split_first().ok_or_else(|| {
        anyhow::anyhow!(
            "record needs a program after `--`, e.g. `record --script out.pds -- mytui`"
        )
    })?;
    let prog = resolve_pty_program(prog);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    // Mirror the child's output to our stdout (so the user sees the UI) and,
    // when asked, into an asciinema cast, all from the reader thread's tap.
    let mut cast_writer = match &cast {
        Some(path) => {
            let file = std::fs::File::create(path)
                .map_err(|e| anyhow::anyhow!("creating cast file {}: {e}", path.display()))?;
            Some(panedrive::CastWriter::new(file, cols, rows)?)
        }
        None => None,
    };
    let tap: OutputTap = Box::new(move |bytes: &[u8]| {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
        if let Some(w) = cast_writer.as_mut() {
            let _ = w.write_output(bytes);
        }
    });
    let backend =
        panedrive::PtyBackend::spawn_tapped(&prog, &arg_refs, rows, cols, &[], Some(tap))?;

    // Put the terminal in raw mode so individual keystrokes reach us (skipped
    // when stdin is piped, which is how the loop is exercised in tests). The
    // guard restores the terminal on drop, so raw mode never leaks even if the
    // record loop panics between here and the explicit restore below.
    let restore = TerminalRestore::raw();

    let mut recorder = panedrive::ScriptRecorder::new();
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(0) => break, // EOF (piped input or closed terminal)
            Ok(_) => {
                if byte[0] == panedrive::record::STOP_BYTE {
                    break;
                }
                let _ = backend.write_bytes(&byte);
                recorder.feed(&byte);
                if !backend.is_alive() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // Restore the terminal before printing our summary (Drop would do it at
    // scope end regardless, but we want cooked mode back before the eprintln).
    drop(restore);

    let text = recorder.finish();
    std::fs::write(&script_path, &text)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", script_path.display()))?;
    eprintln!(
        "wrote {} step line(s) to {}",
        text.lines().count(),
        script_path.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// An RAII guard that puts the terminal in raw mode and restores it on drop, so
/// `record` can never leave the user's terminal in raw mode, not even if the
/// record loop panics. A no-op when stdin is not a TTY (piped input, as in the
/// integration test), where there is nothing to restore.
#[cfg(feature = "pty")]
struct TerminalRestore {
    saved: Option<String>,
}

#[cfg(feature = "pty")]
impl TerminalRestore {
    fn raw() -> Self {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Self { saved: None };
        }
        let saved = stty_capture(&["-g"]).ok();
        let _ = std::process::Command::new("stty")
            .args(["raw", "-echo"])
            .status();
        eprintln!("recording… press Ctrl-] to stop");
        Self { saved }
    }
}

#[cfg(feature = "pty")]
impl Drop for TerminalRestore {
    fn drop(&mut self) {
        if let Some(s) = self.saved.take() {
            let _ = std::process::Command::new("stty").arg(s.trim()).status();
        }
    }
}

#[cfg(feature = "pty")]
fn stty_capture(args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("stty").args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(feature = "pty"))]
fn spawn_record(
    _script_path: PathBuf,
    _cast: Option<PathBuf>,
    _rows: u16,
    _cols: u16,
    _program: Vec<String>,
) -> anyhow::Result<ExitCode> {
    anyhow::bail!("record spawns a program in a PTY, so it requires building with `--features pty`")
}

/// Resolve the text for `type` from exactly one source: a literal argument,
/// stdin, or an environment variable. More than one, or none, is a usage error.
fn resolve_type_text(
    text: Option<String>,
    stdin: bool,
    from_env: Option<String>,
) -> anyhow::Result<String> {
    match (text, stdin, from_env) {
        (Some(t), false, None) => Ok(t),
        (None, true, None) => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            // Strip one trailing line ending so `echo secret |` and
            // `printf %s "$s" |` both yield the same value.
            if s.ends_with('\n') {
                s.pop();
                if s.ends_with('\r') {
                    s.pop();
                }
            }
            Ok(s)
        }
        (None, false, Some(var)) => std::env::var(&var)
            .map_err(|_| anyhow::anyhow!("environment variable {var} is not set")),
        _ => anyhow::bail!(
            "provide exactly one text source: a literal argument, --stdin, or --from-env VAR"
        ),
    }
}

/// Build an *attach* backend for the one-shot commands (press/type/capture).
/// The PTY backend spawns a program, so it is not valid here.
fn backend_for(backend: Backend, pane: String) -> anyhow::Result<Box<dyn PaneBackend>> {
    match backend {
        Backend::Tmux => Ok(Box::new(TmuxBackend::new(pane))),
        Backend::Zellij => Ok(Box::new(ZellijBackend::new(pane))),
        Backend::Screen => Ok(Box::new(ScreenBackend::new(pane))),
        Backend::Pty => anyhow::bail!(
            "the pty backend spawns a program, so it only works with `run ... -- <program>`, not one-shot commands"
        ),
    }
}

/// Build the backend for `run`: tmux/zellij attach to `pane`, pty spawns the
/// program given after `--`.
fn run_backend(
    backend: Backend,
    pane: Option<String>,
    program: Vec<String>,
    rows: u16,
    cols: u16,
    env: &[(String, String)],
    cast: Option<PathBuf>,
) -> anyhow::Result<Box<dyn PaneBackend>> {
    match backend {
        Backend::Tmux => {
            let pane =
                pane.ok_or_else(|| anyhow::anyhow!("--pane is required for the tmux backend"))?;
            Ok(Box::new(TmuxBackend::new(pane)))
        }
        Backend::Zellij => {
            let session = pane.ok_or_else(|| {
                anyhow::anyhow!("--pane (session name) is required for the zellij backend")
            })?;
            Ok(Box::new(ZellijBackend::new(session)))
        }
        Backend::Screen => {
            let session = pane.ok_or_else(|| {
                anyhow::anyhow!("--pane (session name) is required for the screen backend")
            })?;
            Ok(Box::new(ScreenBackend::new(session)))
        }
        Backend::Pty => spawn_pty(program, rows, cols, env, cast),
    }
}

#[cfg(feature = "pty")]
fn spawn_pty(
    program: Vec<String>,
    rows: u16,
    cols: u16,
    env: &[(String, String)],
    cast: Option<PathBuf>,
) -> anyhow::Result<Box<dyn PaneBackend>> {
    use panedrive::backend::pty::OutputTap;

    let (prog, args) = program.split_first().ok_or_else(|| {
        anyhow::anyhow!(
            "the pty backend needs a program after `--`, e.g. `run s --backend pty -- mytui`"
        )
    })?;
    // The PTY host resolves a bare program name through PATH but, unlike
    // std::process, does not resolve a cwd-relative path (`./mytui`,
    // `target/debug/mytui`). Canonicalize a path-like program so it works the
    // way a shell user expects; leave bare names for the PATH lookup.
    let prog = resolve_pty_program(prog);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // With --cast, tap the raw output into an asciinema recording. The cast's
    // dimensions match the PTY we spawn.
    let tap: Option<OutputTap> = match cast {
        Some(path) => {
            let file = std::fs::File::create(&path)
                .map_err(|e| anyhow::anyhow!("creating cast file {}: {e}", path.display()))?;
            let mut writer = panedrive::CastWriter::new(file, cols, rows)?;
            Some(Box::new(move |bytes: &[u8]| {
                let _ = writer.write_output(bytes);
            }))
        }
        None => None,
    };
    Ok(Box::new(panedrive::PtyBackend::spawn_tapped(
        &prog, &arg_refs, rows, cols, env, tap,
    )?))
}

/// Resolve a cwd-relative, path-like program to an absolute path. A bare name
/// (no separator) is returned unchanged so the PTY host can search PATH.
#[cfg(feature = "pty")]
fn resolve_pty_program(prog: &str) -> String {
    if prog.contains(std::path::MAIN_SEPARATOR) {
        if let Ok(abs) = std::fs::canonicalize(prog) {
            return abs.to_string_lossy().into_owned();
        }
    }
    prog.to_string()
}

#[cfg(not(feature = "pty"))]
fn spawn_pty(
    _program: Vec<String>,
    _rows: u16,
    _cols: u16,
    _env: &[(String, String)],
    _cast: Option<PathBuf>,
) -> anyhow::Result<Box<dyn PaneBackend>> {
    anyhow::bail!("the pty backend requires building panedrive with `--features pty`")
}

#[cfg(test)]
mod resolve_tests {
    use super::{Backend, resolve_type_text, run_backend};

    #[test]
    fn resolve_type_text_errors_with_no_source() {
        assert!(resolve_type_text(None, false, None).is_err());
    }

    #[test]
    fn resolve_type_text_errors_with_two_sources() {
        // A literal plus --stdin is ambiguous: exactly one source is required.
        assert!(resolve_type_text(Some("x".into()), true, None).is_err());
    }

    #[test]
    fn resolve_type_text_errors_with_literal_and_env() {
        assert!(resolve_type_text(Some("x".into()), false, Some("V".into())).is_err());
    }

    #[test]
    fn resolve_type_text_reads_a_set_env_var() {
        let var = "PANEDRIVE_RESOLVE_TEST_SET";
        // SAFETY: single-threaded test scope, unique var name, cleaned up.
        unsafe {
            std::env::set_var(var, "from-the-env");
        }
        let got = resolve_type_text(None, false, Some(var.into()));
        unsafe {
            std::env::remove_var(var);
        }
        assert_eq!(got.unwrap(), "from-the-env");
    }

    #[test]
    fn resolve_type_text_errors_on_unset_env_var() {
        let var = "PANEDRIVE_RESOLVE_TEST_UNSET";
        // SAFETY: single-threaded test scope, unique var name.
        unsafe {
            std::env::remove_var(var);
        }
        assert!(resolve_type_text(None, false, Some(var.into())).is_err());
    }

    #[test]
    fn run_backend_requires_a_pane_for_attach_backends() {
        // Each attach backend errors on a missing --pane without spawning
        // anything. The pty arm is feature-gated and not exercised here.
        assert!(run_backend(Backend::Tmux, None, vec![], 24, 80, &[], None).is_err());
        assert!(run_backend(Backend::Zellij, None, vec![], 24, 80, &[], None).is_err());
        assert!(run_backend(Backend::Screen, None, vec![], 24, 80, &[], None).is_err());
    }
}

#[cfg(all(test, feature = "pty"))]
mod pty_program_tests {
    use super::resolve_pty_program;

    #[test]
    fn bare_name_is_left_for_path_lookup() {
        assert_eq!(resolve_pty_program("mytui"), "mytui");
    }

    #[test]
    fn path_like_existing_program_becomes_absolute() {
        let f = std::env::temp_dir().join(format!("panedrive-resolve-{}", std::process::id()));
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        let got = resolve_pty_program(&f.to_string_lossy());
        assert!(
            std::path::Path::new(&got).is_absolute(),
            "path-like program should resolve to an absolute path, got {got}"
        );
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn missing_path_like_program_is_left_unchanged() {
        assert_eq!(resolve_pty_program("./no/such/prog"), "./no/such/prog");
    }
}

#[cfg(test)]
mod suite_report_tests {
    use super::CellReport;
    use panedrive::{CellSpec, FailReason, RunResult, StepFailure};

    fn cell() -> CellSpec {
        CellSpec {
            rows: 24,
            cols: 80,
            env: vec![],
            id: vec![("size".into(), "24x80".into()), ("SEED".into(), "3".into())],
        }
    }

    #[test]
    fn passed_cell_builds_a_labelled_envelope() {
        let r = CellReport::from_outcome(&cell(), &RunResult::Passed, &[], None);
        assert!(r.ok);
        let v: serde_json::Value = serde_json::from_str(&r.line).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["ok"], true);
        assert_eq!(v["outcome"], "passed");
        assert_eq!(v["id"]["size"], "24x80");
        assert_eq!(v["id"]["SEED"], "3");
        assert_eq!(r.human, "size=24x80 SEED=3: passed");
        assert!(v.get("artifacts").is_none(), "no bundle on a pass");
    }

    #[test]
    fn failed_cell_carries_the_failure() {
        let fail = StepFailure {
            step: 2,
            kind: "assert",
            cond: "x=1".into(),
            path: "x".into(),
            reason: FailReason::NoState,
        };
        let r = CellReport::from_outcome(&cell(), &RunResult::Failed(fail), &[], None);
        assert!(!r.ok);
        let v: serde_json::Value = serde_json::from_str(&r.line).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["outcome"], "failed");
        assert_eq!(v["failed_step"], 2);
        assert!(v["failure"].is_object());
        assert!(r.human.ends_with(": failed"));
    }

    #[test]
    fn a_failed_cell_carries_the_artifacts_pointer_when_a_bundle_was_written() {
        let fail = StepFailure {
            step: 1,
            kind: "assert",
            cond: "x=1".into(),
            path: "x".into(),
            reason: FailReason::NoState,
        };
        let bundle = serde_json::json!({ "dir": "/tmp/art/000-size=24x80_SEED=3" });
        let r =
            CellReport::from_outcome(&cell(), &RunResult::Failed(fail), &[], Some(bundle.clone()));
        let v: serde_json::Value = serde_json::from_str(&r.line).unwrap();
        assert_eq!(
            v["artifacts"], bundle,
            "the cell envelope points at its bundle"
        );
    }

    #[test]
    fn cell_slug_is_filesystem_safe_and_index_prefixed() {
        // The index prefixes every slug (stable ordering, guaranteed uniqueness),
        // and any character unsafe in a path (here the `=` and space of the label,
        // and a stray `/`) is replaced with `_`.
        let c = CellSpec {
            rows: 24,
            cols: 80,
            env: vec![],
            id: vec![("mode".into(), "a/b".into())],
        };
        assert_eq!(super::cell_slug(7, &c), "007-mode_a_b");
        let empty = CellSpec {
            rows: 24,
            cols: 80,
            env: vec![],
            id: vec![],
        };
        assert_eq!(super::cell_slug(0, &empty), "000");
    }

    #[test]
    fn error_cell_is_failed_with_an_error_field() {
        let r = CellReport::error(&cell(), "spawn boom");
        assert!(!r.ok);
        let v: serde_json::Value = serde_json::from_str(&r.line).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["outcome"], "failed");
        assert_eq!(v["error"], "spawn boom");
        assert!(r.human.contains("error: spawn boom"));
    }

    #[test]
    fn a_cell_with_no_bindings_labels_as_cell() {
        let empty = CellSpec {
            rows: 24,
            cols: 80,
            env: vec![],
            id: vec![],
        };
        let r = CellReport::from_outcome(&empty, &RunResult::Passed, &[], None);
        assert_eq!(r.human, "cell: passed");
        let v: serde_json::Value = serde_json::from_str(&r.line).unwrap();
        assert!(v["id"].as_object().unwrap().is_empty());
    }
}
