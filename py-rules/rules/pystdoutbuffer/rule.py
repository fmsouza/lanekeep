"""local/py-stdout-buffer, authored in Python.

A port of `lanekeep/rules/py-stdout-buffer.ts`. Text written to `sys.stdout`
rather than to `sys.stdout.buffer` fails on Windows in two distinct ways:
`sys.stdout` encodes with the locale codec (cp1252), so any text carrying a
non-ASCII byte dies with `UnicodeEncodeError` partway through — the output is
*truncated* at the first non-ASCII character, which is what makes it quiet —
and it translates newlines, so a value read back by a shell script arrives
carrying a carriage return. `sys.stdout.buffer.write` of raw bytes avoids both
at once.
"""

from wit_world.imports import types

from lanekeep import Handlers, capture

ID = "local/py-stdout-buffer"


def metadata():
    return types.RuleMetadata(
        id=ID,
        languages=["python"],
        severity="error",
        card=types.RuleCard(
            message="text written to sys.stdout",
            remediation=(
                "write bytes through `sys.stdout.buffer.write`, which neither "
                "re-encodes nor translates newlines"
            ),
            examples=types.RuleExamples(
                bad='sys.stdout.write(text)',
                good='sys.stdout.buffer.write(text.encode("utf-8"))',
            ),
        ),
        gates=types.RuleGates(
            path_matches=[],
            path_not_matches=[],
            file_contains=["sys.stdout"],
            file_not_contains=[],
        ),
        queries=[types.QueryFor(language="python", query="""
            (call function: (attribute object: (_) @obj attribute: (identifier) @method)) @call
        """)],
        timeout=None,
    )


def check(ctx, m):
    obj = capture(m, "obj")
    method = capture(m, "method")
    call = capture(m, "call")
    if obj is None or method is None or call is None:
        return

    if ctx.text(method) != "write":
        return
    if ctx.text(obj) != "sys.stdout":
        return

    ctx.report(
        call,
        "sys.stdout encodes with the locale codec, which on Windows truncates the output at the first non-ASCII byte",
        None,
    )


def handlers():
    return Handlers(metadata=metadata, check=check)
