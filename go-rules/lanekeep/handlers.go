package lanekeep

import "errors"

// Handlers is one rule's two passes, held so that [ResetRand] runs before either of them.
//
// # Why the fields are unexported
//
// The determinism argument in the package documentation only holds if the reset happens on
// *every* invocation, and the first version of this SDK asked each rule author to remember to
// call [ResetRand] themselves. That is documentation standing in for enforcement, which the
// design authority explicitly rejected as unenforceable for this hazard — and it is a weaker
// guard than the rest of the sandbox gets, where a capability is withheld rather than
// discouraged. An author who forgets produces a rule that is correct on every test anyone
// would think to write and scheduling-dependent under a real corpus, because the state at
// fault lives in TinyGo's runtime rather than in the rule.
//
// So the handler a rule declares is reachable only through [Handlers.Check] and
// [Handlers.Reduce], which reset first and then delegate. There is no accessor that skips the
// reset and [NewHandlers] is the only constructor, so omitting it is not a mistake an author
// can make. This does not make the hazard unnecessary to understand — a rule that keeps its
// own long-lived state still has to think about it — but it makes the common path correct by
// construction rather than by recollection.
//
// # Why it is generic
//
// C and R are the context types, and this type never looks inside them; it only hands them
// back to the handler. Naming the generated `types.CheckContext` here instead would put an
// `internal/` type in an exported signature, which would stop an out-of-tree rule author —
// who generates their own bindings, and whose `CheckContext` is a different Go type from this
// module's — from using the SDK at all. It would also stop anything outside this module from
// testing the wrapper, which is how the omission would go unnoticed.
//
// Inference fills both parameters in when both handlers are given. A rule with only one pass
// names them, because an untyped nil carries nothing to infer from:
//
//	lanekeep.NewHandlers[types.CheckContext, types.ReduceContext](check, nil)
type Handlers[C, R any] struct {
	check  func(C, Match) error
	reduce func(R) error
}

// NewHandlers declares one rule's passes. Either may be nil; a rule with no cross-file pass
// passes nil for reduce, and the component entry reports that through `has-reduce`.
//
// The world's `check` and `reduce` return `result<_, rule-error>`; a plain Go error is the
// natural spelling of that on this side, and the component entry converts it.
func NewHandlers[C, R any](check func(C, Match) error, reduce func(R) error) Handlers[C, R] {
	return Handlers[C, R]{check: check, reduce: reduce}
}

// HasCheck reports whether this rule has a per-file pass, answering the world's `has-check`.
func (h Handlers[C, R]) HasCheck() bool { return h.check != nil }

// HasReduce reports whether this rule has a cross-file pass, answering `has-reduce`.
func (h Handlers[C, R]) HasReduce() bool { return h.reduce != nil }

// Check runs the per-file pass for one query match, after resetting the map-iteration
// generators. See [Handlers] for why that ordering is structural rather than a convention.
//
// Calling this on a rule that declared no check answers with an error rather than trapping:
// `check` has an error channel, and a host that dispatched without consulting `has-check`
// should be told which of the two disagreed.
func (h Handlers[C, R]) Check(ctx C, m Match) error {
	ResetRand()
	if h.check == nil {
		return errors.New("this rule has no per-file pass: `has-check` reports false for it")
	}
	return h.check(ctx, m)
}

// Reduce runs the cross-file pass, after the same reset and on the same terms as
// [Handlers.Check].
func (h Handlers[C, R]) Reduce(ctx R) error {
	ResetRand()
	if h.reduce == nil {
		return errors.New("this rule has no cross-file pass: `has-reduce` reports false for it")
	}
	return h.reduce(ctx)
}
