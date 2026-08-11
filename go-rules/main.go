// Command go-builtins is the WebAssembly component the Go-authored built-in rules ship in.
//
// It is the Go counterpart of what `rust-rules/lanekeep-rule`'s `ruleset!` macro generates for a
// Rust rule crate: a table of the rules this component hosts, and the seven exports of
// `crates/lanekeep-wasm/wit/world.wit`'s `rule` world dispatched over it. `just go-rules` builds
// it and copies the artifact to `crates/lanekeep-rules/components/go-builtins.wasm`.
//
// # One component for every Go rule, and not one each
//
// Every export but `rules` takes an index into the table below, which is what lets one artifact
// host several rules. For a Rust rule that shape is a convenience — a rule crate is a 26 KB
// component and one each would be perfectly affordable. Here it is closer to the JavaScript
// case: a TinyGo artifact carries a runtime, so the marginal rule is far cheaper than the first.
// Rules go in [ruleset] in the order they are to be enumerated, and the index a rule sits at is
// what `crates/lanekeep-rules`' own table dispatches on.
//
// # A table rather than a generated dispatch
//
// The Rust lane needs a macro because each rule crate's generated bindings are private to it, so
// its SDK cannot name the types in a handler's signature. Go's bindings are an ordinary
// importable package, so a plain slice of structs says the same thing with nothing generated and
// nothing to expand. What it does not do by itself is keep the id and the index in step — so
// `rules` reports [ruleset]'s own ids, in its own order, and every other export looks the index
// back up in the same slice. The list is the mapping; there is no second place for a number to
// be written down and drift.
package main

import (
	"strconv"

	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/rule"
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
	"github.com/fmsouza/lanekeep/go-rules/lanekeep"
	"github.com/fmsouza/lanekeep/go-rules/rules/nocontextinstruct"
	"github.com/fmsouza/lanekeep/go-rules/rules/nopackageinit"
	"go.bytecodealliance.org/cm"
)

// The two `result` shapes the world's exports answer with, named so the calls below read.
//
// `cm.Result[Shape, OK, Err]` needs its shape spelled at every construction, and the generated
// signatures already fix them: `check` and `reduce` return `result<_, rule-error>`, `configure`
// returns `result<_, string>`.
type (
	passResult      = cm.Result[types.RuleError, struct{}, types.RuleError]
	configureResult = cm.Result[string, struct{}, string]
)

// hosted is one rule as this component holds it: what it says it is, how it takes options, and
// its two passes.
//
// **The passes are embedded rather than stored as funcs**, so that `ruleset[i].Check(...)` is a
// call on [lanekeep.Handlers] and there is no second, reset-free way to reach the handler
// underneath. That wrapper resets TinyGo's map-iteration generators before delegating; a
// dispatch that called a rule's own function directly would reopen a determinism hole that no
// test in this repository would catch, because nothing about a rule's output looks wrong when
// map order varies. See the `lanekeep` package documentation.
type hosted struct {
	// id is what a config names and what `rules` enumerates. Its position in [ruleset] is the
	// index every other export takes.
	id string

	// metadata is what this rule is, read once at prepare time and after `configure`.
	metadata func() types.RuleMetadata

	// configure hands the rule its options as JSON, or `null` when it was named with none.
	configure func(optionsJSON string) error

	lanekeep.Handlers[types.CheckContext, types.ReduceContext]
}

// ruleset is every rule this component hosts, in the order `rules` reports them.
//
// **Ordered by id, and that is a constraint rather than a tidy habit.**
// `crates/lanekeep-rules`' `COMPONENT_RULES` is sorted by rule name, and the index in each of its
// rows is what this component is dispatched on — so a table in any other order means a config
// naming one rule running the other, with both components answering perfectly well and nothing
// anywhere to notice. `crates/lanekeep-wasm/tests/world_shape.rs` asserts the order this reports.
//
// A rule that sorts into the middle therefore renumbers every rule after it, and the rows in
// `COMPONENT_RULES` have to move with it in the same change.
var ruleset = []hosted{
	{
		id:        nocontextinstruct.ID,
		metadata:  nocontextinstruct.Metadata,
		configure: takesNoOptions,
		Handlers:  nocontextinstruct.Handlers(),
	},
	{
		id:        nopackageinit.ID,
		metadata:  nopackageinit.Metadata,
		configure: takesNoOptions,
		Handlers:  nopackageinit.Handlers(),
	},
}

// ruleIDs is [ruleset]'s ids, in its order, built once.
//
// A package-level variable rather than a slice built inside `rules`: `cm.ToList` does not copy,
// so the host reads through this pointer after the export has returned. See the same note in
// `rules/nopackageinit/rule.go` for why a local would be the one shape that could dangle here.
var ruleIDs = identify(ruleset)

// identify is [ruleIDs]'s initializer, split out so it can be a loop.
func identify(rules []hosted) []string {
	ids := make([]string, len(rules))
	for i := range rules {
		ids[i] = rules[i].id
	}
	return ids
}

// main is never called: `-target=wasm-unknown` produces a reactor, whose `_initialize` runs
// package initialization and then waits to be called through an export. Go requires a `main` in
// package `main` all the same.
func main() {}

// init wires the seven exports.
//
// `_initialize` runs this before the host can call any of them, which is the ordering the
// component model guarantees for a reactor. The irony of an `init` function in the component
// hosting `lanekeep/no-package-init` is noted and left alone: the rule is about a project's own
// Go code, this is the one construct wit-bindgen-go's generated `Exports` struct can be filled
// from at the right moment, and a cleverer spelling in an entry point is worse than an obvious
// one.
func init() {
	rule.Exports.Rules = rules
	rule.Exports.Metadata = metadata
	rule.Exports.Configure = configure
	rule.Exports.HasCheck = hasCheck
	rule.Exports.HasReduce = hasReduce
	rule.Exports.Check = check
	rule.Exports.Reduce = reduce
}

// rules enumerates every rule this component hosts, by id, in index order.
func rules() cm.List[string] {
	return cm.ToList(ruleIDs)
}

// metadata answers what one rule is.
//
// Traps on an index this component does not host; see [noSuchRule].
func metadata(index uint32) types.RuleMetadata {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.metadata()
}

// configure hands one rule its options.
//
// It has an error channel, so an unknown index is answered rather than trapped: the host asked
// about a rule, and the reply that names the mistake is more use than a bare `unreachable`.
func configure(index uint32, optionsJSON string) configureResult {
	hosted, ok := at(index)
	if !ok {
		return cm.Err[configureResult](noSuchRule(index))
	}
	if err := hosted.configure(optionsJSON); err != nil {
		return cm.Err[configureResult](err.Error())
	}
	return cm.OK[configureResult](struct{}{})
}

// hasCheck reports whether one rule has a per-file pass.
//
// From the same value the dispatch uses, so what this answers and what `check` then does cannot
// disagree. Traps on an unknown index; see [noSuchRule].
func hasCheck(index uint32) bool {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.HasCheck()
}

// hasReduce reports whether one rule has a cross-file pass, on the same terms as [hasCheck].
func hasReduce(index uint32) bool {
	hosted, ok := at(index)
	if !ok {
		panic(noSuchRule(index))
	}
	return hosted.HasReduce()
}

// check runs one rule's per-file pass for one query match.
//
// `m` is lifted as a `cm.List`, and `Slice()` is what turns that pointer-and-length pair into the
// plain slice a rule reads — no copy, and nothing to convert.
//
// # The borrowed handle has to be dropped, and nothing in the generated code does it
//
// `ctx` is a `borrow<check-context>`: the host owns the resource and lends it for the length of
// this call. In the canonical ABI that lending is *counted* — the handle is added to this
// component's table against a borrow scope, and the runtime checks at the end of the call that
// every handle lent during it has been given back. `resource.drop` on a borrow is how a guest
// gives one back, and it does not run the resource's destructor: the owner keeps the value, and
// only the loan is closed.
//
// `wit-bindgen-go` 0.7.0 does not emit that drop. Its `wasmexport_Check` reinterprets the
// integer and hands it straight over, so a guest that does nothing here returns with the loan
// outstanding and wasmtime refuses the call with **`borrow handles still remain at the end of the
// call`** — a message about the runtime's own bookkeeping that names neither the rule, nor the
// handle, nor the export. Worse, it arrives after the rule body has already run, so every report
// the rule made is discarded and the failure looks like a rule that found nothing.
//
// It is invisible to everything short of a real call. The artifact is a valid component, its
// import list is exactly the declared world, its `rules`, `metadata`, `configure`, `has-check`
// and `has-reduce` all answer correctly, and its digest is current — the two exports that take a
// context are the only ones that fail, and they are the two a load-time or metadata-level check
// never exercises.
//
// `defer` rather than a drop before each `return`, so a handler that grows a second early return
// cannot leave one path lending.
func check(index uint32, ctx cm.Rep, m types.Match) passResult {
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

// reduce runs one rule's cross-file pass, on the same terms as [check] — including the drop,
// which `reduce` needs for exactly the same reason and which nothing generated supplies either.
func reduce(index uint32, ctx cm.Rep) passResult {
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

// takesNoOptions is the `configure` of a rule that has none.
//
// It accepts rather than refuses, and that is deliberate. The TypeScript rules these components
// replace export a plain object, so a config that names one with options today is accepted and
// the options are ignored; a component that refused would turn a working config into a failed
// run at the moment the built-in is swapped over, which is not a change that belongs inside a
// migration.
func takesNoOptions(string) error { return nil }

// at is the rule at an index, or `false` when this component hosts no such rule.
//
// A lookup that answers rather than an index expression that panics, so each caller decides how
// to refuse — three of the world's exports have no channel to refuse through and three do.
//
// Compared as `uint64` on both sides: `len` is an `int`, which is 32 bits on this target, so
// converting the parameter to `int` first would wrap an index above 2^31 into a negative and
// admit it.
func at(index uint32) (*hosted, bool) {
	if uint64(index) >= uint64(len(ruleset)) {
		return nil, false
	}
	return &ruleset[index], true
}

// noSuchRule is what an index this component does not host is refused with.
//
// **Half the exports that take an index can only trap on it**, and that is the honest answer
// rather than a shortcut: `metadata`, `has-check` and `has-reduce` have no error channel, so the
// alternatives are to invent one rule's answer for another rule's index or to return a
// plausible-looking blank. Both would be a host and a component silently disagreeing about which
// rule is running, which is the failure the `rules`-then-index arrangement exists to make
// impossible. `configure`, `check` and `reduce` do have a channel and use it, so the message is
// shared and the way it is delivered is not.
//
// Neither path is reachable from a host that enumerates first: `rules` reports exactly the
// indices that answer. Reaching one means the host asked about a rule this component never
// listed.
//
// Built by concatenation rather than with `fmt.Sprintf`, which would pull reflection into an
// artifact whose whole point is to be small.
func noSuchRule(index uint32) string {
	return "no rule at index " + strconv.FormatUint(uint64(index), 10) +
		": this component's `rules` export lists every index that answers, and this is not one of them"
}

// failed is a graceful failure of `check` or `reduce`.
//
// `frames` is left empty. It carries a parsed stack for a language that can produce one, and
// this build cannot: `-panic=trap` turns a Go panic into a wasm trap with nothing attached, so
// there is no stack for a *returned* error to have come from either.
func failed(message string) passResult {
	return cm.Err[passResult](types.RuleError{Message: message})
}
