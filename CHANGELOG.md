# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Both crates (`paneview` and `panedrive`) share one version and are released together.

Entries are kept on a single line each: cargo-dist injects this file into the GitHub Release body, where GitHub renders every newline as a hard break, so wrapped lines would show as ragged mid-sentence breaks.

## [Unreleased]

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

[Unreleased]: https://github.com/0xheartcode/panekit/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/0xheartcode/panekit/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/0xheartcode/panekit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/0xheartcode/panekit/releases/tag/v0.1.0
