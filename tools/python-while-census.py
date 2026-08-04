"""What Python's `while` loops actually are.

RECORDED 08-04, over /usr/lib/python3.12 and /usr/lib/python3/dist-packages,
2,513 loops:

     761  30.3%  while True / while 1      <- walks nothing at all
     467  18.6%  other comparison
     292  11.6%  counter compared and rebound
     282  11.2%  compound condition
     168   6.7%  advances a cursor
     137   5.5%  calls something each time

BEWARE THE 11.6%. That category is LOOSE — it counts a rebind anywhere,
including inside a branch, and loops that index nothing. It was quoted into a
doc comment before being tightened, and the number a rule can act on is:

    307  condition is a bounded comparison
     94    ...advanced by one at the TOP LEVEL of the body
     53      ...and indexes a sequence with it
     41      ...indexes nothing: a walk over a range

3.7%, not 11.6%. A category counted loosely is never the category a rule can
act on; re-derive the tight number before trusting a loose one.


The normalizer emits `Opaque{kind:"while"}` on the stated ground that "a while
walks something unnamed". That was never measured. This classifies every
`while` in a corpus by its condition, and by whether the body mutates what the
condition reads — which is what separates draining a worklist from spinning on
a flag.

    python3 while_census.py ROOT...
"""

import ast
import os
import sys
from collections import Counter

def mutations(body, name):
    """Method calls on `name` inside the loop body, e.g. `queue.pop()`."""
    found = set()
    for node in ast.walk(ast.Module(body=body, type_ignores=[])):
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == name
        ):
            found.add(node.func.attr)
    return found

def rebinds(body, name):
    """Whether the body assigns to `name`, e.g. `i += 1` or `node = node.next`."""
    for node in ast.walk(ast.Module(body=body, type_ignores=[])):
        if isinstance(node, (ast.Assign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if isinstance(target, ast.Name) and target.id == name:
                    return True
    return False

DRAINING = {"pop", "popleft", "popitem", "get_nowait", "get"}

def classify(node):
    test = node.test
    if isinstance(test, ast.Constant) and test.value in (True, 1):
        return "while True / while 1"
    if isinstance(test, ast.NamedExpr):
        return "walrus: read until exhausted"
    if isinstance(test, ast.Name):
        taken = mutations(node.body, test.id)
        if taken & DRAINING:
            return "drains a container (truthiness)"
        if rebinds(node.body, test.id):
            return "advances a cursor (rebinds the name)"
        return "spins on a flag"
    if isinstance(test, ast.UnaryOp) and isinstance(test.op, ast.Not):
        return "until a flag is set"
    if isinstance(test, ast.Compare):
        left = test.left
        if (
            len(test.ops) == 1
            and isinstance(test.ops[0], (ast.Lt, ast.LtE, ast.Gt, ast.GtE))
            and isinstance(left, ast.Name)
            and rebinds(node.body, left.id)
        ):
            return "index walk (bounded, counter advanced)"
        return "other comparison"
    if isinstance(test, ast.Call):
        return "calls something each time"
    if isinstance(test, ast.Attribute):
        return "reads an attribute each time"
    if isinstance(test, ast.BoolOp):
        return "compound condition"
    return f"other ({type(test).__name__})"

def main(roots):
    tally = Counter()
    total = 0
    for root in roots:
        for directory, _, names in os.walk(root):
            for name in names:
                if not name.endswith(".py"):
                    continue
                try:
                    with open(os.path.join(directory, name), "rb") as handle:
                        tree = ast.parse(handle.read())
                except (SyntaxError, ValueError, OSError):
                    continue
                for node in ast.walk(tree):
                    if isinstance(node, ast.While):
                        total += 1
                        tally[classify(node)] += 1
    print(f"while loops   {total}\n")
    for kind, count in tally.most_common():
        print(f"{count:>6}  {100 * count / total:>5.1f}%  {kind}")

if __name__ == "__main__":
    main(sys.argv[1:])
