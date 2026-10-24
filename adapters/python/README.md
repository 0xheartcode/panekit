# Python seam adapter

The panekit state seam is just an atomically written JSON file, so any language
can expose it. This is the Python reference adapter, the counterpart of the Rust
[`paneview`](../../paneview) crate (and a sibling of the
[Node adapter](../node)).

## Use it in your app

Copy `panedrive_seam.py` into your project (standard library only) and write a
snapshot whenever your UI state changes:

```python
from panedrive_seam import write_snapshot_from_env, SnapshotWriter

# One-liner: write to $PANEDRIVE_STATE when a driver is present, else a no-op.
write_snapshot_from_env({"focus": "fleet", "bag": {"count": 2}})

# Or a stateful writer that skips unchanged writes and stamps _meta.seq
# (so `panedrive run --ack` can wait for the app to acknowledge input):
writer = SnapshotWriter.from_env()   # None when not under a driver
if writer is not None:
    writer.write({"focus": "fleet", "bag": {"count": 2}})
```

Keep the snapshot **machine-first**: flat, stable keys and enums as strings, not
a mirror of the on-screen layout. The `_meta` key is reserved for the writer's
frame counter. See [`../../docs/SEAM.md`](../../docs/SEAM.md) for the full
contract. `SnapshotWriter` also takes `compact=True` for single-line JSON.

## Try the example

```sh
panedrive run adapters/python/counter.pds --backend pty \
  --state /tmp/py-counter.json \
  -- python3 "$(pwd)/adapters/python/example_counter.py" /tmp/py-counter.json
```

The script and state paths are absolute on purpose: the pty backend spawns
`python3` with its own working directory, so a relative script path may not
resolve.

`example_counter.py` is a tiny line-driven "TUI" instrumented with the adapter;
`counter.pds` drives it with the *same* script and conditions as the Rust
`counter_tui` and Node examples, which is the point: the seam is
language-agnostic.

Confirm the seam file the app wrote:

```sh
panedrive state --state /tmp/py-counter.json --paths
panedrive validate-seam /tmp/py-counter.json
```
