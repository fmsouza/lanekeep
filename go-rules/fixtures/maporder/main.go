// Command maporder is a rule that reports the order it visited a map in.
//
// It exists for one assertion, made in `crates/lanekeep-wasm/tests/go_map_order.rs`: **a Go
// rule's map iteration order does not depend on how much work preceded it on that instance.**
// Nothing here is a rule anyone would ship, and the artifact it builds is a test fixture rather
// than a built-in — `crates/lanekeep-wasm/tests/fixtures/go-maporder.wasm`, built by
// `just go-rules` beside the component that does ship.
//
// # The hazard, and why a fixture is the only way to see it
//
// TinyGo randomizes map iteration. `src/runtime/hashmap.go` draws from `fastrand()` three times
// per map a rule builds and walks — once for the map's hash seed (line 77, and again at line 294
// if it grows), and twice for an iterator's start bucket and start index (lines 402-403). On
// `-target=wasm-unknown` that generator is seeded to a constant, because `hardwareRand` returns
// `(0, false)`, so there is no entropy anywhere in the build.
//
// A fixed seed is not a fixed answer. `fastrand` is a xorshift32 over a single global that
// *advances*, and iteration order depends on the position in that cycle rather than on the seed
// — so the order a map is walked in is a function of how many draws happened before it. Put that
// beside the instance lifetime `crates/lanekeep-wasm/wit/world.wit` fixes, **one instance per
// (worker, component)** shared across every rule it hosts and every file that worker handles,
// and the draw count standing before any given `check` becomes a rayon work-stealing artifact.
// That is scheduling-dependent output with every cache-key input identical, which is exactly
// what the determinism invariant forbids.
//
// `go-rules/lanekeep`'s [lanekeep.Handlers] closes it by pinning both generators to their
// initial position at the top of every host-called path. No rule can see that state and no
// review of rule source would catch its absence, so the only evidence available is behavioral:
// drive one instance repeatedly and watch the reported order hold still.
//
// # Three passes per call, which is what stops the test passing vacuously
//
// [visits] builds and walks the same map three times and reports all three orders. The middle
// claim of the test rests on it: if those three agree, then this fixture's observable does *not*
// move with the generator position, the reset has nothing to do, and the outer assertion is
// green whether or not [lanekeep.ResetRand] does anything at all. So the Rust test asserts they
// disagree, and that assertion is the fixture's own sensitivity check rather than a property of
// the SDK.
//
// Three fresh maps rather than three walks of one, because the seed is drawn per map and it is
// the seed that decides which bucket a key lands in. And all three orders reach the reported
// message, which is what keeps LLVM from deleting two of them: on this target an unobserved
// loop is removed outright, a trap `crates/lanekeep-wasm/tests/fixtures/limits/` and
// `.../engine-rule/` both carry notes about.
//
// # Eight keys, and the number is chosen rather than round
//
// `hashmapOverLoadFactor` grows while `n > 6 << bucketBits`, so a size hint of eight gives
// `bucketBits = 1` — two buckets — where anything up to six gives one. Both halves of the
// variation are live only above that line. With a single bucket `startBucket` is `fastrand() & 0`
// and is always zero, and the seed cannot move a key either, because every key is in the one
// bucket in insertion order; the whole order is then a rotation by `startIndex`, one of *n*, and
// nothing else about the generator can reach it.
//
// Measured, on the neutered build the task report records: four keys in one bucket gave `abcd`
// on three of six calls, since only four of the eight `startIndex` values land on a distinct
// rotation. Eight keys in two buckets gave a different order on every one of the six.
package main

import (
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/rule"
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
	"github.com/fmsouza/lanekeep/go-rules/lanekeep"
	"go.bytecodealliance.org/cm"
)

// The two `result` shapes the world's exports answer with, named as `go-rules/main.go` names
// them.
type (
	passResult      = cm.Result[types.RuleError, struct{}, types.RuleError]
	configureResult = cm.Result[string, struct{}, string]
)

// id is what this fixture calls itself. Namespaced like a real rule, because
// `crates/lanekeep-wasm/src/load.rs` and the config layer both treat an id as namespaced and a
// fixture that is shaped differently from the thing it stands in for tests the wrong shape.
const id = "lanekeep/map-order"

// passes is how many times [visits] builds and walks the map in one `check`.
//
// Three rather than two: two orders that happen to agree is a coincidence a reader would not
// question, and three makes "all of them agree" a claim worth asserting against.
const passes = 3

// separator is what [visits] puts between one pass's order and the next.
//
// Outside the key alphabet on purpose, so `crates/lanekeep-wasm/tests/go_map_order.rs` can split
// the message back into passes without the split depending on how long a pass is.
const separator = '|'

// keys is what the map is filled with, one byte each so that a visit order is a readable string.
//
// Eight, for the bucket-count reason in the package documentation. The *keys* are package level
// and the *map* is not: [visit] builds a fresh one every pass, because the seed drawn at
// construction is half of what makes the order move and a map built once would leave only the
// iterator's start position.
var keys = []string{"a", "b", "c", "d", "e", "f", "g", "h"}

// The lists [metadata] hands the host, at package level for the reason
// `go-rules/rules/nopackageinit/rule.go` states at length: `cm.ToList` does not copy, so the
// host reads through this pointer after the export has returned.
var (
	// TypeScript rather than Go, and that is not a slip. The harness driving this fixture lends
	// it a context over a parsed TypeScript file, exactly as `tests/navigation.rs` and
	// `tests/reads.rs` do — nothing here reads the tree, and a fixture claiming a language its
	// context is not would be a second thing to explain.
	languages = []string{"typescript"}

	// The four gates, all empty: this fixture is driven directly rather than selected by a
	// walker, so nothing consults them.
	noPatterns []string

	// The one id `rules` enumerates.
	ruleIDs = []string{id}
)

// handlers is this fixture's entry points, held the way every shipped rule holds them.
//
// **Through [lanekeep.NewHandlers], which is the whole point.** A fixture that called its own
// `check` directly would report a map order with no reset in front of it and would prove the
// opposite of what it is for. The reset is reachable no other way — `Handlers`' fields are
// unexported and this constructor is the only one — so what the test observes is the same path
// `no-package-init` and `no-context-in-struct` take.
var handlers = lanekeep.NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
	metadata, nil, check, nil,
)

// main is never called: `-target=wasm-unknown` produces a reactor. Go requires it all the same.
func main() {}

// init wires the seven exports, as `go-rules/main.go` does and for the same reason.
//
// One rule, so the index is checked rather than looked up. An index this fixture does not host
// is refused on the same terms the shipped component refuses one: through the `result` where
// there is one, and by trapping where the world left no channel.
func init() {
	rule.Exports.Rules = rules
	rule.Exports.Metadata = ruleMetadata
	rule.Exports.Configure = configure
	rule.Exports.HasCheck = hasCheck
	rule.Exports.HasReduce = hasReduce
	rule.Exports.Check = checkExport
	rule.Exports.Reduce = reduceExport
}

// rules enumerates the one rule this fixture hosts.
func rules() cm.List[string] {
	return cm.ToList(ruleIDs)
}

// ruleMetadata answers the world's `metadata`.
func ruleMetadata(index uint32) types.RuleMetadata {
	if index != 0 {
		panic(noSuchRule)
	}
	return handlers.Metadata()
}

// configure answers the world's `configure`, which for this fixture accepts anything.
func configure(index uint32, optionsJSON string) configureResult {
	if index != 0 {
		return cm.Err[configureResult](noSuchRule)
	}
	if err := handlers.Configure(optionsJSON); err != nil {
		return cm.Err[configureResult](err.Error())
	}
	return cm.OK[configureResult](struct{}{})
}

// hasCheck answers the world's `has-check`.
func hasCheck(index uint32) bool {
	if index != 0 {
		panic(noSuchRule)
	}
	return handlers.HasCheck()
}

// hasReduce answers the world's `has-reduce`.
func hasReduce(index uint32) bool {
	if index != 0 {
		panic(noSuchRule)
	}
	return handlers.HasReduce()
}

// checkExport runs the per-file pass.
//
// The `defer context.ResourceDrop()` is not optional and nothing generated supplies it:
// `wit-bindgen-go` 0.7.0 emits no `resource.drop` for a `borrow<>`, so a guest that simply uses
// the handle returns with the loan outstanding and wasmtime answers `borrow handles still remain
// at the end of the call` — after the rule body has run, so every report it made is discarded
// and the failure looks exactly like a rule that found nothing. `go-rules/main.go` carries the
// long version.
func checkExport(index uint32, ctx cm.Rep, m types.Match) passResult {
	context := cm.Reinterpret[types.CheckContext](ctx)
	defer context.ResourceDrop()

	if index != 0 {
		return failed(noSuchRule)
	}
	if err := handlers.Check(context, m.Slice()); err != nil {
		return failed(err.Error())
	}
	return cm.OK[passResult](struct{}{})
}

// reduceExport runs the cross-file pass, which this fixture does not have.
func reduceExport(index uint32, ctx cm.Rep) passResult {
	context := cm.Reinterpret[types.ReduceContext](ctx)
	defer context.ResourceDrop()

	if index != 0 {
		return failed(noSuchRule)
	}
	if err := handlers.Reduce(context); err != nil {
		return failed(err.Error())
	}
	return cm.OK[passResult](struct{}{})
}

// noSuchRule is what an index this fixture does not host is refused with.
const noSuchRule = "no rule at index other than 0: this fixture hosts exactly one"

// failed is a graceful failure of `check` or `reduce`.
func failed(message string) passResult {
	return cm.Err[passResult](types.RuleError{Message: message})
}

// metadata is what this fixture says it is.
func metadata() types.RuleMetadata {
	return types.RuleMetadata{
		ID:        id,
		Languages: cm.ToList(languages),
		Severity:  "error",
		Card: types.RuleCard{
			Message:     "a map was visited in this order",
			Remediation: "nothing: this is a fixture, and the message is the observation",
			Examples: types.RuleExamples{
				Bad:  "for k := range m { }",
				Good: "for _, k := range sorted(m) { }",
			},
		},
		// Never compiled and never run: the harness hands `check` a match it built itself, which
		// is what lets one call be one observation. A syntactically real query all the same, so
		// that nothing downstream has to special-case this fixture.
		Query: "(program) @file",
		Gates: types.RuleGates{
			PathMatches:     cm.ToList(noPatterns),
			PathNotMatches:  cm.ToList(noPatterns),
			FileContains:    cm.ToList(noPatterns),
			FileNotContains: cm.ToList(noPatterns),
		},
		Timeout: cm.None[uint64](),
	}
}

// check reports the order [visits] observed, at the root.
//
// The root rather than a captured node, because the node is not what this fixture is about and a
// capture it could fail to find would be a second way for the call to report nothing — which is
// indistinguishable, from the host, from a map order that came out empty.
func check(ctx types.CheckContext, _ lanekeep.Match) error {
	ctx.Report(rootNode, cm.Some(visits()), cm.None[types.Fix]())
	return nil
}

// rootNode is the handle every report here is made at.
//
// Zero, and spelled as a named constant because zero is a *valid* handle rather than a stand-in
// for none — the trap `lanekeep.Capture`'s second return value exists for.
const rootNode types.Node = 0

// visits builds and walks the map [passes] times, and joins what it saw.
//
// The three orders are separated by [separator] and every one of them reaches the returned
// string; see the package documentation for why both of those are load-bearing.
func visits() string {
	out := make([]byte, 0, passes*(len(keys)+1))
	for pass := range passes {
		if pass > 0 {
			out = append(out, separator)
		}
		out = append(out, visit()...)
	}
	return string(out)
}

// visit builds a map over [keys] and returns the first byte of each key, in iteration order.
//
// A fresh map every call. Reusing one would drop the hash seed from what varies and leave only
// the iterator's start position, which is the weaker half.
func visit() []byte {
	m := make(map[string]struct{}, len(keys))
	for _, key := range keys {
		m[key] = struct{}{}
	}

	order := make([]byte, 0, len(m))
	for key := range m {
		order = append(order, key[0])
	}
	return order
}
