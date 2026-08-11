//! `check-context`'s two tracked-read methods, driven by a real component.
//!
//! The host here is the host: `lanekeep_wasm::host` over a real [`FileAccess`] rooted at a
//! real directory. What is under test is that a rule running as a WebAssembly component
//! reaches exactly the files a rule running under QuickJS reaches, is refused the same ones
//! for the same reasons, and leaves the same dependency record behind. Confinement is a
//! security property and tracking is a cache-correctness one, so two engines disagreeing about
//! either is worse than either answer alone.
//!
//! # What a read answers with no `FileAccess`: it traps
//!
//! `lanekeep-js` installs `readFile` and `fileExists` only when file access was attached, so a
//! rule calling one without it reaches a function that is genuinely absent and gets a
//! `TypeError` — which `lanekeep-engine` turns into `RunError::Rule` and the run fails. WIT has
//! no absent export, so that absence cannot cross; this host answers by failing the call, which
//! is the same refusal by the only means the boundary has. `src/host.rs`'s module header
//! carries the reasoning and what the three alternatives cost.
//!
//! The consequence is what [`a_host_with_no_file_access_refuses_the_call`] asserts, and it
//! asserts on `root_cause()` rather than on the rendered error: wasmtime prefixes a host
//! function's error with a backtrace whose top frame already spells the method name, so
//! `format!("{err:?}").contains("read-file")` passes whatever the host said.
//!
//! # Every expectation is written twice
//!
//! Once as a literal, and once as what `FileAccess` answers when called directly for the same
//! path — see [`the_component_path_refuses_exactly_what_file_access_refuses`]. Neither alone is
//! enough. The literals would still pass if `FileAccess` itself started answering nonsense, and
//! the parity sweep alone would pass if both sides agreed on nonsense.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` is listed because only it fires: nothing here panics
// directly, and an unfulfilled `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::path::PathBuf;
use std::sync::Arc;

use lanekeep_core::files::{FileAccess, ReadError};
use lanekeep_core::tracked::TrackedRead;
use lanekeep_lang::Language;
use lanekeep_lang_js::TypeScript;
use lanekeep_nodes::NodeArena;
use lanekeep_wasm::bindings::{Rule, types};
use lanekeep_wasm::engine;
use lanekeep_wasm::host::{CheckContext, HostState};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, Resource};

/// The component under test, as built by `just wasm-fixtures`.
const READS: &[u8] = include_bytes!("fixtures/reads.wasm");

/// The path the context reports as the file under check.
///
/// Never one of the files read below: `read-file` is how a rule reaches a file *other* than the
/// one it is checking, and a fixture where the two coincided would not show the difference.
const FILE: &str = "src/example.ts";

/// The file under check. Nothing here queries it; a context has to have parsed something.
const SOURCE: &str = "const alpha = 1;\n";

/// A throwaway project directory to root reads at.
///
/// Built under `std::env::temp_dir()` rather than at a written path, for the reason
/// `lanekeep-core`'s own suite does it: `Path::is_absolute` is platform-specific, so a literal
/// takes a different branch on Windows than on Unix and the same test asserts different things
/// on each.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("lanekeep-wasm-reads-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");
        let project = Self { dir };
        for (path, contents) in files {
            project.write(path, contents.as_bytes());
        }
        project
    }

    fn write(&self, path: &str, contents: &[u8]) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates the parent directory");
        }
        std::fs::write(full, contents).expect("writes the file");
    }

    /// A fresh access over the same root, for the assertions that compare the component path
    /// against the direct one.
    fn access(&self) -> FileAccess {
        FileAccess::new(&self.dir)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One instantiated component, one store, and one context lent across every call.
///
/// The context survives between calls deliberately: memoization and the dependency record are
/// properties of a `check-context`, and a harness that rebuilt one per call could not observe
/// either.
struct Harness {
    store: Store<HostState>,
    rule: Rule,
    context: Resource<CheckContext>,
}

impl Harness {
    /// Build a harness whose context has file access, or — with `None` — one that has none.
    fn new(files: Option<FileAccess>) -> Self {
        let engine = engine().expect("the shipped wasmtime configuration builds an engine");
        let component = Component::new(&engine, READS).expect("the fixture is a valid component");
        let mut linker = Linker::new(&engine);
        Rule::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .expect("the real host satisfies every import the world declares");

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        let tree = parser.parse(SOURCE, None).expect("parses");

        let mut context = CheckContext::new(
            NodeArena::new(tree, SOURCE.to_owned()),
            FILE,
            Arc::new(TypeScript),
        );
        if let Some(files) = files {
            context = context.with_file_access(Arc::new(files));
        }

        let mut store = Store::new(&engine, HostState::new());

        // `lanekeep_wasm::engine` enables epoch interruption, and wasmtime starts every store at
        // deadline zero — which has always elapsed. Without this line every call into a guest
        // traps with `wasm trap: interrupt` before running a single instruction. Nothing advances
        // the epoch in this file, so an out-of-reach deadline is what "no limit" means here;
        // `lanekeep_wasm::WasmRuntime` is what arms a real one per invocation, against a ticker.
        store.set_epoch_deadline(u64::MAX / 2);
        let context = store
            .data_mut()
            .push_check_context(context)
            .expect("the resource table accepts a context");
        let rule = Rule::instantiate(&mut store, &component, &linker).expect("instantiates");

        Self {
            store,
            rule,
            context,
        }
    }

    /// Invoke one probe, returning what the *host* saw. An `Err` here is a trap.
    ///
    /// Separate from [`Harness::probe`] because one test needs the error rather than the
    /// reports, and a helper that unwrapped would make that test unwritable.
    fn call(&mut self, probe: &str, args: &[&str]) -> wasmtime::Result<()> {
        // The probe name first, then one entry per argument. Handles are all the root's, which
        // is zero — nothing on this surface reads them, and zero is the handle a truthiness
        // test would discard.
        let captures: Vec<types::MatchEntry> = std::iter::once(probe)
            .chain(args.iter().copied())
            .map(|name| types::MatchEntry {
                name: name.to_owned(),
                node: NodeArena::ROOT,
            })
            .collect();

        match self.rule.call_check(
            &mut self.store,
            0,
            Resource::new_borrow(self.context.rep()),
            &captures,
        )? {
            Ok(()) => Ok(()),
            // A Rust guest has no stack to hand back and reports a failure by trapping, so
            // this probe never takes the world's graceful channel. Folding one into an error
            // rather than ignoring it keeps "an `Err` here is a trap" true of the signature
            // without silently passing a failure off as a success.
            Err(failure) => Err(wasmtime::Error::msg(format!(
                "the guest returned a failure rather than trapping: {failure:?}"
            ))),
        }
    }

    /// Invoke one probe and collect the messages it reported.
    fn probe(&mut self, probe: &str, args: &[&str]) -> Vec<String> {
        self.call(probe, args)
            .expect("check returns without trapping");
        self.store
            .data_mut()
            .check_context_mut(&self.context)
            .expect("the context outlives the call that borrowed it")
            .take_reports()
            .into_iter()
            .map(|report| report.message.unwrap_or_else(|| "<no message>".to_owned()))
            .collect()
    }

    /// What the context would hand a cache as this file's dependencies.
    fn dependencies(&mut self) -> Vec<TrackedRead> {
        self.store
            .data_mut()
            .check_context_mut(&self.context)
            .expect("the context outlives the call that borrowed it")
            .dependencies()
    }
}

/// What `read-file` answered, rendered exactly as the guest renders it.
///
/// A second implementation of the guest's `read_outcome`, which is the point: the parity sweep
/// compares two independently compiled renderings of the same three outcomes.
fn read_outcome(outcome: Result<Option<String>, ReadError>) -> String {
    match outcome {
        Ok(Some(text)) => format!("text({text})"),
        Ok(None) => "none".to_owned(),
        Err(problem) => refusal(&problem),
    }
}

/// What `file-exists` answered, rendered exactly as the guest renders it.
fn exists_outcome(outcome: Result<bool, ReadError>) -> String {
    match outcome {
        Ok(true) => "yes".to_owned(),
        Ok(false) => "no".to_owned(),
        Err(problem) => refusal(&problem),
    }
}

/// Which refusal, and the path it carried.
fn refusal(problem: &ReadError) -> String {
    match problem {
        ReadError::EscapesRoot { path } => format!("escapes-root({path})"),
        ReadError::Absolute { path } => format!("absolute({path})"),
        ReadError::NotText { path } => format!("not-text({path})"),
    }
}

/// An absolute path, built rather than written.
///
/// `/etc/passwd` is absolute on Unix and merely *rooted* on Windows, and `C:\...` is the
/// reverse, so a literal sends the same input down a different branch on each platform.
fn absolute_path() -> String {
    std::env::temp_dir()
        .join("lanekeep-wasm-absolute-read-probe.json")
        .display()
        .to_string()
}

#[test]
fn a_file_inside_the_root_comes_back_as_text() {
    let project = Project::new("inside", &[("config.json", "{}")]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(
        harness.probe("read", &["config.json"]),
        ["read=text({})"],
        "the bytes on disk, not a summary of them"
    );
}

#[test]
fn a_file_in_a_subdirectory_comes_back_too() {
    // Confinement is about the root, not about depth: everything under it is reachable.
    let project = Project::new("nested", &[("pkg/manifest.json", "{\"n\":1}")]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(
        harness.probe("read", &["pkg/manifest.json"]),
        ["read=text({\"n\":1})"]
    );
}

#[test]
fn a_file_that_is_not_there_is_none_rather_than_a_refusal() {
    // The distinction the world's `result<option<string>, read-error>` exists to carry. A rule
    // asking whether a config is present should not have to handle an error to find out, and a
    // host that refused instead would make "no tsconfig" indistinguishable from "you may not
    // look".
    let project = Project::new("absent", &[]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(harness.probe("read", &["nope.json"]), ["read=none"]);
    assert_eq!(harness.probe("exists", &["nope.json"]), ["exists=no"]);
}

#[test]
fn a_path_that_escapes_the_root_is_refused_as_a_value() {
    // Through the `result`'s error case, not as a trap. `Harness::probe` expects the call to
    // return, so a host that trapped here fails this test on that expectation rather than on
    // the rendering.
    let project = Project::new("escape", &[("inside.json", "{}")]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(
        harness.probe("read", &["../outside.json"]),
        ["read=escapes-root(../outside.json)"],
        "the case names the reason and the payload is the path as the rule wrote it"
    );
    assert_eq!(
        harness.probe("exists", &["../../etc/passwd"]),
        ["exists=escapes-root(../../etc/passwd)"],
        "both methods refuse, and neither answers `no` — which would say the file is not there"
    );
}

#[test]
fn an_absolute_path_is_refused_as_its_own_case() {
    // A different case from `escapes-root`, because it is a different mistake: an absolute path
    // makes a rule's answer depend on where the project is checked out, whether or not it
    // happens to point outside the root.
    let project = Project::new("absolute", &[]);
    let mut harness = Harness::new(Some(project.access()));
    let path = absolute_path();

    assert_eq!(
        harness.probe("read", &[&path]),
        [format!("read=absolute({path})")]
    );
}

#[test]
fn a_file_that_is_not_text_is_refused_but_still_exists() {
    // Two different questions with two different answers, which is why `file-exists` is not
    // `read-file(...).is_some()`. Returning `no` here would claim something untrue about the
    // filesystem.
    let project = Project::new("binary", &[]);
    project.write("blob.bin", &[0xff, 0xfe, 0x00]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(
        harness.probe("sweep", &["blob.bin"]),
        ["blob.bin read=not-text(blob.bin) exists=yes"]
    );
}

#[test]
fn a_refusal_does_not_end_the_invocation() {
    // The whole reason the refusal is a `result` rather than a trap: the rule is still running
    // afterwards. The escaping path sits in the middle, so a host that unwound the instance
    // would lose the third line and not merely mislabel the second.
    let project = Project::new("continues", &[("first.json", "1")]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(
        harness.probe("sweep", &["first.json", "../outside.json", "third.json"]),
        [
            "first.json read=text(1) exists=yes",
            "../outside.json read=escapes-root(../outside.json) \
             exists=escapes-root(../outside.json)",
            "third.json read=none exists=no",
        ]
    );
}

#[test]
fn a_read_is_recorded_as_a_dependency_and_so_is_an_absence() {
    // The half that makes a cache *wrong* rather than merely cold is the second one. A rule
    // that asked whether a file was there and was told no has depended on that answer, so the
    // entry has to be invalidated when the file appears — which is what a `None` hash records.
    let project = Project::new("recorded", &[("present.json", "{}")]);
    let mut harness = Harness::new(Some(project.access()));

    harness.probe("read", &["present.json"]);
    harness.probe("read", &["missing.json"]);

    let recorded = harness.dependencies();
    assert_eq!(
        recorded
            .iter()
            .map(|read| (read.path.as_str(), read.hash.is_some()))
            .collect::<Vec<_>>(),
        vec![("missing.json", false), ("present.json", true)],
        "both files, in path order, and the absent one carries no hash"
    );

    // And the hashes themselves, which the shape assertion above cannot see. Compared against
    // a direct `FileAccess` over the same root rather than against a digest computed here: what
    // has to hold is that the component path records what the JavaScript path would have.
    let direct = project.access();
    direct.read("present.json").expect("allowed");
    direct.read("missing.json").expect("allowed");
    assert_eq!(
        recorded,
        direct.dependencies(),
        "the component path records exactly what a direct read records"
    );
}

#[test]
fn a_refused_read_is_not_a_dependency() {
    // It never produced an answer, so there is nothing for a cache to depend on — and a path
    // outside the root is not something a later run could make appear inside it.
    let project = Project::new("refused", &[]);
    let mut harness = Harness::new(Some(project.access()));

    harness.probe("read", &["../outside.json"]);
    harness.probe("exists", &[&absolute_path()]);

    assert!(
        harness.dependencies().is_empty(),
        "a refusal is not an answer, so it is not a dependency"
    );
}

#[test]
fn one_file_is_one_dependency_however_many_times_it_is_read() {
    // Two methods, three spellings, one entry. A dependency list that grew per call would make
    // two cache entries for identical dependency sets compare unequal, which reads as a cache
    // miss with no cause anyone can find.
    let project = Project::new("once", &[("a.json", "{}")]);
    let mut harness = Harness::new(Some(project.access()));

    harness.probe("sweep", &["a.json", "./a.json", "pkg/../a.json"]);

    assert_eq!(
        harness
            .dependencies()
            .iter()
            .map(|read| read.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.json"]
    );
}

#[test]
fn a_second_read_returns_what_the_first_one_saw() {
    // Across two `check` calls over one `check-context`, with the file rewritten in between.
    // A rule that saw a file change under it could report differently on two runs over
    // identical input — the determinism invariant — and the cache would record one of the two
    // hashes with no way to say which answer used it.
    let project = Project::new("memoized", &[("a.json", "before")]);
    let mut harness = Harness::new(Some(project.access()));

    assert_eq!(harness.probe("read", &["a.json"]), ["read=text(before)"]);

    std::fs::write(project.dir.join("a.json"), "after").expect("rewrites the file");

    // Proving the rewrite landed, and this line is not decoration: without it the assertion
    // below passes unchanged against a write that silently did nothing, which is the shape of
    // a test that looks like it checks something and does not.
    assert_eq!(
        project.access().read("a.json").expect("allowed").as_deref(),
        Some("after"),
        "the rewrite reached the disk"
    );

    assert_eq!(
        harness.probe("read", &["a.json"]),
        ["read=text(before)"],
        "the run must see one version of a file"
    );

    // And the dependency records the version that was used, not the one on disk now.
    let before = project.access();
    std::fs::write(project.dir.join("a.json"), "before").expect("restores the file");
    before.read("a.json").expect("allowed");
    assert_eq!(harness.dependencies(), before.dependencies());
}

#[test]
fn the_component_path_refuses_exactly_what_file_access_refuses() {
    // Confinement is a security property, so the two engines must not disagree about what is
    // reachable — a file `lanekeep-wasm` allows and `lanekeep-js` forbids is worse than either
    // answer alone. Every expectation here is computed by calling `FileAccess` directly, so the
    // sweep cannot drift as `FileAccess` grows cases; the literal assertions in the tests above
    // are what stops both sides agreeing on nonsense.
    let project = Project::new("parity", &[("a.json", "{}"), ("sub/b.json", "2")]);
    project.write("blob.bin", &[0xff, 0xfe, 0x00]);

    let absolute = absolute_path();
    let mut paths: Vec<String> = vec![
        "a.json".to_owned(),
        "sub/b.json".to_owned(),
        "./a.json".to_owned(),
        "sub/../a.json".to_owned(),
        "missing.json".to_owned(),
        "blob.bin".to_owned(),
        "../outside.json".to_owned(),
        "../../etc/passwd".to_owned(),
        "sub/../../outside".to_owned(),
        absolute,
    ];

    // Nothing about `escape.json` looks like traversal, which is why the check canonicalizes
    // rather than comparing strings. Unix only, because that is where a symlink can be made
    // without elevated privileges — and the expectations are computed, so a platform with a
    // shorter list still asserts everything on it.
    #[cfg(unix)]
    let outside = {
        let outside = std::env::temp_dir().join("lanekeep-wasm-reads-symlink-target.json");
        std::fs::write(&outside, "secrets").expect("writes the target");
        let _ = std::fs::remove_file(project.dir.join("escape.json"));
        std::os::unix::fs::symlink(&outside, project.dir.join("escape.json"))
            .expect("creates the symlink");
        paths.push("escape.json".to_owned());
        outside
    };

    let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
    let observed = Harness::new(Some(project.access())).probe("sweep", &borrowed);

    let direct = project.access();
    let expected: Vec<String> = paths
        .iter()
        .map(|path| {
            format!(
                "{path} read={} exists={}",
                read_outcome(direct.read(path)),
                exists_outcome(direct.exists(path))
            )
        })
        .collect();

    assert_eq!(observed, expected);

    // Not vacuous: the sweep has to contain a refusal of each kind and a success, or it would
    // be asserting that two implementations agree about nothing in particular.
    for marker in [
        "text({})",
        "none",
        "escapes-root(",
        "absolute(",
        "not-text(",
    ] {
        assert!(
            expected.iter().any(|line| line.contains(marker)),
            "the sweep covers `{marker}`: {expected:?}"
        );
    }

    #[cfg(unix)]
    let _ = std::fs::remove_file(&outside);
}

#[test]
fn a_host_with_no_file_access_refuses_the_call() {
    // WIT has no absent export, so the absence `lanekeep-js` expresses by not installing the
    // functions cannot cross. Failing the call is the same refusal by the only means the
    // boundary has: under QuickJS the `TypeError` becomes `RunError::Rule` and the run fails,
    // and here the trap does the same. What must not happen is an *answer* — `none` would say
    // the file is not there and `false` would say it does not exist.
    let project = Project::new("no-access", &[("a.json", "{}")]);

    for (probe, method) in [("read", "read-file"), ("exists", "file-exists")] {
        let mut harness = Harness::new(None);
        let error = harness
            .call(probe, &["a.json"])
            .expect_err("a host with no file access has no answer to give");

        // `root_cause`, not the rendered error: wasmtime prefixes a host function's error with
        // a backtrace whose top frame is spelled `...[method]check-context.read-file`, so an
        // assertion on `format!("{error:?}")` passes whatever the host actually said.
        let cause = error.root_cause().to_string();
        assert!(
            cause.contains(&format!("check-context.{method}")),
            "the host names the method that could not be answered: {cause}"
        );
        assert!(
            cause.contains("built without file access"),
            "and why, rather than leaving a reader to guess: {cause}"
        );
    }

    // The project exists and holds a file, so nothing above turned on the file being missing.
    assert!(
        project.access().exists("a.json").expect("allowed"),
        "the file the probes asked for is really there"
    );
}

#[test]
fn a_context_with_no_file_access_has_no_dependencies() {
    // Empty because nothing was read and nothing could be — the honest answer to the question
    // the *engine* asks a context. It is not the answer a rule gets, which is the trap above.
    let mut harness = Harness::new(None);
    let _ = harness.call("read", &["a.json"]);

    assert!(harness.dependencies().is_empty());
}
