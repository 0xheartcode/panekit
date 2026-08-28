# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Both crates (`paneview` and `panedrive`) share one version and are released together.

## [Unreleased]

## [0.1.1] - 2026-08-28

### Added

- **zellij backend** (`ZellijBackend`): a `PaneBackend` that attaches to a
  running zellij session, driving it with `zellij action write-chars` / `write`
  and reading it with `dump-screen`. The `--pane` value is the session name.
- **GNU screen backend** (`ScreenBackend`): a `PaneBackend` that attaches to a
  running screen session via `-X stuff` and reads it via `-X hardcopy`. The
  `--pane` value is the session name.
- **`panedrive run <script>`**: a step-runner that executes a line-oriented
  script (`press`, `type`, `wait-until`, `assert`, `capture`, `sleep`) against
  one backend in a single process. This is what lets the CLI drive the **PTY**
  backend, which spawns and owns the target program: `run <script> --backend pty
  -- <program> [args...]`. The attach backends batch steps the same way with
  `--pane`. Exit codes match the single-shot contract (0 pass, 1 failed
  assert/wait, 2 usage or backend error).
- **Secret-safe `type` in scripts**: a script `type` step accepts
  `--from-env VAR` (resolved at run time, never baked into the script) and
  `--paste` (route through the backend's paste transport), so scripted logins do
  not put secrets in the script file or in `send-keys` argv.
- **Richer conditions**: numeric `>`, `<`, `>=`, `<=` and substring `~=`
  (contains), in addition to `=` / `!=` / existence.
- **Structured-scrape fallback**: `run --from-capture` evaluates conditions
  against the captured screen (`screen`, `lines.<n>`) instead of a JSON seam, so
  an uninstrumented app degrades gracefully instead of not working.
- **`run --settle`**: after each key/type step, wait for the seam to change
  before the next step, so an `assert` right after a `press` does not race the
  UI's asynchronous snapshot.
- **Cross-language seam spec** (`docs/SEAM.md`): the one-page JSON-file contract
  plus Rust/Go/Python/JS adapters, since the driver is language-agnostic.
- **Build metadata in `--version`**: `panedrive --version` now reports the git
  short-SHA and commit date (e.g. `0.1.1 (a1b2c3d4e5 2026-08-28)`), with a clean
  fallback to the bare version when built without git.
- `CHANGELOG.md` following Keep a Changelog.
- `docs.rs` metadata to build both crates with all features.

### Changed

- README: mark the project as published, add install instructions and badges,
  and document the `run` script-runner and the zellij and PTY backends.

## [0.1.0] - 2026-08-28

Initial release of the two-crate toolkit for driving and verifying terminal UIs
headlessly: real-key input paired with a structured JSON state seam.

### Added

- **`paneview`** — the state seam library linked into a UI. `DumpState` trait and
  `write_snapshot` to expose app state as JSON instead of scraping the screen.
- **`panedrive`** — the out-of-process driver (library + CLI). Press real keys,
  wait on JSON-state conditions, assert, and record.
- **tmux backend** (default) — attach to and drive an already-running pane.
- **PTY backend** behind the `pty` feature — spawn a TUI in an owned
  pseudo-terminal and parse its screen with `vt100`; no multiplexer needed, suited
  to CI and in-process `cargo test`. Library API today.
- **Secret-safe text input** for `type`: `--stdin`, `--from-env`, and tmux
  `--paste`, so secrets never appear in argv.
- **Dependency-layering wall** (`make deps`): `panedrive` may depend on `paneview`,
  never the reverse.
- **CI**: deterministic quality gate (`make check`), cargo-deny supply-chain check,
  and cross-platform release binaries via cargo-dist.

[Unreleased]: https://github.com/0xheartcode/panekit/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/0xheartcode/panekit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/0xheartcode/panekit/releases/tag/v0.1.0
