//! asciinema v2 "cast" recording of a driven run.
//!
//! A cast is a header line followed by one `[t, "o", data]` line per output
//! chunk, so writing one is pure formatting over `serde_json`, no new
//! dependency. panedrive records a cast on the two backends where it can see the
//! raw output stream:
//!
//! - **pty**: it owns the child's stream, so the tap forwards every chunk
//!   straight to a [`CastWriter`] (see [`PtyBackend::spawn_tapped`]).
//! - **tmux**: [`TmuxCastRecorder`] taps the pane with `tmux pipe-pane` and
//!   tails the piped bytes on a background thread, timestamping on arrival.
//!
//! screen and zellij are intentionally unsupported: they expose only periodic
//! snapshots, not a byte stream, so a smooth cast is not recoverable there.
//!
//! [`PtyBackend::spawn_tapped`]: crate::backend::pty::PtyBackend::spawn_tapped

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Writes an asciinema v2 cast: a header line, then timestamped output events.
pub struct CastWriter<W: Write> {
    out: W,
    start: Instant,
}

impl<W: Write> CastWriter<W> {
    /// Write the v2 header (`{"version":2,"width":..,"height":..}`) and start
    /// the clock. Subsequent [`write_output`](Self::write_output) events are
    /// timestamped relative to this call.
    pub fn new(mut out: W, width: u16, height: u16) -> io::Result<Self> {
        let header = serde_json::json!({
            "version": 2,
            "width": width,
            "height": height,
            "title": "panedrive run",
        });
        writeln!(out, "{header}")?;
        out.flush()?;
        Ok(Self {
            out,
            start: Instant::now(),
        })
    }

    /// Append one output event carrying `bytes`, timestamped at the elapsed time
    /// since the cast began.
    pub fn write_output(&mut self, bytes: &[u8]) -> io::Result<()> {
        let t = self.start.elapsed().as_secs_f64();
        self.write_output_at(t, bytes)
    }

    /// Append one output event at an explicit time offset (seconds). Invalid
    /// UTF-8 (e.g. a multi-byte sequence split across reads) is replaced rather
    /// than dropped, since a cast's data must be valid UTF-8.
    pub fn write_output_at(&mut self, t: f64, bytes: &[u8]) -> io::Result<()> {
        let data = String::from_utf8_lossy(bytes);
        let line = serde_json::json!([t, "o", data]);
        writeln!(self.out, "{line}")?;
        self.out.flush()
    }
}

/// Records a tmux pane's output to a cast. Starts `tmux pipe-pane` writing the
/// pane's raw output to a temp file, and tails that file on a background thread,
/// timestamping bytes as they arrive. Call [`stop`](Self::stop) when the run
/// ends; dropping also stops it.
pub struct TmuxCastRecorder {
    pane: String,
    raw_path: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TmuxCastRecorder {
    /// Begin recording pane `pane` into the already-opened cast file `out`.
    pub fn start(pane: String, out: File) -> io::Result<Self> {
        let (width, height) = pane_size(&pane);
        let raw_path =
            std::env::temp_dir().join(format!("panedrive-cast-{}.raw", std::process::id()));
        // Start empty so the tail thread sees only output from now on.
        File::create(&raw_path)?;
        // Tell tmux to pipe this pane's output into `cat >> rawfile`.
        run_tmux(&[
            "pipe-pane",
            "-o",
            "-t",
            &pane,
            &format!("cat >> {}", shell_quote(&raw_path)),
        ])?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let raw = raw_path.clone();
        let handle = std::thread::spawn(move || {
            tail_into_cast(&raw, out, width, height, &thread_stop);
        });
        Ok(Self {
            pane,
            raw_path,
            stop,
            handle: Some(handle),
        })
    }

    /// Stop piping, drain the last bytes, and join the tail thread.
    pub fn stop(&mut self) {
        // Turn the pipe off first so no new bytes arrive, give `cat` a moment to
        // flush, then signal the tail thread to drain and exit.
        let _ = run_tmux(&["pipe-pane", "-o", "-t", &self.pane]);
        std::thread::sleep(Duration::from_millis(80));
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.raw_path);
    }
}

impl Drop for TmuxCastRecorder {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.stop();
        }
    }
}

/// Tail `raw_path` into a cast on `out` until `stop` is set and no bytes remain.
fn tail_into_cast(
    raw_path: &std::path::Path,
    out: File,
    width: u16,
    height: u16,
    stop: &AtomicBool,
) {
    let mut cast = match CastWriter::new(out, width, height) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut file = loop {
        match File::open(raw_path) {
            Ok(f) => break f,
            Err(_) if !stop.load(Ordering::SeqCst) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    };
    let mut buf = [0u8; 4096];
    loop {
        match file.read(&mut buf) {
            Ok(0) => {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(n) => {
                let _ = cast.write_output(&buf[..n]);
            }
            Err(_) => return,
        }
    }
}

/// Query a tmux pane's `width height`, defaulting to 80x24 if unavailable.
fn pane_size(pane: &str) -> (u16, u16) {
    let out = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_width} #{pane_height}",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            let mut it = s.split_whitespace();
            if let (Some(Ok(w)), Some(Ok(h))) = (
                it.next().map(str::parse::<u16>),
                it.next().map(str::parse::<u16>),
            ) {
                return (w, h);
            }
        }
    }
    (80, 24)
}

fn run_tmux(args: &[&str]) -> io::Result<()> {
    let status = Command::new("tmux").args(args).status()?;
    if !status.success() {
        return Err(io::Error::other(format!("tmux {} failed", args.join(" "))));
    }
    Ok(())
}

/// Single-quote a path for a tmux `pipe-pane` shell command.
fn shell_quote(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn cast_writes_a_v2_header_and_output_events() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut cast = CastWriter::new(&mut buf, 80, 24).unwrap();
            cast.write_output_at(0.0, b"hello").unwrap();
            cast.write_output_at(0.5, b"\x1b[2Jworld").unwrap();
        }
        let text = String::from_utf8(buf).unwrap();
        let mut lines = text.lines();

        let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 80);
        assert_eq!(header["height"], 24);

        let e1: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(e1[0], 0.0);
        assert_eq!(e1[1], "o");
        assert_eq!(e1[2], "hello");

        let e2: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(e2[1], "o");
        // control bytes survive as an escaped UTF-8 string
        assert_eq!(e2[2], "\u{1b}[2Jworld");
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote(std::path::Path::new("/tmp/a b")), "'/tmp/a b'");
    }
}
