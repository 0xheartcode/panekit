//! Terminal-host backends. A [`PaneBackend`] is the *only* host-specific part
//! of the driver: it sends keys into a running UI and captures its screen.
//! Everything above it, waiting, asserting, reading the JSON seam, is
//! backend-agnostic, so adding zellij or a raw PTY is one new impl of this
//! trait, nothing else.

pub mod tmux;

#[cfg(feature = "pty")]
pub mod pty;

use crate::key::Key;
use std::io;

/// A channel into one running terminal UI: press keys, read the screen.
pub trait PaneBackend {
    /// Send a sequence of logical key presses to the target pane, in order.
    fn send_keys(&self, keys: &[Key]) -> io::Result<()>;

    /// Capture the pane's currently visible text (best-effort; used only when
    /// no [`paneview`](https://docs.rs/paneview) JSON seam is available).
    fn capture(&self) -> io::Result<String>;
}
