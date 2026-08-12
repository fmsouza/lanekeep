"""A component that is not a rule, for the determinism test.

Reports `hash('lanekeep')`, a set's iteration order, and the same set's
`sorted()` order. `just py-rules` builds it twice from identical source; the two
artifacts differ in the first two observables and agree on the third, which is
the set-iteration hazard and its mitigation. See
`crates/lanekeep-wasm/tests/python_determinism.rs`.
"""

import wit_world
from wit_world.imports import types

WORDS = {
    "lanekeep",
    "python",
    "determinism",
    "cache",
    "wasm",
    "sandbox",
    "engine",
    "rule",
}


class WitWorld(wit_world.WitWorld):
    def rules(self):
        return ["determinism-probe"]

    def metadata(self, index):
        return types.RuleMetadata(
            id="determinism-probe",
            languages=["python"],
            severity="error",
            card=types.RuleCard(
                message="probe",
                remediation="none",
                examples=types.RuleExamples(bad="x", good="y"),
            ),
            query="(identifier) @id",
            gates=types.RuleGates(
                path_matches=[],
                path_not_matches=[],
                file_contains=[],
                file_not_contains=[],
            ),
            timeout=None,
        )

    def configure(self, index, options_json):
        pass

    def has_check(self, index):
        return True

    def has_reduce(self, index):
        return False

    def check(self, index, ctx, m):
        ctx.report(0, f"hash={hash('lanekeep')}", None)
        ctx.report(0, f"set-order={list(WORDS)}", None)
        ctx.report(0, f"sorted={sorted(WORDS)}", None)

    def reduce(self, index, ctx):
        pass
