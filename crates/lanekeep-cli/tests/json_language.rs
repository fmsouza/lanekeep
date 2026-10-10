//! `json`: a rule can target JSON files, through the binary, like any other language (#283).

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

/// The directive token, assembled rather than written: lanekeep checks this file, and a token
/// spelled out here would be a live directive in it.
const NEXT_LINE: &str = concat!("lanekeep", "-ignore-next-line");

/// Reports every key spelled `TODO`, at the key's text.
const KEY_RULE: &str = "import { defineRule } from 'lanekeep'\n\
    export default defineRule({\n\
      id: 'local/no-todo-key',\n\
      language: ['json'],\n\
      card: { message: 'a TODO key', remediation: 'name the key', \
              examples: { bad: '{\"TODO\": 1}', good: '{\"done\": 1}' } },\n\
      query: '(pair key: (string (string_content) @k) (#eq? @k \"TODO\"))',\n\
      check(ctx, m) { ctx.report(m.k) },\n\
    })\n";

const CONFIG: &str = r#"{"include": ["src/**"], "rules": ["./rules/key.ts"]}"#;

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-json-language-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");
        let project = Self { dir };
        for (path, contents) in files {
            let full = project.dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("creates parent");
            }
            std::fs::write(full, contents).expect("writes");
        }
        project
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    /// `check --format json --no-cache`, parsed, plus the exit code.
    fn check_json(&self, extra: &[&str]) -> (i32, serde_json::Value) {
        let mut args = vec!["check", "--format", "json", "--no-cache"];
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

fn violations(json: &serde_json::Value) -> &Vec<serde_json::Value> {
    json["violations"].as_array().expect("a violations array")
}

fn violations_of<'a>(json: &'a serde_json::Value, rule: &str) -> Vec<&'a serde_json::Value> {
    violations(json)
        .iter()
        .filter(|v| v["rule_id"] == rule)
        .collect()
}

#[test]
fn a_json_rule_reports_at_the_matched_key() {
    let project = Project::new(
        "reports",
        &[
            ("rules/key.ts", KEY_RULE),
            ("lanekeep.json", CONFIG),
            ("src/messages.json", "{\n  \"TODO\": 1,\n  \"done\": 2\n}\n"),
        ],
    );
    let (code, json) = project.check_json(&[]);
    assert_eq!(code, 1, "{json}");
    let found = violations_of(&json, "local/no-todo-key");
    assert_eq!(found.len(), 1, "{json}");
    assert_eq!(found[0]["location"]["file"], "src/messages.json");
    assert_eq!(found[0]["location"]["position"]["line"], 2);
    // `  "TODO"`: the key's text starts after two spaces and the opening quote.
    assert_eq!(found[0]["location"]["position"]["column"], 4);
    assert_eq!(violations(&json).len(), 1, "nothing else: {json}");
}

/// `.jsonc` is JSON too, and its comments carry directives — the text scan finds them.
#[test]
fn a_directive_in_a_jsonc_comment_silences_the_next_line() {
    let settings = format!(
        "{{\n  // {NEXT_LINE} local/no-todo-key reason: a key a third-party tool reads\n  \
         \"TODO\": 1\n}}\n"
    );
    let project = Project::new(
        "suppressed",
        &[
            ("rules/key.ts", KEY_RULE),
            ("lanekeep.json", CONFIG),
            ("src/settings.jsonc", &settings),
        ],
    );
    let (code, json) = project.check_json(&["--report-unused-suppressions"]);
    assert_eq!(code, 0, "{json}");
    assert!(
        violations(&json).is_empty(),
        "silenced, and the directive counted as used: {json}"
    );
}

/// Registering a grammar must not start parsing files no rule asked for: a config whose
/// `include` covers a malformed `.json` but whose rules all target TypeScript says nothing
/// about it — no `lanekeep/parse`, no violation.
#[test]
fn json_files_no_rule_targets_are_not_parsed() {
    let ts_rule = "import { defineRule } from 'lanekeep'\n\
        export default defineRule({\n\
          id: 'local/anchor',\n\
          language: ['typescript'],\n\
          card: { message: 'a program', remediation: 'n/a', \
                  examples: { bad: 'a', good: 'b' } },\n\
          query: '(program) @p',\n\
          check(ctx, m) {},\n\
        })\n";
    let project = Project::new(
        "untargeted",
        &[
            ("rules/anchor.ts", ts_rule),
            (
                "lanekeep.json",
                r#"{"include": ["src/**"], "rules": ["./rules/anchor.ts"]}"#,
            ),
            ("src/a.ts", "export const a = 1\n"),
            ("src/broken.json", "{\"a\": 1,,,\n"),
        ],
    );
    let (code, json) = project.check_json(&[]);
    assert_eq!(code, 0, "{json}");
    assert!(violations(&json).is_empty(), "{json}");
}

/// A trailing comma is not JSON, and the grammar says so: the file is reported as one the
/// parser could not read whole, naming the json parser.
#[test]
fn a_trailing_comma_is_a_parse_fault_under_a_json_rule() {
    let project = Project::new(
        "trailing-comma",
        &[
            ("rules/key.ts", KEY_RULE),
            ("lanekeep.json", CONFIG),
            ("src/tsconfig.json", "{\n  \"strict\": true,\n}\n"),
        ],
    );
    let (_, json) = project.check_json(&[]);
    let faults = violations_of(&json, "lanekeep/parse");
    assert_eq!(faults.len(), 1, "{json}");
    assert!(
        faults[0]["message"]
            .as_str()
            .expect("a message")
            .starts_with("the json parser "),
        "{json}"
    );
}

/// A language that is not registered is still refused, and the refusal lists `json` among the
/// languages that are. Sass rather than the issue's own `css`, which is registered now: `.scss`
/// is a different grammar from CSS's and stays unclaimed.
#[test]
fn an_unknown_language_lists_json_as_known() {
    let scss_rule = KEY_RULE.replace("['json']", "['scss']");
    let project = Project::new(
        "unknown",
        &[
            ("rules/key.ts", &scss_rule),
            ("lanekeep.json", CONFIG),
            ("src/a.scss", "$x: 1px;\n"),
        ],
    );
    let output = project.run(&["check", "--no-cache"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown language `scss`"), "{stderr}");
    // The whole list, not each name: `css` alone would be found inside `scss` above.
    assert!(
        stderr.contains(
            "known languages: css, go, javascript, json, python, rust, toml, tsx, typescript, yaml"
        ),
        "{stderr}"
    );
}
