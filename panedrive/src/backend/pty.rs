//! The PTY backend: **spawn** a TUI in a pseudo-terminal this process owns,
//! then drive it, no tmux, no zellij, no multiplexer. Ideal for CI and
//! in-process integration tests, where there is no terminal to attach to.
//!
//! Unlike [`TmuxBackend`](super::tmux::TmuxBackend), which *attaches* to an
//! already-running pane, `PtyBackend` *owns* the child: it holds the PTY master,
//! reads its output on a background thread into a `vt100` screen model, and
//! kills the child on drop.
//!
//! Requires the `pty` cargo feature.

use super::PaneBackend;
use crate::key::Key;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A TUI running in a pseudo-terminal owned by this process.
pub struct PtyBackend {
    writer: Mutex<Box<dyn Write + Send>>,
    screen: Arc<Mutex<vt100::Parser>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    // Kept alive so the PTY stays open for the lifetime of the backend.
    _master: Box<dyn MasterPty + Send>,
    _reader: JoinHandle<()>,
}

impl PtyBackend {
    /// Spawn `program args...` in a fresh `rows`×`cols` PTY and start reading
    /// its output into the screen model.
    pub fn spawn(program: &str, args: &[&str], rows: u16, cols: u16) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(format!("spawn {program}: {e}")))?;
        // The slave handle is no longer needed once the child holds it.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(format!("clone reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(format!("take writer: {e}")))?;

        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let sink = Arc::clone(&screen);
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or closed PTY
                    Ok(n) => {
                        if let Ok(mut parser) = sink.lock() {
                            parser.process(&buf[..n]);
                        }
                    }
                }
            }
        });

        Ok(Self {
            writer: Mutex::new(writer),
            screen,
            child: Mutex::new(child),
            _master: pair.master,
            _reader: reader_thread,
        })
    }

    /// Whether the child is still running.
    pub fn is_alive(&self) -> bool {
        match self.child.lock() {
            Ok(mut c) => matches!(c.try_wait(), Ok(None)),
            Err(_) => false,
        }
    }

    /// Kill the child process (idempotent; also runs on drop).
    pub fn kill(&self) -> io::Result<()> {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
        Ok(())
    }
}

impl PaneBackend for PtyBackend {
    fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("pty writer poisoned"))?;
        for key in keys {
            w.write_all(&key.to_bytes())?;
        }
        w.flush()
    }

    fn capture(&self) -> io::Result<String> {
        let parser = self
            .screen
            .lock()
            .map_err(|_| io::Error::other("pty screen poisoned"))?;
        Ok(parser.screen().contents())
    }
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
