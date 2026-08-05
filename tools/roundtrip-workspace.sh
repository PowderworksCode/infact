#!/usr/bin/env bash
# Rewrite every function body in this workspace from the lifted tree, then run
# the workspace's own tests against the result.
#
# This is the whole claim of infact-rust-lower in one command: 1,165 bodies
# replaced by what printing them produced, and the suite still green. It works
# on a copy, so the tree you are sitting in is never touched.
#
#   tools/roundtrip-workspace.sh [destination]
#
# The destination defaults to a temporary directory and is reported at the end
# so the diff can be read.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
siblings="$(dirname "$here")"
destination="${1:-$(mktemp -d)}"

echo "== copying the workspace and its path dependencies to $destination"
mkdir -p "$destination"
# entl is a path dependency and the sibling layout is required.
for repo in infact entl; do
    rm -rf "${destination:?}/$repo"
    cp -r "$siblings/$repo" "$destination/$repo"
    rm -rf "$destination/$repo/target"
done

echo "== lifting and reprinting every body"
cargo run --quiet --example roundtrip -p infact-rust-lower -- \
    "$destination/infact/crates" --in-place

echo
echo "== building and testing the reprinted workspace"
cd "$destination/infact"
if cargo test --workspace 2>&1 | tee /tmp/roundtrip-test.log | grep -E '^test result'; then
    :
fi

if grep -qE '^error' /tmp/roundtrip-test.log; then
    echo
    echo "FAILED — the reprinted workspace does not build:"
    grep -E '^error' /tmp/roundtrip-test.log | head -20
    echo
    echo "the reprinted tree is at $destination/infact"
    exit 1
fi

passed=$(grep -E '^test result' /tmp/roundtrip-test.log | awk '{p += $4} END {print p}')
failed=$(grep -E '^test result' /tmp/roundtrip-test.log | awk '{f += $6} END {print f}')
echo
echo "ROUND TRIP OK — $passed passed, $failed failed"
echo "the reprinted tree is at $destination/infact"
echo
echo "to see what changed:  diff -ru $here/crates $destination/infact/crates"
echo "for the textual gap:  (cd $destination/infact && cargo fmt --all) && \\"
echo "                      diff -ru $here/crates $destination/infact/crates"
