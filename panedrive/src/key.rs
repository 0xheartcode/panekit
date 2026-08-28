//! Logical, backend-agnostic key presses.
//!
//! The driver speaks [`Key`]; each backend translates it into that host's wire
//! encoding. Parsing accepts a small human spec (`Enter`, `Down`, `C-c`, or a
//! literal character) so the CLI and scripts stay readable.

/// A single logical key press, independent of any terminal host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A literal printable character.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    /// `Ctrl` + a letter, e.g. `Key::Ctrl('c')`.
    Ctrl(char),
}

/// How a [`Key`] is handed to `tmux send-keys`: either a named key (`Enter`,
/// `C-c`) or a literal character sent with `-l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxKey {
    Named(String),
    Literal(char),
}

/// How a [`Key`] is handed to `zellij action`: printable characters go through
/// `write-chars` (a literal string), everything else through `write` as the raw
/// terminal byte sequence (the same bytes [`Key::to_bytes`] produces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZellijKey {
    /// A literal character, delivered with `zellij action write-chars`.
    Chars(char),
    /// The raw bytes of a named/control key, delivered with `zellij action
    /// write <byte>...` (each byte as a decimal argument).
    Bytes(Vec<u8>),
}

impl Key {
    /// Parse one token from the human spec.
    ///
    /// Names are case-insensitive (`Enter`/`enter`); `C-x` / `c-x` is Ctrl+x;
    /// any single character is itself. Anything else is an error rather than a
    /// silent no-op.
    pub fn parse(token: &str) -> anyhow::Result<Key> {
        let t = token.trim();
        let key = match t.to_ascii_lowercase().as_str() {
            "enter" | "cr" | "return" => Key::Enter,
            "tab" => Key::Tab,
            "esc" | "escape" => Key::Esc,
            "backspace" | "bspace" | "bs" => Key::Backspace,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            _ => return Self::parse_char_forms(t),
        };
        Ok(key)
    }

    fn parse_char_forms(t: &str) -> anyhow::Result<Key> {
        if let Some(rest) = t.strip_prefix("C-").or_else(|| t.strip_prefix("c-")) {
            return match exactly_one_char(rest) {
                Some(c) => Ok(Key::Ctrl(c)),
                None => anyhow::bail!("expected one char after `C-`, got {t:?}"),
            };
        }
        match exactly_one_char(t) {
            Some(c) => Ok(Key::Char(c)),
            None => anyhow::bail!("unknown key {t:?}"),
        }
    }

    /// Encode as the raw bytes a terminal sends for this key, used by the PTY
    /// backend, which writes straight to the pseudo-terminal. Named keys become
    /// their control/escape sequences; a char becomes its UTF-8 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Key::Char(c) => c.to_string().into_bytes(),
            Key::Enter => vec![b'\r'],
            Key::Tab => vec![b'\t'],
            Key::Backspace => vec![0x7f],
            Key::Esc => vec![0x1b],
            Key::Up => vec![0x1b, b'[', b'A'],
            Key::Down => vec![0x1b, b'[', b'B'],
            Key::Right => vec![0x1b, b'[', b'C'],
            Key::Left => vec![0x1b, b'[', b'D'],
            // Ctrl+letter is the letter with the top three bits cleared.
            Key::Ctrl(c) => vec![(c.to_ascii_uppercase() as u8) & 0x1f],
        }
    }

    /// Translate to the `tmux send-keys` representation.
    pub fn to_tmux(&self) -> TmuxKey {
        match self {
            Key::Char(c) => TmuxKey::Literal(*c),
            Key::Enter => TmuxKey::Named("Enter".into()),
            Key::Tab => TmuxKey::Named("Tab".into()),
            Key::Backspace => TmuxKey::Named("BSpace".into()),
            Key::Esc => TmuxKey::Named("Escape".into()),
            Key::Up => TmuxKey::Named("Up".into()),
            Key::Down => TmuxKey::Named("Down".into()),
            Key::Left => TmuxKey::Named("Left".into()),
            Key::Right => TmuxKey::Named("Right".into()),
            Key::Ctrl(c) => TmuxKey::Named(format!("C-{c}")),
        }
    }

    /// Translate to the `zellij action` representation. Printable characters
    /// become a `write-chars` literal; named and control keys become the raw
    /// byte sequence [`Key::to_bytes`] produces, delivered with `write`.
    pub fn to_zellij(&self) -> ZellijKey {
        match self {
            Key::Char(c) => ZellijKey::Chars(*c),
            other => ZellijKey::Bytes(other.to_bytes()),
        }
    }
}

fn exactly_one_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Parse a whitespace- or comma-separated spec into a sequence of keys, e.g.
/// `"2 Down Down Enter"` or `"C-c"`.
pub fn parse_keys(spec: &str) -> anyhow::Result<Vec<Key>> {
    spec.split([' ', ',', '\t'])
        .filter(|t| !t.is_empty())
        .map(Key::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_keys_case_insensitively() {
        assert_eq!(Key::parse("Enter").unwrap(), Key::Enter);
        assert_eq!(Key::parse("enter").unwrap(), Key::Enter);
        assert_eq!(Key::parse("Down").unwrap(), Key::Down);
        assert_eq!(Key::parse("ESC").unwrap(), Key::Esc);
    }

    #[test]
    fn parses_ctrl_and_literal() {
        assert_eq!(Key::parse("C-c").unwrap(), Key::Ctrl('c'));
        assert_eq!(Key::parse("c-x").unwrap(), Key::Ctrl('x'));
        assert_eq!(Key::parse("q").unwrap(), Key::Char('q'));
        assert_eq!(Key::parse("2").unwrap(), Key::Char('2'));
    }

    #[test]
    fn rejects_garbage_rather_than_guessing() {
        assert!(Key::parse("wat").is_err());
        assert!(Key::parse("C-").is_err());
        assert!(Key::parse("C-ab").is_err());
        assert!(Key::parse("").is_err());
    }

    #[test]
    fn tmux_encoding_splits_literal_from_named() {
        assert_eq!(Key::Char('a').to_tmux(), TmuxKey::Literal('a'));
        assert_eq!(Key::Enter.to_tmux(), TmuxKey::Named("Enter".into()));
        assert_eq!(Key::Esc.to_tmux(), TmuxKey::Named("Escape".into()));
        assert_eq!(Key::Ctrl('c').to_tmux(), TmuxKey::Named("C-c".into()));
    }

    #[test]
    fn zellij_encoding_splits_chars_from_raw_bytes() {
        assert_eq!(Key::Char('a').to_zellij(), ZellijKey::Chars('a'));
        assert_eq!(Key::Char('é').to_zellij(), ZellijKey::Chars('é'));
        assert_eq!(Key::Enter.to_zellij(), ZellijKey::Bytes(vec![b'\r']));
        assert_eq!(Key::Esc.to_zellij(), ZellijKey::Bytes(vec![0x1b]));
        assert_eq!(
            Key::Up.to_zellij(),
            ZellijKey::Bytes(vec![0x1b, b'[', b'A'])
        );
        assert_eq!(Key::Ctrl('c').to_zellij(), ZellijKey::Bytes(vec![0x03]));
    }

    #[test]
    fn parse_keys_handles_separators_and_skips_blanks() {
        let ks = parse_keys("2  Down,Down Enter").unwrap();
        assert_eq!(ks, vec![Key::Char('2'), Key::Down, Key::Down, Key::Enter]);
        assert!(parse_keys("").unwrap().is_empty());
    }

    #[test]
    fn to_bytes_encodes_control_and_escape_sequences() {
        assert_eq!(Key::Char('a').to_bytes(), b"a");
        assert_eq!(Key::Char('é').to_bytes(), "é".as_bytes());
        assert_eq!(Key::Enter.to_bytes(), vec![b'\r']);
        assert_eq!(Key::Tab.to_bytes(), vec![b'\t']);
        assert_eq!(Key::Backspace.to_bytes(), vec![0x7f]);
        assert_eq!(Key::Esc.to_bytes(), vec![0x1b]);
        assert_eq!(Key::Up.to_bytes(), vec![0x1b, b'[', b'A']);
        assert_eq!(Key::Down.to_bytes(), vec![0x1b, b'[', b'B']);
        assert_eq!(Key::Right.to_bytes(), vec![0x1b, b'[', b'C']);
        assert_eq!(Key::Left.to_bytes(), vec![0x1b, b'[', b'D']);
        assert_eq!(Key::Ctrl('c').to_bytes(), vec![0x03]);
        assert_eq!(Key::Ctrl('a').to_bytes(), vec![0x01]);
    }

    #[test]
    fn parses_every_named_key_and_its_aliases() {
        assert_eq!(Key::parse("Tab").unwrap(), Key::Tab);
        assert_eq!(Key::parse("Backspace").unwrap(), Key::Backspace);
        assert_eq!(Key::parse("bspace").unwrap(), Key::Backspace);
        assert_eq!(Key::parse("bs").unwrap(), Key::Backspace);
        assert_eq!(Key::parse("Up").unwrap(), Key::Up);
        assert_eq!(Key::parse("Left").unwrap(), Key::Left);
        assert_eq!(Key::parse("Right").unwrap(), Key::Right);
        assert_eq!(Key::parse("return").unwrap(), Key::Enter);
    }
}
