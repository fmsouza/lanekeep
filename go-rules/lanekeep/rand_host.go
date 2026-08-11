//go:build !wasm_unknown

package lanekeep

// ResetRand does nothing here, and that is not a fallback — it is the absence of the problem.
//
// The hazard it answers is TinyGo's, and specifically TinyGo's on `-target=wasm-unknown`: a
// pair of advancing runtime globals that decide map iteration order and outlive a `check`
// call. `rand.go` holds the real implementation and is selected by the `wasm_unknown` build
// tag, which is the only configuration any shipped artifact is built with.
//
// This file exists so that `go test`, `go vet` and an editor's language server work on an
// ordinary host toolchain over this module and the rule packages above it — every rule calls
// [ResetRand] at the top of its handlers, and without a host-side declaration none of them
// would compile outside a TinyGo build.
//
// It must not be read as evidence about the shipping build in either direction. A host build
// cannot demonstrate the reset works, because the linkname is not here; and a host build
// cannot fail because of it either. Whether the real one binds to the right runtime symbols
// is established behaviorally against a `wasm-unknown` artifact — a wrong linkname target
// does not fail a build, so nothing short of running it is proof. `rand.go`'s documentation
// carries the measurement.
func ResetRand() {}
