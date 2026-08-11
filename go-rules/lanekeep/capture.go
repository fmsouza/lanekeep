package lanekeep

import "github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"

// Node is an opaque reference into the arena of the check-context that produced it.
//
// An alias rather than a defined type, so it is the very type the generated bindings use
// and there is nothing to convert. It mirrors the WIT `node` alias
// (`crates/lanekeep-wasm/wit/world.wit`), which is itself a plain `u32` rather than a
// resource.
//
// # Zero is a valid handle
//
// The root node's handle is 0. That is why [Capture] answers with a second `bool` rather
// than with a zero value standing in for "not found": the two are indistinguishable
// otherwise, and lanekeep has already paid for that confusion once — `no-unwrap` lost its
// whole `#[test]` exemption to a JavaScript `if (!node)` that discarded handle zero, and
// lost it silently, because the check it skipped only ever removed violations.
type Node = types.Node

// Match is one query match's captures: the name the query gave each, and the node it bound.
//
// A capture that did not participate in the match is simply absent from the list — the WIT
// `match` doc comment records this, and there is no analog of a JavaScript object holding an
// explicit `undefined` under a key, so there is no third state to represent.
//
// This is an alias for a Go slice, not a second copy of the record. The generated
// [types.Match] is a `cm.List[types.MatchEntry]`, whose promoted `Slice()` method yields
// exactly this type, so a rule hands its match straight over:
//
//	node, ok := lanekeep.Capture(m.Slice(), "name")
//
// Aliasing rather than redeclaring is deliberate. A structurally mirrored copy of
// `match-entry` would not break when a field is added to the WIT record — a struct literal
// converting field by field still compiles — so the two would drift with nothing to notice.
// The Rust SDK reaches the same end by a different road (`rust-rules/lanekeep-rule/src/lib.rs`
// asserts a `Capture` trait, because each Rust rule crate's generated bindings are private to
// it); Go's generated bindings are an ordinary importable package, so the type itself can be
// shared and no projection is needed.
type Match = []types.MatchEntry

// Capture returns the node bound to name in m, and whether m carried that name at all.
//
// The second return value is not decoration. See [Node]: handle 0 is the root, so a bare
// `Node` return could not distinguish a rule matching the root from a rule matching nothing.
//
// A query match is a handful of captures at most, so the linear scan is not a cost worth an
// index to avoid.
func Capture(m Match, name string) (Node, bool) {
	for i := range m {
		if m[i].Name == name {
			return m[i].Node, true
		}
	}
	return 0, false
}
