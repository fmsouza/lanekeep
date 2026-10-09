"""Patch upstream tree-sitter-typescript's resolved grammar.json into lanekeep's.

usage: patch.py (typescript|tsx) UPSTREAM_GRAMMAR_JSON OUT_GRAMMAR_JSON

Three changes, each asserted to apply exactly once so a different upstream fails here rather
than producing a grammar that silently lacks one of them:

1. `export type * from '...'` and `export type * as ns from '...'` (TypeScript 5.0). Two
   `export_statement` alternatives, the ones upstream pull request #360 proposes.

2. `f<typeof import('m')>()` (upstream issue #367). Upstream lists
   `[call_expression, _type_query_call_expression]` in `precedences`, which resolves the
   reduce/reduce after `f < typeof import('m')` statically, toward the expression, so the
   type-argument reading is dropped before the `>` that would decide it. Removing the entry and
   declaring the pair a conflict keeps both readings alive for the GLR parser. Declaring the
   conflict while keeping the precedence entry changes nothing: the precedence wins first.

3. The grammar's name, so the generated C symbols (`tree_sitter_lanekeep_typescript`, ...) cannot
   collide with upstream's `tree_sitter_typescript` in a binary that links both crates. A static
   archive's duplicate symbol is not an error; the linker takes whichever it reads first.

Stdlib only, and applied to grammar.json rather than to `define-grammar.js`: generating from
the JavaScript needs Node and tree-sitter-javascript's own grammar.js at a matching version,
while generating from the JSON is what `regenerate.sh` checks is byte-reproducible.
"""

import json
import sys

NAMES = {"typescript": "lanekeep_typescript", "tsx": "lanekeep_tsx"}


def string(value):
    return {"type": "STRING", "value": value}


def symbol(name):
    return {"type": "SYMBOL", "name": name}


def main():
    language, source, destination = sys.argv[1:]
    with open(source, encoding="utf-8") as handle:
        grammar = json.load(handle)

    if grammar["name"] != language:
        sys.exit(f"expected the {language} grammar, found {grammar['name']!r}")
    grammar["name"] = NAMES[language]

    alternatives = grammar["rules"]["export_statement"]["members"]
    export_type_clause = [
        string("export"),
        string("type"),
        symbol("export_clause"),
    ]
    anchors = [
        i
        for i, alternative in enumerate(alternatives)
        if alternative.get("type") == "SEQ"
        and alternative["members"][:3] == export_type_clause
    ]
    if len(anchors) != 1:
        sys.exit(f"expected one `export type {{...}}` alternative, found {len(anchors)}")
    alternatives[anchors[0] + 1 : anchors[0] + 1] = [
        {
            "type": "SEQ",
            "members": [
                string("export"),
                string("type"),
                string("*"),
                symbol("_from_clause"),
                symbol("_semicolon"),
            ],
        },
        {
            "type": "SEQ",
            "members": [
                string("export"),
                string("type"),
                symbol("namespace_export"),
                symbol("_from_clause"),
                symbol("_semicolon"),
            ],
        },
    ]

    pair = [symbol("call_expression"), symbol("_type_query_call_expression")]
    kept = [entry for entry in grammar["precedences"] if entry != pair]
    if len(kept) != len(grammar["precedences"]) - 1:
        sys.exit("expected exactly one [call_expression, _type_query_call_expression] precedence")
    grammar["precedences"] = kept

    conflict = ["call_expression", "_type_query_call_expression"]
    if conflict in grammar["conflicts"]:
        sys.exit("upstream already declares the conflict; this patch is out of date")
    grammar["conflicts"].append(conflict)

    with open(destination, "w", encoding="utf-8") as handle:
        json.dump(grammar, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


if __name__ == "__main__":
    main()
