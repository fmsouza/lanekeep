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
