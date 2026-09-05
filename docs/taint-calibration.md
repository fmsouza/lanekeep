# Taint-analysis calibration: the false-positive rate before shipping (#195)

This is the measurement #185 §B.6 committed to: the taint analysis (`flow` / `checkFlow`,
`no-secret-in-string`, #194) is flow-sensitive but neither path- nor field-sensitive, so it
over-approximates by construction. Before the over-approximation is asserted to be acceptable,
it is measured against a real corpus, and the number is published with the discipline
`AGENTS.md` demands: both SHAs, the machine, the exact queries, measured against an immutable
snapshot rather than a working checkout.

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
