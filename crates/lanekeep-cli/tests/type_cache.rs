//! What a type-aware run recorded, and what changing it invalidates.
//!
//! The cross-file oracle reads declaration files through the same tracked, confined access
//! `ctx.readFile` uses, so a `.d.ts` is a dependency of every file that imported it — and an
//! *absent* one is a dependency too, with a null hash, so the answer "I could not see it"
//! is reconsidered the moment it appears. Neither property is visible from inside the engine:
//! it takes two processes over one tree, which is why this drives the real binary.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A project on disk, checked by shelling out to the real binary.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-type-cache-{name}-{}-{seq}",
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

    fn remove(&self, path: &str) {
        let _ = std::fs::remove_file(self.dir.join(path));
    }

    fn check_profiled(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg("--profile")
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

/// The exact row `write_profile` prints for a rule that did no work this run.
///
/// Copied from `component_cache.rs`, whose doc explains what it does and does not
/// distinguish: a gated-out rule renders this identical row, which is why every warm
/// assertion below pairs it with [`cached_count`].
fn zero_work_row(id: &str) -> String {
    format!(
        "  {:<40} {:>9.1?} {:>9.1?} {:>9.1?} {:>9}",
        id,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        0
    )
}

/// The `cached` counter for one rule, from `write_gate_profile`'s table.
fn cached_count(stderr: &str, id: &str) -> u64 {
    let table = stderr
        .split("what each rule looked at")
        .nth(1)
        .expect("a gate profile table in stderr");
    let line = table
        .lines()
        .find(|line| line.split_whitespace().next() == Some(id))
        .expect("a gate-table row for the rule");
    line.split_whitespace()
        .nth(3)
        .expect("a third counter in the row")
        .parse()
        .expect("a number")
}

/// A project rule that reports every `bigint`-typed declarator, which is the smallest thing
/// that can depend on a declaration file: without the `.d.ts`, `typeOf` answers nothing.
const RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/no-bigint',\n\
      requires: ['types'],\n\
      language: 'typescript',\n\
      query: { typescript: '(variable_declarator name: (identifier) @name)' },\n\
      card: { message: 'a bigint', remediation: 'use a string',\n\
              examples: { bad: 'const a = 1n', good: 'const a = \\'1\\'' } },\n\
      check(ctx, m) {\n\
        if (ctx.types.typeOf(m.name)?.primitive === 'bigint') ctx.report(m.name);\n\
      },\n\
    });\n";

const CONFIG: &str = "import { defineConfig } from 'lanekeep';\n\
    import rule from './rule';\n\
    export default defineConfig({ include: ['src/**'], namespaces: ['local'], rules: [rule] });\n";

/// The importer and the two files it might resolve to.
fn project(name: &str, with_dist: bool) -> Project {
    let project = Project::new(
        name,
        &[
            ("rule.ts", RULE),
            ("lanekeep.config.ts", CONFIG),
            (
                "src/a.ts",
                "import { id } from './dist/ids';\nconst mine = id;\n",
            ),
            ("src/b.ts", "const other = 1;\n"),
        ],
    );
    if with_dist {
        project.write("src/dist/ids.d.ts", "export declare const id: bigint;\n");
    }
    project
}

/// The absent-`dist` case, in both directions.
///
/// Absent: the declaration is not there, `typeOf` answers nothing, the rule is silent — and
/// the *miss* is recorded, which is the half a cache is wrong without. Then the file appears
/// and the importer's entry invalidates on its own, in a fresh process, with nothing else
/// touched. The two halves are one test because either alone passes against the bug: without
/// the second, an implementation that recorded nothing is green; without the first, one that
/// invalidated everything on every run is.
#[test]
fn an_absent_declaration_file_appearing_invalidates_its_importer() {
    let project = project("absent-dist", false);

    let cold = project.check_profiled();
    assert_eq!(cold.status.code(), Some(0), "{}", describe(&cold));

    let warm = project.check_profiled();
    let warm_stderr = String::from_utf8_lossy(&warm.stderr).into_owned();
    assert!(
        cached_count(&warm_stderr, "local/no-bigint") > 0,
        "the second run over unchanged input should have been served from the cache: {}",
        describe(&warm)
    );

    // The declaration appears. Nothing else about the project changed — not the rule, not the
    // config, not `src/a.ts`'s own bytes — so only a recorded *absence* can invalidate.
    project.write("src/dist/ids.d.ts", "export declare const id: bigint;\n");

    let after = project.check_profiled();
    let combined = describe(&after);
    assert_eq!(after.status.code(), Some(1), "{combined}");
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("src/a.ts"),
        "the importer must be re-checked and must now report: {combined}"
    );
}

/// Editing a `.d.ts` invalidates the importer, and an unrelated edit does not.
///
/// Both halves, named separately because they fail in opposite directions: the first fails
/// for an oracle whose reads are not recorded, the second for one that records the whole
/// project as every file's dependency.
#[test]
fn a_declaration_edit_invalidates_the_importer_and_an_unrelated_edit_does_not() {
    let project = project("edit-dts", true);

    let cold = project.check_profiled();
    assert_eq!(cold.status.code(), Some(1), "{}", describe(&cold));

    let warm = project.check_profiled();
    let warm_stderr = String::from_utf8_lossy(&warm.stderr).into_owned();
    assert!(
        warm_stderr.contains(&zero_work_row("local/no-bigint")),
        "an unchanged run should be a full cache hit: {}",
        describe(&warm)
    );

    // The declaration changes meaning. `src/a.ts`'s own bytes did not move, so only its
    // recorded read of `src/dist/ids.d.ts` can carry this.
    project.write("src/dist/ids.d.ts", "export declare const id: string;\n");
    let edited = project.check_profiled();
    let combined = describe(&edited);
    assert_eq!(
        edited.status.code(),
        Some(0),
        "the declaration is no longer a bigint, so nothing should report: {combined}"
    );

    // And an edit to a file nothing read leaves the importer's entry alone. `src/b.ts` is in
    // the corpus and is nobody's dependency, so the run recomputes b and reuses a.
    let before = project.check_profiled();
    assert_eq!(before.status.code(), Some(0), "{}", describe(&before));
    project.write("src/b.ts", "const other = 2;\n");
    let after = project.check_profiled();
    let after_stderr = String::from_utf8_lossy(&after.stderr).into_owned();
    assert!(
        cached_count(&after_stderr, "local/no-bigint") > 0,
        "an edit to a file nothing depends on must leave the other entries served: {}",
        describe(&after)
    );
}

/// A declaration file *vanishing* invalidates too, which is the direction nothing else covers.
#[test]
fn a_declaration_file_vanishing_invalidates_its_importer() {
    let project = project("vanish-dts", true);
    let cold = project.check_profiled();
    assert_eq!(cold.status.code(), Some(1), "{}", describe(&cold));

    project.remove("src/dist/ids.d.ts");
    let after = project.check_profiled();
    assert_eq!(
        after.status.code(),
        Some(0),
        "with the declaration gone the oracle answers nothing and the rule is silent: {}",
        describe(&after)
    );
}

/// A package linked out of the project is a warm cache **hit**, until `npm install` replaces it.
///
/// pnpm — and `npm link`, and a workspace — makes `node_modules/pkg` a symlink to a store
/// outside the project, so every read under it resolves out of the root and is refused unread.
/// Recorded as an ordinary absence, the validator then looked for the file, *followed the
/// link*, found it, read "appeared" and invalidated: every importer of a linked package missed
/// the cache on every run, forever, with nothing in the output to say why — and the validator
/// had read bytes outside the project root to decide it.
///
/// Both halves, because either alone passes against a wrong answer: a validator that held
/// everything would pass the first, and one that invalidated everything would pass the second.
#[cfg(unix)]
#[test]
fn a_package_linked_out_of_the_project_is_a_warm_hit_until_it_is_installed() {
    let project = Project::new(
        "linked-package",
        &[
            ("rule.ts", RULE),
            ("lanekeep.config.ts", CONFIG),
            ("src/a.ts", "import { id } from 'pkg';\nconst mine = id;\n"),
            ("src/b.ts", "const other = 1;\n"),
        ],
    );
    let store = std::env::temp_dir().join(format!(
        "lanekeep-type-cache-linked-store-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).expect("creates the store directory");
    std::fs::write(store.join("package.json"), "{\"types\": \"index.d.ts\"}\n")
        .expect("writes the store manifest");
    std::fs::write(
        store.join("index.d.ts"),
        "export declare const id: bigint;\n",
    )
    .expect("writes the store declaration");
    std::fs::create_dir_all(project.dir.join("node_modules")).expect("creates node_modules");
    std::os::unix::fs::symlink(&store, project.dir.join("node_modules/pkg"))
        .expect("links the package out of the project");

    let cold = project.check_profiled();
    assert_eq!(
        cold.status.code(),
        Some(0),
        "every read under the link is refused, so the type is unknown and the rule is silent: {}",
        describe(&cold)
    );

    let warm = project.check_profiled();
    let warm_stderr = String::from_utf8_lossy(&warm.stderr).into_owned();
    // Every file, not merely one: `src/b.ts` depends on nothing and would be served whatever
    // the refusal was recorded as, so a `> 0` here passes against the bug this pins.
    assert_eq!(
        cached_count(&warm_stderr, "local/no-bigint"),
        2,
        "nothing moved, and the refusal a rule was given is the refusal it would be given \
         again, so both files must be served from the cache: {}",
        describe(&warm)
    );
    assert!(
        warm_stderr.contains(&zero_work_row("local/no-bigint")),
        "and the rule should have done no work at all: {}",
        describe(&warm)
    );

    // `npm install` replaces the link with a real directory inside the project. The refusal is
    // no longer the answer, so the entry that rested on it has to go.
    std::fs::remove_file(project.dir.join("node_modules/pkg")).expect("removes the link");
    project.write(
        "node_modules/pkg/package.json",
        "{\"types\": \"index.d.ts\"}\n",
    );
    project.write(
        "node_modules/pkg/index.d.ts",
        "export declare const id: bigint;\n",
    );

    let installed = project.check_profiled();
    let combined = describe(&installed);
    assert_eq!(installed.status.code(), Some(1), "{combined}");
    assert!(
        String::from_utf8_lossy(&installed.stdout).contains("src/a.ts"),
        "the importer must be re-checked and must now report: {combined}"
    );

    let _ = std::fs::remove_dir_all(&store);
}

/// Two runs over one tree produce byte-identical output.
///
/// The determinism invariant, over the part of it this change is most able to break: probe
/// order, `export *` order and the declaration cache are all iteration over something, and an
/// agent reading lanekeep's output twice must not see reordering as change.
#[test]
fn two_runs_over_one_tree_are_byte_identical() {
    let project = project("determinism", true);
    let first = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
        .arg("check")
        .arg("--no-cache")
        .arg("--format")
        .arg("json")
        .arg(&project.dir)
        .output()
        .expect("runs the binary");
    let second = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
        .arg("check")
        .arg("--no-cache")
        .arg("--format")
        .arg("json")
        .arg(&project.dir)
        .output()
        .expect("runs the binary");

    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "two runs over identical input disagreed"
    );
}
