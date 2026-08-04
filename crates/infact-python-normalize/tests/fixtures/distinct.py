"""Callables that are genuinely different operations.

Every pair here is close enough that a normalizer erasing one distinction
would collapse them, and each distinction is one a consumer would report on.
Nothing in this file is copied from a library; each function is the smallest
thing that exercises the distinction it is named for.
"""


def first_matching(xs, p):
    for x in xs:
        if p(x):
            return x
    return None


def last_matching(xs, p):
    for x in reversed(xs):
        if p(x):
            return x
    return None


def mapped(xs, g):
    return [g(x) for x in xs]


def filtered(xs, p):
    return [x for x in xs if p(x)]


def mapped_set(xs, g):
    return {g(x) for x in xs}


def mapped_lazily(xs, g):
    return (g(x) for x in xs)


def keys_of(pairs):
    return [k for k, v in pairs]


def values_of(pairs):
    return [v for k, v in pairs]


def counted(xs, p):
    total = 0
    for x in xs:
        if p(x):
            total += 1
    return total


def summed(xs):
    total = 0
    for x in xs:
        total += x
    return total


def any_matching(xs, p):
    for x in xs:
        if p(x):
            return True
    return False


def all_matching(xs, p):
    for x in xs:
        if not p(x):
            return False
    return True


def get_or_none(d, k):
    try:
        return d[k]
    except KeyError:
        return None


def get_or_default(d, k, fallback):
    try:
        return d[k]
    except KeyError:
        return fallback


def parse_or_none(text):
    try:
        return int(text)
    except ValueError:
        return None


def index_of(xs, target):
    for i, x in enumerate(xs):
        if x == target:
            return i
    return -1


def flattened(rows):
    return [cell for row in rows for cell in row]


def grouped(pairs):
    out = {}
    for k, v in pairs:
        out[k] = v
    return out


def deduplicated(xs):
    seen = set()
    for x in xs:
        seen.add(x)
    return seen


def chunked_pairs(xs):
    return [(a, b) for a, b in xs]


def take_while_positive(xs):
    out = []
    for x in xs:
        if x <= 0:
            break
        out.append(x)
    return out


def running_totals(xs):
    total = 0
    out = []
    for x in xs:
        total += x
        out.append(total)
    return out
