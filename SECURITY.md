# Security policy

## Reporting a vulnerability

**Do not open a public issue for a security vulnerability.**

Report it through GitHub's private vulnerability reporting, which is enabled on this repository:

1. Go to the [Security tab](https://github.com/fmsouza/lanekeep/security).
2. Choose **Report a vulnerability**.

This creates a private advisory visible only to you and the maintainers.

Please include the version or commit affected, a description of the issue, the steps to reproduce
it, and what an attacker gains. A proof of concept is welcome but not required to file.

You can expect an acknowledgement within 72 hours and an assessment within seven days. Fixes are
released as soon as they are ready; the advisory is published once a fix is available, crediting
you unless you prefer otherwise.

## Supported versions

lanekeep is in early development and has not had its first release. Until v1.0, only the latest
published version receives security fixes.

## Threat model

lanekeep is designed to run as a pre-commit hook and inside CI. That makes it a supply-chain
target, and it is the reason the design deliberately has very little attack surface:

- **No code execution.** Rules are data, not code. Nothing is `eval`'d, dynamically loaded, or
  compiled at runtime. There is no plugin system, which means there is nothing to sandbox because
  there is nothing to execute.
- **No network access.** In any mode, for any reason.
- **Constrained filesystem access.** Reads are limited to files matching the resolved `include`
  globs. Writes happen only under `--fix`, only to files that matched, and only within the ranges
  of reported violations.
- **Reviewed rules.** Every built-in rule is reviewed by a maintainer before it ships.

### What we consider a vulnerability

- Any code execution reachable from a config file, a rule definition, or a source file being
  analysed.
- Any network request originating from lanekeep.
- Any read outside the resolved `include` set, or any write outside `--fix`'s permitted ranges.
- Path traversal via `extends:`, `include:`, or a suppression directive.
- Memory-safety failures reachable from untrusted input, including a malformed or deliberately
  crafted cache file.
- A dependency vulnerability reachable from lanekeep's own code paths.

### What we do not

- A rule producing a wrong result. That is a correctness bug — please file it as a normal issue.
- Resource exhaustion from a pathological input file, unless it is disproportionate to the input's
  size.
- Anything requiring an attacker who can already write to the repository being analysed. Such an
  attacker can already modify the source and the CI configuration; lanekeep is not a boundary
  against them.

## Supply chain

- Dependencies are audited in CI with `cargo-audit` and `cargo-deny`, on every pull request and
  daily on a schedule.
- GitHub Actions are pinned to commit SHAs, never to floating tags.
- Releases carry SLSA provenance attestations; npm packages publish with provenance.
- The dependency surface is kept deliberately small — see §11 of
  [`docs/architecture.md`](docs/architecture.md).
