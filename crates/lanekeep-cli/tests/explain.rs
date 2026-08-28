//! `lanekeep explain` and `lanekeep rules --json`, through the binary.

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

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, config: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-explain-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("creates dir");

        let project = Self { dir };
        std::fs::write(project.dir.join("lanekeep.json"), config).expect("writes config");
        std::fs::write(project.dir.join("src/a.ts"), "const a = 1;\n").expect("writes source");
        project
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
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

/// A per-file rule and a cross-file one, which is what `explain` has to tell apart.
///
/// JSON rather than a `lanekeep.config.ts`: both of these rules are compiled into a component
/// now, and a component is not a value a module can import. What `explain` reads is a loaded
/// `Config`, which is the same either way.
const CONFIG: &str = r#"{
      "include": ["src/**"],
      "timeouts": {"rule": 600000, "global": 600000},
      "rules": ["lanekeep/no-default-export",
                {"rule": "lanekeep/no-unused-exports", "options": {}}]
    }"#;

#[test]
fn explain_prints_the_card() {
    let project = Project::new("card", CONFIG);
    let output = project.run(&["explain", "lanekeep/no-default-export"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(stdout.contains("lanekeep/no-default-export"), "{stdout}");
    assert!(stdout.contains("default export"), "{stdout}");
    assert!(stdout.contains("Fix:"), "{stdout}");
    assert!(stdout.contains("Bad:"), "{stdout}");
    assert!(stdout.contains("Good:"), "{stdout}");
}

#[test]
fn explain_says_when_a_rule_reads_the_whole_corpus() {
    // It changes what `--since` and `--staged` do with the rule, which is exactly the kind
    // of thing someone runs `explain` to find out.
    let project = Project::new("cross-file", CONFIG);
    let output = project.run(&["explain", "lanekeep/no-unused-exports"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("whole corpus"), "{stdout}");

    let other = project.run(&["explain", "lanekeep/no-default-export"]);
    assert!(
        !String::from_utf8_lossy(&other.stdout).contains("whole corpus"),
        "a per-file rule should not claim otherwise: {}",
        describe(&other)
    );
}

#[test]
fn explain_lists_what_is_configured_when_the_rule_is_unknown() {
    // The answer is always in the list, so printing the list beats any guess at what was
    // meant.
    let project = Project::new("unknown", CONFIG);
    let output = project.run(&["explain", "lanekeep/no-such-rule"]);
    let combined = describe(&output);

    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-unused-exports"),
        "{combined}"
    );
}

#[test]
fn explain_suggests_the_namespaced_form_of_a_bare_id() {
    // The common mistake by a wide margin: writing `no-default-export` for
    // `lanekeep/no-default-export`.
    let project = Project::new("bare", CONFIG);
    let output = project.run(&["explain", "no-default-export"]);
    let combined = describe(&output);

    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("did you mean"), "{combined}");
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "{combined}"
    );
}

#[test]
fn explain_as_json_is_valid_and_complete() {
    let project = Project::new("json", CONFIG);
    let output = project.run(&["explain", "lanekeep/no-default-export", "--json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));

    assert_eq!(parsed["id"], "lanekeep/no-default-export");
    assert_eq!(parsed["severity"], "error");
    assert_eq!(parsed["crossFile"], false);
    assert!(parsed["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(
        parsed["remediation"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
    );
    assert!(parsed["examples"]["bad"].as_str().is_some());
    assert!(parsed["examples"]["good"].as_str().is_some());
    assert!(
        parsed["query"].is_object()
            && !parsed["query"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
        "the query is one entry per declared language: {:?}",
        parsed["query"]
    );
}

#[test]
fn rules_as_json_lists_every_configured_rule() {
    let project = Project::new("rules-json", CONFIG);
    let output = project.run(&["rules", "--json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));

    assert_eq!(parsed["version"], 1);
    assert_eq!(parsed["declared"], 2);
    assert_eq!(parsed["enabled"], 2);

    let ids: Vec<&str> = parsed["rules"]
        .as_array()
        .expect("rules array")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["lanekeep/no-default-export", "lanekeep/no-unused-exports"]
    );
}

#[test]
fn rules_without_json_stays_human() {
    let project = Project::new("rules-human", CONFIG);
    let output = project.run(&["rules"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rule(s) configured"), "{stdout}");
    assert!(!stdout.starts_with('{'), "{stdout}");
}

#[test]
fn explain_output_is_identical_across_runs() {
    // Same reason as everything else here: an agent reads it twice.
    let project = Project::new("stable", CONFIG);
    let first = project
        .run(&["explain", "lanekeep/no-default-export"])
        .stdout;
    for _ in 0..3 {
        assert_eq!(
            project
                .run(&["explain", "lanekeep/no-default-export"])
                .stdout,
            first
        );
    }
}
