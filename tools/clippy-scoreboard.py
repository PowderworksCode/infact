"""Score our library-opportunity findings against clippy's own test corpus.

Clippy's ui tests carry their ground truth inline: `//~^ lint_name` marks the
line above as a place the lint fires. That makes them a labelled corpus nobody
wrote for us, which is worth more than any fixture we invent.

A finding counts as a hit when the range it reports contains a line clippy
annotated *and* names an API that lint is about. Our findings span statements
while clippy points at one construct, so demanding identical lines would measure
formatting rather than detection — but demanding only overlap measures nothing
at all. That version of this script once scored `manual_flatten` at 10/10 on the
strength of `Option::is_none_or` firing on the same loops, which is a false
positive landing in the right neighbourhood. Off-target findings are counted and
shown separately rather than silently credited.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

# Derived from this file's location so the harness travels with the repo rather
# than with one machine. Override any of them when the layout differs.
INFACT = Path(os.environ.get("INFACT_ROOT") or Path(__file__).resolve().parents[1])
PARSERS = Path(
    os.environ.get("INFACT_PARSER_PATH") or INFACT.parent / "entl" / "parser-packs"
)
MEASURE = Path(os.environ.get("INFACT_MEASURE") or INFACT.parent / "measure")

ANNOTATION = re.compile(r"//~(?P<up>\^*)(?P<down>v*)\s*(?P<lint>[A-Za-z_:]+)")

# What each lint says the code should have used. A finding is credited only if
# it names one of these, so that overlapping the right lines is not enough.
EXPECTED_API = {
    "manual_find": {"find"},
    "manual_find_map": {"find_map"},
    "manual_filter_map": {"filter_map"},
    "filter_map_next": {"find_map"},
    "manual_flatten": {"flatten"},
    "manual_retain": {"retain"},
    "manual_split_once": {"split_once"},
    "manual_strip": {"strip_prefix", "strip_suffix"},
    "manual_unwrap_or": {"unwrap_or"},
    "manual_while_let_some": {"pop", "pop_front"},
    "needless_range_loop": {"iter", "enumerate", "copy_from_slice"},
    "search_is_some": {"any", "position"},
}


def on_target(api, lint):
    """Whether a reported API is what this lint says to reach for."""
    return api.rsplit("::", 1)[-1] in EXPECTED_API.get(lint, set())
FINDING = re.compile(r"^(?P<file>.+?):(?P<start>\d+)-(?P<end>\d+)\s+(?P<api>\S+)")


def expected(path):
    """Line numbers clippy says its lint fires on, by lint name."""
    marks = {}
    for index, line in enumerate(path.read_text(errors="replace").splitlines(), start=1):
        found = ANNOTATION.search(line)
        if not found:
            continue
        # `^` counts lines upward from the annotation, `v` counts downward
        offset = -len(found.group("up")) or len(found.group("down"))
        marks.setdefault(found.group("lint"), set()).add(index + offset)
    return marks


def findings(source, packs):
    """Run the checker over one file, returning the ranges it reports."""
    scan = MEASURE / "bench-scan"
    subprocess.run(["rm", "-rf", str(scan)], check=False)
    (scan / "src").mkdir(parents=True)
    (scan / "src" / "lib.rs").write_text(source.read_text(errors="replace"))
    reported = []
    for pack in packs:
        result = subprocess.run(
            [
                str(INFACT / "target/release/infact"), "behaviors", str(scan),
                "--parser-path", str(PARSERS),
                "--catalog-path", str(pack / "api"),
                "--behavior-path", str(pack / "behaviors"),
            ],
            capture_output=True, text=True,
        )
        for line in result.stdout.splitlines():
            match = FINDING.match(line.strip())
            if match:
                reported.append(
                    (int(match["start"]), int(match["end"]), match["api"])
                )
    return reported


def main():
    binary = INFACT / "target/release/infact"
    if not binary.exists():
        sys.exit(f"no infact binary at {binary} — cargo build --release -p infact-cli")

    # Every pack that has behaviors derived, rather than a hardcoded list, so
    # adding a library to the score is a matter of building its pack.
    packs = sorted(p for p in (MEASURE / "packs").glob("*") if (p / "behaviors").is_dir())
    if not packs:
        sys.exit(f"no behavior packs under {MEASURE / 'packs'}")

    tests = sorted((MEASURE / "clippy").glob("*.rs"))
    if not tests:
        sys.exit(f"no clippy corpus under {MEASURE / 'clippy'} — see tools/README.md")
    total_expected = total_hit = total_reported = total_stray = 0
    rows = []
    for test in tests:
        marks = expected(test)
        wanted = set().union(*marks.values()) if marks else set()
        reported = findings(test, packs)
        hit = set()
        credited = set()
        for lint, lines in marks.items():
            for line in lines:
                for index, (start, end, api) in enumerate(reported):
                    if start <= line <= end and on_target(api, lint):
                        hit.add(line)
                        credited.add(index)
        # A finding is only wrong-kind if no lint in this file asks for the API
        # it names. One that names the right API but sits on a line clippy left
        # alone is usually clippy declining for a reason it can see and we
        # cannot — a const context, a deref coercion — not a mistake.
        asked = set().union(*(EXPECTED_API.get(lint, set()) for lint in marks)) if marks else set()
        stray = sum(
            1
            for index, (_, _, api) in enumerate(reported)
            if index not in credited and api.rsplit("::", 1)[-1] not in asked
        )
        rows.append((test.stem, len(wanted), len(reported), len(hit), stray))
        total_expected += len(wanted)
        total_reported += len(reported)
        total_hit += len(hit)
        total_stray += stray

    width = max(len(name) for name, *_ in rows)
    print(f"{'lint corpus':<{width}}  expected  reported  found  off-target")
    print("-" * (width + 40))
    for name, wanted, reported, hit, stray in rows:
        flag = "" if hit or not wanted else "   <- zero"
        print(
            f"{name:<{width}}  {wanted:>8}  {reported:>8}  {hit:>5}  {stray:>10}{flag}"
        )
    print("-" * (width + 40))
    print(
        f"{'TOTAL':<{width}}  {total_expected:>8}  {total_reported:>8}  "
        f"{total_hit:>5}  {total_stray:>10}"
    )
    if total_expected:
        print(f"\nrecall against clippy: {100 * total_hit / total_expected:.1f}%")
    if total_reported:
        credited = total_reported - total_stray
        print(f"on-target findings:    {credited}/{total_reported}")


if __name__ == "__main__":
    main()
