#!/usr/bin/env node
'use strict';

// A minimal Node "TUI" instrumented with the panekit seam, the JS twin of the
// Rust `counter_tui` example. It reads line commands from stdin (`inc`, `dec`,
// `quit`) and, after every change, writes its state as JSON so `panedrive` can
// drive and assert against it without scraping the screen.
//
//   panedrive run adapters/node/counter.pds --backend pty \
//     --state /tmp/node-counter.json \
//     -- node adapters/node/example-counter.js /tmp/node-counter.json

const readline = require('readline');
const { writeSnapshot } = require('./panedrive-seam');

const statePath = process.argv[2] || 'counter.state.json';
let count = 0;
let last = null;

function snapshot() {
  // Machine-first keys, not a mirror of the screen: this is the seam.
  writeSnapshot({ count, last }, statePath);
}

// Emit the initial state before any input, so a driver can wait on it.
snapshot();

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  const cmd = line.trim();
  if (cmd === 'inc') {
    count += 1;
    last = 'inc';
  } else if (cmd === 'dec') {
    count -= 1;
    last = 'dec';
  } else if (cmd === 'quit') {
    rl.close();
    return;
  } else {
    last = cmd;
  }
  snapshot();
});
rl.on('close', () => process.exit(0));
