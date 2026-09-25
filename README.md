# 🖥️ panekit

[![paneview on crates.io](https://img.shields.io/crates/v/paneview.svg?label=paneview)](https://crates.io/crates/paneview)
[![panedrive on crates.io](https://img.shields.io/crates/v/panedrive.svg?label=panedrive)](https://crates.io/crates/panedrive)
[![docs.rs](https://img.shields.io/docsrs/paneview?label=docs.rs)](https://docs.rs/paneview)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Drive and verify terminal UIs headlessly**, so an agent (or a test, or CI) can
operate a TUI the way a user does: press real keys, then read real state.

> **Status:** v0.1.2, published on [crates.io](https://crates.io/crates/panedrive).
> Two backends are proven live: tmux (attach to a running pane) and PTY (spawn a
> TUI with no multiplexer, behind the `pty` feature); zellij and GNU screen also
> ship. panedrive speaks machine-readable JSON (`--json`, `run --events`), records
> asciinema casts (`--cast`), and can capture a live session into a script
> (`record`). The state seam and condition engine are covered at ~90%, and the
> CLI's exit-code contract is integration-tested.

## Install

```bash
cargo install panedrive          # the driver CLI (tmux backend)
cargo install panedrive --features pty   # add the PTY backend
cargo add paneview               # the state seam, in your UI's crate
```

## Try it in a minute

The repo ships a tiny `counter_tui` example whose seam is
`{ "count": <n>, "last": <cmd>, "ready": <bool> }`. Drive it headlessly with a
script, no terminal to watch:

```bash
git clone https://github.com/0xheartcode/panekit && cd panekit
cargo build -p panedrive --features pty --example counter_tui

# spawn the TUI in a PTY, type two `inc` commands, wait for the seam to catch up
panedrive run panedrive/examples/counter.pds \
  --backend pty --state /tmp/counter.state.json \
  -- ./target/debug/examples/counter_tui /tmp/counter.state.json

echo $?                      # 0: every step passed
cat /tmp/counter.state.json  # {"count":2,"last":"inc","ready":true}
```

Nothing here is counter-specific: the seam is whatever JSON *your* UI puts in
`dump_state`, and conditions are dot-paths into it. Watch a clock field tick,
gate on `queue.len!=0`, assert `date=2026-08-28`, or check a nested
`rows.2.status=done`; the driver only reads JSON, so it works the same for any
shape of state.

## Why this exists

Browsers have Playwright; terminal UIs had nothing clean for the *agent* case.
The trick that makes it work is a split most people skip:

- **Input** goes through the **real keybindings** of the **real running UI**
  (via tmux or a PTY). Full fidelity, with no bypass path that drifts from what
  users actually press.
- **Output** is read from a **JSON state seam** the UI exposes, *not* by scraping
  ASCII off the screen. Structured state means deterministic asserts and
  `wait-until` conditions, with no layout-brittleness and no races.

Pair real-key input with structured-state output and you get fidelity *and*
determinism. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Two crates

| | [`paneview`](paneview) | [`panedrive`](panedrive) |
|---|---|---|
| kind | library (linked **into** your UI) | library + `panedrive` binary (runs **outside**) |
| role | the **state seam**: expose state as JSON | the **driver**: press keys, wait, assert, record |
| deps | tiny (`serde`, `serde_json`) | `clap`; tmux by default, `portable-pty`/`vt100` behind the `pty` feature |
| per-project? | yes, each UI's state shape is its own | no, one driver serves every UI |

The arrow points one way, `panedrive` depends on `paneview`, so any UI can link
the seam without pulling in the driver's machinery. A `make deps` wall enforces
it.

## Quick start

**1. Expose a seam in your UI** (`paneview`):

```rust
use paneview::{write_snapshot, DumpState};

impl DumpState for App {
    fn dump_state(&self) -> serde_json::Value {
        serde_json::json!({ "focus": self.focus_name(), "bag": { "count": self.bag_len() } })
    }
}
// call this wherever state changes (e.g. once per render, or from a
// `--dump-state <path>` flag you add):
write_snapshot(&app, std::path::Path::new("run.state.json"))?;
```

Not a Rust UI? The seam is just a JSON file, so any language can expose one by
writing the same shape (atomically). See [`docs/SEAM.md`](docs/SEAM.md) for the
one-page contract and Go/Python/JS adapters.

**2. Drive it** (`panedrive`), from a shell or an agent:

```bash
# press real keys into the running pane (names + single chars)
panedrive press "2 Down Down Enter" --pane mysession:1.0

# type a literal string char-by-char (e.g. a passphrase or a text field)
panedrive type "my-passphrase" --pane mysession:1.0

# block until the UI reports the state you expect (polls the seam, no sleep-guessing)
panedrive wait-until "focus=fleet" --state run.state.json --timeout-ms 5000

# one-shot assertion (exit 0 held, 1 failed), usable as a CI gate
panedrive assert "bag.count=2" --state run.state.json

# see what you can assert on: every dot-path in the seam, with its type
panedrive state --state run.state.json --paths

# record the state timeline over a window, one JSONL line per change with
# --distinct (catches transitions a single assert would miss)
panedrive watch --state run.state.json --for-ms 10000 --interval-ms 200 --distinct
```

Exit codes: `0` success or held, `1` condition failed or timed out, `2` usage or
backend error.

Every command that takes `--state` also reads `$PANEDRIVE_STATE` as its default,
so the app's snapshot writer and the driver can share one path (`export
PANEDRIVE_STATE=run.state.json`) instead of hand-syncing it.

### Scripts (`run`)

For a whole flow in one process, put the steps in a file (one per line; `#`
comments allowed) and `run` it:

```text
# login.pds
type inc
press Enter
wait-until count=1 --timeout-ms 2000
assert last=inc
capture
```

```bash
# attach to a running pane (tmux, zellij, or screen)
panedrive run login.pds --backend tmux --pane mysession:1.0 --state run.state.json

# or spawn the program yourself in a PTY (no multiplexer, ideal for CI)
panedrive run login.pds --backend pty --state run.state.json -- ./my-tui --flag
```

Steps: `press <keys>`, `type <text>` (also `type --from-env VAR` and
`type --paste ...` for secrets, see below), `wait-until <cond> [--timeout-ms N]
[--interval-ms N]`, `assert <cond>`, `capture`, `sleep <200ms|1s|N>`. The run
stops at the first failing `assert`/`wait-until` and maps to the same exit codes
(`0` all passed, `1` a step failed, `2` usage or backend error). `run` is the
only way to drive the **PTY** backend from the CLI, because that backend spawns
and owns the target program, so it must live for the whole script.

After a key that *changes* state, `wait-until` the new state rather than
`assert` it immediately: the UI writes its next snapshot asynchronously, so an
`assert` right after a `press` can read the pre-change seam. Use `assert` for
state that has already settled. Or pass **`--settle`** to `run`, and every
key/type step waits (up to ~1s) for the seam to change before the next step, so
you can `assert` right after a `press` without the race.

No seam to read? **`run --from-capture`** evaluates conditions against the
captured screen text instead of a state file (`screen` is the whole screen,
`lines.<n>` each row), so `wait-until "screen~=Ready" --from-capture` drives an
uninstrumented app. Less precise than a real seam, but it degrades gracefully.

### Conditions

`wait-until` / `assert` take a tiny expression over the state JSON:

| spec | true when |
|---|---|
| `focus` / `focus?` | the dot-path exists |
| `focus=fleet` | scalar at the path equals `fleet` |
| `open=true` | booleans/numbers compare by text |
| `bag.count!=0` | path exists and its scalar differs |
| `bag.count>2` | numeric compare (`>`, `<`, `>=`, `<=`) |
| `line~=Ready` | the scalar contains the substring |
| `rows.2=x` | array indices are path segments |

Equality is numeric-aware: `count=2` matches a JSON `2` or `2.0` (and `1e3`
matches `1000`); non-numeric scalars compare as text.

When a condition fails, panedrive tells you what the seam actually held, not just
that it failed:

```text
assert failed: count=2 (count was 1)
```

### Machine-readable output, casts, and recording

Built for CI and agents, not just humans:

- **`--json`** on `assert`, `wait-until`, and `run` emits a result object on
  stdout (exit codes unchanged), so a caller gets `{ok, cond, actual, ...}` (and
  for `run`, a per-step `steps` array) without parsing prose.
- **`run --events <path>`** writes a timestamped JSONL track, one line per step
  with its outcome, for feeding a pipeline or aligning a cast. Secrets from
  `type --from-env` are never recorded.
- **`run --cast <path>`** records an [asciinema](https://asciinema.org) v2 cast
  of the session, so a failed run leaves a replayable artifact. Supported on the
  **pty** backend (native) and **tmux** (via `pipe-pane`); screen and zellij
  expose only snapshots, so they are rejected rather than faked.
- **`validate-seam <file>`** checks a JSON document against the seam contract
  (object root, scalar leaves), for bringing up a non-Rust adapter; `--json` too.
- **`record`** (pty feature) spawns a program, forwards your keystrokes to it,
  and writes what you pressed as a `.pds` script, so you can drive a session by
  hand once and replay it forever (`--cast` records it too; Ctrl-] stops):

  ```bash
  panedrive record --script login.pds --cast login.cast -- ./my-tui
  ```

## Secrets

To enter a value without it landing in argv or shell history, read it from stdin
or an environment variable instead of a literal argument:

```bash
read -rs VAULT_PASS
printf %s "$VAULT_PASS" | panedrive type --stdin --pane mysession:1.0
# or
panedrive type --from-env VAULT_PASS --pane mysession:1.0
```

Inside a `run` script the same is available, so a scripted login never puts the
secret in the script file:

```text
# login.pds
wait-until prompt=password
type --from-env VAULT_PASS      # or: type --paste --from-env VAULT_PASS
press Enter
```

Transport matters too, not just the source:

- The **PTY backend** writes bytes straight to the pseudo-terminal (no argv), so
  it is leak-free end to end. Prefer it for secrets.
- The **tmux backend** normally transits the text through `tmux send-keys` argv.
  Add `--paste` to route it through a tmux buffer (`load-buffer` + `paste-buffer`,
  deleted after paste) so it never appears in argv.

Honest limits: the target program still receives the value (that is the point),
root can read process memory, and it may echo on screen unless the field masks
it. `panedrive` only shrinks its own exposure. When the app can ingest a secret
directly (its own stdin, a keyring, a fifo), prefer that over simulating
keystrokes.

## Backends

`panedrive` drives through a single `PaneBackend` trait, so the host is the only
host-specific part:

- **tmux** (default): best for a live session a human may also be watching.
  Attaches to an already-running pane.
- **zellij** (`ZellijBackend`): attaches to a running zellij session, driving it
  with `action write-chars` / `write` and reading it with `dump-screen`. The
  `--pane` value is the session name.
- **GNU screen** (`ScreenBackend`): attaches to a running screen session, driving
  it with `-X stuff` and reading it with `-X hardcopy`. The `--pane` value is the
  session name.
- **PTY** (behind the `pty` feature, `PtyBackend`): *spawns* the TUI in a
  pseudo-terminal this process owns and parses its screen with `vt100`. No
  multiplexer needed, so it suits CI and in-process `cargo test`. Drive it from
  the CLI with `run ... --backend pty -- <program>`, or as a library API
  (spawn/drive/assert/kill).

Reading state via the JSON seam is backend-independent, so most driving does not
depend on which host you use.

## In-process harness

For a Rust UI you can also unit-test the model directly, with no process, no
terminal, and no timing. Implement `InProcessUi` (an `apply_key`, on top of the
`DumpState` seam you already have) and drive it with `Harness`, asserting over
the *same* JSON seam and the *same* condition grammar as the CLI driver:

```rust
use panedrive::harness::Harness;

Harness::new(model)
    .press("Up")?       // real keybindings, dispatched into your model
    .assert("count=1")?; // same conditions as `panedrive assert`
```

It also runs a `.pds` script in-process (`Harness::run`), so the same flow can be
checked at `cargo test` speed and driven for real over tmux/PTY. Because there is
no terminal, settling is disabled (updates are synchronous) and `capture` yields
the empty string.

## Develop

```bash
make check     # fmt + clippy(-D warnings) + dependency-layering + tests + coverage floor
make test-pty  # lint + test the feature-gated PTY backend
make cov       # coverage summary
```

`make check` is the whole gate, and is what a CI job should run. The coverage
floor is a ratchet: never lowered to pass. The gate is a Makefile since panekit
is pure Rust.

## License

MIT © 0xheartcode
