# Type-aware rules

<!--
  Plan 4 adds the authoring, provider-selection and configuration sections ABOVE this one.
  This file starts life carrying the A3 measurement alone, because the measurement is spec
  §5.1's commit 1 and it lands before any A3 code exists.
-->

## Provider measurement

What the `tsc` provider (`types.provider: 'tsc'`, spec §5) has to pay for on a real
monorepo, measured before it was designed rather than claimed after it shipped. Every number
below is keyed to one immutable corpus commit and one machine; a figure measured against a
working tree is reproducible by nobody who lacks that tree.

The design for the type providers is posted on
[#185](https://github.com/fmsouza/lanekeep/issues/185) and kept at
<https://gist.github.com/fmsouza/cda0a0438e1d690a2bc58093f1f6ee89>; the § numbers below are
its sections.

### Reproduction

| | |
|---|---|
| Corpus | `perawallet/pera-react-native` @ **`3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd`** (branch `main`, 2026-09-03) |
| Package manager | `pnpm@10.28.1`, pinned by the corpus's own `packageManager` field — **not npm**: there is no `package-lock.json`, so `npm ci` fails outright |
| Install | `corepack pnpm install --frozen-lockfile` |
| Node | `v24.18.0` |
| TypeScript | `7.0.2` — resolved per-package from the `pnpm-workspace.yaml` catalog (`typescript: ^7.0.2`); there is no root `node_modules/typescript` (pnpm's isolated linker) |
| Machine | macOS 26.6.2, Mac15,10 (Apple M3 Max, 14 cores) |
| Date | 2026-09-07 |
| Measured by | lanekeep epic #185, commit 1 |

### Program shape

The question §5.3 turns on: is there one program or many?

| | |
|---|---|
| Root `tsconfig.json` | none |
| Configs under `apps/*` | 2 |
| Configs under `packages/*` | 40 |
| Configs under `extensions/*` | 12 |
| **Total in the three globs** | **54** |
| Configs carrying `"references"` | 0 |
| Outside the globs | `conformance/tsconfig.json` — a fourth workspace root; it is now in the cost table below rather than only asserted reachable |

**Many programs, no project graph.** Nothing composes these 54 into one build, so the driver
builds one `Program` per config that contains a queried file and an ad-hoc program for a file
no config claims, exactly as §5.3 specifies. The §5.1 decision rule — a root config carrying
`"references"` would send the driver to the reference graph instead — was checked and does not
fire.

Two shapes worth naming before the driver is written, both of them out of §0's scope and both
present here: `apps/mobile/tsconfig.json` uses `compilerOptions.paths`, and every config
`extends` a package resolved from `node_modules`, so no config can be read at all before the
install completes.

### Cost per program

One `tsc` invocation per config, measured with `/usr/bin/time -p` around
`pnpm --dir <dir> exec tsc -p tsconfig.json --noEmit --emitDeclarationOnly false --declaration false`.

The three overrides are required, not tidying: 40 of the 54 configs set `emitDeclarationOnly`,
which TypeScript refuses to combine with `--noEmit`. Cold is the first run after every
`*.tsbuildinfo` was deleted; warm is the next run of the same config. A non-zero exit is a type
error in this commit of the corpus and does not invalidate the timing.

| config | files in program | cold (s) | warm (s) | exit |
|---|---|---|---|---|
| `apps/browser/tsconfig.json` | 1356 | 0.40 | 0.40 | 1 |
| `apps/mobile/tsconfig.json` | 10458 | 1.71 | 1.46 | 1 |
| `conformance/tsconfig.json` | 1201 | 0.54 | 0.50 | 1 |
| `extensions/keystore-chrome/tsconfig.json` | 337 | 0.50 | 0.33 | 1 |
| `extensions/ledger-react-native-usb/tsconfig.json` | 270 | 0.50 | 0.30 | 1 |
| `extensions/ledger-react-native/tsconfig.json` | 615 | 0.50 | 0.31 | 1 |
| `extensions/ledger-shared/tsconfig.json` | 299 | 0.47 | 0.30 | 1 |
| `extensions/ledger-web-ble/tsconfig.json` | 462 | 0.47 | 0.31 | 1 |
| `extensions/ledger-web-usb/tsconfig.json` | 273 | 0.51 | 0.30 | 1 |
| `extensions/passkey-autofill/tsconfig.json` | 323 | 0.49 | 0.30 | 1 |
| `extensions/platform-chrome/tsconfig.json` | 1069 | 0.51 | 0.34 | 1 |
| `extensions/platform-driver/tsconfig.json` | 91 | 0.46 | 0.29 | 1 |
| `extensions/platform-react-native/tsconfig.json` | 1067 | 0.53 | 0.34 | 1 |
| `extensions/platform/tsconfig.json` | 483 | 0.49 | 0.31 | 1 |
| `extensions/provider/tsconfig.json` | 559 | 0.51 | 0.34 | 1 |
| `packages/accounts/tsconfig.json` | 1615 | 0.42 | 0.41 | 1 |
| `packages/age-gate/tsconfig.json` | 159 | 0.47 | 0.29 | 1 |
| `packages/analytics/tsconfig.json` | 144 | 0.46 | 0.29 | 1 |
| `packages/app-integrity/tsconfig.json` | 287 | 0.47 | 0.30 | 1 |
| `packages/arc0027/tsconfig.json` | 147 | 0.48 | 0.31 | 0 |
| `packages/asa-inbox/tsconfig.json` | 740 | 0.51 | 0.34 | 1 |
| `packages/assets/tsconfig.json` | 859 | 0.52 | 0.35 | 1 |
| `packages/background/tsconfig.json` | 264 | 0.48 | 0.30 | 1 |
| `packages/backup/tsconfig.json` | 883 | 0.61 | 0.43 | 1 |
| `packages/banners/tsconfig.json` | 456 | 0.49 | 0.31 | 1 |
| `packages/blockchain/tsconfig.json` | 1150 | 0.63 | 0.36 | 1 |
| `packages/card/tsconfig.json` | 997 | 0.56 | 0.38 | 1 |
| `packages/config/tsconfig.json` | 334 | 0.48 | 0.31 | 1 |
| `packages/contacts/tsconfig.json` | 269 | 0.47 | 0.30 | 1 |
| `packages/currencies/tsconfig.json` | 738 | 0.60 | 0.32 | 1 |
| `packages/database/tsconfig.json` | 685 | 0.55 | 0.31 | 1 |
| `packages/dev-fixtures/tsconfig.json` | 96 | 0.47 | 0.29 | 1 |
| `packages/device/tsconfig.json` | 385 | 0.49 | 0.31 | 1 |
| `packages/fee-delegation/tsconfig.json` | 616 | 0.48 | 0.32 | 1 |
| `packages/hardware-wallet/tsconfig.json` | 153 | 0.50 | 0.30 | 1 |
| `packages/kms/tsconfig.json` | 719 | 0.51 | 0.34 | 1 |
| `packages/ledger/tsconfig.json` | 210 | 0.51 | 0.30 | 1 |
| `packages/messages/tsconfig.json` | 522 | 0.50 | 0.33 | 1 |
| `packages/migrate/tsconfig.json` | 746 | 0.51 | 0.33 | 1 |
| `packages/multisig/tsconfig.json` | 452 | 0.53 | 0.33 | 1 |
| `packages/nfd/tsconfig.json` | 869 | 0.51 | 0.32 | 1 |
| `packages/onramp/tsconfig.json` | 717 | 0.51 | 0.34 | 1 |
| `packages/passkeys/tsconfig.json` | 510 | 0.51 | 0.33 | 1 |
| `packages/polling/tsconfig.json` | 317 | 0.51 | 0.30 | 1 |
| `packages/projects/tsconfig.json` | 419 | 0.60 | 0.35 | 1 |
| `packages/remote-config/tsconfig.json` | 204 | 0.48 | 0.31 | 1 |
| `packages/search/tsconfig.json` | 190 | 0.48 | 0.32 | 1 |
| `packages/security/tsconfig.json` | 332 | 0.48 | 0.34 | 1 |
| `packages/settings/tsconfig.json` | 219 | 0.48 | 0.30 | 1 |
| `packages/shared/tsconfig.json` | 522 | 0.51 | 0.34 | 1 |
| `packages/signing/tsconfig.json` | 1474 | 0.65 | 0.47 | 1 |
| `packages/staking/tsconfig.json` | 401 | 0.50 | 0.31 | 1 |
| `packages/swaps/tsconfig.json` | 628 | 0.54 | 0.34 | 1 |
| `packages/transactions/tsconfig.json` | 918 | 0.63 | 0.42 | 1 |
| `packages/walletconnect/tsconfig.json` | 472 | 0.51 | 0.34 | 1 |
| **total (55 configs)** | **40681** | **29.19** | **19.42** | |

**What this bounds.** Summed cold across all 55 configs, `types.provider: 'tsc'` adds
29.19 s to a cold run before a single lanekeep rule executes — the figure
`timeouts.analysis`'s 60 s default (§5.2) has to be defended against or moved for. The
slowest single config, `apps/mobile/tsconfig.json`, is 1.71 s cold, about 6% of that total.
The 60 s default survives contact with this commit of the corpus: the 55-config sweep uses
49% of it, and the slowest config alone is roughly 35× under it.
