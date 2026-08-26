#!/usr/bin/env bash
# Dependency-layering wall: the seam crate (paneview) must stay independent of
# the driver crate (panedrive). The dependency arrow points one way only,
# panedrive -> paneview, so any UI can link the seam without pulling in tmux,
# clap, or the driver machinery.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fail=0

# paneview must NOT depend on panedrive.
if grep -q 'panedrive' "$root/paneview/Cargo.toml"; then
    echo "FAIL: paneview/Cargo.toml references panedrive (seam must not depend on the driver)"
    fail=1
fi

# panedrive SHOULD depend on paneview (the shared contract).
if ! grep -q 'paneview' "$root/panedrive/Cargo.toml"; then
    echo "FAIL: panedrive/Cargo.toml does not depend on paneview (the shared seam)"
    fail=1
fi

if [ "$fail" -eq 0 ]; then
    echo "OK: layering intact (panedrive -> paneview, one way)"
fi
exit "$fail"
