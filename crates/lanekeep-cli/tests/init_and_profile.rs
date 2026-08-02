//! `lanekeep init` and `check --profile` — the last two items of §12's surface.

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
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-init-{name}-{}-{seq}", std::process::id()));
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

    /// Run the binary with the project directory appended.
    ///
    /// Appended rather than inserted after the subcommand, because `explain` takes the rule
    /// id as its first positional and the directory as its second. Clap accepts positionals
    /// after flags, so one rule works for every command.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    fn exists(&self, path: &str) -> bool {
        self.dir.join(path).exists()
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).expect("reads")
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

// --- init ---------------------------------------------------------------------------------

#[test]
fn init_writes_a_config_and_a_rule() {
    let project = Project::new("writes", &[]);
    let output = project.run(&["init"]);

    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(project.exists("lanekeep.config.ts"));
    assert!(project.exists("lanekeep/rules/no-debugger.ts"));
}

#[test]
fn what_init_writes_actually_runs() {
    // The only assertion that matters. A starter config that does not check anything is a
    // worse starting point than an empty directory, because it looks like it works.
    let project = Project::new(
        "runs",
        &[(
            "src/a.ts",
            "function pay() { debugger; }\nexport default pay;\n",
        )],
    );
    project.run(&["init"]);

    let output = project.run(&["check"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(1), "{combined}");
    assert!(combined.contains("local/no-debugger"), "{combined}");
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "{combined}"
    );
}

#[test]
fn the_starter_rule_can_be_explained() {
    // The card is complete, which is what makes the starter a template rather than a stub.
    let project = Project::new("explain", &[]);
    project.run(&["init"]);

    let output = project.run(&["explain", "local/no-debugger"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(stdout.contains("Fix:"), "{stdout}");
    assert!(stdout.contains("Bad:"), "{stdout}");
    assert!(stdout.contains("Good:"), "{stdout}");
}

#[test]
fn init_refuses_to_overwrite() {
    // A config is a file someone has edited by the time they run this again, and nothing
    // else has a copy.
    let project = Project::new("no-overwrite", &[]);
    project.run(&["init"]);
    project.run(&["init"]);

    let mine = "// mine\n";
    std::fs::write(project.dir.join("lanekeep.config.ts"), mine).expect("writes");
    let output = project.run(&["init"]);

    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert_eq!(
        project.read("lanekeep.config.ts"),
        mine,
        "it was overwritten"
    );
    assert!(
        describe(&output).contains("--force"),
        "the error should say how to proceed: {}",
        describe(&output)
    );
}

#[test]
fn force_overwrites() {
    let project = Project::new("force", &[]);
    project.run(&["init"]);
    std::fs::write(project.dir.join("lanekeep.config.ts"), "// mine\n").expect("writes");

    let output = project.run(&["init", "--force"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(project.read("lanekeep.config.ts").contains("defineConfig"));
}

#[test]
fn init_keeps_the_cache_out_of_git() {
    // The cache is a multi-megabyte binary the first run drops into the working tree. Left
    // unmentioned it gets committed, which is how it went into the first project I tried
    // this on.
    let project = Project::new("gitignore", &[]);
    std::fs::create_dir_all(project.dir.join(".git")).expect("fake repo");

    project.run(&["init"]);

    assert!(project.exists(".gitignore"));
    assert!(
        project.read(".gitignore").contains(".lanekeep/"),
        "{}",
        project.read(".gitignore")
    );
}

#[test]
fn init_appends_rather_than_replacing_an_existing_gitignore() {
    let project = Project::new("gitignore-append", &[(".gitignore", "node_modules\n")]);
    std::fs::create_dir_all(project.dir.join(".git")).expect("fake repo");

    project.run(&["init"]);

    let written = project.read(".gitignore");
    assert!(written.contains("node_modules"), "{written}");
    assert!(written.contains(".lanekeep/"), "{written}");
}

#[test]
fn init_does_not_repeat_an_entry_that_is_already_there() {
    let project = Project::new("gitignore-twice", &[(".gitignore", ".lanekeep/\n")]);
    std::fs::create_dir_all(project.dir.join(".git")).expect("fake repo");

    project.run(&["init"]);

    assert_eq!(
        project.read(".gitignore").matches(".lanekeep/").count(),
        1,
        "the entry was added twice"
    );
}

#[test]
fn init_writes_no_gitignore_outside_a_repository() {
    // Nothing to ignore for, and a stray .gitignore is noise.
    let project = Project::new("gitignore-none", &[]);

    project.run(&["init"]);

    assert!(!project.exists(".gitignore"));
}

// --- profile -------------------------------------------------------------------------------

const CONFIG: &str = "import { defineConfig } from 'lanekeep';\n\
     import rule from 'lanekeep/no-default-export';\n\
     export default defineConfig({ include: ['src/**'], rules: [rule] });\n";

#[test]
fn profile_reports_the_query_handler_split() {
    // The split is the whole point: it tells an author whether their query or their code is
    // the problem, and one total would leave them guessing.
    let project = Project::new(
        "profile",
        &[
            ("lanekeep.config.ts", CONFIG),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    let output = project.run(&["check", "--profile", "--no-cache"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("lanekeep/no-default-export"), "{stderr}");
    assert!(stderr.contains("query"), "{stderr}");
    assert!(stderr.contains("handler"), "{stderr}");
    assert!(stderr.contains("matches"), "{stderr}");
}

#[test]
fn profile_goes_to_stderr_so_json_still_pipes() {
    let project = Project::new(
        "profile-json",
        &[
            ("lanekeep.config.ts", CONFIG),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    let output = project.run(&["check", "--profile", "--no-cache", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("profile leaked into stdout ({e}): {stdout}"));
}

#[test]
fn nothing_is_profiled_without_the_flag() {
    // Measuring costs a clock read per handler invocation, and the path a warm run takes is
    // the one place that matters most.
    let project = Project::new(
        "profile-off",
        &[
            ("lanekeep.config.ts", CONFIG),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    let output = project.run(&["check", "--no-cache"]);
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("profile"),
        "{}",
        describe(&output)
    );
}

#[test]
fn a_rule_that_never_matched_still_appears() {
    // Its query ran on every file, and that cost is real. Omitting it would hide the rule
    // whose query is expensive *and* matches nothing — the worst case there is.
    let project = Project::new(
        "profile-nomatch",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import a from 'lanekeep/no-default-export';\n\
                 import b from 'lanekeep/no-restricted-imports';\n\
                 export default defineConfig({ include: ['src/**'], rules: [a, b({})] });\n",
            ),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    let output = project.run(&["check", "--profile", "--no-cache"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lanekeep/no-restricted-imports"),
        "a rule with no matches was omitted: {stderr}"
    );
}
