//! A tiny interactive fixture for exercising panedrive's screen-text and
//! crash/exit assertions over a real PTY. NOT part of the library; it stands in
//! for a program that renders text and can crash or quit on command.
//!
//! On start it prints a fixed banner, so `expect-screen` has deterministic
//! rendered text to match independent of terminal echo. Then it reads commands,
//! one per line:
//!   `crash` -> print a panic-like line, then exit with code 101
//!   `bye`   -> exit cleanly (code 0)
//! any other line is acknowledged as `saw: <line>`.

use std::io::{BufRead, Write};

fn main() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "READY banner: 1/2/3 day/week/month");
    let _ = out.flush();

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "crash" => {
                let _ = writeln!(out, "thread 'main' panicked at exit_tui");
                let _ = out.flush();
                std::process::exit(101);
            }
            "bye" => std::process::exit(0),
            other => {
                let _ = writeln!(out, "saw: {other}");
                let _ = out.flush();
            }
        }
    }
}
