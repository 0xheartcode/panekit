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

use super::{ChildExit, PaneBackend};
use crate::key::Key;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// How many trailing bytes of the child's output to retain for `--fail-on`
/// scanning. A crash banner ("panicked", a stack trace) lands near the *end* of
/// a run, so the buffer keeps the most recent output and trims the oldest. A
/// test session's output is far smaller than this; the cap only bounds a
/// pathologically chatty program. The buffer is allowed to grow to twice the
/// cap before trimming back, so the trim is amortized and rare.
const OUTPUT_CAP: usize = 1 << 20; // 1 MiB

/// A TUI running in a pseudo-terminal owned by this process.
pub struct PtyBackend {
    writer: Mutex<Box<dyn Write + Send>>,
    screen: Arc<Mutex<vt100::Parser>>,
    // The most recent non-blank rendered frame. A TUI that quits restores the
    // primary screen (the leave-alt-screen sequence), blanking `screen`, so a
    // post-exit capture would otherwise see nothing. This retains the last
    // meaningful frame; `capture` falls back to it once the child has exited, so
    // an `expect-screen` after a clean quit can still assert the final screen.
    last_frame: Arc<Mutex<Option<String>>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    // A bounded, lossy-UTF-8 copy of the child's output stream, scanned by
    // `output_contains` for the `--fail-on` guard.
    output: Arc<Mutex<String>>,
    // Kept alive so the PTY stays open for the lifetime of the backend.
    _master: Box<dyn MasterPty + Send>,
    _reader: JoinHandle<()>,
}

/// A sink for the PTY's raw output bytes, called on the reader thread for every
/// chunk as it arrives. Used to record an asciinema cast alongside driving.
pub type OutputTap = Box<dyn FnMut(&[u8]) + Send>;

/// Keep only the tail of `buf` once it grows past twice `cap`: drop the oldest
/// bytes back down to `cap`, cutting at a char boundary so the retained string
/// stays valid UTF-8. The crash banner a `--fail-on` guard looks for lands near
/// the end of a run, so the tail is what matters; growing to 2x before trimming
/// keeps the trim amortized and bounds memory to under 2x `cap`.
fn retain_tail(buf: &mut String, cap: usize) {
    if buf.len() > cap * 2 {
        let mut cut = buf.len() - cap;
        while cut < buf.len() && !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

impl PtyBackend {
    /// Spawn `program args...` in a fresh `rows`×`cols` PTY and start reading
    /// its output into the screen model.
    pub fn spawn(program: &str, args: &[&str], rows: u16, cols: u16) -> io::Result<Self> {
        Self::spawn_tapped(program, args, rows, cols, None)
    }

    /// Like [`spawn`](Self::spawn), but also forward every raw output chunk to
    /// `tap` as it is read (in addition to feeding the `vt100` screen model), so
    /// a recorder can capture the byte stream this process already owns.
    pub fn spawn_tapped(
        program: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        mut tap: Option<OutputTap>,
    ) -> io::Result<Self> {
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
        let last_frame = Arc::new(Mutex::new(None));
        let lf_sink = Arc::clone(&last_frame);
        let output = Arc::new(Mutex::new(String::new()));
        let out_sink = Arc::clone(&output);
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or closed PTY
                    Ok(n) => {
                        if let Ok(mut parser) = sink.lock() {
                            parser.process(&buf[..n]);
                            // Retain the last non-blank frame. A quit restores the
                            // (blank) primary screen, which we must not record over
                            // the real final frame, so blank snapshots are ignored.
                            let frame = parser.screen().contents();
                            if !frame.trim().is_empty() {
                                if let Ok(mut lf) = lf_sink.lock() {
                                    *lf = Some(frame);
                                }
                            }
                        }
                        if let Ok(mut recorded) = out_sink.lock() {
                            recorded.push_str(&String::from_utf8_lossy(&buf[..n]));
                            retain_tail(&mut recorded, OUTPUT_CAP);
                        }
                        if let Some(tap) = tap.as_mut() {
                            tap(&buf[..n]);
                        }
                    }
                }
            }
        });

        Ok(Self {
            writer: Mutex::new(writer),
            screen,
            last_frame,
            child: Mutex::new(child),
            output,
            _master: pair.master,
            _reader: reader_thread,
        })
    }

    /// Write raw bytes straight to the child's PTY (bypassing key encoding),
    /// used by `record` to forward the user's keystrokes verbatim.
    pub fn write_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("pty writer poisoned"))?;
        w.write_all(bytes)?;
        w.flush()
    }

    /// Whether the child is still running.
    pub fn is_alive(&self) -> bool {
        matches!(self.child_exit(), Some(ChildExit::Running))
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
        let current = {
            let parser = self
                .screen
                .lock()
                .map_err(|_| io::Error::other("pty screen poisoned"))?;
            parser.screen().contents()
        };
        // While the child runs, always report the live screen (unchanged
        // behaviour). Only once it has exited, and only if the live screen is now
        // blank (a quit restored the primary screen), fall back to the last
        // non-blank frame so a post-exit expect-screen still sees the final UI.
        if current.trim().is_empty() && !matches!(self.child_exit(), Some(ChildExit::Running)) {
            if let Ok(lf) = self.last_frame.lock() {
                if let Some(frame) = lf.as_ref() {
                    return Ok(frame.clone());
                }
            }
        }
        Ok(current)
    }

    fn child_exit(&self) -> Option<ChildExit> {
        match self.child.lock() {
            // A reported exit carries the code; `try_wait` erroring means we
            // cannot tell, so treat the child as still running rather than
            // inventing an exit.
            Ok(mut c) => match c.try_wait() {
                Ok(Some(status)) => Some(ChildExit::Exited(Some(status.exit_code() as i32))),
                Ok(None) | Err(_) => Some(ChildExit::Running),
            },
            // A poisoned lock means the reader thread panicked; we own a child
            // either way, so report that we cannot observe it as "running".
            Err(_) => Some(ChildExit::Running),
        }
    }

    fn output_contains(&self, needle: &str) -> bool {
        match self.output.lock() {
            Ok(recorded) => recorded.contains(needle),
            Err(_) => false,
        }
    }
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::retain_tail;

    #[test]
    fn retain_tail_does_not_trim_at_or_below_twice_cap() {
        // len == 2*cap exactly is the boundary: trimming starts strictly ABOVE
        // it. Pinning the exact threshold here (not just "something small")
        // distinguishes `> 2*cap` from `>= 2*cap` and `cap * 2` from `cap + 2`.
        let cap = 5;
        let mut s = "0123456789".to_string(); // 10 bytes == 2*cap
        retain_tail(&mut s, cap);
        assert_eq!(
            s, "0123456789",
            "a buffer at exactly 2*cap is left untouched"
        );

        let mut small = "abc".to_string(); // well under
        retain_tail(&mut small, cap);
        assert_eq!(small, "abc");
    }

    #[test]
    fn retain_tail_keeps_the_end_once_past_twice_cap() {
        let cap = 8;
        let mut s = "0123456789".to_string();
        s.push_str("abcdefghij"); // 20 bytes > 2*cap (16)
        retain_tail(&mut s, cap);
        // Keeps the last `cap` bytes: the newest output survives, the oldest is
        // dropped.
        assert_eq!(s, "cdefghij");
    }

    #[test]
    fn retain_tail_advances_to_a_char_boundary_before_cutting() {
        // "é" is two bytes. Six of them is 12 bytes; with cap 5 the raw cut
        // lands at byte 7, mid-character, so the cut must advance to byte 8. A
        // cut that did not would panic in drain or corrupt the UTF-8.
        let mut s = "é".repeat(6);
        retain_tail(&mut s, 5);
        assert_eq!(s, "éé", "cut advanced to the next char boundary");
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn retain_tail_preserves_a_needle_in_the_surviving_tail() {
        // The whole point: a crash banner emitted at the end of a long run must
        // still be matchable after trimming.
        let cap = 16;
        let mut s = "x".repeat(cap * 3);
        s.push_str("panicked");
        retain_tail(&mut s, cap);
        assert!(
            s.contains("panicked"),
            "the crash banner in the tail must survive the trim"
        );
    }
}
