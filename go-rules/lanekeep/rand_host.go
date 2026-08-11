//go:build !tinygo

package lanekeep

// ResetRand does nothing here, and that is not a fallback — it is the absence of the problem.
//
// The hazard it answers is TinyGo's: a pair of advancing runtime globals that decide map
// iteration order and outlive a `check` call. `rand.go` holds the real implementation, under
// the `wasm_unknown` build tag, which is the only configuration any shipped artifact is built
// with.
//
// This file exists so that `go test`, `go vet` and an editor's language server work on an
// ordinary host toolchain over this module and the rule packages above it — every rule reaches
// [ResetRand] through [WithResetRand], and without a host-side declaration none of them would
// compile outside a TinyGo build.
//
// # The tag is `!tinygo`, and `!wasm_unknown` is the version of it that is quietly wrong
//
// The two look interchangeable and are not. `-target=wasip2` sets
// `["tinygo.wasm", "wasip2", "tinygo", ...]` and **not** `wasm_unknown`, so under
// `!wasm_unknown` a wasip2 build selects *this* file: the no-op ships, and every determinism
// argument in `doc.go` silently stops holding. That build succeeds — measured, exit 0 — and
// nothing local objects, because `realloc.go`'s loud `cabi_realloc` canary is also excluded on
// that target, TinyGo supplying its own under the `wasip2` tag. The only thing left is the
// downstream `wasm-tools` refusal, which names `wasi:cli/environment` and says nothing about
// map iteration order.
//
// `tinygo` is set by every TinyGo target, so `!tinygo` means this file is reachable *only*
// from a host toolchain. A wasip2 build now fails with `undefined: lanekeep.ResetRand` —
// pointing at the rule, at the symbol, and at this pair of files — rather than shipping a rule
// whose reset does nothing.
//
// It must not be read as evidence about the shipping build in either direction. A host build
// cannot demonstrate the reset works, because the `//go:extern` declarations are not here; and
// a host build cannot fail because of them either. Whether the real one binds to the right
// runtime symbols is established behaviorally against a `wasm-unknown` artifact. `rand.go`'s
// documentation carries the measurement.
func ResetRand() {}
