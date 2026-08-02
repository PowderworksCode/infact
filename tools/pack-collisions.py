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


def container(path):
    """The type a callable belongs to, which is what type information can tell apart."""
    return path.rsplit("::", 1)[0] if "::" in path else ""


def one_extends_the_other(left, right):
    """Whether two names describe the same operation with a different knob.

    `counts` and `counts_with_hasher` differ only in what the caller supplies,
    so one derived form is correct and the plainer name is already preferred
    when reporting.
    """
    return left.startswith(right) or right.startswith(left)


def erasure_pairs(paths):
    """Differently-named callables on the SAME container sharing one form.

    Mirrors `distinct_callables_derive_distinct_behaviors` in
    crates/infact-rust-behaviors/tests/collisions.rs. Cross-container sharing is
    a question type information answers; same-container sharing is a distinction
    normalization erased, and nothing downstream can recover it.
    """
    found = set()
    for left in paths:
        for right in paths:
            if left >= right:
                continue
            if leaf(left) == leaf(right):
                continue
            if one_extends_the_other(leaf(left), leaf(right)):
                continue
            if container(left) != container(right):
                continue
            found.add((left, right))
    return found


def main(pack):
    groups = defaultdict(list)
    for file in sorted(Path(pack).glob("*.json")):
        behavior = json.loads(file.read_text())
        key = json.dumps(behavior["program"], sort_keys=True)
        groups[key].append(behavior["callable_path"])

    collisions = {
        key: sorted(set(paths)) for key, paths in groups.items() if len(set(paths)) > 1
    }
    # The split that decides what to do about a collision. Different containers —
    # `std::Entry::key` and `alloc::Entry::key` — are one behavior on two types,
    # and a type-aware resolver settles them. The SAME container — `into_ok` and
    # `into_err` on `Result` — is a distinction no type information can recover,
    # so a resolver would absorb it while looking authoritative.
    same_type = {
        key: (paths, erasure_pairs(paths))
        for key, paths in collisions.items()
        if erasure_pairs(paths)
    }
    cross_type = {
        key: paths
        for key, paths in collisions.items()
        if key not in same_type and len({leaf(path) for path in paths}) > 1
    }

    print(f"{len(groups)} distinct forms from {sum(len(p) for p in groups.values())} behaviors")
    print(f"{len(collisions)} forms are shared by more than one callable")
    print(f"{len(cross_type)} shared across DIFFERENT containers — types can settle these")
    print(f"{len(same_type)} shared within ONE container — types can never settle these\n")

    if not same_type:
        print("no same-container erasures")
        return

    print("=" * 72)
    print("SAME-CONTAINER ERASURES — these are bugs")
    print("=" * 72)
    for key, (paths, pairs) in sorted(same_type.items(), key=lambda item: -len(item[1][1])):
        for left, right in sorted(pairs):
            print(f"  {container(left)}::{{{leaf(left)}, {leaf(right)}}}")
        print(f"        all: {', '.join(paths)}")
        print(f"        form: {key[:150]}\n")


if __name__ == "__main__":
    main(sys.argv[1])
