//! An **in-process** test harness: drive a Rust terminal-UI model directly,
//! without spawning a process or attaching a multiplexer.
//!
//! The out-of-process driver ([`run_script`](crate::run_script) over a
//! [`PaneBackend`]) is the integration shape: it presses real keybindings into a
//! real terminal and reads the JSON seam a running program writes. This harness
//! is its unit-test complement. You construct your model, feed it keys, and
//! assert over the *same* [`paneview`](https://docs.rs/paneview) seam and the
//! *same* [`Condition`] grammar, but everything happens in one process with no
//! terminal, no settling, and no cleanup.
//!
//! It is a thin adapter, not a second engine: [`Harness::run`] reuses the whole
//! [`run_script`](crate::run_script) runner, with an in-process backend whose
//! `send_keys` applies keys straight to the model and a probe that reads
//! [`DumpState::dump_state`]. So a `.pds` script that passes here presses the
//! same keys and checks the same conditions it would over tmux or a PTY, only
//! synchronously. (Settling is therefore unnecessary and disabled: an
//! in-process model updates the instant a key is applied, so there is no
//! asynchronous gap to absorb, and a satisfied `wait-until` returns on its
//! first poll. A `sleep` step and an *unmet* `wait-until` still consume real
//! wall-clock time — the runner is reused verbatim. A `capture` step yields the
//! empty string, since there is no rendered screen — the seam is the truth.)
//!
//! # Example
//!
//! ```
//! use panedrive::{Harness, InProcessUi, Key, RunResult};
//! use paneview::{dump_serialize, DumpState};
//! use serde::Serialize;
//!
//! #[derive(Serialize)]
//! struct Counter {
//!     count: i64,
//!     last: &'static str,
//! }
//!
//! impl DumpState for Counter {
//!     fn dump_state(&self) -> serde_json::Value {
//!         dump_serialize(self)
//!     }
//! }
//!
//! impl InProcessUi for Counter {
//!     fn apply_key(&mut self, key: &Key) {
//!         match key {
//!             Key::Up => {
//!                 self.count += 1;
//!                 self.last = "up";
//!             }
//!             Key::Down => {
//!                 self.count -= 1;
//!                 self.last = "down";
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//!
//! let mut ui = Harness::new(Counter { count: 0, last: "none" });
//! ui.press("Up Up Down").unwrap();
//! ui.assert("count=1").unwrap();
//! ui.assert("last=down").unwrap();
//!
//! // Or drive it with a whole script, exactly as `panedrive run` would.
//! let outcome = ui.run("press Up\nassert count=2\nassert last=up").unwrap();
//! assert_eq!(outcome, RunResult::Passed);
//! ```

use crate::backend::PaneBackend;
use crate::condition::Condition;
use crate::key::{self, Key};
use crate::script::{RunOptions, RunResult, parse_script, probe_sink, run_script};
use paneview::DumpState;
use serde_json::Value;
use std::cell::{Ref, RefCell};
use std::io;

/// A Rust terminal-UI model that can be driven in-process by a [`Harness`].
///
/// Implement it on the same model that already implements
/// [`DumpState`](paneview::DumpState): the seam reports state, and this trait
/// says how the model consumes input. Map each logical [`Key`] to the state
/// change your real key handler makes, so a script drives the harness the same
/// way it drives the running program.
pub trait InProcessUi: DumpState {
    /// Apply one logical key press to the model, mutating it in place.
    fn apply_key(&mut self, key: &Key);

    /// Apply literal text as a block — the paste transport. This backs
    /// [`Harness::type_text`] and a scripted `type --paste` step. The default
    /// applies one [`Key::Char`] per character (identical to per-key typing);
    /// override it only if your model treats a pasted block differently from
    /// individual keystrokes. A plain `type` step (no `--paste`) always delivers
    /// per character through [`apply_key`](InProcessUi::apply_key), mirroring how
    /// the real backends separate keystroke typing from block paste — so an
    /// override affects `type_text`/`type --paste`, not a plain `type`.
    fn apply_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.apply_key(&Key::Char(ch));
        }
    }
}

/// A [`PaneBackend`] that delivers input straight to an in-process model instead
/// of a pane: `send_keys` routes each key through [`InProcessUi::apply_key`], and
/// `paste_text` routes a block through [`InProcessUi::apply_text`] — mirroring how
/// the real backends separate keystroke typing from block paste, so a scripted
/// `type --paste` reaches the same code path here. `capture` returns the empty
/// string (there is no rendered screen — the seam is the truth).
struct InProcessBackend<'a, M: InProcessUi> {
    model: &'a RefCell<M>,
}

impl<M: InProcessUi> PaneBackend for InProcessBackend<'_, M> {
    fn send_keys(&self, keys: &[Key]) -> io::Result<()> {
        let mut model = self.model.borrow_mut();
        for k in keys {
            model.apply_key(k);
        }
        Ok(())
    }

    fn capture(&self) -> io::Result<String> {
        // No terminal in-process: the seam, not the screen, is the truth.
        Ok(String::new())
    }

    fn paste_text(&self, text: &str) -> io::Result<()> {
        // Route paste through apply_text (the block transport), not per-key, so
        // a `type --paste` step exercises the same path as `Harness::type_text`.
        self.model.borrow_mut().apply_text(text);
        Ok(())
    }
}

/// Drives an [`InProcessUi`] model with keys and scripts, asserting over its
/// [`DumpState`](paneview::DumpState) seam. See the [module docs](self) for the
/// full picture and an example.
pub struct Harness<M: InProcessUi> {
    // The model lives behind a `RefCell` because the in-process backend takes
    // `&self` yet must mutate the model, and because `run` hands the same model
    // to both the backend and the state probe at once. The two only ever borrow
    // it at disjoint moments within a step, so the runtime borrow never panics.
    model: RefCell<M>,
}

impl<M: InProcessUi> Harness<M> {
    /// Wrap a fresh model in a harness.
    pub fn new(model: M) -> Self {
        Self {
            model: RefCell::new(model),
        }
    }

    /// Borrow the underlying model, e.g. to read fields the seam does not expose.
    pub fn model(&self) -> Ref<'_, M> {
        self.model.borrow()
    }

    /// The current state seam, exactly what an assertion checks against.
    pub fn state(&self) -> Value {
        self.model.borrow().dump_state()
    }

    /// Press a key spec (`"Down Down Enter"`, `"C-c"`, `"2 Down"`) into the
    /// model. Returns `self` so presses chain. Errors only if the spec does not
    /// parse.
    pub fn press(&mut self, spec: &str) -> anyhow::Result<&mut Self> {
        let keys = key::parse_keys(spec)?;
        let model = self.model.get_mut();
        for k in &keys {
            model.apply_key(k);
        }
        Ok(self)
    }

    /// Type literal text into the model, character by character (via
    /// [`InProcessUi::apply_text`]). Returns `self` so calls chain.
    pub fn type_text(&mut self, text: &str) -> &mut Self {
        self.model.get_mut().apply_text(text);
        self
    }

    /// Check a [`Condition`] spec (`"count=2"`, `"focus=fleet"`, `"n>=3"`)
    /// against the current seam. `Ok(())` if it holds; otherwise an `Err` whose
    /// message names the condition and what the path actually held — the same
    /// explanation a failing `assert` step produces.
    pub fn assert(&self, spec: &str) -> anyhow::Result<()> {
        let cond = Condition::parse(spec)?;
        let state = self.model.borrow().dump_state();
        if cond.eval(&state) {
            Ok(())
        } else {
            anyhow::bail!(
                "assert {} did not hold ({})",
                cond.to_spec(),
                cond.observed(&state).describe(cond.path())
            );
        }
    }

    /// Run a whole `.pds` script against the model, reusing the full
    /// [`run_script`](crate::run_script) runner. Presses, types, waits, asserts,
    /// and captures behave as they do out-of-process, only synchronously (no
    /// settling; `capture` yields the empty string). Returns the same
    /// [`RunResult`] the CLI would, so a script is portable between the harness
    /// and a live run.
    pub fn run(&mut self, script: &str) -> anyhow::Result<RunResult> {
        let steps = parse_script(script)?;
        let model = &self.model;
        let backend = InProcessBackend { model };
        let mut sink = probe_sink(|| Some(model.borrow().dump_state()));
        run_script(&steps, &backend, &RunOptions::new(), &mut sink)
    }

    /// Consume the harness and return the model, e.g. to inspect final state
    /// beyond the seam after a run.
    pub fn into_model(self) -> M {
        self.model.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paneview::dump_serialize;
    use serde::Serialize;

    /// A tiny model: a counter that also remembers the last direction and can
    /// accumulate typed text, enough to exercise press, type, and assert.
    #[derive(Serialize)]
    struct Counter {
        count: i64,
        last: String,
        buf: String,
    }

    impl Counter {
        fn new() -> Self {
            Self {
                count: 0,
                last: "none".into(),
                buf: String::new(),
            }
        }
    }

    impl DumpState for Counter {
        fn dump_state(&self) -> Value {
            dump_serialize(self)
        }
    }

    impl InProcessUi for Counter {
        fn apply_key(&mut self, key: &Key) {
            match key {
                Key::Up => {
                    self.count += 1;
                    self.last = "up".into();
                }
                Key::Down => {
                    self.count -= 1;
                    self.last = "down".into();
                }
                Key::Char(c) => self.buf.push(*c),
                _ => {}
            }
        }
    }

    #[test]
    fn press_applies_keys_and_the_seam_moves() {
        let mut ui = Harness::new(Counter::new());
        ui.press("Up Up Up Down").unwrap();
        assert_eq!(ui.state()["count"], serde_json::json!(2));
        assert_eq!(ui.state()["last"], serde_json::json!("down"));
    }

    #[test]
    fn press_chains() {
        let mut ui = Harness::new(Counter::new());
        ui.press("Up").unwrap().press("Up").unwrap();
        ui.assert("count=2").unwrap();
    }

    #[test]
    fn type_text_feeds_characters_through_apply_key() {
        let mut ui = Harness::new(Counter::new());
        ui.type_text("hi");
        assert_eq!(ui.state()["buf"], serde_json::json!("hi"));
    }

    #[test]
    fn assert_holds_and_explains_a_miss() {
        let mut ui = Harness::new(Counter::new());
        ui.press("Up").unwrap();
        ui.assert("count=1").unwrap();

        let err = ui.assert("count=5").unwrap_err().to_string();
        assert!(
            err.contains("count=5"),
            "message names the condition: {err}"
        );
        assert!(
            err.contains("was 1"),
            "message shows the actual value: {err}"
        );
    }

    #[test]
    fn assert_rejects_an_unparseable_condition() {
        let ui = Harness::new(Counter::new());
        assert!(ui.assert("this is not a condition spec").is_err());
    }

    #[test]
    fn run_drives_a_whole_script_and_passes() {
        let mut ui = Harness::new(Counter::new());
        let outcome = ui
            .run("press Up\npress Up\nassert count=2\nassert last=up\ntype ab\nassert buf=ab")
            .unwrap();
        assert_eq!(outcome, RunResult::Passed);
    }

    #[test]
    fn run_reports_a_failing_assertion() {
        let mut ui = Harness::new(Counter::new());
        let outcome = ui.run("press Up\nassert count=99").unwrap();
        match outcome {
            RunResult::Failed(f) => {
                assert_eq!(f.step, 2);
                assert_eq!(f.kind, "assert");
                assert_eq!(f.cond, "count=99");
            }
            RunResult::Passed => panic!("the mismatched assert should fail the run"),
        }
    }

    #[test]
    fn run_handles_wait_until_immediately_in_process() {
        // In-process the model is already at its value, so a wait-until with a
        // tight timeout still passes on the first poll — no async gap to wait on.
        let mut ui = Harness::new(Counter::new());
        let outcome = ui
            .run("press Up\nwait-until count=1 --timeout-ms 50 --interval-ms 5")
            .unwrap();
        assert_eq!(outcome, RunResult::Passed);
    }

    #[test]
    fn run_surfaces_a_script_parse_error() {
        let mut ui = Harness::new(Counter::new());
        assert!(ui.run("frobnicate nope").is_err());
    }

    #[test]
    fn into_model_returns_the_final_model() {
        let mut ui = Harness::new(Counter::new());
        ui.press("Up Up").unwrap();
        let model = ui.into_model();
        assert_eq!(model.count, 2);
    }

    #[test]
    fn apply_text_default_matches_per_char_keys() {
        // The default apply_text must be equivalent to pressing each char.
        let mut typed = Harness::new(Counter::new());
        typed.type_text("abc");
        let mut pressed = Harness::new(Counter::new());
        pressed.press("a b c").unwrap();
        assert_eq!(typed.state(), pressed.state());
    }

    #[test]
    fn model_borrows_fields_the_seam_also_exposes() {
        let mut ui = Harness::new(Counter::new());
        ui.press("Up Up Up").unwrap();
        // model() reaches the live model; here it agrees with the seam.
        assert_eq!(ui.model().count, 3);
        assert_eq!(ui.model().last, "up");
    }

    #[test]
    fn run_executes_a_capture_step_in_process() {
        // In-process there is no screen, so capture is a no-op that yields the
        // empty string; the step must still run and the script must still pass.
        let mut ui = Harness::new(Counter::new());
        let outcome = ui.run("press Up\ncapture\nassert count=1").unwrap();
        assert_eq!(outcome, RunResult::Passed);
    }

    #[test]
    fn in_process_backend_applies_keys_and_captures_nothing() {
        // The adapter itself: send_keys reaches the model, and capture yields
        // exactly the empty string (there is no rendered screen in-process).
        let model = RefCell::new(Counter::new());
        let backend = InProcessBackend { model: &model };
        backend.send_keys(&[Key::Up, Key::Up]).unwrap();
        assert_eq!(model.borrow().count, 2);
        assert_eq!(backend.capture().unwrap(), "");
    }

    /// A model whose `apply_text` override is distinguishable from per-key
    /// typing: a pasted block lands whole in `pasted`, while per-char keystrokes
    /// accumulate in `keyed`. Lets a test tell which transport a step used.
    #[derive(Serialize)]
    struct PasteAware {
        keyed: String,
        pasted: String,
    }

    impl DumpState for PasteAware {
        fn dump_state(&self) -> Value {
            dump_serialize(self)
        }
    }

    impl InProcessUi for PasteAware {
        fn apply_key(&mut self, key: &Key) {
            if let Key::Char(c) = key {
                self.keyed.push(*c);
            }
        }
        fn apply_text(&mut self, text: &str) {
            self.pasted.push_str(text);
        }
    }

    #[test]
    fn paste_routes_through_apply_text_while_plain_type_goes_per_key() {
        // A scripted `type --paste` must reach the apply_text override (block
        // transport); a plain `type` must go per-key through apply_key. This is
        // the fidelity contract that mirrors the real tmux/PTY backends.
        let mut ui = Harness::new(PasteAware {
            keyed: String::new(),
            pasted: String::new(),
        });
        ui.run("type ab\ntype --paste cd").unwrap();
        assert_eq!(ui.state()["keyed"], serde_json::json!("ab"));
        assert_eq!(ui.state()["pasted"], serde_json::json!("cd"));

        // type_text() uses the same block transport as --paste.
        let mut direct = Harness::new(PasteAware {
            keyed: String::new(),
            pasted: String::new(),
        });
        direct.type_text("ef");
        assert_eq!(direct.state()["pasted"], serde_json::json!("ef"));
        assert_eq!(direct.state()["keyed"], serde_json::json!(""));
    }
}
