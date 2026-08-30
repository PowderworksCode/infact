"""How much of a real corpus can ty actually type?

RECORDED 08-04 against ty 0.0.66. Receivers of method calls, revealed:

                        stdlib (45,116)   third-party (33,046)
    <module ...>              22.3%             25.0%
    Unknown                   36.5%             32.3%
    partly Unknown             9.3%              4.7%
    CONCRETE, non-module      31.8%             37.9%

`<module ...>` receivers are already free from syntactic resolution, and
`Unknown` is where ty adds nothing. The CONCRETE, non-module row is the
marginal gain.

About one receiver in three, and generous: 512 of the stdlib wins are `Never`.

NEST THE CORPUS. Copy into `OUT_DIR/corpus/...`, never to a project root that
recreates stdlib package names — ty then resolves `import encodings` to the
copies and type-checks a fake stdlib against typeshed stubs of itself, which
provokes a salsa cycle. That panic cancels EVERY other file in the run, and it
was misread once as ty panicking on stock CPython. It does not; pristine
CPython checks clean in 0.59s.


`reveal_type()` is ty's own batch mechanism: it reports the inferred type as a
diagnostic. So inject one for every method-call receiver in real code, run ty
once over the lot, and count how many come back concrete rather than `Unknown`.

The receiver of `x.foo()` is the thing we cannot resolve today. That is what is
being measured, not types in general.

    python3 reveal_harness.py SRC_ROOT OUT_DIR N
"""

import ast
import os
import shutil
import sys


class Inject(ast.NodeTransformer):
    """Insert `reveal_type(r)` before any statement calling `r.method()`."""

    def __init__(self):
        self.injected = 0

    def _rewrite_body(self, body):
        out = []
        for statement in body:
            receivers = []
            for node in ast.walk(statement):
                # Do not descend into nested definitions; they get their own pass.
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                    target = node.func.value
                    if isinstance(target, ast.Name) and target.id not in receivers:
                        receivers.append(target.id)
            visited = self.visit(statement)
            # A docstring must stay first, and nothing may precede it.
            if not _is_docstring(statement):
                for name in receivers:
                    out.append(
                        ast.Expr(
                            ast.Call(
                                func=ast.Name(id="reveal_type", ctx=ast.Load()),
                                args=[ast.Name(id=name, ctx=ast.Load())],
                                keywords=[],
                            )
                        )
                    )
                    self.injected += 1
            out.append(visited)
        return out

    def generic_visit(self, node):
        for field, value in ast.iter_fields(node):
            if isinstance(value, list) and value and isinstance(value[0], ast.stmt):
                setattr(node, field, self._rewrite_body(value))
            elif isinstance(value, ast.AST):
                self.visit(value)
            elif isinstance(value, list):
                for item in value:
                    if isinstance(item, ast.AST):
                        self.visit(item)
        return node


def _is_docstring(statement):
    return (
        isinstance(statement, ast.Expr)
        and isinstance(statement.value, ast.Constant)
        and isinstance(statement.value.value, str)
    )


def main(src_root, out_dir, limit):
    if os.path.exists(out_dir):
        shutil.rmtree(out_dir)
    os.makedirs(out_dir)

    candidates = []
    for directory, _, names in os.walk(src_root):
        if "test" in directory:
            continue
        for name in sorted(names):
            if name.endswith(".py") and not name.startswith("test_"):
                candidates.append(os.path.join(directory, name))
    candidates.sort()
    # Spread the sample across the corpus rather than taking the alphabetical head.
    step = max(1, len(candidates) // limit)
    chosen = candidates[::step][:limit]
    # Keep every sibling of a chosen file: a package missing half its modules
    # reports Unknown for reasons that are the harness's fault, not ty's.
    packages = {os.path.dirname(p) for p in chosen}
    chosen = [c for c in candidates if os.path.dirname(c) in packages]

    total = 0
    written = 0
    for path in chosen:
        try:
            with open(path, "rb") as handle:
                tree = ast.parse(handle.read())
        except (SyntaxError, ValueError, OSError):
            continue
        injector = Inject()
        tree = injector.visit(tree)
        ast.fix_missing_locations(tree)
        try:
            text = ast.unparse(tree)
        except Exception:
            continue
        relative = os.path.relpath(path, src_root)
        destination = os.path.join(out_dir, relative)
        os.makedirs(os.path.dirname(destination), exist_ok=True)
        with open(destination, "w") as handle:
            handle.write(text)
        total += injector.injected
        written += 1

    print(f"files written  {written}")
    print(f"reveals        {total}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]))
