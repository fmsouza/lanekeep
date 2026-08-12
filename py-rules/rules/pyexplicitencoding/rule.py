"""local/py-explicit-encoding, authored in Python.

A port of `lanekeep/rules/py-explicit-encoding.ts`. A text file opened without
naming its encoding reads with the locale-dependent default, which on Windows is
cp1252 rather than UTF-8 — a truncated read or a `UnicodeEncodeError` partway
through, on Windows and nowhere else. Naming `encoding="utf-8"` is the fix.
"""

from wit_world.imports import types

from lanekeep import Handlers, capture

ID = "local/py-explicit-encoding"

# The three callable names that take an encoding. `read_bytes` is the binary
# spelling of `read_text` and is deliberately not on the list.
NEEDS_ENCODING = ["open", "read_text", "write_text"]


def metadata():
    return types.RuleMetadata(
        id=ID,
        languages=["python"],
        severity="error",
        card=types.RuleCard(
            message="text file opened without an explicit encoding",
            remediation=(
                'pass `encoding="utf-8"` — the default is locale-dependent, and on '
                "Windows it is cp1252"
            ),
            examples=types.RuleExamples(
                bad="text = path.read_text()",
                good='text = path.read_text(encoding="utf-8")',
            ),
        ),
        # The TypeScript original declares no gates — the query is cheap and the
        # check filters on the callee name itself — so neither does this port. A
        # `file_contains` gate would have to list all three callees, and the gate
        # requires *every* listed needle, which would silence a file that names only
        # one of them.
        gates=types.RuleGates(
            path_matches=[],
            path_not_matches=[],
            file_contains=[],
            file_not_contains=[],
        ),
        query="""
            [
              (call function: (identifier) @fn arguments: (argument_list) @args)
              (call function: (attribute attribute: (identifier) @fn) arguments: (argument_list) @args)
            ] @call
        """,
        timeout=None,
    )


def check(ctx, m):
    fn_node = capture(m, "fn")
    args = capture(m, "args")
    call = capture(m, "call")
    if fn_node is None or args is None or call is None:
        return

    fn = ctx.text(fn_node)
    if fn not in NEEDS_ENCODING:
        return

    # A binary `open` takes no encoding at all — passing one raises `ValueError:
    # binary mode doesn't take an encoding argument`, so reporting it would send an
    # author to a change that breaks the script. `read_text`/`write_text` have no
    # mode; `read_bytes` is the binary spelling and is not on the list.
    if fn == "open" and is_binary_mode(ctx, args):
        return

    for arg in ctx.named_children(args):
        if ctx.kind(arg) != "keyword_argument":
            continue
        if ctx.text(arg).startswith("encoding"):
            return

    ctx.report(
        call,
        f"`{fn}` without `encoding=` reads cp1252 on Windows, which fails on the first non-ASCII byte",
        None,
    )


def is_binary_mode(ctx, args):
    """Whether an `open` call asks for binary mode.

    Mode is the second positional argument or a `mode=` keyword. Only the mode is
    inspected — a path that happens to contain a `b` is not a mode.
    """
    positional = 0

    for arg in ctx.named_children(args):
        kind = ctx.kind(arg)

        if kind == "keyword_argument":
            if not ctx.text(arg).startswith("mode"):
                continue

            # The literal, not the whole `mode=...` text. `mode=readable_mode`
            # contains a `b` in the identifier's spelling, and treating that as
            # binary would silently exempt a call that genuinely needs an encoding.
            # Same discipline as the positional branch below: prove it is a string
            # before reading it.
            parts = ctx.named_children(arg)
            value = parts[-1] if parts else None
            return (
                value is not None
                and ctx.kind(value) == "string"
                and "b" in ctx.text(value)
            )

        positional += 1
        # First positional is the path, second is the mode.
        if positional == 2:
            return kind == "string" and "b" in ctx.text(arg)

    return False


def handlers():
    return Handlers(metadata=metadata, check=check)
