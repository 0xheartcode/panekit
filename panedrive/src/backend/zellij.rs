//! The zellij backend: drives a pane via `zellij action write-chars` /
//! `zellij action write` and reads it via `zellij action dump-screen`. Like the
//! tmux backend it *attaches* to an already-running session rather than owning
//! the process, so it suits a live session a human may also be watching.
//!
//! `zellij action` targets the focused pane of a session, so the "pane" address
//! here is a **session name** (`zellij --session <name> action ...`), not a
//! tmux-style `session:win.pane` target.
//!
//! The command *planning* (which action each key becomes, how printable runs
//! coalesce, how the argv is built) is pure and unit-tested; only the thin
//! [`Command`] execution needs a live zellij.

use super::PaneBackend;
use crate::key::{Key, ZellijKey};
use std::io;
use std::process::Command;

/// Drives the focused pane of one zellij session, addressed by session name.
#[derive(Debug, Clone)]
pub struct ZellijBackend {
    session: String,
}

/// One planned `zellij action <action> <args...>` call, before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Action {
    name: &'static str,
    args: Vec<String>,
}

impl ZellijBackend {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
        }
    }

    /// Build the full argv for one planned action:
    /// `--session=<s> action <name> [-- <args...>]`.
    fn argv(&self, action: &Action) -> Vec<String> {
        // `--session=<s>` in one token so a session name is never mistaken for a
        // flag value, and `--` before the payload so text that begins with `-`
        // (typing `-v`, a negative field value, ...) is taken literally rather
        // than parsed as an option by zellij.
        let mut argv = vec![
            format!("--session={}", self.session),
            "action".to_string(),
            action.name.to_string(),
        ];
        if !action.args.is_empty() {
            argv.push("--".to_string());
            argv.extend(action.args.iter().cloned());
        }
        argv
    }

    /// Run one planned action, surfacing a non-zero exit as an error naming the
    /// session.
    fn run(&self, action: &Action) -> io::Result<()> {
        let status = Command::new("zellij").args(self.argv(action)).status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "zellij action {} failed for session {} (is it running?)",
                action.name, self.session
            )));
        }
        Ok(())
    }
}

/// Plan the actions for a key sequence: coalesce consecutive printable
/// characters into one `write-chars`, and emit each named/control key as a
/// `write` of its raw bytes (each byte a decimal argument) in between.
fn plan_send(keys: &[Key]) -> Vec<Action> {
    let mut plan = Vec::new();
    let mut chars = String::new();
    for key in keys {
        match key.to_zellij() {
            ZellijKey::Chars(c) => chars.push(c),
            ZellijKey::Bytes(bytes) => {
                flush_chars(&mut chars, &mut plan);
                plan.push(Action {
                    name: "write",
                    args: bytes.iter().map(|b| b.to_string()).collect(),
                });
            }
        }
    }
    flush_chars(&mut chars, &mut plan);
    plan
}

/// Push a `write-chars` action for the buffered run, if any, and clear it.
fn flush_chars(buf: &mut String, plan: &mut Vec<Action>) {
    if !buf.is_empty() {
        plan.push(Action {
            name: "write-chars",
            args: vec![std::mem::take(buf)],
        });
    }
}

impl PaneBackend for ZellijBackend {
    fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
        for action in plan_send(keys) {
            self.run(&action)?;
        }
        Ok(())
    }

    fn capture(&self) -> io::Result<String> {
        // `dump-screen` with no `--path` prints the viewport to stdout, so read
        // it straight from the command's output, no temp file needed.
        let action = Action {
            name: "dump-screen",
            args: Vec::new(),
        };
        let out = Command::new("zellij").args(self.argv(&action)).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "zellij action dump-screen failed for session {} (is it running?)",
                self.session
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(name: &'static str, args: &[&str]) -> Action {
        Action {
            name,
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn argv_prefixes_session_and_escapes_the_payload() {
        let z = ZellijBackend::new("demo");
        assert_eq!(
            z.argv(&action("write-chars", &["hi"])),
            vec!["--session=demo", "action", "write-chars", "--", "hi"]
        );
    }

    #[test]
    fn argv_escapes_dash_prefixed_text_as_literal() {
        // Typing "-v" must reach zellij as literal text after `--`, not as a flag.
        let z = ZellijBackend::new("demo");
        assert_eq!(
            z.argv(&action("write-chars", &["-v"])),
            vec!["--session=demo", "action", "write-chars", "--", "-v"]
        );
    }

    #[test]
    fn argv_omits_the_separator_when_there_is_no_payload() {
        // `dump-screen` prints to stdout with no positional path.
        let z = ZellijBackend::new("demo");
        assert_eq!(
            z.argv(&action("dump-screen", &[])),
            vec!["--session=demo", "action", "dump-screen"]
        );
    }

    #[test]
    fn plan_coalesces_chars_and_splits_named_keys() {
        // "hi" then Enter then "yo": one write-chars, one write (13), one
        // write-chars.
        let keys = crate::key::parse_keys("h i Enter y o").unwrap();
        assert_eq!(
            plan_send(&keys),
            vec![
                action("write-chars", &["hi"]),
                action("write", &["13"]),
                action("write-chars", &["yo"]),
            ]
        );
    }

    #[test]
    fn plan_encodes_named_keys_as_decimal_bytes() {
        // Up arrow is ESC [ A → 27 91 65; Ctrl-c is a lone 3.
        assert_eq!(
            plan_send(&crate::key::parse_keys("Up").unwrap()),
            vec![action("write", &["27", "91", "65"])]
        );
        assert_eq!(
            plan_send(&crate::key::parse_keys("C-c").unwrap()),
            vec![action("write", &["3"])]
        );
    }

    #[test]
    fn plan_of_no_keys_is_empty() {
        assert!(plan_send(&[]).is_empty());
    }
}
