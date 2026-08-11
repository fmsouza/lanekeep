"""The WebAssembly component the Python-authored rules ship in.

The Python counterpart of `go-rules/main.go`: a table of the rules this
component hosts, and the seven exports of the `rule` world dispatched over it.
`just py-rules` builds it with `componentize-py --stub-wasi`; the artifact is
not committed because the build is not byte-reproducible.

# One component for every Python rule, and not one each

A Python component carries a CPython runtime, so the marginal rule is far
cheaper than the first — measured ~5.1 KB inside an 18.5 MB component. Rules go
in [RULESET] in the order they are to be enumerated, and the index a rule sits
at is what the exports dispatch on.
"""

import wit_world
from componentize_py_types import Err
from wit_world.imports import types

from rules.nomutabledefaultargument import rule as nomutabledefaultargument
from rules.nobroadexcept import rule as nobroadexcept

# Ordered by id, matching the convention `crates/lanekeep-rules`' COMPONENT_RULES
# uses. The index a rule sits at is what every export but `rules` dispatches on.
RULESET = [
    (nobroadexcept.ID, nobroadexcept.handlers()),
    (nomutabledefaultargument.ID, nomutabledefaultargument.handlers()),
]


class WitWorld(wit_world.WitWorld):
    def rules(self):
        return [rule_id for rule_id, _ in RULESET]

    def metadata(self, index):
        return RULESET[index][1].metadata()

    def configure(self, index, options_json):
        try:
            RULESET[index][1].configure(options_json)
        except Exception as e:  # noqa: BLE001 - the entry converts graceful failures
            raise Err(str(e))

    def has_check(self, index):
        return RULESET[index][1].has_check()

    def has_reduce(self, index):
        return RULESET[index][1].has_reduce()

    def check(self, index, ctx, m):
        try:
            RULESET[index][1].check(ctx, m)
        except Exception as e:  # noqa: BLE001 - the entry converts graceful failures
            raise Err(types.RuleError(message=str(e), frames=[]))

    def reduce(self, index, ctx):
        try:
            RULESET[index][1].reduce(ctx)
        except Exception as e:  # noqa: BLE001 - the entry converts graceful failures
            raise Err(types.RuleError(message=str(e), frames=[]))
