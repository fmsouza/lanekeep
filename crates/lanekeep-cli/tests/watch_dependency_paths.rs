//! `--watch` wakes on an edit to a declaration file the type oracle read, under both
//! providers.
//!
//! This is the pin for task 6.6 fix round 1's item 2 — "no test exercises the wiring from a
//! real run" — and, gated on `tsc_available`, item 1's own pin: `TscProvider::dependency_paths`
//! is what makes the `tsc` half of this true at all. `watch.rs`'s own eight unit tests call
//! `is_interesting` with hand-built allowlists and never run a real check, so a wiring defect
//! at the call site that fills the allowlist — `main.rs`'s `if let Some(sink) = dependencies`
//! — leaves every one of them green. Only a real `lanekeep check --watch`, over a real edit,
//! exercises that call site.
//!
//! The fixture is `server_agreement.rs`'s: a rule typing an imported value, and a `.d.ts`
//! under `node_modules` — outside anything discovery ever selects — that only the oracle
//! reads. `--watch`'s allowlist exists for exactly this file. Duplicated rather than shared,
//! for the reason every file under `tests/` gives: each is its own crate.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below, and the `corpus` helpers this file \
              also compiles, are neither."
)]
#![expect(
    clippy::print_stderr,
    reason = "the `tsc` case has to say why it did nothing when `typescript` is absent: the \
              alternative is a suite reporting a pass for a test it did not run"
)]

mod corpus;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use corpus::tsc_available;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// This repository's own authoring `typescript`, absolute and forward-slashed.
///
/// Spelled with forward slashes for the reason `server_agreement.rs`'s copy gives:
/// `validate_specifier` and JSON escaping both have history with backslashes on Windows, and
/// Rust accepts either separator there.
fn typescript_package() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript")
        .canonicalize()
        .expect("the authoring package is installed")
        .to_string_lossy()
        .replace('\\', "/")
}

/// A rule that asks the oracle for the type of an imported value.
///
/// Byte-identical to `server_agreement.rs`'s copy — see this file's header for why it is
/// duplicated rather than shared.
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

const PACKAGE_JSON: &str = r#"{
  "name": "@acme/rates",
  "version": "1.0.0",
  "types": "index.d.ts"
}
"#;

const DECLARATION_NUMBER: &str = "export declare const rate: number\n";
const DECLARATION_STRING: &str = "export declare const rate: string\n";

/// A project on disk, removed on drop.
struct Tree {
    dir: PathBuf,
}

impl Tree {
    fn new(name: &str, provider: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-watch-dep-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let tree = Self { dir };
        tree.write("package.json", "{}\n");
        tree.write("lanekeep/rules/no-number-money.ts", RULE);
        tree.write("node_modules/@acme/rates/package.json", PACKAGE_JSON);
        tree.write("node_modules/@acme/rates/index.d.ts", DECLARATION_NUMBER);
        tree.write(
            "src/a.ts",
            "import { rate } from '@acme/rates'\nexport const total = rate\n",
        );

        let types_block = if provider == "tsc" {
            format!(
                r#"{{"provider": "tsc", "typescript": "{}"}}"#,
                typescript_package()
            )
        } else {
            format!(r#"{{"provider": "{provider}"}}"#)
        };
        tree.write(
            "lanekeep.json",
            &format!(
                r#"{{"include": ["src/**"],
 "timeouts": {{"rule": 600000, "global": 600000}},
 "types": {types_block},
 "rules": ["./lanekeep/rules/no-number-money.ts"]}}
"#
            ),
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
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A running `lanekeep check --watch --format json`, one report per iteration.
///
/// Iterations are told apart with `serde_json::Deserializer`'s streaming form rather than by
/// reading lines: each iteration's document is one flushed `write_all`
/// (`crates/lanekeep-cli/src/main.rs`), and the streaming deserializer blocks for exactly one
/// complete JSON value at a time regardless of what does or does not separate them on the
/// wire — which is what lets `next_report` block until the *next* iteration specifically,
/// rather than racing an arbitrary read against the child's own buffering.
struct Watcher {
    child: Child,
    reports: Receiver<serde_json::Value>,
}

impl Watcher {
    /// Starts `lanekeep check --watch` over `dir`, stderr inherited — the `tsc` iteration
    /// prints a note there, and a piped stderr nobody drains is a deadlock waiting for one
    /// long enough to fill the pipe (`server_agreement.rs`'s `Session::start` gives the same
    /// reasoning for its own server process).
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

/// Whether a report names the rule this fixture's declaration flips.
fn reports_the_bare_number(report: &serde_json::Value) -> bool {
    report["violations"]
        .as_array()
        .expect("a violations array")
        .iter()
        .any(|v| v["rule_id"] == "local/no-number-money")
}

/// Under the harness's wall — `.config/nextest.toml` terminates a test after four 30-second
/// periods — so that a report that never comes fails naming the read where the wall has not
/// already fallen. A per-read deadline, and several reads share one test, so only an early
/// hang is named; a later one is still nextest's bare `TIMEOUT`. Far above what an iteration
/// needs.
const REPORT_TIMEOUT: Duration = Duration::from_mins(1);

#[test]
fn watch_wakes_for_a_builtin_declaration_under_node_modules() {
    let tree = Tree::new("builtin", "builtin");
    let watcher = Watcher::start(&tree.dir);

    let first = watcher.next_report(REPORT_TIMEOUT);
    assert!(
        reports_the_bare_number(&first),
        "the initial declaration types `rate` as a bare number: {first}"
    );

    // Outside anything discovery selects — the whole reason the allowlist exists. If
    // `main.rs`'s wiring from `Engine::dependency_paths` into the loop's allowlist regresses,
    // this write produces no second report and the test times out rather than failing fast,
    // which is exactly the silent shape the brief calls out.
    tree.write("node_modules/@acme/rates/index.d.ts", DECLARATION_STRING);

    let second = watcher.next_report(REPORT_TIMEOUT);
    assert!(
        !reports_the_bare_number(&second),
        "the edited declaration types `rate` as a string, so the loop must have woken and \
         rechecked to see this: {second}"
    );
}

#[test]
fn watch_wakes_for_a_tsc_declaration_under_node_modules() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }

    let tree = Tree::new("tsc", "tsc");
    let watcher = Watcher::start(&tree.dir);

    let first = watcher.next_report(REPORT_TIMEOUT);
    assert!(
        reports_the_bare_number(&first),
        "the initial declaration types `rate` as a bare number: {first}"
    );

    // Under `tsc` the compiler reads this file through its own host, never through
    // `Query::files` — the one path `TscProvider::dependency_paths` exists to cover. Without
    // it in the union, this edit produces no second report at all.
    tree.write("node_modules/@acme/rates/index.d.ts", DECLARATION_STRING);

    let second = watcher.next_report(REPORT_TIMEOUT);
    assert!(
        !reports_the_bare_number(&second),
        "the edited declaration types `rate` as a string, so the loop must have woken and \
         rechecked to see this: {second}"
    );
}
