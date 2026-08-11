//! Whether each committed `.wasm` fixture is the one its committed sources build.
//!
//! Every other test in this crate loads a prebuilt component out of `tests/fixtures/`, and
//! `just wasm-fixtures` — the recipe that rebuilds them — is deliberately outside every gate so
//! that CI need not install `cargo component`. That trade is the right one and it has a hole in
//! it: editing a fixture's `src/lib.rs` and committing without rebuilding leaves every
//! assertion in this crate pointed at the *previous* binary. Nothing goes red. The suite keeps
//! passing, in full, against a component whose source no longer exists anywhere — and the
//! stronger the fixture-based tests get, the more confidently they assert nothing.
//!
//! `tests/fixture-digests.txt` closes it. `just wasm-fixtures` writes a digest of every source
//! file beside every artifact it builds; this file recomputes them and fails when they
//! disagree. A source edit without a rebuild is then a red gate naming the file that moved.
//!
//! It sits beside this file rather than inside `tests/fixtures/`, which is not tidiness: the
//! walk below covers everything under that directory, so a manifest kept there would be an
//! input to its own digest and could never be written to a value it agreed with.
//!
//! # What is covered, and why it is more than the fixture directories
//!
//! `wit/world.wit` is in here too, and it is the sharpest case rather than an afterthought.
//! Every fixture but `spike` names that directory, so the world is a build input to fourteen of
//! the fifteen committed artifacts. Twelve name it under `[package.metadata.component.target]`
//! and spell the path `../../../wit`; `rejected/wasip1` sits a directory deeper and spells it
//! `../../../../wit`, which is worth knowing before grepping for the three-level form and
//! concluding it is not a consumer. `js-globals` names it on a command line rather than in a
//! manifest — `jco componentize --wit crates/lanekeep-wasm/wit` — so it is a consumer that
//! greps for neither form. Change the world without rebuilding and the fixtures still load, still instantiate
//! and still pass — as components built against an ABI that no longer exists. That is the one
//! staleness a reader is least likely to suspect, because the file that changed is not in
//! `tests/` at all.
//!
//! Recording the world as one entry over-invalidates slightly: `spike` targets its own
//! `wit/spike.wit` and does not care. Over-invalidation costs a rebuild of a directory
//! `just wasm-fixtures` rebuilds wholesale anyway.
//!
//! # What is deliberately not covered
//!
//! **The toolchain that built the artifact.** `cargo component`, `wit-bindgen` and `rustc`
//! versions all move the output bytes, and recording them would make this file machine-specific
//! and red on every upgrade. The artifact digest is here instead, so a rebuild that changes the
//! bytes shows up as a reviewable line rather than a silent binary diff.
//!
//! **That a digest was produced by an actual build.** Running `just wasm-fixtures` records
//! whatever is in the tree, so blessing without rebuilding is still possible. What this rules
//! out is the accident — the edit nobody meant to leave unbuilt — and not a determined hand.
//!
//! # The shipped rule components, on the same terms and in a manifest of their own
//!
//! `crates/lanekeep-rules/components/*.wasm` are built from `rust-rules/` by `just rust-rules`,
//! committed for exactly the reason the fixtures are, and reach the binary through
//! `include_bytes!` in `lanekeep_rules::component`. They are stale in precisely the ways a
//! fixture is: a rule's `src/lib.rs` edited without a rebuild leaves
//! `crates/lanekeep-rules/tests/` asserting against the previous component, and a change to
//! `wit/world.wit` — which both rule crates name as their component target — leaves them
//! satisfying an ABI that no longer exists. The second `#[test]` below covers them with the
//! same walk, the same digests and the same bless protocol.
//!
//! **A second manifest and a second environment variable, and that is load-bearing.** Two
//! recipes rebuild disjoint sets of artifacts: `just wasm-fixtures` the fixtures,
//! `just rust-rules` the rule components. Sharing one manifest between them would mean each
//! recipe re-recording artifacts it did not build — which turns the caveat directly above,
//! that a determined hand can bless without building, into the ordinary result of running
//! either recipe. So each manifest is written only by the recipe that rebuilt what it
//! describes.
//!
//! The two overlap on `wit/world.wit`, deliberately: it is a build input to the fixtures *and*
//! to the rule components, and a change to it has to invalidate both.
//!
//! # And a third, for the one shipped component that is not built from Rust
//!
//! `crates/lanekeep-rules/components/typescript-builtins.wasm` is built by
//! `just typescript-builtins` from TypeScript rule sources, the runtime under
//! `packages/lanekeep/runtime/` and a rolldown configuration — none of which any recipe above
//! knows about. A third recipe rebuilding a disjoint artifact gets a third manifest and a third
//! variable, for the reason the second one exists.
//!
//! **Inputs may be shared between manifests; artifacts may not.** `wit/world.wit` is in all
//! three, and that is right: it is a build input to every component in the tree, and a change to
//! it must send you to every recipe. An *artifact* is different, because exactly one recipe can
//! produce it. Recording `typescript-builtins.wasm` in the rule components' manifest as well
//! would mean that rebuilding it — which changes its bytes every time, since `componentize-js`
//! is not reproducible — leaves that manifest red until somebody runs `just rust-rules`, a
//! recipe about other artifacts entirely that needs `cargo component` and a wasm target to run
//! at all. [`TYPESCRIPT_ARTIFACTS`] is the one list that says which artifacts belong to which,
//! read by the test that owns them and by the test that must not.
//!
//! What that costs, and it is worth stating: `componentize-js` output is **not
//! byte-reproducible** — measured 2026-08-10, three builds from an unchanged tree gave three
//! sizes and three digests — so `AGENTS.md`'s protocol for telling a stale artifact from a
//! current one, rebuild and see that `git status` is clean, cannot be used on it. Digests are
//! the whole of the check here rather than a convenience on top of reproducibility, and they
//! catch staleness in one direction only: blessing re-records without building, so an edited
//! source blessed without a rebuild leaves a stale binary behind a green gate.

// `clippy.toml`'s `allow-expect-in-tests` reaches `#[test]` functions and `#[cfg(test)]`
// modules and nothing else, so the helpers below — which are neither — need the grant
// restating. Only `expect_used` fires: nothing here panics directly, and an unfulfilled
// `expect` attribute is itself an error.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Rewrite the manifest instead of asserting against it.
///
/// Set by `just wasm-fixtures` after it has rebuilt every artifact, and by nothing else. A
/// developer who sets it by hand is recording sources against binaries that were not built from
/// them, which is the state this whole file exists to detect.
const BLESS: &str = "LANEKEEP_BLESS_WASM_FIXTURES";

/// Where the recorded digests live, relative to this crate.
const MANIFEST: &str = "tests/fixture-digests.txt";

/// The header written above the digests, explaining the file to whoever meets it in a diff.
const HEADER: &str = "\
# What every committed `.wasm` fixture in this directory was built from.
#
# Written by `just wasm-fixtures`, asserted by `tests/fixture_currency.rs`, and not to be
# edited by hand: a value here that no build produced is exactly the claim this file exists
# to deny. `<path> <blake3>`, sorted, paths relative to `crates/lanekeep-wasm/`.
#
# A line that moves when no artifact did means a fixture's source was changed without
# rebuilding it — run `just wasm-fixtures`.
";

/// Rewrite the rule-component manifest instead of asserting against it.
///
/// Set by `just rust-rules` and by nothing else. Distinct from [`BLESS`] because the two
/// recipes rebuild disjoint sets of artifacts — see this file's own documentation.
const RULE_BLESS: &str = "LANEKEEP_BLESS_RULE_COMPONENTS";

/// Where the rule components' digests live, relative to this crate.
///
/// Beside the fixtures' manifest rather than under `crates/lanekeep-rules/components/`, for
/// the reason that one is not under `tests/fixtures/`: the walk below covers that directory
/// whole, so a manifest kept there would be an input to its own digest.
const RULE_MANIFEST: &str = "tests/rule-component-digests.txt";

/// The header written above the rule components' digests.
const RULE_HEADER: &str = "\
# What every committed rule component under `crates/lanekeep-rules/components/` was built from.
#
# Written by `just rust-rules`, asserted by `crates/lanekeep-wasm/tests/fixture_currency.rs`,
# and not to be edited by hand: a value here that no build produced is exactly the claim this
# file exists to deny. `<path> <blake3>`, sorted, paths relative to the repository root.
#
# A line that moves when no artifact did means a rule's source, or the world it is built
# against, was changed without rebuilding — run `just rust-rules`.
";

/// Rewrite the TypeScript built-ins' manifest instead of asserting against it.
///
/// Set by `just typescript-builtins` and by nothing else, on the same terms as the two above.
const TYPESCRIPT_BLESS: &str = "LANEKEEP_BLESS_TYPESCRIPT_BUILTINS";

/// Where the TypeScript built-ins' digests live, relative to this crate.
const TYPESCRIPT_MANIFEST: &str = "tests/typescript-component-digests.txt";

/// The header written above the TypeScript built-ins' digests.
const TYPESCRIPT_HEADER: &str = "\
# What the committed TypeScript built-ins component was built from.
#
# Written by `just typescript-builtins`, asserted by
# `crates/lanekeep-wasm/tests/fixture_currency.rs`, and not to be edited by hand: a value here
# that no build produced is exactly the claim this file exists to deny. `<path> <blake3>`,
# sorted, paths relative to the repository root.
#
# A line that moves when the artifact did not means a rule's source, the runtime it is bundled
# with, the world it is built against or the toolchain it is pinned to was changed without
# rebuilding — run `just typescript-builtins`.
";

/// The files under `crates/lanekeep-rules/components/` that `just rust-rules` does not build.
///
/// Read twice, with opposite signs: as the artifact list of
/// [`every_committed_typescript_component_is_the_one_its_sources_build`], and as what
/// [`every_committed_rule_component_is_the_one_its_sources_build`] must leave out of its walk.
/// One list rather than two, so the partition cannot drift into either an artifact in both
/// manifests or an artifact in neither.
///
/// **Two files rather than one, and the second is not a by-product.** The sidecar map is what
/// turns a thrown rule's `entry.js:957:16` back into a line in the author's TypeScript, and it is
/// only correct for the bundle it was generated from — the two are one build's output, produced
/// by one invocation of `jco componentize`, and a map paired with any other component points at
/// arbitrary lines of real files. Recording it here is what makes "rebuilt one without the other"
/// impossible rather than merely unlikely.
///
/// This crate's own directory is the base, so the paths are the repository-relative ones both
/// manifests are keyed on.
const TYPESCRIPT_ARTIFACTS: &[&str] = &[
    "crates/lanekeep-rules/components/typescript-builtins.wasm",
    "crates/lanekeep-rules/components/typescript-builtins.wasm.map",
];

/// Everything the TypeScript built-ins component is built from, other than its own artifact.
///
/// Named files where a directory would drag in something that is not a build input, which is
/// most of them:
///
/// - **The four rule sources, individually.** `crates/lanekeep-rules/rules/` holds eight, and
///   the other four are TypeScript modules that no component is built from. Recording the
///   directory would demand a rebuild — Node and a 13 MiB artifact — after editing a rule this
///   component does not contain.
/// - **`modules/paths.ts`**, which two of the four import through `lanekeep/paths`, and which
///   the bundler inlines.
/// - **`packages/lanekeep/index.js`**, which is where `defineRule` comes from. It is an identity
///   function and it is a build input all the same: every rule calls it, and the bundle carries
///   its body.
/// - **`packages/lanekeep/package.json`**, for the pinned `jco` and `componentize-js` versions.
///   A version bump changes the bytes, the bytes are a `ruleset_hash` input, so a bump that
///   nobody rebuilt against is exactly the staleness this file is about. It also carries the
///   `exports` map and the package's module type, which decide what a specifier means.
/// - **`packages/lanekeep/package-lock.json`**, which is the sharper of the two and not a
///   duplicate of it. What decides these bytes is the whole locked tree — rolldown, the
///   StarlingMonkey build, `wizer`, `binaryen` — and every one of those can move while the two
///   versions in `package.json` stay exactly as they were. A lock refresh is then an artifact
///   nobody rebuilt sitting behind a green manifest, which is the class of failure this file
///   exists for rather than an exception to it. The cost is that `npm install` churn demands a
///   rebuild, and that is the right trade: the churn really did change the toolchain.
/// - **The four runtime files**, not the directory: `runtime/` also holds two `.test.js` files
///   that nothing is built from, and covering it would demand a componentize after editing a
///   JavaScript test. `resolve.js` is in here because the bundle's resolver *is* it —
///   `rolldown.config.mjs` imports it rather than restating the rules.
///
/// `crates/lanekeep-rules/typescript/` is a directory because everything in it, today and
/// later, is a build input by construction: it holds the entry module and the bundler
/// configuration and exists for no other purpose.
const TYPESCRIPT_SOURCES: &[&str] = &[
    "crates/lanekeep-rules/modules/paths.ts",
    "crates/lanekeep-rules/rules/no-circular-imports.ts",
    "crates/lanekeep-rules/rules/no-default-export.ts",
    "crates/lanekeep-rules/rules/no-restricted-imports.ts",
    "crates/lanekeep-rules/rules/no-unused-exports.ts",
    "crates/lanekeep-rules/typescript",
    "crates/lanekeep-wasm/wit",
    "packages/lanekeep/index.js",
    "packages/lanekeep/package-lock.json",
    "packages/lanekeep/package.json",
    "packages/lanekeep/runtime/entry.js",
    "packages/lanekeep/runtime/host.js",
    "packages/lanekeep/runtime/package.json",
    "packages/lanekeep/runtime/resolve.js",
];

#[test]
fn every_committed_artifact_is_the_one_its_sources_build() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let computed = digests(
        &crate_dir,
        &[
            "wit",
            "tests/fixtures",
            // The JavaScript fixture is bundled from these two and from its own `rule.js`,
            // which the walk above already covers. They are outside this crate, so their lines
            // are the only ones in the manifest spelled with a `../../` — the alternative was
            // re-keying every other line against the repository root for the sake of two.
            "../../packages/lanekeep/runtime/host.js",
            "../../packages/lanekeep/runtime/entry.js",
        ],
    );

    // A walk that found nothing would agree with an empty manifest and assert precisely
    // nothing, which is the one way this test could be green while checking no fixture at all.
    // Asserted on what the walk is *for* rather than on a file count: an artifact and the world
    // it was built against are the two things a stale fixture is stale with respect to.
    let artifacts = counted(&computed, "", "wasm");
    assert!(
        artifacts > 1 && computed.contains_key("wit/world.wit"),
        "the walk found {artifacts} artifacts and {} the world: it is no longer looking where \
         the fixtures are, and has been asserting nothing",
        if computed.contains_key("wit/world.wit") {
            "did find"
        } else {
            "did not find"
        }
    );

    reconcile(
        &computed,
        &crate_dir.join(MANIFEST),
        BLESS,
        HEADER,
        "the committed WebAssembly fixtures",
        "just wasm-fixtures",
        None,
    );
}

/// The same claim, for the rule components this build actually ships.
///
/// `crates/lanekeep-rules/components/*.wasm` are `include_bytes!`d into the binary and run
/// against the expectation tables in `crates/lanekeep-rules/tests/`, so a rule whose source
/// moved without a rebuild leaves those tables holding the *previous* component to the current
/// expectations — green, and asserting nothing about the code in the tree.
///
/// Three roots rather than the fixtures' two. `rust-rules/` holds the sources and the lockfile;
/// `crates/lanekeep-rules/components/` holds the artifacts; and `crates/lanekeep-wasm/wit/` is
/// here for the reason it is in the walk above — both rule crates name it under
/// `[package.metadata.component.target]`, so a world edit with no rebuild leaves a shipped rule
/// satisfying an ABI that no longer exists, which is the staleness a reader is least likely to
/// suspect because the file that changed is in neither directory.
///
/// The generated `src/bindings.rs` stays out without being named, exactly as `target/` does:
/// each rule crate's own `.gitignore` is read on the way in, and it excludes both.
///
/// [`TYPESCRIPT_ARTIFACTS`] is dropped from the walk, and it is the only thing in any of these
/// three walks that is named rather than derived. `just rust-rules` does not build it, so
/// recording it here would put a rebuild of the *TypeScript* component behind a run of the
/// *Rust* one — see this file's own documentation for why an artifact belongs to exactly one
/// manifest while an input may be in every one of them. A stray artifact nobody owns is still
/// caught, by [`reconcile`]'s "present but was never recorded".
#[test]
fn every_committed_rule_component_is_the_one_its_sources_build() {
    let root = repository_root();
    let mut computed = digests(
        &root,
        &[
            "crates/lanekeep-wasm/wit",
            "rust-rules",
            "crates/lanekeep-rules/components",
        ],
    );
    for artifact in TYPESCRIPT_ARTIFACTS {
        assert!(
            computed.remove(*artifact).is_some(),
            "`{artifact}` is not there. It is a shipped component that `just rust-rules` does \
             not build, so it is subtracted from this walk — and subtracting something that is \
             absent means the path moved and this manifest has silently taken ownership of an \
             artifact it cannot rebuild."
        );
    }

    // Three claims, one per root, because a walk that stopped looking at any one of them would
    // leave this green while covering less than it says. Properties rather than counts: a rule
    // removed is a change to make deliberately, not a reason for the gate to go red.
    let artifacts = counted(&computed, "crates/lanekeep-rules/components/", "wasm");
    let sources = counted(&computed, "rust-rules/", "rs");
    assert!(
        artifacts > 0 && sources > 0 && computed.contains_key("crates/lanekeep-wasm/wit/world.wit"),
        "the walk found {artifacts} shipped components, {sources} rule sources and {} the \
         world: it is no longer looking where the rule components are, and has been asserting \
         nothing",
        if computed.contains_key("crates/lanekeep-wasm/wit/world.wit") {
            "did find"
        } else {
            "did not find"
        }
    );

    reconcile(
        &computed,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(RULE_MANIFEST),
        RULE_BLESS,
        RULE_HEADER,
        "the committed rule components",
        "just rust-rules",
        Some(
            ". And if it is a component some other recipe builds, it belongs in \
             `TYPESCRIPT_ARTIFACTS` — recording it here makes `just rust-rules` bless an \
             artifact it cannot rebuild, and every rebuild by the recipe that can will then \
             leave this manifest red",
        ),
    );
}

/// The same claim again, for the shipped component built from TypeScript rather than from Rust.
///
/// It is the same staleness with a longer list of ways in. `crates/lanekeep-rules/tests/` runs
/// the four built-ins this component hosts, so a rule source edited without a rebuild leaves
/// those expectations held against the previous binary — but so does an edit to the runtime that
/// assembles `ctx`, to the resolver the bundler resolves imports through, to `defineRule`, to
/// the world, or to the pinned `jco` version. Six directions rather than two, none of them in a
/// directory a reader would think to look at while editing a rule.
///
/// **And here the digests are the whole of the check rather than a second opinion.** For a Rust
/// component `AGENTS.md` offers a stronger protocol — rebuild, and on a consistent tree
/// `git status` is clean — because `cargo component` is byte-reproducible on a pinned toolchain.
/// `componentize-js` is not: three builds from an unchanged tree gave three sizes and three
/// digests, differing in millions of bytes, because the `wizer` snapshot is a heap image whose
/// layout is not stable between processes. So there is no rebuild-and-diff to fall back on, and
/// what these digests cannot see — a source edited, then blessed, without a build — cannot be
/// recovered by any byte-wise means. `crates/lanekeep-wasm/tests/js_globals.rs` is the
/// behavioral check that partly stands in for it.
#[test]
fn every_committed_typescript_component_is_the_one_its_sources_build() {
    let root = repository_root();
    let mut roots: Vec<&str> = TYPESCRIPT_SOURCES.to_vec();
    roots.extend_from_slice(TYPESCRIPT_ARTIFACTS);
    let computed = digests(&root, &roots);

    // A named root that is missing already panics inside `digests`, so what is left to check is
    // the two that are directories — either could come back empty and agree with an empty
    // manifest. Named rather than counted: these are the two files the component is bundled
    // from and the one it is built against, and none of the three is optional.
    //
    // The artifacts are counted through their own list rather than by file extension, because
    // they no longer share one: the component is a `.wasm` and its map is a `.map`, and a count
    // keyed on either would silently stop covering the other.
    let artifacts = TYPESCRIPT_ARTIFACTS
        .iter()
        .filter(|artifact| computed.contains_key(**artifact))
        .count();
    assert!(
        artifacts == TYPESCRIPT_ARTIFACTS.len()
            && computed.contains_key("crates/lanekeep-rules/typescript/entry.ts")
            && computed.contains_key("crates/lanekeep-rules/typescript/rolldown.config.mjs")
            && computed.contains_key("crates/lanekeep-wasm/wit/world.wit"),
        "the walk found {artifacts} components, and {} the entry, {} the bundler configuration \
         and {} the world: it is no longer looking where this component is built from, and has \
         been asserting nothing",
        found(&computed, "crates/lanekeep-rules/typescript/entry.ts"),
        found(
            &computed,
            "crates/lanekeep-rules/typescript/rolldown.config.mjs"
        ),
        found(&computed, "crates/lanekeep-wasm/wit/world.wit"),
    );

    reconcile(
        &computed,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(TYPESCRIPT_MANIFEST),
        TYPESCRIPT_BLESS,
        TYPESCRIPT_HEADER,
        "the committed TypeScript built-ins component",
        "just typescript-builtins",
        None,
    );
}

/// `did find` or `did not find`, for a message that has to name which of several went missing.
fn found(computed: &BTreeMap<String, String>, path: &str) -> &'static str {
    if computed.contains_key(path) {
        "did find"
    } else {
        "did not find"
    }
}

/// The repository root, from this crate's location.
///
/// The rule components' manifest is keyed on paths relative to it rather than to this crate,
/// because two of its three roots are outside this crate entirely and `../../` in every line
/// would be a manifest nobody can read.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("this crate is two directories below the repository root")
        .to_path_buf()
}

/// How many files the walk found under a prefix carrying a given extension.
///
/// `Path::extension` rather than `str::ends_with`, which clippy refuses for the good reason
/// that a suffix comparison is case-sensitive where a file extension is not.
fn counted(computed: &BTreeMap<String, String>, prefix: &str, extension: &str) -> usize {
    computed
        .keys()
        .filter(|path| path.starts_with(prefix))
        .filter(|path| {
            Path::new(path.as_str())
                .extension()
                .is_some_and(|found| found.eq_ignore_ascii_case(extension))
        })
        .count()
}

/// Compare what is in the tree against what was recorded, or rewrite the record.
///
/// `what` and `recipe` are the two halves of the failure message that differ between the
/// manifests: what went stale, and which recipe makes it current again.
///
/// `stray` is a third, and only one manifest needs it. Every complaint here ends by naming
/// `recipe` as the repair, which is right for a stale digest and **wrong for a file that was
/// never recorded at all** when the walk covers a directory another recipe also builds into:
/// `crates/lanekeep-rules/components/` holds artifacts from two recipes, so a new JavaScript
/// component appearing there would be reported by the *Rust* manifest, and running the repair it
/// named would bless it into a manifest that cannot rebuild it — the ownership drift this file's
/// own documentation says must not happen. An error message that sends a reader to the wrong fix
/// is worse than one that says less, so the manifest with that exposure passes the sentence that
/// names the partition.
fn reconcile(
    computed: &BTreeMap<String, String>,
    manifest: &Path,
    bless: &str,
    header: &str,
    what: &str,
    recipe: &str,
    stray: Option<&str>,
) {
    if std::env::var_os(bless).is_some() {
        std::fs::write(manifest, render(header, computed)).expect("the manifest is writable");
        return;
    }

    let recorded = parse(
        &std::fs::read_to_string(manifest).expect("the digests manifest is committed"),
        manifest,
    );

    let mut wrong: Vec<String> = Vec::new();
    for (path, digest) in computed {
        match recorded.get(path) {
            Some(known) if known == digest => {}
            Some(_) => wrong.push(format!("  {path} changed since it was last recorded")),
            None => wrong.push(format!(
                "  {path} is present but was never recorded — if it is not a source, it does \
                 not belong in the tree{}",
                stray.unwrap_or_default()
            )),
        }
    }
    for path in recorded.keys() {
        if !computed.contains_key(path) {
            wrong.push(format!("  {path} is recorded but no longer there"));
        }
    }
    wrong.sort();

    assert!(
        wrong.is_empty(),
        "{what} are not the ones their sources build:\n\n{}\n\n\
         the suite loads those artifacts, so until they are rebuilt it is asserting against \
         binaries that no source in this tree produces. Rebuild and re-record them:\n\n    \
         {recipe}\n",
        wrong.join("\n")
    );
}

/// Every file the committed artifacts are built from, and the artifacts themselves.
///
/// `roots` are relative to `base`, and `base` is what the recorded paths are relative to. For
/// the fixtures that is this crate, whose roots are `wit/` — fourteen of the fifteen fixtures
/// name it as their component target — and `tests/fixtures/`, which holds both the guest crates and
/// their build output. The `.wasm` files need no separate pass; they sit in a root as ordinary
/// files.
/// A root may also name a single file, which is how the JavaScript fixture's two build inputs
/// are covered without dragging their directory in: `packages/lanekeep/runtime/` also holds
/// `resolve.js` and two `.test.js` files, none of which that artifact is built from, and
/// recording the directory would demand a `just wasm-fixtures` — `cargo component` and Node
/// both — after editing a JavaScript test.
fn digests(base: &Path, roots: &[&str]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for root in roots {
        let path = base.join(root);
        if path.is_file() {
            found.insert(slashed(&path, base), digest(&path));
            continue;
        }
        assert!(path.is_dir(), "{} is not there", path.display());
        walk(&path, base, &[], &mut found);
    }
    found
}

/// Descend a directory, recording each file that survives the exclusions in force.
///
/// A cargo package contributes its own `.gitignore` on the way in and those apply for its whole
/// subtree, which is how `target/` and the generated `src/bindings.rs` stay out without this
/// function naming either of them.
fn walk(dir: &Path, base: &Path, excluded: &[PathBuf], found: &mut BTreeMap<String, String>) {
    let mut excluded = excluded.to_vec();
    if dir.join("Cargo.toml").is_file() {
        excluded.extend(exclusions(dir));
    }

    let entries = std::fs::read_dir(dir).expect("a directory this walk reached is readable");
    for entry in entries {
        let path = entry.expect("the directory entry is readable").path();
        if excluded.iter().any(|skip| path.starts_with(skip)) {
            continue;
        }
        if path.is_dir() {
            walk(&path, base, &excluded, found);
        } else {
            found.insert(slashed(&path, base), digest(&path));
        }
    }
}

/// What a package's `.gitignore` keeps out, as absolute paths.
///
/// Read rather than hard-coded so the skip list cannot drift from git's. The parser understands
/// anchored literal paths and nothing else, and says so loudly when it meets anything else: a
/// pattern silently not skipped would make a digest depend on whether the tree has been built,
/// which is a gate that is red on one machine and green on another for no stated reason.
fn exclusions(package: &Path) -> Vec<PathBuf> {
    let path = package.join(".gitignore");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            assert!(
                line.starts_with('/') && !line.contains(['*', '?', '[', ']', '!']),
                "{}: `{line}` is not an anchored literal path, and this parser understands \
                 nothing else. Teach it to match the pattern rather than leaving the entry \
                 unhandled — a file git ignores but this walk hashes moves the digest on \
                 whichever machines happen to have it.",
                path.display()
            );
            package.join(line.trim_start_matches('/').trim_end_matches('/'))
        })
        .collect()
}

/// A file's digest, with line endings folded when the file is text.
///
/// There is no `.gitattributes` here, so a Windows checkout with `core.autocrlf` on holds the
/// same sources under different bytes — the same reason `src/key.rs` normalizes the world
/// before hashing it, and it matters here because CI runs this on Windows. Text is told from
/// binary by git's own test, an embedded NUL, so the `.wasm` artifacts are hashed as they lie
/// and every source is hashed as it reads.
fn digest(path: &Path) -> String {
    let raw = std::fs::read(path).expect("a file this walk reached is readable");
    let bytes = if raw.contains(&0) { raw } else { fold(&raw) };
    blake3::hash(&bytes).to_hex().to_string()
}

/// CRLF to LF, leaving a lone carriage return alone.
fn fold(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().peekable();
    while let Some(&byte) = bytes.next() {
        if byte == b'\r' && bytes.peek() == Some(&&b'\n') {
            continue;
        }
        out.push(byte);
    }
    out
}

/// A path relative to `base`, spelled with forward slashes.
///
/// Windows separates components with a backslash, so a manifest written from the raw path would
/// be a different file on each platform and the gate would be red on whichever one did not
/// write it.
fn slashed(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .expect("the walk started at base")
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The manifest text, header and all.
fn render(header: &str, digests: &BTreeMap<String, String>) -> String {
    let mut out = String::from(header);
    for (path, digest) in digests {
        out.push_str(path);
        out.push(' ');
        out.push_str(digest);
        out.push('\n');
    }
    out
}

/// The manifest, back into the map [`render`] wrote it from.
fn parse(text: &str, path: &Path) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            assert!(
                matches!(line.split_once(' '), Some((file, _)) if !file.is_empty()),
                "{}: `{line}` is not `<path> <digest>`. This file is written by the recipe \
                 its own header names and is not meant to be edited by hand; re-record it.",
                path.display()
            );
            let (file, digest) = line.split_once(' ').expect("asserted just above");
            (file.to_owned(), digest.to_owned())
        })
        .collect()
}
