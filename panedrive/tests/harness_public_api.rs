//! Exercises the in-process [`Harness`] the way a downstream crate would: only
//! through `panedrive`'s public re-exports, from outside the crate. This proves
//! the API surface (`Harness`, `InProcessUi`, `Key`, `RunResult`) is actually
//! usable as exported, which a `src/`-internal unit test cannot guarantee.

use panedrive::{Harness, InProcessUi, Key, RunResult};
use paneview::{DumpState, dump_serialize};
use serde::Serialize;
use serde_json::{Value, json};

/// A minimal two-field model: a selection index that wraps, and a submitted
/// flag, driven entirely by arrow keys and Enter.
#[derive(Serialize)]
struct Menu {
    selected: usize,
    len: usize,
    submitted: bool,
}

impl Menu {
    fn new(len: usize) -> Self {
        Self {
            selected: 0,
            len,
            submitted: false,
        }
    }
}

impl DumpState for Menu {
    fn dump_state(&self) -> Value {
        dump_serialize(self)
    }
}

impl InProcessUi for Menu {
    fn apply_key(&mut self, key: &Key) {
        match key {
            Key::Down => self.selected = (self.selected + 1) % self.len,
            Key::Up => self.selected = (self.selected + self.len - 1) % self.len,
            Key::Enter => self.submitted = true,
            _ => {}
        }
    }
}

#[test]
fn drives_a_menu_through_the_public_harness_api() {
    let mut ui = Harness::new(Menu::new(3));

    // Fluent press chain, then a direct assert.
    ui.press("Down").unwrap().press("Down").unwrap();
    ui.assert("selected=2").unwrap();

    // Wrap-around exercised via a whole script, verdict is RunResult::Passed.
    let outcome = ui
        .run("press Down\nassert selected=0\npress Enter\nassert submitted=true")
        .unwrap();
    assert_eq!(outcome, RunResult::Passed);

    // The seam and the model agree, and into_model hands the model back.
    assert_eq!(
        ui.state(),
        json!({ "selected": 0, "len": 3, "submitted": true })
    );
    let model = ui.into_model();
    assert!(model.submitted);
}

#[test]
fn a_failing_script_reports_the_offending_step() {
    let mut ui = Harness::new(Menu::new(2));
    let outcome = ui.run("press Down\nassert selected=99").unwrap();
    match outcome {
        RunResult::Failed(f) => {
            assert_eq!(f.step, 2);
            assert_eq!(f.cond, "selected=99");
        }
        RunResult::Passed => panic!("a wrong assertion must fail the run"),
    }
}
