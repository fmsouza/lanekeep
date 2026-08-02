# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
