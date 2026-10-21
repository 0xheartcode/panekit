# ADR-001: Two crates (a state seam and a driver)

Status: accepted (v0.1, 2026-08-26)

## Context

We repeatedly build terminal UIs (operator consoles, dashboards, and the like)
that we want an agent (or a plain integration test) to operate and verify: press
keys like a user, then confirm the UI reacted correctly.

Two forcing questions shaped the design:

1. **How do we read the UI's state?** Screen-scraping (`capture-pane` + regex)
   is brittle: it breaks on layout changes, terminal width, and colour codes,
   and it races a mid-render frame.
2. **How do we send input?** A per-project "headless action command"
   (`app do-thing --wait`) tends to call the model layer directly, bypassing the
   real render, so it can pass while the actual screen is broken, and it costs a
   bespoke command grammar in every project.

## Decision

Split the concern into **two crates with opposite linkage**, and bridge them
with a JSON **state seam**:

- **`paneview`** (library, linked *into* each UI): the UI implements one trait,
  `DumpState`, returning its state as JSON. Tiny dependency footprint
  (`serde` + `serde_json`) so any UI can adopt it. The state *shape* is
  inherently per-project, so it lives with the project; only the *contract* is
  shared.
- **`panedrive`** (library + binary, runs *outside* the UI): sends input through
  the real keybindings via a `PaneBackend` (tmux or a PTY), and reads the UI's
  state from the `paneview` JSON rather than the screen. Waiting and asserting
  are expressed as conditions over that JSON.

The dependency arrow is one-way: `panedrive → paneview`. A UI that only wants to
expose state never compiles the driver's tmux/clap machinery. The `make deps`
wall enforces this.

## Why not one crate

Cargo shares `[dependencies]` across a crate's lib and bin targets. Fusing the
seam and the driver would force every UI that links the seam to also pull the
driver's dependencies (tmux orchestration, `clap`), gated behind feature flags.
Two crates keep the seam lean at zero real cost.

## Why not a per-project action CLI

The value proposition is *"the agent sees what a user sees."* That requires
exercising the real render and real keybindings. A per-project action command
bypasses the render, drifts from the interactive path, and multiplies
maintenance across projects. tmux input + a JSON seam gets fidelity *and*
determinism with one shared driver.

## Consequences

- Adopting panekit in a UI is: add `paneview`, implement `DumpState`, emit a
  snapshot (a `--dump-state` flag, a file each render, or on a signal).
- Adding a new terminal host (such as zellij) is one `PaneBackend` impl; the
  condition/wait/assert layer is untouched.
- The PTY backend drives UIs in CI with no multiplexer attached, and the same
  seam and conditions apply, because reading state never depended on the host.
- The state seam is the load-bearing, non-portable half; keep it machine-first
  (stable keys, enums as strings) rather than a mirror of the screen.

## Shipped since v0.1

- `PtyBackend` (behind the `pty` feature): spawns the TUI in a pseudo-terminal
  and parses its screen with `vt100`. Unlike the tmux backend it *owns* the
  child, so it is a library API (spawn/drive/assert/kill) rather than a one-shot
  CLI target. It is the CI shape, no multiplexer required.
- `panedrive watch`: records the state timeline over a window (JSONL), catching
  transitions a single assert would miss. Addresses the sampling half of the
  frame-boundary limitation; states the UI never emits remain out of reach (an
  app-side write-on-change concern).
- `ZellijBackend` (`zellij action write-chars` / `write`, `dump-screen`): a
  `PaneBackend` that attaches to a running zellij session; the `--pane` value is
  the session name. One more host behind the same condition/wait/assert layer.
- `panedrive::harness`: an in-process test harness for Rust UIs. The UI's model
  implements `InProcessUi: DumpState` (an `apply_key`, plus a provided
  `apply_text`), and `Harness<M>` feeds it keys and asserts over the *same*
  `paneview` JSON seam and the *same* condition grammar as the out-of-process
  driver, in one process with no terminal, no timing, and no cleanup. It reuses
  the whole `run_script` runner through an in-process backend, so a `.pds` script
  runs identically here; settling is off (updates are synchronous) and `capture`
  yields the empty string (no rendered screen). It is the unit-test complement to
  driving: same seam and conditions at `cargo test` speed, with the tmux/PTY
  backends reserved for the full-fidelity integration path.
- `expect-screen` / `expect-no-screen` script steps: a point-in-time literal
  substring check over the *rendered* visible text, not the JSON seam. The seam
  stays the primary output contract; these catch layout or clipping bugs a
  correct seam cannot see (a key legend that never displays, a stack trace on
  screen).
- Crash detection on the PTY backend: because that backend *owns* the child,
  `run` fails by default when the program exits unexpectedly, so a crash after
  the last assertion is not a false pass. A non-zero exit always fails. A clean
  exit (code 0) fails only when a later step would still send input to the
  program: a clean quit followed only by read-only steps (an `assert` or
  `wait-until` reading the last snapshot) is not a failure. `--allow-exit` opts
  out, and `--fail-on <substring>` fails the run on a literal substring in the
  program's output stream. The backend also clears any leftover seam file before
  spawning, so a stale snapshot from a previous run cannot pass a `wait-until`
  before the fresh program has written. The attach backends own no child, so
  none of this applies there.
- The run outcome model (`docs/design/outcome-model.md`): `run --json` emits one
  envelope carrying a `schema` version, an `outcome` from a closed set
  (`passed`, `failed`, `retried`), and the 0.1.4 keys unchanged. It is designed
  so retry (now) and a future matrix or sweep runner extend it without a second
  reshape. `--timeout-scale` multiplies every wait for a slow box;
  `--retry-timeouts N` re-runs the whole script on a `wait-until` timeout only
  (never an assertion or a crash); `--artifacts <dir>` writes a failure bundle
  (screen, last seam, events, manifest) the summary points at.
- The seam contract check lives in `paneview` (`paneview::check` / `leaves`), the
  crate that defines the seam, so the rule has one owner. `panedrive`'s
  `validate-seam` and `state --paths` reuse it rather than re-implementing it.
