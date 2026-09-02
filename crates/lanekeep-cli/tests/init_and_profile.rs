//! `lanekeep init` and `check --profile` — the last two items of §12's surface.

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
    // JSON, not TypeScript: configuration is data, and a Go or Python team should not have
    // to write a `.ts` file to say which rules they want.
    assert!(project.exists("lanekeep.json"));
    assert!(project.exists("lanekeep/rules/no-debugger.ts"));
}

#[test]
fn init_scaffolds_for_the_language_it_finds() {
    // A Go team running `init` and receiving a TypeScript config with a rule about
    // `debugger` statements learns that this tool is not really for them.
    for (marker, contents, expected_rule, expected_glob) in [
        (
            "go.mod",
            "module example.com/a\n",
            "no-fmt-println.ts",
            "**/*.go",
        ),
        (
            "pyproject.toml",
            "[project]\nname = \"a\"\n",
            "no-print.ts",
            "**/*.py",
        ),
        (
            "package.json",
            "{}\n",
            "no-debugger.ts",
            "src/**/*.{ts,tsx}",
        ),
    ] {
        let project = Project::new(marker, &[(marker, contents)]);
        let output = project.run(&["init"]);

        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert!(
            project.exists(&format!("lanekeep/rules/{expected_rule}")),
            "{marker} should scaffold {expected_rule}: {}",
            describe(&output)
        );
        assert!(
            project.read("lanekeep.json").contains(expected_glob),
            "{marker} should include {expected_glob}"
        );
    }
}

#[test]
fn every_scaffold_catches_something_on_the_first_run() {
    // The only assertion that matters, for each language rather than only for TypeScript. A
    // starter config that reports nothing is a worse starting point than an empty directory,
    // because it looks like it works.
    for (marker, manifest, source_path, source) in [
        (
            "go.mod",
            "module example.com/a\n",
            "internal/a.go",
            "package a\n\nimport \"fmt\"\n\nfunc F() { fmt.Println(\"x\") }\n",
        ),
        (
            "pyproject.toml",
            "[project]\nname = \"a\"\n",
            "app.py",
            "def f():\n    print(\"x\")\n",
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\n",
            "src/lib.rs",
            "pub fn f() {\n    let _ = dbg!(1);\n}\n",
        ),
    ] {
        let project = Project::new(
            &format!("catches-{marker}"),
            &[(marker, manifest), (source_path, source)],
        );
        project.run(&["init"]);
        let output = project.run(&["check"]);

        assert_eq!(
            output.status.code(),
            Some(1),
            "{marker} scaffold should report a violation: {}",
            describe(&output)
        );
    }
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

    // The budget is raised on the command line rather than in what `init` wrote, which is the
    // whole file under test here. `lanekeep/no-default-export` is compiled into a 12.4 MiB
    // component, so the first run in a fresh project compiles it — 6 s in a release build on an
    // idle machine, and several times that in a debug build with two dozen of these running at
    // once. That is not the starter config failing to check anything, which is what this test
    // is for.
    let output = project.run(&["check", "--timeout", "600000"]);
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

    // `explain` has no `--timeout`, so the raise goes in the config. What is under test is the
    // starter *rule file* rather than the starter config, and reading any rule's card means
    // loading the whole config first — including the 12.4 MiB component behind
    // `lanekeep/no-default-export`. See `what_init_writes_actually_runs`, which does assert on
    // exactly what `init` wrote.
    let config = project.dir.join("lanekeep.json");
    let written = std::fs::read_to_string(&config).expect("init wrote a config");
    std::fs::write(
        &config,
        written.replacen(
            '{',
            "{\n  \"timeouts\": { \"rule\": 600000, \"global\": 600000},",
            1,
        ),
    )
    .expect("writes config");

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
    std::fs::write(project.dir.join("lanekeep.json"), mine).expect("writes");
    let output = project.run(&["init"]);

    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert_eq!(project.read("lanekeep.json"), mine, "it was overwritten");
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
    std::fs::write(project.dir.join("lanekeep.json"), "// mine\n").expect("writes");

    let output = project.run(&["init", "--force"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(project.read("lanekeep.json").contains("\"rules\""));
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

// JSON rather than a `lanekeep.config.ts`: `no-default-export` is compiled into a component
// now, and a component is not a value a module can import. `--profile` measures a rule's query
// and handler time, which is the same either way — and for a component rule it is the first
// thing in the tree that measures one at all.
const CONFIG: &str = r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
     "rules": ["lanekeep/no-default-export"]}"#;

#[test]
fn profile_reports_the_query_handler_split() {
    // The split is the whole point: it tells an author whether their query or their code is
    // the problem, and one total would leave them guessing.
    let project = Project::new(
        "profile",
        &[
            ("lanekeep.json", CONFIG),
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
            ("lanekeep.json", CONFIG),
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
            ("lanekeep.json", CONFIG),
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
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-default-export",
                              {"rule": "lanekeep/no-restricted-imports", "options": {}}]}"#,
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

// --- the gate table: two silences, told apart -----------------------------------------------

/// Matches every identifier the corpus has, so if this rule ever reported anything it would be
/// the gate that stopped it, not the query.
const RULE_GATED: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/gate-excludes-everything',
  severity: 'error',
  card: { message: 'x', remediation: 'n/a', examples: { bad: 'a', good: 'b' } },
  // Names a substring the corpus never contains, so every file is rejected before a parser
  // ever sees it.
  gates: { fileContains: ['THIS_SUBSTRING_NEVER_APPEARS'] },
  query: '(identifier) @id',
  check(ctx, m) { ctx.report(m.id) },
})
";

/// No gate at all — every file reaches its query — and the query names a construct the corpus
/// does not contain, so this rule is silent for a completely different reason than
/// [`RULE_GATED`].
const RULE_EMPTY: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/nothing-to-find',
  severity: 'error',
  card: { message: 'x', remediation: 'n/a', examples: { bad: 'a', good: 'b' } },
  query: '(debugger_statement) @stmt',
  check(ctx, m) { ctx.report(m.stmt) },
})
";

const TWO_SILENT_RULES_CONFIG: &str = r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
    "rules": ["./lanekeep/rules/gate-excludes-everything.ts", "./lanekeep/rules/nothing-to-find.ts"]}"#;

/// The six gate-table counters for one rule's row, in `RuleTiming`'s field order: `path_gated`,
/// `unread`, `cached`, `content_gated`, `language_gated`, `parsed`.
///
/// Parsed from the *second* table — `stderr` split on its own heading first — because the rule
/// id also appears in the query/handler table above it, and a naive whole-stderr search for the
/// id's line would just as happily match that row.
fn gate_row(stderr: &str, id: &str) -> [u64; 6] {
    let table = stderr
        .split("what each rule looked at")
        .nth(1)
        .unwrap_or_else(|| panic!("no gate profile table in stderr: {stderr}"));
    let line = table
        .lines()
        .find(|line| line.trim_start().starts_with(id))
        .unwrap_or_else(|| panic!("no row for {id} in the gate table: {stderr}"));

    let mut numbers = line.split_whitespace().skip(1).map(|token| {
        token
            .parse::<u64>()
            .unwrap_or_else(|e| panic!("not a number ({e}) in row: {line}"))
    });
    std::array::from_fn(|_| {
        numbers
            .next()
            .unwrap_or_else(|| panic!("row is short of six counters: {line}"))
    })
}

#[test]
fn the_profile_tells_a_gated_silence_from_an_empty_one() {
    // Both rules below report zero violations. That is exactly the case the old profile could
    // not speak to: `query`/`handler`/`matches` all read the same — near-zero query time, no
    // matches — whether a rule's gate ate every file or its query found nothing among files it
    // fully read. Telling those two silences apart is the entire point of the six new
    // counters, so asserting only "both are silent" would assert nothing about this change.
    //
    // Swapping `content_gated` and `parsed` in `write_profile`'s column order, or dropping
    // either counter from the row, makes this fail: `gate-excludes-everything` would show
    // `parsed: 1` instead of `content_gated: 1`, or `nothing-to-find` would show
    // `content_gated: 1` instead of `parsed: 1`.
    let project = Project::new(
        "profile-gate-distinction",
        &[
            ("lanekeep.json", TWO_SILENT_RULES_CONFIG),
            ("lanekeep/rules/gate-excludes-everything.ts", RULE_GATED),
            ("lanekeep/rules/nothing-to-find.ts", RULE_EMPTY),
            // Has identifiers (so RULE_GATED's query would match if ever admitted), no
            // `debugger` statement (so RULE_EMPTY's query has nothing to find), and none of
            // RULE_GATED's excluded substring.
            ("src/a.ts", "export const x = 1;\n"),
        ],
    );

    let output = project.run(&["check", "--profile", "--no-cache"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{combined}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let gated = gate_row(&stderr, "local/gate-excludes-everything");
    let empty = gate_row(&stderr, "local/nothing-to-find");

    // [path_gated, unread, cached, content_gated, language_gated, parsed]
    assert_eq!(gated, [0, 0, 0, 1, 0, 0], "gated row: {stderr}");
    assert_eq!(empty, [0, 0, 0, 0, 0, 1], "empty row: {stderr}");

    // The trailing line is the reconciliation check for a reader — one file discovered, and
    // every rule's six counters must sum to it.
    assert!(
        stderr.contains("each row sums to 1 files discovered"),
        "{stderr}"
    );
}
