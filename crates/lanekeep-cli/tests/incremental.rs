//! `--since` and `--staged`, driven through the binary over a real git repository.

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

/// A git repository with a lanekeep config, removed on drop.
struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str, config: &str, files: &[(&str, &str)]) -> Self {
        Self::with_config(name, "lanekeep.config.ts", config, files)
    }

    /// The same, under a chosen config file name, so a test can drive `lanekeep.json`.
    fn with_config(name: &str, config_file: &str, config: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-incr-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");

        let repo = Self { dir };
        repo.git(&["init", "--quiet"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "commit.gpgsign", "false"]);

        repo.write(config_file, config);
        for (path, contents) in files {
            repo.write(path, contents);
        }
        repo.commit("first");
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .expect("runs git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    fn check(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .args(args)
            .output()
            .expect("runs the binary")
    }
}

impl Drop for Repo {
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

/// How many violations the run reported, from the human summary.
fn violation_count(output: &Output) -> usize {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| {
            line.split_once(" error(s)")
                .and_then(|(head, _)| head.rsplit(' ').next())
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap_or(0)
}

const PER_FILE_CONFIG: &str = "import { defineConfig } from 'lanekeep';\n\
     import rule from 'lanekeep/no-default-export';\n\
     export default defineConfig({ include: ['src/**'], rules: [rule] });\n";

const CROSS_FILE_CONFIG: &str = "import { defineConfig } from 'lanekeep';\n\
     import rule from 'lanekeep/no-unused-exports';\n\
     export default defineConfig({ include: ['src/**'], rules: [rule({})] });\n";

#[test]
fn staged_checks_only_what_is_staged() {
    let repo = Repo::new(
        "staged",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );

    // Both files violate; only one is staged.
    repo.write("src/a.ts", "export default 3;\n");
    repo.git(&["add", "src/a.ts"]);

    let output = repo.check(&["--staged"]);
    let combined = describe(&output);
    assert_eq!(violation_count(&output), 1, "{combined}");
    assert!(combined.contains("src/a.ts"), "{combined}");
    assert!(!combined.contains("src/b.ts"), "{combined}");
}

#[test]
fn since_checks_only_what_changed() {
    let repo = Repo::new(
        "since",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );

    repo.write("src/a.ts", "export default 3;\n");

    let output = repo.check(&["--since", "HEAD"]);
    let combined = describe(&output);
    assert_eq!(violation_count(&output), 1, "{combined}");
    assert!(combined.contains("src/a.ts"), "{combined}");
}

#[test]
fn no_selection_checks_everything() {
    let repo = Repo::new(
        "all",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );
    let output = repo.check(&[]);
    assert_eq!(violation_count(&output), 2, "{}", describe(&output));
}

#[test]
fn an_untracked_file_is_selected() {
    // A file you just created is a file you just changed, and it is the likeliest place for
    // a new violation.
    let repo = Repo::new(
        "untracked",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    repo.write("src/new.ts", "export default 1;\n");

    let output = repo.check(&["--since", "HEAD"]);
    let combined = describe(&output);
    assert_eq!(violation_count(&output), 1, "{combined}");
    assert!(combined.contains("src/new.ts"), "{combined}");
}

#[test]
fn a_selected_file_the_config_excludes_is_not_checked() {
    // The selection is intersected with discovery, not used in place of it. Otherwise
    // `--staged` would quietly override `include` and `exclude`.
    let repo = Repo::new(
        "excluded",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "const a = 1;\n"),
            ("vendor/x.ts", "const x = 1;\n"),
        ],
    );
    repo.write("vendor/x.ts", "export default 1;\n");
    repo.git(&["add", "-A"]);

    let output = repo.check(&["--staged"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(violation_count(&output), 0, "{}", describe(&output));
}

#[test]
fn a_deleted_file_does_not_break_the_run() {
    let repo = Repo::new(
        "deleted",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "const a = 1;\n"),
            ("src/b.ts", "const b = 1;\n"),
        ],
    );
    std::fs::remove_file(repo.dir.join("src/b.ts")).expect("removes");
    repo.git(&["add", "-A"]);

    let output = repo.check(&["--staged"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
}

#[test]
fn nothing_staged_checks_nothing() {
    let repo = Repo::new(
        "nothing",
        PER_FILE_CONFIG,
        &[("src/a.ts", "export default 1;\n")],
    );
    let output = repo.check(&["--staged"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{combined}");
    assert_eq!(violation_count(&output), 0, "{combined}");
}

#[test]
fn a_narrowed_run_says_which_cross_file_rules_did_not_run() {
    // The behavior worth being loud about. A cross-file rule over a subset does not give a
    // smaller answer, it gives a wrong one — so it is skipped, and staying quiet about it
    // would make a clean result read as "fixed".
    let repo = Repo::new(
        "cross-file-note",
        CROSS_FILE_CONFIG,
        &[
            ("src/a.ts", "export function used() {}\n"),
            ("src/b.ts", "import { used } from './a';\nused();\n"),
        ],
    );
    repo.write(
        "src/a.ts",
        "export function used() {}\nexport function spare() {}\n",
    );
    repo.git(&["add", "-A"]);

    let output = repo.check(&["--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lanekeep/no-unused-exports"),
        "the skipped rule is not named: {stderr}"
    );
    assert!(stderr.contains("--staged"), "{stderr}");
    assert_eq!(
        violation_count(&output),
        0,
        "the cross-file rule should not have reported: {}",
        describe(&output)
    );
}

#[test]
fn a_full_run_still_runs_cross_file_rules() {
    // The other half: skipping must be a property of narrowing, not of the rule.
    let repo = Repo::new(
        "cross-file-full",
        CROSS_FILE_CONFIG,
        &[
            (
                "src/a.ts",
                "export function used() {}\nexport function spare() {}\n",
            ),
            ("src/b.ts", "import { used } from './a';\nused();\n"),
        ],
    );

    let output = repo.check(&[]);
    let combined = describe(&output);
    assert_eq!(violation_count(&output), 1, "{combined}");
    assert!(combined.contains("spare"), "{combined}");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("does not run cross-file"),
        "a full run should not warn: {combined}"
    );
}

#[test]
fn a_narrowed_run_without_cross_file_rules_is_quiet() {
    let repo = Repo::new("quiet", PER_FILE_CONFIG, &[("src/a.ts", "const a = 1;\n")]);
    repo.write("src/a.ts", "const a = 2;\n");
    repo.git(&["add", "-A"]);

    let output = repo.check(&["--staged"]);
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("does not run cross-file"),
        "nothing was skipped, so nothing should be said: {}",
        describe(&output)
    );
}

#[test]
fn an_unknown_ref_fails_loudly() {
    // Checking everything instead would be a surprising amount of work done silently;
    // checking nothing would look like a clean run.
    let repo = Repo::new(
        "bad-ref",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    let output = repo.check(&["--since", "no-such-ref"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("git"), "{combined}");
}

#[test]
fn since_and_staged_cannot_be_combined() {
    let repo = Repo::new("both", PER_FILE_CONFIG, &[("src/a.ts", "const a = 1;\n")]);
    let output = repo.check(&["--since", "HEAD", "--staged"]);
    assert_ne!(output.status.code(), Some(0), "{}", describe(&output));
}

// --- a rule's options are part of the cache key ---------------------------------------

/// A `lanekeep.json` whose one rule restricts the given modules.
fn json_config(restrictions: &str) -> String {
    format!(
        r#"{{
  "include": ["src/**"],
  "rules": [
    {{ "rule": "lanekeep/no-restricted-imports",
       "options": {{ "restrictions": [{restrictions}] }} }}
  ]
}}
"#
    )
}

/// The same configuration written as a module.
fn module_config(restrictions: &str) -> String {
    format!(
        "import {{ defineConfig }} from 'lanekeep';\n\
         import noRestrictedImports from 'lanekeep/no-restricted-imports';\n\
         export default defineConfig({{\n\
           include: ['src/**'],\n\
           rules: [noRestrictedImports({{ restrictions: [{restrictions}] }})],\n\
         }});\n"
    )
}

const STRIPE: &str = r#"{"module": "stripe"}"#;

/// Editing a rule's options invalidates the cache.
///
/// The options are the whole content of a configurable rule, and for a JSON config they
/// reach neither the module sources `ruleset_hash` covers nor — before this test — the
/// values `config_hash` covers. A warm run therefore kept answering the previous
/// configuration for as long as the cache survived, which is silent in both directions: a
/// restriction added still reports clean, and a restriction removed still reports.
#[test]
fn changing_a_rule_option_in_a_json_config_invalidates_the_cache() {
    let repo = Repo::with_config(
        "json-options",
        "lanekeep.json",
        &json_config(""),
        &[("src/a.ts", "import Stripe from 'stripe'\nStripe\n")],
    );

    // Cold: nothing is restricted, so nothing is reported — and that is cached.
    let clean = repo.check(&[]);
    assert_eq!(clean.status.code(), Some(0), "{}", describe(&clean));
    assert_eq!(violation_count(&clean), 0, "{}", describe(&clean));

    // The only edit is the options. Warm, and it has to be seen.
    repo.write("lanekeep.json", &json_config(STRIPE));
    let restricted = repo.check(&[]);
    assert_eq!(
        violation_count(&restricted),
        1,
        "a warm run answered the configuration from before the edit: {}",
        describe(&restricted)
    );

    // And back: taking the restriction away has to be seen just as much.
    repo.write("lanekeep.json", &json_config(""));
    let relaxed = repo.check(&[]);
    assert_eq!(relaxed.status.code(), Some(0), "{}", describe(&relaxed));
    assert_eq!(
        violation_count(&relaxed),
        0,
        "a warm run kept reporting a restriction the config no longer has: {}",
        describe(&relaxed)
    );
}

/// The same edit in a module config, which is covered by a different hash.
///
/// Options written inline are ordinary module source, so the loader records them and
/// `ruleset_hash` covers them. This pins that, because it is the reason the fix belongs in
/// `config_hash` and needs no second mechanism in the loader.
#[test]
fn changing_a_rule_option_in_a_module_config_invalidates_the_cache() {
    let repo = Repo::new(
        "module-options",
        &module_config(""),
        &[("src/a.ts", "import Stripe from 'stripe'\nStripe\n")],
    );

    let clean = repo.check(&[]);
    assert_eq!(clean.status.code(), Some(0), "{}", describe(&clean));
    assert_eq!(violation_count(&clean), 0, "{}", describe(&clean));

    repo.write("lanekeep.config.ts", &module_config(STRIPE));
    let restricted = repo.check(&[]);
    assert_eq!(
        violation_count(&restricted),
        1,
        "a warm run answered the configuration from before the edit: {}",
        describe(&restricted)
    );
}
