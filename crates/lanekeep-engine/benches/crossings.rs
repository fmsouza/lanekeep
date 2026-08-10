//! What one host-API crossing costs, in each engine, measured against the same rule.
//!
//! Architecture §4's invariant is that "JavaScript executes proportional to matches, never to
//! nodes", and that "any change that increases boundary crossings per file needs a benchmark,
//! not an argument". Moving a rule from QuickJS to a WebAssembly component was expected to
//! leave the *number* of crossings alone and change what one costs: the component model's
//! canonical ABI copies every string and every list through linear memory, where `rquickjs`
//! handed QuickJS a value directly.
//!
//! The "rules are WebAssembly components" decision was accepted with a condition attached to
//! exactly that: benchmark the per-crossing cost **before** the self-check rules are migrated,
//! because it is the one unmeasured quantity that could invert the performance argument. This
//! file is that measurement. `docs/architecture.md` §15.1 records what it came out at, with the
//! date and the machine — deliberately there and not here, because a number in a comment beside
//! the code that produces it is the one nobody re-runs before quoting.
//!
//! The count did not stay alone, as it happens: the port dropped about 30% of this rule's
//! crossings by hoisting two calls out of a loop. That is why every figure below is per arm.
//!
//! # The subject
//!
//! `lanekeep/no-unwrap`, which exists in both forms:
//!
//! - `benches/no-unwrap.ts` is the TypeScript original, byte-identical to the file deleted in
//!   `1fb5d06`. Check it rather than believe it —
//!   `git show 1fb5d06^:crates/lanekeep-rules/rules/no-unwrap.ts | diff - crates/lanekeep-engine/benches/no-unwrap.ts`
//!   is the whole verification, and it must stay empty.
//! - `crates/lanekeep-rules/components/no-unwrap.wasm` is the component that replaced it, read
//!   from where it ships rather than copied, so this bench cannot measure a stale one.
//!
//! It is the subject because it is the heavier of the two ported built-ins by a wide margin:
//! seven distinct host calls including `ancestors`, `named-children` and a `line`/`column` pair
//! per sibling, against `no-glob-import`'s two. A rule that crosses twice per match cannot make
//! a per-crossing cost visible above the cost of parsing the file it crossed about.
//!
//! # The experiment: two corpora that differ by six characters
//!
//! A ratio of whole-run times answers the wrong question. A cold run is dominated by reading,
//! hashing, parsing and query matching, none of which is a crossing, so a 5% difference in run
//! time could be a 5% difference in anything.
//!
//! So the corpora are a matched pair. Both are the same count of files with the same count of
//! functions, each function containing one method call, and they are **byte-identical except
//! for the method's name**:
//!
//! - `mapmap` — a name the rule ignores. Every match costs exactly **one** crossing, the
//!   `text` that reads the name, and the handler returns.
//! - `unwrap` — the name the rule is about. Every match walks the ancestor chain looking for a
//!   `#[test]`, which is where the crossings are.
//!
//! Same bytes, same trees, same node count, same match count, same handler invocation count.
//! Everything that is not a crossing appears identically on both sides and subtracts out, so
//!
//! ```text
//! (hot time − cold time) / (hot crossings − cold crossings)
//! ```
//!
//! is the marginal cost of a crossing, per engine, with no baseline and no shared constant to
//! believe in.
//!
//! # Counting, which is the half that is easy to get wrong
//!
//! Neither engine exposes a host-call counter, and adding one would put an increment on the
//! hot path of the trust boundary for the sake of a benchmark. What this file does instead is
//! **replay** the rule: [`Crossings::of`] walks the same trees the run walked, through the same
//! [`NodeArena`] both engines call, and follows `no-unwrap`'s decision procedure statement for
//! statement, counting each call the rule would have made. It is a transcription, and it is
//! held to the one thing a wrong transcription cannot fake: the replay records what it would
//! have reported, and [`gate`] asserts that set is exactly the run's violations, from both
//! engines. Control flow is what determines the count, and the reported set is what control
//! flow produced.
//!
//! **The two arms do not make the same number of calls, and that is the reason the denominator
//! is per-engine.** The port hoisted `line(ancestor)` and `column(ancestor)` out of the sibling
//! loop, so the component saves two crossings per sibling scanned; against that it pays one
//! more, because `filePath` is a *property* under QuickJS and a *method* on `check-context`. A
//! benchmark that divided both times by one crossing count would report the ratio of the two
//! rules' efficiency and call it the cost of the boundary.
//!
//! # What is included in "a crossing", stated plainly
//!
//! The whole cost of the call. `ctx.kind(n)` is a boundary crossing *and* an arena lookup *and*
//! a string handed back, and the figure covers all three, because all three are what a rule
//! pays to ask the question.
//!
//! **It is not a measurement of the boundary alone, and the difference between the two arms is
//! not one either.** Between two crossings a rule also *executes*, and that execution is
//! interpreted bytecode in one arm and compiled code in the other. Those two effects run in
//! opposite directions inside one number. The report separates out the arena work, which is
//! shared code and can be timed by replaying the same calls with no engine in the way; what
//! remains is the crossing **plus** the in-guest execution, and nothing here can split it
//! further. So the honest reading of a `rest` column that favors the component is not "the
//! canonical ABI is cheaper than `rquickjs`" — it is "whatever the canonical ABI costs extra,
//! compiled guest code more than pays back".
//!
//! The mix is this rule's, not a universal one: mostly scalar and short-string returns
//! (`kind`, `text`, `line`, `column`), plus one `named-children` per subject that returns a
//! list as long as the file has top-level items. A rule with a different mix will see a
//! different number, and a rule that moves lists will see the canonical ABI's copy cost more
//! than this one does.
//!
//! # Not a gate on time
//!
//! `benches/corpus.rs` explains at length why absolute times cannot be asserted on a hosted
//! runner, and nothing here changes that. This file prints its figures and asserts only what
//! is machine-independent: that the replay agrees with both engines about what was reported,
//! which is what makes every number below mean anything.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic,
    clippy::format_push_string,
    reason = "A benchmark is test scaffolding that `clippy.toml`'s allow-*-in-tests does not \
              reach, and its whole output is a printed report. The string building runs once at \
              setup, outside everything timed, on the same terms `benches/corpus.rs` sets out."
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lanekeep_engine::Engine;
use lanekeep_js::RuleRoot;
use lanekeep_lang::Language;
use lanekeep_lang_js::{JavaScript, TypeScript};
use lanekeep_nodes::{Handle, NodeArena};

/// One reported violation, reduced to what both arms have to agree on.
///
/// Not the rule id or the message, and that is not laziness: both are also identical, and the
/// expectation table in `crates/lanekeep-rules/tests/no_unwrap.rs` is where they are pinned,
/// case by case. What this bench needs is the *set*, because the set is what control flow
/// produced and control flow is what the call counts describe.
type Violation = (String, u32, u32);

/// Files in each corpus.
///
/// Small on purpose, twice over. What this measures is a *difference*, and everything the file
/// count buys — reading, hashing, parsing, discovery — is on both sides of the subtraction, so
/// files past the point where the run is worth measuring at all only add variance to both
/// terms. And this bench runs in CI: `just bench` is the `bench (budgets)` job, which already
/// carries `corpus.rs`. Forty files is about five seconds here, and the difference it produces
/// is still more than an order of magnitude larger than the run-to-run spread.
const FILES: usize = 40;

/// Top-level functions per file, each holding one method call.
///
/// This is the number that sets the signal, because `no-unwrap` scans the parent's children
/// looking for the attributes attached to the item it is inside: a call in the *k*-th function
/// costs a scan of *k* siblings. So crossings per file grow with the square of this while
/// parse cost grows with the first power, and the ratio of what is being measured to what is
/// being subtracted improves as it rises.
///
/// Held to ninety-six rather than pushed higher because the scan's cost is also the length of
/// the list `named-children` returns, and a component pays the canonical ABI's copy on that
/// list. A file with a thousand top-level items would make the component's number worse by
/// measuring something no Rust file does.
const FUNCTIONS: usize = 96;

/// Attempts per measurement, the best of which is taken.
///
/// Best rather than mean, for the reason `benches/corpus.rs` gives: every source of noise on a
/// shared machine only ever adds.
const ATTEMPTS: usize = 3;

/// The method name that engages the rule.
const HOT_METHOD: &str = "unwrap";

/// The method name that does not, in the same six characters.
///
/// Same length is the point: the two corpora are byte-identical, so nothing downstream of the
/// bytes — hashing, parsing, node count, match count — can differ between them.
const COLD_METHOD: &str = "mapmap";

/// The TypeScript rule, as it was before the component replaced it.
///
/// `include_str!` rather than a literal here, so the file on disk is the artifact a reader
/// diffs against `1fb5d06^` — see this module's documentation.
const TYPESCRIPT_RULE: &str = include_str!("no-unwrap.ts");

/// A global budget far past anything measured here, so the product's default cannot end a
/// measurement. The same device `benches/corpus.rs` uses, for the same reason.
const BENCH_GLOBAL_TIMEOUT_MS: u64 = 600_000;

/// Which implementation is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arm {
    /// `no-unwrap.ts`, executed by QuickJS.
    TypeScript,
    /// `no-unwrap.wasm`, executed by wasmtime.
    Component,
}

impl Arm {
    const ALL: [Self; 2] = [Self::TypeScript, Self::Component];

    const fn label(self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript (QuickJS)",
            Self::Component => "component (wasmtime)",
        }
    }

    /// The config naming this arm's rule. Both are `lanekeep.json`, so the two runs differ in
    /// the rule they name and in nothing else — not even the format their config is read in.
    const fn config(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript.json",
            Self::Component => "component.json",
        }
    }
}

/// Which corpus is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Corpus {
    /// The method the rule ignores: one crossing per match.
    Cold,
    /// The method the rule is about: the full ancestor walk.
    Hot,
}

impl Corpus {
    const fn method(self) -> &'static str {
        match self {
            Self::Cold => COLD_METHOD,
            Self::Hot => HOT_METHOD,
        }
    }
}

fn main() {
    single_threaded();

    let cold = Project::build(Corpus::Cold);
    let hot = Project::build(Corpus::Hot);

    let mut measured = Vec::new();
    let mut reported = Vec::new();
    for arm in Arm::ALL {
        // The violations come back from the measured runs themselves rather than from four
        // more runs made to collect them. They are the same runs either way, and a bench that
        // runs in CI should not pay twice for one answer.
        let (cold_time, cold_violations) = cold.measure(arm);
        let (hot_time, hot_violations) = hot.measure(arm);
        measured.push((arm, cold_time, hot_time));
        reported.push((Corpus::Cold, arm, cold_violations));
        reported.push((Corpus::Hot, arm, hot_violations));
    }

    // After the engine runs, not before them. The replay is single-threaded work over the same
    // corpora, so putting it first would leave the page cache and the allocator in a state the
    // measured runs did not choose for themselves.
    let cold_calls = Crossings::of(&cold);
    let hot_calls = Crossings::of(&hot);

    report(&measured, &cold_calls, &hot_calls);
    gate(&reported, &cold_calls, &hot_calls);
}

/// Confine the whole run to one thread.
///
/// Two reasons, and the second is the one that would have made the figures wrong.
///
/// A number this bench prints as "ns per call" has to *be* nanoseconds per call. Elapsed time
/// over a fourteen-core run divided by a call count is a throughput, and a reader who compared
/// it against anything they know about either engine would be out by the width of the machine.
///
/// The other is that `rayon`'s `map_init` runs its initializer per *chunk*, and `AGENTS.md`
/// records that the chunk count is a distribution rather than a bound — two runs over one
/// corpus disagree, because rayon splits on how the work is going. Each initializer builds a
/// QuickJS sandbox or a wasmtime store, which is expensive and which does **not** cancel out of
/// a hot-minus-cold difference: the two corpora have different per-item costs, so they get
/// different splits. One thread makes that term a constant, which is what a subtraction needs.
fn single_threaded() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("no global pool has been built yet");
}

/// Print the table, whichever way the figures come out.
///
/// Whichever way is the point. This is the one quantity that could have inverted the argument
/// for putting rules in components, and a benchmark reported only when it is favorable is not a
/// benchmark.
fn report(measured: &[(Arm, Duration, Duration)], cold: &Crossings, hot: &Crossings) {
    println!(
        "\nlanekeep host-API crossings — {FILES} files, {FUNCTIONS} functions each, \
         `lanekeep/no-unwrap`, one thread\n"
    );
    println!(
        "  {:<22} {:>9} {:>9} {:>9} {:>12} {:>9} {:>9} {:>9}",
        "arm", "cold", "hot", "delta", "calls", "ns/call", "arena", "rest"
    );

    let mut per_call = Vec::new();
    for &(arm, cold_time, hot_time) in measured {
        let delta = hot_time.saturating_sub(cold_time);
        let calls = hot.total(arm) - cold.total(arm);
        let arena = hot.arena_time(arm).saturating_sub(cold.arena_time(arm));

        #[expect(
            clippy::cast_precision_loss,
            reason = "A call count is far below 2^53, so the cast is exact."
        )]
        let divisor = calls as f64;
        let ns = delta.as_secs_f64() * 1e9 / divisor;
        let arena_ns = arena.as_secs_f64() * 1e9 / divisor;
        per_call.push(ns);

        println!(
            "  {:<22} {:>8.1?} {:>8.1?} {:>8.1?} {calls:>12} {ns:>8.1} {arena_ns:>8.1} {:>8.1}",
            arm.label(),
            cold_time,
            hot_time,
            delta,
            ns - arena_ns,
        );
    }
    // Said here rather than left to a reader to assume, because the obvious assumption is the
    // wrong one. `rest` is not the boundary: it is the boundary *plus* whatever the rule itself
    // executes between two calls, which is interpreted bytecode on one side and compiled code on
    // the other. Those two move in opposite directions, so the column bounds how much room the
    // boundary has rather than measuring it.
    println!(
        "\n  `arena` is that same call sequence replayed against the same NodeArena with no \
         engine in the way.\n  `rest` is everything else: the crossing, and the rule's own \
         execution between crossings — interpreted\n  in one arm and compiled in the other, \
         which is why this cannot separate them."
    );

    println!("\n  calls counted by replay — the two rules do not make the same ones:");
    for arm in Arm::ALL {
        println!(
            "    {:<22} cold {:>10}  hot {:>10}",
            arm.label(),
            cold.total(arm),
            hot.total(arm)
        );
    }

    if let [typescript, component] = per_call.as_slice() {
        println!(
            "\n  a host call costs {:.2}x through a component ({component:.0} ns against \
             {typescript:.0} ns)\n",
            component / typescript
        );
    }
}

/// The one machine-independent assertion: the replay agrees with both engines.
///
/// Nothing above means anything if the replay followed a different path through the rule than
/// the rule did, and the reported set is what a path produces. Both arms are checked, because
/// the two implementations are held to reporting identically and a divergence here would be
/// that claim failing rather than this bench being wrong.
fn gate(reported: &[(Corpus, Arm, Vec<Violation>)], cold: &Crossings, hot: &Crossings) {
    for (corpus, arm, violations) in reported {
        let replayed = match corpus {
            Corpus::Cold => &cold.reports,
            Corpus::Hot => &hot.reports,
        };
        assert_eq!(
            violations,
            replayed,
            "the replay and the {} arm disagree about what {corpus:?} reports, so the call \
             counts above describe a rule neither engine ran",
            arm.label(),
        );
    }
    println!("  replay agrees with both engines on every violation\n");
}

/// One corpus, with both configs beside it.
struct Project {
    dir: PathBuf,
    /// The result cache, taken from the cache crate rather than spelled out.
    ///
    /// It is a *file*, and `.lanekeep/` is a directory holding it beside the precompiled
    /// components. Reaching for `remove_dir_all(".lanekeep/cache")` is the obvious mistake and
    /// it is a silent one: the call fails, every measured run is a cache hit, and a cache hit
    /// executes no rule at all — so the bench reports a per-crossing cost for a boundary that
    /// was never crossed. It read 3.8 ns against 5.7 ns before this was noticed, both of which
    /// are far too small for a host call and neither of which meant anything.
    cache: PathBuf,
}

impl Project {
    fn build(corpus: Corpus) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-bench-crossings-{}-{:?}",
            std::process::id(),
            corpus
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let project = Self {
            cache: lanekeep_cache::Store::path_for(&dir),
            dir,
        };
        project.write("no-unwrap.ts", TYPESCRIPT_RULE.as_bytes());
        project.write("no-unwrap.wasm", &component_bytes());
        for arm in Arm::ALL {
            let rule = match arm {
                Arm::TypeScript => "./no-unwrap.ts",
                Arm::Component => "./no-unwrap.wasm",
            };
            project.write(arm.config(), config_source(rule).as_bytes());
        }
        for index in 0..FILES {
            project.write(
                &format!("src/m{index:04}.rs"),
                file_source(corpus.method()).as_bytes(),
            );
        }
        project
    }

    fn write(&self, path: &str, contents: &[u8]) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// The engine for one arm, prepared once.
    ///
    /// Outside every measurement on purpose. Preparation loads the config, compiles the query
    /// and — for the component arm — compiles the component, and none of that is a crossing.
    /// It is also identical between the two corpora, so leaving it in would only add a term
    /// that subtracts to zero while carrying its own variance.
    fn engine(&self, arm: Arm) -> Engine {
        let root = RuleRoot::new(&self.dir).expect("canonicalizes");
        let config_path = self.dir.join(arm.config());
        let sandbox =
            lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                .expect("sandbox builds");
        let config = lanekeep_config::load(&sandbox, &root, &config_path).expect("config loads");

        Engine::prepare(
            &config,
            &self.dir,
            root,
            &config_path,
            &lanekeep_languages::registry(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .expect("engine prepares")
    }

    /// The best of [`ATTEMPTS`] cold runs.
    ///
    /// Cold, because a warm run serves the cache and executes no rule at all — there is no
    /// crossing on that path to measure. Only the *result* cache is removed between attempts:
    /// `.lanekeep/components` holds the component wasmtime precompiled, and rebuilding that
    /// per attempt would measure Cranelift.
    fn measure(&self, arm: Arm) -> (Duration, Vec<Violation>) {
        let engine = self.engine(arm);

        // Discarded: the first run pays for the OS page cache, for the component's `.cwasm`,
        // and for whatever the allocator does when it first sees this shape of work.
        let _ = engine.run().expect("warm-up run");

        let mut best = Duration::MAX;
        let mut reported = Vec::new();
        for _ in 0..ATTEMPTS {
            let _ = std::fs::remove_file(&self.cache);
            let start = Instant::now();
            let outcome = engine.run().expect("measured run");
            best = best.min(start.elapsed());

            // After the clock has been read. Every run reports the same thing, so which
            // attempt this comes from does not matter; that it is not inside the measurement
            // does.
            reported = outcome
                .violations
                .iter()
                .map(|v| {
                    (
                        v.location.file.to_string(),
                        v.location.position.line,
                        v.location.position.column,
                    )
                })
                .collect();
        }
        (best, reported)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The shipped component, read from where it ships.
///
/// At run time rather than `include_bytes!`, and from `lanekeep-rules` rather than through a
/// dependency on it: `lanekeep-rules` dev-depends on `lanekeep-testkit`, which depends on this
/// crate, so a dev-dependency here would close a cycle in the publication order that
/// `cargo publish` resolves by refusing.
fn component_bytes() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lanekeep-rules/components/no-unwrap.wasm");
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("reads the shipped component at {}: {e}", path.display()))
}

/// A `lanekeep.json` naming one rule.
///
/// JSON for both arms, which matters: a `lanekeep.config.ts` cannot name a component at all, so
/// a TypeScript config on one side would have made the config format a second difference
/// between the two runs.
///
/// No `namespaces` entry, and that is what lets both arms keep the id `lanekeep/no-unwrap` —
/// the namespace is one of the two the tool defines, so it needs no declaration, and declaring
/// it is refused outright. Both arms therefore report under the same id as well as at the same
/// positions, which is the claim the port was held to.
fn config_source(rule: &str) -> String {
    format!(
        r#"{{"include": ["src/**/*.rs"],
  "timeouts": {{"global": {BENCH_GLOBAL_TIMEOUT_MS}}},
  "rules": ["{rule}"]}}"#
    )
}

/// One source file: [`FUNCTIONS`] top-level functions, each with one method call.
///
/// Deliberately without attributes, `use` declarations or nested items. Every named child of
/// the root is a `function_item`, so the sibling scan `no-unwrap` performs is uniform and the
/// replay has nothing to special-case. Every function is on its own lines, which is what makes
/// the rule's position comparison identify the right one.
fn file_source(method: &str) -> String {
    let mut out = String::with_capacity(FUNCTIONS * 64);
    for index in 0..FUNCTIONS {
        out.push_str(&format!(
            "pub fn f{index}(holder: Holder) -> u32 {{\n    \
             let value = holder.{method}();\n    \
             value\n\
             }}\n\n"
        ));
    }
    out
}

/// How many host calls each arm makes over one corpus, what it would report, and how long the
/// replay itself took.
///
/// See this module's documentation for why this is a replay rather than a counter. The timing
/// is a second use of the same walk: the replay makes the same [`NodeArena`] calls in the same
/// order as the host does, and makes them with no engine in the way. Subtracting it from an
/// engine's figure does **not** leave the boundary — it leaves the boundary plus whatever the
/// rule executes between two calls, which is the caveat the report prints. What it does buy is
/// the scale: whether "a call costs 1.1x" is a call that is mostly boundary or a call that is
/// two-thirds shared arena work with the engines differing over the last third.
struct Crossings {
    typescript: u64,
    component: u64,
    /// Time in [`replay`] alone, per arm. Parsing and [`subjects`] are outside it.
    typescript_arena: Duration,
    component_arena: Duration,
    /// The set [`gate`] holds both engines to, sorted.
    reports: Vec<Violation>,
}

impl Crossings {
    const fn total(&self, arm: Arm) -> u64 {
        match arm {
            Arm::TypeScript => self.typescript,
            Arm::Component => self.component,
        }
    }

    const fn arena_time(&self, arm: Arm) -> Duration {
        match arm {
            Arm::TypeScript => self.typescript_arena,
            Arm::Component => self.component_arena,
        }
    }

    /// Walk every file the run walked, and count.
    ///
    /// The arenas are rebuilt for every attempt and every arm, which looks wasteful and is the
    /// only way the timing means anything. A `NodeArena` interns lazily, so a second pass over
    /// one arena finds every handle already there and costs a fraction of the first — while the
    /// engine, whose cache is removed between measured runs, pays the first pass every time.
    /// Reusing one arena would have made the replay look cheap and the boundary look expensive,
    /// in exactly the direction that flatters the conclusion.
    fn of(project: &Project) -> Self {
        let mut typescript = 0;
        let mut component = 0;
        let mut typescript_arena = Duration::MAX;
        let mut component_arena = Duration::MAX;
        let mut reports = Vec::new();

        let sources: Vec<(String, String)> = (0..FILES)
            .map(|index| {
                let relative = format!("src/m{index:04}.rs");
                let source = std::fs::read_to_string(project.dir.join(&relative))
                    .expect("reads a corpus file");
                (relative, source)
            })
            .collect();

        for attempt in 0..ATTEMPTS {
            for arm in Arm::ALL {
                // Collected on the last attempt of one arm only. Both arms report the same set —
                // that is the claim the port was held to — so collecting from either is enough,
                // and collecting from every pass would only build the same vector six times.
                let collecting = attempt + 1 == ATTEMPTS && arm == Arm::Component;
                let mut calls = 0;
                let mut elapsed = Duration::ZERO;

                for (relative, source) in &sources {
                    let (mut arena, matches) = parsed(source);
                    let mut collected = collecting.then(|| Vec::with_capacity(matches.len()));

                    let start = Instant::now();
                    calls += replay(arm, &mut arena, relative, &matches, &mut collected);
                    elapsed += start.elapsed();

                    for position in collected.unwrap_or_default() {
                        reports.push((relative.clone(), position.0, position.1));
                    }
                }

                match arm {
                    Arm::TypeScript => {
                        typescript = calls;
                        typescript_arena = typescript_arena.min(elapsed);
                    }
                    Arm::Component => {
                        component = calls;
                        component_arena = component_arena.min(elapsed);
                    }
                }
            }
        }

        reports.sort_unstable();
        Self {
            typescript,
            component,
            typescript_arena,
            component_arena,
            reports,
        }
    }
}

/// One file's arena, and the captures the rule's query binds in it.
///
/// **Only the captures are interned**, which is what the query engine does and is the whole
/// reason this does not simply walk the arena. Walking it would intern every node in the file
/// on the way, so the replay would enter a fully populated arena where the engine enters a
/// nearly empty one — and every lazy intern the rule triggers, which is most of what
/// `named-children` costs, would already have been paid.
fn parsed(source: &str) -> (NodeArena, Vec<(Handle, Handle)>) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lanekeep_lang_rust::Rust.grammar())
        .expect("sets the Rust grammar");
    let tree = parser.parse(source, None).expect("parses");

    let mut arena = NodeArena::new(tree, source.to_owned());
    let paths = subjects(&arena);
    let matches = paths
        .into_iter()
        .filter_map(|(method, call)| Some((arena.intern_path(method)?, arena.intern_path(call)?)))
        .collect();
    (arena, matches)
}

/// Every `(method, call)` pair the rule's query binds, as arena paths, in tree order.
///
/// The query is
/// `(call_expression function: (field_expression field: (field_identifier) @method)) @call`.
/// Matched by walking the tree-sitter tree rather than by compiling it: what the replay needs
/// is the pairs, and running the real query here would make this file depend on the query
/// engine agreeing with itself, which is not what is under test.
fn subjects(arena: &NodeArena) -> Vec<(Vec<u32>, Vec<u32>)> {
    let mut found = Vec::new();
    let mut stack = vec![arena.tree().root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && let Some(method) = node
                .child_by_field_name("function")
                .filter(|function| function.kind() == "field_expression")
                .and_then(|function| function.child_by_field_name("field"))
                .filter(|field| field.kind() == "field_identifier")
            && let Some(pair) = arena.path_of(method).zip(arena.path_of(node))
        {
            found.push(pair);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    found
}

/// `no-unwrap`'s `check`, replayed, counting every host call it makes.
///
/// Read this beside `benches/no-unwrap.ts` and `rust-rules/no-unwrap/src/lib.rs`. Where the two
/// differ, the difference is called out at the line it happens on — those differences are the
/// whole reason the denominators are per-arm.
///
/// **Every counted call performs its arena lookup**, including the ones one arm repeats and the
/// other hoists. Reusing a position already in hand would be the natural way to write this and
/// would break the timing the caller takes off it: the TypeScript arm's extra `line(ancestor)`
/// and `column(ancestor)` would then cost nothing, and the arena work would come out identical
/// for two arms that do measurably different amounts of it.
///
/// `reports` collects the positions when it is `Some`, which is how [`gate`] gets the set both
/// engines are held to.
fn replay(
    arm: Arm,
    arena: &mut NodeArena,
    path: &str,
    matches: &[(Handle, Handle)],
    reports: &mut Option<Vec<(u32, u32)>>,
) -> u64 {
    let mut calls = 0;

    for &(method_node, call) in matches {
        // `ctx.text(m.method)`
        calls += 1;
        let method = arena.text(method_node).unwrap_or_default();
        if method != "unwrap" && method != "expect" {
            continue;
        }

        // `ctx.filePath`, and the first place the two arms diverge: a property installed on the
        // context object under QuickJS, so no call at all, against `check-context.file-path`,
        // which is one.
        if arm == Arm::Component {
            calls += 1;
        }

        if path.contains("/tests/") || path.starts_with("tests/") {
            continue;
        }
        if in_test_code(arm, arena, call, &mut calls) {
            continue;
        }

        // No `allow` patterns are configured, so the loop over them makes no call either way.

        // `ctx.report(m.call, ...)`
        calls += 1;
        if let (Some(collected), Some(position)) = (reports.as_mut(), arena.position(call)) {
            collected.push(position);
        }
    }

    calls
}

/// `no-unwrap`'s `inTestCode` / `in_test_code`, replayed.
fn in_test_code(arm: Arm, arena: &mut NodeArena, node: Handle, calls: &mut u64) -> bool {
    // `ctx.ancestors(node)`
    *calls += 1;
    let chain = arena.ancestors(node);

    for index in 0..chain.len() {
        let ancestor = chain[index];

        // `ctx.kind(ancestor)`
        *calls += 1;
        let kind = arena.kind(ancestor).unwrap_or_default();
        if kind != "function_item" && kind != "mod_item" {
            continue;
        }

        let Some(&parent) = chain.get(index + 1) else {
            continue;
        };

        // The second divergence, and the larger one. The component reads the item's own line
        // and column *once*, here, with `line` and `column` being separate methods that each
        // resolve the node. The TypeScript original asks for both again on every sibling it
        // compares against, which is what the two branches in the loop below are.
        let hoisted = if arm == Arm::Component {
            *calls += 2;
            let (Some(line), Some(column)) = (
                arena.position(ancestor).map(|p| p.0),
                arena.position(ancestor).map(|p| p.1),
            ) else {
                continue;
            };
            Some((line, column))
        } else {
            None
        };

        // `ctx.namedChildren(parent)`
        *calls += 1;
        let siblings = arena.named_children(parent);

        // Whether each attribute in the current run says `test`, rather than its text. The
        // decision is the same and the string does not have to be kept alive to make it — and
        // in this corpus the branch is never taken at all, since nothing carries an attribute.
        let mut attached: Vec<bool> = Vec::new();
        for sibling in siblings {
            // `ctx.kind(sibling)`
            *calls += 1;
            if arena.kind(sibling) == Some("attribute_item") {
                // `ctx.text(sibling)`
                *calls += 1;
                attached.push(arena.text(sibling).is_some_and(mentions_test));
                continue;
            }

            // `ctx.line(sibling)`, and under QuickJS `ctx.line(ancestor)` with it.
            *calls += 1;
            let sibling_line = arena.position(sibling).map(|p| p.0);
            let ancestor_line = if let Some((line, _)) = hoisted {
                Some(line)
            } else {
                *calls += 1;
                arena.position(ancestor).map(|p| p.0)
            };

            if sibling_line == ancestor_line {
                // `ctx.column(sibling)`, and under QuickJS `ctx.column(ancestor)` with it.
                // Both implementations short-circuit the `&&`, so neither is reached when the
                // lines already differ.
                *calls += 1;
                let sibling_column = arena.position(sibling).map(|p| p.1);
                let ancestor_column = if let Some((_, column)) = hoisted {
                    Some(column)
                } else {
                    *calls += 1;
                    arena.position(ancestor).map(|p| p.1)
                };

                if sibling_column == ancestor_column {
                    if attached.iter().any(|&says_test| says_test) {
                        return true;
                    }
                    break;
                }
            }

            attached.clear();
        }
    }

    false
}

/// `\btest\b`, as both implementations spell it.
fn mentions_test(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices("test").any(|(start, needle)| {
        let before = start.checked_sub(1).and_then(|index| bytes.get(index));
        let after = bytes.get(start + needle.len());
        before.is_none_or(|b| !is_word_byte(*b)) && after.is_none_or(|b| !is_word_byte(*b))
    })
}

/// Whether a byte is one JavaScript's `\w` matches. ASCII only, as `\w` is.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
