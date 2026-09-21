# Node.js seam adapter

The panekit state seam is just an atomically written JSON file, so any language
can expose it. This is the Node.js reference adapter, the JS counterpart of the
Rust [`paneview`](../../paneview) crate.

## Use it in your app

Copy `panedrive-seam.js` into your project (it has no dependencies) and call
`writeSnapshot` whenever your UI state changes:

```js
const { writeSnapshot } = require('./panedrive-seam');

// … on every state change:
writeSnapshot({ focus: 'fleet', bag: { count: 2 } }, process.env.PANEDRIVE_STATE);
```

Keep the snapshot **machine-first**: flat, stable keys and enums as strings,
not a mirror of the on-screen layout. See [`../../docs/SEAM.md`](../../docs/SEAM.md)
for the full contract.

## Try the example

```sh
panedrive run adapters/node/counter.pds --backend pty \
  --state /tmp/node-counter.json \
  -- node "$(pwd)/adapters/node/example-counter.js" /tmp/node-counter.json
```

The script and state paths are absolute on purpose: the pty backend spawns
`node` with its own working directory, so a relative script path may not
resolve.

`example-counter.js` is a tiny line-driven "TUI" instrumented with the adapter;
`counter.pds` drives it with the *same* script and conditions as the Rust
`counter_tui` example, which is the point: the seam is language-agnostic.

Confirm the seam file the app wrote:

```sh
panedrive state --state /tmp/node-counter.json --paths
panedrive validate-seam /tmp/node-counter.json
```
