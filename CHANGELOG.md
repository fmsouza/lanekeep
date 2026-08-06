# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/fmsouza/lanekeep/compare/v0.6.1...v0.7.0) - 2026-08-06

### Added

- *(wasm)* [**breaking**] the host API in WIT, and lanekeep-wasm ([#103](https://github.com/fmsouza/lanekeep/pull/103))

### Fixed

- *(cli)* honor --timeout ([#91](https://github.com/fmsouza/lanekeep/pull/91))

### Performance

- *(engine)* match every rule in one traversal ([#93](https://github.com/fmsouza/lanekeep/pull/93))

## [0.6.1](https://github.com/fmsouza/lanekeep/compare/v0.6.0...v0.6.1) - 2026-08-03

### Performance

**Every scenario got faster, and nothing about behavior changed.** Measured on a development
machine over the §15 corpus — 2,000 files, 20 rules — and noisy to within about 10%, so read
them as magnitudes:

| Scenario | Before | After |
| --- | --- | --- |
| Cold full run | ~3.3 s | **~2.1 s** |
| Warm, no changes | ~102 ms | **~48 ms** |
| Warm, 1 changed file, `--staged` | ~75 ms | **~25 ms** |

- *(engine)* parse each file once, not once per rule ([#86](https://github.com/fmsouza/lanekeep/pull/86))

  A file admitted by twenty rules was parsed twenty times. The architecture said the queries
  run across a single shared parse; the engine built a parser per rule instead. This is most
  of the cold gain.

- *(engine)* compile rule queries in parallel ([#85](https://github.com/fmsouza/lanekeep/pull/85))

  A tree-sitter query costs a couple of milliseconds to compile and a rule compiles one per
  language it declares, so twenty rules over two languages was ~88 ms before a single file was
  read — more than an entire warm run. This is most of the `--staged` gain.

Nothing here changes a public API, a rule's behavior, or what a run reports. The budgets in
[§15](https://github.com/fmsouza/lanekeep/blob/main/docs/architecture.md) are still not met —
cold is ~2.6× over — and that section now says where the remaining time goes.

## [0.6.0](https://github.com/fmsouza/lanekeep/compare/v0.5.0...v0.6.0) - 2026-08-03

### Added

- *(types)* ship TypeScript definitions for the host API ([#82](https://github.com/fmsouza/lanekeep/pull/82))

  `npm install --save-dev lanekeep` now gives you the binary **and** types, so `ctx`
  autocompletes and a typo'd method is a compile error rather than a rule that throws inside
  the sandbox. They live in the `lanekeep` package because that is the specifier a rule
  imports from — types under any other name are types no editor finds.

  A Go, Python or Rust project can add the npm package as a dev dependency purely for
  authoring; nothing about the checker needs Node.

  The definitions are asserted against the engine's own registration in both directions, so
  they cannot drift into describing a method that does not exist.

**Nothing in this release changes a public Rust API.** It is `0.6.0` rather than `0.5.1` as a
deliberate signal that editor types have arrived, not because anything broke — but cargo reads
a `0.x` minor bump as breaking, so a dependent pinning `lanekeep-core = "0.5"` will need to
widen that requirement.

## [0.5.0](https://github.com/fmsouza/lanekeep/compare/v0.4.0...v0.5.0) - 2026-08-03

### Added

- *(lang)* support Rust ([#78](https://github.com/fmsouza/lanekeep/pull/78))

  Rust joins TypeScript, JavaScript, Python and Go. Two built-in rules ship with it:
  `lanekeep/no-glob-import` and `lanekeep/no-unwrap`.

- *(config)* configure in JSON, and scaffold for the language found ([#72](https://github.com/fmsouza/lanekeep/pull/72))

  **`lanekeep.json` is the new config format.** Rules are still TypeScript — that is what
  makes them programs — but saying *which* rules to run is data, and a Go, Python or Rust
  team should not have to write a `.ts` file for it. The config carries a `$schema` key, so
  editors offer completion and validation with nothing installed.

  `lanekeep init` now detects the project from its manifest and scaffolds to match: the right
  include glob, a starter rule in that language, and a built-in worth having on.

  `lanekeep.config.ts` still works and is unchanged. Both formats compile to the same thing
  before anything reads them, so they cannot differ in behavior.

### Changed

- **`BindingKind` gained two variants**: `module` and `trait`, which Rust needs and the
  earlier languages had no word for. Adding a variant to a public enum breaks an exhaustive
  `match` downstream, which is why this is 0.5.0 rather than 0.4.1.

  No effect on rule authors: `ctx.bindingKind` returns strings, and the new ones are only ever
  produced for Rust files. Only a Rust crate matching on `lanekeep_lang::BindingKind` needs a
  new arm.

### Documentation

- Per-language guides now live in the [wiki](https://github.com/fmsouza/lanekeep/wiki), and
  the README points at them rather than being written around TypeScript
  ([#76](https://github.com/fmsouza/lanekeep/pull/76)).

## [0.4.0](https://github.com/fmsouza/lanekeep/compare/v0.3.2...v0.4.0) - 2026-08-03

### Added

- *(lang)* support Go ([#66](https://github.com/fmsouza/lanekeep/pull/66))

  Go joins TypeScript, JavaScript and Python. Two built-in rules ship with it:
  `lanekeep/no-context-in-struct` and `lanekeep/no-package-init`.

- *(release)* let Go projects pin lanekeep with `go tool` ([#67](https://github.com/fmsouza/lanekeep/pull/67))

  ```sh
  go get -tool github.com/fmsouza/lanekeep/cmd/lanekeep
  go tool lanekeep check ./...
  ```

  The fifth distribution channel, and the first with no publish step of its own — a Go
  module version is a git tag, so the tags this release already pushes are its versions.

### Changed

- **`BindingKind` gained three variants**: `Type`, `Receiver` and `TypeParam`, which Go
  needs and the earlier languages had no word for. Adding a variant to a public enum breaks
  an exhaustive `match` downstream, which is why this is 0.4.0 rather than 0.3.3.

  No effect on rule authors: `ctx.bindingKind` returns strings, and the new ones are only
  ever produced for Go files. Only a Rust crate matching on `lanekeep_lang::BindingKind`
  needs a new arm.

### Fixed

- *(release)* stop proposing a release for the one already in flight ([#65](https://github.com/fmsouza/lanekeep/pull/65))

## [0.3.2](https://github.com/fmsouza/lanekeep/compare/v0.3.1...v0.3.2) - 2026-08-02

### Added

- *(release)* publish to PyPI, so `pip install lanekeep` works ([#62](https://github.com/fmsouza/lanekeep/pull/62))

### Fixed

- *(release)* build the Linux binaries against glibc 2.17 ([#62](https://github.com/fmsouza/lanekeep/pull/62))

  0.3.1's Linux binaries required glibc 2.39 and did not start on Ubuntu 22.04, Debian 12 or
  RHEL 9 — on npm, the releases page and Homebrew alike. Anyone on those platforms should
  upgrade; there is no workaround on 0.3.1 short of building from source.

## [0.3.1](https://github.com/fmsouza/lanekeep/compare/v0.3.0...v0.3.1) - 2026-08-02

### Added

- *(cli)* add --watch ([#56](https://github.com/fmsouza/lanekeep/pull/56))
- *(server)* serve LSP diagnostics over stdio ([#58](https://github.com/fmsouza/lanekeep/pull/58))
- *(server)* serve MCP over stdio ([#59](https://github.com/fmsouza/lanekeep/pull/59))

## [0.3.0](https://github.com/fmsouza/lanekeep/compare/v0.2.1...v0.3.0) - 2026-08-02

### Added

- *(lang)* support Python ([#53](https://github.com/fmsouza/lanekeep/pull/53))

## [0.2.1](https://github.com/fmsouza/lanekeep/compare/v0.2.0...v0.2.1) - 2026-08-02

### Fixed

- *(release)* keep one changelog, at the root ([#51](https://github.com/fmsouza/lanekeep/pull/51))

### Other

- release v0.2.0 ([#48](https://github.com/fmsouza/lanekeep/pull/48))

## [0.2.0](https://github.com/fmsouza/lanekeep/compare/v0.1.1...v0.2.0) - 2026-08-02

### Fixed

- *(engine)* choose the grammar by the file, not by the rule ([#45](https://github.com/fmsouza/lanekeep/pull/45))
