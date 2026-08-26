//! The tmux backend: drives a pane via `tmux send-keys` and reads it via
//! `tmux capture-pane`. Best for driving a *live* session a human may also be
//! watching. (A dependency-free PTY backend for CI is planned, see the repo
//! roadmap.)

use super::PaneBackend;
use crate::key::{Key, TmuxKey};
use std::io;
use std::process::Command;

/// Drives a single tmux pane, addressed by any tmux target (`session:win.pane`,
/// `%id`, or a bare pane index).
#[derive(Debug, Clone)]
pub struct TmuxBackend {
    pane: String,
}

impl TmuxBackend {
    pub fn new(pane: impl Into<String>) -> Self {
        Self { pane: pane.into() }
    }

    /// Run one `tmux send-keys -t <pane> <extra...>`.
    fn send(&self, extra: &[&str]) -> io::Result<()> {
        let mut cmd = Command::new("tmux");
        cmd.arg("send-keys").arg("-t").arg(&self.pane);
        for a in extra {
            cmd.arg(a);
        }
        let status = cmd.status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "tmux send-keys failed for pane {} (is the pane alive?)",
                self.pane
            )));
        }
        Ok(())
    }

    /// Emit a run of literal characters as a single `-l` send, then clear it.
    fn flush_literal(&self, buf: &mut String) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        // `-l` sends the whole run literally, so characters that collide with
        // tmux key names are typed, not interpreted.
        self.send(&["-l", buf])?;
        buf.clear();
        Ok(())
    }
}

impl PaneBackend for TmuxBackend {
    fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
        // Coalesce consecutive literal characters into one `send-keys -l` call
        // (a whole typed word is one process, not one per character); named
        // keys (Enter, arrows, C-c) are sent individually between the runs.
        let mut literal = String::new();
        for key in keys {
            match key.to_tmux() {
                TmuxKey::Literal(c) => literal.push(c),
                TmuxKey::Named(name) => {
                    self.flush_literal(&mut literal)?;
                    self.send(&[name.as_str()])?;
                }
            }
        }
        self.flush_literal(&mut literal)
    }

    fn capture(&self) -> io::Result<String> {
        let out = Command::new("tmux")
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(&self.pane)
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "tmux capture-pane failed for pane {}",
                self.pane
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
