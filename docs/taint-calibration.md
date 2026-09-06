# Taint-analysis calibration: the false-positive rate before shipping (#195)

This is the measurement #185 §B.6 committed to: the taint analysis (`flow` / `checkFlow`,
`no-secret-in-string`, #194) is flow-sensitive but neither path- nor field-sensitive, so it
over-approximates by construction. Before the over-approximation is asserted to be acceptable,
it is measured against a real corpus, and the number is published with the discipline
`AGENTS.md` demands: both SHAs, the machine, the exact queries, measured against an immutable
snapshot rather than a working checkout.

**This document has two parts.** Everything from here through the Appendix is the original
#195 measurement, kept as the pre-#217 baseline — it found a dominant false negative and one
fixable false positive that made a false-positive rate premature to publish. See **Re-run
after #217 / #218 (#220)** at the end of this document for a trustworthy re-measurement once
both were closed, and the B4 verdict it makes possible.

## Reproduction

| | |
|---|---|
| Corpus | `perawallet/pera-react-native` @ **`3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd`** (branch `main`, 2026-09-03) |
| Corpus snapshot | `git archive 3b17bb2… \| tar -x` — an immutable extract, **not** a working checkout |
| lanekeep | **`281fb79`** (`feat: taint analysis … (#194) (#215)`), `lanekeep 0.8.1`, `HOST_API_VERSION=5` |
| Toolchain | `rustc 1.95.0`, pinned by `rust-toolchain.toml` |
| Machine | Apple M3 Max, 14 cores, macOS 26.6.2 (Darwin 25.6.0, arm64) |
| Date | 2026-09-05 |
| Scope | `apps/*/src`, `packages/*/src`, `extensions/*/src` (the corpus's own shipped-code globs); excluding `tools/`, `conformance/`, `*.{spec,test,stories}`, test setup. 3,985 files parsed, 0 aborts. |
| Run | `lanekeep check … --no-cache --format json`, **0.47 s wall** (3.51 s user / 14 cores), 30.6 MB peak RSS |

The `no-secret-in-string` rule ships with placeholder queries (`getSecret`/`log`/`redact`)
that do not occur in a real wallet, so a meaningful measurement required queries fitted to this
corpus's actual secret-access and logging surface. Those queries were drafted from an inventory
of the corpus and signed off before the run; they are reproduced in full in the appendix.

## What the number is, and why it is not the number the ticket expected

**4 findings. All 4 are false positives. 0 true positives.** But the headline is not a
100% false-positive rate — n = 4 is far too small to publish a rate, and the reason it is
small is the finding that actually matters:

**The dominant real secret-access pattern is invisible to the v1 analysis — a false
*negative*, not a false positive.** In this corpus a decrypted secret is handed to a
*callback parameter* (`withSecret(id, bytes => …)`, and the `withBackupMnemonic` /
`withBackupAuthSecretKey` / `withBackupEncryptionKey` wrappers) — 35 call sites across 13
files, the primary way secrets are touched. A probe confirmed the source queries *capture* the
callback parameter (7 captures across 6 fixture forms) but the analyzer produces **zero** flows
from any of them, while direct-return and property-read controls in the same fixture report
normally. **The v1 analyzer seeds taint at a source-captured *expression* and propagates it
along def-use; a source capture that lands on a *parameter binding* is not seeded as a tainted
definition, so its uses never reach a sink.** A taint tool that reports "clean" while missing
the corpus's main secret pattern gives false assurance — which is a more serious result than
any false-positive rate.

So the four findings come almost entirely from the two source shapes that *do* work
(property-read, direct-return), and every one is a false positive:

### Four-way classification

| Class | Count | |
|---|---|---|
| True positive | 0 | no genuine secret-into-string flow was surfaced |
| **FP — field-insensitivity** | **3** | `.length` of a secret is not the secret |
| FP — path-insensitivity | 0 | — |
| **FP — other** | **1** | a sanitizer bypassed by compound-sink containment |
| *(False negative — parameter-origin)* | *≥35 sites* | *the `withSecret(…)` class, not surfaced at all* |

**The three field-insensitivity FPs** — `extensions/keystore-chrome/src/keystore/sign.ts:112`:

```ts
const seed = key.privateKey.length === 64 ? key.privateKey.subarray(0, 32) : key.privateKey
if (seed.length !== 32) {
    throw new InvalidKeyDataError(`Ed25519 private key must be 32 bytes (got ${seed.length})`)
}
```

`seed` is (a slice of) the private key, genuinely tainted. But the sink interpolates
`${seed.length}` — the byte *count*, 32 or 64, not the key material. Field-insensitive taint
treats every property read of a tainted value as tainted, so `.length` is flagged. A
field-sensitive analysis would know `.length` carries no key bytes. This is the exact
over-approximation #194's sensitivity table promised (`o.secret` taints `o` entirely). It
reports **three times** at the one sink — once per source read of `key.privateKey` (`:107/:108/:109`)
— which is also a live example of the per-`(source,sink)` reporting granularity: three
byte-identical violations at `sign.ts:112` where a reader wants one.

**The one other FP** — `packages/migrate/src/migrate/migrateLegacyAccount.ts:81`:

```ts
`secretKey=${describeBytes(account.secretKey)}, ` +
```

`describeBytes` is a sanitizer (it returns `'null'` / `'empty'` / `'${length}B'`, never the
bytes) and is in the sanitizer set. The value that reaches the string is therefore clean. The
flow fires anyway because the sink node — the template substitution — *syntactically contains*
the tainted `account.secretKey` read, and the analyzer's containment check treats a source
inside a compound sink expression as reaching the sink without accounting for the sanitizer
wrapping it. This is not a sensitivity dimension; it is the compound-sink limitation flagged as
Minor #2 in #194's final review, now confirmed to fire on real code.

## Perf

Not a concern at this scale. 3,985 files in **0.47 s** wall; the per-sink CFG rebuild that
#194's review flagged as a scaling watch-item did not manifest as a problem on a 113 KLOC
corpus.

## Recommendation: does B4 exist, and what is it?

**B4 is not yet decidable from this run, and the honest reason is the false negative, not the
rate.** Reporting the difference rather than adjusting the expectation (per `AGENTS.md`): the
measurement the ticket asked for cannot be trusted while the dominant source shape produces no
findings. Two pre-B4 correctness gaps must close first, and neither is a new subsystem:

1. **Parameter-origin taint** — seed taint at a source capture that lands on a parameter
   binding, so callback-delivered secrets (`withSecret(bytes => …)`) are tracked. This stays
   intra-procedural; it is a fix to how the existing analyzer seeds a source, not a new
   analysis. Without it the tool is unsound for this corpus in the direction that matters
   (missed flows).
2. **The compound-sink sanitizer bypass** — a source contained in a sink expression should not
   report when an intervening sanitizer wraps it. A confirmed FP bug, independent of B4.

**Then re-run this calibration** at a new pinned pair of SHAs to get an FP rate that means
something.

On the thin evidence available now:

- **Field sensitivity is the more likely B4** than cross-function summaries: field-insensitivity
  caused 3 of the 4 FPs (the "property of a secret that is not itself secret" shape — `.length`,
  and by extension `.byteLength`, `.constructor`, an address checksum, etc.), and it is the
  dimension #194 already names as the one producing user-visible false positives. But three
  findings do not justify committing to an access-path abstraction with a widening bound; the
  re-measurement after fix (1) is what should decide it.
- **Cross-function summaries are not indicated by this corpus.** The secret flows here are
  intra-function (the secret lives and dies inside the `withSecret` callback body). What is
  missing is parameter-origin *seeding*, not cross-function *composition*. No finding here
  argues for the summary machinery.

**Shippability note.** `no-secret-in-string`, even with corpus-fitted queries, is not a
comprehensive secret-leak detector for a codebase built on callback-delivered secrets until
parameter-origin taint lands. It is sound for the source shapes it does handle (direct-return,
property-read), modulo the two FP causes above. This does not block shipping the `flow` /
`checkFlow` capability — it scopes what the built-in rule can currently claim.

## Appendix — the exact queries measured

Fitted to this corpus and signed off before the run. Sources are matched by call/property
*shape*, never by bare identifier text, because the corpus's own analytics event names
(`onb_createacc_pass_begin`, …) contain secret-word fragments and would false-positive a
name-based heuristic — a trap the corpus documents against itself.

**sources** — `withSecret`/`withBackupMnemonic`/`withBackupAuthSecretKey`/`withBackupEncryptionKey`
callback parameter (parenthesized and bare arrow forms); direct returns
`consumePendingImportMnemonic`/`entropyToMnemonic`/`mnemonicIndexToWord`; property reads
`.privateKey`/`.secretKey`/`.mnemonic`.

**sinks** — `logger.{error,warn,info,debug,critical}`, `console.{log,warn,error,debug,info}`,
`JSON.stringify`, template-literal interpolations, `Sentry.{captureException,captureMessage}` /
`analytics.logEvent`, and `ky`/`queryClient` `body`/`json` object-literal fields.

**sanitizers** — `redactSensitiveUrl`/`redactSensitiveContext`/`redactSensitiveValue`/
`redactErrorForReport`/`scrubString`/`scrubEvent`/`scrubLegacyPayloadSecrets`/`describeBytes`,
plus `hashPin`/`pbkdf2` (a PIN hash is not the secret).

### Exact queries

```
;; sources
;; 1a. withSecret family — parenthesized single-param arrow: (bytes) => ...
(call_expression
   function: (identifier) @fn
   (#any-of? @fn "withSecret" "withBackupMnemonic" "withBackupAuthSecretKey" "withBackupEncryptionKey")
   arguments: (arguments (arrow_function
     parameters: (formal_parameters . (required_parameter (identifier) @source)))))
;; 1b. withSecret family — unparenthesized single-param arrow: bytes => ...  (the real corpus form)
(call_expression
   function: (identifier) @fn
   (#any-of? @fn "withSecret" "withBackupMnemonic" "withBackupAuthSecretKey" "withBackupEncryptionKey")
   arguments: (arguments (arrow_function parameter: (identifier) @source)))
;; 2. Direct-return secret accessors.
(call_expression function: (identifier) @fn
   (#any-of? @fn "consumePendingImportMnemonic" "entropyToMnemonic" "mnemonicIndexToWord")) @source
;; 3. Property reads on secret-shaped fields.
(member_expression property: (property_identifier) @prop
   (#any-of? @prop "privateKey" "secretKey" "mnemonic")) @source

;; sinks
(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "logger") (#any-of? @prop "error" "warn" "info" "debug" "critical")
   arguments: (arguments (_) @sink))
(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "console") (#any-of? @prop "log" "warn" "error" "debug" "info")
   arguments: (arguments (_) @sink))
(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "JSON") (#eq? @prop "stringify") arguments: (arguments (_) @sink))
(template_substitution (_) @sink)
(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#any-of? @obj "Sentry" "analytics" "analyticsService")
   (#any-of? @prop "captureException" "captureMessage" "logEvent")
   arguments: (arguments (_) @sink))
(pair key: [(property_identifier) (string)] @key (#any-of? @key "body" "json") value: (_) @sink)

;; sanitizers
(call_expression function: (identifier) @sanitizer
   (#any-of? @sanitizer "redactSensitiveUrl" "redactSensitiveContext" "redactSensitiveValue"
                        "redactErrorForReport" "scrubString" "scrubEvent"
                        "scrubLegacyPayloadSecrets" "describeBytes" "hashPin" "pbkdf2"))
```

**Note, added by the #220 re-run below:** the sanitizer query above captures `@sanitizer` on the
callee `(identifier)`, not on the enclosing `call_expression`. The analyzer's containment checks
match against the whole call node, so a query captured this way never satisfies them — see
**Re-run after #217 / #218 (#220)** for how this was found and fixed. Anyone re-deriving this
query today should capture the whole `call_expression`, not just the identifier.

Two shapes deviate from a naive draft and matter for anyone re-deriving them against
`tree-sitter-typescript@0.23.2`: an unparenthesized single arrow parameter is
`arrow_function parameter: (identifier)` (query 1b), while a parenthesized one is
`formal_parameters . (required_parameter (identifier))` (query 1a) — the corpus uses the bare
form. A `flow.sanitizers` query must bind `@sanitizer` or the engine refuses it at load. Sink
query 6's `(string)` key alternative never matches (a string node's text includes its quotes),
which is harmless — the `property_identifier` alternative matches the common `body:`/`json:`
form.

### To reproduce

1. `git clone https://github.com/perawallet/pera-react-native && cd pera-react-native && git archive 3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd | tar -x -C <snapshot-dir>`
2. Build lanekeep at `281fb79`: `cargo build --release -p lanekeep-cli`.
3. Put a rule (`requires: ['dataflow']`, the `flow` block above, `checkFlow(ctx, path) { ctx.report(path.sink, …) }`) and a `defineConfig` in a subdirectory of `<snapshot-dir>` with `include: ['apps/*/src/**/*.{ts,tsx}', 'packages/*/src/**/*.{ts,tsx}', 'extensions/*/src/**/*.{ts,tsx}']`, `exclude` the test/tool/conformance globs listed under Scope, and `namespaces: ['calib']`. (Rule-module resolution is confined to the project root, so the rule must live under `<snapshot-dir>`.)
4. `lanekeep check <snapshot-dir> --config <that-config> --no-cache --format json`.

The `Violation` the reporters serialize carries no `FlowPath`, so the source location, matched
role and step count reach machine-readable output only if `checkFlow` encodes them into the
report message.

The callback-param probe was the six `withSecret((sk) => …)` forms (bare and parenthesized
arrows, expression and block bodies, one-assignment indirection, template-literal sink, and a
wrapper taking the callback first) plus two controls (a direct-return accessor and a
property-read) in a single fixture; the source queries captured the parameter in all six
(a diagnostic gate rule reported seven captures) while the flow rule reported neither.

## Re-run after #217 / #218 (#220)

#217 (parameter-origin seeding) and #218 (the compound-sink sanitizer fix) close the two
correctness gaps #195 identified. This section re-runs the same calibration at a new pinned
pair and answers the B4 question the original run could not decide.

### Reproduction

| | |
|---|---|
| Corpus | `perawallet/pera-react-native` @ **`3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd`** — unchanged from #195 |
| lanekeep | this pull request — taint-analysis code frozen at **`75af559`** (the sanitizer-capture fix, below), calibration harness at **`e2fda4f`**; branch `claude/lanekeep-issue-220-19638d` at measurement time. `lanekeep 0.8.1`, `HOST_API_VERSION=5` — same binary version and host API as #195's **`281fb79`** |
| Toolchain | `rustc 1.95.0`, pinned by `rust-toolchain.toml` — unchanged |
| Machine | Apple M3 Max, 14 cores, macOS 26.6.2 (Darwin 25.6.0, arm64) — same machine class as #195 |
| Date | 2026-09-06 |
| Scope | same include globs as the Appendix above (`apps/*/src`, `packages/*/src`, `extensions/*/src`); excludes reconstructed from #195's prose Scope row. 3,984 files parsed, 0 aborts (#195: 3,985 — a one-file difference, immaterial to scope) |
| Run | `just taint-calibration <corpus>` → `lanekeep check <snapshot> --no-cache --format json`. lanekeep's own check wall-clock: **0.39–0.43 s** (#195: **0.47 s**). Harness total about 2 s, including a `git archive` + `tar` extract of 5,562 files |

### What changed since #195

Two things changed, not one. **lanekeep itself** moved from `281fb79` to this pull request,
which includes #217 (seed taint at a source captured on a parameter binding) and #218 (honor a
sanitizer wrapping a source in a compound sink). **The sanitizer query also changed** — from
capturing `@sanitizer` on the callee identifier to capturing the whole call expression — and
that second change is not cosmetic.

#195's own sanitizer query (reproduced in the Appendix above), and the shipped
`lanekeep/no-secret-in-string` rule that used the identical idiom, captured `@sanitizer` on the
bare `(identifier)` node. But the analyzer's `is_member` and `sanitizer_between` checks match
against the enclosing `call_expression`, never against the identifier inside it. So #218's
compound-sink fix was a no-op through #195's own query: the fix landed in the analyzer, but
nothing in the query shape let it fire. This pull request corrects the query to capture the
whole call — `(call_expression function:(identifier) @fn (#any-of? @fn …)) @sanitizer` — in
both the calibration rule and the shipped `lanekeep/no-secret-in-string` rule, which carried the
same latent bug: its documented-as-silent example, `log(redact(getSecret()))`, actually reported
before this fix.

### Result: 3 findings, 0 true positives

All three findings sink at the same place, `extensions/keystore-chrome/src/keystore/sign.ts:112`
— the `${seed.length}` template substitution described above — with sources at the three
`key.privateKey` reads that feed `seed` (lines 107–109). This is the identical
field-insensitivity shape #194's sensitivity table names, reported three times by the
per-`(source, sink)` granularity #195 already flags as a reporting artifact: **one logical false
positive, not three.** Verified by tracing the mechanism in `flow.rs` directly and by
manual inspection of the site.

| Class | #195 (`281fb79`) | #220 (this pull request) |
|---|---|---|
| True positive | 0 | 0 |
| FP — field-insensitivity | 3 | 3 (= 1 logical site) |
| FP — path-insensitivity | 0 | 0 |
| FP — other (compound-sink sanitizer bypass) | 1 | **0 — fixed** |
| False negative — parameter-origin (`withSecret`) | ≥35 sites invisible | **closed** (16 in-scope sites now seeded; 0 leak) |

### Two headline changes vs #195

**The compound-sink FP is gone.** #195's FP #4 — `packages/migrate/src/migrate/migrateLegacyAccount.ts:81`,
a template string that calls `describeBytes(account.secretKey)` to build a `secretKey=` field —
no longer reports. `describeBytes` is a `@sanitizer`; with the call-capture fix, both
`is_member` (the sink itself is the sanitizer call) and `sanitizer_between` cut the flow. #218's
fix is now actually reachable, and it works: confirmed absent from the run's JSON output and by
a manual check of the site.

**The `withSecret` false negative is closed.** #195's dominant concern — that callback-delivered
secrets were invisible to the analyzer — is fixed by #217. An independent manual audit enumerated
all 16 in-scope `withSecret` / `withBackup*` sites: 11 are captured by the source query (bare,
parenthesized, and async arrow forms), and 5 are not (named-handler references and
zero-parameter callbacks — a query-coverage gap, not an analyzer gap, and none of the 5 leak
either). This 16-site count is not directly comparable to #195's 35-site tally — the two figures
use different enumeration scopes, this one counting callback call sites within the calibration's
include/exclude globs while #195's tally was broader — and the substantive point is that every
in-scope callback-delivered secret is now seeded, with none reaching a sink. Every captured
callback's secret is either consumed by crypto, derivation, or decoding,
or only reaches a sink through an opaque call. Zero flows is the correct answer here, and #217's
seeding is demonstrably live: a direct `bytes => logger.info(bytes)` now reports, proven by the
`calibration_queries` `RuleTester` tests and by tracing the `flow.rs` mechanism directly.

Two of the 16 sites need an honest caveat, and it is deliberately not phrased as "used safely":
`packages/card/src/api/transport/baanx-client.ts:82` builds an Authorization header from
`textDecoder.decode(bytes)` inside a template string, and `packages/card/src/session/session.ts:91`
assigns `textDecoder.decode(bytes)` directly to a `refreshToken` field for a token-refresh
request. Both are legitimate destinations for the secret, not leaks. They stay invisible to the
analyzer only because `textDecoder.decode(...)` is an opaque call — the analyzer does not model
what a called function does with its argument, which is an intra-procedural limitation, not a
#217 shortfall. So there is no false assurance about a real leak at these two sites, but the
reason they are silent is the opaque-call boundary, not "safe by construction."

### Perf

Still not a concern. lanekeep's own check wall-clock is **0.39–0.43 s** across runs on 3,984
files, statistically the same as #195's **0.47 s** on 3,985.

### B4 verdict

**B4 is now decidable, and it is field sensitivity.** With both of #195's confounders removed —
the false negative closed by #217, the compound-sink FP fixed by #218 — the residual false
positives are trustworthy, and 100% of them are field-insensitivity: the "a property of a secret
that is not itself the secret" shape #194's sensitivity table already names, here `.length`, and
by extension `.byteLength`, a checksum, or similar. This confirms #195's hypothesis rather than
merely repeating it.

The sample-size caveat from #195 still applies, in a narrower form: the absolute count is one
field-insensitivity site (reported three times by the per-`(source, sink)` granularity), so a
published false-positive *rate* remains statistically meaningless — the decision rests on the
qualitative pattern, not on a large sample. Path- or branch-sensitivity is not indicated (0
path-insensitivity FPs, same as #195), and cross-function summaries are not indicated either: the
flows are intra-function, #217 fixed seeding rather than composition, and no finding here argues
for summary machinery.

### Harness and follow-ups

The calibration harness is now committed (`just taint-calibration`, `scripts/calibration/`,
`scripts/taint-calibration.sh`), so this re-run — and any future one — is a repeatable command
rather than the manual procedure #195 documented.

One footgun was left open as a follow-up and has since closed: nothing validated that a
`@sanitizer` capture was bound to a whole call rather than to an identifier inside it, so a rule
could write the identifier-capture idiom and have its sanitizer silently do nothing — exactly the
bug this re-run fixed in the calibration rule and in `no-secret-in-string`. Config load now
refuses a `@sanitizer` bound in a call's callee slot (#223), and the engine's
"copy-me" fixture `SECRET_FLOW_RULE` in `crates/lanekeep-engine/src/lib.rs`, which still carried
the idiom, captures the whole call and exercises the cut (#224).
