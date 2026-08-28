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

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use panedrive::{
    Key, PaneBackend, RunResult, TmuxBackend, WaitOutcome, ZellijBackend, condition::Condition,
    driver, key, parse_script, run_script,
};

#[derive(Parser)]
#[command(
    name = "panedrive",
    version,
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
        #[arg(long)]
        state: PathBuf,
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 50)]
        interval_ms: u64,
    },
    /// Evaluate a condition against the state seam once (no waiting).
    Assert {
        /// Condition over the state JSON.
        cond: String,
        #[arg(long)]
        state: PathBuf,
    },
    /// Record the state seam over a window, printing each observed state as a
    /// JSONL line (`{"t_ms":..,"state":..}`), catches transitions a single
    /// assert would miss. Pair with `press` to record what the UI does.
    Watch {
        #[arg(long)]
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
        /// State seam path that `assert` / `wait-until` steps read.
        #[arg(long)]
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
    },
}

#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum Backend {
    /// Attach to a running tmux pane.
    Tmux,
    /// Attach to a running zellij session (the `--pane` value is the session
    /// name).
    Zellij,
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
                    eprintln!("held after {} ms", took.as_millis());
                    Ok(ExitCode::SUCCESS)
                }
                WaitOutcome::TimedOut => {
                    eprintln!("timed out after {timeout_ms} ms");
                    Ok(ExitCode::from(1))
                }
            }
        }
        Cmd::Assert { cond, state } => {
            let cond = Condition::parse(&cond)?;
            match driver::read_state_file(&state) {
                Some(value) if cond.eval(&value) => Ok(ExitCode::SUCCESS),
                Some(_) => {
                    eprintln!("assertion failed: {cond:?}");
                    Ok(ExitCode::from(1))
                }
                None => {
                    eprintln!("no readable state at {}", state.display());
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
        } => {
            let text = std::fs::read_to_string(&script)
                .map_err(|e| anyhow::anyhow!("reading script {}: {e}", script.display()))?;
            let steps = parse_script(&text)?;
            let backend = run_backend(backend, pane, program, rows, cols)?;
            let probe = || match &state {
                Some(path) => driver::read_state_file(path),
                None => None,
            };
            let outcome = run_script(&steps, backend.as_ref(), probe, |screen| print!("{screen}"))?;
            match outcome {
                RunResult::Passed => Ok(ExitCode::SUCCESS),
                RunResult::Failed(why) => {
                    eprintln!("{why}");
                    Ok(ExitCode::from(1))
                }
            }
        }
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
        Backend::Pty => spawn_pty(program, rows, cols),
    }
}

#[cfg(feature = "pty")]
fn spawn_pty(program: Vec<String>, rows: u16, cols: u16) -> anyhow::Result<Box<dyn PaneBackend>> {
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
    Ok(Box::new(panedrive::PtyBackend::spawn(
        &prog, &arg_refs, rows, cols,
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
