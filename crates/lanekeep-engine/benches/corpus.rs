//! The performance gate: a fixed corpus, the three scenarios from architecture §15.
//!
//! # Why this is not criterion
//!
//! Criterion measures *relative* change against a saved baseline, which needs the baseline
//! to persist between CI runs and needs the two runs to happen on comparable hardware.
//! Neither holds for a hosted runner. What §15 actually specifies is absolute budgets, and
//! an absolute budget needs no baseline at all.
//!
//! It is also zero dependencies, which for a gate that exists to catch a regression in this
//! repository is worth more than distribution plots.
//!
//! # The report is absolute, the gate is relative
//!
//! §15's budgets are absolute numbers, and absolute numbers cannot be gated on a hosted
//! runner. This suite's first CI run measured a cold pass at 10.9 s against 1.5 s on a
//! developer machine — seven times slower, on hardware that varies run to run. Any absolute
//! threshold loose enough not to flake there is too loose to catch anything.
//!
//! So the two jobs are separated:
//!
//! **The report** prints every scenario against its §15 budget, exactly, every run. That is
//! what a human reads to see a 20% regression, and it is why the budgets stay in the table
//! even though none is met yet.
//!
//! **The gate** asserts only machine-independent *ratios* — properties that hold on any
//! hardware because both sides move together. They are chosen to be the claims the design
//! actually rests on, so a change that breaks one has broken something real:
//!
//! - A warm run must cost a small fraction of a cold one. This is the cache's entire
//!   purpose, and it is what caught the sandbox being built per worker on a warm run.
//! - Checking one selected file must not cost more than checking everything warm. If it
//!   does, `--staged` is not an optimization.
//!
//! Plus one absolute ceiling so deep enough breakage — an accidental infinite loop, a
//! quadratic blowup — fails instead of hanging the job.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    clippy::format_push_string,
    clippy::format_collect,
    clippy::panic,
    reason = "A benchmark is test scaffolding that `clippy.toml`'s allow-*-in-tests does \
              not reach, and its whole output is a printed report. The string building \
              here runs once at setup, not in anything measured — optimizing it would \
              obscure what the corpus looks like for no gain."
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lanekeep_core::FilePath;
use lanekeep_engine::Engine;
use lanekeep_js::RuleRoot;
use lanekeep_lang_js::{JavaScript, TypeScript};

/// Files in the synthetic corpus. §15 specifies "~2k files".
const FILES: usize = 2_000;

/// Rules in the synthetic ruleset. §15 specifies "~20 rules".
const RULES: usize = 20;

/// The most a warm run may cost, as a fraction of a cold one.
///
/// The cache's whole purpose. Machine-independent because both numbers move with the
/// hardware — measured at ~4% on a developer machine and ~1% on a hosted runner, so 25% is
/// loose enough never to flake and tight enough that losing an order of magnitude fires it.
/// This is the check that would have caught a warm run starting a QuickJS engine per worker.
const WARM_FRACTION_OF_COLD: f64 = 0.25;

/// The most a one-file selected run may cost, as a fraction of a warm full run.
///
/// Deliberately generous. Measured at 0.57 on a developer machine but 0.89 on a hosted
/// runner, because on slow hardware both are dominated by loading and rewriting the whole
/// cache file and the selection's advantage shrinks toward nothing. A limit of 1.0 would
/// have 11% of margin on CI, which is a flake waiting to happen.
///
/// At 1.5 it still catches what it is for: a selection that re-checks everything, or one
/// that costs materially more than no selection at all. Tightening it needs a cache format
/// that can be written in part, not a smaller number here.
const SELECTED_FRACTION_OF_WARM: f64 = 1.5;

/// An absolute ceiling on the cold run, well past any plausible hardware.
///
/// Not a budget — §15's budget is 800 ms and this is 75 times that. It exists so an
/// accidental infinite loop or a quadratic blowup fails the job instead of running until
/// the runner's own timeout, with no indication of what went wrong.
const COLD_CEILING: Duration = Duration::from_secs(60);

/// One scenario's budget, from architecture §15.
struct Budget {
    name: &'static str,
    /// `None` for a scenario measured but not gated.
    limit: Option<Duration>,
}

const BUDGETS: &[Budget] = &[
    Budget {
        name: "cold full run",
        limit: Some(Duration::from_millis(800)),
    },
    Budget {
        name: "warm, no changes",
        limit: Some(Duration::from_millis(25)),
    },
    Budget {
        // No budget, and deliberately so. With full discovery, finding which one file
        // changed means reading and hashing all of them — so this is the row above plus one
        // file's work, and it can never beat it. Reported because it is what `lanekeep
        // check` with no flags actually costs.
        name: "warm, 1 changed (all)",
        limit: None,
    },
    Budget {
        // The §15 budget belongs here: "one changed file" is the pre-commit workflow, and
        // the pre-commit workflow is `--staged`. Reaching 10ms with full discovery would
        // need the walker to decide a file is unchanged without reading it.
        name: "warm, 1 changed (--staged)",
        limit: Some(Duration::from_millis(10)),
    },
];

fn main() {
    let corpus = Corpus::build();

    // Once through cold, discarded: the first run pays for the OS page cache and for
    // whatever the allocator does when it first sees this shape of work. Measuring it would
    // be measuring the machine's disk, not this engine.
    let _ = corpus.engine().run().expect("warm-up run");
    let _ = std::fs::remove_dir_all(corpus.dir.join(".lanekeep"));

    let cold = measure(|| {
        let _ = std::fs::remove_dir_all(corpus.dir.join(".lanekeep"));
        let engine = corpus.engine();
        engine.run().expect("cold run");
    });

    // Populate the cache, then measure hitting it.
    let _ = corpus.engine().run().expect("populating run");
    let warm = measure(|| {
        let engine = corpus.engine();
        engine.run().expect("warm run");
    });

    let warm_one_all = measure(|| {
        corpus.touch(0);
        let engine = corpus.engine();
        engine.run().expect("warm run with one change");
    });

    // What a pre-commit hook does: hand the engine the file that changed instead of asking
    // it to find out. This is the scenario §15's 10ms budget describes.
    let one_file = [FilePath::new("src/m0000.ts")];
    let warm_one_selected = measure(|| {
        corpus.touch(0);
        let engine = corpus.engine();
        engine.run_over(&one_file).expect("warm selected run");
    });

    let measured = [cold, warm, warm_one_all, warm_one_selected];
    report(&measured);

    gate(&measured);
}

/// The machine-independent checks.
///
/// Ratios rather than absolute times, so the same thresholds hold on a laptop and on a
/// throttled two-core runner. See the module documentation for why absolute budgets are
/// reported but not gated.
fn gate(measured: &[Duration]) {
    let [cold, warm, _, selected] = measured else {
        panic!(
            "expected one measurement per scenario, got {}",
            measured.len()
        );
    };

    let warm_ratio = warm.as_secs_f64() / cold.as_secs_f64();
    let selected_ratio = selected.as_secs_f64() / warm.as_secs_f64();

    println!("  warm / cold          {:>6.2}%", warm_ratio * 100.0);
    println!("  selected / warm      {selected_ratio:>7.2}x\n");

    assert!(
        *cold < COLD_CEILING,
        "a cold run took {cold:.1?}, past the {COLD_CEILING:.0?} ceiling — this is not slow \
         hardware, something is looping or quadratic"
    );

    assert!(
        warm_ratio <= WARM_FRACTION_OF_COLD,
        "a warm run cost {:.1}% of a cold one, over the {:.0}% the cache exists to deliver \
         (cold {cold:.1?}, warm {warm:.1?})",
        warm_ratio * 100.0,
        WARM_FRACTION_OF_COLD * 100.0,
    );

    assert!(
        selected_ratio <= SELECTED_FRACTION_OF_WARM,
        "checking one selected file cost {selected:.1?}, more than checking everything warm \
         at {warm:.1?} — a file selection that costs more than no selection is not one"
    );
}

/// Print the table a human reads, whether or not the gate fires.
fn report(measured: &[Duration]) {
    println!("\nlanekeep performance — {FILES} files, {RULES} rules\n");
    println!(
        "  {:<28} {:>10} {:>10} {:>8}",
        "scenario", "measured", "budget", "ratio"
    );

    for (budget, taken) in BUDGETS.iter().zip(measured) {
        // Against the budget, not against a stored baseline. This is the number that shows a
        // 20% regression to someone reading the log, which the gate deliberately does not.
        match budget.limit {
            Some(limit) => println!(
                "  {:<28} {:>9.1?} {:>9.1?} {:>7.2}x",
                budget.name,
                taken,
                limit,
                taken.as_secs_f64() / limit.as_secs_f64()
            ),
            None => println!(
                "  {:<28} {:>9.1?} {:>9} {:>8}",
                budget.name, taken, "—", "—"
            ),
        }
    }
    println!("\n  budgets are reported, not gated — the gate is on ratios below\n");
}

/// The best of several attempts.
///
/// Best rather than mean: this measures a lower bound on how long the work takes, and every
/// source of noise on a shared machine — scheduling, another job, thermal throttling — only
/// ever adds. An average blends the engine's cost with the runner's mood.
fn measure(mut run: impl FnMut()) -> Duration {
    const ATTEMPTS: usize = 5;

    let mut best = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let start = Instant::now();
        run();
        best = best.min(start.elapsed());
    }
    best
}

/// The synthetic project everything is measured against.
struct Corpus {
    dir: PathBuf,
}

impl Corpus {
    fn build() -> Self {
        let dir =
            std::env::temp_dir().join(format!("lanekeep-bench-corpus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let corpus = Self { dir };
        for index in 0..RULES {
            corpus.write(&format!("rules/r{index:02}.ts"), &rule_source(index));
        }
        corpus.write("lanekeep.config.ts", &config_source());
        for index in 0..FILES {
            corpus.write(&format!("src/m{index:04}.ts"), &file_source(index));
        }
        corpus
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// Change one file's bytes, for the one-changed-file scenario.
    fn touch(&self, index: usize) {
        // A comment carrying the nanosecond the write happened. It has to differ from the
        // last one or the content hash is unchanged and the file is a cache hit — which
        // would measure the no-changes scenario a second time.
        let stamp = Instant::now().elapsed().as_nanos();
        self.write(
            &format!("src/m{index:04}.ts"),
            &format!(
                "// touched {stamp}{}\n{}",
                std::process::id(),
                file_source(index)
            ),
        );
    }

    fn engine(&self) -> Engine {
        let root = RuleRoot::new(&self.dir).expect("canonicalizes");
        let config_path = self.dir.join("lanekeep.config.ts");
        let sandbox =
            lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                .expect("sandbox builds");
        let config = lanekeep_config::load(&sandbox, &root, &config_path).expect("config loads");

        Engine::prepare(
            &config,
            &self.dir,
            root,
            &config_path,
            &lanekeep_lang_js::registry(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .expect("engine prepares")
    }
}

impl Drop for Corpus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One rule, varied so the ruleset is not twenty copies of one query.
///
/// Every fourth rule carries a `fileContains` gate, which is roughly the mix a real config
/// has — most rules are narrow, some are not. A suite where every rule was gated would
/// measure the gates and flatter the engine.
fn rule_source(index: usize) -> String {
    let queries = [
        "(debugger_statement) @stmt",
        "(export_statement \"default\") @stmt",
        "(import_statement source: (string) @source) @stmt",
        "(call_expression function: (identifier) @fn) @stmt",
        "(class_declaration name: (type_identifier) @name) @stmt",
    ];
    let query = queries[index % queries.len()];
    let gate = if index.is_multiple_of(4) {
        "  gates: { fileContains: ['export'] },\n"
    } else {
        ""
    };

    format!(
        "import {{ defineRule }} from 'lanekeep';\n\
         export default defineRule({{\n\
         \x20 id: 'local/bench-{index:02}',\n\
         \x20 severity: 'warn',\n\
         \x20 card: {{\n\
         \x20   message: 'bench rule {index}',\n\
         \x20   remediation: 'nothing to do, this is a benchmark',\n\
         \x20   examples: {{ bad: 'debugger;', good: 'const a = 1;' }},\n\
         \x20 }},\n\
         {gate}\
         \x20 query: '{query}',\n\
         \x20 check(ctx, m) {{\n\
         \x20   if (m.stmt && ctx.kind(m.stmt) === 'debugger_statement') ctx.report(m.stmt);\n\
         \x20 }},\n\
         }});\n"
    )
}

fn config_source() -> String {
    let imports: String = (0..RULES)
        .map(|i| format!("import r{i:02} from './rules/r{i:02}';\n"))
        .collect();
    let names: Vec<String> = (0..RULES).map(|i| format!("r{i:02}")).collect();

    format!(
        "import {{ defineConfig }} from 'lanekeep';\n\
         {imports}\
         export default defineConfig({{\n\
         \x20 include: ['src/**/*.ts'],\n\
         \x20 rules: [{}],\n\
         }});\n",
        names.join(", ")
    )
}

/// One source file, sized and shaped like something a person would write.
///
/// Every twentieth file has a `debugger`, so the handler path is exercised rather than only
/// the query path — a corpus with no matches would measure the gates and nothing else.
fn file_source(index: usize) -> String {
    let mut out = String::with_capacity(1_400);
    out.push_str(&format!(
        "import {{ helper }} from './m{:04}';\n\
         import type {{ Thing }} from './types';\n\n",
        (index + 1) % FILES
    ));

    for n in 0..8 {
        out.push_str(&format!(
            "export function fn{index}_{n}(input: string): string {{\n\
             \x20 const parts = input.split('/');\n\
             \x20 if (parts.length > {n}) {{\n\
             \x20   return parts.map((p) => p.trim()).join('-');\n\
             \x20 }}\n\
             \x20 return helper(input);\n\
             }}\n\n"
        ));
    }

    out.push_str(&format!("export class Service{index} {{\n"));
    for n in 0..4 {
        out.push_str(&format!(
            "  method{n}(value: number): number {{ return value * {n}; }}\n"
        ));
    }
    out.push_str("}\n");

    if index.is_multiple_of(20) {
        out.push_str("\nexport function debugMe() {\n  debugger;\n}\n");
    }

    out
}
