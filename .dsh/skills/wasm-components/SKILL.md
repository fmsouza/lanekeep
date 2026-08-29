---
name: wasm-components
description: How lanekeep's WebAssembly rule components are built, embedded, and published (and why they are no longer committed to git).
---

# lanekeep WASM components (build/publish)

on 2026-08-22: the `.wasm` components under `crates/lanekeep-rules/components/` were un-committed
(PR #126's committed binaries became build-time artifacts). Key facts to remember:

- **Two categories.** (a) Shipped built-ins — `go-builtins`, `no-glob-import`, `no-unwrap` —
  embedded into the published binary via `include_bytes!("../components/…")` in
  `crates/lanekeep-rules/src/lib.rs`. (`typescript-builtins.wasm`(+`.map`) is NOT shipped since
  #147's revert: built when missing for tests/benches only, excluded from the `.crate`.)
  (b) Self-check components — 6 Rust rules, loaded by path from `lanekeep.json` and
  `tests/*.rs` — plus the 4-rule Python component (2 self-check rules and Python ports of 2
  shipped built-ins) exercised only by `crates/lanekeep-rules/tests/python_rules.rs`, never
  wired into the config; none ship.
- **`crates/lanekeep-rules/build.rs` builds the 3 shipped components from source when missing**
  (and `typescript-builtins` best-effort, for the tests that read it), and is a strict no-op
  when present (so `cargo publish`'s verify build and a fresh clone with the files present
  never reach for cargo-component/Node/TinyGo). It reproduces `just rust-rules` /
  `just go-rules` / `just typescript-builtins` exactly.
- **`build.rs` must emit `cargo:rerun-if-changed` on the OUTPUT `.wasm` paths too**, not only on
  sources. Otherwise deleting a `.wasm` (as `git checkout` of this change does) leaves cargo's
  incremental build skipping build.rs, and `include_bytes!` fails with "file not found".
- **`components/` is gitignored**, so `crates/lanekeep-rules/Cargo.toml` needs an explicit
  `include = ["components/**", …]` or `cargo publish` (no `--no-verify`) drops them and the verify
  build fails. `cargo package --list` is the local check that the `.wasm` ride in the tarball.
- **CI**: `ci.yml`'s `components` job builds all lanes, uploads `components/`, and `gate`/`test`
  download it before `just check`. `release.yml` has the same `components` job feeding `build`
  (embed) and `publish` (package).
- **Reproducibility**: Rust (`cargo component`) and Go (TinyGo) components are byte-reproducible on
  pinned toolchains; `componentize-js` (typescript-builtins) is NOT — three builds of one tree gave three
  distinct sizes with ~2.9 MB of content differing between two of them (the `wizer` heap
  image; AGENTS.md has the dated measurement). The Go `go-maporder.wasm` fixture is still committed and keeps a digest currency check in
  `fixture_currency.rs` (`go-component-digests.txt`).
- **`lanekeep.json`** allows `crates/lanekeep-rules/build.rs` in `no-unwrap` and
  `no-ambient-authority` (a build script legitimately shells out / unwraps).

Traps hit: a subagent implementing the 8 self-check test migrations produced non-compiling
`assert_every_case` closures (mixed `return;` vs `return Ok(());`, stray `.map_err`) and copied
Python-rule prose ("not byte-reproducible", "CARGO_MANIFEST_DIR to project root") that is false for
Rust rules — re-run the compile, never trust a delegated self-report.
