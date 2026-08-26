# panedrive

The **driver** half of [panekit](https://github.com/0xheartcode/panekit): press
real keys into a terminal UI, then read its state from a JSON seam (not the
screen) to wait, assert, and record.

```bash
panedrive press "2 Down Enter" --pane mysession:1.0     # named keys + chars
panedrive type "my-passphrase"      --pane mysession:1.0     # a literal string
panedrive wait-until "focus=fleet" --state run.state.json --timeout-ms 5000
panedrive assert     "bag.count=2" --state run.state.json   # exit 0 held, 1 failed
panedrive watch      --state run.state.json --distinct       # record transitions
```

Input goes through the real keybindings of the real running UI; output is read
from the [`paneview`](https://crates.io/crates/paneview) state seam, so asserts
are deterministic with no screen-scraping.

For secrets, read the value from stdin or an env var instead of argv, and use
the PTY backend or tmux `--paste` so it does not transit `send-keys` argv:

```bash
panedrive type --from-env VAULT_PASS --paste --pane mysession:1.0
```

## Backends

Driving is abstracted behind one `PaneBackend` trait:

- **tmux** (default): attach to a running pane.
- **PTY** (behind the `pty` feature, `PtyBackend`): spawn the UI in a
  pseudo-terminal this process owns and parse its screen with `vt100`. No
  multiplexer needed, so it suits CI and in-process `cargo test`.

Part of [**panekit**](https://github.com/0xheartcode/panekit). MIT licensed.
