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
//! The third arm answers a question the first two cannot. A Rust component and a TypeScript
//! module differ in two things at once — the boundary they cross and the language they are
//! written in — so a ratio between them cannot say which of the two moved. A component built
//! from *the same TypeScript source* pays the canonical ABI **and** carries a JavaScript engine
//! of its own, and comparing it against the same rule under QuickJS is the closest this can get
//! to holding the rule still and changing only the engine underneath it.
//!
//! # The subject
//!
//! `lanekeep/no-unwrap`, which exists in three forms:
//!
//! - `benches/no-unwrap.ts` is the TypeScript original, byte-identical to the file deleted in
//!   `1fb5d06`. Check it rather than believe it —
//!   `git show 1fb5d06^:crates/lanekeep-rules/rules/no-unwrap.ts | diff - crates/lanekeep-engine/benches/no-unwrap.ts`
//!   is the whole verification, and it must stay empty.
//! - `crates/lanekeep-rules/components/no-unwrap.wasm` is the component that replaced it, read
//!   from where it ships rather than copied, so this bench cannot measure a stale one.
//! - `target/bench/no-unwrap-js.wasm` is that same TypeScript source inside StarlingMonkey,
//!   built by `just bench-js-component` from `benches/no-unwrap-entry.ts` with the flags the
//!   shipped built-ins component is built with.
//!
//! **The third is not committed, and this bench runs without it.** It is 13 MB; every crate in
//! this workspace is published and crates.io refuses a package over 10 MiB, so committing it
//! would make `lanekeep-engine` unpublishable in order to hold a benchmark input. It is built
//! into `target/` instead, which needs Node and `jco` — neither of which any gate may require —
//! and when it is absent the report prints two arms and says which recipe produces the third.
//! Every assertion below is per arm, so the missing arm removes a row rather than weakening a
//! claim.
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
//! statement, counting each call the rule would have made.
//!
//! Two things hold that transcription honest, and the weaker one is the one that looks
//! convincing.
//!
//! **The assertion.** The replay records what it would have reported, and [`gate`] holds *each
//! arm* to *its own branch* of the replay, then the two branches to each other. Control flow
//! determines the count and the reported set is what control flow produced, so a branch that
//! took a different path could not agree. What this cannot see is a call on a path all three
//! take which the replay simply omits — the count would be low and every report identical.
//!
//! **The arithmetic, which is what actually pins the numbers.** The corpus is regular enough
//! that all three denominators are derivable by hand and were: `FILES` files of `FUNCTIONS`
//! functions, an ancestor chain of four, and a call in the *k*-th function scanning *k*
//! siblings, gives `13 + 3k` calls per subject under QuickJS, `14 + 2k` under the Rust
//! component and `14 + 3k` under the JavaScript one, which sums to exactly the 597,120, 418,560
//! and 600,960 the replay prints. A count that agrees with closed-form arithmetic over the
//! corpus geometry is not resting on the transcription being faithful; it is two independent
//! derivations of one number. **Anyone changing the corpus shape or the rule should redo that
//! sum rather than trust these two paragraphs**, because the assertion alone would not notice.
//!
//! **The three arms do not make the same number of calls, and that is the reason the
//! denominator is per-engine.** The Rust port hoisted `line(ancestor)` and `column(ancestor)`
//! out of the sibling loop, so it saves two crossings per sibling scanned; against that it pays
//! one more, because `filePath` is a *property* under QuickJS and a *method* on
//! `check-context`. The JavaScript component runs the unported source, so it keeps the loop and
//! pays that same extra call — `host.js` memoizes `filePath` per `check`, and `check` is per
//! match, so it is one crossing for every match the rule engages with. A benchmark that divided
//! every time by one crossing count would report the ratio of the rules' efficiency and call it
//! the cost of the boundary.
//!
//! # What is included in "a crossing", stated plainly
//!
//! The whole cost of the call. `ctx.kind(n)` is a boundary crossing *and* an arena lookup *and*
//! a string handed back, and the figure covers all three, because all three are what a rule
//! pays to ask the question.
//!
//! **It is not a measurement of the boundary alone, and the difference between two arms is not
//! one either.** Between two crossings a rule also *executes*, and that execution is
//! interpreted bytecode in one arm, compiled code in another and bytecode inside a compiled
//! engine in the third. Those effects run in opposite directions inside one number. The report
//! separates out the arena work, which is shared code and can be timed by replaying the same
//! calls with no engine in the way; what remains is the crossing **plus** the in-guest
//! execution, and nothing here can split it further. So the honest reading of a `rest` column
//! that favors the Rust component is not "the canonical ABI is cheaper than `rquickjs`" — it is
//! "whatever the canonical ABI costs extra, compiled guest code more than pays back". The
//! JavaScript arm is where that cancellation is undone: it pays the same canonical ABI with no
//! compiled rule body to pay it back with, and a host call from it also traverses `host.js`'s
//! `ctx` shim, the generated bindings and StarlingMonkey's own call machinery.
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
    /// `no-unwrap.wasm`, the Rust port, executed by wasmtime.
    Component,
    /// `no-unwrap-js.wasm`, the *same TypeScript source*, executed by StarlingMonkey inside
    /// wasmtime.
    JavaScript,
}

impl Arm {
    const ALL: [Self; 3] = [Self::TypeScript, Self::Component, Self::JavaScript];

    const fn label(self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript (QuickJS)",
            Self::Component => "Rust component",
            Self::JavaScript => "JS component",
        }
    }

    /// Where this arm's numbers go in the arrays [`Crossings`] keeps.
    const fn index(self) -> usize {
        match self {
            Self::TypeScript => 0,
            Self::Component => 1,
            Self::JavaScript => 2,
        }
    }

    /// The config naming this arm's rule. All three are `lanekeep.json`, so the runs differ in
    /// the rule they name and in nothing else — not even the format their config is read in.
    const fn config(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript.json",
            Self::Component => "component.json",
            Self::JavaScript => "javascript.json",
        }
    }

    /// The rule this arm's config names, as the config spells it.
    const fn rule(self) -> &'static str {
        match self {
            Self::TypeScript => "./no-unwrap.ts",
            Self::Component => "./no-unwrap.wasm",
            Self::JavaScript => "./no-unwrap-js.wasm",
        }
    }

    /// Whether a rule crosses the component boundary to read `ctx.filePath`.
    ///
    /// Both components do, once per engaged match: it is a `check-context` method, and
    /// `host.js` memoizes it for the life of one `check` call. Under QuickJS it is an ordinary
    /// value property on the context object (`lanekeep_js::host`'s `object.set("filePath", …)`)
    /// and costs nothing.
    const fn calls_file_path(self) -> bool {
        !matches!(self, Self::TypeScript)
    }

    /// Whether this arm hoists the item's own line and column out of the sibling loop.
    ///
    /// The Rust port does; the two arms running `no-unwrap.ts` do not, because that is the
    /// source they run.
    const fn hoists_position(self) -> bool {
        matches!(self, Self::Component)
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

    // Read once and lent to both corpora: 13 MB, and a second read would only prove the file
    // is still there.
    let javascript = javascript_component_bytes();
    let arms: Vec<Arm> = Arm::ALL
        .into_iter()
        .filter(|arm| *arm != Arm::JavaScript || javascript.is_some())
        .collect();

    let cold = Project::build(Corpus::Cold, javascript.as_deref());
    let hot = Project::build(Corpus::Hot, javascript.as_deref());

    let mut measured = Vec::new();
    let mut reported = Vec::new();
    for arm in arms.iter().copied() {
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
        per_call.push((arm, ns));

        println!(
            "  {:<22} {:>8.1?} {:>8.1?} {:>8.1?} {calls:>12} {ns:>8.1} {arena_ns:>8.1} {:>8.1}",
            arm.label(),
            cold_time,
            hot_time,
            delta,
            ns - arena_ns,
        );
    }
    // Two warnings, said here rather than left to a reader to assume, because the obvious
    // assumption is wrong in both cases.
    //
    // `rest` is not the boundary: it is the boundary *plus* whatever the rule itself executes
    // between two calls, which is interpreted bytecode on one side and compiled code on the
    // other. Those move in opposite directions, so the column bounds how much room the boundary
    // has rather than measuring it.
    //
    // And `rest` is a residue. It is a difference of two separately timed quantities, so it
    // inherits the whole of the engine measurement's absolute noise on about a third of the
    // magnitude — measured at roughly 15% run to run on the component arm against 4% for the
    // `ns/call` it came from. The `arena` figures are steady to a percent or two; the gap
    // between the two arms' `rest` is not larger than the spread of the noisier one. Read the
    // split as a scale rather than as a measurement, and read the ratio below as the result.
    println!(
        "\n  `arena` is that same call sequence replayed against the same NodeArena with no \
         engine in the way.\n  `rest` is everything else: the crossing, and the rule's own \
         execution between crossings — interpreted\n  in one arm and compiled in the other, \
         which is why this cannot separate them. It is also a residue, and\n  moves by ~15% \
         between runs where `ns/call` moves by ~4%: indicative, not measured."
    );

    println!("\n  calls counted by replay — the three rules do not make the same ones:");
    for &(arm, _) in &per_call {
        println!(
            "    {:<22} cold {:>10}  hot {:>10}",
            arm.label(),
            cold.total(arm),
            hot.total(arm)
        );
    }

    // Every ratio against QuickJS, which is what the migration is a move away from. Printed per
    // arm rather than as one line, so an absent JavaScript arm removes a row instead of
    // silently changing what the surviving line means.
    println!();
    let baseline = per_call
        .iter()
        .find(|(arm, _)| *arm == Arm::TypeScript)
        .map(|&(_, ns)| ns);
    for &(arm, ns) in &per_call {
        let Some(baseline) = baseline.filter(|_| arm != Arm::TypeScript) else {
            continue;
        };
        println!(
            "  a host call costs {:.2}x through the {} ({ns:.0} ns against {baseline:.0} ns)",
            ns / baseline,
            arm.label(),
        );
    }

    if per_call.len() < Arm::ALL.len() {
        println!(
            "\n  the JavaScript component arm did not run: {} is not there.\n  \
             `just bench-js-component` builds it — it needs Node and jco, which no gate may \
             require,\n  and it is 13 MB, which is why it is not committed.",
            javascript_component_path().display()
        );
    }
    println!();
}

/// The machine-independent assertions: each branch of the replay agrees with its own engine.
///
/// Nothing above means anything if the replay followed a different path through the rule than
/// the rule did, and the reported set is what a path produces.
///
/// **Each arm against its own branch, which is a stronger claim than it looks.** The replay has
/// a branch per arm, and they produce the different denominators the report divides by. Holding
/// every engine to *one* of those branches would leave the others validated by nothing — the
/// engines would agree with each other and with whichever branch was collected, and the
/// uncollected branches could count anything at all. So the comparison is per arm, and the
/// branches are then also compared against each other: they transcribe one rule and are held to
/// the reporting parity the port itself was held to.
///
/// That is not a theory about an earlier version, it is what the earlier version did. Measured
/// by breaking the TypeScript branch alone — an extra `continue` before its `report`, leaving
/// the component branch untouched — which this gate fails on and which the version that
/// collected only from the component branch passed. An assertion that cannot fail on half the
/// code it is supposed to cover is worth less than its wording suggests.
///
/// **The cross-branch comparison covers every arm, including one that did not run.** The
/// branches are pure functions of the corpus, so a missing artifact removes an engine from the
/// first loop and nothing from the second — which is what keeps the JavaScript branch from
/// drifting on a machine that never builds its component.
fn gate(reported: &[(Corpus, Arm, Vec<Violation>)], cold: &Crossings, hot: &Crossings) {
    for (corpus, arm, violations) in reported {
        let replayed = match corpus {
            Corpus::Cold => cold,
            Corpus::Hot => hot,
        };
        if let Some(detail) = disagreement(violations, replayed.reports(*arm)) {
            panic!(
                "the {} arm and its branch of the replay disagree about what {corpus:?} reports, \
                 so that arm's call count above describes a rule the engine did not run\n                   {detail}",
                arm.label(),
            );
        }
    }

    for (corpus, replayed) in [(Corpus::Cold, cold), (Corpus::Hot, hot)] {
        for arm in Arm::ALL {
            if let Some(detail) =
                disagreement(replayed.reports(Arm::TypeScript), replayed.reports(arm))
            {
                panic!(
                    "the {} branch of the replay disagrees with the TypeScript branch about what \
                     {corpus:?} reports, so they are not transcriptions of one rule\n  {detail}",
                    arm.label(),
                );
            }
        }
    }

    println!("  each arm agrees with its own branch of the replay, and the branches agree\n");
}

/// How two report sets differ, in one line, or `None` when they do not.
///
/// Hand-rolled rather than `assert_eq!`, which prints both vectors in full: the hot corpus
/// produces 3,840 violations, so a failure was 100 KB of output with the one differing entry
/// somewhere inside it. The counts and the first divergence are what a reader needs, and they
/// fit on a line.
fn disagreement(left: &[Violation], right: &[Violation]) -> Option<String> {
    if left == right {
        return None;
    }

    let first = left.iter().zip(right).find(|(l, r)| l != r).map_or_else(
        || "they agree as far as the shorter one goes".to_owned(),
        |(l, r)| format!("first difference: engine {l:?} against replay {r:?}"),
    );

    Some(format!(
        "{} against {} violations; {first}",
        left.len(),
        right.len()
    ))
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
    /// `javascript` is the JavaScript component's bytes when it has been built, and `None` when
    /// it has not — in which case that arm's rule file is never written and its config is never
    /// read, so an absent artifact costs a row in the report and nothing else.
    fn build(corpus: Corpus, javascript: Option<&[u8]>) -> Self {
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
        if let Some(bytes) = javascript {
            project.write("no-unwrap-js.wasm", bytes);
        }
        for arm in Arm::ALL {
            project.write(arm.config(), config_source(arm.rule()).as_bytes());
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

/// Where `just bench-js-component` writes the JavaScript component.
///
/// Under `target/`, which is gitignored, because the artifact is 13 MB and every crate here is
/// published — see this module's documentation. The repository's own `target/`, spelled from
/// this crate's manifest directory, because that is where the recipe writes it; a
/// `CARGO_TARGET_DIR` pointing somewhere else moves cargo's output and not this file.
fn javascript_component_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/no-unwrap-js.wasm")
}

/// The JavaScript component's bytes, or `None` when nobody has built it.
///
/// **Absence is the only failure treated as absence.** A path that is there but unreadable is a
/// broken build rather than a missing one, and reporting two arms in that case would hide it.
fn javascript_component_bytes() -> Option<Vec<u8>> {
    let path = javascript_component_path();
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("reads the JavaScript component at {}: {e}", path.display()),
    }
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
    /// Host calls per arm, indexed by [`Arm::index`].
    calls: [u64; Arm::ALL.len()],
    /// Time in [`replay`] alone, per arm. Parsing and [`subjects`] are outside it.
    arena: [Duration; Arm::ALL.len()],
    /// What each arm's branch of the replay would report, sorted.
    ///
    /// **Per arm, and it has to be.** An earlier version collected from the component branch
    /// only and held both engines to that one set, which establishes TypeScript engine ≡
    /// component engine ≡ component replay — and leaves the *TypeScript* branch of the replay,
    /// the branch producing the larger denominator, compared against nothing at all. The
    /// branches differ in more than counters: the Rust component's has an `else { continue }`
    /// on an absent position that the other two do not, which is inert on this corpus and is
    /// exactly the kind of thing an unchecked branch accumulates.
    reports: [Vec<Violation>; Arm::ALL.len()],
}

impl Crossings {
    const fn total(&self, arm: Arm) -> u64 {
        self.calls[arm.index()]
    }

    const fn arena_time(&self, arm: Arm) -> Duration {
        self.arena[arm.index()]
    }

    const fn reports(&self, arm: Arm) -> &Vec<Violation> {
        &self.reports[arm.index()]
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
        let mut calls = [0; Arm::ALL.len()];
        let mut arena = [Duration::MAX; Arm::ALL.len()];

        let sources: Vec<(String, String)> = (0..FILES)
            .map(|index| {
                let relative = format!("src/m{index:04}.rs");
                let source = std::fs::read_to_string(project.dir.join(&relative))
                    .expect("reads a corpus file");
                (relative, source)
            })
            .collect();

        for _ in 0..ATTEMPTS {
            for arm in Arm::ALL {
                let mut counted = 0;
                let mut elapsed = Duration::ZERO;

                for (relative, source) in &sources {
                    let (mut nodes, matches) = parsed(source);
                    let start = Instant::now();
                    counted += replay(arm, &mut nodes, relative, &matches, &mut None);
                    elapsed += start.elapsed();
                }

                calls[arm.index()] = counted;
                arena[arm.index()] = arena[arm.index()].min(elapsed);
            }
        }

        // A pass of its own, per arm, with the clock off. The timed passes above collect
        // nothing at all: pushing a position happens *inside* `replay`, so a collecting pass is
        // a slower pass, and folding it into the attempts either biases whichever arm collects
        // or forces every arm to pay a cost no engine has.
        let reports = Arm::ALL.map(|arm| Self::collect(arm, &sources));

        Self {
            calls,
            arena,
            reports,
        }
    }

    /// One arm's branch of the replay, run for what it reports rather than for what it costs.
    fn collect(arm: Arm, sources: &[(String, String)]) -> Vec<Violation> {
        let mut reports = Vec::new();
        for (relative, source) in sources {
            let (mut arena, matches) = parsed(source);
            let mut collected = Some(Vec::with_capacity(matches.len()));
            replay(arm, &mut arena, relative, &matches, &mut collected);
            for position in collected.unwrap_or_default() {
                reports.push((relative.clone(), position.0, position.1));
            }
        }
        reports.sort_unstable();
        reports
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
/// **Every counted call that has an arena lookup performs it**, including the ones one arm
/// repeats and the other hoists. Reusing a position already in hand would be the natural way to
/// write this and would break the timing the caller takes off it: the TypeScript arm's extra
/// `line(ancestor)` and `column(ancestor)` would then cost nothing, and the arena work would
/// come out identical for two arms that do measurably different amounts of it.
///
/// The exception is the component's `file-path`, counted below and modeled by nothing, because
/// the host answers it by cloning a `String` and never touches the arena
/// (`lanekeep_wasm::host`'s `HostCheckContext::file_path`). Its cost lands in `rest` rather than
/// in `arena`, which is where a string copy belongs — but it is a modeling choice rather than a
/// measurement, and it is one call per matched site against the ~13 + 2k that site costs.
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

        // `ctx.filePath`, and the first place the arms diverge: a property installed on the
        // context object under QuickJS, so no call at all, against `check-context.file-path`,
        // which is one — for either component, the Rust one because it is a method and the
        // JavaScript one because `host.js` fronts that method with a getter memoized for the
        // life of one `check`.
        if arm.calls_file_path() {
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

        // The second divergence, and the larger one. The Rust port reads the item's own line
        // and column *once*, here, with `line` and `column` being separate methods that each
        // resolve the node. The TypeScript original asks for both again on every sibling it
        // compares against, which is what the two branches in the loop below are — and the
        // JavaScript component runs that original, so it takes the unhoisted branch too.
        let hoisted = if arm.hoists_position() {
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
