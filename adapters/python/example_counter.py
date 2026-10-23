#!/usr/bin/env python3
"""A minimal Python "TUI" instrumented with the panekit seam, the Python twin of
the Rust `counter_tui` example. It reads line commands from stdin (`inc`, `dec`,
`quit`) and, after every change, writes its state as JSON so `panedrive` can
drive and assert against it without scraping the screen.

  panedrive run adapters/python/counter.pds --backend pty \
    --state /tmp/py-counter.json \
    -- python3 "$(pwd)/adapters/python/example_counter.py" /tmp/py-counter.json
"""

import sys

from panedrive_seam import SnapshotWriter


def main():
    state_path = sys.argv[1] if len(sys.argv) > 1 else "counter.state.json"
    # A stateful writer stamps _meta.seq, so `panedrive run --ack` can wait for
    # the app to acknowledge each keypress, the same as the Rust writer.
    writer = SnapshotWriter(state_path)
    count = 0
    last = "start"

    # Emit the initial state before any input, so a driver can wait on it.
    writer.write({"count": count, "last": last})

    for line in sys.stdin:
        cmd = line.strip()
        if cmd == "inc":
            count += 1
            last = "inc"
        elif cmd == "dec":
            count -= 1
            last = "dec"
        elif cmd == "quit":
            break
        else:
            last = "unknown:{}".format(cmd)
        writer.write({"count": count, "last": last})


if __name__ == "__main__":
    main()
