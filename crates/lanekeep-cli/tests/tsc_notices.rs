//! The stderr notices a `tsc`-backed run prints, and `explain`'s matching line.
//!
//! A notice is information only when it is not noise: a `builtin` run must say nothing about
//! providers at all, which is why every positive assertion below is paired with the negative
//! one in `a_builtin_run_says_nothing_about_providers`.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]
#![expect(
    clippy::print_stderr,
    reason = "a test that finds `typescript` absent has to say so on the terminal: the \
              alternative is a suite reporting a pass for a test it did not run"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

mod corpus;

use corpus::tsc_available;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, config: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-tsc-notices-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("creates dir");
        std::fs::create_dir_all(dir.join("rules")).expect("creates rules dir");

        let project = Self { dir };
        std::fs::write(project.dir.join("lanekeep.config.ts"), config).expect("writes config");
        std::fs::write(project.dir.join("rules/typed.ts"), RULE_TYPED).expect("writes rule");
        std::fs::write(project.dir.join("src/a.ts"), "const a = 1;\n").expect("writes source");
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .args(args)
            .arg(&self.dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("runs the binary")
    }

    /// A git repository over the whole project, one commit, nothing staged.
    ///
    /// `--staged` and `--since` are exercised for their narrowing, not for which files they
    /// name, so an empty index after this is fine — `is_narrowed()` only looks at the flag.
    fn git_init_with_one_commit(&self) {
        self.git(&["init", "--quiet"]);
        self.git(&["config", "user.email", "test@example.com"]);
        self.git(&["config", "user.name", "Test"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", "first"]);
    }

    fn git(&self, args: &[&str]) {
        // AGENTS.md's first trap: under an exported `GIT_DIR` (as a git hook sets it), `git -C
        // <tmpdir> init` initializes the *real* repository rather than this throwaway one, and
        // records it as bare.
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("runs git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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

const RULE_TYPED: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'acme/typed', requires: ['types'], severity: 'error',\n\
      query: '(variable_declarator name: (identifier) @name)',\n\
      card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
      check(ctx, m) { if (ctx.types.typeOf(m.name) !== undefined) ctx.report(m.name); },\n\
    });\n";

const CONFIG_BUILTIN: &str = "import { defineConfig } from 'lanekeep';\n\
    import typed from './rules/typed.ts';\n\
    export default defineConfig({ namespaces: ['acme'], rules: [typed] });\n";

/// `typescript` is the authoring package's, by absolute path with forward slashes, because the
/// fixture project has no `node_modules` of its own (Task 11's `tsc_types_block` reasoning).
fn config_tsc(extra: &str) -> String {
    let typescript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript")
        .canonicalize()
        .map_or_else(
            |_| "./node_modules/typescript".to_owned(),
            |p| p.to_string_lossy().replace('\\', "/"),
        );
    format!(
        "import {{ defineConfig }} from 'lanekeep';\n\
         import typed from './rules/typed.ts';\n\
         export default defineConfig({{ namespaces: ['acme'], rules: [typed], \
         types: {{ provider: 'tsc', typescript: '{typescript}' }}{extra} }});\n"
    )
}

/// A `tsc` config whose `command` can never be spawned, for the refusal path: `command` is
/// `types.command`, so — unlike `config_tsc`'s `extra`, which lands beside `types` for fields
/// such as `timeouts` that are not part of it — this has to sit inside the same object as
/// `provider`.
fn config_tsc_unstartable() -> String {
    "import { defineConfig } from 'lanekeep';\n\
     import typed from './rules/typed.ts';\n\
     export default defineConfig({ namespaces: ['acme'], rules: [typed], \
     types: { provider: 'tsc', command: ['definitely-not-node'] } });\n"
        .to_owned()
}

/// The slow-hook note. Deliberately not gated on `tsc` being installed: it prints at prepare
/// from the *configuration*, before anything is spawned, and it is a project's warning that
/// the hook it just configured is a slow one.
#[test]
fn a_tsc_run_says_it_will_build_the_projects_program() {
    let project = Project::new("slow-hook", &config_tsc(""));
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("types.provider is tsc"),
        "{}",
        describe(&output)
    );
    assert!(stderr.contains("slow hook"), "{}", describe(&output));
}

/// The claim the test above makes in its own doc comment, forced honest: the note is decided
/// from the configuration alone, so it has to print even when `tsc` can never be spawned at
/// all — and it has to print *before* the refusal that follows, not after. Deliberately not
/// gated on Node being available, the same reasoning `lanekeep-engine`'s
/// `an_unstartable_provider_is_silent_when_nothing_requires_types` gives for
/// `definitely-not-node`: it cannot be spawned anywhere, so this needs no toolchain and runs on
/// every platform.
#[test]
fn a_run_says_so_before_refusing_when_tsc_cannot_even_be_spawned() {
    let project = Project::new("no-node", &config_tsc_unstartable());
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let note_at = stderr
        .find("types.provider is tsc")
        .unwrap_or_else(|| panic!("note missing: {}", describe(&output)));
    let refusal_at = stderr
        .find("acme/typed")
        .unwrap_or_else(|| panic!("refusal missing: {}", describe(&output)));
    assert!(
        note_at < refusal_at,
        "the note must print before the refusal it explains: {}",
        describe(&output)
    );
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
}

/// And a builtin run says nothing, which is the half that makes the note information.
#[test]
fn a_builtin_run_says_nothing_about_providers() {
    let project = Project::new("quiet", CONFIG_BUILTIN);
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("types.provider"), "{}", describe(&output));
}

/// `--since` and `--staged` never skip a type-aware rule; under `tsc` they say which rules are
/// about to make a narrowed run expensive. Naming them is the point: "this is slow" is not
/// actionable and "these three rules are why" is.
#[test]
fn a_narrowed_tsc_run_names_the_type_aware_rules_it_still_runs() {
    // The note prints once the provider is built, so a machine without the authoring
    // package's `typescript` (CI's macOS and Windows jobs) reaches the refusal first.
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let project = Project::new("narrowed", &config_tsc(""));
    project.git_init_with_one_commit();
    let output = project.run(&["check", "--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("acme/typed"), "{}", describe(&output));
    assert!(
        stderr.contains("still run"),
        "the note says they run, not that they are skipped: {}",
        describe(&output)
    );
}

/// The negative half of the test above: under `builtin` the whole-program cost the note warns
/// about does not exist — a `builtin` run reads a file's own declared types, so narrowing to
/// `--staged` narrows that work exactly as it narrows everything else. `CONFIG_BUILTIN` already
/// configures `acme/typed`, whose `requires: ['types']` is what would have triggered the note
/// under `tsc`.
#[test]
fn a_builtin_run_narrowed_by_staged_says_nothing_about_providers() {
    let project = Project::new("narrowed-builtin", CONFIG_BUILTIN);
    project.git_init_with_one_commit();
    let output = project.run(&["check", "--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("type-aware"), "{}", describe(&output));
}

/// §5.7's budget bullet, through the binary: a breach cancels the run with exit 2 and names
/// analysis, never a rule. One millisecond is below the handshake, let alone a program build,
/// so this covers the "lowering the budget below a real build" direction as well; the
/// provider-level `Timeout` test in `tsc/mod.rs` covers the other.
#[test]
fn an_analysis_budget_breach_cancels_the_run_naming_analysis() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/.bin/tsc (the gate job covers it)");
        return;
    }
    let project = Project::new("budget", &config_tsc(", timeouts: { analysis: 1 }"));
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(stderr.contains("type analysis"), "{}", describe(&output));
    assert!(
        !stderr.contains("rule `"),
        "names analysis, not a rule: {}",
        describe(&output)
    );
}

/// `rules` and `explain` read a run's metadata; neither needs a compiler to do it.
///
/// Both used to prepare in full: under `types.provider: 'tsc'` that wrote `.lanekeep/driver.mjs`
/// into the project, tried to spawn the sidecar, and — when it could not be started — exited 2
/// out of the capability gate without printing the card or the list at all. Nothing either
/// command prints comes from a provider. Deliberately not gated on Node: `definitely-not-node`
/// cannot be spawned anywhere, which is what makes this run on every machine.
#[test]
fn metadata_commands_need_no_provider_at_all() {
    let project = Project::new("metadata-no-provider", &config_tsc_unstartable());

    let explained = project.run(&["explain", "acme/typed"]);
    assert_eq!(explained.status.code(), Some(0), "{}", describe(&explained));
    assert!(
        String::from_utf8_lossy(&explained.stdout).contains("acme/typed"),
        "{}",
        describe(&explained)
    );

    let listed = project.run(&["rules"]);
    assert_eq!(listed.status.code(), Some(0), "{}", describe(&listed));
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("acme/typed"),
        "{}",
        describe(&listed)
    );

    // And nothing was written into the project. `.lanekeep/` is where the driver is written
    // and where the cache lives; a metadata command that leaves one behind has spawned, or
    // tried to.
    assert!(
        !project.dir.join(".lanekeep").exists(),
        "a metadata command must not write `.lanekeep/`: {}",
        describe(&listed)
    );
}

/// `check --fix` spawns one sidecar, not two.
///
/// A `--fix` run prepares twice: once to find the violations, and once more to re-check what
/// the fixes left behind. The second preparation used to build a second provider — a second
/// Node process and a second copy of every program in the project — for a pass that is a cache
/// miss on the handful of files that changed. Counted rather than timed: the `types.command`
/// below is a wrapper that appends a line per exec and then becomes `node`, so the count is
/// what the operating system actually did.
///
/// Unix only, because the wrapper is a shell script.
#[cfg(unix)]
#[test]
fn a_fix_run_spawns_the_sidecar_once() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    let project = Project::new("fix-one-spawn", CONFIG_BUILTIN);
    let log = project.dir.join("spawns.log");
    let wrapper = project.dir.join("node-wrapper.sh");
    project.write(
        "node-wrapper.sh",
        &format!(
            "#!/bin/sh\nprintf 'spawn\\n' >> '{}'\nexec node \"$@\"\n",
            log.display()
        ),
    );
    std::fs::set_permissions(
        &wrapper,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("makes the wrapper executable");

    // The rule reports and offers a safe fix, so `--fix` really does write and re-check: a
    // run that changes nothing returns before the second preparation and would pass against
    // the very bug this asserts is gone.
    project.write("rules/typed.ts", RULE_TYPED_FIX);
    project.write("src/a.ts", "var a = 1;\n");
    project.write(
        "lanekeep.config.ts",
        &config_tsc_with_command(&wrapper.display().to_string()),
    );

    let output = project.run(&["check", "--fix"]);
    let spawns = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        spawns.lines().count(),
        1,
        "one sidecar for the whole `--fix` run: {}",
        describe(&output)
    );
    assert_eq!(
        project_source(&project),
        "let a = 1;\n",
        "the fix was applied, so the re-check really happened: {}",
        describe(&output)
    );
}

#[cfg(unix)]
fn project_source(project: &Project) -> String {
    std::fs::read_to_string(project.dir.join("src/a.ts")).expect("reads the fixed source")
}

/// A type-aware rule with a safe fix, so `--fix` writes and then re-checks.
#[cfg(unix)]
const RULE_TYPED_FIX: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'acme/typed', requires: ['types'], severity: 'error',\n\
      query: '(variable_declaration) @decl',\n\
      card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
      check(ctx, m) {\n\
        ctx.types.typeOf(m.decl);\n\
        const text = ctx.text(m.decl);\n\
        if (!text.startsWith('var ')) return;\n\
        ctx.report(m.decl, { message: 'm', fix: { node: m.decl, text: text.replace('var ', 'let '), safe: true } });\n\
      },\n\
    });\n";

/// `config_tsc`, with `types.command` naming the wrapper the spawn count is read from.
#[cfg(unix)]
fn config_tsc_with_command(command: &str) -> String {
    let typescript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript")
        .canonicalize()
        .map_or_else(
            |_| "./node_modules/typescript".to_owned(),
            |p| p.to_string_lossy().replace('\\', "/"),
        );
    format!(
        "import {{ defineConfig }} from 'lanekeep';\n\
         import typed from './rules/typed.ts';\n\
         export default defineConfig({{ namespaces: ['acme'], rules: [typed], \
         types: {{ provider: 'tsc', command: ['{command}'], typescript: '{typescript}' }} }});\n"
    )
}

/// And the re-check answers from the **fixed** bytes, not from the program it started with.
///
/// The sibling above pins that `--fix` reuses one sidecar; this pins what that reuse must not
/// cost. A held driver used to hand back the program it already had whenever the set of files
/// it was asked about had not widened, so the second pass typed the pre-fix text: the rule
/// below saw `number` where the file now says `"x"`, and the listing that keyed the run
/// carried the old content hash as well — a cache entry stored under bytes that are gone.
///
/// The rule reports the primitive it was answered rather than testing it, so the assertion is
/// on the type itself and a silent provider cannot pass this: `nothing` is a distinct message
/// from either answer.
///
/// Unix only, on `a_fix_run_spawns_the_sidecar_once`' terms: it shares that test's wrapper
/// scaffolding for nothing but the `types.command` shape, and the two are the pair that covers
/// the reuse — one that it happens, one that it is still correct.
#[cfg(unix)]
#[test]
fn a_fix_runs_recheck_types_the_fixed_bytes() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    let project = Project::new("fix-retypes", CONFIG_BUILTIN);
    project.write("rules/typed.ts", RULE_TYPED_REPORTS_THE_TYPE);
    project.write("src/a.ts", "const a = 1;\n");
    project.write("lanekeep.config.ts", &config_tsc(""));

    let output = project.run(&["check", "--fix"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        project_source(&project),
        "const a = \"x\";\n",
        "the fix was applied, so there is a second pass at all: {}",
        describe(&output)
    );
    // The whole line, naming the file: the rule also runs over the project's own `rules/` and
    // reports a `string` there, so a bare `contains("the type is string")` is satisfied by a
    // file the fix never touched — it passes against the very bug this test is for.
    assert!(
        stdout.contains("src/a.ts:1:7 error [acme/typed] the type is string"),
        "the re-check answered from the pre-fix program: {}",
        describe(&output)
    );
    assert!(
        !stdout.contains("src/a.ts:1:7 error [acme/typed] the type is number"),
        "the report names the type the file had before the fix: {}",
        describe(&output)
    );
}

/// A type-aware rule whose message *is* the type it was answered, and whose fix changes it.
///
/// `a = 1` becomes `a = "x"`, so the declaration's type moves from `number` to `string` — the
/// one thing a re-check reading the pre-fix program cannot say.
#[cfg(unix)]
const RULE_TYPED_REPORTS_THE_TYPE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'acme/typed', requires: ['types'], severity: 'error',\n\
      query: '(variable_declarator name: (identifier) @name) @decl',\n\
      card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
      check(ctx, m) {\n\
        const t = ctx.types.typeOf(m.name);\n\
        const seen = t === undefined ? 'nothing' : (t.primitive ?? t.text);\n\
        const text = ctx.text(m.decl);\n\
        const message = `the type is ${seen}`;\n\
        if (text === 'a = 1') {\n\
          ctx.report(m.name, { message, fix: { node: m.decl, text: 'a = \"x\"', safe: true } });\n\
          return;\n\
        }\n\
        ctx.report(m.name, { message });\n\
      },\n\
    });\n";

/// The ad-hoc notice, end to end, on stderr and not stdout.
///
/// The fixture project has no `tsconfig.json` of its own, so every file it checks is typed with
/// the driver's own options rather than the project's — `strict` off — and the answers depend on
/// where the root was pointed. Nothing failed, so this is a notice; nothing said so before,
/// which is the defect.
#[test]
fn a_run_says_when_files_were_typed_without_a_tsconfig() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    let project = Project::new("adhoc-notice", &config_tsc(""));
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("typed without a `tsconfig.json` under the project root"),
        "{}",
        describe(&output)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("tsconfig.json"),
        "a notice belongs on stderr; stdout carries the report: {}",
        describe(&output)
    );
}

/// And a project whose own `tsconfig.json` claims its files says nothing at all.
#[test]
fn a_run_whose_tsconfig_claims_everything_says_nothing_about_it() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (the gate job covers it)");
        return;
    }
    let project = Project::new("adhoc-quiet", &config_tsc(""));
    project.write(
        "tsconfig.json",
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"src\"]}\n",
    );
    let output = project.run(&["check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("typed without a `tsconfig.json`"),
        "{}",
        describe(&output)
    );
}

/// `explain` gains the matching line, beside the cross-file one it already prints.
#[test]
fn explain_says_a_rule_is_type_aware() {
    let project = Project::new("explain", &config_tsc(""));
    let output = project.run(&["explain", "acme/typed"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The line as it is written, not a substring of it. `contains("type")` was satisfied by
    // the rule's own id, by `typed`, and by half the words on the page — it passed against an
    // `explain` that printed nothing about types at all.
    assert!(
        stdout.contains("This rule asks for types."),
        "{}",
        describe(&output)
    );
    assert!(
        stdout.contains(
            "Which oracle answers is `types.provider`: `builtin` needs no toolchain, `tsc` \
             builds the project's own program."
        ),
        "it names the setting that decides which oracle answers, and what each one costs: {}",
        describe(&output)
    );
}
