# The run outcome model

Status: accepted, implemented in panedrive 0.1.5.

This note pins the result contract of `panedrive run` so it is reshaped once and
then only extended. Every later feature that reports a result (retry now, a
matrix or sweep runner later) produces or aggregates the shape defined here.

## Why

Through 0.1.4 the `--json` result grew one key at a time (`ok`, then
`failed_step`, `failure`, `steps`). That is fine until two features need to agree
on what an outcome *is*: retry has to say "this failed but we tried again," and a
future matrix runner has to carry many results at once. Deciding the closed set
of outcomes now, and a single envelope that carries any one of them, means those
features add data without renaming or repurposing a field a 0.1.4 consumer
already reads.

## The closed set of outcomes

A single run ends in exactly one outcome:

- `passed` : every step held, and the owned program (if any) did not exit in a
  way that invalidates the run.
- `failed` : one step did not hold, or the owned program exited in a way that
  invalidates the run. The `failure.kind` says which: `assert`, `wait-until`,
  `expect-screen`, `expect-no-screen`, `child-exit`, or `fail-on`.
- `retried` : the run was attempted more than once (see `--retry-timeouts`). The
  envelope still reports the terminal truth through `ok`, plus how many attempts
  it took; the terminal attempt was itself a `passed` or `failed` run.

Exit codes are unchanged from 0.1.4: `0` when the run passed (including a retry
that ended in a pass), `1` when it failed (including a retry that ended in a
fail), `2` for a usage or backend error.

## The JSON envelope

The envelope is an additive superset of the 0.1.4 shape. Every key 0.1.4 emitted
keeps its name and meaning; 0.1.5 only adds keys, so an existing consumer that
reads `ok`, `failed_step`, `failure`, or `steps` keeps working unchanged.

A passed run:

```json
{ "schema": 1, "ok": true, "outcome": "passed", "steps": [ ... ] }
```

A failed run:

```json
{ "schema": 1, "ok": false, "outcome": "failed",
  "failed_step": 2, "failure": { "step": 2, "kind": "child-exit", ... },
  "steps": [ ... ] }
```

A retried run carries `attempts` and keeps `ok` as the terminal truth:

```json
{ "schema": 1, "ok": true, "outcome": "retried", "attempts": 2, "steps": [ ... ] }
```

New keys:

- `schema` : an integer the consumer can branch on. `1` is this contract. It is
  bumped only on a breaking change to the envelope, which this model exists to
  avoid.
- `outcome` : one of the closed set above. Redundant with `ok` plus
  `failure.kind` on purpose, so a consumer can switch on one field.
- `attempts` : present only when `outcome` is `retried`; the number of attempts
  made, terminal attempt included.
- `artifacts` : present only when `--artifacts` was set; see below.

`failure` is unchanged from 0.1.4: `step`, `kind`, `cond`, `path`, and the
reason-specific keys (`actual`, `timeout_ms`, `pattern`, `negate`, `exit_code`,
`early`, `error`). No key there is renamed or removed.

## Artifacts pointer

When `run --artifacts <dir>` is set, a failed run writes a small bundle to `dir`
and the envelope points at it:

```json
"artifacts": {
  "dir": "<dir>",
  "manifest": "<dir>/manifest.json",
  "screen": "<dir>/screen.txt",
  "seam": "<dir>/seam.json",
  "events": "<dir>/events.jsonl"
}
```

A key is present only for a file that was actually written (a run with no seam
has no `seam`). The bundle is bounded and never records secrets, following the
same rule as `--events`: a `type --from-env` value is not recorded.

## Child-exit, refined

A program the pty backend owns may exit during the run. The rule, refined in
0.1.5 so a clean quit is not a false failure:

- A non-zero exit at any point fails the run (`failure.kind` = `child-exit`,
  `early` = false). A crash is always a failure.
- A clean exit (code 0) fails the run only if a later step would still send input
  to the program (`press` or `type`). Exiting while input remains is a real
  problem (`early` = true). Exiting when only read-only steps remain
  (`assert`, `wait-until`, `capture`, `expect-screen`, `expect-no-screen`,
  `sleep`) is a clean quit: those steps read the last snapshot or screen and the
  run can still pass.

This is why a script that quits the app on its last input step, then asserts the
final saved state, passes rather than reporting "exited before the script
finished."

## Designed for aggregation (matrix and sweep, later)

A matrix or parameter sweep runs one script many times and must report all of
them. The single-run envelope above is self-contained, so a future aggregate
wraps a list of them without reshaping any of them:

```json
{ "schema": 1, "outcome": "matrix", "ok": false,
  "cells": [
    { "id": { "size": "80x24" }, "result": { "schema": 1, "ok": true, "outcome": "passed", ... } },
    { "id": { "size": "120x40" }, "result": { "schema": 1, "ok": false, "outcome": "failed", ... } }
  ] }
```

`matrix` is reserved here, not implemented in 0.1.5. Recording it now is the
point of the model: when the matrix runner lands, it adds a cell wrapper and a
new `outcome` value, and changes nothing a single-run consumer reads.

## Explicitly out of 0.1.5

- Matrix from script headers and parameter sweep: deferred, accommodated above.
- Regex and golden snapshots for the screen assertions: deferred; the 0.1.4
  literal-substring match stands.
