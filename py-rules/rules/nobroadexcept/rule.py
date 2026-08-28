"""lanekeep/no-broad-except, authored in Python.

A port of `crates/lanekeep-rules/rules/no-broad-except.ts`. The two Python
built-ins cannot ship from `lanekeep-rules` (crates.io's 10 MiB cap), so this
lives here as the proof that the lane handles a real rule. The fidelity test in
`crates/lanekeep-rules/tests/python_rules.rs` holds it to the TypeScript
original's cases.
"""

from wit_world.imports import types

from lanekeep import Handlers, capture

ID = "lanekeep/no-broad-except"


def metadata():
    return types.RuleMetadata(
        id=ID,
        languages=["python"],
        severity="error",
        card=types.RuleCard(
            message="except catches too much",
            remediation=(
                "name the exceptions this block can actually raise, so an "
                "unexpected one still surfaces"
            ),
            examples=types.RuleExamples(
                bad="try:\n    parse(raw)\nexcept Exception:\n    return None",
                good="try:\n    parse(raw)\nexcept ValueError:\n    return None",
            ),
        ),
        gates=types.RuleGates(
            path_matches=[],
            path_not_matches=[],
            file_contains=["except"],
            file_not_contains=[],
        ),
        queries=[types.QueryFor(language="python", query="(except_clause) @clause")],
        timeout=None,
    )


def check(ctx, m):
    clause = capture(m, "clause")
    if clause is None:
        return

    children = ctx.named_children(clause)
    caught = children[0] if children else None

    # `except:` — no exception named at all. The first named child of a bare
    # clause is its block, because `except` itself is an anonymous token.
    if caught is None or ctx.kind(caught) == "block":
        ctx.report(
            clause,
            "bare `except:` also catches KeyboardInterrupt and SystemExit",
            None,
        )
        return

    # `except Exception:` and `except Exception as e:`, which wraps the name in
    # a pattern.
    if ctx.kind(caught) == "as_pattern":
        named_children = ctx.named_children(caught)
        named = named_children[0] if named_children else None
    else:
        named = caught
    if named is None or ctx.kind(named) != "identifier":
        return

    name = ctx.text(named)
    if name != "Exception" and name != "BaseException":
        return

    # Locally defined or imported under that name — a different exception
    # entirely.
    if ctx.binding_kind(named) is not None:
        return

    ctx.report(
        clause,
        f"`except {name}` catches every error the block can raise, including bugs",
        None,
    )


def handlers():
    return Handlers(metadata=metadata, check=check)
