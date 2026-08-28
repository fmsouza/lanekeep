"""lanekeep/no-mutable-default-argument, authored in Python.

A port of `crates/lanekeep-rules/rules/no-mutable-default-argument.ts`, on the
same terms as `rules/nobroadexcept/rule.py`.
"""

from wit_world.imports import types

from lanekeep import Handlers, capture

ID = "lanekeep/no-mutable-default-argument"


def metadata():
    return types.RuleMetadata(
        id=ID,
        languages=["python"],
        severity="error",
        card=types.RuleCard(
            message="mutable default argument",
            remediation=(
                "default to None and build the value inside the function, so "
                "each call gets its own"
            ),
            examples=types.RuleExamples(
                bad="def add(item, items=[]):",
                good=(
                    "def add(item, items=None):\n"
                    "    items = [] if items is None else items"
                ),
            ),
        ),
        gates=types.RuleGates(
            path_matches=[],
            path_not_matches=[],
            file_contains=[],
            file_not_contains=[],
        ),
        queries=[types.QueryFor(language="python", query="(default_parameter value: (_) @value) @param")],
        timeout=None,
    )


def check(ctx, m):
    value = capture(m, "value")
    param = capture(m, "param")
    if value is None or param is None:
        return

    kind = ctx.kind(value)

    # A literal container is constructed once, at definition time.
    if kind in ("list", "dictionary", "set"):
        ctx.report(
            param,
            f"default `{ctx.text(value)}` is created once and shared by every call",
            None,
        )
        return

    # `list()`, `dict()`, `set()` — the same thing spelled as a call. Only the
    # builtins, and only when nothing local has taken the name.
    if kind == "call":
        children = ctx.named_children(value)
        callee = children[0] if children else None
        if callee is None or ctx.kind(callee) != "identifier":
            return

        name = ctx.text(callee)
        if name not in ("list", "dict", "set"):
            return
        if ctx.binding_kind(callee) is not None:
            return

        ctx.report(
            param,
            f"default `{name}()` is created once and shared by every call",
            None,
        )


def handlers():
    return Handlers(metadata=metadata, check=check)
