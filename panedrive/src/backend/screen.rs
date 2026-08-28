//! The GNU screen backend: drives a session via `screen -X stuff` and reads it
//! via `screen -X hardcopy`. Like the tmux and zellij backends it *attaches* to
//! an already-running session rather than owning the process.
//!
//! screen's `stuff` injects a raw byte string into the pane's input, so, like
//! the PTY backend, a [`Key`] is delivered as the exact bytes a terminal sends
//! ([`Key::to_bytes`]); no named-key translation is needed. The "pane" address
//! here is a **screen session name** (`screen -S <name> -X ...`).

use super::PaneBackend;
use crate::key::Key;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Drives the current window of one screen session, addressed by session name.
#[derive(Debug, Clone)]
pub struct ScreenBackend {
    session: String,
}

impl ScreenBackend {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
        }
    }

    /// Run one `screen -S <session> -X <command> <args...>`, surfacing a
    /// non-zero exit as an error naming the session.
    fn command(&self, args: &[&str]) -> io::Result<()> {
        let status = Command::new("screen")
            .arg("-S")
            .arg(&self.session)
            .arg("-X")
            .args(args)
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "screen -X {} failed for session {} (is it running?)",
                args.first().copied().unwrap_or_default(),
                self.session
            )));
        }
        Ok(())
    }

    /// Where `capture` asks screen to write its `hardcopy`: a temp path owned by
    /// this process. The session name is reduced to filename-safe characters so
    /// a name with a path separator cannot redirect the dump.
    fn dump_path(&self) -> PathBuf {
        let safe: String = self
            .session
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::env::temp_dir().join(format!(
            "panedrive-screen-{}-{}.dump",
            safe,
            std::process::id()
        ))
    }
}

/// The exact bytes to `stuff` for a key sequence: each key as its raw terminal
/// bytes, concatenated. `to_bytes` yields ASCII control codes and UTF-8 text,
/// which is always valid UTF-8, so the result is a lossless `String`.
fn stuff_payload(keys: &[Key]) -> String {
    let mut bytes = Vec::new();
    for key in keys {
        bytes.extend_from_slice(&key.to_bytes());
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

impl PaneBackend for ScreenBackend {
    fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
        let payload = stuff_payload(keys);
        if payload.is_empty() {
            return Ok(());
        }
        // screen's `stuff` takes the whole next argument as the literal string,
        // including a leading `-` (verified live), so no `--` guard is needed;
        // adding one actually stops screen from stuffing anything.
        self.command(&["stuff", &payload])
    }

    fn capture(&self) -> io::Result<String> {
        let path = self.dump_path();
        let path_str = path.to_string_lossy().into_owned();
        self.command(&["hardcopy", &path_str])?;
        // `-X hardcopy` returns before screen has written the file, so poll
        // briefly for it rather than racing the write.
        let contents = read_when_ready(&path);
        let _ = std::fs::remove_file(&path);
        contents
    }
}

/// Read `path`, retrying while it does not exist yet (up to ~500ms) so a
/// just-issued `hardcopy` that has not landed is waited out rather than failing.
fn read_when_ready(path: &Path) -> io::Result<String> {
    for _ in 0..50 {
        match std::fs::read_to_string(path) {
            Ok(contents) => return Ok(contents),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    std::fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::parse_keys;

    #[test]
    fn stuff_payload_concatenates_raw_bytes() {
        // "hi" then Enter → "hi\r"; the named key is a raw CR, not a name.
        assert_eq!(stuff_payload(&parse_keys("h i Enter").unwrap()), "hi\r");
        // Up arrow is the ESC [ A sequence.
        assert_eq!(stuff_payload(&parse_keys("Up").unwrap()), "\u{1b}[A");
        // Ctrl-c is a lone 0x03.
        assert_eq!(stuff_payload(&parse_keys("C-c").unwrap()), "\u{3}");
        assert_eq!(stuff_payload(&[]), "");
    }

    #[test]
    fn dump_path_is_session_scoped_sanitized_and_in_temp() {
        let p = ScreenBackend::new("a/b").dump_path();
        assert_eq!(p.parent().unwrap(), std::env::temp_dir());
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("panedrive-screen-a_b-"), "got {name}");
        assert!(name.ends_with(".dump"));
    }
}
