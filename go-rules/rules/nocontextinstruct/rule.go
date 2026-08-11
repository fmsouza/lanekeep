// Package nocontextinstruct implements `lanekeep/no-context-in-struct`: a `context.Context`
// kept as a struct field.
//
// The context package says it plainly: do not store Contexts inside a struct type; pass one
// explicitly, as the first parameter. A stored context outlives the call it was scoped to, so
// cancellation and deadlines stop meaning what the caller intended — a long-lived client holds
// the context of whichever request happened to construct it, and cancelling that request cancels
// work belonging to every other.
//
// It is architectural rather than stylistic: the damage shows up as unrelated requests failing
// together, which is nearly impossible to attribute back to the field.
//
// # The resolver is what keeps this from being a text match
//
// [types.CheckContext.BindingKind] says whether the qualifier is an import at all, so a
// package-level name that happens to read `context` does not fire.
//
// What it deliberately does *not* distinguish is which module the import points at. The host API
// answers what kind of binding a name has, not where it came from, so a package aliased to
// `context` and exposing a `Context` is reported the same as the standard library's. Widening
// the host API to expose the module would bump its version, which is a cache key input — a
// bigger change than this rule justifies. The false positive it leaves is narrow and, where it
// happens, is usually the same mistake wearing a different import path. It is pinned by a case
// in `crates/lanekeep-rules/tests/no_context_in_struct.rs` rather than left to be rediscovered.
//
// # A port, held to reporting identically
//
// This was `crates/lanekeep-rules/rules/no-context-in-struct.ts`, a TypeScript module evaluated
// in QuickJS on every run, and the users being kept faith with are the ones whose configs did
// not change. Every string in [Metadata] is that file's, character for character — [QUERY]
// including its whitespace — and the message [check] reports is the one it reported. A shared
// case table holds both implementations to that claim while the two exist side by side.
//
// # Nothing here may panic
//
// A panic in a guest is a trap, and a trap aborts the call before any report crosses back, so a
// wrong assumption about a match's shape would surface as "the host recorded no violations" —
// which is exactly what a broken `report` looks like too. Every lookup below answers rather than
// indexes.
package nocontextinstruct

import (
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
	"github.com/fmsouza/lanekeep/go-rules/lanekeep"
	"go.bytecodealliance.org/cm"
)

// ID is this rule's id, as a config names it and as `rules` enumerates it.
const ID = "lanekeep/no-context-in-struct"

// PKG is the capture bound to the qualifier of the field's type — the `context` of
// `context.Context`.
const PKG = "pkg"

// NAME is the capture bound to the type's own name, the `Context`.
const NAME = "name"

// FIELD is the capture bound to the whole field declaration, which is where a violation is
// reported.
const FIELD = "field"

// PACKAGE is the qualifier a violation has. Compared against, not searched for.
const PACKAGE = "context"

// TYPE is the type name a violation has.
const TYPE = "Context"

// MESSAGE is what a violation says. Byte for byte the TypeScript original's.
const MESSAGE = "a context.Context stored in a struct outlives the call it was scoped to, so cancelling one request can cancel unrelated work"

// QUERY is the two patterns this rule matches, verbatim from the TypeScript source.
//
// **Two patterns rather than one**, because `ctx context.Context` and `ctx *context.Context`
// differ by a `pointer_type` in between and a query matching only the bare form would pass the
// pointer one silently. Both bind the same three captures, so [check] does not care which fired
// and there is nothing to branch on.
//
// The leading newline and the space indentation are the TypeScript template literal's, kept
// rather than reflowed to this file's tabs: tree-sitter ignores the whitespace either way, and
// keeping the bytes identical lets the two implementations be compared by a machine instead of
// by eye while both exist.
const QUERY = `
    (field_declaration
      type: (qualified_type
        package: (package_identifier) @pkg
        name: (type_identifier) @name)) @field
    (field_declaration
      type: (pointer_type
        (qualified_type
          package: (package_identifier) @pkg
          name: (type_identifier) @name))) @field
  `

// The lists [Metadata] hands to the host, at package level rather than built per call.
//
// **Not a micro-optimization — a lifetime.** `cm.ToList` does not copy: it takes the slice's
// data pointer and length, and the host reads through that pointer after this function has
// returned. See the same note in `rules/nopackageinit/rule.go`, which sets it out at length.
var (
	// One or several; a rule does not run on a file whose language it does not name. `go`
	// alone, exactly as the TypeScript original's `language: 'go'`.
	languages = []string{"go"}

	// `file-contains` is an **and** over every listed substring. One token is all this rule
	// needs: a file with no `context` in its bytes has neither the import nor the field.
	fileContains = []string{"context"}

	// The three gates this rule does not use. Shared, because they are all the same empty list
	// and a reader should not have to check whether three separate ones differ.
	noPatterns []string
)

// Metadata is what this rule is, read once at prepare time.
func Metadata() types.RuleMetadata {
	return types.RuleMetadata{
		ID:        ID,
		Languages: cm.ToList(languages),
		Severity:  "error",
		Card: types.RuleCard{
			Message:     "a context.Context is stored in a struct",
			Remediation: "pass the context as the first parameter of each method instead, so its cancellation scope matches the call",
			Examples: types.RuleExamples{
				Bad:  "type Client struct {\n\tctx context.Context\n}",
				Good: "type Client struct{}\n\nfunc (c *Client) Do(ctx context.Context) error { return nil }",
			},
		},
		Query: QUERY,
		Gates: types.RuleGates{
			PathMatches:     cm.ToList(noPatterns),
			PathNotMatches:  cm.ToList(noPatterns),
			FileContains:    cm.ToList(fileContains),
			FileNotContains: cm.ToList(noPatterns),
		},
		// None means the default per-invocation budget.
		Timeout: cm.None[uint64](),
	}
}

// Handlers is this rule's passes: a per-file one, and no cross-file one.
//
// Constructed through [lanekeep.NewHandlers] rather than handed over as bare functions, which is
// what puts the map-iteration reset ahead of every invocation without this file mentioning it.
// The type parameters are named because an untyped `nil` reduce carries nothing to infer from.
func Handlers() lanekeep.Handlers[types.CheckContext, types.ReduceContext] {
	return lanekeep.NewHandlers[types.CheckContext, types.ReduceContext](check, nil)
}

// check reports one stored `context.Context`.
//
// Both query patterns bind all three captures on every match, so a missing one is unreachable —
// and it is a quiet return rather than an error because the alternative in a guest is noise the
// host cannot tell from a rule that legitimately found nothing.
//
// # `Value()` would be wrong on the binding kind, and wrong quietly
//
// `binding-kind` answers `option<binding-kind>`, and `import` is the **first** case of that
// enum — so it is also the zero value, and `cm.Option.Value()` hands back the zero value for a
// `none`. Reading it that way would report every qualifier that resolves to no binding at all,
// which is precisely `a_type_named_context_from_no_import_passes`. That is the same shape as the
// handle-zero bug recorded in [lanekeep.Node]: a mistake that only ever *adds* violations, and
// so looks like a rule being strict rather than a rule being broken. [cm.Option.Some] is a
// pointer method and answers `nil` for a `none`, which is the distinction this needs — and it is
// why the option is bound to a variable first, since a method with a pointer receiver cannot be
// called on the result of a function call.
func check(ctx types.CheckContext, m lanekeep.Match) error {
	name, ok := lanekeep.Capture(m, NAME)
	if !ok {
		return nil
	}
	pkg, ok := lanekeep.Capture(m, PKG)
	if !ok {
		return nil
	}
	field, ok := lanekeep.Capture(m, FIELD)
	if !ok {
		return nil
	}

	// `Value()` is the empty string when the host has no text for the node, which is neither of
	// these and so is not a violation — the same answer the TypeScript original's
	// `ctx.text(m.name) !== 'Context'` gives for `undefined`.
	if ctx.Text(name).Value() != TYPE {
		return nil
	}
	if ctx.Text(pkg).Value() != PACKAGE {
		return nil
	}

	// A qualifier that is not an import is a local name that happens to read `context`, and a
	// type of the same shape from somewhere else entirely.
	kind := ctx.BindingKind(pkg)
	binding := kind.Some()
	if binding == nil || *binding != types.BindingKindImport {
		return nil
	}

	ctx.Report(field, cm.Some(MESSAGE), cm.None[types.Fix]())
	return nil
}
