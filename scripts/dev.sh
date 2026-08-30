#!/usr/bin/env bash
# Stand up a fresh Infact checkout: hooks, the workspace, and the measurement
# harnesses under tools/. Safe to re-run; every step is idempotent.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# A clone runs no hooks until it is pointed at them: core.hooksPath is per-clone
# configuration, so nothing a checkout carries can set it for you.
git config core.hooksPath .githooks
if [ ! -d .githooks ]; then
    echo "note: .githooks is fleet-managed and not synced here yet; git will"
    echo "      start using it the moment ordnung writes it."
fi

if ! command -v cargo >/dev/null; then
    echo "error: cargo is not on PATH; install Rust from https://rustup.rs" >&2
    exit 1
fi

# Infact reads entl through Cargo `path` dependencies naming ../entl, so the
# sibling checkout is a build requirement rather than a convenience. Cargo's own
# message for a missing path dependency names one manifest and one path, which
# does not say that a whole repository is absent.
if [ ! -f ../entl/Cargo.toml ]; then
    cat >&2 <<'MISSING'
error: ../entl is missing.

Infact depends on entl through Cargo path dependencies, so entl must sit
beside this checkout:

    powderworks/
      entl/
      infact/     <- you are here

Clone it next to this one and re-run:

    git -C .. clone https://github.com/PowderworksCode/entl
MISSING
    exit 1
fi

# rust-toolchain.toml names the channel, and rustup installs it on first use.
echo "== workspace"
cargo build --workspace --all-targets

# Each of these is its own workspace, so --workspace above does not reach them.
# They are how the work is measured, and they break when the crates they read
# change shape, so a checkout that builds should build them too.
for harness in tools/discard-golden tools/ts-scoreboard; do
    echo "== $harness"
    cargo build --manifest-path "$harness/Cargo.toml"
done

echo
echo "ready. the gate this repository runs in CI:"
echo "  cargo fmt --all --check"
echo "  cargo clippy --workspace --all-targets -- -D warnings"
echo "  cargo test --workspace"
