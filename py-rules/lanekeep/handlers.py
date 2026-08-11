"""The rule declaration: four entry points, dispatched by the component entry.

The Python counterpart of `go-rules/lanekeep`'s `Handlers`. It is deliberately
type-agnostic — it never names the generated bindings' types — so its tests run
on the host with plain Python, and an out-of-tree rule author whose bindings are
their own can use it.
"""


class Handlers:
    """One rule's four entry points, as the component entry dispatches them.

    `metadata` is required — the host reads it at prepare time, and a rule
    without it cannot load. The other three may be `None`; `has_check` and
    `has_reduce` answer from the same values the dispatch uses, so what the
    component says about itself and what it does cannot disagree.
    """

    def __init__(self, metadata, configure=None, check=None, reduce=None):
        self._metadata = metadata
        self._configure = configure
        self._check = check
        self._reduce = reduce

    def metadata(self):
        return self._metadata()

    def configure(self, options_json):
        if self._configure is None:
            return None
        return self._configure(options_json)

    def has_check(self):
        return self._check is not None

    def has_reduce(self):
        return self._reduce is not None

    def check(self, ctx, m):
        if self._check is None:
            raise RuntimeError(
                "this rule has no per-file pass: `has_check` reports false for it"
            )
        return self._check(ctx, m)

    def reduce(self, ctx):
        if self._reduce is None:
            raise RuntimeError(
                "this rule has no cross-file pass: `has_reduce` reports false for it"
            )
        return self._reduce(ctx)
