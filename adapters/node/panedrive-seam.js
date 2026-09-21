'use strict';

// A language-agnostic panekit state seam for Node.js apps, the JS counterpart
// of paneview's `write_snapshot`. Your app calls `writeSnapshot(state, path)`
// on every state change; `panedrive` reads that JSON and asserts against it.
//
// No dependency on panedrive itself: the seam is just an atomically written
// JSON file. See ../../docs/SEAM.md for the contract other languages follow.

const fs = require('fs');

/**
 * Write `state` (any JSON-serializable value) to `path` atomically: serialize
 * to a sibling `*.tmp` file, then rename it into place. The rename is atomic on
 * POSIX, so a concurrent reader never observes a half-written snapshot, exactly
 * how paneview's `write_snapshot` behaves in Rust.
 *
 * @param {unknown} state - the JSON-serializable state to expose
 * @param {string} path - where to write the snapshot
 */
function writeSnapshot(state, path) {
  const tmp = `${path}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(state, null, 2));
  fs.renameSync(tmp, path);
}

module.exports = { writeSnapshot };
