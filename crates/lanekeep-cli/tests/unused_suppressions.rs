//! `suppressions.unused`: a directive that silenced nothing can fail the run (#285).

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

struct Project {
    dir: PathBuf,
}

impl Project {
    /// The issue's reproduction: a built-in rule, and a directive above code it never reports.
    fn stale(name: &str, suppressions: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-unused-suppressions-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("creates dir");
        let project = Self { dir };
        project.configure(suppressions);
        std::fs::write(
            project.dir.join("src/a.ts"),
            format!(
                "// {NEXT_LINE} lanekeep/no-default-export reason: stale, nothing below \
                 violates\nexport const a = 1;\n"
            ),
        )
        .expect("writes");
        project
    }

    /// Rewrite `lanekeep.json` with `suppressions` as given; empty means no block at all.
    fn configure(&self, suppressions: &str) {
        let block = if suppressions.is_empty() {
            String::new()
        } else {
            format!(r#", "suppressions": {suppressions}"#)
        };
        std::fs::write(
            self.dir.join("lanekeep.json"),
            format!(
                r#"{{"include": ["src/**/*.ts"], "rules": [{{"rule": "lanekeep/no-default-export"}}]{block}}}"#
            ),
        )
        .expect("writes");
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    /// `check --format json`, parsed: the exit code and the `lanekeep/suppression` severities.
    fn check(&self, extra: &[&str]) -> (i32, Vec<String>) {
        let mut args = vec!["check", "--format", "json"];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        let code = output.status.code().expect("exited");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let severities = json["violations"]
            .as_array()
            .expect("a violations array")
            .iter()
            .filter(|v| v["rule_id"] == "lanekeep/suppression")
            .map(|v| v["severity"].as_str().expect("a severity").to_owned())
            .collect();
        (code, severities)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_configured_error_fails_the_run() {
    let project = Project::stale("error", r#"{"unused": "error"}"#);
    assert_eq!(
        project.check(&["--no-cache"]),
        (1, vec!["error".to_owned()])
    );
}

#[test]
fn without_the_key_a_stale_directive_is_not_reported() {
    let project = Project::stale("absent", "");
    assert_eq!(project.check(&["--no-cache"]), (0, Vec::new()));
}

#[test]
fn the_flag_alone_still_warns_and_passes() {
    let project = Project::stale("flag", "");
    assert_eq!(
        project.check(&["--no-cache", "--report-unused-suppressions"]),
        (0, vec!["warn".to_owned()])
    );
}

#[test]
fn the_flag_does_not_lower_a_configured_error() {
    let project = Project::stale("flag-floor", r#"{"unused": "error"}"#);
    assert_eq!(
        project.check(&["--no-cache", "--report-unused-suppressions"]),
        (1, vec!["error".to_owned()])
    );
}

#[test]
fn raising_the_setting_between_warm_runs_is_not_served_stale() {
    // Through the cache, both ways round: the second run's verdict has to follow the config it
    // was given, not the one the cache was written under.
    let project = Project::stale("warm", r#"{"unused": "warn"}"#);
    assert_eq!(project.check(&[]), (0, vec!["warn".to_owned()]));
    project.configure(r#"{"unused": "error"}"#);
    assert_eq!(project.check(&[]), (1, vec!["error".to_owned()]));
    project.configure(r#"{"unused": "off"}"#);
    assert_eq!(project.check(&[]), (0, Vec::new()));
}

#[test]
fn an_unknown_key_under_suppressions_is_refused() {
    // The issue's second half: a misspelled key used to load and do nothing.
    let project = Project::stale("unknown-key", r#"{"unusedd": "error"}"#);
    let output = project.run(&["check", "--no-cache"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("unusedd"), "{stderr}");
}
