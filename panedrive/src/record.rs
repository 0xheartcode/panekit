//! Decode a raw key byte stream into a panedrive script.
//!
//! [`ScriptRecorder`] is fed the bytes a user types (as they arrive, possibly
//! split mid-escape-sequence) and accumulates `.pds` lines: runs of printable
//! characters become a single `type` line, named keys (Enter, arrows, Ctrl-*)
//! become `press` lines. The interactive loop that owns the terminal and the
//! child PTY lives in the `record` CLI subcommand; this pure decoder is where
//! the fiddly parsing is, so it can be unit-tested without a terminal.

/// The stop key for an interactive recording: Ctrl-] (as in telnet), chosen so
/// it does not collide with keys an app is likely to want.
pub const STOP_BYTE: u8 = 0x1d;

/// Incrementally turns typed bytes into script lines.
#[derive(Default)]
pub struct ScriptRecorder {
    lines: Vec<String>,
    /// A pending run of literal (typed) bytes, flushed as one `type` line.
    literal: Vec<u8>,
    /// Bytes held back because they may be the start of an escape sequence that
    /// has not fully arrived yet.
    pending: Vec<u8>,
}

impl ScriptRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw input bytes. Complete keys are decoded now; an incomplete
    /// trailing escape sequence is held until the next call.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut i = 0;
        while i < self.pending.len() {
            let b = self.pending[i];
            if b == 0x1b {
                let rest = &self.pending[i..];
                if rest.len() == 1 {
                    break; // lone ESC so far, wait for more
                }
                if rest[1] == b'[' {
                    if rest.len() < 3 {
                        break; // CSI not complete yet
                    }
                    match rest[2] {
                        b'A' => self.press("Up"),
                        b'B' => self.press("Down"),
                        b'C' => self.press("Right"),
                        b'D' => self.press("Left"),
                        _ => self.press("Escape"), // unknown CSI final
                    }
                    i += 3;
                    continue;
                }
                // ESC followed by a non-`[` byte: record a bare Escape and let
                // the following byte be decoded on its own.
                self.press("Escape");
                i += 1;
                continue;
            }
            match b {
                0x0d | 0x0a => self.press("Enter"),
                0x09 => self.press("Tab"),
                0x08 | 0x7f => self.press("Backspace"),
                // Ctrl-letter: 0x01=C-a .. 0x1a=C-z (Tab/Enter handled above).
                0x01..=0x1a => {
                    let c = (b'a' + (b - 1)) as char;
                    self.press_owned(format!("C-{c}"));
                }
                // Other C0 controls have no key name; skip them.
                0x00 | 0x1c..=0x1f => {}
                _ => {
                    self.literal.push(b);
                }
            }
            i += 1;
        }
        self.pending.drain(0..i);
    }

    /// Flush any pending literal run and return the assembled script text
    /// (always newline-terminated; empty input yields an empty string).
    pub fn finish(mut self) -> String {
        self.flush_literal();
        if self.lines.is_empty() {
            return String::new();
        }
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    fn press(&mut self, name: &str) {
        self.flush_literal();
        self.lines.push(format!("press {name}"));
    }

    fn press_owned(&mut self, name: String) {
        self.flush_literal();
        self.lines.push(format!("press {name}"));
    }

    fn flush_literal(&mut self) {
        if self.literal.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.literal).into_owned();
        self.literal.clear();
        self.lines.push(format!("type {text}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_becomes_a_single_type_line() {
        let mut r = ScriptRecorder::new();
        r.feed(b"hello");
        assert_eq!(r.finish(), "type hello\n");
    }

    #[test]
    fn enter_flushes_the_literal_and_emits_press() {
        let mut r = ScriptRecorder::new();
        r.feed(b"inc\r");
        assert_eq!(r.finish(), "type inc\npress Enter\n");
    }

    #[test]
    fn arrow_keys_decode_even_when_split_across_feeds() {
        let mut r = ScriptRecorder::new();
        // ESC [ A arrives one byte at a time.
        r.feed(b"\x1b");
        r.feed(b"[");
        r.feed(b"A");
        r.feed(b"\x1b[B");
        assert_eq!(r.finish(), "press Up\npress Down\n");
    }

    #[test]
    fn escape_sequence_split_across_two_feeds_decodes_as_one_key() {
        // The ESC byte arrives alone (held back as an incomplete sequence),
        // then the rest of the CSI (`[A`) arrives on the next feed. The finished
        // script must decode the whole thing as a single `press Up`.
        let mut r = ScriptRecorder::new();
        r.feed(b"\x1b");
        r.feed(b"[A");
        assert_eq!(r.finish(), "press Up\n");
    }

    #[test]
    fn ctrl_letters_and_named_keys() {
        let mut r = ScriptRecorder::new();
        r.feed(b"a"); // literal
        r.feed(&[0x03]); // Ctrl-C
        r.feed(&[0x09]); // Tab
        r.feed(&[0x7f]); // Backspace
        assert_eq!(
            r.finish(),
            "type a\npress C-c\npress Tab\npress Backspace\n"
        );
    }

    #[test]
    fn empty_input_yields_empty_script() {
        assert_eq!(ScriptRecorder::new().finish(), "");
    }
}
