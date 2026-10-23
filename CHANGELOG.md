# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The two crates (`paneview` and `panedrive`) are versioned and released independently; from 0.1.3 (paneview) and 0.1.4 (panedrive) onward each section names the crate it covers. Earlier sections predate the split, when both crates shared one version.

Entries are kept on a single line each: cargo-dist injects this file into the GitHub Release body, where GitHub renders every newline as a hard break, so wrapped lines would show as ragged mid-sentence breaks.

## [panedrive 0.1.6] - 2026-10-23

Ships `panedrive` only. Theme: CI robustness and honest input timing. Additive and backward compatible. Reuses `paneview` 0.1.4 for the `_meta.seq` frame counter.

### Added

- **`run --ack`**: after each key/`type` step, wait up to ~1s for the app to acknowledge the input by advancing `_meta.seq` (the `paneview` `SnapshotWriter` frame counter), a precise alternative to `--settle`'s whole-seam change heuristic. When the seam carries no `_meta.seq` (an app that does not stamp it, or `--from-capture`) it falls back to the same change detection as `--settle`, so it never hangs. Not combined with `--settle`.
- **`run -e <step>` (inline script)**: pass script steps directly on the command line, repeatable, instead of a script file, e.g. `run -e "press Enter" -e "assert ready=true"`. Each `-e` is one line fed to the same parser. A script path and `-e` are mutually exclusive, and one of the two is required.

### Changed

- **Post-exit screen capture (pty backend)**: a `capture` or `expect-screen` after a clean exit now returns the app's last rendered frame instead of the blank primary screen a quitting TUI restores. The backend retains the last non-blank frame and serves it once the child has exited; while the child runs, `capture` is unchanged. This is the screen-side complement to the 0.1.5 read-only-after-exit rule.
- **Relocatable failure bundle**: the `--artifacts` `manifest.json` now references its members (`screen.txt`, `seam.json`, `events.jsonl`) by bare basename instead of the absolute path they were written at, so the bundle can be uploaded as a CI artifact and inspected after it is downloaded on another machine. The run summary's `artifacts` object keeps full paths for the operator on the machine that ran.
- panedrive now depends on paneview 0.1.4.

## [paneview 0.1.4] - 2026-10-23

Ships `paneview` only. `panedrive` is unchanged. Theme: ergonomics for an app that emits a snapshot every render. All additive and backward compatible; the existing `write_snapshot` and `dump_serialize` behave exactly as before.

### Added

- **`SnapshotWriter`**: a stateful writer for a render loop. It stamps each snapshot it writes with a monotonic frame counter at `_meta.seq` (the first write is `1`), so a driver can tell a fresh frame from a stale file and confirm a keypress produced a new frame, and it skips the write entirely when the state is unchanged from the last one it wrote, so a redraw without a state change does not churn the file (`seq` advances only on a real change). `without_meta` turns the stamp off; `seq()` reads the counter.
- **`paneview::write_snapshot_from_env`** and **`SnapshotWriter::from_env`**: write to (or target) the path named by `PANEDRIVE_STATE`, the same variable `panedrive` reads, so an app lands its snapshot where the driver looks with no hand-synced path. The free function is a one-liner that is a no-op when the variable is unset, so an app not under a driver pays almost nothing; `from_env` returns `Option<SnapshotWriter>` for the counter-and-skip path. `paneview::STATE_ENV` names the variable.
- **`paneview::testing::assert_snapshot_contains`**: a partial-match assertion for an app's own unit tests. It checks that a snapshot contains the named fields (extra keys, extra trailing array elements, and the `_meta` stamp are ignored) and panics with the dot-path and both values on a mismatch, pointing the panic at the caller.

## [panedrive 0.1.5] - 2026-10-21

Ships `panedrive` only. Theme: a run result you can trust and inspect in CI. The `--json` summary is an additive superset of 0.1.4 (every existing key keeps its name and meaning), so a 0.1.4 consumer keeps working. Reuses `paneview` 0.1.3. See `docs/design/outcome-model.md`.

### Added

- **Result outcome model**: the `run --json` summary now carries `schema` (an integer the consumer can branch on, `1` today), `outcome` (`passed`, `failed`, or `retried`), and, when retried, `attempts`. Exit codes are unchanged (0 pass, 1 fail, 2 usage). The closed outcome set and the envelope are pinned in `docs/design/outcome-model.md`, which also designs it to carry a future matrix or sweep without another reshape.
- **`run --timeout-scale <factor>`**: multiply every `wait-until` timeout and the `--settle` wait by a factor, so one script runs on a slow or loaded box without editing each step. A non-positive or non-finite factor is a usage error.
- **`run --retry-timeouts <N>`** (pty backend): if the run fails because a `wait-until` timed out, re-run the whole script up to `N` more times. Only a timeout is retried; an assertion mismatch or a crash is deterministic and is never retried, so a real failure is never masked.
- **`run --artifacts <dir>`**: on a failed run, write a bundle (final screen, last seam snapshot, event track, and a `manifest.json` that references them and embeds the failure) so a failure can be inspected without a re-run. The `--json` summary points at it. Secret-safe: a `type --from-env` value is never recorded.

### Changed

- **Child-exit, refined**: a clean exit (code 0) no longer fails the run when only read-only steps remain after it. A quit-the-app flow (press the quit key, then `assert` or `wait-until` the final saved state) now passes, where 0.1.4 reported "exited before the script finished". A non-zero exit still always fails, and a clean exit before a later `press`/`type` still fails.
- The seam contract check now lives in `paneview` (reused via `paneview::check` / `leaves`); `validate-seam` and `state --paths` are unchanged in behaviour. panedrive now depends on paneview 0.1.3.

## [paneview 0.1.3] - 2026-10-21

Ships `paneview` only. `panedrive` is unchanged. Theme: make the seam contract one owner's job, and stop the writer from hiding failures. All additive and backward compatible.

### Added

- **`paneview::check`**: validate a value against the seam contract (`Result<(), Vec<Violation>>`), so an app can unit-test its own snapshot without pulling in the driver. `paneview::leaves` and `Leaf` (every assertable dot-path) move here too, so the contract has a single owner; `panedrive`'s `validate-seam` and `state --paths` reuse them unchanged.
- **`paneview::try_dump_serialize`**: derive a snapshot from a `Serialize` type and surface a serialization failure as an `Err`, for an app that wants to handle the failure itself.
- **Seam-design guide**: the `paneview` README now documents the conventions (machine-first keys, stable keys, scalar leaves, a count beside a list, enums as lowercase strings, no secrets) and the snapshot lifecycle.

### Changed

- **`dump_serialize` surfaces a failure instead of hiding it**: a serialization failure now yields a self-describing `{"_error": "..."}` marker object rather than `Value::Null`, so a broken snapshot no longer looks like a valid but empty state. The success path is unchanged.
- **Unique snapshot temp file**: `write_snapshot` writes its temp file under a per-process, per-call name (`<path>.<pid>.<n>.tmp`) before the atomic rename, so two writers aimed at one path (two instances, or parallel tests) can no longer clobber each other's temp file.
- **Arrays are documented as first-class**: `SEAM.md` and the validator docs state the rule plainly (the root must be an object, nesting and arrays are fine as containers, only asserted leaves must be scalar) with a regression test, so nobody re-derives "no arrays" from the scalar-leaf wording.

## [0.1.4] - 2026-10-20

Ships `panedrive` only. `paneview` is unchanged and stays at 0.1.2. Theme: stop a run from reporting a false pass.

### Added

- **Screen-text assertions**: `expect-screen <substring>` and `expect-no-screen <substring>` steps in the `run` grammar assert over the pane's *rendered* text, catching layout/clipping bugs (a legend that never shows, a stack trace on screen) that a correct state seam structurally cannot see. Literal substring match for now; regex and golden snapshots are deferred.
- **Crash detection**: a run now fails when the program the PTY backend owns exits unexpectedly (before the script ends, or with a non-zero status), so a crash *after* the last assertion is no longer reported as a pass. `run --allow-exit` opts out for a script that deliberately quits the program. The new `PaneBackend::child_exit` reports an owned child's status (attach backends own no child, so they are unaffected).
- **`run --fail-on <substring>`**: fail the run if the program's output ever contains the pattern (e.g. `panicked`), a continuous guard over the program's output stream (pty backend only). Backed by `PaneBackend::output_contains`.

### Changed

- **Behaviour (pty backend)**: by default, a program that exits unexpectedly now fails the run. A script that intends the program to exit must pass `--allow-exit`. A clean exit (code 0) caused by the final step is still a pass, so scripts that quit the app on their last step are unaffected.
- The PTY backend clears any leftover state-seam file before spawning the program, so a `wait-until` cannot pass immediately on a previous run's stale state before the freshly spawned program has written anything. Attach backends drive a process they do not own, so their seam file is left untouched.

## [0.1.3] - 2026-09-25

Ships `panedrive` only. `paneview` is unchanged and stays at 0.1.2.

### Added

- **In-process test harness** (`panedrive::harness`): `Harness` plus the `InProcessUi` trait (extends `paneview::DumpState`) unit-test a Rust TUI's model against the paneview seam without spawning a process. You feed keys with `press`/`type_text`, read `state()`, and `assert` over the same condition grammar as the driver; `Harness::run` executes a `.pds` script in-process by reusing the whole `run_script` runner behind an in-process backend. Settling is off (updates are synchronous) and `capture` yields the empty string. No new dependencies, no feature flag.

### Changed

- Docs: `docs/ARCHITECTURE.md` moves `ZellijBackend` and the in-process harness out of "Future" into "Shipped since v0.1" (both now ship), and the README documents the in-process harness.

## [0.1.2] - 2026-09-21

### Added

- **Rich assertion failures**: a failed `assert` (and a failed `run` step) now reports the value the seam actually held, e.g. `assert failed: count=2 (count was 1)`, instead of a Debug-printed condition, so you can see *why* without re-reading the state.
- **`--json` results**: `assert`, `wait-until`, and `run` take `--json` to emit a machine-readable result on stdout (exit codes unchanged), turning panedrive into a clean tool-call target for CI or an agent.
- **`run --events <path>`**: write a timestamped JSONL event track (one line per step, with the assertion outcome), for feeding CI/an agent or aligning a `--cast` recording. Secrets from `type --from-env` are never recorded.
- **`panedrive state [--paths]`**: pretty-print the seam, or list every assertable dot-path with its value and type, so you can see the surface a condition can target before writing one.
- **`panedrive validate-seam <file>`**: check a JSON file against the seam contract (object root, scalar leaves), human-readable or `--json`, for bringing up a non-Rust adapter.
- **`--cast <path>`**: `run` records an asciinema v2 cast of the session on the pty backend (native) and tmux (via `pipe-pane`); screen and zellij are rejected since they expose only snapshots, not a byte stream.
- **`panedrive record`**: spawn a program in a PTY, forward your keystrokes to it, and write what you pressed as a `.pds` script (Ctrl-] to stop); optional `--cast`. Requires the `pty` feature.
- **Node.js seam adapter** (`adapters/node`): a dependency-free `writeSnapshot`, a dogfooded example, and a driving script that reuses the Rust example's exact conditions, proving the seam is language-agnostic.
- **`PANEDRIVE_STATE`**: `--state` defaults from this environment variable across commands, so the app's snapshot writer and the driver can share one path instead of hand-syncing it.

### Changed

- **Library API (breaking, pre-publish)**: the script runner is one deep function again, `run_script(steps, backend, &RunOptions, &mut dyn RunSink)`, replacing the `run_script` / `run_script_settling` / `run_script_recording` trio (a 6-arg widest form behind two forwarders). `RunOptions` carries settling; the `RunSink` trait bundles the state probe, capture output, and per-step event channels, with `ClosureSink` / `probe_sink` for closure callers. Reshaped now so 0.1.2 ships the clean surface rather than needing a later minor bump.
- **`#![deny(missing_docs)]`** on both crates, with every public item documented, so the docs.rs pages are complete.
- CHANGELOG: keep each entry on one line so the generated release notes render as flowing paragraphs instead of hard-wrapped fragments.

### Fixed

- **`record` terminal safety**: raw mode is now entered and restored through an RAII guard, so a panic in the record loop can no longer leave your terminal stuck in raw mode.

## [0.1.1] - 2026-08-28

### Added

- **zellij backend** (`ZellijBackend`): a `PaneBackend` that attaches to a running zellij session, driving it with `zellij action write-chars` / `write` and reading it with `dump-screen`. The `--pane` value is the session name.
- **GNU screen backend** (`ScreenBackend`): a `PaneBackend` that attaches to a running screen session via `-X stuff` and reads it via `-X hardcopy`. The `--pane` value is the session name.
- **`panedrive run <script>`**: a step-runner that executes a line-oriented script (`press`, `type`, `wait-until`, `assert`, `capture`, `sleep`) against one backend in a single process. This is what lets the CLI drive the **PTY** backend, which spawns and owns the target program: `run <script> --backend pty -- <program> [args...]`. The attach backends batch steps the same way with `--pane`. Exit codes match the single-shot contract (0 pass, 1 failed assert/wait, 2 usage or backend error).
- **Secret-safe `type` in scripts**: a script `type` step accepts `--from-env VAR` (resolved at run time, never baked into the script) and `--paste` (route through the backend's paste transport), so scripted logins do not put secrets in the script file or in `send-keys` argv.
- **Richer conditions**: numeric `>`, `<`, `>=`, `<=` and substring `~=` (contains), in addition to `=` / `!=` / existence.
- **Structured-scrape fallback**: `run --from-capture` evaluates conditions against the captured screen (`screen`, `lines.<n>`) instead of a JSON seam, so an uninstrumented app degrades gracefully instead of not working.
- **`run --settle`**: after each key/type step, wait for the seam to change before the next step, so an `assert` right after a `press` does not race the UI's asynchronous snapshot.
- **Cross-language seam spec** (`docs/SEAM.md`): the one-page JSON-file contract plus Rust/Go/Python/JS adapters, since the driver is language-agnostic.
- **Build metadata in `--version`**: `panedrive --version` now reports the git short-SHA and commit timestamp (e.g. `0.1.1 (a1b2c3d4e5 2026-08-28T12:21:14Z)`); the timestamp is the commit time, so it stays deterministic, with a clean fallback to the bare version when built without git.
- `CHANGELOG.md` following Keep a Changelog.
- `docs.rs` metadata to build both crates with all features.

### Changed

- README: mark the project as published, add install instructions and badges, and document the `run` script-runner and the zellij and PTY backends.

## [0.1.0] - 2026-08-28

Initial release of the two-crate toolkit for driving and verifying terminal UIs headlessly: real-key input paired with a structured JSON state seam.

### Added

- **`paneview`**: the state seam library linked into a UI. `DumpState` trait and `write_snapshot` to expose app state as JSON instead of scraping the screen.
- **`panedrive`**: the out-of-process driver (library + CLI). Press real keys, wait on JSON-state conditions, assert, and record.
- **tmux backend** (default): attach to and drive an already-running pane.
- **PTY backend** behind the `pty` feature: spawn a TUI in an owned pseudo-terminal and parse its screen with `vt100`; no multiplexer needed, suited to CI and in-process `cargo test`. Library API today.
- **Secret-safe text input** for `type`: `--stdin`, `--from-env`, and tmux `--paste`, so secrets never appear in argv.
- **Dependency-layering wall** (`make deps`): `panedrive` may depend on `paneview`, never the reverse.
- **CI**: deterministic quality gate (`make check`), cargo-deny supply-chain check, and cross-platform release binaries via cargo-dist.

[panedrive 0.1.6]: https://github.com/0xheartcode/panekit/compare/panedrive-v0.1.5...v0.1.6
[paneview 0.1.4]: https://github.com/0xheartcode/panekit/compare/panedrive-v0.1.5...v0.1.6
[panedrive 0.1.5]: https://github.com/0xheartcode/panekit/compare/v0.1.4...panedrive-v0.1.5
[paneview 0.1.3]: https://github.com/0xheartcode/panekit/compare/v0.1.4...6d7b5df
[0.1.4]: https://github.com/0xheartcode/panekit/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/0xheartcode/panekit/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/0xheartcode/panekit/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/0xheartcode/panekit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/0xheartcode/panekit/releases/tag/v0.1.0
