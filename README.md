# 🖥️ panekit

**Drive and verify terminal UIs headlessly**, so an agent (or a test, or CI) can
operate a TUI the way a user does: press real keys, then read real state.

> **Status:** v0.1.0, working and unpublished. Two backends are proven live: tmux
> (attach to a running pane) and PTY (spawn a TUI with no multiplexer, behind the
> `pty` feature). The state seam and condition engine are covered at ~90%, and the
> CLI's exit-code contract is integration-tested.

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

# record the state timeline over a window, one JSONL line per change with
# --distinct (catches transitions a single assert would miss)
panedrive watch --state run.state.json --for-ms 10000 --interval-ms 200 --distinct
```

Exit codes: `0` success or held, `1` condition failed or timed out, `2` usage or
backend error.

### Conditions

`wait-until` / `assert` take a tiny expression over the state JSON:

| spec | true when |
|---|---|
| `focus` / `focus?` | the dot-path exists |
| `focus=fleet` | scalar at the path equals `fleet` |
| `open=true` | booleans/numbers compare by text |
| `bag.count!=0` | path exists and its scalar differs |
| `rows.2=x` | array indices are path segments |

Equality is numeric-aware: `count=2` matches a JSON `2` or `2.0` (and `1e3`
matches `1000`); non-numeric scalars compare as text.

## Secrets

To enter a value without it landing in argv or shell history, read it from stdin
or an environment variable instead of a literal argument:

```bash
read -rs VAULT_PASS
printf %s "$VAULT_PASS" | panedrive type --stdin --pane mysession:1.0
# or
panedrive type --from-env VAULT_PASS --pane mysession:1.0
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
- **PTY** (behind the `pty` feature, `PtyBackend`): *spawns* the TUI in a
  pseudo-terminal this process owns and parses its screen with `vt100`. No
  multiplexer needed, so it suits CI and in-process `cargo test`. It is a library
  API (spawn/drive/assert/kill), not the one-shot CLI.
- **zellij**: planned (`action write` + `action dump-screen`), one more impl of
  the same trait.

Reading state via the JSON seam is backend-independent, so most driving does not
depend on which host you use.

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
