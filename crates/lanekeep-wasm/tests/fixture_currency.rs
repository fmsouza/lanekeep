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
//! Every fixture but `spike` names that directory under `[package.metadata.component.target]`,
//! so the world is a build input to eleven of the twelve committed artifacts. Ten spell the path
//! `../../../wit`; `rejected/wasip1` sits a directory deeper and spells it `../../../../wit`,
//! which is worth knowing before grepping for the three-level form and concluding it is not a
//! consumer. Change the world without rebuilding and the fixtures still load, still instantiate
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

#[test]
fn every_committed_artifact_is_the_one_its_sources_build() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let computed = digests(&crate_dir, &["wit", "tests/fixtures"]);

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
#[test]
fn every_committed_rule_component_is_the_one_its_sources_build() {
    let root = repository_root();
    let computed = digests(
        &root,
        &[
            "crates/lanekeep-wasm/wit",
            "rust-rules",
            "crates/lanekeep-rules/components",
        ],
    );

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
    );
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
fn reconcile(
    computed: &BTreeMap<String, String>,
    manifest: &Path,
    bless: &str,
    header: &str,
    what: &str,
    recipe: &str,
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
                 not belong in the tree"
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
/// the fixtures that is this crate, whose two roots are `wit/` — eleven of the twelve fixtures name
/// it as their component target — and `tests/fixtures/`, which holds both the guest crates and
/// their build output. The `.wasm` files need no separate pass; they sit in a root as ordinary
/// files.
fn digests(base: &Path, roots: &[&str]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for root in roots {
        let dir = base.join(root);
        assert!(dir.is_dir(), "{} is not there", dir.display());
        walk(&dir, base, &[], &mut found);
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
