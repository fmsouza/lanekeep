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
//! # What is recorded, and how it stays machine-independent
//!
//! **The toolchain that built the artifact, as version numbers.** `cargo component`, `rustc`,
//! `go`, `tinygo`, `wasm-opt` and `wasm-tools` versions all move the output bytes, and a
//! contributor building through a different one produces different bytes from identical recorded
//! sources. Each manifest records the versions beside the source digests, as `# tool <name>
//! <version>` lines, so a rebuild through a different toolchain is a *named* failure — which
//! tool, which version — rather than an 87-byte diff with no explanation.
//!
//! Only the version *number* is recorded, not the full `--version` string: `go version` prints
//! `go1.26.5 darwin/arm64` on one machine and `go1.26.5 linux/amd64` on another, and only the
//! `1.26.5` is an input to the bytes. Recording the whole string would make the manifest
//! machine-specific and red on every platform change.
//!
//! The comparison is made only against tools that are present: the gate job has no TinyGo,
//! `wasm-opt`, `wasm-tools` or `cargo component`, so it checks the digests and skips the tool
//! versions, while a maintainer's `just go-rules` / `just wasm-fixtures` — and CI's `components`
//! job — has the toolchain and gets the named check.
//!
//! # What is deliberately not covered
//!
//! **That a digest was produced by an actual build.** Running `just wasm-fixtures` records
//! whatever is in the tree, so blessing without rebuilding is still possible. What this rules
//! out is the accident — the edit nobody meant to leave unbuilt — and not a determined hand.
//!
//! # The one fixture that `just wasm-fixtures` does not build
//!
//! `go-maporder.wasm` sits in `tests/fixtures/` beside the fixtures `just wasm-fixtures` builds,
//! and it belongs to `just go-rules` instead. [`GO_FIXTURE_ARTIFACTS`] is what subtracts it from
//! the fixtures walk, and the deciding rule is the one this whole file is about: a digest
//! belongs to the recipe that can rebuild the artifact, and `wasm-fixtures` needs
//! `cargo-component` where this needs TinyGo. Recording it in [`MANIFEST`] would put a TinyGo
//! build behind a recipe that has never heard of one, and would leave every checkout without
//! TinyGo unable to make `just wasm-fixtures` produce a clean tree.

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

/// The header written above the tool versions and digests, explaining the file to whoever
/// meets it in a diff.
const HEADER: &str = "\
# What every committed `.wasm` fixture in this directory was built from.
#
# Written by `just wasm-fixtures`, asserted by `tests/fixture_currency.rs`, and not to be
# edited by hand: a value here that no build produced is exactly the claim this file exists
# to deny. `<path> <blake3>`, sorted, paths relative to `crates/lanekeep-wasm/`.
#
# A line that moves when no artifact did means a fixture's source was changed without
# rebuilding it — run `just wasm-fixtures`.
#
# tool-versions (the toolchain that built the artifacts, asserted by fixture_currency.rs):
#   A version here that differs from the toolchain on PATH names the tool that drifted, not
#   an unexplained byte diff. Rebuild with the pinned toolchain and re-record: `just wasm-fixtures`.
";

/// The fixture `just go-rules` builds, which lives under `tests/fixtures/` all the same.
///
/// It is the one artifact under `tests/fixtures/` that `just wasm-fixtures` does not build, and
/// the reason it is filed here rather than in the body of the fixture test is the rule this file
/// is built on: the recipe that records a digest has to be the recipe that can rebuild the
/// artifact. That recipe globs the directory for Rust crates and needs `cargo-component`; this
/// fixture is Go and needs TinyGo. Recording it in [`MANIFEST`] would leave every checkout
/// without TinyGo unable to make `just wasm-fixtures` produce a clean tree.
///
/// Keyed on the repository root, and read against this crate by stripping [`CRATE_FROM_ROOT`] —
/// one list read two ways rather than two lists to keep in step.
const GO_FIXTURE_ARTIFACTS: &[&str] = &["crates/lanekeep-wasm/tests/fixtures/go-maporder.wasm"];

/// This crate's own path from the repository root, with its trailing separator.
///
/// [`GO_FIXTURE_ARTIFACTS`] names files inside this crate in the *repository's* key space, while
/// the fixtures walk is keyed on this crate. This is what converts between the two, and a path
/// that does not start with it is a path that has moved out of this crate — which the strip
/// refuses rather than silently passing through.
const CRATE_FROM_ROOT: &str = "crates/lanekeep-wasm/";

/// Rewrite the Go fixture's manifest instead of asserting against it.
///
/// Set by `just go-rules` and by nothing else, on the same terms as [`BLESS`]. Distinct from
/// [`BLESS`] because the two recipes rebuild disjoint artifacts.
const GO_BLESS: &str = "LANEKEEP_BLESS_GO_RULES";

/// Where the Go fixture's digests live, relative to this crate.
const GO_MANIFEST: &str = "tests/go-component-digests.txt";

/// The header written above the Go fixture's tool versions and digests.
const GO_HEADER: &str = "\
# What the committed Go fixture was built from: the determinism fixture behind
# `crates/lanekeep-wasm/tests/go_map_order.rs`.
#
# Written by `just go-rules`, asserted by `crates/lanekeep-wasm/tests/fixture_currency.rs`, and
# not to be edited by hand: a value here that no build produced is exactly the claim this file
# exists to deny. `<path> <blake3>`, sorted, paths relative to the repository root.
#
# A line that moves when the fixture did not means a source under `go-rules/`, the SDK it is
# built on, the generated bindings or the world it is built against was changed without
# rebuilding — run `just go-rules`.
#
# tool-versions (the toolchain that built the fixture, asserted by fixture_currency.rs):
#   A version here that differs from the toolchain on PATH names the tool that drifted, not
#   an unexplained byte diff. Rebuild with the pinned toolchain and re-record: `just go-rules`.
";

#[test]
fn every_committed_artifact_is_the_one_its_sources_build() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut computed = digests(
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

    // The Go fixture sits in this directory and is not this recipe's to rebuild, exactly as two
    // shipped components sit in `crates/lanekeep-rules/components/` and are not `just
    // rust-rules`' to rebuild. Subtracted rather than filtered by extension or prefix: a name is
    // a decision somebody made, and a pattern would quietly adopt the next Go fixture into a
    // manifest whose recipe cannot build it.
    for artifact in GO_FIXTURE_ARTIFACTS {
        let key = artifact
            .strip_prefix(CRATE_FROM_ROOT)
            .expect("a Go fixture recorded against this crate's walk is inside this crate");
        assert!(
            computed.remove(key).is_some(),
            "`{artifact}` is not there. It is a fixture `just go-rules` builds and `just \
             wasm-fixtures` cannot, so it is subtracted from this walk — and subtracting \
             something that is absent means the path moved and this manifest has silently taken \
             ownership of an artifact it cannot rebuild."
        );
    }

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

    let tools = tool_versions(&[
        ("rustc", "rustc", &["--version"][..]),
        ("cargo-component", "cargo-component", &["--version"][..]),
    ]);

    reconcile(
        &computed,
        &tools,
        &crate_dir.join(MANIFEST),
        BLESS,
        HEADER,
        "the committed WebAssembly fixtures",
        "just wasm-fixtures",
    );
}

/// The same claim, for the one committed fixture `just go-rules` builds rather than
/// `wasm-fixtures`.
///
/// `go-rules/` holds everything the fixture is compiled from — the fixture package, the SDK it
/// shares, the committed `wit-bindgen-go` bindings, and the module files that pin
/// `go.bytecodealliance.org/cm` — so it is a directory rather than a list of files. Everything
/// in it is a build input by construction. `crates/lanekeep-wasm/wit/` is here because the
/// fixture is built against it with TinyGo's `-wit-package`, so a world edit with no rebuild
/// leaves it satisfying an ABI that no longer exists.
///
/// The fixture itself is named through [`GO_FIXTURE_ARTIFACTS`] and keyed on the repository root,
/// which is why this walk starts from [`repository_root`] where the fixtures walk starts from
/// this crate.
#[test]
fn every_committed_go_fixture_is_the_one_its_sources_build() {
    let root = repository_root();
    let mut roots: Vec<&str> = vec!["crates/lanekeep-wasm/wit", "go-rules"];
    roots.extend_from_slice(GO_FIXTURE_ARTIFACTS);
    let computed = digests(&root, &roots);

    // A named root that is missing already panics inside `digests`, so what is left to check is
    // the directory and the artifact — either could come back empty and agree with an empty
    // manifest. Properties rather than counts: a rule added or removed is a change to make
    // deliberately and not a reason for the gate to go red.
    let sources = counted(&computed, "go-rules/", "go");
    assert!(
        sources > 0
            && computed.contains_key("crates/lanekeep-wasm/tests/fixtures/go-maporder.wasm")
            && computed.contains_key("go-rules/fixtures/maporder/main.go")
            && computed.contains_key("crates/lanekeep-wasm/wit/world.wit"),
        "the walk found {sources} Go sources, and {} the fixture, {} the fixture entry and {} the \
         world: it is no longer looking where the fixture is built from, and has been asserting \
         nothing",
        found(
            &computed,
            "crates/lanekeep-wasm/tests/fixtures/go-maporder.wasm"
        ),
        found(&computed, "go-rules/fixtures/maporder/main.go"),
        found(&computed, "crates/lanekeep-wasm/wit/world.wit"),
    );

    // `wasm-opt` is the one tool whose program is an override: TinyGo honors `$WASMOPT` ahead
    // of the copy in its own root, and the `go-rules` recipe passes it through, so the version
    // captured here has to be the one the recipe actually used.
    let wasmopt = std::env::var("WASMOPT").unwrap_or_else(|_| "wasm-opt".to_owned());
    let tools = tool_versions(&[
        ("go", "go", &["version"][..]),
        ("tinygo", "tinygo", &["version"][..]),
        ("wasm-opt", wasmopt.as_str(), &["--version"][..]),
        ("wasm-tools", "wasm-tools", &["--version"][..]),
    ]);

    reconcile(
        &computed,
        &tools,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(GO_MANIFEST),
        GO_BLESS,
        GO_HEADER,
        "the committed Go fixture",
        "just go-rules",
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
/// The Go fixture's manifest is keyed on paths relative to the repository root rather than to
/// this crate, because its roots are `go-rules/` and `crates/lanekeep-wasm/wit/` — outside this
/// crate — and `../../` in every line would be a manifest nobody can read.
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
/// `tools` is the toolchain captured from the current `PATH` — only the tools that are present,
/// so the gate job (which has no component toolchain) compares nothing here and still checks the
/// digests. `what` and `recipe` are the two halves of the failure message: what went stale, and
/// which recipe makes it current again.
fn reconcile(
    computed: &BTreeMap<String, String>,
    tools: &BTreeMap<String, String>,
    manifest: &Path,
    bless: &str,
    header: &str,
    what: &str,
    recipe: &str,
) {
    if std::env::var_os(bless).is_some() {
        std::fs::write(manifest, render(header, tools, computed))
            .expect("the manifest is writable");
        return;
    }

    let text = std::fs::read_to_string(manifest).expect("the digests manifest is committed");
    let recorded = parse(&text, manifest);
    let recorded_tools = parse_tools(&text, manifest);

    let mut wrong: Vec<String> = Vec::new();

    // Tool versions first: a mismatch here names the tool, which is the whole point of recording
    // them. A tool that is recorded but absent from this `PATH` is skipped rather than compared —
    // the gate job has no component toolchain, and "not installed" is not a version drift.
    for (name, version) in tools {
        match recorded_tools.get(name) {
            Some(known) if known == version => {}
            Some(known) => {
                wrong.push(format!(
                    "  {name}: recorded {known}, this toolchain has {version}"
                ));
            }
            None => wrong.push(format!(
                "  {name}: this toolchain has {version}, but the manifest records no version \
                 for it"
            )),
        }
    }

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
/// the fixtures that is this crate, whose roots are `wit/` — every committed fixture but `spike`
/// names it as their component target, `spike` having a `wit/spike.wit` of its own — and
/// `tests/fixtures/`, which holds both the guest crates and their build output. The `.wasm` files
/// need no separate pass; they sit in a root as ordinary files.
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
/// A package contributes its own `.gitignore` on the way in and those apply for its whole
/// subtree, which is how `target/` and the generated `src/bindings.rs` stay out without this
/// function naming either of them.
///
/// A package is anything with a manifest at its root — `Cargo.toml` or `go.mod`. The second is
/// not symmetry for its own sake: `go-rules/.gitignore` names the artifacts a person produces by
/// running `tinygo build -o` at the module root by hand, and a walk that hashed one of those
/// would report it as "present but was never recorded" — a red gate, on a machine that happens
/// to have a stray file, naming a manifest that is perfectly current.
fn walk(dir: &Path, base: &Path, excluded: &[PathBuf], found: &mut BTreeMap<String, String>) {
    let mut excluded = excluded.to_vec();
    if dir.join("Cargo.toml").is_file() || dir.join("go.mod").is_file() {
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

/// What a package's `.gitignore` keeps out, as absolute paths — for either kind of manifest.
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
fn render(
    header: &str,
    tools: &BTreeMap<String, String>,
    digests: &BTreeMap<String, String>,
) -> String {
    let mut out = String::from(header);
    for (name, version) in tools {
        out.push_str("# tool ");
        out.push_str(name);
        out.push(' ');
        out.push_str(version);
        out.push('\n');
    }
    for (path, digest) in digests {
        out.push_str(path);
        out.push(' ');
        out.push_str(digest);
        out.push('\n');
    }
    out
}

/// The tool versions, back into the map [`render`] wrote them from.
///
/// Only the `# tool <name> <version>` lines; the header prose and the digest lines are left to
/// [`parse`]. The `# tool ` prefix is what keeps the two apart: no header line starts with it.
fn parse_tools(text: &str, path: &Path) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("# tool "))
        .map(|line| {
            let rest = line.trim_start_matches("# tool ").trim();
            assert!(
                rest.split_once(' ').is_some(),
                "{}: `{line}` is not `# tool <name> <version>`. This file is written by the \
                 recipe its own header names and is not meant to be edited by hand; re-record it.",
                path.display()
            );
            let (name, version) = rest.split_once(' ').expect("asserted just above");
            (name.to_owned(), version.to_owned())
        })
        .collect()
}

/// The version number a tool reports, or `None` when it is not installed.
///
/// The version is the first whitespace-delimited token that contains a digit, with any leading
/// non-digit characters stripped — which is how every tool here reports its version: `go version
/// go1.26.5 darwin/arm64` → `1.26.5`, `wasm-opt version 131` → `131`, `wasm-tools 1.255.0` →
/// `1.255.0`, `rustc 1.95.0 (…)` → `1.95.0`. Only the number is kept so the manifest is not
/// machine-specific: the platform half of `go version`'s output is not an input to the bytes.
fn version_of(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .find(|token| token.chars().any(|c| c.is_ascii_digit()))
        .map(|token| {
            token
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .to_owned()
        })
}

/// The tool versions that decide a committed artifact's bytes, for the tools that are present.
///
/// Each entry is `(name, program, args)`. A tool that is not installed is dropped rather than
/// recorded, so the gate job — which has no component toolchain — records nothing here and the
/// assert path skips the comparison, while a maintainer's recipe has the toolchain and gets the
/// named check.
fn tool_versions(tools: &[(&str, &str, &[&str])]) -> BTreeMap<String, String> {
    tools
        .iter()
        .filter_map(|(name, program, args)| {
            version_of(program, args).map(|version| ((*name).to_owned(), version))
        })
        .collect()
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
