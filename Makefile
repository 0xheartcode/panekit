# panekit: the walls.
#
# Philosophy: we do not rely on remembering best practices. We encode them as
# DETERMINISTIC checks that pass or fail, and loop until they pass. `make check`
# is the single gate; CI runs exactly it.
#
#   make check     fmt + clippy(-D warnings) + dependency-layering + tests + coverage floor
#   make cov       human-readable coverage report
#   make cov-html  browsable HTML coverage under target/llvm-cov/html
#   make audit     supply-chain gate (cargo-deny), run before a crates.io publish
#
# Rule: never lower a threshold to make a check pass. Fix the code, or change the
# threshold ON PURPOSE in a commit that says why. COV_FLOOR is a ratchet.

# Workspace line-coverage floor, measured with --all-features so the pty
# backend and the `record` loop are IN the number, not silently excluded.
# Ratcheted 2026-09-21: 85 -> 90 (actual 91.2%, pty paths now proven by
# tests/record_pty.rs and the pty roundtrips). Ratchet UP as slices land;
# never down to pass.
COV_FLOOR ?= 90

.PHONY: check fmt lint deps test test-pty cov-gate cov cov-html audit package

check: fmt lint deps test cov-gate
	@echo "═══ make check: ALL WALLS GREEN ═══"

# The PTY backend lives behind the `pty` feature, so `check` (default features)
# does not build it. Run this to lint + test that path.
test-pty:
	@echo "── pty backend (feature-gated) ──"
	cargo clippy -p panedrive --features pty --all-targets -- -D warnings
	cargo test -p panedrive --features pty

fmt:
	@echo "── fmt ──"
	cargo fmt --check

lint:
	@echo "── clippy (warnings are errors) ──"
	cargo clippy --workspace --all-targets -- -D warnings

deps:
	@echo "── dependency-layering (paneview must not know panedrive) ──"
	./scripts/check-deps.sh

test:
	@echo "── tests ──"
	cargo test --workspace --quiet

# --all-features so the pty backend and the `record` loop are measured, not
# invisibly excluded (they were, before 2026-09-21). The pty roundtrips and
# tests/record_pty.rs run under instrumentation and cover those paths.
cov-gate:
	@echo "── coverage floor ($(COV_FLOOR)% lines, --all-features) ──"
	cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines $(COV_FLOOR)

cov:
	cargo llvm-cov --workspace --all-features --summary-only

cov-html:
	cargo llvm-cov --workspace --all-features --html
	@echo "open target/llvm-cov/html/index.html"

# Supply-chain gate (advisories/bans/licenses/sources), config in deny.toml.
# --all-features so the pty backend's deps are in the graph; the flag must
# precede the subcommand in cargo-deny.
audit:
	cargo deny --all-features check

# Validate that both crates package cleanly (manifests, README/LICENSE, file
# set) without needing the registry, so publish blockers surface before publish.
# --no-verify skips the build step, which for panedrive would need paneview on
# crates.io first.
package:
	cargo package --workspace --no-verify
