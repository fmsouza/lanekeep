package lanekeep

import "errors"

// Handlers is one rule's four entry points, held so that [ResetRand] runs before each of them.
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
// So a rule's own funcs are reachable only through the four methods below, which reset first
// and then delegate. There is no accessor that skips the reset and [NewHandlers] is the only
// constructor, so omitting it is not a mistake an author can make. This does not make the
// hazard unnecessary to understand — a rule that keeps its own long-lived state still has to
// think about it — but it makes the common path correct by construction rather than by
// recollection.
//
// # All four, not only the two passes
//
// `metadata` and `configure` were bare funcs on the component entry until a review asked why.
// Nothing was wrong with either rule that ships — both build their metadata from package-level
// constants — but the exposure is real and it is the same one: a rule whose `configure` decodes
// options into a `map[string]any` and builds a list by ranging it gets an order that depends on
// how many draws the shared generator has already taken, and one instance is shared per
// (worker, component) across every file that worker handles, so that count is a rayon
// scheduling artifact rather than anything the rule can see.
//
// Leaving two of the four paths unguarded and writing down why would have been the same
// substitution of documentation for enforcement that this type exists to undo. The cost of
// covering them is one store to each of two globals per call, on calls that happen once per
// rule per instance.
//
// # Why it is generic
//
// M, C and R are the metadata and context types, and this type never looks inside them; it only
// hands the contexts back to the handler and the metadata back to the host. Naming the generated
// `types.RuleMetadata` here instead would put an `internal/` type in an exported signature, which
// would stop an out-of-tree rule author — who generates their own bindings, and whose types are
// different Go types from this module's — from using the SDK at all. It would also stop anything
// outside this module from testing the wrapper, which is how the omission would go unnoticed.
//
// Nothing infers from an untyped nil, and a rule with no cross-file pass passes one, so all three
// parameters are named at the call site in practice:
//
//	lanekeep.NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
//		Metadata, nil, check, nil,
//	)
type Handlers[M, C, R any] struct {
	metadata  func() M
	configure func(string) error
	check     func(C, Match) error
	reduce    func(R) error
}

// NewHandlers declares one rule's entry points.
//
// `metadata` is the one that is not optional: the world has no optional exports and the host
// reads a rule's metadata at prepare time, so a rule without it cannot load. The other three may
// be nil — a rule with no cross-file pass passes nil for `reduce`, and the entry reports that
// through `has-reduce`; a rule with no options passes nil for `configure`, and [Handlers.Configure]
// accepts on its behalf.
//
// The world's `check` and `reduce` return `result<_, rule-error>` and `configure` returns
// `result<_, string>`; a plain Go error is the natural spelling of all three on this side, and
// the component entry converts them.
func NewHandlers[M, C, R any](
	metadata func() M,
	configure func(string) error,
	check func(C, Match) error,
	reduce func(R) error,
) Handlers[M, C, R] {
	return Handlers[M, C, R]{
		metadata:  metadata,
		configure: configure,
		check:     check,
		reduce:    reduce,
	}
}

// Metadata is what the rule says it is, after the reset. See [Handlers] for why the reset is
// here rather than at each rule's discretion.
//
// A rule that declared none panics, which on this target is a trap. That is the honest answer
// and not a shortcut: `metadata` is the one export with no error channel *and* no sensible
// default — a blank record would be a rule with no id and no query, which the host would load
// and then never fire, silently. See the component entry's `noSuchRule` for the same reasoning
// applied to the other channel-less exports.
func (h Handlers[M, C, R]) Metadata() M {
	ResetRand()
	if h.metadata == nil {
		panic("this rule declared no metadata: `NewHandlers` takes it first, and the world has no optional exports")
	}
	return h.metadata()
}

// Configure hands the rule its options as JSON, after the same reset.
//
// A rule that declared no `configure` **accepts** rather than refuses, and that is a migration
// constraint rather than a preference. The TypeScript rules these components replace export a
// plain object, so a config naming one with options today is accepted and the options ignored; a
// component that refused would turn a working config into a failed run at the moment the
// built-in is swapped over, which is not a change that belongs inside a migration.
func (h Handlers[M, C, R]) Configure(optionsJSON string) error {
	ResetRand()
	if h.configure == nil {
		return nil
	}
	return h.configure(optionsJSON)
}

// HasCheck reports whether this rule has a per-file pass, answering the world's `has-check`.
func (h Handlers[M, C, R]) HasCheck() bool { return h.check != nil }

// HasReduce reports whether this rule has a cross-file pass, answering `has-reduce`.
func (h Handlers[M, C, R]) HasReduce() bool { return h.reduce != nil }

// Check runs the per-file pass for one query match, after resetting the map-iteration
// generators. See [Handlers] for why that ordering is structural rather than a convention.
//
// Calling this on a rule that declared no check answers with an error rather than trapping:
// `check` has an error channel, and a host that dispatched without consulting `has-check`
// should be told which of the two disagreed.
func (h Handlers[M, C, R]) Check(ctx C, m Match) error {
	ResetRand()
	if h.check == nil {
		return errors.New("this rule has no per-file pass: `has-check` reports false for it")
	}
	return h.check(ctx, m)
}

// Reduce runs the cross-file pass, after the same reset and on the same terms as
// [Handlers.Check].
func (h Handlers[M, C, R]) Reduce(ctx R) error {
	ResetRand()
	if h.reduce == nil {
		return errors.New("this rule has no cross-file pass: `has-reduce` reports false for it")
	}
	return h.reduce(ctx)
}
