//go:build wasm_unknown

package lanekeep

import "unsafe"

// The component model requires every component to export `cabi_realloc`, and TinyGo defines
// one only under the `wasip2` build tag (`src/runtime/runtime_wasip2.go`). On `wasm-unknown`
// — the target lanekeep builds Go rules for, because it is the only one whose components
// import nothing but `lanekeep:host/types` — the guest has to supply it, or the very first
// build fails at the componentization step with:
//
//	error: failed to encode a component from module
//	Caused by:
//	    0: module does not export a function named `cabi_realloc`
//
// Nothing in that message points at a build tag, which is why this file exists in the SDK
// rather than in each rule: no rule author should have to find it twice.
//
// # The obvious alternative fails, and its error names the wrong layer
//
// Reaching for the libc symbol instead —
//
//	//export realloc
//	func libc_realloc(ptr unsafe.Pointer, size uintptr) unsafe.Pointer
//
// — gives `failed to resolve import env::realloc` / `module requires an import interface
// named env`. That reads as a component-model problem and is not one:
// `src/runtime/arch_tinygowasm_malloc.go`, which exports `realloc` as a libc symbol, carries
// the build tag `tinygo.wasm && !(custommalloc || wasm_unknown || gc.boehm)` and is therefore
// excluded on this very target, so the bodyless declaration became an *import* instead of
// binding to anything. It is the shape AGENTS.md already names: a fixture refused by an
// earlier gate than the one under test fails with a message that sends you somewhere else.
//
// `runtime.realloc` is the right symbol and exists in every one of TinyGo's garbage
// collectors (`gc_leaking.go`, `gc_blocks.go`), so this is not a workaround so much as
// TinyGo's own wasip2 shim under a different build tag. Verified sound under a moving
// collector, not only the leaking default: the spike's `-gc=precise` and `-gc=conservative`
// artifacts both instantiate and run.

//go:linkname runtimeRealloc runtime.realloc
func runtimeRealloc(ptr unsafe.Pointer, size uintptr) unsafe.Pointer

// Both pragmas, deliberately, though **either one alone produces the export** — measured, by
// building with each in turn and reading the artifact's export list. They are here because
// this function is standing in for one of the generated exports, and every export in
// `internal/lanekeep/host/rule/rule.wasm.go` carries exactly this pair: `//go:wasmexport rules`
// above `//export rules`, and so on for all seven. Matching that is what keeps a reader from
// having to work out whether the difference means something. (TinyGo's own wasip2 shim, in
// `src/runtime/runtime_wasip2.go`, writes only `//export` — so there is no single upstream
// convention to defer to, and the neighboring generated code is the better guide.)

//go:wasmexport cabi_realloc
//export cabi_realloc
func cabi_realloc(ptr unsafe.Pointer, oldsize, align, newsize uintptr) unsafe.Pointer {
	return runtimeRealloc(ptr, newsize)
}
