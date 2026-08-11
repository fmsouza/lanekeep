//go:build wasm_unknown

package lanekeep

// TinyGo's map iteration is randomized from a pair of package-level pseudo-random generators
// in its runtime, and this file writes to both of them. See the package documentation for why
// that is a determinism requirement rather than a tuning knob.
//
// # Which global, and why the obvious one is the wrong one
//
// There are two, and they are not interchangeable. From TinyGo 0.41.1's own
// `src/runtime/algorithm.go`:
//
//	func fastrand() uint32   { xorshift32State = xorshift32(xorshift32State); return xorshift32State }
//	func fastrand64() uint64 { xorshift64State = xorshiftMult64(xorshift64State); return xorshift64State }
//
// Map iteration reaches for the **32-bit** one. `src/runtime/hashmap.go` calls `fastrand()` at
// three sites — line 77 and line 294 for a map's hash seed, and lines 402-403 for an
// iterator's start bucket and start index — and `fastrand()` advances `xorshift32State` alone.
// Pinning `xorshift64State` and nothing else therefore leaves map order exactly as
// scheduling-dependent as it was, while looking from the call site as though the hazard were
// closed. Measured rather than reasoned: a probe that resets only the 64-bit global does not
// recover the map order a fresh guest starts with, and one that resets the 32-bit global does.
//
// Both are written here. The 32-bit one because it is what map order depends on; the 64-bit
// one because `fastrand64` backs `math/rand` through the `math/rand.fastrand64` linkname in
// that same runtime file, and is the same class of advancing global.
//
// `1` is not an arbitrary sentinel. It is the value both globals are declared with, and the
// value `initRand` leaves them at on this target: it computes `xorshift64State = uint64(r|1)`
// from `hardwareRand()`, and `runtime_tinygowasm_unknown.go` implements `hardwareRand` as
// `return 0, false`, so `r` is 0 and both settle at 1. Resetting to 1 restores exactly the
// position a freshly instantiated guest starts from, which is what makes an invocation's map
// order a function of that invocation's inputs alone.
//
// # `//go:extern`, and not `//go:linkname`
//
// This is the pragma that reads an existing global. `//go:linkname` is the wrong tool for a
// variable here, and it fails differently in each of the two toolchains that see this module,
// which is worth stating because neither failure names the real problem:
//
//   - Under TinyGo, `//go:linkname xorshift32State runtime.xorshift32State` on a `var` emits a
//     *definition* rather than a reference, and the build dies with
//     `error: Linking globals named 'runtime.xorshift32State': symbol multiply defined!` — a
//     message about the linker that says nothing about the pragma being the wrong one.
//   - Under host Go the same declaration links fine and writes to a symbol nobody reads, so a
//     `go test` over this package would pass while the reset did nothing at all.
//
// `//go:extern` is also the safer of the two in the way that matters most here, and this was
// checked rather than assumed: a misspelled target fails the link, loudly, at build time —
// `wasm-ld: error: lto.tmp: undefined symbol: runtime.xorshift32StateTYPO`. So a future TinyGo
// that renames either global cannot turn this file into a silent no-op; it turns it into a
// build failure naming the symbol that moved. That property is why the names below can be
// trusted without a test asserting them, and it is the opposite of what `//go:linkname` gives.
//
// None of that is visible from a host build, where this file is excluded by its build tag. The
// reset is proved by behavior against a `wasm-unknown` artifact — map order restored across a
// call, with a neutered `ResetRand` as the control that shows the check can fail.

//go:extern runtime.xorshift32State
var xorshift32State uint32

//go:extern runtime.xorshift64State
var xorshift64State uint64

// ResetRand pins TinyGo's map-iteration pseudo-random generators to their initial position.
//
// Called at the top of every check and reduce. See the package documentation for why.
func ResetRand() {
	xorshift32State = 1
	xorshift64State = 1
}
