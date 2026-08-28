//! Live check for the zellij backend. A fully attached roundtrip (drive a pane,
//! read it back) needs a zellij session hosted in a real terminal with a known
//! layout, which is too environment-specific to assert reliably in CI. So the
//! deterministic encoding (which bytes/chars each key becomes, how the argv is
//! built) is covered by unit tests in `key.rs` and `backend::zellij`; this test
//! confirms the backend actually reaches a real `zellij` and surfaces a failure
//! cleanly when the target session does not exist. Skips when zellij is absent.

use panedrive::{PaneBackend, ZellijBackend, parse_keys};
use std::process::Command;

fn zellij_available() -> bool {
    Command::new("zellij")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn driving_a_missing_session_is_a_clean_error() {
    if !zellij_available() {
        eprintln!("skipping zellij_roundtrip: zellij not installed");
        return;
    }

    // A session name that cannot exist for this process.
    let session = format!("panedrive-nosuch-{}", std::process::id());
    let backend = ZellijBackend::new(session);

    let err = backend.send_keys(&parse_keys("Enter").unwrap());
    assert!(
        err.is_err(),
        "sending to a non-existent zellij session should error, not silently pass"
    );
}
