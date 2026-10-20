//! Terminal-host backends. A [`PaneBackend`] is the *only* host-specific part
//! of the driver: it sends keys into a running UI and captures its screen.
//! Everything above it, waiting, asserting, reading the JSON seam, is
//! backend-agnostic, so adding zellij or a raw PTY is one new impl of this
//! trait, nothing else.

pub mod screen;
pub mod tmux;
pub mod zellij;

#[cfg(feature = "pty")]
pub mod pty;

use crate::key::Key;
use std::io;

/// The status of a child process a backend spawned and owns.
///
/// Only a backend that *owns* its target process (the PTY backend) can report
/// this. The attach backends (tmux / zellij / screen) drive a process they do
/// not own, so they return `None` from [`PaneBackend::child_exit`] and this
/// type never comes up for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    /// The backend-owned child is still running.
    Running,
    /// The backend-owned child has exited. The code is `None` when the backend
    /// could not determine one. (The PTY backend always reports a code; `None`
    /// exists for backends that cannot.)
    Exited(Option<i32>),
}

/// A channel into one running terminal UI: press keys, read the screen.
pub trait PaneBackend {
    /// Send a sequence of logical key presses to the target pane, in order.
    fn send_keys(&self, keys: &[Key]) -> io::Result<()>;

    /// Capture the pane's currently visible text (best-effort; used only when
    /// no [`paneview`](https://docs.rs/paneview) JSON seam is available).
    fn capture(&self) -> io::Result<String>;

    /// Deliver literal text to the pane as a block rather than key events.
    ///
    /// The default types it character by character. The tmux backend overrides
    /// this to route the text through a tmux buffer, so the value never appears
    /// in a `tmux send-keys` argv; use it for secrets. A PTY backend is already
    /// argv-safe (it writes straight to the pseudo-terminal), so the default is
    /// fine there.
    fn paste_text(&self, text: &str) -> io::Result<()> {
        let keys: Vec<Key> = text.chars().map(Key::Char).collect();
        self.send_keys(&keys)
    }

    /// The status of a child process this backend spawned and owns, or `None`
    /// for an attach backend that drives a process it does not own.
    ///
    /// The script runner uses this to fail a run whose program crashed or
    /// exited unexpectedly, so a crash after the last assertion is not reported
    /// as a pass. The default returns `None`, so only the PTY backend (which
    /// owns its child) participates; attach backends are unaffected.
    fn child_exit(&self) -> Option<ChildExit> {
        None
    }

    /// Whether the backend-owned child's output has contained `needle` since it
    /// was spawned, for the `run --fail-on` output guard.
    ///
    /// The default returns `false`, so a backend that does not retain the
    /// child's output stream (every attach backend) never matches. Only the
    /// PTY backend, which already reads the byte stream it owns, overrides this.
    fn output_contains(&self, _needle: &str) -> bool {
        false
    }
}
