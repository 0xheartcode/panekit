"""A language-agnostic panekit state seam for Python apps, the counterpart of
paneview's `write_snapshot` / `SnapshotWriter`. Your app writes its state as a
snapshot on every change; `panedrive` reads that JSON and asserts against it.

No dependency on panedrive itself: the seam is just an atomically written JSON
file. See ../../docs/SEAM.md for the contract other languages follow.
"""

import json
import os

# The environment variable that names the seam snapshot path. `panedrive` reads
# it as the default for `--state`, so writing here lands where the driver looks.
STATE_ENV = "PANEDRIVE_STATE"

_TMP_SEQ = 0


def _write_atomic(payload, path, compact):
    """Serialize `payload` to `path` atomically: a unique sibling temp file, then
    an os.replace (atomic on the same filesystem), so a reader never sees a
    half-written snapshot."""
    global _TMP_SEQ
    _TMP_SEQ += 1
    tmp = "{}.{}.{}.tmp".format(path, os.getpid(), _TMP_SEQ)
    separators = (",", ":") if compact else None
    indent = None if compact else 2
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=indent, separators=separators)
    os.replace(tmp, path)


def write_snapshot(state, path, compact=False):
    """Write `state` (any JSON-serializable value) to `path` atomically. The
    one-liner form, with no frame counter and no skip-unchanged."""
    _write_atomic(state, path, compact)


def write_snapshot_from_env(state, compact=False):
    """Write to the path named by STATE_ENV when it is set; otherwise a no-op.
    Returns True when a snapshot was written, False when the variable is unset,
    so an app not under a driver pays almost nothing."""
    path = os.environ.get(STATE_ENV)
    if not path:
        return False
    write_snapshot(state, path, compact)
    return True


class SnapshotWriter:
    """A stateful writer for an app that snapshots every render, mirroring
    paneview's `SnapshotWriter`: it skips a write when the state is unchanged and
    stamps a monotonic frame counter at `_meta.seq` (first write is 1), which a
    driver reads to tell a fresh frame from a stale file and to confirm a
    keypress produced a new frame."""

    def __init__(self, path, meta=True, compact=False):
        self.path = path
        self.seq = 0
        self._last = None
        self._meta = meta
        self._compact = compact

    @classmethod
    def from_env(cls, meta=True, compact=False):
        """A writer targeting STATE_ENV, or None when it is unset or empty."""
        path = os.environ.get(STATE_ENV)
        if not path:
            return None
        return cls(path, meta=meta, compact=compact)

    def write(self, state):
        """Write `state` if it changed since the last write. Returns True when a
        snapshot was written (state differed, seq advanced), False when skipped
        as unchanged. Change is judged before the `_meta` stamp, so the
        ever-advancing counter never counts as a change by itself."""
        if state == self._last:
            return False
        self.seq += 1
        payload = state
        # `_meta` is reserved for this stamp; only an object root can carry it.
        if self._meta and isinstance(state, dict):
            payload = dict(state)
            payload["_meta"] = {"seq": self.seq}
        _write_atomic(payload, self.path, self._compact)
        # Store a copy so a later in-place mutation by the caller is not aliased.
        self._last = dict(state) if isinstance(state, dict) else state
        return True
