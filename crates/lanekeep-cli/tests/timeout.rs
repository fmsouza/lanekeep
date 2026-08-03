//! `--timeout` sets the global run budget, in both directions.
//!
//! It set neither for the whole life of the flag. `check` destructured `timeout`, validated
//! that it was non-zero — with a comment explaining that accepting a value and ignoring it
//! would "surface much later as an unexplained breach of a budget they thought they had
//! changed" — and then never passed it to the engine.
//!
//! What kept it quiet is that the failure has no symptom of its own. Lowering the budget
//! looks like it worked, because the run completes either way. Raising it looks like the run
//! is simply slow. And the message the breach prints ends with "raise it with `--timeout`" —
//! advice that could not work, printed by the code the advice was about.
//!
//! So both directions are asserted here, and the *raise* is the one that matters: a test
//! that only lowered the budget would have passed against the broken build.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A rule that burns bytecode rather than sleeping, because the sandbox has no clock.
///
/// It also has to burn a *lot* of it: the global budget is polled by QuickJS's interrupt
/// handler, which only runs while JavaScript does, so a handler that returns after a
/// handful of operations can overrun the budget without ever being asked to stop. A rule
/// doing real work is what makes this fixture deterministic instead of a race.
///
/// The iteration count is chosen for margin on both sides: it runs about two seconds in a
/// debug build, so the 200 ms budget below is breached by roughly ten times over, and the
/// 60 s budget has thirty times the headroom it needs. A machine has to be an order of
/// magnitude off nominal in one direction or the other before either assertion is close.
const SLOW_RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/slow',
  severity: 'error',
  card: { message: 'slow', remediation: 'n/a', examples: { bad: 'a', good: 'b' } },
  query: '(debugger_statement) @stmt',
  check(ctx, m) {
    let n = 0
    for (let i = 0; i < 6000000; i++) { n += i % 7 }
    if (n < 0) ctx.report(m.stmt)
  },
})
";

struct Project {
    dir: PathBuf,
}

impl Project {
    /// A scaffolded project whose one rule takes appreciably longer than a moment.
    ///
    /// Built by `init` rather than by hand so the fixture cannot drift from what the tool
    /// actually writes; only the rule and the timeouts are replaced.
    ///
    /// The per-rule budget is set far above anything this rule needs, so the only limit
    /// that can bind is the global one. Without that, both directions of the test would
    /// exit 2 and the assertion would be reading a per-rule timeout as evidence about a
    /// flag that never touched it.
    fn new(name: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-timeout-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("creates dir");
        std::fs::write(dir.join("package.json"), "{}\n").expect("writes the marker");

        let project = Self { dir };
        project.run(&["init"]);
        std::fs::write(project.dir.join("lanekeep/rules/slow.ts"), SLOW_RULE)
            .expect("writes the rule");
        std::fs::write(
            project.dir.join("src/a.ts"),
            "export function a() {\n  debugger;\n}\n",
        )
        .expect("writes a file the rule matches");
        project.rewrite_config(|config| {
            config["rules"] = serde_json::json!(["./lanekeep/rules/slow.ts"]);
            config["timeouts"] = serde_json::json!({ "rule": 300_000 });
        });
        project
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    fn rewrite_config(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let path = self.dir.join("lanekeep.json");
        let text = std::fs::read_to_string(&path).expect("reads the config");
        let mut config: serde_json::Value = serde_json::from_str(&text).expect("parses");
        edit(&mut config);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&config).expect("serializes"),
        )
        .expect("writes the config");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Whether this run died on the *global* budget specifically.
///
/// Matched on the wording the breach prints rather than on the exit code alone, because a
/// per-rule timeout, a broken config and a missing rule all exit 2 as well.
fn exceeded_the_run_budget(output: &Output) -> bool {
    output.status.code() == Some(2)
        && String::from_utf8_lossy(&output.stderr).contains("the run exceeded its")
}

#[test]
fn the_flag_lowers_the_budget() {
    // `--no-cache` throughout: a warm run does no work, and a budget bounds work.
    let project = Project::new("lower");
    let output = project.run(&["check", "--no-cache", "--timeout", "200"]);

    assert!(
        exceeded_the_run_budget(&output),
        "a 200 ms budget should cancel this run: {}",
        describe(&output)
    );
}

#[test]
fn the_flag_raises_a_budget_the_config_set() {
    // The direction that was broken, and the reason the flag exists. The config says a
    // fifth of a second, the flag says a minute, and the flag is the more specific
    // statement — it was typed for this run.
    let project = Project::new("raise");
    project.rewrite_config(|config| {
        config["timeouts"] = serde_json::json!({ "rule": 300_000, "global": 200 });
    });

    let without = project.run(&["check", "--no-cache"]);
    assert!(
        exceeded_the_run_budget(&without),
        "the config's own 200 ms budget should cancel the run: {}",
        describe(&without)
    );

    let with = project.run(&["check", "--no-cache", "--timeout", "60000"]);
    assert!(
        !exceeded_the_run_budget(&with),
        "--timeout 60000 should override the config's 200 ms: {}",
        describe(&with)
    );
    // And it ran to the end, rather than stopping quietly somewhere short of it. The rule
    // reports nothing, so a completed run is clean.
    assert_eq!(with.status.code(), Some(0), "{}", describe(&with));
}

#[test]
fn zero_is_refused_rather_than_ignored() {
    // Rejected because a budget of zero cancels every run before it starts, which is
    // indistinguishable from the tool being broken. This was the one part of the flag that
    // already worked, and it is what made the rest look implemented.
    let project = Project::new("zero");
    let output = project.run(&["check", "--timeout", "0"]);

    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("greater than zero"),
        "{}",
        describe(&output)
    );
}
