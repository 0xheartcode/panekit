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

## Future

- `ZellijBackend` (`zellij action write` / `dump-screen`).
- Optional in-process test harness for Rust UIs (inject events into the real
  model+view) as a unit-test complement to out-of-process driving.

## Shipped since v0.1

- `PtyBackend` (behind the `pty` feature): spawns the TUI in a pseudo-terminal
  and parses its screen with `vt100`. Unlike the tmux backend it *owns* the
  child, so it is a library API (spawn/drive/assert/kill) rather than a one-shot
  CLI target. It is the CI shape, no multiplexer required.
- `panedrive watch`: records the state timeline over a window (JSONL), catching
  transitions a single assert would miss. Addresses the sampling half of the
  frame-boundary limitation; states the UI never emits remain out of reach (an
  app-side write-on-change concern).
