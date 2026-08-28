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
// # All four host-called paths, because covering one of them covered one of them
//
// [lanekeep.Handlers] resets on `metadata`, `configure`, `check` and `reduce` alike. This fixture
// used to declare `metadata` from constants, no `configure` at all and no `reduce`, and reported
// a map order from `check` alone — so deleting the reset from any of the other three was caught
// by nothing, while the Rust test beside it claimed a wasm probe caught all four.
//
// Each of the four therefore reports an order of its own now, through whichever channel that
// export has:
//
//   - `check` reports it as a violation message, which is the original probe.
//   - `metadata` reports it as its card's message, which is the only string on that export a
//     host reads back verbatim.
//   - `configure` has no return value at all, so it stores what it saw in [configured] and the
//     next `check` carries it out as the first field of its message.
//   - `reduce` reports it as a cross-file violation message.
//
// # Two rules, because one rule cannot exercise `configure` after work
//
// `configure` runs once per (rule, instance), before that rule's first `check`. On a component
// hosting one rule it is therefore always the first thing called on a fresh instance, where the
// generator is at its initial position whether or not anything reset it — so the reset is
// invisible and a probe against one rule proves nothing.
//
// The hazard is real for a component hosting several: `crates/lanekeep-wasm/src/runtime.rs`
// configures a rule lazily, on the way to its first use, and one instance serves every rule its
// component hosts. So rule B's `configure` runs on an instance rule A has already checked many
// files on. Two rules here is what lets the test stand in that position: configure the first,
// work it, then reach the second and compare what its `configure` saw.
//
// Both rules are the same rule, differing only in id. Nothing here is about what a rule does.
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
	"strconv"

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

// The ids this fixture's two rules call themselves.
//
// Namespaced like a real rule, because `crates/lanekeep-wasm/src/load.rs` and the config layer
// both treat an id as namespaced and a fixture that is shaped differently from the thing it
// stands in for tests the wrong shape. Sorted, as `go-rules/main.go`'s own table is.
const (
	firstID = "lanekeep/map-order"
	lateID  = "lanekeep/map-order-late"
)

// passes is how many times [visits] builds and walks the map in one call.
//
// Three rather than two: two orders that happen to agree is a coincidence a reader would not
// question, and three makes "all of them agree" a claim worth asserting against.
const passes = 3

// separator is what [visits] puts between one pass's order and the next.
//
// Outside the key alphabet on purpose, so `crates/lanekeep-wasm/tests/go_map_order.rs` can split
// a message back into fields without the split depending on how long a field is.
const separator = '|'

// section is what [check] puts between the whole of what `configure` observed and the whole of
// what it observed itself.
//
// A second character rather than another [separator], because both halves are [passes] orders
// long: one separator would give a flat list of six fields whose split point a reader has to
// count out, and a reader who counted wrong would be splitting a correct message in the wrong
// place with nothing to say so.
const section = '#'

// keys is what the map is filled with, one byte each so that a visit order is a readable string.
//
// Eight, for the bucket-count reason in the package documentation. The *keys* are package level
// and the *map* is not: [visit] builds a fresh one every pass, because the seed drawn at
// construction is half of what makes the order move and a map built once would leave only the
// iterator's start position.
var keys = []string{"a", "b", "c", "d", "e", "f", "g", "h"}

// The lists [metadataFor] hands the host, at package level rather than built per call.
//
// **Not a micro-optimization — a lifetime.** `cm.ToList` does not copy: it takes the slice's
// data pointer and length, and the host reads through that pointer after the export has
// returned. `go-rules/rules/nopackageinit/rule.go` sets this out at length.
var (
	// TypeScript rather than Go, and that is not a slip. The harness driving this fixture lends
	// it a context over a parsed TypeScript file, exactly as `tests/navigation.rs` and
	// `tests/reads.rs` do — nothing here reads the tree, and a fixture claiming a language its
	// context is not would be a second thing to explain.
	languages = []string{"typescript"}

	// The four gates, all empty: this fixture is driven directly rather than selected by a
	// walker, so nothing consults them.
	noPatterns []string
)

// configured is the visit order the most recent `configure` observed.
//
// `configure` answers with a `result<_, string>` whose success arm carries nothing, so there is
// no return value for it to report an order through. It leaves it here instead and [check]
// carries it out — which is enough, because what the test compares is one `configure`'s
// observation against another's rather than against anything a single call returns.
//
// One variable for both rules rather than one each. The test reads it through a `check` that it
// sequences immediately after the `configure` it is asking about, so a second slot would buy
// nothing but a way for the two to be read out of order.
var configured string

// metadataOrder is the visit order the most recent `metadata` observed, held at package level.
//
// The string is handed back inside a `rule-metadata`, which the host reads through *after* the
// export returns — the same lifetime rule the `cm.ToList` note above states, and the reason a
// local would be the one shape here that could dangle. Keeping the live reference is free and
// removes the question.
var metadataOrder string

// hosted is one rule as this fixture holds it, exactly as `go-rules/main.go` holds one.
//
// Every callable embedded rather than stored as a func, so that there is no reset-free way to
// reach the rule's own function underneath. A fixture that called its own `check` directly would
// report a map order with no reset in front of it and would prove the opposite of what it is for.
type hosted struct {
	id string

	lanekeep.Handlers[types.RuleMetadata, types.CheckContext, types.ReduceContext]
}

// ruleset is the two rules this fixture hosts, in the order `rules` reports them.
//
// Both are the same rule. The second exists so that its `configure` can be reached late, on an
// instance the first has already worked; see the package documentation.
var ruleset = []hosted{
	{id: firstID, Handlers: handlersFor(firstID)},
	{id: lateID, Handlers: handlersFor(lateID)},
}

// handlersFor builds one rule's entry points.
//
// **Through [lanekeep.NewHandlers], which is the whole point.** The reset is reachable no other
// way — `Handlers`' fields are unexported and this constructor is the only one — so what the test
// observes is the same path `no-package-init` and `no-context-in-struct` take.
//
// All four are declared, including the two this fixture would otherwise have left nil: a nil
// `configure` or `reduce` still resets before answering on the rule's behalf, but it answers
// without observing anything, so the reset in front of it would be unprobed.
func handlersFor(id string) lanekeep.Handlers[types.RuleMetadata, types.CheckContext, types.ReduceContext] {
	return lanekeep.NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		metadataFor(id), configure, check, reduce,
	)
}

// ruleIDs is [ruleset]'s ids, in its order, built once and held at package level for the
// lifetime reason the `cm.ToList` note above gives.
var ruleIDs = identify(ruleset)

// identify is [ruleIDs]'s initializer, split out so it can be a loop.
func identify(rules []hosted) []string {
	ids := make([]string, len(rules))
	for i := range rules {
		ids[i] = rules[i].id
	}
	return ids
}

// main is never called: `-target=wasm-unknown` produces a reactor. Go requires it all the same.
func main() {}

// init wires the seven exports, as `go-rules/main.go` does and for the same reason.
func init() {
	rule.Exports.Rules = rules
	rule.Exports.Metadata = ruleMetadata
	rule.Exports.Configure = configureExport
	rule.Exports.HasCheck = hasCheck
	rule.Exports.HasReduce = hasReduce
	rule.Exports.Check = checkExport
	rule.Exports.Reduce = reduceExport
}

// at looks one rule up by the index every export but `rules` takes.
func at(index uint32) (*hosted, bool) {
	if index >= uint32(len(ruleset)) {
		return nil, false
	}
	return &ruleset[index], true
}

// noSuchRule names an index this fixture does not host.
func noSuchRule(index uint32) string {
	return "no rule at index " + strconv.FormatUint(uint64(index), 10) +
		": this fixture hosts " + strconv.Itoa(len(ruleset))
}

// rules enumerates the rules this fixture hosts.
func rules() cm.List[string] {
	return cm.ToList(ruleIDs)
}

// ruleMetadata answers the world's `metadata`.
func ruleMetadata(index uint32) types.RuleMetadata {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.Metadata()
}

// configureExport answers the world's `configure`, which for this fixture accepts anything.
func configureExport(index uint32, optionsJSON string) configureResult {
	hosted, ok := at(index)
	if !ok {
		return cm.Err[configureResult](noSuchRule(index))
	}
	if err := hosted.Configure(optionsJSON); err != nil {
		return cm.Err[configureResult](err.Error())
	}
	return cm.OK[configureResult](struct{}{})
}

// hasCheck answers the world's `has-check`.
func hasCheck(index uint32) bool {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.HasCheck()
}

// hasReduce answers the world's `has-reduce`.
func hasReduce(index uint32) bool {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.HasReduce()
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

	hosted, ok := at(index)
	if !ok {
		return failed(noSuchRule(index))
	}
	if err := hosted.Check(context, m.Slice()); err != nil {
		return failed(err.Error())
	}
	return cm.OK[passResult](struct{}{})
}

// reduceExport runs the cross-file pass.
func reduceExport(index uint32, ctx cm.Rep) passResult {
	context := cm.Reinterpret[types.ReduceContext](ctx)
	defer context.ResourceDrop()

	hosted, ok := at(index)
	if !ok {
		return failed(noSuchRule(index))
	}
	if err := hosted.Reduce(context); err != nil {
		return failed(err.Error())
	}
	return cm.OK[passResult](struct{}{})
}

// failed is a graceful failure of `check` or `reduce`.
func failed(message string) passResult {
	return cm.Err[passResult](types.RuleError{Message: message})
}

// metadataFor is one rule's `metadata`, reporting the order *it* visited a map in.
//
// The card's message is the observation. Every other string is a constant, because nothing about
// this fixture is a rule and the card is only here because the record has the field.
func metadataFor(id string) func() types.RuleMetadata {
	return func() types.RuleMetadata {
		metadataOrder = visits()
		return types.RuleMetadata{
			ID:        id,
			Languages: cm.ToList(languages),
			Severity:  "error",
			Card: types.RuleCard{
				Message:     metadataOrder,
				Remediation: "nothing: this is a fixture, and the message is the observation",
				Examples: types.RuleExamples{
					Bad:  "for k := range m { }",
					Good: "for _, k := range sorted(m) { }",
				},
			},
			// Never compiled and never run: the harness hands `check` a match it built itself,
			// which is what lets one call be one observation. A syntactically real query all the
			// same, so that nothing downstream has to special-case this fixture.
			Queries: lanekeep.Queries("typescript", "(program) @file"),
			Gates: types.RuleGates{
				PathMatches:     cm.ToList(noPatterns),
				PathNotMatches:  cm.ToList(noPatterns),
				FileContains:    cm.ToList(noPatterns),
				FileNotContains: cm.ToList(noPatterns),
			},
			Timeout: cm.None[uint64](),
		}
	}
}

// configure records the order it visited a map in, and accepts whatever it was sent.
//
// The options are ignored on purpose: what is being probed is the reset in front of this call,
// not anything the rule does with what it is handed.
func configure(string) error {
	configured = visits()
	return nil
}

// check reports the order `configure` saw and the order [visits] sees now, at the root.
//
// The root rather than a captured node, because the node is not what this fixture is about and a
// capture it could fail to find would be a second way for the call to report nothing — which is
// indistinguishable, from the host, from a map order that came out empty.
func check(ctx types.CheckContext, _ lanekeep.Match) error {
	ctx.Report(rootNode, cm.Some(configured+string(section)+visits()), cm.None[types.Fix]())
	return nil
}

// reduce reports the order it visited a map in, as a cross-file violation.
//
// The location is a constant and means nothing. All three of its parts are filled in because the
// host **refuses** a cross-file report with no line or column — "a cross-file violation with no
// site is unactionable" — so a `none` here is not a fixture reporting less, it is a fixture whose
// `reduce` fails.
func reduce(ctx types.ReduceContext) error {
	ctx.Report(
		types.ReduceLocation{
			File:   reduceFile,
			Line:   cm.Some[uint32](reduceLine),
			Column: cm.Some[uint32](reduceColumn),
		},
		cm.Some(visits()),
	)
	return nil
}

// Where every cross-file report here is made. Nothing reads it; see [reduce].
const (
	reduceFile   = "src/a.ts"
	reduceLine   = 1
	reduceColumn = 1
)

// rootNode is the handle every per-file report here is made at.
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
