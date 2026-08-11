// Package lanekeep is the SDK every Go-authored rule in this module shares.
//
// Four things, deliberately, and nothing else: a name lookup over a query match ([Capture]),
// a glob matcher ([GlobMatches]), a reset for TinyGo's map-iteration randomness
// ([ResetRand]), and the `cabi_realloc` export the component model requires and TinyGo does
// not supply on this target. The first two mirror `rust-rules/lanekeep-rule`; the last two
// are TinyGo's tax, and they are here so that no rule author meets either of them as an error
// message.
//
// This package is not itself a WebAssembly component. It has no world of its own and exports
// none of the seven functions in `crates/lanekeep-wasm/wit/world.wit` — a rule package that
// is a component imports this one the way it would import any other library, and the
// generated bindings under `internal/` are shared by both.
//
// # Determinism, and the runtime state a rule author cannot see
//
// lanekeep's central invariant is that a run is deterministic given
// `(bytes, path, ruleset, config, tracked reads)`. TinyGo makes that harder than it looks in
// a way nothing in a rule's own source reveals.
//
// TinyGo randomizes map iteration order: `src/runtime/hashmap.go` seeds an iterator's start
// bucket and start index from `fastrand()`, a xorshift over a single package-level global
// that advances on every draw. On `-target=wasm-unknown` there is no entropy to seed it with
// — `hardwareRand()` is `return 0, false` — so the sequence is fixed, and three separate
// processes produce byte-identical map orders. That sounds like the end of the problem and is
// not, because iteration order depends on the *position* in the sequence, not on the seed:
//
//	prior draws  resulting map order
//	0            [3809771, 16096528, 16096528, 13700953, ...]
//	1            [3809771, 13700953, 16096528, 16096528, ...]
//	4            [13700953, 16096528, 16096528, 6767677, ...]
//
// Now place that beside the instance lifetime `world rule` fixes: the host instantiates once
// per (worker, component), shared across every rule that component hosts and every file that
// worker handles. Every map a Go rule builds or iterates advances the shared counter, so the
// number of draws standing before any given check is a function of rayon's work-stealing
// schedule. A Go rule that iterates a map would therefore produce scheduling-dependent
// output, on a fixed seed, with every cache-key input identical.
//
// Sorting violations by `(ruleId, file, line, column)` does not rescue this. A rule that
// picks *which* node to report by map order reports a different violation, not the same one
// in a different position.
//
// [ResetRand] closes it by returning the generators to their initial position, which makes map
// order a function of that call's inputs alone. It is the posture the QuickJS sandbox already
// takes with `Math.random` and `Date.now`: withhold the capability rather than ask authors not
// to reach for it. Documenting "sort before you iterate" was rejected as unenforceable, and
// refusing `range` over a map was rejected as costing Go authors a normal language feature the
// reset already makes safe.
//
// # The reset is structural, not a rule an author follows
//
// Nothing here asks an author to remember anything. A rule declares its passes through
// [NewHandlers], whose [Handlers.Check] and [Handlers.Reduce] reset before delegating, and the
// declared handler is unreachable except through them — the fields are unexported and there is
// no other constructor. So the reset happens on every invocation by construction.
//
// An earlier version of this package did ask, in a section here headed "what a rule author has
// to remember". That was documentation standing in for enforcement of exactly the hazard the
// design authority had already rejected documentation for, and it would have failed in the
// quietest possible way: a rule that omitted the call passes every test anyone would write and
// misbehaves only under a real corpus, on a schedule, with every cache-key input identical.
//
//	func check(ctx types.CheckContext, m types.Match) error {
//		node, ok := lanekeep.Capture(m.Slice(), "name")
//		...
//	}
//
//	var rule = lanekeep.NewHandlers(check, nil)  // the reset comes with it
//
// [ResetRand] stays exported because the fixtures that prove it need to drive it directly, not
// because a rule should call it.
//
// One sharp edge belongs to the generated bindings rather than to this package, and it will
// hit every author on their first optional-returning call: `cm.Option[T]`'s `Some` is a
// pointer method, so `ctx.Text(node).Some()` does not compile. Bind the option to a variable
// first.
package lanekeep
