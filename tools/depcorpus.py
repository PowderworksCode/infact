"""Scan every crate in the local Cargo registry with a behavior pack.

The clippy corpus says whether a change finds MORE. This says whether it finds
more of the same thing, which is a different question and the one that has
twice caught a change being technically right and useless.

The number that matters is not the total. It is the share held by the noisiest
API. `Option::is_none_or` once fired 1,390 times across five hundred crates and
`Option::map_or` nine hundred; both were correct, both subsumed every narrower
behavior, and both had to be removed. A run where one API holds most of the
findings is that failure, whatever the total says.

The second number is the repeat multiplier: distinct (crate, file, api) against
raw findings. Reporting every occurrence rather than the first was the largest
recall win the Rust side has had, and the risk it carried was that real code
would multiply the same way and drown the output. Measured at 1.36x, it does
not: a labelled corpus repeats a shape deliberately and real crates do it
occasionally.

Nothing here is vendored. It reads whatever is unpacked under
~/.cargo/registry/src, so two machines will not agree on the total unless they
agree on the registry. Compare shares, and re-run the baseline pack rather than
trusting a recorded total from elsewhere.
"""

import os
import subprocess
import sys
from collections import Counter
from pathlib import Path

# Derived from this file's location so the harness travels with the repo rather
# than with one machine. Override any of them when the layout differs.
INFACT = Path(os.environ.get("INFACT_ROOT") or Path(__file__).resolve().parents[1])
PARSERS = Path(
    os.environ.get("INFACT_PARSER_PATH") or INFACT.parent / "entl" / "parser-packs"
)
REGISTRY = Path(
    os.environ.get("CARGO_HOME") or Path.home() / ".cargo"
) / "registry" / "src"

# `<file>:<start>-<end> <api>` is what `infact behaviors` prints per finding.
FINDING_FIELDS = 2


def crates(limit):
    """Every unpacked crate in the local registry, in a stable order."""
    found = sorted(path for path in REGISTRY.glob("*/*") if path.is_dir())
    return found[:limit]


def findings(crate, pack):
    """Every (file, api) this pack reports in one crate."""
    result = subprocess.run(
        [
            str(INFACT / "target/release/infact"), "behaviors", str(crate),
            "--parser-path", str(PARSERS),
            "--catalog-path", str(pack / "api"),
            "--behavior-path", str(pack / "behaviors"),
        ],
        capture_output=True, text=True, timeout=900,
    )
    reported = []
    for line in result.stdout.splitlines():
        parts = line.strip().split()
        if len(parts) >= FINDING_FIELDS and ":" in parts[0]:
            reported.append((parts[0].split(":")[0], parts[1]))
    return reported


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} <pack directory> [crate limit]")
    pack = Path(sys.argv[1])
    if not (pack / "behaviors").is_dir():
        sys.exit(f"{pack} has no behaviors/ subdirectory")
    binary = INFACT / "target/release/infact"
    if not binary.exists():
        sys.exit(f"no infact binary at {binary} — cargo build --release -p infact-cli")
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else None

    corpus = crates(limit)
    print(f"{len(corpus)} crates from {REGISTRY}", flush=True)

    by_api = Counter()
    by_crate = Counter()
    distinct = set()
    total = 0
    for index, crate in enumerate(corpus):
        for path, api in findings(crate, pack):
            by_api[api] += 1
            by_crate[crate.name] += 1
            distinct.add((crate.name, path, api))
            total += 1
        if index % 50 == 0:
            print(f"  {index}/{len(corpus)}  {total} findings", flush=True)

    print(f"\nfindings                    {total}")
    print(f"crates with >= 1 finding    {len(by_crate)} of {len(corpus)}")
    print(f"distinct (crate,file,api)   {len(distinct)}")
    if distinct:
        print(f"REPEAT MULTIPLIER           {total / len(distinct):.2f}x")
    print(f"distinct APIs named         {len(by_api)}")

    # The check worth repeating every time. One API holding most of the
    # findings is the failure this whole harness exists to catch.
    print("\ntop APIs:")
    for api, count in by_api.most_common(12):
        print(f"  {count:>6}  ({100.0 * count / total:4.1f}%)  {api}")
    print("\nnoisiest crates:")
    for crate, count in by_crate.most_common(8):
        print(f"  {count:>6}  {crate}")


if __name__ == "__main__":
    main()
