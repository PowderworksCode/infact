"""Where do bare-name callees actually come from?

RECORDED 08-04, over the installed Python:

    /usr/lib/python3/dist-packages   5,058 files, 128,137 bare-name calls
      builtin 42.1%  import 30.3%  module def/class 18.1%  local 7.4%
      module assign 1.1%  UNRESOLVED 0.9%
      -> 99.1% resolvable with no type inference at all
    /usr/lib/python3.12 (stdlib)       574 files,  23,901 calls -> 99.0%

This is the measurement that decided `infact-python-normalize` resolves called
names from syntax rather than from a type checker: an LSP would buy the last
0.9%, which is mostly star-imports and gettext installing `_` into builtins.

The 219,960 attribute calls it also counts are the OTHER question — receiver
types — which syntax cannot answer. See `ty-receiver-coverage.py`.


For every `foo(...)` in the corpus, ask what would be needed to resolve `foo`
to something better than a free variable. Uses `ast` rather than the entl pack
on purpose: this is a sizing measurement, not product code, and the question is
about Python's own scoping rather than about our forms.

Categories are checked in resolution order, innermost first.
"""

import ast
import builtins
import os
import sys
from collections import Counter

BUILTINS = set(dir(builtins))


class Scan(ast.NodeVisitor):
    def __init__(self):
        self.module_defs = set()      # class / def at module level
        self.module_imports = set()   # import x, from y import z
        self.module_assigns = set()   # X = ... at module level
        self.calls = []               # (name, [enclosing local name sets])
        self.stack = []               # local binding sets, innermost last

    # -- module-level binding collection -----------------------------------
    def collect_module(self, tree):
        for node in tree.body:
            self._collect_top(node)

    def _collect_top(self, node):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            self.module_defs.add(node.name)
        elif isinstance(node, ast.Import):
            for alias in node.names:
                self.module_imports.add(alias.asname or alias.name.split(".")[0])
        elif isinstance(node, ast.ImportFrom):
            for alias in node.names:
                self.module_imports.add(alias.asname or alias.name)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                for name in _names(target):
                    self.module_assigns.add(name)
        elif isinstance(node, (ast.If, ast.Try)):
            # `try: import cjson except ImportError: import json` is idiomatic
            for child in ast.iter_child_nodes(node):
                if isinstance(child, ast.stmt):
                    self._collect_top(child)
            for group in ("body", "orelse", "finalbody", "handlers"):
                for child in getattr(node, group, []) or []:
                    if isinstance(child, ast.ExceptHandler):
                        for inner in child.body:
                            self._collect_top(inner)
                    elif isinstance(child, ast.stmt):
                        self._collect_top(child)

    def classify(self, name):
        for frame in reversed(self.stack):
            if name in frame:
                return "local"
        if name in self.module_defs:
            return "module def/class"
        if name in self.module_imports:
            return "import"
        if name in self.module_assigns:
            return "module assign"
        if name in BUILTINS:
            return "builtin"
        return "unresolved"


def _names(target):
    if isinstance(target, ast.Name):
        return [target.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        out = []
        for element in target.elts:
            out.extend(_names(element))
        return out
    return []


METHODS = Counter()


def main(roots):
    tally = Counter()
    unresolved = Counter()
    files = 0
    for root in roots:
        for directory, _, names in os.walk(root):
            for name in names:
                if not name.endswith(".py"):
                    continue
                path = os.path.join(directory, name)
                try:
                    with open(path, "rb") as handle:
                        tree = ast.parse(handle.read())
                except (SyntaxError, ValueError, OSError):
                    continue
                files += 1
                scan = Scan()
                scan.collect_module(tree)
                # walk with a fresh call list, classifying in scope
                scan.calls = []
                Resolver(scan).visit(tree)
                for kind, callee in scan.calls:
                    tally[kind] += 1
                    if kind == "unresolved":
                        unresolved[callee] += 1

    total = sum(tally.values())
    print(f"files parsed          {files}")
    print(f"bare-name calls       {total}\n")
    for kind, count in tally.most_common():
        print(f"{count:>8}  {100 * count / total:>5.1f}%  {kind}")
    syntactic = sum(tally[k] for k in ("local", "module def/class", "import", "module assign", "builtin"))
    print(f"\nresolvable without any type inference: {syntactic} ({100 * syntactic / total:.1f}%)")
    print("\n-- most common unresolved names --")
    for name, count in unresolved.most_common(15):
        print(f"{count:>6}  {name}")
    print(f"\nattribute/method calls (the second problem): {sum(METHODS.values())}")


class Resolver(ast.NodeVisitor):
    """Second pass: classify each call against the scope stack."""

    def __init__(self, scan):
        self.scan = scan

    def visit_FunctionDef(self, node):
        self._function(node)

    def visit_AsyncFunctionDef(self, node):
        self._function(node)

    def _function(self, node):
        saved = self.scan.stack
        self.scan._function_frame(node)
        self.generic_visit(node)
        self.scan.stack = saved

    def visit_Call(self, node):
        if isinstance(node.func, ast.Name):
            self.scan.calls.append((self.scan.classify(node.func.id), node.func.id))
        elif isinstance(node.func, ast.Attribute):
            METHODS[node.func.attr] += 1
        self.generic_visit(node)


def _function_frame(self, node):
    local = set()
    args = node.args
    for group in (args.posonlyargs, args.args, args.kwonlyargs):
        local.update(a.arg for a in group)
    if args.vararg:
        local.add(args.vararg.arg)
    if args.kwarg:
        local.add(args.kwarg.arg)
    for inner in ast.walk(node):
        if isinstance(inner, ast.Assign):
            for target in inner.targets:
                local.update(_names(target))
        elif isinstance(inner, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and inner is not node:
            local.add(inner.name)
        elif isinstance(inner, (ast.Import, ast.ImportFrom)):
            for alias in inner.names:
                local.add(alias.asname or alias.name.split(".")[0])
        elif isinstance(inner, ast.For):
            local.update(_names(inner.target))
    self.stack = self.stack + [local]


Scan._function_frame = _function_frame

if __name__ == "__main__":
    main(sys.argv[1:])
