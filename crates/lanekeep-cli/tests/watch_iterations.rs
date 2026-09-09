//! Twenty `--watch` iterations against one warm cache report exactly what a cold run reports.
//!
//! The property this pins is #191's: a `--watch` loop does not accumulate state across
//! iterations. An earlier version of this file drove twenty separate `lanekeep check --format
//! json` *processes* rather than a `--watch` loop — candid about the substitution in its own
//! header, and wrong to be: twenty separate processes cannot accumulate in-process state under
//! any implementation, so that version was green whether `watch_with` held a provider across
//! iterations or not. It could not fail for the property it was written to guard.
//!
//! **A second, later defect survived a first rewrite of this file**: the rewrite drove a real
//! `--watch` loop, but its inter-iteration edits rewrote `src/c.ts` to the identical bytes it
//! already held — a file the rule's own query never matches (`export const n = 1` has no
//! `(variable_declarator name: (identifier) value: (identifier))` shape) and one that carries
//! no relationship to the type oracle at all. That edit moves the mtime and wakes the loop, but
//! every iteration's *answer* is unaffected by it either way, so a provider held stale across
//! iterations — one that stopped re-reading `node_modules/@acme/rates/index.d.ts` — would have
//! produced the same wrong answer twenty times running, and this file would have stayed green
//! throughout. Twenty repetitions of a no-op prove only that the loop keeps running, not that
//! it keeps answering correctly.
//!
//! This version edits the thing a type-aware answer actually depends on: it alternates
//! `node_modules/@acme/rates/index.d.ts`'s declared type for `rate` between `number` and
//! `string` on every iteration, and compares each iteration's report against whichever of two
//! **cold** references — one per declared type, each a `lanekeep check` over a directory that
//! has never been checked — matches the state that iteration was taken in. A provider held
//! across iterations that failed to notice the declaration changed would answer the *previous*
//! iteration's type, which is exactly the other cold reference, so the mismatch is caught at
//! the first iteration whose answer lags rather than only at the last.
//!
//! `Watcher` and `Tree` are duplicated from `watch_dependency_paths.rs` rather than shared, for
//! the reason that file's own header gives: each file under `tests/` is its own crate, so
//! nothing under here can import another test file's items. The fixture — the rule, its
//! `@acme/rates` package, the two declared-type states — is `watch_dependency_paths.rs`'s own,
//! which already exists to prove the allowlist wakes the loop for exactly this file.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A rule that asks the oracle for the type of an imported value.
///
/// Byte-identical to `server_agreement.rs`'s and `watch_dependency_paths.rs`'s copy — see this
/// file's header for why it is duplicated rather than shared.
const RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-number-money',
  severity: 'error',
  requires: ['types'],
  card: {
    message: 'a money value must not be a bare number',
    remediation: 'give it the Money type the package exports',
    examples: { bad: 'const total = rate', good: 'const total: Money = rate' },
  },
  query: '(variable_declarator name: (identifier) @name value: (identifier) @value)',
  check(ctx, m) {
    const type = ctx.types.typeOf(m.value)
    if (type === undefined) return
    if (type.primitive !== 'number') return
    ctx.report(m.name)
  },
})
";

/// The fixture package the rule resolves through. Byte-identical to `server_agreement.rs`'s
/// copy, which is itself kept byte-identical to plan 4's resolver fixture.
const PACKAGE_JSON: &str = r#"{
  "name": "@acme/rates",
  "version": "1.0.0",
  "types": "index.d.ts"
}
"#;

/// The two declared-type states `node_modules/@acme/rates/index.d.ts` alternates between.
///
/// `NUMBER` makes the rule fire on both `src/a.ts` and `src/b.ts`; `STRING` makes it fire on
/// neither. The two states are far enough apart that a provider answering the wrong one is not
/// a subtle diff — it is a report with violations where the cold reference has none, or vice
/// versa.
const DECLARATION_NUMBER: &str = "export declare const rate: number\n";
const DECLARATION_STRING: &str = "export declare const rate: string\n";

/// A project on disk, removed on drop.
struct Tree {
    dir: PathBuf,
}

impl Tree {
    /// A fresh tree with `node_modules/@acme/rates/index.d.ts` declaring `rate` as
    /// `declaration` (one of the two constants above).
    fn new(name: &str, declaration: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-iterate-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let tree = Self { dir };
        tree.write("package.json", "{}\n");
        tree.write("lanekeep/rules/no-number-money.ts", RULE);
        tree.write("node_modules/@acme/rates/package.json", PACKAGE_JSON);
        tree.write("node_modules/@acme/rates/index.d.ts", declaration);
        tree.write(
            "src/a.ts",
            "import { rate } from '@acme/rates'\nexport const total = rate\n",
        );
        tree.write(
            "src/b.ts",
            "import { rate } from '@acme/rates'\nexport const fee = rate\n",
        );
        tree.write(
            "lanekeep.json",
            r#"{"include": ["src/**"],
 "timeouts": {"rule": 600000, "global": 600000},
 "rules": ["./lanekeep/rules/no-number-money.ts"]}
"#,
        );
        tree
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// A single cold `lanekeep check`, parsed — the answer every warm iteration taken at this
    /// tree's declared type is compared against.
    fn cold_report(&self) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .args(["--format", "json", "--timeout", "600000"])
            .output()
            .expect("runs the binary");
        assert_ne!(
            output.status.code(),
            Some(2),
            "the run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("a cold check reports valid JSON")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A running `lanekeep check --watch --format json`, one report per iteration.
///
/// Byte-identical in construction to `watch_dependency_paths.rs`'s copy — see that file's own
/// doc comment for why the streaming `serde_json::Deserializer` is what lets `next_report`
/// block on exactly the next iteration rather than racing an arbitrary read against the
/// child's own buffering.
struct Watcher {
    child: Child,
    reports: Receiver<serde_json::Value>,
}

impl Watcher {
    fn start(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(dir)
            .args(["--format", "json", "--timeout", "600000", "--watch"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawns `lanekeep check --watch`");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let stream =
                serde_json::Deserializer::from_reader(stdout).into_iter::<serde_json::Value>();
            for value in stream {
                let Ok(value) = value else { break };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self { child, reports: rx }
    }

    /// The next iteration's report, or a panic past `timeout` — which means the loop never
    /// woke for the edit this test made.
    fn next_report(&self, timeout: Duration) -> serde_json::Value {
        self.reports
            .recv_timeout(timeout)
            .expect("a `--watch` iteration reports before the timeout")
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Killed rather than asked to stop: `--watch` has no exit but Ctrl-C, and a foreground
        // loop the test started is a loop the test ends.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Under the harness's wall — `.config/nextest.toml` terminates a test after four 30-second
/// periods — so that a report that never comes fails naming the read where the wall has not
/// already fallen. A per-read deadline, and several reads share one test, so only an early
/// hang is named; a later one is still nextest's bare `TIMEOUT`. Far above what an iteration
/// needs.
const REPORT_TIMEOUT: Duration = Duration::from_mins(1);

#[test]
fn twenty_watch_iterations_are_byte_identical_to_a_cold_run() {
    // Two cold references, one per declared type — computed once, up front, each over a
    // directory that has never been checked and has no `.lanekeep/` in it.
    let expected_number = Tree::new("cold-number", DECLARATION_NUMBER).cold_report();
    let expected_string = Tree::new("cold-string", DECLARATION_STRING).cold_report();
    assert!(
        expected_number
            .to_string()
            .contains("local/no-number-money"),
        "the number declaration must report the type-aware rule: {expected_number}"
    );
    assert!(
        !expected_string
            .to_string()
            .contains("local/no-number-money"),
        "the string declaration must not report it: {expected_string}"
    );

    let warm = Tree::new("warm", DECLARATION_NUMBER);
    let watcher = Watcher::start(&warm.dir);

    // Iteration 1: the loop's own initial check, before any edit, taken at the `number` state
    // the warm tree started in.
    let first = watcher.next_report(REPORT_TIMEOUT);
    assert_eq!(
        first, expected_number,
        "iteration 1 (the loop's initial check) differs from a cold run at the same declared \
         type"
    );

    // Iterations 2 through 20: the declared type alternates every iteration, and every report
    // is compared against the cold reference for the type it was just written as — so a
    // provider held across iterations that failed to re-read the declaration would answer the
    // *previous* iteration's type, which is exactly the other cold reference, and the mismatch
    // is caught at whichever iteration first lags rather than only at the last.
    for iteration in 2..=20 {
        let declaration = if iteration % 2 == 0 {
            DECLARATION_STRING
        } else {
            DECLARATION_NUMBER
        };
        let expected = if iteration % 2 == 0 {
            &expected_string
        } else {
            &expected_number
        };
        warm.write("node_modules/@acme/rates/index.d.ts", declaration);
        let got = watcher.next_report(REPORT_TIMEOUT);
        assert_eq!(
            &got,
            expected,
            "iteration {iteration} (declared `{}`) differs from a cold run at the same \
             declared type",
            if iteration % 2 == 0 {
                "string"
            } else {
                "number"
            }
        );
    }

    assert!(
        warm.dir.join(".lanekeep").exists(),
        "the iterations were meant to be warm; nothing wrote a cache"
    );
}
