//! `--fix`, through the binary — the only path that writes to a user's files.

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
    fn new(name: &str, rule: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-fix-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let project = Self { dir };
        project.write("rule.ts", rule);
        project.write(
            "lanekeep.config.ts",
            "import { defineConfig } from 'lanekeep';\n\
             import rule from './rule';\n\
             export default defineConfig({ include: ['src/**'], rules: [rule] });\n",
        );
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

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).expect("reads")
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

/// A rule that rewrites `var` to `let`, and says the rewrite is safe.
const SAFE_RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/no-var',
  query: '(variable_declaration) @decl',
  card: {
    message: 'var declaration',
    remediation: 'use let or const',
    examples: { bad: 'var a = 1;', good: 'let a = 1;' },
  },
  check(ctx, m) {
    ctx.report(m.decl, {
      message: 'var declaration',
      fix: { node: m.decl, text: ctx.text(m.decl).replace('var ', 'let '), safe: true },
    });
  },
});
";

/// The same rewrite, offered as a suggestion rather than a safe fix.
const SUGGESTION_RULE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/no-var',
  query: '(variable_declaration) @decl',
  card: {
    message: 'var declaration',
    remediation: 'use let or const',
    examples: { bad: 'var a = 1;', good: 'let a = 1;' },
  },
  check(ctx, m) {
    ctx.report(m.decl, {
      fix: { node: m.decl, text: ctx.text(m.decl).replace('var ', 'let ') },
    });
  },
});
";

#[test]
fn a_safe_fix_is_applied() {
    let project = Project::new("safe", SAFE_RULE, &[("src/a.ts", "var a = 1;\n")]);
    let output = project.check(&["--fix"]);

    assert_eq!(project.read("src/a.ts"), "let a = 1;\n");
    assert_eq!(
        output.status.code(),
        Some(0),
        "nothing should be left to report:\n{}",
        describe(&output)
    );
}

#[test]
fn a_suggestion_is_not_applied() {
    // A rule that did not say its fix preserves behavior does not get to rewrite code.
    let project = Project::new(
        "suggestion",
        SUGGESTION_RULE,
        &[("src/a.ts", "var a = 1;\n")],
    );
    let output = project.check(&["--fix"]);

    assert_eq!(project.read("src/a.ts"), "var a = 1;\n");
    assert_eq!(
        output.status.code(),
        Some(1),
        "the violation is still there:\n{}",
        describe(&output)
    );
}

#[test]
fn nothing_is_written_without_the_flag() {
    // The whole point of the flag. A checker that edited files by default would be
    // unusable in CI.
    let project = Project::new("no-flag", SAFE_RULE, &[("src/a.ts", "var a = 1;\n")]);
    let output = project.check(&[]);

    assert_eq!(project.read("src/a.ts"), "var a = 1;\n");
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
}

#[test]
fn several_files_are_all_fixed() {
    let project = Project::new(
        "several",
        SAFE_RULE,
        &[
            ("src/a.ts", "var a = 1;\n"),
            ("src/b.ts", "var b = 2;\n"),
            ("src/c.ts", "let c = 3;\n"),
        ],
    );
    project.check(&["--fix"]);

    assert_eq!(project.read("src/a.ts"), "let a = 1;\n");
    assert_eq!(project.read("src/b.ts"), "let b = 2;\n");
    assert_eq!(project.read("src/c.ts"), "let c = 3;\n", "untouched");
}

#[test]
fn several_fixes_in_one_file_all_land() {
    // The reason edits are applied last first: each earlier offset stays valid.
    let project = Project::new(
        "many-in-one",
        SAFE_RULE,
        &[("src/a.ts", "var a = 1;\nvar b = 2;\nvar c = 3;\n")],
    );
    project.check(&["--fix"]);
    assert_eq!(
        project.read("src/a.ts"),
        "let a = 1;\nlet b = 2;\nlet c = 3;\n"
    );
}

#[test]
fn what_was_fixed_is_reported() {
    let project = Project::new(
        "reported",
        SAFE_RULE,
        &[("src/a.ts", "var a = 1;\n"), ("src/b.ts", "var b = 2;\n")],
    );
    let output = project.check(&["--fix"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("fixed 2 violation(s)"), "{stderr}");
    assert!(stderr.contains("2 file(s)"), "{stderr}");
}

#[test]
fn what_is_left_is_reported_after_fixing() {
    // Reporting the pre-fix violations would list things that are no longer there.
    const PARTIAL: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/no-var',
  query: '(variable_declaration) @decl',
  card: {
    message: 'var declaration',
    remediation: 'use let or const',
    examples: { bad: 'var a = 1;', good: 'let a = 1;' },
  },
  check(ctx, m) {
    // Only the first declaration in a file gets a fix; the rest are reported bare.
    if (ctx.line(m.decl) === 1) {
      ctx.report(m.decl, {
        fix: { node: m.decl, text: ctx.text(m.decl).replace('var ', 'let '), safe: true },
      });
    } else {
      ctx.report(m.decl);
    }
  },
});
";
    let project = Project::new(
        "partial",
        PARTIAL,
        &[("src/a.ts", "var a = 1;\nvar b = 2;\n")],
    );
    let output = project.check(&["--fix"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(project.read("src/a.ts"), "let a = 1;\nvar b = 2;\n");
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(stdout.contains("src/a.ts:2:1"), "{stdout}");
    assert!(
        !stdout.contains("src/a.ts:1:1"),
        "reported a violation that had just been fixed: {stdout}"
    );
}

#[test]
fn a_file_whose_fixes_are_all_suggestions_is_not_rewritten() {
    // The guard this needs: the file *has* fixes, so it reaches the writing path, and none
    // of them apply. Writing it back with identical bytes would still update its mtime and
    // make the file look changed to everything else watching the tree.
    let project = Project::new(
        "suggestions-only",
        SUGGESTION_RULE,
        &[("src/a.ts", "var a = 1;\n")],
    );
    let before = std::fs::metadata(project.dir.join("src/a.ts"))
        .and_then(|m| m.modified())
        .expect("has a modified time");

    // A resolution coarser than the edit is plausible on some filesystems, so the check is
    // on content and mtime together rather than mtime alone.
    project.check(&["--fix"]);

    let after = std::fs::metadata(project.dir.join("src/a.ts"))
        .and_then(|m| m.modified())
        .expect("has a modified time");
    assert_eq!(project.read("src/a.ts"), "var a = 1;\n");
    assert_eq!(
        before, after,
        "a file with no applicable fixes was rewritten"
    );
}

#[test]
fn a_clean_file_is_not_rewritten() {
    let project = Project::new("untouched", SAFE_RULE, &[("src/a.ts", "let a = 1;\n")]);
    let before = std::fs::metadata(project.dir.join("src/a.ts"))
        .and_then(|m| m.modified())
        .expect("has a modified time");

    project.check(&["--fix"]);

    let after = std::fs::metadata(project.dir.join("src/a.ts"))
        .and_then(|m| m.modified())
        .expect("has a modified time");
    assert_eq!(before, after, "a clean file was rewritten");
}

#[test]
fn fixing_is_idempotent() {
    // Running it twice must not keep editing. A fix that reapplied to its own output would
    // corrupt a file on the second run.
    let project = Project::new("idempotent", SAFE_RULE, &[("src/a.ts", "var a = 1;\n")]);
    project.check(&["--fix"]);
    let once = project.read("src/a.ts");
    project.check(&["--fix"]);
    assert_eq!(project.read("src/a.ts"), once);
}

#[test]
fn a_fix_survives_the_cache() {
    // The second run is a cache hit for the unfixed file, and the fix has to come back out
    // of the entry — otherwise `--fix` would work once and then silently stop.
    let project = Project::new("cached", SAFE_RULE, &[("src/a.ts", "var a = 1;\n")]);
    project.check(&[]); // populate the cache with the unfixed result
    project.check(&["--fix"]);
    assert_eq!(project.read("src/a.ts"), "let a = 1;\n");
}

#[test]
fn a_fix_is_visible_in_json_without_the_flag() {
    // So an editor or an agent can offer it without lanekeep writing anything.
    let project = Project::new("json", SAFE_RULE, &[("src/a.ts", "var a = 1;\n")]);
    let output = project.check(&["--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));
    let fix = &parsed["violations"][0]["fix"];

    assert_eq!(fix["safe"], true, "{stdout}");
    assert_eq!(fix["replacement"], "let a = 1;", "{stdout}");
    assert_eq!(project.read("src/a.ts"), "var a = 1;\n", "nothing written");
}

#[test]
fn a_violation_without_a_fix_has_no_fix_field() {
    const BARE: &str = r"import { defineRule } from 'lanekeep';
export default defineRule({
  id: 'local/no-var',
  query: '(variable_declaration) @decl',
  card: {
    message: 'var declaration',
    remediation: 'use let or const',
    examples: { bad: 'var a = 1;', good: 'let a = 1;' },
  },
  check(ctx, m) { ctx.report(m.decl); },
});
";
    let project = Project::new("bare", BARE, &[("src/a.ts", "var a = 1;\n")]);
    let output = project.check(&["--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));
    assert!(
        parsed["violations"][0].get("fix").is_none(),
        "an absent fix should not appear at all: {stdout}"
    );
}
