// Package nopackageinit implements `lanekeep/no-package-init`: `func init()` at package level.
//
// An init function runs when the package is imported, before `main`, in an order the language
// decides. Nothing calls it, so nothing in the code says when it happens; a reader tracing
// startup finds no edge leading to it. Two packages that both register into a shared map depend
// on an order neither one states, and the failure — a missing registration, a nil global —
// surfaces far from the cause and moves when an unrelated import is added.
//
// It is also the usual way a package acquires hidden startup cost: an import that looks free
// opens a connection or reads a file. That is what makes this architectural rather than
// stylistic, and worth stating as a project convention rather than arguing case by case.
//
// Wiring done explicitly from `main` — or a `New...` returning an error — is traceable,
// testable, and ordered by the code rather than by the linker.
//
// # A port, held to reporting identically
//
// This was `crates/lanekeep-rules/rules/no-package-init.ts`, a TypeScript module evaluated in
// QuickJS on every run, and the users being kept faith with are the ones whose configs did not
// change. Every string in [Metadata] is that file's, character for character, and the message
// [check] reports is the one it reported — a shared case table holds both implementations to
// that claim while the two exist side by side.
//
// # Nothing here may panic
//
// A panic in a guest is a trap, and a trap aborts the call before any report crosses back, so a
// wrong assumption about a match's shape would surface as "the host recorded no violations" —
// which is exactly what a broken `report` looks like too. Every lookup below answers rather than
// indexes.
package nopackageinit

import (
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
	"github.com/fmsouza/lanekeep/go-rules/lanekeep"
	"go.bytecodealliance.org/cm"
)

// ID is this rule's id, as a config names it and as `rules` enumerates it.
const ID = "lanekeep/no-package-init"

// NAME is the capture bound to the declared function's name.
const NAME = "name"

// FUNC is the capture bound to the whole declaration, which is where a violation is reported.
const FUNC = "func"

// MESSAGE is what a violation says. Byte for byte the TypeScript original's.
const MESSAGE = "`init` runs at import time in an order nothing states, so what it sets up is untraceable from the code that depends on it"

// The lists [Metadata] hands to the host, at package level rather than built per call.
//
// **Not a micro-optimization — a lifetime.** `cm.ToList` does not copy: it takes the slice's
// data pointer and length, and the host reads through that pointer after this function has
// returned. A slice literal inside `Metadata` is a local whose escape TinyGo would have to infer
// through `unsafe.SliceData`, and a wrong inference there is a lifted list pointing at reclaimed
// stack — which on this target, where the collector never frees anything, would be the one way
// to produce a dangling pointer. A package-level variable is in the globals and cannot be
// anywhere else.
var (
	// One or several; a rule does not run on a file whose language it does not name. `go`
	// alone, exactly as the TypeScript original's `language: 'go'`.
	languages = []string{"go"}

	// `file-contains` is an **and** over every listed substring, so a one-token gate is the only
	// shape this rule could have: `init` is in every subject, and nothing else is.
	fileContains = []string{"init"}

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
			Message:     "package-level func init()",
			Remediation: "move the work into an explicit constructor or a call from main, so the order it happens in is written down",
			Examples: types.RuleExamples{
				Bad:  "func init() {\n\tregistry[\"pg\"] = newPostgres()\n}",
				Good: "func Register(r map[string]Driver) {\n\tr[\"pg\"] = newPostgres()\n}",
			},
		},
		// `function_declaration` is only ever package level in Go — a function inside a function
		// is a `func_literal`, which this cannot match. So matching the name is enough, and there
		// is no nesting check to get wrong.
		Query: "(function_declaration name: (identifier) @name) @func",
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

// Handlers is everything the host calls on this rule: its metadata, and a per-file pass. No
// options, and no cross-file pass.
//
// Constructed through [lanekeep.NewHandlers] rather than handed over as bare functions, which is
// what puts the map-iteration reset ahead of every invocation without this file mentioning it.
// The type parameters are named because an untyped `nil` carries nothing to infer from.
func Handlers() lanekeep.Handlers[types.RuleMetadata, types.CheckContext, types.ReduceContext] {
	return lanekeep.NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		Metadata, nil, check, nil,
	)
}

// check reports one `func init()`.
//
// The query binds both captures on every match, so a missing one is unreachable — and it is a
// quiet return rather than an error because the alternative in a guest is noise the host cannot
// tell from a rule that legitimately found nothing.
func check(ctx types.CheckContext, m lanekeep.Match) error {
	name, ok := lanekeep.Capture(m, NAME)
	if !ok {
		return nil
	}
	declaration, ok := lanekeep.Capture(m, FUNC)
	if !ok {
		return nil
	}

	// `Value()` is the empty string when the host has no text for the node, which is not `init`
	// and so is not a violation — the same answer the TypeScript original's
	// `ctx.text(m.name) !== 'init'` gives for `undefined`.
	if ctx.Text(name).Value() != "init" {
		return nil
	}

	ctx.Report(declaration, cm.Some(MESSAGE), cm.None[types.Fix]())
	return nil
}
