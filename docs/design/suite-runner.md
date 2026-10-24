# Suite runner: matrix and parameter sweep

Status: accepted (panedrive 0.1.7)

Builds on `outcome-model.md`, which reserved an aggregate `matrix` outcome and
pinned the single-run envelope as self-contained precisely so this runner could
land without reshaping anything a single-run consumer reads.

## Context

Running one script once answers "does this flow pass on this configuration?".
Teams instead ask "does it pass across every terminal size?" and "does it pass
for seeds 1..100?", and today they hand-roll shell loops around `panedrive run`
and parse the output themselves. The suite runner makes that a first-class
feature: expand one script across a set of configurations, run each, and report
all of them in a form a machine can read line by line.

## The cell model

A run expands into **cells**. Each cell is a binding of axis names to values,
and the cartesian product of all axes is the set of cells. A single-configuration
run (today's behaviour) is the degenerate one-cell case and is unchanged: no
axes means one cell, emitted exactly as before.

Axes come from two sources, which compose into one product:

- **Matrix, from script headers.** Leading `#` comment lines in the `.pds`
  script declare axes, so a suite travels with its script:
  - `# size: 24x80, 40x120` gives a `size` axis of two terminal dimensions.
  - `# env: THEME=light, THEME=dark` gives an `env` axis of child environment
    bindings.
  (Header comments are parsed from the raw script text, since the step parser
  drops `#` lines. Only these two keys are recognised in 0.1.7.)
- **Sweep, from the CLI.** `--sweep VAR=1..8` (an inclusive integer range) or
  `--sweep VAR=a,b,c` (an explicit list) adds a `VAR` axis whose value is passed
  to the child as the environment variable `VAR`.

## Per-cell execution

Each cell runs the whole existing `run_script` engine, unchanged, against a
**fresh backend spawned for that cell**:

- **size axis** sets the spawned PTY's rows and cols (overriding `--rows`/
  `--cols`) for that cell.
- **env axis and sweep var** are set on the **child process's** environment (via
  the PTY `CommandBuilder`), never on panedrive's own process environment. This
  is the load-bearing decision for parallelism: `std::env::set_var` is
  process-global and not thread-safe, so we never touch it; a child's env is
  private to that child and safe to set from many threads at once.
- **seam isolation**: each cell gets a unique state path (the `--state` base
  with the cell id appended, or a fresh temp path), and the child's
  `PANEDRIVE_STATE` is pointed at it, so parallel cells never read, write, or
  clear each other's snapshot. The pre-spawn stale-seam clear stays per cell.
- **retries nest inside a cell**: `--retry-timeouts` re-runs a single cell on a
  timeout, exactly as today; it does not re-run the suite.

Because `run_script` is already backend/opts/sink-parameterised and holds no
global state, a cell is just one more call to it with that cell's backend, a
cloned `RunOptions`, and a per-cell sink.

## Parallelism

`--jobs N` (default 1) runs up to N cells concurrently on plain `std::thread`
workers pulling from a shared queue (no new dependency). Thread-safety rests on
three rules, all already true of the per-cell design:

1. No shared mutable process state: env goes to the child, not to us.
2. Each cell owns its backend, child, and seam path.
3. Each cell buffers its own events and captured screens in its own sink; the
   single NDJSON writer is behind a mutex and emits one complete line per
   finished cell, so output never interleaves.

## Output

A multi-cell run under `--json` emits **NDJSON**: one self-contained outcome
envelope per cell, on its own line, each carrying an added `id` object (its axis
bindings, e.g. `{"size":"40x120","SEED":"3"}`). The envelope is otherwise
byte-identical to the single-run `--json` summary (`schema`, `ok`, `outcome`,
`steps`, and the failure/artifact keys), so an existing consumer parses each line
with the code it already has. A line is written the moment its cell finishes,
which is what makes `--jobs` observable in real time.

This is a deliberate, documented departure from the reserved aggregate `matrix`
object in `outcome-model.md`: NDJSON streams (an aggregate object must buffer
every cell before it can be emitted) and it is exactly the "one JSON line per
run" shape teams were hand-rolling. The aggregate object remains a valid
consumer-side fold of the lines, so nothing in the reserved model is
contradicted; we simply ship the streamable form. Without `--json`, a human
summary goes to stderr (one line per cell plus a pass/fail tally). The process
exit code is `0` only if every cell passed, `1` if any cell failed, matching the
single-run contract.

## Constraints and gates

- **pty only.** Matrix and sweep need to own the child and set its environment
  and size, which only the PTY backend does. `--sweep`, a `# size:`/`# env:`
  header, and `--jobs > 1` are usage errors on the attach backends.
- **single-destination flags.** `--cast` and `--events` write one file and
  cannot represent many cells, so they are rejected alongside a multi-cell run
  (as `--retry-timeouts` already rejects `--cast`).
- **`--jobs` requires a multi-cell run** (an axis or a sweep); `--jobs` with a
  single cell is a usage error, not a silent no-op.

## Deferred (not in 0.1.7)

- Matrix/sweep on the attach backends (they own no child to configure).
- Templating an explicit seam path given in the program's own argv; the env
  convention (`PANEDRIVE_STATE`) is the supported path for per-cell isolation.
- An aggregate `matrix` object output mode; NDJSON is the shipped form and folds
  into the aggregate on the consumer side if wanted.
