//! End-to-end test of the `record` subcommand: run the real `panedrive record`
//! binary against a PTY-spawned program, feed it a keystroke stream on stdin
//! (which, being piped, keeps the loop out of raw mode), and confirm the bytes
//! were decoded into the expected `.pds` script. This exercises `spawn_record`
//! and its terminal-restore guard, which unit tests of the decoder cannot.
//! Only built with `--features pty`.

#![cfg(feature = "pty")]

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn record_decodes_piped_keystrokes_into_a_pds_script() {
    let bin = env!("CARGO_BIN_EXE_panedrive");
    let out = std::env::temp_dir().join(format!("panedrive-record-pty-{}.pds", std::process::id()));
    std::fs::remove_file(&out).ok();

    // `cat` is a always-available program that stays alive in the PTY while we
    // feed input, so the record loop keeps reading rather than exiting early on
    // a dead child.
    let mut child = Command::new(bin)
        .args(["record", "--script", &out.to_string_lossy(), "--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn panedrive record");

    // Type `hi`, an Up arrow (ESC [ A), Enter (CR), then Ctrl-] to stop.
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(b"hi\x1b[A\r\x1d")
            .expect("write keystrokes");
        stdin.flush().expect("flush");
    } // dropping stdin closes it

    let status = child.wait().expect("wait for record");
    assert!(status.success(), "record should exit 0, got {status:?}");

    let text = std::fs::read_to_string(&out).expect("recorded .pds written");
    std::fs::remove_file(&out).ok();

    assert_eq!(
        text, "type hi\npress Up\npress Enter\n",
        "the piped keystrokes should decode to these steps, got {text:?}"
    );
}
