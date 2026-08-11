package lanekeep

import (
	"errors"
	"testing"

	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
)

// The reset that makes a Go rule deterministic is not observable from a host build — `rand.go`
// is excluded there by its build tag, so [ResetRand] is the documented no-op. What these tests
// pin is the other half of the guarantee, which *is* observable here: that the only route to a
// rule's handler runs through the wrapper. That the wrapper's reset then does something is
// proved against a `wasm-unknown` artifact, where map order can be watched.
//
// The structural half is carried by the type rather than by an assertion: `Handlers`' fields
// are unexported and [NewHandlers] is the only constructor, so a component entry outside this
// package cannot reach any of a rule's four funcs without going through [Handlers.Metadata],
// [Handlers.Configure], [Handlers.Check] or [Handlers.Reduce]. Deleting the reset from any of
// them is what a wasm probe catches; what these tests catch is a method that stopped delegating.

func TestHandlersCheckDelegatesWithItsArguments(t *testing.T) {
	want := errors.New("from the rule")
	var gotCtx types.CheckContext
	var gotMatch Match

	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		nil, nil,
		func(ctx types.CheckContext, m Match) error {
			gotCtx, gotMatch = ctx, m
			return want
		},
		nil,
	)

	m := Match{{Name: "call", Node: 7}}
	if err := h.Check(types.CheckContext(42), m); !errors.Is(err, want) {
		t.Fatalf("Check returned %v, want the rule's own error", err)
	}
	if gotCtx != types.CheckContext(42) {
		t.Errorf("handler saw ctx %v, want 42", gotCtx)
	}
	if len(gotMatch) != 1 || gotMatch[0].Name != "call" || gotMatch[0].Node != 7 {
		t.Errorf("handler saw match %v, want the one it was given", gotMatch)
	}
}

func TestHandlersReduceDelegatesWithItsArgument(t *testing.T) {
	want := errors.New("from the rule")
	var gotCtx types.ReduceContext

	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		nil, nil, nil,
		func(ctx types.ReduceContext) error {
			gotCtx = ctx
			return want
		},
	)

	if err := h.Reduce(types.ReduceContext(9)); !errors.Is(err, want) {
		t.Fatalf("Reduce returned %v, want the rule's own error", err)
	}
	if gotCtx != types.ReduceContext(9) {
		t.Errorf("handler saw ctx %v, want 9", gotCtx)
	}
}

// `metadata` and `configure` go through the wrapper for the same reason the two passes do, and
// the reason is not hypothetical: a rule whose `configure` decodes its options into a
// `map[string]any` and builds a list by ranging it produces an order that depends on how many
// draws the shared generator has already taken. One instance is shared per (worker, component)
// across every file that worker handles, so that count is a rayon scheduling artifact.
func TestHandlersMetadataDelegates(t *testing.T) {
	calls := 0
	want := types.RuleMetadata{ID: "lanekeep/probe"}

	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		func() types.RuleMetadata { calls++; return want },
		nil, nil, nil,
	)

	if got := h.Metadata(); got.ID != want.ID {
		t.Fatalf("Metadata() = %v, want the rule's own %v", got.ID, want.ID)
	}
	if calls != 1 {
		t.Errorf("the rule's metadata func ran %d times, want 1", calls)
	}
}

func TestHandlersConfigureDelegatesWithItsArgument(t *testing.T) {
	want := errors.New("from the rule")
	var got string

	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		nil,
		func(optionsJSON string) error { got = optionsJSON; return want },
		nil, nil,
	)

	if err := h.Configure(`{"allow":["x"]}`); !errors.Is(err, want) {
		t.Fatalf("Configure returned %v, want the rule's own error", err)
	}
	if got != `{"allow":["x"]}` {
		t.Errorf("the rule saw options %q, want the ones it was given", got)
	}
}

// A rule that declares no `configure` accepts whatever it is sent rather than refusing.
//
// Deliberate, and a migration constraint rather than a preference: the TypeScript rules these
// components replace export a plain object, so a config naming one with options today is
// accepted and the options ignored. A component that refused would turn a working config into a
// failed run at the moment the built-in is swapped over.
func TestHandlersWithNoConfigureAcceptOptions(t *testing.T) {
	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](nil, nil, nil, nil)

	if err := h.Configure(`{"limit":1}`); err != nil {
		t.Errorf("a rule with no options must accept rather than refuse: %v", err)
	}
}

// `has-check` and `has-reduce` are what the host consults before dispatching, so they have to
// follow from the same value the dispatch does — not from a second declaration a rule could
// get out of step with.
func TestHandlersReportWhichPassesExist(t *testing.T) {
	check := func(types.CheckContext, Match) error { return nil }
	reduce := func(types.ReduceContext) error { return nil }

	for _, c := range []struct {
		name               string
		h                  Handlers[types.RuleMetadata, types.CheckContext, types.ReduceContext]
		wantCheck, wantRed bool
	}{
		{"both", NewHandlers[types.RuleMetadata](nil, nil, check, reduce), true, true},
		{"check only", NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](nil, nil, check, nil), true, false},
		{"reduce only", NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](nil, nil, nil, reduce), false, true},
		{"neither", NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](nil, nil, nil, nil), false, false},
	} {
		if got := c.h.HasCheck(); got != c.wantCheck {
			t.Errorf("%s: HasCheck() = %v, want %v", c.name, got, c.wantCheck)
		}
		if got := c.h.HasReduce(); got != c.wantRed {
			t.Errorf("%s: HasReduce() = %v, want %v", c.name, got, c.wantRed)
		}
	}
}

// A host that dispatches without consulting `has-check` first must be told which of the two
// disagreed, not met with a nil dereference — `check` has an error channel precisely so a
// guest can answer instead of trapping.
func TestHandlersRefuseAPassTheyDoNotHave(t *testing.T) {
	h := NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](nil, nil, nil, nil)

	if err := h.Check(types.CheckContext(0), nil); err == nil {
		t.Error("Check on a rule with no per-file pass must answer with an error")
	}
	if err := h.Reduce(types.ReduceContext(0)); err == nil {
		t.Error("Reduce on a rule with no cross-file pass must answer with an error")
	}
}
