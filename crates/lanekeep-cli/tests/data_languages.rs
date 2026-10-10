//! `css`, `toml` and `yaml`: a rule can target each, through the binary, like any other
//! language (#283) — and a suppression directive works in each one's own comment syntax.
//!
//! `json` has its own file, `json_language.rs`, from the slice that added it.

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

/// One language's case: a rule over it, a file that violates it once, and where.
struct Case {
    language: &'static str,
    rule_id: &'static str,
    /// A tree-sitter query in that language's grammar, capturing `@n`.
    query: &'static str,
    file: &'static str,
    /// The violating source, with the violation on `line`.
    source: &'static str,
    line: u64,
    column: u64,
    /// The language's comment, wrapping a directive: `{}` is replaced by the directive text.
    comment: &'static str,
    /// A file the grammar cannot read whole.
    malformed: &'static str,
}

const CASES: [Case; 3] = [
    Case {
        language: "css",
        rule_id: "local/no-float",
        query: "(declaration (property_name) @n (#eq? @n \"float\"))",
        file: "src/layout.css",
        source: ".a {\n  color: red;\n  float: left;\n}\n",
        line: 3,
        column: 3,
        comment: "/* {} */",
        malformed: ".a { color: red;\n$x: 1px;\n",
    },
    Case {
        language: "toml",
        rule_id: "local/no-todo-key",
        query: "(pair (bare_key) @n (#eq? @n \"TODO\"))",
        file: "src/Cargo.toml",
        source: "[package]\nname = \"x\"\nTODO = 1\n",
        line: 3,
        column: 1,
        comment: "# {}",
        malformed: "[package\nname =\n",
    },
    Case {
        language: "yaml",
        rule_id: "local/no-latest-runner",
        query: "(block_mapping_pair key: (flow_node) @k \
                value: (flow_node (plain_scalar (string_scalar) @n)) \
                (#eq? @k \"runs-on\") (#eq? @n \"ubuntu-latest\"))",
        file: "src/ci.yml",
        source: "jobs:\n  build:\n    runs-on: ubuntu-latest\n",
        line: 3,
        column: 14,
        comment: "# {}",
        malformed: "a: [1, 2\nb: 3\n",
    },
];

impl Case {
    /// The rule module, as TypeScript.
    fn rule(&self) -> String {
        format!(
            "import {{ defineRule }} from 'lanekeep'\n\
             export default defineRule({{\n\
               id: '{id}',\n\
               language: ['{language}'],\n\
               card: {{ message: 'not this', remediation: 'something else', \
                        examples: {{ bad: 'a', good: 'b' }} }},\n\
               query: {query:?},\n\
               check(ctx, m) {{ ctx.report(m.n) }},\n\
             }})\n",
            id = self.rule_id,
            language = self.language,
            query = self.query,
        )
    }

    fn rule_path(&self) -> String {
        format!("rules/{}.ts", self.language)
    }

    fn config(&self) -> String {
        format!(
            r#"{{"include": ["src/**"], "rules": ["./{}"]}}"#,
            self.rule_path()
        )
    }

    /// The violating source with a next-line directive inserted above the violation, in this
    /// language's comment syntax, ending in an `expires:` — the field a block comment's closer
    /// used to swallow.
    fn suppressed(&self) -> String {
        let directive = format!(
            "{NEXT_LINE} {} reason: a tool reads this expires: 2999-12-31",
            self.rule_id
        );
        let comment = self.comment.replace("{}", &directive);
        let mut lines: Vec<String> = self.source.lines().map(str::to_owned).collect();
        let at = usize::try_from(self.line - 1).expect("small");
        let indent: String = lines[at].chars().take_while(|c| *c == ' ').collect();
        lines.insert(at, format!("{indent}{comment}"));
        lines.join("\n") + "\n"
    }
}

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-data-languages-{name}-{}-{seq}",
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

    fn for_case(name: &str, case: &Case, source: &str) -> Self {
        Self::new(
            &format!("{name}-{}", case.language),
            &[
                (&case.rule_path(), &case.rule()),
                ("lanekeep.json", &case.config()),
                (case.file, source),
            ],
        )
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

#[test]
fn a_rule_reports_at_the_matched_node_in_each_language() {
    for case in &CASES {
        let project = Project::for_case("reports", case, case.source);
        let (code, json) = project.check_json(&[]);
        let found = violations(&json);
        assert_eq!(code, 1, "{}: {json}", case.language);
        assert_eq!(found.len(), 1, "{}: {json}", case.language);
        assert_eq!(found[0]["rule_id"], case.rule_id);
        assert_eq!(found[0]["location"]["file"], case.file);
        let position = &found[0]["location"]["position"];
        assert_eq!(
            (position["line"].as_u64(), position["column"].as_u64()),
            (Some(case.line), Some(case.column)),
            "{}: {json}",
            case.language
        );
    }
}

/// CSS `/* */`, TOML and YAML `#`: a directive in each silences the line after it, is counted
/// as used, and its `expires:` is read — in CSS, from inside the block comment.
#[test]
fn a_directive_in_each_languages_comment_silences_the_next_line() {
    for case in &CASES {
        let source = case.suppressed();
        let project = Project::for_case("suppressed", case, &source);
        let (code, json) = project.check_json(&["--report-unused-suppressions"]);
        assert_eq!(code, 0, "{}:\n{source}\n{json}", case.language);
        assert!(
            violations(&json).is_empty(),
            "{}: silenced, the directive well-formed and counted as used:\n{source}\n{json}",
            case.language
        );
    }
}

/// Registering a grammar must not start parsing files no rule asked for: a config whose
/// `include` covers malformed stylesheets and manifests, but whose rules all target
/// TypeScript, says nothing about them.
#[test]
fn files_no_rule_targets_are_not_parsed() {
    let ts_rule = "import { defineRule } from 'lanekeep'\n\
        export default defineRule({\n\
          id: 'local/anchor',\n\
          language: ['typescript'],\n\
          card: { message: 'a program', remediation: 'n/a', \
                  examples: { bad: 'a', good: 'b' } },\n\
          query: '(program) @p',\n\
          check(ctx, m) {},\n\
        })\n";
    let mut files = vec![
        ("rules/anchor.ts", ts_rule),
        (
            "lanekeep.json",
            r#"{"include": ["src/**"], "rules": ["./rules/anchor.ts"]}"#,
        ),
        ("src/a.ts", "export const a = 1\n"),
    ];
    for case in &CASES {
        files.push((case.file, case.malformed));
    }
    let project = Project::new("untargeted", &files);
    let (code, json) = project.check_json(&[]);
    assert_eq!(code, 0, "{json}");
    assert!(violations(&json).is_empty(), "{json}");
}

/// A file the grammar cannot read whole is reported as one, naming that language's parser.
#[test]
fn a_malformed_file_is_a_parse_fault_under_a_rule_for_its_language() {
    for case in &CASES {
        let project = Project::for_case("fault", case, case.malformed);
        let (_, json) = project.check_json(&[]);
        let faults: Vec<_> = violations(&json)
            .iter()
            .filter(|v| v["rule_id"] == "lanekeep/parse")
            .collect();
        assert_eq!(faults.len(), 1, "{}: {json}", case.language);
        let message = faults[0]["message"].as_str().expect("a message");
        assert!(
            message.starts_with(&format!("the {} parser ", case.language)),
            "{message}"
        );
    }
}
