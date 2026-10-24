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

## Emit on every render

An app that redraws in a loop wants a writer it can call each frame without
hand-syncing a path or churning the file. `SnapshotWriter` is that writer:

```rust
// Set up once. `from_env` targets $PANEDRIVE_STATE (the path panedrive reads),
// and is `None` when the app is not under a driver, so production pays nothing.
let mut seam = paneview::SnapshotWriter::from_env();

// Each render:
if let Some(w) = seam.as_mut() {
    w.write(&app)?; // skipped when unchanged; stamps _meta.seq on a real change
}
```

It does two things across its lifetime: it **skips the write when the state is
unchanged**, so a redraw with no state change does not rewrite the file, and it
stamps a monotonic frame counter at **`_meta.seq`** (first write is `1`) that
advances only on a real change. A driver reads `_meta.seq` to tell a fresh frame
from a stale file and to confirm a keypress actually produced a new frame.
`without_meta()` turns the stamp off; `seq()` reads the counter.

For a one-off (no counter, no skip), `paneview::write_snapshot_from_env(&app)`
writes to `$PANEDRIVE_STATE` when set and is a no-op otherwise.
`paneview::STATE_ENV` names the variable.

Add `.compact()` (e.g. `SnapshotWriter::new(path).compact()`) to write compact
single-line JSON instead of the indented default: fewer bytes per frame, same
seam content.

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

To assert on just the fields a test cares about, `paneview::testing::assert_snapshot_contains(&actual, &expected)`
is a partial match: it ignores extra keys, extra trailing array elements, and the
`_meta` stamp, and panics with the dot-path of the first field that differs.

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
