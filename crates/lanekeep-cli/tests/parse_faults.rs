//! `lanekeep/parse`: a file lanekeep's parser could not read whole is reported, not passed
//! over (#271).

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// The directive tokens, assembled rather than written: lanekeep checks this file, and a token
/// spelled out here would be a live directive in it.
const NEXT_LINE: &str = concat!("lanekeep", "-ignore-next-line");
const WHOLE_FILE: &str = concat!("lanekeep", "-ignore-file");

/// The Vitest `importOriginal` idiom tree-sitter-typescript 0.23.2 misreads, with a statement
/// after it, so the root is `ERROR`.
const REPRO: &str = "hoist('a', async importOriginal => {\n    const actual =\n        \
                     await importOriginal<typeof import('vitest')>()\n})\n\n1\n";

const ANCHOR_RULE: &str = "import { defineRule } from 'lanekeep'\n\
    export default defineRule({\n\
      id: 'local/anchor',\n\
      language: ['typescript', 'tsx'],\n\
      card: { message: 'program root reached', remediation: 'n/a', \
              examples: { bad: 'a', good: 'b' } },\n\
      query: '(program) @file',\n\
      check(ctx, m) { ctx.report(m.file) },\n\
    })\n";

const PYTHON_ANCHOR_RULE: &str = "import { defineRule } from 'lanekeep'\n\
    export default defineRule({\n\
      id: 'local/py-anchor',\n\
      language: ['python'],\n\
      card: { message: 'module root reached', remediation: 'n/a', \
              examples: { bad: 'a', good: 'b' } },\n\
      query: '(module) @file',\n\
      check(ctx, m) { ctx.report(m.file) },\n\
    })\n";

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-parse-faults-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");
        let project = Self { dir };
        for (path, contents) in files {
            project.write(path, contents);
        }
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// Run `lanekeep` with the project directory appended.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    /// `check --format json`, parsed, plus the exit code.
    fn check_json(&self, extra: &[&str]) -> (i32, serde_json::Value) {
        let mut args = vec!["check", "--format", "json"];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        let code = output.status.code().expect("exited");
        let json = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (code, json)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The violations under `rule`, from a `--format json` document.
fn violations_of<'a>(json: &'a serde_json::Value, rule: &str) -> Vec<&'a serde_json::Value> {
    json["violations"]
        .as_array()
        .expect("a violations array")
        .iter()
        .filter(|v| v["rule_id"] == rule)
        .collect()
}

fn json_config(severity: &str) -> String {
    format!(
        r#"{{"include": ["src/**/*.ts"], "rules": ["./rules/anchor.ts"], "severity": {severity}}}"#
    )
}

fn ts_config(severity: &str) -> String {
    format!(
        "import {{ defineConfig }} from 'lanekeep'\n\
         import anchor from './rules/anchor'\n\
         export default defineConfig({{ include: ['src/**/*.ts'], rules: [anchor], \
         severity: {severity} }})\n"
    )
}

fn assert_warns_and_passes(project: &Project) {
    let (code, json) = project.check_json(&["--no-cache"]);
    assert_eq!(code, 0, "a warning does not fail the run: {json}");
    let faults = violations_of(&json, "lanekeep/parse");
    assert_eq!(faults.len(), 1, "{json}");
    assert_eq!(faults[0]["severity"], "warn");
    assert_eq!(faults[0]["location"]["position"]["line"], 1);
    assert_eq!(faults[0]["location"]["position"]["column"], 1);
    assert!(
        faults[0]["message"]
            .as_str()
            .expect("a message")
            .starts_with("the typescript parser could not read this file as a whole"),
        "{json}"
    );
    assert!(
        violations_of(&json, "local/anchor").is_empty(),
        "the anchor cannot match an `ERROR` root: {json}"
    );
}

/// Paired with `…_ts`: the two config formats reach the engine by different routes.
#[test]
fn a_faulted_file_warns_and_the_run_passes_json() {
    let project = Project::new(
        "warn-json",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            ("lanekeep.json", &json_config("{}")),
            ("src/repro.ts", REPRO),
        ],
    );
    assert_warns_and_passes(&project);
}

#[test]
fn a_faulted_file_warns_and_the_run_passes_ts() {
    let project = Project::new(
        "warn-ts",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            ("lanekeep.config.ts", &ts_config("{}")),
            ("src/repro.ts", REPRO),
        ],
    );
    assert_warns_and_passes(&project);
}

#[test]
fn a_python_file_names_its_grammar() {
    let project = Project::new(
        "python",
        &[
            ("rules/py-anchor.ts", PYTHON_ANCHOR_RULE),
            (
                "lanekeep.json",
                r#"{"include": ["src/**/*.py"], "rules": ["./rules/py-anchor.ts"]}"#,
            ),
            ("src/a.py", "def f(:\n    pass\n"),
        ],
    );
    let (_, json) = project.check_json(&["--no-cache"]);
    let faults = violations_of(&json, "lanekeep/parse");
    assert_eq!(faults.len(), 1, "{json}");
    assert!(
        faults[0]["message"]
            .as_str()
            .expect("a message")
            .starts_with("the python parser "),
        "{json}"
    );
}

fn fails_on_error(project: &Project) {
    let (code, json) = project.check_json(&["--no-cache"]);
    assert_eq!(code, 1, "an error-severity fault fails the run: {json}");
    let faults = violations_of(&json, "lanekeep/parse");
    assert_eq!(faults.len(), 1, "{json}");
    assert_eq!(faults[0]["severity"], "error");
}

fn silent_when_off(project: &Project) {
    let (code, json) = project.check_json(&["--no-cache"]);
    assert_eq!(code, 0, "{json}");
    assert!(violations_of(&json, "lanekeep/parse").is_empty(), "{json}");
}

#[test]
fn an_error_parse_severity_fails_the_run_json() {
    fails_on_error(&Project::new(
        "error-json",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.json",
                &json_config(r#"{"lanekeep/parse": "error"}"#),
            ),
            ("src/repro.ts", REPRO),
        ],
    ));
}

#[test]
fn an_error_parse_severity_fails_the_run_ts() {
    fails_on_error(&Project::new(
        "error-ts",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.config.ts",
                &ts_config("{ 'lanekeep/parse': 'error' }"),
            ),
            ("src/repro.ts", REPRO),
        ],
    ));
}

#[test]
fn an_off_parse_severity_reports_nothing_json() {
    silent_when_off(&Project::new(
        "off-json",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.json",
                &json_config(r#"{"lanekeep/parse": "off"}"#),
            ),
            ("src/repro.ts", REPRO),
        ],
    ));
}

#[test]
fn an_off_parse_severity_reports_nothing_ts() {
    silent_when_off(&Project::new(
        "off-ts",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.config.ts",
                &ts_config("{ 'lanekeep/parse': 'off' }"),
            ),
            ("src/repro.ts", REPRO),
        ],
    ));
}

#[test]
fn the_root_case_is_acknowledged_line_by_line_under_forbid_file_scope() {
    // Under `forbidFileScope` every whole-file directive is itself an error, so the next-line
    // form is the only acknowledgement. It has to land on the line the report names.
    let acknowledged =
        format!("// {NEXT_LINE} lanekeep/parse reason: tree-sitter-typescript#367\n{REPRO}");
    let project = Project::new(
        "forbid-file-scope",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.json",
                r#"{"include": ["src/**/*.ts"], "rules": ["./rules/anchor.ts"],
                    "suppressions": {"forbidFileScope": true}}"#,
            ),
            ("src/repro.ts", &acknowledged),
        ],
    );
    let (code, json) = project.check_json(&["--no-cache"]);
    assert_eq!(code, 0, "{json}");
    assert_eq!(json["total"], 0, "{json}");
}

#[test]
fn a_whole_file_acknowledgement_under_require_expiry_still_silences() {
    let acknowledged =
        format!("// {WHOLE_FILE} lanekeep/parse reason: tree-sitter-typescript#367\n{REPRO}");
    let project = Project::new(
        "require-expiry",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.json",
                r#"{"include": ["src/**/*.ts"], "rules": ["./rules/anchor.ts"],
                    "suppressions": {"requireExpiry": true}}"#,
            ),
            ("src/repro.ts", &acknowledged),
        ],
    );
    let (code, json) = project.check_json(&["--no-cache"]);
    assert!(violations_of(&json, "lanekeep/parse").is_empty(), "{json}");
    let policy = violations_of(&json, "lanekeep/suppression");
    assert_eq!(policy.len(), 1, "{json}");
    assert!(
        policy[0]["message"]
            .as_str()
            .expect("a message")
            .starts_with("suppression has no `expires:`"),
        "{json}"
    );
    assert_eq!(code, 1, "the policy violation is an error: {json}");
}

#[test]
fn an_acknowledgement_of_an_off_report_is_unused() {
    let acknowledged =
        format!("// {NEXT_LINE} lanekeep/parse reason: tree-sitter-typescript#367\n{REPRO}");
    let project = Project::new(
        "unused-when-off",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            (
                "lanekeep.json",
                &json_config(r#"{"lanekeep/parse": "off"}"#),
            ),
            ("src/repro.ts", &acknowledged),
        ],
    );
    let (_, json) = project.check_json(&["--no-cache", "--report-unused-suppressions"]);
    let unused = violations_of(&json, "lanekeep/suppression");
    assert_eq!(unused.len(), 1, "{json}");
    assert_eq!(
        unused[0]["message"],
        "suppression silenced nothing — \"tree-sitter-typescript#367\""
    );
}

#[test]
fn a_severity_change_between_warm_runs_is_not_served_stale() {
    let project = Project::new(
        "warm-toggle",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            ("lanekeep.json", &json_config("{}")),
            ("src/repro.ts", REPRO),
        ],
    );
    let count = |project: &Project| {
        let (_, json) = project.check_json(&[]);
        violations_of(&json, "lanekeep/parse").len()
    };
    assert_eq!(count(&project), 1, "cold, warn");
    project.write(
        "lanekeep.json",
        &json_config(r#"{"lanekeep/parse": "off"}"#),
    );
    assert_eq!(
        count(&project),
        0,
        "off: the warn-era entry must not be served"
    );
    project.write("lanekeep.json", &json_config("{}"));
    assert_eq!(
        count(&project),
        1,
        "warn again: the off-era entry must not be served"
    );
}

/// The `faulted` counter for one rule's row of the gate table: its seventh number.
fn gate_faulted(stderr: &str, id: &str) -> u64 {
    let table = stderr
        .split("what each rule looked at")
        .nth(1)
        .unwrap_or_else(|| panic!("no gate table: {stderr}"));
    let line = table
        .lines()
        .find(|line| line.split_whitespace().next() == Some(id))
        .unwrap_or_else(|| panic!("no row for {id}: {stderr}"));
    line.split_whitespace()
        .nth(7)
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("no faulted column: {line}"))
}

#[test]
fn the_profile_counts_a_file_the_parser_could_not_read() {
    let project = Project::new(
        "profile",
        &[
            ("rules/anchor.ts", ANCHOR_RULE),
            ("lanekeep.json", &json_config("{}")),
            ("src/repro.ts", REPRO),
            ("src/clean.ts", "const a = 1;\n"),
        ],
    );
    let output = project.run(&["check", "--profile", "--no-cache"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(gate_faulted(&stderr, "local/anchor"), 1, "{stderr}");
}
