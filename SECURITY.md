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
target.

Rules are TypeScript programs, so they are executable code by design. The security posture is
therefore about **confinement**, not absence:

- **No ambient authority.** Rules execute in an embedded QuickJS sandbox and can reach only the
  host functions lanekeep exposes. `fs`, `process`, `child_process`, network APIs and dynamic
  `import()` are not restricted — they are absent from the context. A rule inherits none of the
  host process's authority.
- **No network access.** In any mode, for any reason, with no configuration that enables it.
- **Filesystem confinement.** Rules read only through `ctx.readFile`, which is confined to the
  project root and rejects traversal outside it. Writes happen only under `--fix`, only to files
  that matched, and only within the ranges of reported violations.
- **Bounded execution.** A per-invocation execution timeout (1 s default), a global run budget
  (15 s default) and a per-runtime memory ceiling are always enforced and cannot be disabled.
  Turing-complete rules can fail to terminate, and a rule that hangs a pre-commit hook is
  indistinguishable from a broken tool. Breaching any limit cancels the run and exits `2` — a
  checker that could not finish is never reported as one that found nothing.
- **Determinism by construction.** The sandbox withholds `Math.random`, `Date.now` and `new
  Date()`. This is a correctness property as much as a security one — nondeterminism would make
  the cache unsound.
- **Reviewed rules.** Every built-in rule is reviewed by a maintainer before it ships.

### What we consider a vulnerability

- **Sandbox escape.** Any means by which rule code reaches capability outside the documented host
  API — spawning a process, opening a socket, loading native code, or obtaining a reference to a
  host object that was not deliberately exposed.
- Any read outside the project root, or any write outside `--fix`'s permitted ranges, including
  via path traversal in `ctx.readFile`, `include:`, or a suppression directive.
- Any network request originating from lanekeep.
- A rule that evades the per-invocation timeout, the global run budget, or the memory ceiling — or
  that causes a breach to be reported as a clean run rather than cancelling it.
- Code execution reachable from a *source file being analysed*, as opposed to from a rule. Rules
  are trusted-ish by the person who installed them; analysed source is not trusted at all.
- Memory-safety failures reachable from untrusted input, including a malformed or deliberately
  crafted cache file.
- A dependency vulnerability reachable from lanekeep's own code paths.

### What we do not

- A rule producing a wrong result. That is a correctness bug — please file it as a normal issue.
- **A malicious rule doing what rules are allowed to do.** A rule in your repository can report
  misleading violations, read any file under the project root, and consume its resource budget.
  The confinement bounds blast radius and makes third-party rule sets reviewable; it does not make
  unread code safe to run.
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
