//! `panedrive`, a headless driver for terminal UIs.
//!
//! It does two things, and only the first is host-specific:
//!
//! 1. **Send input** into a running UI (via a [`PaneBackend`], tmux today;
//!    zellij / raw-PTY are drop-in impls of the same trait).
//! 2. **Read state** from the UI's [`paneview`](https://docs.rs/paneview) JSON
//!    seam and [`wait_until`] / assert a [`Condition`] over it, completely
//!    host-independent, because it reads JSON, not the screen.
//!
//! The split is deliberate: pairing tmux *input* with a JSON *state* seam gives
//! you real-keybinding fidelity without the flakiness of screen-scraping. See
//! `docs/ARCHITECTURE.md`.

pub mod backend;
pub mod condition;
pub mod driver;
pub mod key;
pub mod script;

#[cfg(feature = "pty")]
pub use backend::pty::PtyBackend;
pub use backend::{PaneBackend, screen::ScreenBackend, tmux::TmuxBackend, zellij::ZellijBackend};
pub use condition::Condition;
pub use driver::{WaitOutcome, read_state_file, wait_until, watch};
pub use key::{Key, TmuxKey, ZellijKey, parse_keys};
pub use script::{RunResult, Step, parse_script, run_script};
