# paneview

The **state seam** a terminal UI exposes so agents and tests can read its state
as JSON instead of scraping the rendered screen.

Implement one trait on your app model:

```rust
use paneview::{dump_serialize, DumpState};

impl DumpState for App {
    fn dump_state(&self) -> serde_json::Value {
        dump_serialize(self) // or hand-build a machine-first json!({ ... })
    }
}
```

Then wire it to a `--dump-state` flag (`paneview::print_snapshot`) or a snapshot
file (`paneview::write_snapshot`, atomic). The companion
[`panedrive`](https://github.com/0xheartcode/panekit) crate drives your UI and
asserts against exactly this JSON.

Part of [**panekit**](https://github.com/0xheartcode/panekit). Near-zero
dependencies (`serde` + `serde_json`) by design, so it is safe to link into any
UI.
