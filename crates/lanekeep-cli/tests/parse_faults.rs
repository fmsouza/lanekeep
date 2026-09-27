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
