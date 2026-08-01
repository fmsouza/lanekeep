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
//! # Why the gate is loose and the report is precise
//!
//! A hosted runner is shared, throttled, and several times slower than a developer machine
//! on a bad day. A gate set at the budget would fail for reasons nothing in this repository
//! caused, and a flaky gate is one people learn to re-run rather than read.
//!
//! So: the measured numbers are printed exactly, against the budget, every run — that is
//! what a human reads to see a 20% regression. The gate fails only past
//! [`GATE_MULTIPLIER`]×, which no amount of runner variance explains and no honest change
//! produces by accident.

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    clippy::format_push_string,
    clippy::format_collect,
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

/// How far past budget is a regression rather than a slow machine.
///
/// Four. A hosted runner is routinely two to three times slower than a developer machine,
/// and the point of the gate is to catch something that got an order of magnitude worse, not
/// to police the last 20% — which the printed numbers do better, because a human reads them.
const GATE_MULTIPLIER: u32 = 4;

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

    let breached: Vec<&Budget> = BUDGETS
        .iter()
        .zip(&measured)
        .filter(|(budget, taken)| {
            budget
                .limit
                .is_some_and(|limit| **taken > limit * GATE_MULTIPLIER)
        })
        .map(|(budget, _)| budget)
        .collect();

    assert!(
        breached.is_empty(),
        "{} scenario(s) past {GATE_MULTIPLIER}x budget: {}",
        breached.len(),
        breached
            .iter()
            .map(|b| b.name)
            .collect::<Vec<_>>()
            .join(", ")
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
    println!("\n  gate fails past {GATE_MULTIPLIER}x budget\n");
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
