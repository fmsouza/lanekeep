//! The runtime: an engine, a store per worker, and the three limits neither can escape.
//!
//! The direct successor to `lanekeep_js::Sandbox`, and the same arrangement seen from the
//! other side. There a rayon worker owns a QuickJS `Runtime`; here it owns a
//! [`wasmtime::Store`], and the [`wasmtime::Engine`] the stores are built on is shared for the
//! whole run because it is the unit compiled code is cached in. What both share is
//! `lanekeep_core::limits`: one [`RunClock`], one [`Budget`] per runtime built from it, one
//! recorded [`lanekeep_core::limits::Trip`] read afterwards. Two clocks would each enforce
//! their own share of the global budget correctly while the run as a whole overran both.
//!
//! # How a limit stops a component, and where that differs from QuickJS
//!
//! QuickJS polls an interrupt handler every so many bytecodes. wasmtime has no instruction
//! counter in the same sense; what it has is **epoch interruption**, where Cranelift emits a
//! comparison against a shared counter at every function entry and every loop back-edge, and
//! the embedder advances that counter from outside. So this module runs one ticker thread per
//! engine (`EpochTicker`), and the deadline callback each store installs asks
//! [`Budget::should_interrupt`] exactly the question the QuickJS handler asks.
//!
//! Two consequences worth stating rather than discovering:
//!
//! - **Enabling epoch interruption changes every store's default.** wasmtime starts a store at
//!   deadline zero, which has always elapsed, so a store built on this engine traps
//!   immediately unless something calls [`wasmtime::Store::set_epoch_deadline`].
//!   [`WasmRuntime`] does that per invocation; a caller building a bare `Store` on
//!   [`engine`] has to do it too.
//! - **Time passing between invocations is invisible.** The epoch check is compiled into guest
//!   code, so it only runs while guest code runs. A rule that returns after a handful of
//!   operations can overrun the global budget without ever being asked to stop. That is the
//!   same gap `AGENTS.md` records against QuickJS's interrupt handler, for the same reason,
//!   and switching engines does not touch it — see `tests/limits.rs`, which measures it. The
//!   outer clock check that closes it is Task 17's, and it belongs outside both engines
//!   because both need it.
//!
//! # The two tunables that bake into every precompiled artifact
//!
//! [`MEMORY_RESERVATION`] and [`MEMORY_GUARD_SIZE`] are recorded in a `.cwasm` and checked when
//! one is loaded — read from `wasmtime-47.0.3/src/engine/serialization.rs:354-363`, where they
//! are two of the `check_int` calls, rather than inferred from the error text. A module
//! compiled under different values is refused, saying so exactly: "Module was compiled with a
//! memory reservation of '4294967296' but '1048576' is expected for the host". They are
//! therefore cache-key inputs (Task 15) and not settings anybody flips, which is why they are
//! constants here rather than literals at the [`wasmtime::Config`] call site.
//!
//! [`EPOCH_INTERRUPTION`] is checked by the same routine, one `check_bool` further down. It is a
//! third artifact-validity key and it is constant — this engine always enables it — but a change
//! of mind about that invalidates precompiled artifacts exactly as a change to the two sizes
//! would.
//!
//! **Those three plus the wasmtime version and the target triple are not the whole of it.**
//! `check_tunables` performs twenty-six comparisons — seven `check_int`, sixteen `check_bool`,
//! and one each of `check_collector`, `check_cost` and `check_inlining` — so twenty-three fields
//! beyond the three constants above are covered by the version and the triple **only if the
//! wasmtime feature set is pinned as well**. `component-model-async` moves `concurrency_support`
//! (`config.rs:2627`), `rr` moves `recording` (`:2631`), and `custom-virtual-memory` moves
//! `memory_reservation` and `memory_init_cow` (`:2654-2656`). Cargo's feature unification can
//! move any of them without the version moving, which would hand out a cache hit for an artifact
//! the engine cannot load.
//!
//! `signals_based_traps` is checked too and is *not* reachable this way — it is moved by the
//! build cfg `has_native_signals` rather than by a Cargo feature, and there is no
//! `signals-based-traps` feature in wasmtime 47.0.3. Worth stating because it is the example
//! this comment used to give, and an implementer would have gone looking for a feature that does
//! not exist.
//!
//! # Instantiation happens once per (worker, rule), and this module is what makes that true
//!
//! [`MEMORY_RESERVATION`]'s whole argument rests on that bound: roughly three hundred and
//! fifty instantiations for a run, against a 1.6× on guest compute that grows with the corpus.
//! It used to be a paragraph asking callers to behave. It is now the shape of the API:
//! [`RuleSet`] resolves each component's imports once for the whole run (`RulePre`, from
//! `Linker::instantiate_pre`), and each worker's [`WasmRuntime`] holds **one `Option` per
//! rule** — [`WasmRuntime::instantiations`] counts what it built — filled the first time that
//! rule is actually asked to run. At ten thousand files times ten rules the penalty of getting
//! this wrong is about three and a half seconds, and 4 GiB stops being defensible.
//!
//! **An `Option` per rule and not one flag per worker.** One lazy flag covering the whole
//! ruleset recovers only the all-cache-hits case and pays in full the moment a single file
//! misses. Measured 2026-08-05 at twenty rules and fourteen workers: eager instantiation costs
//! 12,010 µs from `Component::from_file`, 12,115 µs from a byte slice and 10,967 µs on the
//! mapped path, against 135.2, 125.8 and 133.4 µs for creating the same fourteen stores and
//! instantiating nothing — a factor of **82 to 96** (medians, n = 30 per arm). The
//! same-session QuickJS baseline is a 34.6 ms warm run, so eager instantiation adds about a
//! third to a warm run in order to be *able* to do work a warm run never does. This engine
//! already found and fixed the same shape once: `docs/architecture.md` §15 records a warm run
//! that built a QuickJS sandbox per worker and evaluated every rule module into it at 4.7×
//! the lazy version. The difference is that the QuickJS cost grew with *workers* and this one
//! grows with *rules × workers*, and `lanekeep.json` already runs twenty-three rules.
//!
//! Two adjacent hazards, both of which this arrangement is shaped around. `rayon`'s
//! `map_init` runs its initializer per chunk rather than per thread — an `AGENTS.md` trap —
//! so instantiating there would quietly turn "per component per worker" into something nearer
//! "per component per file"; nothing is instantiated in a constructor here, which makes where
//! the initializer runs irrelevant. And **a `Store` never reclaims an instance**: instantiating
//! repeatedly into one stops at 3,333 component instances, the 3,334th failing with `resource
//! limit exceeded: instance count too high at 10001`, three core instances per component
//! instance against wasmtime's default cap. `tests/instantiation.rs` asserts that number,
//! because it is the ceiling a per-file design would hit rather than merely be slow under.
//!
//! # A rule's `check` must be a pure function of its arguments
//!
//! Stated here because nobody had stated it for either engine, and reusing one instance across
//! every file a worker handles is what makes it load-bearing. A rule that carries state
//! between files is **outside what lanekeep's determinism invariant covers**: rayon's
//! work-stealing makes the file-to-worker assignment run-dependent, so such a rule's answer
//! depends on which files its worker happened to see first, and can differ between runs on
//! identical input.
//!
//! This is not new and components neither introduce it nor remove it — `docs/architecture.md`
//! §4 already records that each worker owns one JavaScript runtime and context "created once
//! per run and reused across that worker's files". What is new is that it is written down, so
//! the alternative is a decision rather than an inheritance. That alternative should be priced
//! when it is proposed rather than assumed cheap: a fresh instance per file per rule is 40,000
//! instantiations against 280 for the same corpus and ruleset, measured at 331.5 ms
//! single-threaded — a different cost class by a factor of 143.
//!
//! # No pooling allocator, and no dividing an instantiation cost by the worker count
//!
//! Both halves are measured, and both are conditions of the decision record's acceptance.
//!
//! **Pooling lost in all four configurations.** At fourteen workers it loses to the on-demand
//! allocator at both reservation sizes and in both component shapes — 18,033 against 13,679 µs
//! and 14,998 against 10,795 µs at the 4 GiB default, 7,657 against 6,010 µs and 3,881 against
//! 1,907 µs at 2 MiB. Its worst penalty, 2.04×, is in the cell that is otherwise the best and
//! had most reason to favor it: pre-mapped pool slots at a small reservation. Single-threaded
//! the two are within run-to-run noise and the sign flips, so no advantage should be claimed
//! there in either direction. There is no dependency-policy objection to add on top —
//! wasmtime defines `pooling-allocator = ["runtime", "std"]`, both already in the cleared set,
//! and `cargo tree --edges normal` gives 113 external packages with the feature and 113
//! without. The measurement is the whole argument, which is why this module's one
//! [`wasmtime::Config`] does not name an allocation strategy, and why turning one on is a
//! re-measurement rather than a flag.
//!
//! **Instantiation does not parallelize.** The same per-worker work at fourteen workers takes
//! **30 to 59×** as long as at one, against 1.5 to 2.1× for a pure-arithmetic control at the
//! same thread counts on the same machine. The mechanism is XNU's per-process `vm_map` lock,
//! taken exclusively for `mmap`, `munmap` and `mprotect`, so every mapping in a process
//! serializes against every other regardless of thread — demonstrated with a ninety-line C
//! program containing no wasmtime, where the same mapping pattern scales at 20.2× across
//! threads in one address space and 5.0× across forked processes doing identical work. So any
//! arithmetic of the form "one instantiation costs X, we have N workers, therefore X/N" is
//! wrong, and the lever worth designing around is fewer mappings per instance rather than more
//! threads. Every figure here is from one 14-core M3 Max running XNU; Linux's `mmap_lock` has
//! different characteristics and this does not transfer unmeasured, in either direction.
//!
//! Two settings that look as though they belong beside them do not. The growth reservation is
//! bound to `_` in that same routine, as a runtime setting that does not affect compilation —
//! and it is left at its default here for a second reason: it applies only once a memory
//! outgrows its reservation, and a 32-bit linear memory cannot outgrow 4 GiB.
//! [`EPOCH_TICK_INTERVAL`] is host-side entirely; the compiled code only ever compares against
//! a counter, so changing it changes when a breach is noticed and nothing about what was
//! compiled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use lanekeep_core::limits::{Budget, Limits, RunClock};
use wasmtime::component::{Component, HasSelf, Linker, Resource};
use wasmtime::{Config, Engine, ResourceLimiter, Store, UpdateDeadline};

use crate::bindings::{Rule, RulePre, types};
use crate::error::{WasmError, classify};
use crate::host::{CheckContext, HostState, ReduceContext};

/// Address space reserved ahead of time for each linear memory, in bytes.
///
/// **4 GiB, which is what `wasmtime` picks by default on a 64-bit host — and setting it
/// anyway is the point.** The default is platform-dependent (10 MiB on 32-bit), and this
/// number is recorded in every precompiled artifact, so a value that came from the platform
/// rather than from here would be a cache-key input nobody had written down.
///
/// # It is the value, and the value was not the expected one
///
/// The decision record behind this sub-project accepted shrinking it, on a measurement of
/// *instantiation*: 4 GiB to 2 MiB with no guard took the eager arm from 10,795 µs to 1,907 µs
/// at fourteen workers, because XNU takes a per-process `vm_map` lock exclusively for `mmap`,
/// `munmap` and `mprotect` and a smaller reservation shortens each hold. That effect is real
/// and it reproduces here. What it leaves out is **execution**, and execution moves further in
/// the other direction.
///
/// Measured in this workspace on 2026-08-06, Apple M3 Max, `aarch64-apple-darwin`, `rustc`
/// 1.95.0, `wasmtime` 47.0.3, against `tests/fixtures/limits.wasm`, **in the release profile**,
/// eleven interleaved rounds, reported as the **median**.
///
/// Two notes on the method, both of which changed the numbers that were here before:
///
/// - **Minimum-of-N is the wrong statistic for this measurement.** These timings are bimodal —
///   a 4 GiB/0 `strided` call ran 120,607 µs at its fastest and 143,022 µs at its median across
///   eleven rounds on identical bytes — so a minimum reports whichever arm happened to catch the
///   rare fast state. An earlier version of this table used minima and manufactured an 18%
///   result that does not exist.
/// - **The first version of these figures was taken on a contended machine.** An orphaned test
///   binary from a mutation run was holding six of fourteen cores. Nothing it changed survived
///   into this table, but it is why the numbers moved.
///
/// - **strided** — one `strided 100` call: a hundred million iterations reading three fields of
///   a struct through one opaque index, so its cost is dominated by bounds checks.
/// - **work** — one `work 300` call: three hundred million `black_box`ed iterations, so its cost
///   is dominated by ordinary loads and stores through the memory base.
///
/// | reservation / guard | strided | work | artifact |
/// |---|---|---|---|
/// | **4 GiB / 32 MiB (shipped)** | **86,229 µs** | **145,488 µs** | **145,792 B** |
/// | 4 GiB / 0 | 143,022 µs | 146,996 µs | 162,184 B |
/// | 16 MiB / 32 MiB | 139,200 µs | 238,977 µs | 162,184 B |
/// | 16 MiB / 0 | 144,878 µs | 239,176 µs | 162,184 B |
///
/// # Two independent mechanisms, and the 2×2 separates them
///
/// Read down a column and the story is not one effect but two, each needing a different setting
/// and each showing up on a different workload. This is worth the space because an earlier
/// version of this comment attributed the whole result to one of them and was wrong about which.
///
/// **The reservation buys a non-relocatable memory.** `Memory::memory_may_move`
/// (`wasmtime-environ-47.0.3/src/types.rs:2331-2353`) returns false when the memory's maximum
/// does not exceed the reservation; a 32-bit memory with no declared maximum has a maximum of
/// 4 GiB, so that is false at exactly 4 GiB and true below it. A memory that cannot move has a
/// compile-time-constant base and bound instead of two vmcontext loads per access. That is the
/// `work` column, and it is visible **with the guard at zero**, where nothing this probe does is
/// elided — the margin there is one byte, so a single-byte access would still qualify and a
/// `u64` one does not: 146,996 µs against 239,176 µs, a factor of **1.63**.
///
/// **The reservation and the guard together buy bounds-check elision.** The condition is
/// `u32::MAX <= reservation + guard - offset_and_size`
/// (`wasmtime-internal-cranelift-47.0.3/src/bounds_checks.rs:299-300`) — so it needs *both*, and
/// 4 GiB with no guard misses it by a byte for any access wider than one. That is the `strided`
/// column, and it is visible **at a fixed 4 GiB reservation**, where `memory_may_move` is
/// already false either way: 86,229 µs against 143,022 µs, a factor of **1.66**.
///
/// The two are separable because each has a row where the other is held constant, and the
/// artifact size is the independent witness: **145,792 bytes appears in exactly the one arm
/// where elision fires**, and every arm that emits bounds checks is 162,184 bytes.
///
/// Together, dropping the reservation to 16 MiB costs **1.61× on `strided` and 1.64× on `work`**.
///
/// # What it costs, and the assumption that bound depends on
///
/// About **10–13 ms** of instantiation per run at fourteen workers, for three hundred and fifty
/// instantiations. Measured here at 11.7 ms against 1.9 ms for a 16 MiB reservation; an
/// independent reader measured the delta at 12.6 ms.
///
/// The reason that is affordable is not its size but its **shape**: the instantiation penalty is
/// bounded by workers × rules and does not grow with the corpus, while the 1.6× applies to guest
/// compute, which grows with corpus × matches. On a run small enough for instantiation to
/// dominate, twelve milliseconds is imperceptible; on a run large enough to care about,
/// execution dominates.
///
/// **That argument depends on an instance per (worker, rule) rather than per file, and that is
/// now enforced rather than assumed.** [`RuleSet`] resolves each component once for the run and
/// each worker's [`WasmRuntime`] holds one `Option` per rule, filled by [`WasmRuntime::rule`]
/// and by nothing else — so a cache sized to the ruleset is the ceiling, and there is nowhere
/// to put a second instance of a rule. `tests/instantiation.rs` asserts it, and the two eager
/// designs it is written against fail it.
///
/// The reason it is worth enforcing rather than documenting: at ten thousand files times ten
/// rules the penalty is on the order of three and a half seconds and 4 GiB stops being
/// defensible — so an engine that instantiated per file would have invalidated this choice, not
/// merely made it slower. It would also not get that far. Under the shipped 64 MiB per-worker
/// ceiling one store accepts about sixty instances of `tests/fixtures/world-shape.wasm` before
/// [`WasmError::MemoryExceeded`], because linear memories never shrink and `MemoryCeiling` sums
/// every grant; with that ceiling raised, wasmtime's own cap stops it at 3,333. If the shape
/// ever changes, this constant is one of the things that has to be re-measured rather than
/// inherited.
///
/// This reverses what the decision record expected without contradicting what it measured: the
/// design spike measured instantiation and never measured execution. It is written down here
/// because the number cannot be revisited cheaply — it is baked into every `.cwasm`, so changing
/// it invalidates every cached result in every checkout.
pub const MEMORY_RESERVATION: u64 = 4 * 1024 * 1024 * 1024;

/// Unmapped guard region placed after each linear memory, in bytes.
///
/// **32 MiB, which is wasmtime's default on a 64-bit host.** Set explicitly for the reason
/// [`MEMORY_RESERVATION`] is: the default is platform-dependent, and this number is recorded in
/// every precompiled artifact.
///
/// At the shipped 4 GiB reservation this is what turns bounds-check elision on — see the second
/// mechanism on [`MEMORY_RESERVATION`], and note that it takes both settings, not either. On
/// `strided` it is worth **1.66×**: 86,229 µs against 143,022 µs with the guard at zero.
///
/// # A zero guard was measured, shipped, and reversed
///
/// Recorded rather than quietly corrected, because the way it was wrong is more useful than the
/// value. Three claims were made for it and none of them survived:
///
/// - **"It is 18% faster on `work`."** It is not. That came from taking minima of a bimodal
///   distribution on a machine with an orphaned test binary on six of fourteen cores. Measured
///   cleanly, by median, a zero guard is about **1% slower** on `work` — 146,996 µs against
///   145,488 µs — and there is no workload here that prefers it.
/// - **"The mechanism is `guard_before_linear_memory` costing extra mappings."** No: the step is
///   at any nonzero guard, and 4 KiB behaves like 32 MiB, which is a codegen switch rather than
///   a mapping cost.
/// - **"The mechanism is the GVN'd partial check at `bounds_checks.rs:423`."** Also no, and this
///   one is worth being precise about because it is nearly right. That path is real, but it
///   requires `offset_and_size <= memory_guard_size` *and* is only reached when elision did not
///   fire — so at a 4 GiB reservation neither shipped arm takes it. It is reachable at a small
///   reservation, and there it is worth about **4%**: `16 MiB / 32 MiB` at 139,200 µs against
///   `16 MiB / 0` at 144,878 µs. Against elision's 1.66× it is a rounding error, and the
///   artifact size proves the two apart — the GVN arm emits 162,184 bytes, the same as every
///   other bounds-checked configuration, where the elided arm emits 145,792.
///
/// So the guard's win here is elision, which is the mechanism [`MEMORY_RESERVATION`] needs the
/// guard for. Both constants describe one decision.
///
/// Like the reservation, this is baked into every precompiled artifact, so it is a cache-key
/// input rather than a knob.
pub const MEMORY_GUARD_SIZE: u64 = 32 * 1024 * 1024;

/// Whether the engine compiles epoch checks into guest code.
///
/// A constant rather than a literal at the [`wasmtime::Config`] call site, for the same reason
/// the two sizes above are: **it is an artifact-validity key**, checked by the same
/// `wasmtime-47.0.3/src/engine/serialization.rs` routine — a `check_bool` a few lines below the
/// two `check_int` calls that compare the reservation and the guard. A `.cwasm` compiled
/// without epoch checks is not loadable by an engine that has them, because the checks are
/// compiled *in*. Task 15 hashes what it can read, and a bare `true` inside a function is not
/// something it can read.
///
/// It is always `true` and there is no way to ask for `false`. Turning it off would remove both
/// wall-clock limits.
pub const EPOCH_INTERRUPTION: bool = true;

/// How often the ticker advances the engine's epoch.
///
/// This is the resolution of both wall-clock limits, and it is a floor on how long a runaway
/// rule keeps running after its budget is spent: the guest is stopped at the first epoch check
/// it reaches after the tick that crosses the deadline. One millisecond against a default
/// per-invocation budget of one second is a 0.1% overshoot, which is far below the noise in
/// the measurement it bounds.
///
/// It is not a cache-key input. The ticker is host-side and the compiled code only ever
/// compares against a counter, so the interval changes when a breach is noticed and nothing
/// about what was compiled.
pub const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(1);

/// The [`wasmtime::Config`] every engine in this crate is built from.
///
/// One configuration, not one per call site. Two engines configured differently would compile
/// artifacts neither could load from the other, and the failure would surface as a cache miss
/// nobody could explain.
fn config() -> Config {
    let mut config = Config::new();
    config
        .epoch_interruption(EPOCH_INTERRUPTION)
        .memory_reservation(MEMORY_RESERVATION)
        .memory_guard_size(MEMORY_GUARD_SIZE);
    config
}

/// Builds the [`Engine`] rule components execute on.
///
/// One `Engine` is shared across a run. It is the unit wasmtime caches compiled code in, so
/// building a second one would recompile everything the first already holds.
///
/// **A store built directly on this engine must set an epoch deadline before it calls a
/// guest.** Epoch interruption is on, and wasmtime starts every store at deadline zero, which
/// has always elapsed. [`WasmRuntime`] handles that per invocation; a caller assembling its
/// own store — the tests in this crate that predate this module do — needs
/// [`wasmtime::Store::set_epoch_deadline`].
///
/// # Errors
///
/// Returns the error `wasmtime` reports when the configuration cannot be realized on this
/// host — a compiler backend the target does not support, or a memory tunable the platform
/// rejects.
pub fn engine() -> wasmtime::Result<Engine> {
    Engine::new(&config())
}

/// The thread that advances an engine's epoch, and stops when the run does.
///
/// One per [`WasmEngine`], which is one per run. `AGENTS.md` names the shape this must not
/// take: a `--watch` loop or a long-lived server that leaked one of these per iteration would
/// accumulate threads for the life of the process, each advancing a counter nothing reads.
/// The [`Drop`] impl is what prevents it.
///
/// The wait is a [`Condvar`] rather than a sleep, and that is load-bearing rather than tidy:
/// `tests/limits.rs` builds an engine with an interval measured in hours to pin down what
/// happens when the epoch provably cannot advance, and a sleeping thread would make dropping
/// that engine take hours.
struct EpochTicker {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    /// Start ticking. The thread holds a weak reference, so it cannot keep the engine alive.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] when the thread cannot be spawned. **Fallible on
    /// purpose**, and this is the one place in this module where discarding an error would
    /// break the invariant it exists to protect: both wall-clock limits are enforced by this
    /// thread advancing the epoch, so a runtime built without one runs every rule under no
    /// timeout at all. Swallowing the failure would be a limit that degraded silently, in the
    /// module that owns "limits cancel the run; they never degrade it".
    fn start(engine: &Engine, interval: Duration) -> Result<Self, WasmError> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = Arc::clone(&stop);
        let weak = engine.weak();

        let handle = std::thread::Builder::new()
            .name("lanekeep-epoch".to_owned())
            .spawn(move || {
                let (lock, wake) = &*signal;
                loop {
                    // A poisoned mutex means the ticker's own thread panicked, which it
                    // cannot: nothing in this loop can panic. Treating poison as "stop" keeps
                    // the thread from spinning if the impossible happens, and costs the run
                    // nothing that is not already lost.
                    let Ok(stopped) = lock.lock() else { return };

                    // `wait_timeout_while` and not `wait_timeout`, because the predicate has
                    // to be tested *before* the wait as well as after it. Written the other
                    // way round this loop waits the full interval even when the stop flag was
                    // already set — the drop below runs to completion, notifies a condvar
                    // nobody is waiting on yet, and only then does the ticker reach its first
                    // wait. With an interval measured in hours that is indistinguishable from
                    // a hang, which is exactly how it was found.
                    let Ok((stopped, _)) = wake.wait_timeout_while(stopped, interval, |s| !*s)
                    else {
                        return;
                    };
                    if *stopped {
                        return;
                    }
                    drop(stopped);

                    // The engine is gone, so nothing is left to interrupt.
                    let Some(engine) = weak.upgrade() else { return };
                    engine.increment_epoch();
                }
            })
            .map_err(|e| {
                WasmError::Engine(format!(
                    "could not start the epoch ticker thread: {e}\n  \
                     both the per-invocation and the global wall-clock budgets are enforced by \
                     that thread advancing the engine's epoch, so a runtime built without one \
                     would run every rule under no timeout at all"
                ))
            })?;

        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stop;
        if let Ok(mut stopped) = lock.lock() {
            *stopped = true;
        }
        wake.notify_all();
        if let Some(handle) = self.handle.take() {
            // A ticker whose thread panicked is a ticker that stopped ticking, which the run
            // will notice as a budget that never fires. Nothing useful can be done about it
            // here, and propagating a panic out of a `Drop` would abort the process.
            drop(handle.join());
        }
    }
}

/// An engine and the epoch ticker that bounds what runs on it.
///
/// Shared across a run: every worker's [`WasmRuntime`] holds the same one, so there is one
/// compiled-code cache and one ticker rather than one of each per thread.
pub struct WasmEngine {
    /// Held for its [`Drop`], which is the whole of what it does from this type's point of
    /// view — hence the underscore, which is the difference between a field nothing reads and
    /// a field nothing *uses*.
    ///
    /// Declared first so it is joined before the engine it ticks is dropped. Correctness does
    /// not depend on the order — the thread holds a weak reference and checks it — but a
    /// shutdown that tears down in the order it was built in is easier to reason about.
    _ticker: EpochTicker,
    engine: Engine,
}

impl std::fmt::Debug for WasmEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmEngine").finish_non_exhaustive()
    }
}

impl WasmEngine {
    /// Build an engine ticking at [`EPOCH_TICK_INTERVAL`].
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] when `wasmtime` cannot realize the configuration, which
    /// means a broken build rather than anything about a rule.
    pub fn new() -> Result<Arc<Self>, WasmError> {
        Self::with_tick_interval(EPOCH_TICK_INTERVAL)
    }

    /// Build an engine ticking at a chosen interval.
    ///
    /// For a test that has to know whether a tick could have landed inside a call. A run uses
    /// [`WasmEngine::new`]; the interval is not configuration a user sets, because it trades
    /// the resolution of every limit against a cost nobody has measured a reason to pay.
    ///
    /// # Errors
    ///
    /// As [`WasmEngine::new`].
    pub fn with_tick_interval(interval: Duration) -> Result<Arc<Self>, WasmError> {
        let engine = engine().map_err(|e| WasmError::Engine(e.to_string()))?;
        let ticker = EpochTicker::start(&engine, interval)?;
        Ok(Arc::new(Self {
            _ticker: ticker,
            engine,
        }))
    }

    /// The engine itself, for compiling components.
    #[must_use]
    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile a component from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] when the bytes are not a valid component, or declare a
    /// feature this engine does not enable.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, WasmError> {
        Component::new(&self.engine, bytes).map_err(|e| WasmError::Engine(e.to_string()))
    }
}

/// Where a rule sits in a [`RuleSet`], and therefore which `Option` holds its instance.
///
/// An index rather than a name, because it is read on the hot path — once per rule per file —
/// and because it is what makes the per-worker cache a `Vec` of `Option`s with no lookup at
/// all. Only meaningful against the set that issued it; [`RuleSet`] and every
/// slot-taking method on [`WasmRuntime`] refuse one that is out of range rather than
/// panicking, because a slot arrives from a caller and an engine that panics on a malformed
/// input has failed at its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleSlot(usize);

impl RuleSlot {
    /// The index, for a caller keeping its own parallel array.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// One rule, resolved against the host world once for the whole run.
struct Prepared {
    /// Whatever identifies the rule to a reader — a rule id. Diagnostics only.
    id: String,
    pre: RulePre<RuntimeState>,
}

/// Every rule component a run will execute, linked and type-checked once.
///
/// Shared by every worker, which is the point: `Linker::instantiate_pre` resolves and
/// type-checks a component's imports independently of how many stores end up instantiating
/// it, so doing it per worker would repeat that work fourteen times for an answer that cannot
/// differ. The [`wasmtime::component::Linker`] lives here for the same reason — it is built
/// from the host world and carries no per-store state.
///
/// Built before the workers start and then shared behind an [`Arc`]. Nothing here is
/// instantiated: an instance belongs to a store, and a store belongs to a worker.
pub struct RuleSet {
    linker: Linker<RuntimeState>,
    rules: Vec<Prepared>,
}

impl std::fmt::Debug for RuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSet")
            .field(
                "rules",
                &self.rules.iter().map(|rule| &rule.id).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl RuleSet {
    /// An empty set, with the host world linked and no rules in it.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] if the world cannot be linked, which means a broken
    /// build rather than anything about a rule.
    pub fn new(engine: &WasmEngine) -> Result<Self, WasmError> {
        let mut linker = Linker::new(engine.engine());
        Rule::add_to_linker::<RuntimeState, HasSelf<HostState>>(&mut linker, |state| {
            &mut state.host
        })
        .map_err(|e| WasmError::Engine(e.to_string()))?;
        Ok(Self {
            linker,
            rules: Vec::new(),
        })
    }

    /// Resolve a component against the host world and give it a slot.
    ///
    /// This is where a component's imports are matched against what the host provides and
    /// its exports are checked against the world — once, for the run. Everything a worker
    /// does afterwards is instantiation.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] when the component does not satisfy the world: an import
    /// the host does not provide, or a missing or wrongly typed export. A component that
    /// reaches for capability the sandbox withholds is refused earlier and more specifically,
    /// by `crate::load::check_imports`, which does not depend on the host declining to link
    /// it.
    pub fn add(
        &mut self,
        id: impl Into<String>,
        component: &Component,
    ) -> Result<RuleSlot, WasmError> {
        let pre = self
            .linker
            .instantiate_pre(component)
            .and_then(RulePre::new)
            .map_err(|e| WasmError::Engine(e.to_string()))?;
        let slot = RuleSlot(self.rules.len());
        self.rules.push(Prepared { id: id.into(), pre });
        Ok(slot)
    }

    /// How many rules are in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the set holds no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every slot in the set, in the order the rules were added.
    pub fn slots(&self) -> impl Iterator<Item = RuleSlot> {
        (0..self.rules.len()).map(RuleSlot)
    }

    /// What a slot's rule is called, or `None` for a slot this set never issued.
    #[must_use]
    pub fn id(&self, slot: RuleSlot) -> Option<&str> {
        self.rules.get(slot.0).map(|rule| rule.id.as_str())
    }

    /// The pre-instantiation a slot names.
    fn rule(&self, slot: RuleSlot) -> Result<&Prepared, WasmError> {
        self.rules.get(slot.0).ok_or_else(|| {
            WasmError::Engine(format!(
                "rule slot {} is not in this rule set, which holds {}\n  \
                 a slot is only meaningful against the set that issued it",
                slot.0,
                self.rules.len()
            ))
        })
    }
}

/// The memory ceiling, summed across every linear memory in one store.
///
/// **Hand-written rather than `StoreLimitsBuilder::memory_size`, and the difference is the
/// point.** wasmtime's own helper applies its ceiling to each linear memory independently: it
/// compares `desired` against the limit and knows nothing about the other memories in the
/// store. A store holds one instance per rule, so under that helper a worker running twenty
/// rules could reach twenty times the budget it was given, and nothing would say so.
///
/// What this tracks instead is the running total of every growth this store has been asked
/// for. Linear memories never shrink and a store never releases an instance, so the total is
/// monotonic and needs no bookkeeping per memory.
///
/// Growth is refused with an **error rather than `Ok(false)`**. `Ok(false)` makes
/// `memory.grow` return -1, which is a value the guest may ignore — a rule that handled it and
/// carried on would have degraded the run instead of ending it, which is exactly the
/// behavior `docs/architecture.md` §6.8 rules out. An error traps the guest where it stands.
struct MemoryCeiling {
    /// The ceiling in bytes, from `Limits::memory_bytes`.
    ceiling: usize,
    /// Every byte of linear memory and table storage this store has been granted.
    total: usize,
    /// The ceiling, recorded when it was breached, and taken by the classifier.
    breached: Option<usize>,
}

impl MemoryCeiling {
    const fn new(ceiling: usize) -> Self {
        Self {
            ceiling,
            total: 0,
            breached: None,
        }
    }

    /// Forget a breach recorded by an earlier invocation.
    ///
    /// Mirrors `Budget::arm` clearing the trip record: a breach reported twice would blame the
    /// next invocation for a ceiling the previous one hit. Note that `total` is deliberately
    /// *not* reset — the memory is still held.
    const fn arm(&mut self) {
        self.breached = None;
    }

    /// The ceiling that was breached, if one was. Clears the record.
    const fn take_breach(&mut self) -> Option<usize> {
        self.breached.take()
    }

    /// Account for a request to grow, refusing what would cross the ceiling.
    fn admit(&mut self, current: usize, desired: usize, what: &str) -> wasmtime::Result<bool> {
        let requested = desired.saturating_sub(current);
        let proposed = self.total.saturating_add(requested);
        if proposed > self.ceiling {
            self.breached = Some(self.ceiling);
            return Err(wasmtime::Error::msg(format!(
                "growing {what} by {requested} bytes would take this store to {proposed} \
                 bytes, past its {} byte ceiling",
                self.ceiling
            )));
        }
        self.total = proposed;
        Ok(true)
    }
}

impl ResourceLimiter for MemoryCeiling {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.admit(current, desired, "linear memory")
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Counted against the same total, in bytes. wasmtime hands table sizes in elements and
        // documents each as costing a pointer's worth of space. A table is small next to a
        // linear memory and a Rust guest never grows one — but a ceiling that only covered
        // linear memory would leave `table.grow` as an unbounded allocation path, and leaving
        // one open because today's guests do not use it is how it stays open.
        let width = size_of::<usize>();
        self.admit(
            current.saturating_mul(width),
            desired.saturating_mul(width),
            "a table",
        )
    }
}

/// The store's data: the host API's state, and the ceiling over it.
///
/// Two fields rather than one because they answer to different owners. [`HostState`] is the
/// world's — every method in `crate::host` reads it — and the ceiling is the runtime's, read
/// by wasmtime through [`wasmtime::Store::limiter`] and by `WasmRuntime::classify`. Putting
/// the ceiling inside `HostState` would make a limit look like part of the host API.
struct RuntimeState {
    host: HostState,
    memory: MemoryCeiling,
}

/// A WebAssembly component runtime that rule components execute in.
///
/// Not `Sync`: a [`wasmtime::Store`] is single-threaded, so each rayon worker owns one. They
/// share a [`WasmEngine`], so there is one compiled-code cache and one epoch ticker, a
/// [`RuleSet`], so each component's imports are resolved once for the run rather than once
/// per worker, and a [`RunClock`], so the global budget is measured once for the run rather
/// than once per worker.
pub struct WasmRuntime {
    engine: Arc<WasmEngine>,
    rules: Arc<RuleSet>,
    /// One `Option` per rule in [`Self::rules`], filled on that rule's first use in this
    /// store.
    ///
    /// **Per rule and not per worker, and that is the whole of condition 1.** A single flag
    /// covering the ruleset recovers only the all-cache-hits case and pays for every rule the
    /// moment one file misses; this pays for exactly the rules that ran. Sized once at
    /// construction, so a slot lookup is an index and the hot path allocates nothing.
    ///
    /// Nothing ever clears an entry. A `Store` does not reclaim an instance — instantiating
    /// repeatedly into one stops at 3,333 — so an entry that could be dropped and rebuilt
    /// would consume a slot without releasing one.
    instances: Vec<Option<Rule>>,
    /// How many instances this store has built, as evidence.
    ///
    /// Nothing in execution reads it. It exists because "instantiation is lazy" and
    /// "instantiation is reused" are both claims a test can only make about a number, and a
    /// counter kept by a test harness proves something about the harness. This one is
    /// incremented in the single place that instantiates.
    instantiations: usize,
    store: Store<RuntimeState>,
    limits: Limits,
    budget: Arc<Budget>,
    /// Set once the epoch callback has fired for this store, purely as evidence.
    ///
    /// Nothing in enforcement reads it. It exists because a budget test can pass for the wrong
    /// reason — because the work was fast rather than because the limit was enforced — and
    /// this is what lets `tests/limits.rs` tell those apart.
    interrupted: Arc<AtomicBool>,
}

impl std::fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmRuntime")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WasmRuntime {
    /// Build a runtime sharing an engine and a run clock.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] if the world cannot be linked, which indicates a broken
    /// build rather than anything about a rule.
    pub fn new(
        engine: Arc<WasmEngine>,
        limits: Limits,
        clock: Arc<RunClock>,
    ) -> Result<Self, WasmError> {
        let rules = Arc::new(RuleSet::new(&engine)?);
        Ok(Self::for_rules(engine, rules, limits, clock))
    }

    /// Build a worker's runtime over the run's rule set.
    ///
    /// **The per-worker constructor, and it instantiates nothing.** That is deliberate rather
    /// than incidental: `rayon`'s `map_init` runs its initializer per chunk rather than per
    /// thread, so any instantiation done here would multiply by chunks instead of by workers.
    /// What it allocates is one `None` per rule.
    ///
    /// Infallible, and that is a consequence rather than a convenience: the one thing that
    /// could fail — linking the host world — happens once in [`RuleSet::new`] for the whole
    /// run, so a worker cannot fail to start for a reason the run had not already found.
    #[must_use]
    pub fn for_rules(
        engine: Arc<WasmEngine>,
        rules: Arc<RuleSet>,
        limits: Limits,
        clock: Arc<RunClock>,
    ) -> Self {
        let instances = (0..rules.len()).map(|_| None).collect();

        let mut store = Store::new(
            engine.engine(),
            RuntimeState {
                host: HostState::new(),
                memory: MemoryCeiling::new(limits.memory_bytes),
            },
        );
        store.limiter(|state| &mut state.memory);

        let budget = Budget::new(clock);
        let interrupted = Arc::new(AtomicBool::new(false));

        let callback_budget = Arc::clone(&budget);
        let callback_flag = Arc::clone(&interrupted);
        store.epoch_deadline_callback(move |_| {
            callback_flag.store(true, Ordering::Relaxed);
            if callback_budget.should_interrupt() {
                // An error rather than `UpdateDeadline::Interrupt`, which would also trap. The
                // difference is the message: `Interrupt` produces wasmtime's own generic
                // "epoch deadline reached" text, and this says which budget and how long, so a
                // run whose `Trip` record was somehow lost still reports something true.
                return Err(wasmtime::Error::msg(format!(
                    "execution was stopped after {:?}: a lanekeep budget is spent",
                    callback_budget.clock().elapsed()
                )));
            }
            Ok(UpdateDeadline::Continue(1))
        });

        Self {
            engine,
            rules,
            instances,
            instantiations: 0,
            store,
            limits,
            budget,
            interrupted,
        }
    }

    /// Build a runtime with its own engine and run clock, starting now.
    ///
    /// For a single-threaded run or a test. A real run shares one engine and one clock across
    /// workers — an engine per worker would recompile every component per worker, and a clock
    /// per worker would multiply the global budget by the worker count.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::new`].
    pub fn with_limits(limits: Limits) -> Result<Self, WasmError> {
        let clock = RunClock::start(limits.global_timeout);
        Self::new(WasmEngine::new()?, limits, clock)
    }

    /// The budgets in force.
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The engine this runtime's store was built on, for compiling components.
    #[must_use]
    pub fn engine(&self) -> &Arc<WasmEngine> {
        &self.engine
    }

    /// Whether the epoch deadline callback has fired for this store since it was built.
    ///
    /// Evidence, not enforcement. A test asserting that a call was stopped by a limit and not
    /// merely that it finished needs to know the mechanism ran at all; without this, a run
    /// that completed because the guest was fast is indistinguishable from one that completed
    /// because the ticker never ticked.
    #[must_use]
    pub fn epoch_fired(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }

    /// The host state, for reading back what a rule reported.
    #[must_use]
    pub fn host(&self) -> &HostState {
        &self.store.data().host
    }

    /// The host state, for lending a context to a guest.
    pub fn host_mut(&mut self) -> &mut HostState {
        &mut self.store.data_mut().host
    }

    /// Instantiate a compiled component in this runtime's store.
    ///
    /// Budgeted like any other guest call: instantiation runs the component's own
    /// initialization, and it is where a linear memory's minimum is first allocated — so it is
    /// the first thing that can breach the memory ceiling, before a rule has run a line.
    ///
    /// Two things follow that are worth knowing before relying on either. `lanekeep_js::Sandbox`
    /// deliberately runs its bootstrap *before* installing the interrupt handler, so that an
    /// expired run reports its timeout rather than "could not build a sandbox"; the reasoning
    /// does not carry over, because this runs guest code and allocates a guest's memory, and
    /// because what it reports when the run is spent is [`WasmError::RunTimeout`] — the true
    /// diagnostic rather than the misleading one. And whether a spent run budget is *noticed*
    /// during an instantiation is a race: instantiating this crate's fixtures takes tens of
    /// microseconds and the epoch advances every millisecond, so the check that would catch it
    /// usually is not reached. That is the same gap the module header describes, seen at the
    /// shortest call there is, and it is Task 17's to close rather than something to paper over
    /// with a second clock read here.
    ///
    /// # How often this may be called is a load-bearing assumption, and this is the raw form
    ///
    /// [`MEMORY_RESERVATION`] is chosen on the basis that instantiation happens **per (worker,
    /// rule)**. What makes that true is [`RuleSet`] plus the per-rule `Option` behind
    /// [`WasmRuntime::rule`], which instantiates at most once per slot per store. This method
    /// is underneath that: it takes a `Component` rather than a slot, so it resolves the
    /// component's imports on every call and keeps no instance, and calling it per file is
    /// exactly the shape the bound rules out. It stays public because the fixture tests in
    /// this crate drive one component directly and have no ruleset to speak of.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached limit, or [`WasmError::Engine`] when the component
    /// does not satisfy the world.
    pub fn instantiate(&mut self, component: &Component) -> Result<Rule, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = Rule::instantiate(&mut self.store, component, &self.rules.linker);
        self.disarm();
        self.instantiations += 1;
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// The rule set this runtime executes against.
    #[must_use]
    pub fn rules(&self) -> &Arc<RuleSet> {
        &self.rules
    }

    /// How many instances this store has built since it was created.
    ///
    /// Evidence, exactly as [`WasmRuntime::epoch_fired`] is. Two claims depend on it and
    /// neither can be made about anything else: that a worker whose files all hit the cache
    /// instantiates *nothing*, and that a worker handling three files that match the same rule
    /// instantiates that rule *once*. A counter kept by a test harness would prove a property
    /// of the harness.
    #[must_use]
    pub const fn instantiations(&self) -> usize {
        self.instantiations
    }

    /// Whether a slot's rule has been instantiated in this store yet.
    #[must_use]
    pub fn is_instantiated(&self, slot: RuleSlot) -> bool {
        self.instances
            .get(slot.index())
            .is_some_and(Option::is_some)
    }

    /// This store's instance of a rule, building it on first use.
    ///
    /// **The lazy per-rule instance cache, and condition 1 in one method.** The first call for
    /// a slot instantiates; every later call for the same slot returns what that one built.
    /// A rule whose query never matches anything this worker saw is never instantiated at all,
    /// which is the case eager instantiation pays 82 to 96× for.
    ///
    /// A failure is not remembered. `Worker::sandbox` in `lanekeep-engine` caches its build
    /// failure so it is reported once per worker rather than retried per file, and the
    /// reasoning does not carry: every failure this can return cancels the run, so there is no
    /// next file to retry for.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached limit — instantiation runs the component's own
    /// initialization and allocates its linear memory's minimum — or [`WasmError::Engine`] for
    /// a slot this runtime's rule set never issued.
    pub fn rule(&mut self, slot: RuleSlot) -> Result<&Rule, WasmError> {
        if !self.is_instantiated(slot) {
            let rules = Arc::clone(&self.rules);
            let pre = &rules.rule(slot)?.pre;

            let timeout = self.limits.rule_timeout;
            self.arm(timeout);
            let outcome = pre.instantiate(&mut self.store);
            self.disarm();
            let instance = outcome.map_err(|error| self.classify(&error, timeout))?;

            self.instantiations += 1;
            // `rules.rule(slot)` above already refused an out-of-range slot, and `instances`
            // is sized from the same set, so this index is in range. `get_mut` rather than
            // `[]` regardless: the workspace denies panicking outside tests, and an engine
            // that panics on a malformed input has failed at its job.
            if let Some(entry) = self.instances.get_mut(slot.index()) {
                *entry = Some(instance);
            }
        }

        self.instances
            .get(slot.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                WasmError::Engine(format!(
                    "rule slot {} was instantiated and then could not be found",
                    slot.index()
                ))
            })
    }

    /// Ask a slot's rule whether it has a per-file pass, instantiating it if needed.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn has_check(&mut self, slot: RuleSlot) -> Result<bool, WasmError> {
        self.rule(slot)?;
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_has_check(store));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Ask a slot's rule whether it has a cross-file pass, instantiating it if needed.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn has_reduce(&mut self, slot: RuleSlot) -> Result<bool, WasmError> {
        self.rule(slot)?;
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_has_reduce(store));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run a slot's per-file pass for one query match, under the default budget.
    ///
    /// **This is the call the bound is about.** It is reached once per (rule, match) and
    /// instantiates at most once per (worker, rule), because the instance it needs is the one
    /// [`WasmRuntime::rule`] cached.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn check(
        &mut self,
        slot: RuleSlot,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
    ) -> Result<(), WasmError> {
        self.check_with_timeout(slot, context, captures, self.limits.rule_timeout)
    }

    /// Run a slot's per-file pass under an explicit budget, for a rule that declared its own.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn check_with_timeout(
        &mut self,
        slot: RuleSlot,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        self.rule(slot)?;
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome =
            self.with_instance(slot, |rule, store| rule.call_check(store, borrow, captures));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run a slot's cross-file pass, under the default budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn reduce(
        &mut self,
        slot: RuleSlot,
        context: &Resource<ReduceContext>,
    ) -> Result<(), WasmError> {
        self.reduce_with_timeout(slot, context, self.limits.rule_timeout)
    }

    /// Run a slot's cross-file pass under an explicit budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn reduce_with_timeout(
        &mut self,
        slot: RuleSlot,
        context: &Resource<ReduceContext>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        self.rule(slot)?;
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_reduce(store, borrow));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Call into a slot's already-built instance.
    ///
    /// The instance and the store are separate fields, so lending one immutably while the
    /// other is borrowed mutably is a disjoint-field borrow rather than a problem. It is a
    /// closure and not a returned reference because the caller needs `&mut self` again
    /// afterwards, to classify.
    ///
    /// The caller is expected to have called [`WasmRuntime::rule`] first; a slot with no
    /// instance is reported rather than instantiated here, so that "this call instantiated"
    /// stays true of exactly one place.
    fn with_instance<T>(
        &mut self,
        slot: RuleSlot,
        call: impl FnOnce(&Rule, &mut Store<RuntimeState>) -> wasmtime::Result<T>,
    ) -> wasmtime::Result<T> {
        let Some(rule) = self.instances.get(slot.index()).and_then(Option::as_ref) else {
            return Err(wasmtime::Error::msg(format!(
                "rule slot {} has no instance in this store",
                slot.index()
            )));
        };
        call(rule, &mut self.store)
    }

    /// Ask a rule whether it has a per-file pass.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_has_check(&mut self, rule: &Rule) -> Result<bool, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = rule.call_has_check(&mut self.store);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Ask a rule whether it has a cross-file pass.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_has_reduce(&mut self, rule: &Rule) -> Result<bool, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = rule.call_has_reduce(&mut self.store);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run a rule's per-file pass for one query match, under the default budget.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached budget or a trapping guest. Every variant cancels
    /// the run.
    pub fn call_check(
        &mut self,
        rule: &Rule,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
    ) -> Result<(), WasmError> {
        self.call_check_with_timeout(rule, context, captures, self.limits.rule_timeout)
    }

    /// Run a rule's per-file pass under an explicit budget, for a rule that declared its own.
    ///
    /// The context is lent rather than handed over: what crosses is a fresh borrow of the
    /// resource the caller owns, so the handle the guest holds is dead the moment the call
    /// returns.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_check_with_timeout(
        &mut self,
        rule: &Rule,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome = rule.call_check(&mut self.store, borrow, captures);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run a rule's cross-file pass, under the default budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_reduce(
        &mut self,
        rule: &Rule,
        context: &Resource<ReduceContext>,
    ) -> Result<(), WasmError> {
        self.call_reduce_with_timeout(rule, context, self.limits.rule_timeout)
    }

    /// Run a rule's cross-file pass under an explicit budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_reduce_with_timeout(
        &mut self,
        rule: &Rule,
        context: &Resource<ReduceContext>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome = rule.call_reduce(&mut self.store, borrow);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Start the clock on one invocation.
    ///
    /// Three things, and each is re-done per call rather than left standing. The budget is
    /// armed, exactly as `lanekeep_js::Sandbox` arms it. The memory ceiling forgets any breach
    /// an earlier call recorded, so one is never reported twice. And the store's epoch
    /// deadline is set one tick ahead — a stale deadline would let a short handler slip past
    /// while stopping a long one at an arbitrary point, which is the same reason QuickJS's
    /// deadline is re-armed rather than left running.
    fn arm(&mut self, timeout: Duration) {
        self.budget.arm(timeout);
        self.store.data_mut().memory.arm();
        self.store.set_epoch_deadline(1);
    }

    /// Stop enforcing an invocation budget.
    fn disarm(&self) {
        self.budget.disarm();
    }

    /// Turn a raw wasmtime failure into something that says what actually happened.
    fn classify(&mut self, error: &wasmtime::Error, timeout: Duration) -> WasmError {
        let trip = self.budget.take_trip();
        let breach = self.store.data_mut().memory.take_breach();
        classify(error, trip, self.budget.clock(), timeout, breach)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_builds() {
        assert!(engine().is_ok());
    }

    #[test]
    fn the_ceiling_sums_across_memories_rather_than_applying_per_memory() {
        // The reason this type exists instead of `StoreLimitsBuilder::memory_size`. Two
        // memories, each half the ceiling: the second is admitted and a third byte is not.
        // Under a per-memory ceiling every one of these would be admitted.
        let mut ceiling = MemoryCeiling::new(1024);
        assert!(ceiling.admit(0, 512, "a").expect("fits"));
        assert!(ceiling.admit(0, 512, "b").expect("fits exactly"));
        assert!(
            ceiling.admit(0, 1, "c").is_err(),
            "a third memory must not get its own 1024 bytes"
        );
    }

    #[test]
    fn only_the_growth_counts_and_not_the_new_size() {
        // `memory_growing` reports the whole desired size, not the delta. Counting `desired`
        // would charge a memory its entire size on every grow, so a memory growing one page at
        // a time would hit a 1 MiB ceiling after 6 pages instead of 16.
        let mut ceiling = MemoryCeiling::new(1024);
        assert!(ceiling.admit(0, 512, "a").expect("fits"));
        assert!(ceiling.admit(512, 768, "a").expect("a further 256 fits"));
        assert_eq!(ceiling.total, 768);
    }

    #[test]
    fn a_breach_is_recorded_once_and_taken_once() {
        let mut ceiling = MemoryCeiling::new(16);
        assert!(ceiling.admit(0, 32, "a").is_err());
        assert_eq!(ceiling.take_breach(), Some(16));
        assert_eq!(
            ceiling.take_breach(),
            None,
            "a breach must not be reported twice"
        );
    }

    #[test]
    fn arming_forgets_a_breach_but_not_the_memory_that_is_still_held() {
        let mut ceiling = MemoryCeiling::new(1024);
        assert!(ceiling.admit(0, 1000, "a").expect("fits"));
        assert!(ceiling.admit(0, 1000, "b").is_err());

        ceiling.arm();
        assert_eq!(
            ceiling.take_breach(),
            None,
            "the next invocation starts clean"
        );
        assert_eq!(
            ceiling.total, 1000,
            "the memory the first grant handed out is still out there"
        );
    }

    #[test]
    fn a_table_is_charged_by_the_byte_and_not_by_the_element() {
        // Otherwise `table.grow` would be an allocation path the ceiling does not see.
        let width = size_of::<usize>();
        let mut ceiling = MemoryCeiling::new(width * 4);
        assert!(
            ResourceLimiter::table_growing(&mut ceiling, 0, 4, None).expect("fits"),
            "four elements is exactly the ceiling"
        );
        assert!(ResourceLimiter::table_growing(&mut ceiling, 4, 5, None).is_err());
    }

    #[test]
    fn dropping_an_engine_stops_its_ticker_promptly() {
        // A ticker that slept for its interval would make this take an hour. It is the same
        // property a `--watch` loop depends on, measured at a scale where a sleep would be
        // unmistakable.
        let started = std::time::Instant::now();
        let engine = WasmEngine::with_tick_interval(Duration::from_hours(1)).expect("builds");
        drop(engine);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "dropping the engine waited for the tick interval"
        );
    }
}
