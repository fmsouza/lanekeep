package lanekeep

import "testing"

func TestCaptureFindsANamedNode(t *testing.T) {
	m := Match{{Name: "func", Node: 7}, {Name: "name", Node: 12}}
	got, ok := Capture(m, "name")
	if !ok || got != 12 {
		t.Fatalf("Capture(m, \"name\") = (%v, %v), want (12, true)", got, ok)
	}
}

func TestCaptureReportsAMissingName(t *testing.T) {
	m := Match{{Name: "func", Node: 7}}
	if _, ok := Capture(m, "absent"); ok {
		t.Fatal("a name no capture carries must report false, not node zero")
	}
}

// The two tests above pin the halves of the contract separately and, between them, leave the
// case where the two halves meet: neither binds a capture to handle 0, so both pass against a
// `Capture` that returns `(m[i].Node, m[i].Node != 0)` — which is exactly the "zero doubles as
// absent" defect the whole two-value return exists to prevent. The root's handle is 0, so a
// rule matching the root is a real input, not a contrived one.
func TestCaptureFindsTheRootHandle(t *testing.T) {
	m := Match{{Name: "root", Node: 0}, {Name: "name", Node: 12}}
	got, ok := Capture(m, "root")
	if !ok || got != 0 {
		t.Fatalf("Capture(m, \"root\") = (%v, %v), want (0, true): handle 0 is the root, not a miss", got, ok)
	}
}
