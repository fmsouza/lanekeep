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
//!   and switching engines does not touch it — see `tests/limits.rs`, which measures it.
//!   **This is still true of this module and is no longer true of a run**, because
//!   `lanekeep_engine::Engine::check_file` asks the same [`RunClock`] between one file and the
//!   next. That check is deliberately outside both engines: it is the one place a run can be
//!   stopped while no handler is executing, which is where a run spends most of its time.
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
//! # Instantiation happens once per (worker, component), and a worker is not a thread
//!
//! This module bounds instantiation to one per (worker, component): [`RuleSet`] resolves each
//! component's imports once for the whole run (`RulePre`, from `Linker::instantiate_pre`), and
//! each worker's [`WasmRuntime`] holds **one `Option` per component** —
//! [`WasmRuntime::instantiations`] counts what it built — filled the first time any rule that
//! component hosts is actually asked to run. It used to be a paragraph asking callers to
//! behave; it is now the shape of the API, and `tests/instantiation.rs` fails the three designs
//! it is written against.
//!
//! **Per component and not per rule, and the difference is the reason the world's exports take
//! a rule index.** A component is a compiled program and the rules on top of it are noise:
//! measured 2026-08-10, a JavaScript engine compiled to a component is 12.34 MiB and three
//! further rules with real bodies add 9,702 bytes. A per-rule instance cache would build that
//! engine once per rule in every worker's store, giving back at run time exactly what the
//! shared artifact saves on disk. The one exception is two slots naming the *same* rule of one
//! component, which hold different configurations of one guest slot and cannot share;
//! [`RuleSet::add`] splits those.
//!
//! **The ceiling argument that used to sit in that sentence was wrong twice over, and the
//! measured version is worth more than the claim it replaces.** It said four per-rule instances
//! breached the shipped 64 MiB per-worker ceiling. Four copies of 12.34 MiB is 49.4 MiB, which
//! is under it — and artifact bytes are not what `MemoryCeiling` sums, which is linear memory.
//! Measured 2026-08-11 through this type: `typescript-builtins.wasm` declares 145 pages, so an
//! instance grants 9,502,720 bytes before it runs, and one store takes **seven** and refuses the
//! eighth with [`WasmError::MemoryExceeded`] — bare, and equally after each has been configured
//! and had `metadata` read. So four is not a breach; what it costs is headroom, taking what is
//! left for a rule's own allocations from 54.9 MiB to 27.8 MiB, because linear memories never
//! shrink and every grant counts against the same ceiling. What does not survive a per-rule
//! bound is the next component: eight rules in one artifact would be refused outright, and the
//! shared TypeScript one already hosts four.
//!
//! **What that bound is worth depended on a number that was wrong, and it is corrected here.**
//! An earlier version of this paragraph said "roughly three hundred and fifty instantiations
//! for a run", from workers × rules at fourteen workers. A worker is a `rayon` `map_init`
//! initializer, and `map_init` runs **per chunk rather than per thread** — an `AGENTS.md` trap
//! this module already cites two sections down for a different reason. So the count is set by
//! rayon's adaptive splitting, not by the thread count, and it **does** grow with the corpus.
//! Measured through `lanekeep-engine` on 2026-08-06, release, 14 threads, one instance per
//! (worker, rule) throughout:
//!
//! | files | rules | stores | instantiations |
//! |---|---|---|---|
//! | 2,000 | 1 | 813 | 813 |
//! | 2,000 | 10 | 579 | 5,790 |
//! | 10,000 | 10 | 1,038 | 10,380 |
//!
//! Thirty times the old figure at the smallest of those and thirty again at the largest. The
//! counts are not even stable between runs of one corpus — rayon splits on how the work is
//! going — so this is a distribution rather than a bound.
//!
//! **The design is not falsified; the arithmetic behind [`MEMORY_RESERVATION`] is.** The
//! per-worker cache still works, and it is what keeps the count at workers × components instead
//! of at files × rules, which would be one hundred thousand for the last row. The table was
//! measured when a component hosted one rule, so its rows read the same either way; a ruleset
//! whose ten rules share one component now costs a tenth of them. What is gone is "the
//! penalty does not grow with the corpus", and with it the claim that instantiation is a
//! constant a large run can ignore. [`MEMORY_RESERVATION`] carries the re-derivation.
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
//! the initializer runs irrelevant. And **a `Store` never reclaims an instance**, so a per-file
//! design would not merely be slow — it would run out. Two ceilings say so, and the nearer one
//! is lanekeep's: under the shipped 64 MiB per-worker limit a store accepts about sixty
//! instances of `tests/fixtures/world-shape.wasm` before [`WasmError::MemoryExceeded`], because
//! linear memories never shrink and `MemoryCeiling` sums every grant. With that raised,
//! wasmtime's own cap stops it at 3,333, the 3,334th failing with `resource limit exceeded:
//! instance count too high at 10001` — three core instances per component instance against a
//! default of 10,000. `tests/instantiation.rs` asserts both: the sixty as an order of magnitude,
//! since it moves with the fixture's linear-memory minimum, and the 3,333 exactly, since it is
//! deterministic and a store that had started reclaiming instances is worth knowing about.
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
use crate::key::ExternalBinding;
use crate::load::{ComponentIdentity, Loaded};
use crate::sourcemap::SourceMap;

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
/// # What it costs — re-derived, because the first derivation rested on a number that was wrong
///
/// This section used to say the instantiation penalty is "about 10–13 ms per run at fourteen
/// workers, for three hundred and fifty instantiations", and that it "is bounded by workers ×
/// rules and **does not grow with the corpus**". The second half is false. A worker is a rayon
/// `map_init` initializer and `map_init` runs per *chunk*, so the store count is set by adaptive
/// splitting rather than by the thread count — 1,038 stores and 10,380 instantiations at ten
/// thousand files times ten rules, against the 140 that figure assumed. The module header has
/// the table.
///
/// **Both terms were therefore re-measured end to end through `lanekeep-engine`**, 2026-08-06,
/// Apple M3 Max, release, 14 threads, cache off, best of three.
///
/// **Term A — instantiation**, with a rule whose handler is a few host calls and essentially no
/// guest compute, so the whole difference is the reservation's cost:
///
/// | corpus | 4 GiB | 16 MiB | delta |
/// |---|---|---|---|
/// | 2,000 files × 1 rule | 45.1 ms | 28.9 ms | 16.2 ms |
/// | 2,000 files × 10 rules | 384.5 ms | 135.3 ms | **249.2 ms** |
/// | 10,000 files × 10 rules | 764.6 ms | 502.8 ms | **261.8 ms** |
///
/// Roughly 20–40 µs per instantiation, stated as a range because the two arms do not instantiate
/// the same number of times — rayon splits differently on each — so dividing one wall clock by
/// the other's count is approximate by construction.
///
/// **Term B — guest compute**, `tests/fixtures/limits.wasm`'s two memory-bound probes driven
/// through the engine on one file, where instantiation is one call and negligible:
///
/// | probe | 4 GiB | 16 MiB | ratio |
/// |---|---|---|---|
/// | `work 300` | 490.2 ms | 737.0 ms | **1.50×** |
/// | `strided 100` | 266.2 ms | 430.3 ms | **1.62×** |
///
/// So term B reproduces: the 1.5–1.6× on memory-bound guest code is real and survives being
/// measured through the whole engine rather than against a bare store.
///
/// # The trade is a crossover, not a bound, and it depends on the ruleset
///
/// 4 GiB is worth having exactly when the guest compute it speeds up outweighs the instantiation
/// it slows down:
///
/// ```text
/// 0.6 × (guest compute at 4 GiB)  >  (instantiations) × (20–40 µs)
/// ```
///
/// At 2,000 files × 10 rules that is 249 ms of instantiation to recover, so the run needs about
/// **415 ms of memory-bound guest compute across 20,000 invocations — roughly 21 µs each**. For
/// scale, `strided`'s hundred million strided loads cost about 89 ms, so 21 µs is on the order of
/// twenty-four thousand memory-bound operations per match. A rule that walks a subtree and
/// compares a few strings is well under it; a rule doing real in-guest analysis is over it.
///
/// **So the honest statement is that the answer depends on corpus shape and rule weight, and
/// this constant cannot be justified for all of them.** It is left at 4 GiB rather than reversed,
/// for three reasons and none of them is that the old argument still holds. Changing it
/// invalidates every precompiled artifact and every cached result in every checkout, so it is
/// not a knob to flip on one machine's numbers. The guest-compute term still has only a
/// synthetic measurement behind it: two real components ship now, `lanekeep/no-unwrap` and
/// `lanekeep/no-glob-import`, and both are light — they walk a short ancestor chain and compare
/// strings, which §15.1 measured at the low end of the range this constant was reasoned over, so
/// they do not exercise the case that would settle it. And the lever that would remove the
/// question is not this constant: **`with_min_len` on `lanekeep-engine`'s `par_iter` would bound
/// the store count directly**, and it is a bigger change than it looks, because the same
/// initializer builds the QuickJS sandbox and the change would move the JavaScript path's
/// measured behavior too.
///
/// The trigger this used to carry — "revisit when a real component ruleset exists" — has half
/// fired, and the half that fired is not the informative one. A component ruleset exists; a
/// component ruleset doing heavy in-guest analysis does not, and that is the shape the constant
/// is unjustified for. So the revisit condition is restated rather than discharged: **revisit when
/// a component does real per-match computation**, and re-measure both terms rather than either
/// half. A rule that walks a subtree and compares a few strings will keep answering "fine"
/// however many of them ship.
///
/// **The argument depends on an instance per (worker, component) rather than per file, and on
/// every path a run takes that is now enforced rather than assumed.** [`RuleSet`] resolves each
/// component once for the run and each worker's [`WasmRuntime`] holds one `Option` per
/// component, filled by [`WasmRuntime::rule`] and by nothing else — so a cache sized to the
/// components is the ceiling, and there is nowhere to put a second instance of one.
/// `tests/instantiation.rs` asserts it, and the two eager designs it is written against fail it.
///
/// **One unbounded path remains, and it is named here rather than left to be discovered.**
/// [`WasmRuntime::instantiate`] is public, keeps no instance and adds one to the store on every
/// call, so the cache is not what bounds *it*. It has no production caller — only
/// `tests/limits.rs` and the two cap cases in `tests/instantiation.rs`, which exist precisely to
/// instantiate repeatedly — and its own documentation says that calling it per file is the
/// shape this constant rules out. If it ever acquires a production caller, this number is one
/// of the things that has to be re-measured.
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
///
/// `pub(crate)` for [`crate::key`], which reads the three artifact-validity constants back off
/// an [`Engine`] built from it rather than restating them — a second copy of a cache-key input
/// is a drift source, and these three are already documented as things nobody flips.
pub(crate) fn config() -> Config {
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
/// One per [`WasmEngine`], and a run builds **two** when its config names a component:
/// `lanekeep_config::describe_components` builds an engine to ask each component what it is
/// and drops it before returning, and `lanekeep_engine::load_components` builds the one the
/// run executes on. They do not overlap in what they are for and they barely overlap in time,
/// but "one per run" was the claim this made and it is no longer the shape.
///
/// `AGENTS.md` names what must not happen: a `--watch` loop or a long-lived server that leaked
/// one of these per iteration would accumulate threads for the life of the process, each
/// advancing a counter nothing reads. The [`Drop`] impl is what prevents it, and it is what
/// makes the config-time engine cost nothing beyond the moment it is used — which is worth
/// knowing now that a run builds one more than it used to.
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

/// One component instance's worth of rules, resolved against the host world once for the run.
///
/// **The unit a store instantiates, which is not the same as a rule.** A component hosts a list
/// of rules and every export but `rules` takes an index into it, so one instance serves every
/// rule its component hosts — which is the whole reason the index parameter exists. Measured
/// 2026-08-10, a JavaScript engine compiled to a component is 12.34 MiB and three further rules
/// with real bodies add 9,702 bytes; instantiating per rule would hand every worker one copy of
/// that engine per rule and give back at run time exactly what the shared artifact saved on
/// disk.
///
/// # Why this is not simply "one per component"
///
/// A guest holds one configuration per rule index: `configure(rule, options)` writes it and
/// every later `check` reads it. So an instance can serve at most one slot per index, and two
/// slots naming the *same* rule of one component need an instance each — otherwise the second
/// `configure` overwrites the first, and which one won would depend on which slot that worker
/// reached first, which is a determinism failure rather than an inefficiency.
///
/// That case is a real config rather than a hypothetical: `lanekeep-config`'s `hash_ruleset`
/// records that `["./r.wasm", {"rule": "./r.wasm", "options": {…}}]` "is how a rule is used bare
/// and configured in the same run", and nothing deduplicates it.
///
/// So [`RuleSet::add`] shares an instance across rules of one component and splits on a
/// collision, and [`PreparedInstance::taken`] is what it splits on.
struct PreparedInstance {
    /// Which component this is, so a second load of the same bytes is recognized as one.
    identity: ComponentIdentity,
    pre: RulePre<RuntimeState>,
    /// The component's source map, or `None` for one that shipped without one.
    ///
    /// Held per instance rather than per rule because a map describes the *component*: the four
    /// TypeScript built-ins are one bundle and one map, and a copy per rule would be four
    /// references to the same table for no reason. It is read once per thrown error, which is
    /// once per run — every variant of `WasmError` cancels it.
    source_map: Option<Arc<SourceMap>>,
    /// The rule indices already served by this instance.
    ///
    /// A `Vec` and a linear scan, because it holds one entry per rule of one component — a
    /// handful, searched once per `add` and never on the hot path.
    taken: Vec<u32>,
}

/// One rule, and which instance serves it.
struct Prepared {
    /// Whatever identifies the rule to a reader — a rule id. Diagnostics only.
    id: String,
    /// Which entry in [`RuleSet::instances`] this rule's component instance is built from.
    instance: usize,
    /// Which of the component's rules this slot names.
    ///
    /// A component hosts a *list* of rules and every export but `rules` takes an index into
    /// it, so this is what every call this set's slots reach has to carry. Recorded once at
    /// [`RuleSet::add`] rather than passed per call, for the reason [`Prepared::options`] is:
    /// two callers disagreeing about which rule a slot means is a rule answering to another
    /// rule's configuration, silently.
    index: u32,
    /// The options this rule's `configure` is called with, as JSON.
    ///
    /// Not diagnostics: [`WasmRuntime::rule`] reads it on every worker, because the world's
    /// `configure` is declared "once, after instantiation and before any `check`" and an
    /// instance is built per worker. Held here rather than per worker because the answer is
    /// the run's, not the store's — two workers configuring one rule differently is the
    /// determinism failure this arrangement exists to make impossible.
    options: String,
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
    /// What a store instantiates, one entry per (component, colliding-index) group.
    instances: Vec<PreparedInstance>,
    rules: Vec<Prepared>,
    /// What was bound beside the declared world, as declared at the call site.
    ///
    /// Evidence rather than enforcement, in the same sense [`WasmRuntime::instantiations`] is:
    /// nothing in linking or execution reads it, and it exists so that "this run bound nothing
    /// the cache key did not know about" is a claim something can be made about.
    external: Vec<ExternalBinding>,
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
            instances: Vec::new(),
            rules: Vec::new(),
            external: Vec::new(),
        })
    }

    /// Resolve a loaded component against the host world and give it a slot.
    ///
    /// This is where a component's imports are matched against what the host provides and
    /// its exports are checked against the world — once, for the run. Everything a worker
    /// does afterwards is instantiation.
    ///
    /// # It takes a [`Loaded`] rather than a `Component`, and that is the point
    ///
    /// [`Loaded`] can only be produced by [`crate::load::ComponentLoader::load`], which checks
    /// the component's import list against a permitted set before it hands one back. So there
    /// is no path from bytes to a running instance that skips that check — not because
    /// everyone remembers to call it, but because the unchecked path does not compile.
    ///
    /// It used to take a bare `&Component`, and that was a hole rather than a convenience:
    /// `WasmEngine::compile` followed by `add` reached an instance without ever consulting the
    /// import list, three tests in this crate took exactly that route, and every load-path test
    /// passed regardless. Decision-record condition 4 exists because a wrongly-targeted
    /// component failing to instantiate is *incidental* protection — it holds only while this
    /// host declines to link WASI — so a check that can be bypassed is not the condition.
    ///
    /// # `options_json` is a parameter and not a later call, for the reason `loaded` is a
    /// [`Loaded`]
    ///
    /// The world declares `configure` as happening "once, after instantiation and before any
    /// `check`", and an instance is built lazily *per worker* — so a configuration step a
    /// caller performs is a step that runs on whichever worker the caller happened to hold and
    /// on none of the others. A rule configured on some workers and not others answers
    /// differently depending on which files rayon gave which worker, which is the determinism
    /// invariant rather than a nicety.
    ///
    /// Taking the options here makes it unavoidable instead: [`WasmRuntime::rule`] configures
    /// every instance it builds, and there is no way to reach a slot's instance that skips it.
    /// `"null"` is the shape for a rule named with no options — the world says so, so a guest
    /// has one code path rather than two — and it is spelled out by the caller rather than
    /// defaulted here, because "this rule has no options" and "nobody said" are different
    /// claims and only the caller can tell them apart.
    ///
    /// # `index` names one of the component's rules, and the caller has to know it
    ///
    /// A component hosts a list of rules; `index` is the position in that list, which is what
    /// the world's `rules` export enumerates and what every other export takes. A component
    /// hosting one rule is added at `0`, and pays a `u32` for the arrangement that lets one
    /// 12.34 MiB JavaScript engine serve every rule built on it rather than one copy each.
    ///
    /// It is a parameter rather than something this method discovers, because discovering it
    /// means *running* the component: `rules` is an export, so enumerating requires a store and
    /// an instance, and this type deliberately holds neither — it resolves imports once for the
    /// whole run and leaves instantiation to whichever worker needs it.
    /// [`WasmRuntime::call_rules`] is the enumeration, and a caller asks it once.
    ///
    /// # Rules of one component share an instance, and this is where that is decided
    ///
    /// Two slots naming the same component at *different* indices are the ordinary case for a
    /// multi-rule component, and they resolve against it once and share one instance per store.
    /// That is the whole reason the world's exports take an index: a component is a compiled
    /// program, the rules on top of it are noise, and instantiating per rule would give back at
    /// run time exactly what a shared artifact saves on disk.
    ///
    /// Two slots naming the same component at the *same* index are split onto separate
    /// instances, because a guest holds one configuration per rule index. See
    /// `PreparedInstance` for why that case is a real config rather than a hypothetical.
    ///
    /// **Sameness is the bytes a component was compiled from, not the [`Loaded`] value.**
    /// `lanekeep-config` loads once per rule *reference*, so several rules of one shared
    /// component arrive as several `Loaded`s over identical bytes, and object identity would
    /// call them different components.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] when the component does not satisfy the world: an import
    /// the host does not provide, or a missing or wrongly typed export.
    pub fn add(
        &mut self,
        id: impl Into<String>,
        loaded: &Loaded,
        index: u32,
        options_json: impl Into<String>,
    ) -> Result<RuleSlot, WasmError> {
        let instance = if let Some(existing) = self.sharing(loaded.identity(), index) {
            existing
        } else {
            // Resolved before anything is pushed, so a component that does not satisfy the
            // world leaves this set exactly as it found it.
            let pre = self
                .linker
                .instantiate_pre(loaded.component())
                .and_then(RulePre::new)
                .map_err(|e| WasmError::Engine(e.to_string()))?;
            self.instances.push(PreparedInstance {
                identity: *loaded.identity(),
                pre,
                source_map: loaded.source_map().map(Arc::clone),
                taken: Vec::new(),
            });
            self.instances.len().saturating_sub(1)
        };

        if let Some(entry) = self.instances.get_mut(instance) {
            entry.taken.push(index);
        }

        let slot = RuleSlot(self.rules.len());
        self.rules.push(Prepared {
            id: id.into(),
            instance,
            index,
            options: options_json.into(),
        });
        Ok(slot)
    }

    /// The instance a rule of this component at this index may share, if there is one.
    ///
    /// The first entry for the component that has not already taken the index. "First" rather
    /// than "any" only matters when a component is named three or more times for one rule, and
    /// then only for which instance each lands on — every arrangement holds one configuration
    /// per instance either way.
    fn sharing(&self, identity: &ComponentIdentity, index: u32) -> Option<usize> {
        self.instances
            .iter()
            .position(|entry| entry.identity == *identity && !entry.taken.contains(&index))
    }

    /// How many component instances a store built from this set will hold.
    ///
    /// One per entry in [`RuleSet::instances`], which is at most one per rule and is fewer
    /// whenever a component hosts more than one of them. [`WasmRuntime`] sizes its instance
    /// cache from this rather than from [`RuleSet::len`].
    pub(crate) fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// The linker, for a host that must bind more than the declared world.
    ///
    /// Nothing in this crate needs it: a Rust rule built for `wasm32-unknown-unknown` imports
    /// exactly `lanekeep:host/types@0.1.0` and [`RuleSet::new`] has already bound that. It
    /// exists for the Go authoring lane, whose components import `wasi:io/error`,
    /// `wasi:io/streams`, `wasi:cli/stdout` and `wasi:random/random` unconditionally because
    /// TinyGo's runtime does — and those have to be **bound**, not declined, since a declined
    /// import means the component never instantiates at all.
    ///
    /// **Binding is one half and permitting is the other, and they are deliberately separate.**
    /// An interface bound here but not named in
    /// [`crate::load::PermittedImports`] is still refused at load, and one permitted but not
    /// bound still fails to instantiate. Both orders fail closed, which is why widening either
    /// alone is safe and only widening both grants anything.
    ///
    /// Whatever is bound here must be a *fixed* source rather than an ambient one — a random
    /// stream seeded from the run rather than the host's, sinks rather than the process's real
    /// stdout — or the determinism invariant is gone whatever the import list says.
    ///
    /// # It takes a declaration, and that is a cache-key requirement rather than paperwork
    ///
    /// Fixed is not enough on its own. A *different* fixed entropy source pins a *different*
    /// map iteration order for a Go rule, so it reports a different violation while the world,
    /// the component and the config are all unchanged — every cache-key input identical, a
    /// warm run serving the old answer with no symptom. [`crate::key::host_api_hash`] folds
    /// [`crate::key::EXTERNAL_BINDINGS`] in for exactly that reason, and this parameter is
    /// what points a binder at it: reaching the linker means naming the interface and the
    /// value that fixes its answers.
    ///
    /// **The declaration made here is recorded, not checked against
    /// [`crate::key::EXTERNAL_BINDINGS`], and the residual gap is stated rather than implied.**
    /// The cache key is computed when a configuration is loaded, before any [`RuleSet`]
    /// exists, so this cannot consult what the key was built from. What it can do is make the
    /// declaration exist and be readable — [`RuleSet::external_bindings`] — so a run that
    /// binds something is in a position to compare, and so nothing gets bound by a caller who
    /// never had to think about the value.
    pub fn linker_mut(&mut self, declared: &ExternalBinding) -> &mut Linker<RuntimeState> {
        self.external.push(*declared);
        &mut self.linker
    }

    /// Every interface bound through [`RuleSet::linker_mut`], in the order it was bound.
    ///
    /// Empty for a set that only ever linked the declared world, which is every set this
    /// crate builds today.
    #[must_use]
    pub fn external_bindings(&self) -> &[ExternalBinding] {
        &self.external
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

    /// Which of its component's rules a slot names, or `None` for a slot this set never
    /// issued.
    ///
    /// The `rule` parameter every export but `rules` takes. Distinct from
    /// [`RuleSlot::index`], which is a position in *this set* — the two agree only for a run
    /// whose every component hosts exactly one rule, which is why neither is derived from the
    /// other.
    #[must_use]
    pub fn rule_index(&self, slot: RuleSlot) -> Option<u32> {
        self.rules.get(slot.0).map(|rule| rule.index)
    }

    /// The rule a slot names.
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

    /// The source map of the component a slot's rule lives in, if it ships with one.
    ///
    /// `None` for a slot this set never issued, on the same terms as [`RuleSet::rule_index`]:
    /// a missing map and an unknown slot are both "nothing to remap with", and a caller reaching
    /// here is rendering a failure rather than deciding whether one happened.
    fn source_map(&self, slot: RuleSlot) -> Option<&Arc<SourceMap>> {
        let at = self.rules.get(slot.0)?.instance;
        self.instances.get(at)?.source_map.as_ref()
    }

    /// The pre-instantiation one of this set's instances is built from.
    fn pre(&self, instance: usize) -> Result<&RulePre<RuntimeState>, WasmError> {
        self.instances
            .get(instance)
            .map(|entry| &entry.pre)
            .ok_or_else(|| {
                WasmError::Engine(format!(
                    "component instance {instance} is not in this rule set, which holds {}",
                    self.instances.len()
                ))
            })
    }
}

/// The memory ceiling, summed across every linear memory in one store.
///
/// **Hand-written rather than `StoreLimitsBuilder::memory_size`, and the difference is the
/// point.** wasmtime's own helper applies its ceiling to each linear memory independently: it
/// compares `desired` against the limit and knows nothing about the other memories in the
/// store. A store holds one instance per component, so under that helper a worker running
/// twenty components could reach twenty times the budget it was given, and nothing would say so.
/// Sharing an instance across a component's rules lowers the count and does not change the
/// argument: what a per-memory ceiling multiplies by is the number of instances, whatever
/// decides it.
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
///
/// **Public with private fields, and nothing here is a knob.** It is public only because
/// [`RuleSet::linker_mut`] cannot name `Linker<RuntimeState>` otherwise, and an embedder
/// binding an extra interface has to name the store's data type to write the closure. Nothing
/// outside this module may read or write either field: the host state belongs to the world and
/// the ceiling belongs to the runtime.
pub struct RuntimeState {
    host: HostState,
    memory: MemoryCeiling,
}

impl std::fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeState").finish_non_exhaustive()
    }
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
    /// One `Option` per *component instance* the set will build, filled on first use.
    ///
    /// **Per rule was wrong and this is the correction.** A single flag covering the ruleset
    /// recovers only the all-cache-hits case and pays for every rule the moment one file
    /// misses, which is why this is not that; but a slot-indexed cache is the opposite mistake,
    /// because several rules of one component share one instance and a per-slot cache builds
    /// the component once per rule. For the 12.34 MiB JavaScript engine that is one copy of the
    /// engine per rule in every worker's store, which the shipped 64 MiB per-worker ceiling
    /// would refuse. Indexed by `Prepared::instance`, sized from
    /// [`RuleSet::instance_count`].
    ///
    /// Nothing ever clears an entry. A `Store` does not reclaim an instance — instantiating
    /// repeatedly into one stops at 3,333 — so an entry that could be dropped and rebuilt
    /// would consume a slot without releasing one.
    instances: Vec<Option<Rule>>,
    /// Whether each *rule* has had `configure` called in this store yet.
    ///
    /// Per slot where [`Self::instances`] is per component, because the two are lazy about
    /// different things. An instance is shared; a configuration is not — the guest holds one
    /// per rule index — so a component whose rule 0 has run must still configure rule 1 before
    /// rule 1 checks anything. Keeping the flag here rather than inferring it from the instance
    /// is what makes "no `check` reaches an unconfigured rule" true per rule rather than per
    /// component.
    configured: Vec<bool>,
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
    /// What it allocates is one `None` per component instance the set needs, and one
    /// `configure`-yet flag per rule.
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
        let instances = (0..rules.instance_count()).map(|_| None).collect();
        let configured = vec![false; rules.len()];

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
            configured,
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
    /// shortest call there is, and it is bounded outside this crate — `lanekeep_engine` asks the
    /// clock at each file boundary, so a run already spent when a file begins never reaches an
    /// instantiation at all.
    ///
    /// **That bound is at file granularity, and the race survives inside one file.** A budget
    /// that expires after the boundary check and before an instantiation later in the same file
    /// is back to depending on whether a tick lands in those tens of microseconds. The
    /// conclusion is unchanged and the reason is the conclusion: a second clock read here would
    /// close this one call site and leave every other guest entry point exactly as it was, which
    /// is how a limit ends up enforced in some places and not others. One check where a file's
    /// work begins is the whole of it.
    ///
    /// # How often this may be called is a load-bearing assumption, and this is the raw form
    ///
    /// [`MEMORY_RESERVATION`] is chosen on the basis that instantiation happens **per (worker,
    /// component)**. What makes that true is [`RuleSet`] plus the per-component `Option` behind
    /// [`WasmRuntime::rule`], which instantiates at most once per component per store. This method
    /// is underneath that: it takes a component rather than a slot, so it resolves the
    /// component's imports on every call and keeps no instance, and calling it per file is
    /// exactly the shape the bound rules out.
    ///
    /// It stays public for two callers, both of them tests and neither of them a run.
    /// `tests/limits.rs` drives one component under several budgets and wants the primitive
    /// rather than a ruleset; and the two cap cases in `tests/instantiation.rs` instantiate
    /// into one store until it refuses, which is the one thing a bounded API cannot express.
    ///
    /// **It does not configure what it builds, and cannot.** A slot carries the options
    /// [`RuleSet::add`] recorded; a bare component carries none, so there is nothing to hand
    /// over. That is a second reason this is not the door a run comes through: an instance
    /// from here has never seen `configure`, which the world declares must happen before any
    /// `check`. [`WasmRuntime::rule`] is where both obligations are met at once.
    ///
    /// # It takes a [`Loaded`] for the same reason [`RuleSet::add`] does
    ///
    /// It took a bare `&Component` until a review found what that meant: a published method
    /// that reaches a *running instance* without the import check decision-record condition 4
    /// rests on, in a crate `release-plz.toml` ships. `Loaded` is only produced by
    /// [`crate::load::ComponentLoader::load`], which runs [`crate::load::check_imports`] first,
    /// so the bypass now fails to compile rather than being ruled out by everyone remembering
    /// that this is not the door a run comes through. That is the same repair `add` had, and
    /// its rationale is the one worth reading: a check being *reachable* was never the
    /// requirement, the check being *unavoidable* is.
    ///
    /// What this does **not** bound is how often it may be called; the paragraph above still
    /// governs that, and [`MEMORY_RESERVATION`] names this method as the one unbounded path.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached limit, or [`WasmError::Engine`] when the component
    /// does not satisfy the world.
    pub fn instantiate(&mut self, loaded: &Loaded) -> Result<Rule, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = Rule::instantiate(&mut self.store, loaded.component(), &self.rules.linker);
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
    /// Evidence, exactly as [`WasmRuntime::epoch_fired`] is. Three claims depend on it and
    /// none can be made about anything else: that a worker whose files all hit the cache
    /// instantiates *nothing*, that a worker handling three files that match the same rule
    /// instantiates *once*, and that a component hosting several rules is instantiated once
    /// however many of them run. A counter kept by a test harness would prove a property of the
    /// harness.
    #[must_use]
    pub const fn instantiations(&self) -> usize {
        self.instantiations
    }

    /// Whether the instance a slot's rule runs in has been built in this store yet.
    ///
    /// **The component's, not the rule's.** Several rules of one component share one instance,
    /// so this answers `true` for a rule that has never run once a sibling rule of the same
    /// component has. [`WasmRuntime::is_configured`] is the per-rule question.
    #[must_use]
    pub fn is_instantiated(&self, slot: RuleSlot) -> bool {
        self.rules.rule(slot).is_ok_and(|rule| {
            self.instances
                .get(rule.instance)
                .is_some_and(Option::is_some)
        })
    }

    /// Whether a slot's rule has been handed its options in this store yet.
    ///
    /// Evidence, on the same terms as [`WasmRuntime::instantiations`]. A shared instance makes
    /// "was this rule reached" a different question from "was its component built", and this is
    /// the half that stayed per rule.
    #[must_use]
    pub fn is_configured(&self, slot: RuleSlot) -> bool {
        self.configured.get(slot.index()).copied().unwrap_or(false)
    }

    /// This store's instance for a rule, building it and configuring the rule on first use.
    ///
    /// **Two lazinesses, and they are lazy about different things.** The *instance* is built the
    /// first time any rule of its component is reached, and shared by the rest — a component is
    /// a compiled program and its rules are what run inside it. The *configuration* is applied
    /// the first time each rule is reached, because a guest holds one per rule index. So a
    /// worker that runs rule 3 of a four-rule component builds one instance and configures one
    /// rule, and a worker whose queries matched nothing does neither. That second case is what
    /// eager instantiation pays 82 to 96× for.
    ///
    /// A failure is not remembered. `Worker::sandbox` in `lanekeep-engine` caches its build
    /// failure so it is reported once per worker rather than retried per file, and the
    /// reasoning does not carry: every failure this can return cancels the run, so there is no
    /// next file to retry for.
    ///
    /// # This is where `configure` happens, and it is the only place it can happen
    ///
    /// The world declares `configure` as running "once, after instantiation and before any
    /// `check`". Instantiation is lazy and per worker, so nothing a *caller* does can satisfy
    /// that: a rule first instantiated inside `check` on worker N would never have passed
    /// through a configuration step performed anywhere else, and a rule running configured on
    /// some workers and unconfigured on others answers differently depending on how rayon
    /// split the corpus. That is a determinism failure, not a missing feature.
    ///
    /// So the call sits between the instantiation and the instance being handed out, reading
    /// the options [`RuleSet::add`] recorded. Both halves of "once" follow from where it is:
    /// once per (worker, rule) because the `configured` flag is set beside it, and before any
    /// `check` because every entry point goes through here first.
    ///
    /// **An unconfigured rule is unreachable, which is what a shared instance leaves of "a
    /// refusal leaves no instance behind".** That sentence used to be true literally: the
    /// instance was stored only after its one rule accepted its options. It cannot stay literal
    /// once an instance serves several rules — rule 0 configuring successfully is exactly why
    /// the instance is kept when rule 1 then refuses — and the property that mattered survives
    /// in a stronger form. Nothing reaches a `check` except through here, and this returns
    /// before handing anything back unless *this rule* has been configured. Per rule, where it
    /// used to be per instance.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached limit — instantiation runs the component's own
    /// initialization and allocates its linear memory's minimum — [`WasmError::Misconfigured`]
    /// when the guest declines the options it was handed, or [`WasmError::Engine`] for a slot
    /// this runtime's rule set never issued.
    pub fn rule(&mut self, slot: RuleSlot) -> Result<&Rule, WasmError> {
        let rules = Arc::clone(&self.rules);
        let prepared = rules.rule(slot)?;
        let at = prepared.instance;
        let timeout = self.limits.rule_timeout;

        if !self.instances.get(at).is_some_and(Option::is_some) {
            self.arm(timeout);
            let outcome = rules.pre(at)?.instantiate(&mut self.store);
            self.disarm();
            let instance = outcome.map_err(|error| self.classify(&error, timeout))?;

            self.instantiations += 1;

            // Stored before `configure` runs, and that is the shared-instance shape rather than
            // a relaxation: the same instance is about to serve every other rule its component
            // hosts, so it does not belong to whichever rule happened to reach it first. What
            // stops an unconfigured rule being *used* is the flag below, not the absence of an
            // instance.
            //
            // `rules.rule(slot)` above already refused an out-of-range slot and `instances` is
            // sized from the same set, so this index is in range. `get_mut` rather than `[]`
            // regardless: the workspace denies panicking outside tests, and an engine that
            // panics on a malformed input has failed at its job.
            if let Some(entry) = self.instances.get_mut(at) {
                *entry = Some(instance);
            }
        }

        if !self.is_configured(slot) {
            self.arm(timeout);
            let configured = self.with_instance(slot, |rule, store| {
                rule.call_configure(store, prepared.index, &prepared.options)
            });
            self.disarm();
            match configured {
                Ok(Ok(())) => {}
                Ok(Err(message)) => return Err(WasmError::Misconfigured { message }),
                Err(error) => return Err(self.classify(&error, timeout)),
            }
            if let Some(entry) = self.configured.get_mut(slot.index()) {
                *entry = true;
            }
        }

        self.instances
            .get(at)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                WasmError::Engine(format!(
                    "component instance {at} was built and then could not be found"
                ))
            })
    }

    /// What this rule says it is.
    ///
    /// Instantiates and configures the rule if it is not already, on the same terms as
    /// [`Self::rule`] — prepare time is the one point where every rule is instantiated
    /// regardless of whether its query will match, because a rule that cannot describe itself
    /// cannot be run at all.
    ///
    /// **A rule that refuses its options never gets to describe itself**, because
    /// configuration happens on the way to the instance this reads from. That ordering is the
    /// world's rather than this method's, and it is the useful one: a component handed
    /// configuration it cannot use has been misconfigured, and reporting that is more use than
    /// reporting the metadata of a rule the run is about to refuse anyway.
    ///
    /// # Errors
    ///
    /// [`WasmError`] if the slot is unknown, instantiation fails, the guest declines its
    /// options, or the guest traps.
    pub fn metadata(&mut self, slot: RuleSlot) -> Result<types::RuleMetadata, WasmError> {
        self.rule(slot)?;
        let index = self.rule_index(slot)?;
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_metadata(store, index));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Which of its component's rules a slot names.
    ///
    /// A method rather than an inline `self.rules.rule(slot)?.index` at five call sites: the
    /// index is `Copy`, so reading it here ends the borrow of the rule set before the caller
    /// needs `&mut self` again to call into the store.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] for a slot this runtime's rule set never issued.
    fn rule_index(&self, slot: RuleSlot) -> Result<u32, WasmError> {
        Ok(self.rules.rule(slot)?.index)
    }

    /// Ask a slot's rule whether it has a per-file pass, instantiating it if needed.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::rule`] and [`WasmRuntime::call_check`].
    pub fn has_check(&mut self, slot: RuleSlot) -> Result<bool, WasmError> {
        self.rule(slot)?;
        let index = self.rule_index(slot)?;
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_has_check(store, index));
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
        let index = self.rule_index(slot)?;
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| rule.call_has_reduce(store, index));
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run a slot's per-file pass for one query match, under the default budget.
    ///
    /// **This is the call the bound is about.** It is reached once per (rule, match) and
    /// instantiates at most once per (worker, component), because the instance it needs is the one
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
        let index = self.rule_index(slot)?;
        let borrow = Resource::new_borrow(context.rep());
        // Taken before the call rather than after it, and cloned rather than borrowed, because
        // `settle` needs `&mut self` and the map lives in the set behind `self.rules`. An `Arc`
        // clone is a refcount bump on a table that is shared by every worker anyway.
        let source_map = self.rules.source_map(slot).map(Arc::clone);
        self.arm(timeout);
        let outcome = self.with_instance(slot, |rule, store| {
            rule.call_check(store, index, borrow, captures)
        });
        self.disarm();
        self.settle(outcome, timeout, source_map.as_deref())
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
        let index = self.rule_index(slot)?;
        let borrow = Resource::new_borrow(context.rep());
        let source_map = self.rules.source_map(slot).map(Arc::clone);
        self.arm(timeout);
        let outcome =
            self.with_instance(slot, |rule, store| rule.call_reduce(store, index, borrow));
        self.disarm();
        self.settle(outcome, timeout, source_map.as_deref())
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
        let at = self
            .rules
            .rule(slot)
            .map(|rule| rule.instance)
            .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
        let Some(rule) = self.instances.get(at).and_then(Option::as_ref) else {
            return Err(wasmtime::Error::msg(format!(
                "rule slot {} has no instance in this store",
                slot.index()
            )));
        };
        call(rule, &mut self.store)
    }

    /// Every rule an instance hosts, by id, in index order.
    ///
    /// **The enumeration [`RuleSet::add`] cannot perform for itself.** `rules` is an export, so
    /// asking it needs a store and an instance, and a rule set holds neither — which is why the
    /// index is a parameter there and an answer here.
    ///
    /// `lanekeep_config::describe_components` is the caller: once per component at config load,
    /// through a throwaway runtime built and dropped for the question, then one
    /// [`RuleSet::add`] per id it answered with. That costs an instantiation the description
    /// then repeats, which is the price of an index that cannot be discovered without a store —
    /// and it is paid once for the run rather than once per worker.
    ///
    /// It takes no index, and it is the only export that does not: the list is the component's
    /// rather than any one rule's. It is also the one export that is meaningful *before*
    /// `configure` — a rule's id cannot depend on its options, because the id is how a config
    /// names the rule in the first place — which is why the world splits it from `metadata`
    /// rather than returning a list of those.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_rules(&mut self, rule: &Rule) -> Result<Vec<String>, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = rule.call_rules(&mut self.store);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Ask one of an instance's rules whether it has a per-file pass.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_has_check(&mut self, rule: &Rule, index: u32) -> Result<bool, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = rule.call_has_check(&mut self.store, index);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Ask one of an instance's rules whether it has a cross-file pass.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_has_reduce(&mut self, rule: &Rule, index: u32) -> Result<bool, WasmError> {
        let timeout = self.limits.rule_timeout;
        self.arm(timeout);
        let outcome = rule.call_has_reduce(&mut self.store, index);
        self.disarm();
        outcome.map_err(|error| self.classify(&error, timeout))
    }

    /// Run one of an instance's rules' per-file pass for one query match, under the default
    /// budget.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError`] on a breached budget, a trapping guest, or a guest that reported
    /// its own failure through the world's `rule-error`. Every variant cancels the run.
    pub fn call_check(
        &mut self,
        rule: &Rule,
        index: u32,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
    ) -> Result<(), WasmError> {
        self.call_check_with_timeout(rule, index, context, captures, self.limits.rule_timeout)
    }

    /// Run one of an instance's rules' per-file pass under an explicit budget, for a rule that
    /// declared its own.
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
        index: u32,
        context: &Resource<CheckContext>,
        captures: &Vec<types::MatchEntry>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome = rule.call_check(&mut self.store, index, borrow, captures);
        self.disarm();
        // No source map, and it is the parameter list that says why: an instance is not a slot.
        // A map belongs to a component, and what identifies a component here is the slot it was
        // added under — this entry point takes a bare `Rule`, which carries nothing to look one
        // up by. Every failure through here therefore reports in the space the guest was
        // compiled to. Nothing in a run reaches it: `lanekeep-engine` drives slots.
        self.settle(outcome, timeout, None)
    }

    /// Run one of an instance's rules' cross-file pass, under the default budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_reduce(
        &mut self,
        rule: &Rule,
        index: u32,
        context: &Resource<ReduceContext>,
    ) -> Result<(), WasmError> {
        self.call_reduce_with_timeout(rule, index, context, self.limits.rule_timeout)
    }

    /// Run one of an instance's rules' cross-file pass under an explicit budget.
    ///
    /// # Errors
    ///
    /// As [`WasmRuntime::call_check`].
    pub fn call_reduce_with_timeout(
        &mut self,
        rule: &Rule,
        index: u32,
        context: &Resource<ReduceContext>,
        timeout: Duration,
    ) -> Result<(), WasmError> {
        let borrow = Resource::new_borrow(context.rep());
        self.arm(timeout);
        let outcome = rule.call_reduce(&mut self.store, index, borrow);
        self.disarm();
        // Unmapped, for the reason `call_check_with_timeout` gives.
        self.settle(outcome, timeout, None)
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

    /// Fold a handler's two ways of failing into one.
    ///
    /// `check` and `reduce` return `result<_, rule-error>`, so there are three outcomes rather
    /// than two: the call succeeded, the call succeeded and the *rule* said it failed, or the
    /// call did not return at all. The middle one is a value the guest built on purpose and is
    /// passed through untouched; the last is a trap and goes to [`Self::classify`], which is
    /// where a breached budget outranks whatever the engine said.
    ///
    /// Keeping the distinction is the whole reason the world's signature changed. A guest that
    /// traps loses its message, its error type and its stack, so collapsing a returned failure
    /// into a trap here would throw away exactly what the channel was added to carry — and it
    /// would do it invisibly, since both spellings end the run.
    fn settle(
        &mut self,
        outcome: wasmtime::Result<Result<(), types::RuleError>>,
        timeout: Duration,
        source_map: Option<&SourceMap>,
    ) -> Result<(), WasmError> {
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(mut failure)) => {
                // The one place a guest's own positions are touched. They arrive in the space
                // the guest was compiled to — for a bundled TypeScript rule that is the flattened
                // entry module — and the component's own map is what turns them back into the
                // file its author edited. Untouched when there is no map, which is every Rust
                // rule and every component whose author shipped none.
                if let Some(map) = source_map {
                    failure.frames = map.remap(failure.frames);
                }
                Err(WasmError::from(failure))
            }
            Err(error) => Err(self.classify(&error, timeout)),
        }
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
