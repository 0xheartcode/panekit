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

## Check your own snapshot

You do not need the driver to test that your snapshot is well formed.
`paneview::check` validates a value against the seam contract, so you can assert
on it in a plain unit test:

```rust
use paneview::{check, DumpState};

let app = App { /* ... */ };
assert!(check(&app.dump_state()).is_ok(), "snapshot must satisfy the seam contract");
```

`paneview::leaves` lists every assertable dot-path (what `panedrive state
--paths` prints), and `try_dump_serialize` returns a serialization error as a
`Result` instead of the `{"_error": "..."}` marker that `dump_serialize` writes
on failure.

## Seam design: a few rules that keep it useful

- **Machine-first, not a screen mirror.** Emit the state a test asserts on
  (`screen`, `count`, `selected`), not the pixels. If you rename a label on
  screen, the seam should not change.
- **Keep keys stable.** A driver script addresses `bag.count` by name; renaming
  a key breaks every script that reads it, so treat keys as an API.
- **Scalars where you assert.** Conditions compare scalar leaves (string,
  number, bool). Nesting and arrays are fine as containers (address an element
  by index, `rows.2.status`), but the leaf you compare should be a scalar.
- **A count beside a list.** When you emit a list, emit its length too
  (`rows` plus `rows_count`), so a script can assert "three rows" without
  walking the array.
- **Enums as lowercase strings.** `"screen": "list"` reads and asserts more
  cleanly than a number, and survives reordering of the enum.
- **No secrets in the seam.** The snapshot is written to disk in plain text and
  read by anything that can see the file. Keep tokens, passwords, and personal
  data out of it.

## Snapshot lifecycle

`write_snapshot` writes to a unique temp file and renames it into place, so a
reader never sees a half-written file. The newest snapshot stays on disk after
your program exits; nothing in `paneview` deletes it. If you want a driver to
assert on your final state, emit one last snapshot just before exiting. On the
driver side, `panedrive` clears a stale seam file before it spawns a program it
owns, so a previous run's snapshot cannot be mistaken for the new run's.

Part of [**panekit**](https://github.com/0xheartcode/panekit). Near-zero
dependencies (`serde` + `serde_json`) by design, so it is safe to link into any
UI.
