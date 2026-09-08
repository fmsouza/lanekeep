//! The `tsc` provider's own tests.
//!
//! A file of its own rather than a `#[cfg(test)] mod tests` inside `mod.rs`, so that the
//! `local/no-ambient-observation` exemption `lanekeep.json` grants can name the *tests*
//! instead of the provider. That rule's `allow` is per file and it has no test exemption, so
//! while these lived beside the provider the whole of `mod.rs` was exempt — and a clock read
//! added to the provider's own code would not have been reported. The two reads here are a
//! spawn that was told to be slow and four threads queued behind one sidecar, both of which
//! are timings a test makes rather than an input an answer depends on.
//!
//! It is in `lanekeep/no-unwrap`'s `allow` for the reason `AGENTS.md` records against clippy's
//! own grant: that rule exempts `#[test]` functions and files under a `tests/` directory, and
//! the fixture helpers below are neither — this is `src/`, and the `#[cfg(test)]` sits on the
//! `mod tests;` in `mod.rs` rather than in the text of this file — so an `expect` in a helper
//! is reported while the identical one inside a `#[test]` body is not.

use std::time::Duration;

use lanekeep_core::{AnalysisBudget, FilePath, TypesConfig, TypesProvider};

use super::*;
use crate::provider::{BeginRunError, TypeProvider};

/// Whether the authoring package's `typescript` is installed.
fn tsc_available() -> bool {
    typescript_package().join("package.json").is_file()
}

/// The absolute path of this repository's own `typescript`, for a fixture's `types` block.
fn typescript_package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/lanekeep/node_modules/typescript")
}

/// `moduleResolution` is named rather than left to default because the fixture below
/// imports out of its own `node_modules`, and the default for an `ES2022` target is
/// `classic`, which never looks there.
const STRICT: &str = "{\"compilerOptions\":{\"strict\":true,\"target\":\"ES2022\",\
                      \"module\":\"ESNext\",\"moduleResolution\":\"bundler\"},\
                      \"include\":[\"src\"]}\n";

fn fixture(name: &str) -> PathBuf {
    fixture_with(name, STRICT, &[])
}

/// A fixture with a chosen `tsconfig.json` and any extra files beside it.
///
/// The `tsconfig.json` is a parameter because the whole point of the key tests below is
/// that two projects differing in nothing else must not key the same.
///
/// `src/a.ts` imports out of the fixture's own `node_modules`, and that is load-bearing
/// rather than realism: TypeScript resolves a `node_modules` specifier through
/// `realpath` (`preserveSymlinks` is false), so a root reached through a symlink — which
/// on macOS `std::env::temp_dir()` always is — resolves the dependency to a path *outside*
/// the root as the driver was given it. Without an import there is no realpathed entry, no
/// `..`-prefixed listing row, and the control assertion below cannot see the defect it
/// exists to catch.
fn fixture_with(name: &str, tsconfig: &str, extra: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lanekeep-tsc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("creates the fixture");
    std::fs::create_dir_all(dir.join("node_modules/dep")).expect("creates the dependency");
    std::fs::write(
        dir.join("package.json"),
        "{\"name\":\"f\",\"private\":true}\n",
    )
    .expect("writes package.json");
    std::fs::write(
        dir.join("node_modules/dep/package.json"),
        "{\"name\":\"dep\",\"version\":\"1.0.0\",\"types\":\"index.d.ts\"}\n",
    )
    .expect("writes the dependency manifest");
    std::fs::write(
        dir.join("node_modules/dep/index.d.ts"),
        "export declare const d: number\n",
    )
    .expect("writes the dependency types");
    std::fs::write(dir.join("tsconfig.json"), tsconfig).expect("writes tsconfig.json");
    std::fs::write(
        dir.join("src/a.ts"),
        "import { d } from \"dep\"\nexport const n: number = d\n",
    )
    .expect("writes a source file");
    for (name, contents) in extra {
        std::fs::write(dir.join(name), contents).expect("writes an extra fixture file");
    }
    dir
}

/// Removes a `relative_fixture`'s directory when it goes out of scope, panic or not.
///
/// Cleanup keyed on this process's pid and run only at the *front* of the next call — what
/// `relative_fixture` did before this guard existed — only ever removes what a fixture of the
/// same name is about to rebuild in *this* process; it can never reach what an earlier
/// process, including one that panicked before its own front-cleanup ran, left behind. That
/// leaked a fixture directory under `target/` per test process — 46, measured in one worktree.
/// A `Drop` guard removes the directory the test that built it is actually done with,
/// regardless of how the test ends.
struct RelativeFixture {
    /// The `root` directory a provider is pointed at.
    absolute: PathBuf,
    /// Its parent, `lanekeep-tsc-<name>-<pid>` — what actually has to be removed. `absolute`
    /// itself is one segment short: the extra `root` segment documented above means the
    /// directory a caller uses is not the directory this fixture owns.
    top: PathBuf,
}

impl std::ops::Deref for RelativeFixture {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.absolute
    }
}

impl Drop for RelativeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.top);
    }
}

/// A minimal project under the workspace's `target/`, and the relative path that names it.
///
/// Under `target/` rather than under `std::env::temp_dir()` for one reason: the relative
/// spelling has to be a path this process can actually name, and a temporary directory on
/// macOS is `/var/folders/...`, whose relative form from the crate is a climb through the
/// whole filesystem. A test binary's working directory is its package root, which is asserted
/// rather than assumed — a runner that changed it would otherwise make this test pass by
/// building the fixture somewhere nobody looked.
///
/// **The extra `root` segment is what makes the fixture able to fail.** Applying a relative
/// path twice lands back on the same directory whenever the climb at its front is exactly as
/// deep as the descent at its end — `../../target/x`, resolved from `<workspace>/target/x`,
/// is `<workspace>/target/x` again — so a fixture one level shallower passes against the
/// defect. One level deeper resolves to `<workspace>/target/target/x/root`, which is nowhere.
fn relative_fixture(name: &str) -> (RelativeFixture, PathBuf) {
    let relative = PathBuf::from(format!(
        "../../target/lanekeep-tsc-{name}-{}/root",
        std::process::id()
    ));
    assert_eq!(
        std::env::current_dir().expect("a working directory"),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "this test names its fixture relative to the package root"
    );
    let absolute = Path::new(env!("CARGO_MANIFEST_DIR")).join(&relative);
    let top = absolute
        .parent()
        .expect("the root has a parent — it is one segment short of `absolute`")
        .to_path_buf();
    let _ = std::fs::remove_dir_all(&absolute);
    std::fs::create_dir_all(absolute.join("src")).expect("creates the fixture");
    std::fs::write(
        absolute.join("package.json"),
        "{\"name\":\"relative\",\"private\":true}\n",
    )
    .expect("writes package.json");
    std::fs::write(absolute.join("tsconfig.json"), STRICT).expect("writes tsconfig.json");
    std::fs::write(absolute.join("src/a.ts"), "export const n: number = 1\n")
        .expect("writes a source file");
    (RelativeFixture { absolute, top }, relative)
}

/// A relative `PATH` argument is one project root, not two.
///
/// `spawn` sets the child's working directory to the root *and* passes the driver's path and
/// the root itself as arguments. Both were built by joining onto the root as given, so a
/// relative one was applied twice and node answered `Cannot find module
/// '<root>/<root>/.lanekeep/types-driver-....mjs'` — a message naming a path nobody wrote, for
/// a configuration that is otherwise correct. Measured against the reference corpus, which is
/// where it was found: every `tsc` run there had to be given an absolute `PATH`.
#[test]
fn a_relative_project_root_is_not_applied_twice() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let (_root, relative) = relative_fixture("relative-root");
    let provider = TscProvider::spawn(
        &relative,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts under a relative root");
    provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect("the programs build");
    // The listing is the run key, so a root that resolved to somewhere else would key on
    // nothing: an empty answer folds to a constant every such run would share.
    assert_ne!(provider.programs_hash(), [0; 32]);
    drop(provider);
}

/// The **default** `types.typescript`, which is relative, loads from the project root.
///
/// Every other fixture here names an absolute path, because a throwaway project has no
/// `node_modules` — so the value every project that never configures one uses was covered by
/// nothing. It is resolved by the driver's `createRequire` against the project root's own
/// `package.json`, and the root is given **relatively** here so that "against the project
/// root" is what is being asserted: this test's own working directory has no
/// `node_modules/typescript`, so a specifier anchored anywhere but the root finds nothing.
#[test]
#[cfg(unix)]
fn the_default_relative_typescript_resolves_against_the_project_root() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let (root, relative) = relative_fixture("default-typescript");
    std::fs::create_dir_all(root.join("node_modules")).expect("creates node_modules");
    // Linked rather than copied: the package is tens of megabytes, and a copy per fixture is a
    // cost every run of this suite would pay. `#[cfg(unix)]` because a directory symlink on
    // Windows needs a privilege a test cannot assume.
    std::os::unix::fs::symlink(
        typescript_package()
            .canonicalize()
            .expect("the package is there"),
        root.join("node_modules/typescript"),
    )
    .expect("links the typescript package into the fixture");
    let provider = TscProvider::spawn(
        &relative,
        &TypesConfig {
            provider: TypesProvider::Tsc,
            command: vec!["node".to_owned()],
            ..TypesConfig::default()
        },
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the default `./node_modules/typescript` loads");
    assert!(
        provider.typescript_version().starts_with('5'),
        "got: {}",
        provider.typescript_version()
    );
    drop(provider);
}

/// The per-run key term `begin_run` answers for such a fixture.
fn run_key(name: &str, tsconfig: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let root = fixture_with(name, tsconfig, extra);
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    provider
        .begin_run(
            &|| vec![FilePath::new("src/a.ts")],
            AnalysisBudget::start(Duration::from_mins(2)),
        )
        .expect("the programs build")
}

/// The provider layer of the ad-hoc notice, both ways round.
///
/// A file no `tsconfig.json` under the root claims is typed with this driver's own options
/// — `strict` off — so its answers depend on where the root was pointed rather than on the
/// project. Nothing failed, so it cannot be an error; nothing said so either, which is the
/// defect. The negative half is what makes the notice information rather than noise.
#[test]
fn a_file_no_tsconfig_claims_is_reported_and_an_ordinary_project_is_not() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let claimed = fixture("notice-claimed");
    let provider = TscProvider::spawn(
        &claimed,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect("the programs build");
    assert_eq!(
        provider.notices(),
        Vec::<String>::new(),
        "a project whose own `tsconfig.json` claims the file has nothing to report"
    );

    // The same fixture with its `tsconfig.json` removed: nothing under the root claims the
    // file any more, so it falls to the ad-hoc program.
    let adhoc = fixture("notice-adhoc");
    std::fs::remove_file(adhoc.join("tsconfig.json")).expect("removes the config");
    let provider = TscProvider::spawn(
        &adhoc,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect("the programs build");
    let notices = provider.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("1 file(s)"), "{notices:?}");
    assert!(notices[0].contains("tsconfig.json"), "{notices:?}");
}

/// The `adhoc` list reaches the key, which the listing on its own cannot carry.
///
/// The same file typed with the project's `strict` and typed without it has a byte-identical
/// listing — the listing is paths and content hashes — so without this section a warm run
/// would answer the previous configuration.
///
/// Over synthetic answers rather than over two fixtures, and that is the whole point of the
/// test: the pair of projects this used to run — one with a `tsconfig.json` and one without —
/// have different *listings* too, since one of them lists a `tsconfig.json` and the other does
/// not. Deleting the `adhoc` fold left that version green. Here the listing is held identical
/// by construction, so the inequality below can only be the fold this test is named for.
#[test]
fn the_adhoc_list_is_folded_into_the_key() {
    let listing = serde_json::json!([["src/a.ts", "abc"], ["tsconfig.json", "def"]]);
    let folded = |adhoc: serde_json::Value| {
        fold_programs(&serde_json::json!({ "listing": listing, "adhoc": adhoc }))
            .expect("a well-shaped answer")
    };
    assert_ne!(
        folded(serde_json::json!([])),
        folded(serde_json::json!(["src/a.ts"])),
        "one listing, two configurations: without the `adhoc` fold these are one key"
    );
    assert_eq!(
        folded(serde_json::json!(["src/a.ts"])),
        folded(serde_json::json!(["src/a.ts"])),
        "and the same answer twice is the same key"
    );
}

/// An `adhoc` entry this cannot read is a refusal, not a shorter list.
///
/// The same reasoning as the row decoder's: dropping what it could not understand would fold
/// two different answers to the same bytes, which is a cache key claiming two different
/// programs are one program.
#[test]
fn an_adhoc_entry_that_is_not_a_path_is_refused() {
    let listing = serde_json::json!([["src/a.ts", "abc"]]);
    for bad in [
        serde_json::json!({ "src/a.ts": true }),
        serde_json::json!("src/a.ts"),
        serde_json::json!([7]),
        serde_json::json!(["src/a.ts", null]),
    ] {
        let error = fold_programs(&serde_json::json!({ "listing": listing, "adhoc": bad }))
            .expect_err("unreadable");
        assert!(matches!(error, ProviderError::Refused(_)), "got: {error:?}");
    }
}

/// The extensions the driver has a `ts.ScriptKind` for, and the ones it has none for.
///
/// A table because the two lists are kept in step with `scriptKindOf` in `driver.mjs` by hand:
/// an extension this admits and the driver does not is a request that can only answer nothing,
/// and one the driver types and this refuses is a file the run never builds a program for.
/// Uppercase is in the table because the check lowercases, and a repository with `A.TS` in it
/// is not the place to discover that.
#[test]
fn typed_extension_admits_what_the_driver_can_parse_and_nothing_else() {
    for path in [
        "src/a.ts",
        "src/a.tsx",
        "src/a.mts",
        "src/a.cts",
        "src/a.js",
        "src/a.jsx",
        "src/a.mjs",
        "src/a.cjs",
        "src/a.d.ts",
        "src/A.TS",
    ] {
        assert!(typed_extension(path), "{path} is one the driver parses");
    }
    for path in [
        "README.md",
        "package.json",
        "src/a.css",
        "scripts/a.py",
        "src/ts",
        "src/a.typescript",
    ] {
        assert!(
            !typed_extension(path),
            "{path} reaches no program, so asking about it is work with no answer"
        );
    }
}

fn tsc_config() -> TypesConfig {
    TypesConfig {
        provider: TypesProvider::Tsc,
        command: vec!["node".to_owned()],
        typescript: typescript_package()
            .canonicalize()
            .expect("the package is there")
            .to_string_lossy()
            .replace('\\', "/"),
    }
}

#[test]
fn the_handshake_reports_the_projects_typescript_version() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("hello");
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    assert!(
        provider.typescript_version().starts_with('5'),
        "got: {}",
        provider.typescript_version()
    );
}

#[test]
fn a_command_that_is_not_there_is_unavailable_rather_than_a_timeout() {
    let root = fixture("unavailable");
    let error = TscProvider::spawn(
        &root,
        &TypesConfig {
            provider: TypesProvider::Tsc,
            command: vec!["definitely-not-node".to_owned()],
            ..TypesConfig::default()
        },
        AnalysisBudget::start(Duration::from_secs(5)),
    )
    .expect_err("nothing to spawn");
    assert!(
        matches!(error, ProviderError::Unavailable(_)),
        "got: {error:?}"
    );
    let rendered = error.to_string();
    assert!(
        rendered.contains("`types.command` on PATH"),
        "a command that is not there is a PATH problem: {rendered}"
    );
    assert!(
        !rendered.contains("point `types.typescript`"),
        "nothing here says anything about the package: {rendered}"
    );
}

#[test]
fn the_identity_moves_with_the_typescript_version() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("identity");
    let real = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the real package");

    let stub = root.join("stub-typescript");
    std::fs::create_dir_all(stub.join("lib")).expect("creates the stub");
    std::fs::write(
        stub.join("package.json"),
        "{\"name\":\"typescript\",\"version\":\"0.0.0-fixture\",\"main\":\"lib/typescript.js\"}\n",
    )
    .expect("writes the stub manifest");
    std::fs::write(
        stub.join("lib/typescript.js"),
        "const noop = () => {};\nmodule.exports = { version: '0.0.0-fixture', \
         createProgram: noop, findConfigFile: noop, readConfigFile: noop, \
         parseJsonConfigFileContent: noop, resolveModuleName: noop }\n",
    )
    .expect("writes the stub module");

    let stubbed = TscProvider::spawn(
        &root,
        &TypesConfig {
            typescript: stub.to_string_lossy().replace('\\', "/"),
            ..tsc_config()
        },
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the stub answers hello");

    assert_eq!(stubbed.typescript_version(), "0.0.0-fixture");
    assert_ne!(real.identity(), stubbed.identity());
}

#[test]
fn a_typescript_without_the_compiler_api_is_unloadable_naming_what_is_missing() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("unsupported-api");
    let stub = root.join("stub-typescript");
    std::fs::create_dir_all(stub.join("lib")).expect("creates the stub");
    std::fs::write(
        stub.join("package.json"),
        "{\"name\":\"typescript\",\"version\":\"7.0.2\",\"main\":\"lib/typescript.js\"}\n",
    )
    .expect("writes the stub manifest");
    std::fs::write(
        stub.join("lib/typescript.js"),
        "module.exports = { version: '7.0.2' }\n",
    )
    .expect("writes the stub module");
    let error = TscProvider::spawn(
        &root,
        &TypesConfig {
            typescript: stub.to_string_lossy().replace('\\', "/"),
            ..tsc_config()
        },
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect_err("no compiler API behind it");
    let text = error.to_string();
    // `Unloadable`, not `Unavailable`: the command ran and answered. Both cases used to be
    // one variant, so this rendered the PATH remedy — advice about the one part of the
    // configuration that was already right.
    assert!(
        matches!(error, ProviderError::Unloadable(_)),
        "got: {error:?}"
    );
    assert!(text.contains("7.0.2"), "{text}");
    assert!(text.contains("createProgram"), "{text}");
    assert!(text.contains("point `types.typescript`"), "{text}");
    assert!(!text.contains("`types.command` on PATH"), "{text}");
}

#[test]
fn a_typescript_that_cannot_be_loaded_is_unloadable_naming_the_path() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("unloadable");
    let error = TscProvider::spawn(
        &root,
        &TypesConfig {
            typescript: "./node_modules/typescript".to_owned(),
            ..tsc_config()
        },
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect_err("nothing to load: the fixture has no node_modules");
    let text = error.to_string();
    // The package is what has to move; see the sibling above.
    assert!(
        matches!(error, ProviderError::Unloadable(_)),
        "got: {error:?}"
    );
    assert!(text.contains("./node_modules/typescript"), "{text}");
    assert!(text.contains("pnpm"), "{text}");
    assert!(text.contains("point `types.typescript`"), "{text}");
    assert!(!text.contains("`types.command` on PATH"), "{text}");
}

#[test]
fn the_run_key_moves_when_only_the_tsconfig_moves() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // Measured before the fix: flipping `strict` changed `typeOf` from `string | undefined`
    // to `string` with a byte-identical listing, so a warm run kept answering the previous
    // configuration. A `tsconfig.json` is in no program's `getSourceFiles()`.
    let loose = "{\"compilerOptions\":{\"strict\":false,\"target\":\"ES2022\",\
                 \"module\":\"ESNext\",\"moduleResolution\":\"bundler\"},\
                 \"include\":[\"src\"]}\n";
    let strict = run_key("strict-on", STRICT, &[]);
    let again = run_key("strict-on-again", STRICT, &[]);
    let relaxed = run_key("strict-off", loose, &[]);
    // The control, and it is load-bearing: the two fixtures below differ in their directory
    // names as well as in their configs, so without this the inequality could be the name.
    assert_eq!(
        strict, again,
        "two projects with identical bytes must key identically"
    );
    assert_ne!(
        strict, relaxed,
        "`strict` decides what the compiler answers and must decide the key"
    );
}

#[test]
fn the_run_key_moves_when_only_an_extended_config_moves() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // The `extends` chain is read by the config parser and by nothing else, so it reaches
    // the key only because the parse host's `readFile` is recorded.
    let extending = "{\"extends\":\"./base.json\",\"include\":[\"src\"]}\n";
    let on = "{\"compilerOptions\":{\"strict\":true,\"target\":\"ES2022\",\
              \"module\":\"ESNext\",\"moduleResolution\":\"bundler\"}}\n";
    let off = "{\"compilerOptions\":{\"strict\":false,\"target\":\"ES2022\",\
               \"module\":\"ESNext\",\"moduleResolution\":\"bundler\"}}\n";
    let strict = run_key("extends-on", extending, &[("base.json", on)]);
    let relaxed = run_key("extends-off", extending, &[("base.json", off)]);
    assert_ne!(
        strict, relaxed,
        "an `extends`ed config decides what the compiler answers and must decide the key"
    );
}

#[test]
#[expect(
    unsafe_code,
    reason = "the only way to put a variable in the *parent's* environment, which is what \
              `env_remove` has to be tested against. Sound **only** under nextest, which \
              gives each test its own process: this test is the only lanekeep code in that process, \
              and the one other thread — libtest's, which spawned this one and is blocked \
              on the result channel — reads no environment variable while the test runs, \
              so nothing in the process can observe the change. Under a plain `cargo test`, where the \
              whole suite shares one process, it is a data race against any concurrent \
              `getenv` — the variable is removed a few hundred milliseconds later, before \
              any assertion, which narrows the window and does not close it. `just test` \
              runs nextest; run this test no other way"
)]
fn the_drivers_test_only_delay_is_not_inherited_from_the_parent() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // SAFETY: see the `expect` above — one process per test, nothing before this reads the
    // environment, and nothing has spawned a thread yet.
    //
    // Thirty seconds rather than five, against a ten-second assertion below. The margin is
    // the whole test: a delay only a little longer than a plausibly slow handshake makes
    // this a race between the bug and the machine, and the losing side is a green run on a
    // loaded CI box. Three times the ceiling is a gap no legitimate `hello` closes.
    unsafe { std::env::set_var("LANEKEEP_TSC_DRIVER_DELAY_MS", "30000") };
    let root = fixture("inherited-delay");
    let started = std::time::Instant::now();
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    );
    // SAFETY: as above, and before any assertion, so a failure does not leave it set.
    unsafe { std::env::remove_var("LANEKEEP_TSC_DRIVER_DELAY_MS") };
    let elapsed = started.elapsed();
    provider.expect("the sidecar starts");
    assert!(
        elapsed < Duration::from_secs(10),
        "`hello` took {elapsed:?}, so the parent's delay reached the child"
    );
}

#[test]
fn an_answer_carrying_another_id_is_refused_rather_than_misattributed() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("desync");
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    // The driver answers a line it cannot parse with `id: 0`. That line is now ahead of the
    // next request's own answer in the stream, which is exactly the shape a stray write to
    // stdout takes — and reading it as this request's answer would attribute every later
    // answer to the wrong question, silently, for the rest of the run.
    provider.write_raw("{ not json\n");
    let error = provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect_err("the stale line is not this request's answer");
    let text = error.to_string();
    assert!(matches!(error, ProviderError::Refused(_)), "got: {error:?}");
    assert!(text.contains("answered id 0"), "{text}");
    assert!(text.contains("request id 2"), "{text}");
}

#[test]
fn the_first_failure_is_kept_and_a_later_one_does_not_replace_it() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // The contract Task 9 wires the engine to: every `TypeProvider` method answers "I don't
    // know" on a failure, so without this a run would finish degraded under a valid key.
    let root = fixture("sticky");
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");
    assert_eq!(provider.failure(), None, "nothing has failed yet");

    provider.kill_sidecar();
    let first = provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect_err("the sidecar is gone");
    assert!(matches!(first, ProviderError::Refused(_)), "got: {first:?}");
    assert_eq!(provider.failure(), Some(first.clone()));

    // A second failure, within the same run. The first is still the one kept, because it
    // is the one that explains the rest.
    //
    // Both within one run deliberately: `begin_run` clears the slot, so a second failure
    // raised across one would be the first failure of a new run rather than a later
    // failure of this one — see `a_new_run_clears_the_previous_runs_failure`.
    let second = provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect_err("the sidecar is still gone");
    assert!(
        matches!(second, ProviderError::Refused(_)),
        "got: {second:?}"
    );
    assert_eq!(provider.failure(), Some(first));
}

#[test]
fn a_new_run_clears_the_previous_runs_failure() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // A provider a session holds across runs (plan 6) must not be cancelled forever for a
    // previous run's breach. The failure is sticky *within* a run — that is what makes one
    // broken answer cancel the run it broke — and `begin_run` is where a run begins.
    let root = fixture("cleared-failure");
    let provider = TscProvider::spawn(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
    )
    .expect("the sidecar starts");

    // Planted rather than provoked: every way of provoking a real timeout kills the
    // sidecar, and then the second run fails on its own account and says nothing about
    // whether the first run's record survived.
    provider.remember(&ProviderError::Timeout);
    assert_eq!(
        provider.failure(),
        Some(ProviderError::Timeout),
        "the run that timed out records it"
    );

    provider
        .begin_run(
            &|| vec![FilePath::new("src/a.ts")],
            AnalysisBudget::start(Duration::from_mins(2)),
        )
        .expect("the sidecar is healthy, so a new run begins");
    assert_eq!(
        provider.failure(),
        None,
        "a new run inherited the previous run's cancellation"
    );
    assert_eq!(
        TypeProvider::failure(&provider),
        None,
        "and the engine would still be asking about the old one"
    );
}

#[test]
fn a_listing_that_is_not_a_list_of_pairs_is_refused_rather_than_folded() {
    // Folding an unreadable answer to a constant is a cache key claiming two different
    // programs are one program.
    let good = serde_json::json!([["src/a.ts", "abc"], ["tsconfig.json", "def"]]);
    assert!(fold_programs(&good).is_ok());
    for bad in [
        serde_json::json!({ "src/a.ts": "abc" }),
        serde_json::json!("not a list at all"),
        serde_json::json!([["src/a.ts"]]),
        serde_json::json!([["src/a.ts", "abc", "extra"]]),
        serde_json::json!([["src/a.ts", 7]]),
        serde_json::json!(["src/a.ts"]),
    ] {
        let error = fold_programs(&bad).expect_err("unreadable");
        assert!(matches!(error, ProviderError::Refused(_)), "got: {error:?}");
    }
}

#[test]
fn an_empty_command_is_unavailable_and_creates_no_lanekeep_directory() {
    let root = fixture("empty-command");
    let error = TscProvider::spawn(
        &root,
        &TypesConfig {
            provider: TypesProvider::Tsc,
            command: Vec::new(),
            ..TypesConfig::default()
        },
        AnalysisBudget::start(Duration::from_secs(5)),
    )
    .expect_err("nothing to spawn");
    assert!(
        matches!(error, ProviderError::Unavailable(_)),
        "got: {error:?}"
    );
    assert!(
        !root.join(".lanekeep").exists(),
        "a run that could never start a sidecar left a `.lanekeep/` behind"
    );
}

#[test]
fn a_driver_made_to_sleep_past_the_budget_times_out() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    let root = fixture("timeout");
    let provider = TscProvider::spawn_with_env(
        &root,
        &tsc_config(),
        AnalysisBudget::start(Duration::from_mins(2)),
        &[("LANEKEEP_TSC_DRIVER_DELAY_MS", "2000")],
    )
    .expect("the sidecar starts");
    let error = provider
        .begin_run(
            &|| vec![FilePath::new("src/a.ts")],
            AnalysisBudget::start(Duration::from_millis(200)),
        )
        .expect_err("the request outlives the run's budget");
    assert!(matches!(error, BeginRunError::Timeout(_)), "got: {error:?}");
}

#[test]
fn concurrent_requests_add_service_time_rather_than_waiting_time() {
    if !tsc_available() {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    }
    // One sidecar answers one request at a time, so four workers issuing one request each
    // spend four service times and three of them also spend a queue wait. Charging the
    // wait makes the accumulator grow with the number of rayon workers rather than with
    // the work: measured on the placement this test replaces, fourteen workers each
    // waiting about 200 ms charged 2.866 s against 205 ms of wall clock, so a 60 s budget
    // bounded 60/P seconds of real analysis and the breach named a duration nobody could
    // observe. Service time only, and one sidecar serving the run, makes the sum the wall
    // time the sidecar was busy.
    let root = fixture("concurrent-charge");
    let budget = AnalysisBudget::start(Duration::from_mins(2));
    let provider = TscProvider::spawn_with_env(
        &root,
        &tsc_config(),
        budget.clone(),
        &[("LANEKEEP_TSC_DRIVER_DELAY_MS", "100")],
    )
    .expect("the sidecar starts");
    // The program is built once, here, outside the measurement: the first `programs` pays
    // for `createProgram` as well as for the delay, and that cost is real analysis time
    // that would sit inside the window and blur what it is measuring.
    provider
        .programs(&[FilePath::new("src/a.ts")])
        .expect("the warm-up request is answered");

    let before = budget.spent();
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                provider
                    .programs(&[FilePath::new("src/a.ts")])
                    .expect("the request is answered");
            });
        }
    });
    let wall = started.elapsed();
    let charged = budget
        .spent()
        .checked_sub(before)
        .expect("the accumulator only grows");

    // Four answers at a hundred milliseconds each, so nothing short of 400 ms can be a
    // correct sum. The margins either side are wide on purpose: the floor is the delay the
    // driver really sleeps and the ceiling is generous enough that a loaded machine does
    // not decide the verdict.
    assert!(
        charged >= Duration::from_millis(400),
        "four answers at 100 ms charged only {charged:?}"
    );
    assert!(
        charged <= wall + Duration::from_millis(250),
        "charged {charged:?} against {wall:?} of wall clock, so waiting is being charged: \
         the sum of service times cannot exceed the time the sidecar was busy"
    );
}

#[test]
fn no_provider_error_renders_a_run_of_spaces() {
    // Every variant's message is a multi-line `#[error]` string, and the only way to write one
    // that renders correctly is the `\n  \` continuation form: the trailing backslash eats the
    // source newline and the indentation that follows it, so the two spaces after the `\n` are
    // the only ones in the rendered text. Written without it — a long line, or a `\n` followed
    // by the source's own indentation — the message keeps that indentation mid-sentence, which
    // is invisible in the source and a wall of spaces on a terminal.
    let errors = [
        ProviderError::Unavailable("why".into()),
        ProviderError::Unloadable("why".into()),
        ProviderError::Unwritable("/somewhere/.lanekeep".into()),
        ProviderError::Refused("why".into()),
        ProviderError::Timeout,
    ];
    for error in errors {
        let rendered = error.to_string();
        for line in rendered.lines() {
            // The two-space indent a continuation line opens with is the message's own
            // structure; anything after it is inside a sentence.
            let sentence = line.trim_start();
            assert!(
                !sentence.contains("  "),
                "`{rendered}` carries a run of spaces inside a sentence"
            );
        }
    }
}
