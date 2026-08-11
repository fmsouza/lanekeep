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
// package cannot reach a handler without going through [Handlers.Check] or [Handlers.Reduce].
// Deleting the reset from either method is what the wasm probe catches.

func TestHandlersCheckDelegatesWithItsArguments(t *testing.T) {
	want := errors.New("from the rule")
	var gotCtx types.CheckContext
	var gotMatch Match

	h := NewHandlers[types.CheckContext, types.ReduceContext](func(ctx types.CheckContext, m Match) error {
		gotCtx, gotMatch = ctx, m
		return want
	}, nil)

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

	h := NewHandlers[types.CheckContext, types.ReduceContext](nil, func(ctx types.ReduceContext) error {
		gotCtx = ctx
		return want
	})

	if err := h.Reduce(types.ReduceContext(9)); !errors.Is(err, want) {
		t.Fatalf("Reduce returned %v, want the rule's own error", err)
	}
	if gotCtx != types.ReduceContext(9) {
		t.Errorf("handler saw ctx %v, want 9", gotCtx)
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
		h                  Handlers[types.CheckContext, types.ReduceContext]
		wantCheck, wantRed bool
	}{
		{"both", NewHandlers(check, reduce), true, true},
		{"check only", NewHandlers[types.CheckContext, types.ReduceContext](check, nil), true, false},
		{"reduce only", NewHandlers[types.CheckContext, types.ReduceContext](nil, reduce), false, true},
		{"neither", NewHandlers[types.CheckContext, types.ReduceContext](nil, nil), false, false},
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
	h := NewHandlers[types.CheckContext, types.ReduceContext](nil, nil)

	if err := h.Check(types.CheckContext(0), nil); err == nil {
		t.Error("Check on a rule with no per-file pass must answer with an error")
	}
	if err := h.Reduce(types.ReduceContext(0)); err == nil {
		t.Error("Reduce on a rule with no cross-file pass must answer with an error")
	}
}
