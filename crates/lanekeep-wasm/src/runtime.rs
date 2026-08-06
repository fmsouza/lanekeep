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
//! `check_tunables` compares twenty-three fields, and the remaining twenty are covered by the
//! version and the triple only if the wasmtime **feature set** is pinned too: enabling
//! `signals-based-traps` moves `memory_may_move` and `signals_based_traps`, both of which that
//! routine checks, and workspace feature unification can move them without the version moving.
//! Whatever hashes these has to hash the resolved feature set as well, or it will hand out a
//! cache hit for an artifact the engine cannot load.
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

use crate::bindings::{Rule, types};
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
/// 1.95.0, `wasmtime` 47.0.3, against `tests/fixtures/limits.wasm`, **in the release profile**
/// and as minima of nine interleaved rounds. The profile is load-bearing for the last two
/// columns: an earlier version of this table was taken in the test profile and overstated
/// boundary crossings by about 24×, because that cost is host-side Rust rather than guest code.
///
/// - **strided** — one `strided 100` call: a hundred million iterations reading three fields of
///   a struct through one opaque index, which is the access pattern
///   [`MEMORY_GUARD_SIZE`] decides.
/// - **work** — one `work 300` call: three hundred million `black_box`ed iterations.
/// - **cross** — twenty thousand `tick` calls, each a component call plus two host calls.
/// - **inst** — fourteen threads each instantiating twenty-five times.
///
/// | reservation / guard | strided | work | 20k cross | inst ×350 |
/// |---|---|---|---|---|
/// | **4 GiB / 32 MiB (shipped)** | **87,732 µs** | **148,689 µs** | **6,965 µs** | **11,731 µs** |
/// | 4 GiB / 0 | 120,283 µs | 126,474 µs | 7,304 µs | 10,915 µs |
/// | 16 MiB / 32 MiB | 146,850 µs | 243,795 µs | 7,186 µs | 1,933 µs |
/// | 16 MiB / 0 | 151,414 µs | 243,651 µs | 7,229 µs | 1,650 µs |
///
/// **Below 4 GiB, execution costs about 1.65× and the exact size stops mattering.** Holding the
/// guard at its default, dropping the reservation costs 1.67× on `strided` and 1.64× on `work`.
/// It is not a gradient: a finer sweep over 64 MiB, 16 MiB, 2 MiB and 0 puts all four within 6%
/// of each other, so the trade is binary. What it buys back is about **9.8 ms of instantiation
/// per run at fourteen workers** — 11.7 ms against 1.9 ms for three hundred and fifty
/// instantiations — which is 1.2% of the 800 ms cold budget in `docs/architecture.md` §15. A
/// bounded 10 ms against an unbounded factor of 1.65 on the quantity that grows with corpus and
/// ruleset size.
///
/// The `cross` column is what bounds the trade rather than what settles it: a call whose cost is
/// crossing the boundary rather than computing behind it is **flat across every configuration**,
/// 6.9–7.3 ms for twenty thousand calls. So 1.65× is a ceiling a compute-bound guest reaches and
/// a host-call-bound one does not.
///
/// # What 4 GiB actually buys, which is not what it looks like
///
/// The obvious explanation is bounds-check elision, and it is **wrong** — worth stating, because
/// it is what an earlier version of this comment claimed. Elision needs
/// `u32::MAX <= reservation + guard - offset_and_size`
/// (`wasmtime-internal-cranelift-47.0.3/src/bounds_checks.rs:299-300`), and at 4 GiB the sum
/// exceeds `u32::MAX` by exactly one byte, so it is false for any access wider than a byte.
/// Nothing here is elided.
///
/// What the threshold actually crosses is `Memory::memory_may_move`
/// (`wasmtime-environ-47.0.3/src/types.rs:2331-2353`): a 32-bit memory with no declared maximum
/// has a `maximum_byte_size` of 4 GiB, so `max > reservation` is false at exactly 4 GiB and true
/// at every smaller value. When a memory may not move, its base pointer and its bound are
/// compile-time constants; when it may, both are loads from the vmcontext on every access. Same
/// threshold, same decision, different reason — and the difference matters, because it says the
/// win survives access patterns no elision would help, which is what the `strided` column shows.
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
/// # A zero guard was measured, shipped, and reversed
///
/// It is recorded rather than quietly corrected, because the way it was wrong is more useful
/// than the value. A zero guard is 16–19% *faster* on `work` — a tight loop over an
/// accumulator — and that result reproduces across profiles and across duplicated arms. It was
/// taken as evidence, and it was the only workload measured that had any opinion at all.
///
/// On `strided`, three loads at distinct static offsets off one dynamic index, it **loses by
/// 37–42%**. Release, minima of nine interleaved rounds, at a 4 GiB reservation, with each of
/// the two decisive arms configured twice and independently:
///
/// | guard | strided | work | artifact |
/// |---|---|---|---|
/// | **32 MiB (shipped)** | **87,732 / 87,535 µs** | 148,689 / 150,909 µs | **145,792 bytes** |
/// | 64 KiB | 88,366 µs | 149,269 µs | 145,792 bytes |
/// | 4 KiB | 87,731 µs | 150,751 µs | 145,792 bytes |
/// | 0 | 120,283 / 124,505 µs | **126,474 / 126,369 µs** | 162,184 bytes |
///
/// **The step is at any nonzero guard, not at a large one.** Four kilobytes behaves identically
/// to thirty-two megabytes, to within 1%, which rules out a mapping cost and points at a
/// codegen switch. That switch is in wasmtime's source and is documented there:
/// `wasmtime-internal-cranelift-47.0.3/src/bounds_checks.rs:423` takes a cheaper path when
/// `offset_and_size <= memory_guard_size`, emitting the partial check `index > bound` and
/// leaving the overshoot to the guard page — and the comment above it says why that is the
/// point, that "a series of Wasm loads that use the same dynamic index operand but different
/// static offset immediates" then "all emit the same `index > bound` check, which we can GVN".
/// A zero guard turns one check into three. The artifact is 11% larger for the same reason.
///
/// So the two results are not symmetric. The `strided` win has a mechanism in the source and an
/// access pattern that real code produces; the `work` win has neither, and is a
/// microarchitectural accident of one loop. Between a measured win with a mechanism on a
/// realistic pattern and a measured win without one on an artificial pattern, the first decides
/// — and on the half of this task with the highest undo cost, the default is where to be when
/// the evidence is mixed.
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
                    "could not start the epoch ticker thread: {e}\n                       both the per-invocation and the global wall-clock budgets are enforced by                      that thread advancing the engine's epoch, so a runtime without one would                      run every rule under no timeout at all"
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
/// share a [`WasmEngine`], so there is one compiled-code cache and one epoch ticker, and a
/// [`RunClock`], so the global budget is measured once for the run rather than once per
/// worker.
pub struct WasmRuntime {
    engine: Arc<WasmEngine>,
    linker: Linker<RuntimeState>,
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
        let mut linker = Linker::new(engine.engine());
        Rule::add_to_linker::<RuntimeState, HasSelf<HostState>>(&mut linker, |state| {
            &mut state.host
        })
        .map_err(|e| WasmError::Engine(e.to_string()))?;

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

        Ok(Self {
            engine,
            linker,
            store,
            limits,
            budget,
            interrupted,
        })
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
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached limit, or [`WasmError::Engine`] when the component
    /// does not satisfy the world.
    pub fn instantiate(&mut self, component: &Component) -> Result<Rule, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = Rule::instantiate(&mut self.store, component, &self.linker);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
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
