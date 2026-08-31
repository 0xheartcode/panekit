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
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use panedrive::{
    Key, Observed, PaneBackend, RunEvent, RunResult, ScreenBackend, TmuxBackend, WaitOutcome,
    ZellijBackend, condition::Condition, driver, key, parse_script, run_script_recording, seam,
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
    Run {
        /// Path to the script file (one step per line; `#` comments allowed).
        script: PathBuf,
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
    },
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
        } => {
            // Join so a quoted spec ("2 Down Enter") and separate args
            // (2 Down Enter) both parse identically.
            let keys = key::parse_keys(&keys.join(" "))?;
            backend_for(backend, pane)?.send_keys(&keys)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Type {
            text,
            stdin,
            from_env,
            pane,
            backend,
            paste,
        } => {
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
        Cmd::Capture { pane, backend } => {
            print!("{}", backend_for(backend, pane)?.capture()?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::WaitUntil {
            cond,
            state,
            timeout_ms,
            interval_ms,
            json,
        } => {
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
                    // Read once more so the message/JSON can report what the seam
                    // last held, not just that it timed out.
                    let observed = driver::read_state_file(&state)
                        .map(|v| cond.observed(&v))
                        .unwrap_or(Observed::Missing);
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "ok": false, "cond": cond.to_spec(), "path": cond.path(),
                                "timeout_ms": timeout_ms, "actual": observed_json(&observed),
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
        Cmd::State { state, paths } => {
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
        Cmd::ValidateSeam { file, json } => {
            let bytes = std::fs::read(&file)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
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
        Cmd::Assert { cond, state, json } => {
            let cond = Condition::parse(&cond)?;
            match driver::read_state_file(&state) {
                Some(value) => {
                    let ok = cond.eval(&value);
                    if json {
                        let actual = observed_json(&cond.observed(&value));
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
        Cmd::Watch {
            state,
            for_ms,
            interval_ms,
            distinct,
        } => {
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
        Cmd::Run {
            script,
            backend,
            state,
            pane,
            program,
            rows,
            cols,
            settle,
            from_capture,
            json,
            events,
            cast,
        } => {
            if cast.is_some() && !matches!(backend, Backend::Pty | Backend::Tmux) {
                anyhow::bail!("--cast is supported on the pty and tmux backends only");
            }
            let text = std::fs::read_to_string(&script)
                .map_err(|e| anyhow::anyhow!("reading script {}: {e}", script.display()))?;
            let steps = parse_script(&text)?;
            // The pty backend records via an output tap; tmux records via a
            // pipe-pane recorder started below. Route the cast path accordingly.
            let pty_cast = matches!(backend, Backend::Pty)
                .then(|| cast.clone())
                .flatten();
            let pane_for_cast = pane.clone();
            let backend_kind = backend;
            let backend = run_backend(backend, pane, program, rows, cols, pty_cast)?;
            // Start the tmux cast recorder (only for tmux + --cast).
            let mut tmux_recorder = match (backend_kind, &cast) {
                (Backend::Tmux, Some(path)) => {
                    let pane = pane_for_cast.ok_or_else(|| {
                        anyhow::anyhow!("--pane is required to record a tmux cast")
                    })?;
                    let file = std::fs::File::create(path).map_err(|e| {
                        anyhow::anyhow!("creating cast file {}: {e}", path.display())
                    })?;
                    Some(panedrive::TmuxCastRecorder::start(pane, file)?)
                }
                _ => None,
            };
            let b: &dyn PaneBackend = backend.as_ref();
            let settle = settle.then(|| Duration::from_millis(1000));
            // Read state from the JSON seam, or, with --from-capture, from the
            // pane's visible text wrapped as {screen, lines} for uninstrumented
            // apps.
            let mut probe: Box<dyn FnMut() -> Option<serde_json::Value>> = if from_capture {
                Box::new(move || b.capture().ok().map(|text| driver::screen_state(&text)))
            } else {
                Box::new(move || state.as_deref().and_then(driver::read_state_file))
            };
            let mut events_file =
                match &events {
                    Some(p) => Some(std::fs::File::create(p).map_err(|e| {
                        anyhow::anyhow!("creating events file {}: {e}", p.display())
                    })?),
                    None => None,
                };
            let mut collected: Vec<RunEvent> = Vec::new();
            let outcome = run_script_recording(
                &steps,
                b,
                settle,
                &mut probe,
                |screen| print!("{screen}"),
                |e| {
                    if let Some(f) = events_file.as_mut() {
                        let _ = writeln!(f, "{}", e.to_json());
                    }
                    if json {
                        collected.push(e.clone());
                    }
                },
            )?;
            // Flush and finalize the tmux cast (drops the pipe, joins the tail).
            if let Some(r) = tmux_recorder.as_mut() {
                r.stop();
            }
            let steps_json: Vec<Value> = collected.iter().map(RunEvent::to_json).collect();
            match outcome {
                RunResult::Passed => {
                    if json {
                        println!("{}", serde_json::json!({ "ok": true, "steps": steps_json }));
                    }
                    Ok(ExitCode::SUCCESS)
                }
                RunResult::Failed(failure) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "ok": false, "failed_step": failure.step,
                                "failure": failure.to_json(), "steps": steps_json,
                            })
                        );
                    } else {
                        eprintln!("{}", failure.human());
                    }
                    Ok(ExitCode::from(1))
                }
            }
        }
        Cmd::Record {
            script,
            cast,
            rows,
            cols,
            program,
        } => spawn_record(script, cast, rows, cols, program),
    }
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
    use std::io::IsTerminal;

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
    let backend = panedrive::PtyBackend::spawn_tapped(&prog, &arg_refs, rows, cols, Some(tap))?;

    // Put the terminal in raw mode so individual keystrokes reach us (skipped
    // when stdin is piped, which is how the loop is exercised in tests).
    let is_tty = std::io::stdin().is_terminal();
    let saved_tty = if is_tty {
        let s = stty_capture(&["-g"]).ok();
        let _ = std::process::Command::new("stty")
            .args(["raw", "-echo"])
            .status();
        eprintln!("recording… press Ctrl-] to stop");
        s
    } else {
        None
    };

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

    // Restore the terminal before printing our summary.
    if let Some(s) = saved_tty {
        let _ = std::process::Command::new("stty").arg(s.trim()).status();
    }

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

/// Render an observed path value as JSON for `--json` output: the scalar text,
/// or `null` when the path was absent or non-scalar.
fn observed_json(observed: &Observed) -> Value {
    match observed.scalar() {
        Some(s) => Value::String(s.to_string()),
        None => Value::Null,
    }
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
        Backend::Pty => spawn_pty(program, rows, cols, cast),
    }
}

#[cfg(feature = "pty")]
fn spawn_pty(
    program: Vec<String>,
    rows: u16,
    cols: u16,
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
        &prog, &arg_refs, rows, cols, tap,
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
    _cast: Option<PathBuf>,
) -> anyhow::Result<Box<dyn PaneBackend>> {
    anyhow::bail!("the pty backend requires building panedrive with `--features pty`")
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
