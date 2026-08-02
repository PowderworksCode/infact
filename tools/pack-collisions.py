"""Report distinct library callables that derive to the same behavior.

A collision is one of two things. Either the two callables really are the same
behavior spelled twice — `counts` and `counts_with_hasher` differ only in what
the caller supplies — which is expected and already handled by preferring the
plainer name. Or a distinction was erased in normalization, in which case every
match against that form names the wrong API, silently, forever.

Four such erasures turned up by accident in one session: `true` reduced to `()`,
`filter_map` to `map`, `next_back` outranking `next`, and `e.into()` to `e`.
Each produced hundreds to thousands of false matches. This finds them on
purpose.
"""

import json
import sys
from collections import defaultdict
from pathlib import Path


def leaf(path):
    return path.rsplit("::", 1)[-1]


def main(pack):
    groups = defaultdict(list)
    for file in sorted(Path(pack).glob("*.json")):
        behavior = json.loads(file.read_text())
        key = json.dumps(behavior["program"], sort_keys=True)
        groups[key].append(behavior["callable_path"])

    collisions = {
        key: sorted(set(paths)) for key, paths in groups.items() if len(set(paths)) > 1
    }
    # A shared leaf name across types is the benign case: `Option::map` and
    # `Result::map` are the same behavior on different containers, and a caller
    # who reimplements one has reimplemented the other.
    suspicious = {
        key: paths
        for key, paths in collisions.items()
        if len({leaf(path) for path in paths}) > 1
    }

    print(f"{len(groups)} distinct forms from {sum(len(p) for p in groups.values())} behaviors")
    print(f"{len(collisions)} forms are shared by more than one callable")
    print(f"{len(suspicious)} of those are shared across *different* names\n")

    for key, paths in sorted(suspicious.items(), key=lambda item: -len(item[1]))[:20]:
        print(f"  {len(paths)}x  {', '.join(paths)}")
        print(f"        {key[:150]}\n")


if __name__ == "__main__":
    main(sys.argv[1])
