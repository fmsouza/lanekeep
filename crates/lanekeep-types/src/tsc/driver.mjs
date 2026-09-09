// lanekeep's `tsc` sidecar.
//
// Spawned once per run by `crates/lanekeep-types/src/tsc/mod.rs`, which embeds this file with
// `include_str!` and writes it into the project's `.lanekeep/`. It speaks newline-delimited
// JSON on stdin and stdout: one request object per line in, one response object per line out,
// answered in the order received because every handler is synchronous.
//
//   in   {"id": 7, "op": "typeOf", "file": "...", "start": 10, "end": 15}
//   out  {"id": 7, "ok": true,  "value": {"text": "number", "primitive": "number"}}
//   out  {"id": 7, "ok": false, "error": "..."}
//
// # Determinism rules, which are the whole reason answers are normalized here
//
// A rule's verdict may depend on nothing but `(bytes, path, ruleset, config, tracked reads)`.
// The compiler's own output is not in that tuple: `typeToString` is display text, a union's
// member order is the order the checker happened to build it in, and an overload set has no
// single return type. So:
//
//   * every union is sorted by its members' normalized `text` and deduplicated, so
//     `string | number` and `number | string` are one answer;
//   * `primitive` is decided by a fixed, ordered sequence of `ts.TypeFlags` tests, never by
//     string matching on `typeToString`;
//   * a `symbol`'s `module` is the specifier **as written at the use site**, not a resolved
//     path, so two checkouts of one project agree;
//   * `programs` emits every path relative to the project root, with forward slashes, whether
//     or not the file is under it — a `../` path is still a path two checkouts agree on, and
//     an absolute one would be the first machine-dependent term in the run key. The root and
//     every path crossing this boundary are realpathed first (see `projectRoot`), or a root
//     reached through a symlink makes every `node_modules` resolution `../`-prefixed and
//     absolute after all;
//   * `returnTypeOf` refuses an overload set whose members disagree rather than picking one;
//   * nothing here reads a clock, a random source or an environment variable that lanekeep
//     sets. `LANEKEEP_TSC_DRIVER_DELAY_MS` is read below and is test-only: lanekeep never
//     sets it, and it exists so a test can make a real process spend real time, which is the
//     only way to exercise a wall-clock budget.
//
// # Caching
//
// One `Program` per `tsconfig.json` that claims a file, found by walking up from the file and
// bounded by the project root, plus one ad-hoc program for files no config claims. Source
// files are cached by `(path, content hash)` and reused across programs and across rebuilds,
// and a rebuild passes the previous program as `oldProgram` — which is what makes the plan-6
// server case affordable.
//
// A re-answered `programs` is not free. It reads and hashes everything the programs this
// request's files fall under read — their roots, the declaration files they reached by
// resolution, the `package.json`s that resolution consulted, the `tsconfig.json` and its
// `extends` chain — once per such program that read it, and rebuilds only the ones something
// moved under. The memo `refreshProgram` carries is per program and not per request, because
// the read set a refresh compares is per program: a file P of them read is read and hashed P
// times. That read is what a held driver costs to keep honest: the alternative is a session
// answering out of bytes that are gone, and a listing that is the run's own cache key naming
// them.
//
// A held program no file of the request falls under is left alone: it keeps its program and
// its reads, contributes no row to the listing, and is refreshed by the request that next
// names one of its files. It is the listing that forces this rather than the cost — the
// listing is the run key, and a fresh driver over the same request holds no such program at
// all, so contributing its rows would key a held session differently from `lanekeep check`
// over identical bytes.
//
// **A known divergence from a fresh `tsc`, documented rather than fixed.** A config's own
// roots are re-expanded only when the config chain itself moved (see `buildProgram`'s
// `refreshOptions`), so a file newly created on disk that the config's `include` glob would
// match does not become a root of a held program until something forces an options reparse.
// It becomes one as soon as lanekeep's discovery names it — the request list is the other
// half of the root set — which is the path every file a rule is asked about takes; what is
// left over is a file discovery does not name and `include` does.
//
// **And that reaches the answers, not only the listing.** An extra root is not merely an extra
// row in the key: a `declare global` in one augments what every other root in the program
// sees, so for such a file a fresh `tsc` and a held driver answer *different types* for the
// same unchanged bytes. Stated at full strength here because "the listing differs" reads as a
// cache-key nuisance and this is a wrong `typeOf`.

import { createRequire } from 'node:module'
import { createHash } from 'node:crypto'
import { createInterface } from 'node:readline'
import fs from 'node:fs'
import path from 'node:path'

const [, , projectRootArgument, typescriptSpecifier] = process.argv
if (!projectRootArgument || !typescriptSpecifier) {
  fs.writeSync(2, 'usage: driver.mjs <project-root> <typescript-specifier>\n')
  process.exit(64)
}

/**
 * `fs.realpathSync`, memoized, falling back to the path itself where nothing is there yet.
 *
 * Only successes are cached: a path that does not resolve today may exist tomorrow — the
 * driver outlives a run in the plan-6 server case — and caching the fallback would pin the
 * unresolved spelling for the sidecar's whole life.
 *
 * The fallback is right for a *file*, which may be written between two requests, and wrong for
 * the project root: see `projectRoot` below, which refuses rather than falling back.
 */
const realpaths = new Map()
function realpath(absolute) {
  const cached = realpaths.get(absolute)
  if (cached !== undefined) return cached
  try {
    const resolved = fs.realpathSync(absolute)
    realpaths.set(absolute, resolved)
    return resolved
  } catch {
    return absolute
  }
}

/** An incoming path as this driver names it: absolute and realpathed. */
function boundaryPath(given) {
  return realpath(path.resolve(given))
}

/**
 * The project root, **realpathed**, which is what keeps the listing free of machine paths.
 *
 * TypeScript resolves a `node_modules` specifier through `realpath` — `preserveSymlinks` is
 * false and this driver does not change that — so with a root reached through a symlink every
 * resolved dependency lands *outside* `projectRoot` as it was spelled, and `listedPath` writes
 * it as `../../../private/var/.../<this project's own directory name>/…`. On macOS
 * `std::env::temp_dir()` and `os.tmpdir()` are always such a root, under pnpm that is most of
 * a listing, and every row of it is a machine-dependent term in the cache key: two
 * byte-identical projects would key differently. Realpathing the root, and every path that
 * crosses this boundary, is what makes the two spellings one listing — and it is also what
 * collapses the duplicate row (one file listed once relative and once absolute) the same
 * defect produced.
 */
const projectRoot = (() => {
  const resolved = path.resolve(projectRootArgument)
  try {
    return fs.realpathSync(resolved)
  } catch (error) {
    // Refused loudly rather than falling back to the unresolved spelling. Falling back gives
    // up the guarantee this realpathing exists for, silently and for the driver's whole life:
    // every `node_modules` resolution then lands outside the root as it was spelled, the
    // listing carries the checkout's location, and two byte-identical projects key
    // differently. A root nobody can resolve is not a project, so there is nothing to serve.
    // The Rust side has not had its `hello` answered when this happens, so it reports a
    // refused provider (`ProviderError::Refused`) carrying this line from the sidecar's
    // stderr. Written synchronously: on macOS a pipe write through `process.stderr` is
    // asynchronous, and `process.exit` does not wait for it, so the one line this exists to
    // deliver could be dropped.
    fs.writeSync(2, `the project root ${resolved} does not resolve: ${error?.message ?? error}\n`)
    process.exit(66)
  }
})()

// Resolved from the project root's own `package.json`, so a relative specifier such as
// `./node_modules/typescript` means what a file in that project would mean by it, and a bare
// one resolves through that project's `node_modules` rather than through lanekeep's.
const requireFromProject = createRequire(path.join(projectRoot, 'package.json'))

// A package that cannot be loaded, or one without the compiler API this driver is written
// against, is answered rather than thrown: the Rust side learns *why* from `hello` and refuses
// with the version and the missing function in the message, instead of a dead pipe. The
// reference corpus is on TypeScript 7.0.2, whose package need not expose this API at all.
let ts = null
let loadError = null
try {
  ts = requireFromProject(typescriptSpecifier)
} catch (error) {
  loadError = `cannot load the typescript package \`${typescriptSpecifier}\` from ${projectRoot}: ${error?.message ?? error}`
}
const REQUIRED_API = [
  'createProgram',
  'findConfigFile',
  'readConfigFile',
  'parseJsonConfigFileContent',
  'resolveModuleName',
]
const unsupported = ts ? REQUIRED_API.filter((name) => typeof ts[name] !== 'function') : []

/**
 * The `typescript` package's own directory, excluded from `programs` whole.
 *
 * Its bytes are a function of the TypeScript version, which [`TscProvider::identity`] already
 * folds, and listing eight megabytes of `lib.*.d.ts` per run would dominate the hash for no
 * information. The exclusion covers the package rather than only its `lib/` because
 * `types.typescript` may point *outside* the project root — at a workspace package's copy
 * under pnpm, which is the documented remedy — and the `package.json` module resolution reads
 * on the way there would then be listed as `../../../…`, putting the checkout's location back
 * into the key that item 1's realpathing exists to keep out of it.
 *
 * Derived from the package's own `package.json` rather than from `path.dirname` of the
 * resolved main: a package whose main sits at its root would make that dirname the *parent*
 * directory, and excluding a whole `node_modules/` is a silent hole. Falls back to the main's
 * directory, which is what the exclusion has always been, when there is no resolvable manifest.
 */
const typescriptLib = ts ? realpath(path.dirname(requireFromProject.resolve(typescriptSpecifier))) : ''
function typescriptPackageRoot() {
  if (!ts) return ''
  try {
    return path.dirname(realpath(requireFromProject.resolve(`${typescriptSpecifier}/package.json`)))
  } catch {
    return typescriptLib
  }
}
const typescriptRoot = typescriptPackageRoot()

// Test-only. Never set by lanekeep — see the determinism rules above. `Atomics.wait` rather
// than a timer because every handler here is synchronous and must stay that way.
const delayMs = Number.parseInt(process.env.LANEKEEP_TSC_DRIVER_DELAY_MS ?? '', 10)
const sleepBuffer = new Int32Array(new SharedArrayBuffer(4))
function delayIfAsked() {
  if (Number.isFinite(delayMs) && delayMs > 0) {
    Atomics.wait(sleepBuffer, 0, 0, delayMs)
  }
}

function digest(text) {
  // The BOM is stripped here as well as by `readCanonical` below, so that the two sides of a
  // staleness comparison agree even if some later reader hands over raw bytes. A hash that
  // depends on which reader produced the text is a program that rebuilds forever.
  return createHash('sha256')
    .update(text.charCodeAt(0) === 0xfeff ? text.slice(1) : text, 'utf8')
    .digest('hex')
}

/**
 * A file's text as *the compiler* reads it, which is the only form anything here hashes.
 *
 * `ts.sys.readFile` strips a leading UTF-8 byte-order mark and transcodes a UTF-16 file to a
 * JavaScript string; `fs.readFileSync(name, 'utf8')` does neither. Both readers were in use —
 * `recordRead` saw the compiler's text for a `tsconfig.json` or a `package.json` while
 * `diskHash` re-read the raw bytes — so a marked file never matched the hash recorded for it
 * and every `programs` call rebuilt every program it decided, forever. Nothing answered
 * wrongly, which is what made it invisible: the only symptom was a held session paying a full
 * build per request.
 *
 * `ts.sys.readFile` rather than a BOM strip written here, because the canonical form worth
 * agreeing on is the compiler's own — a UTF-16 source file is text to it and mojibake to a raw
 * UTF-8 read, and hashing the mojibake would be one more form to keep in step by hand.
 * `undefined` where the file cannot be read, exactly as that reader answers.
 */
function readCanonical(fileName) {
  return ts.sys.readFile(fileName)
}

/** `path` -> `{ hash, text, sourceFile }`, keyed by content so a rebuild reuses what it can. */
const sourceFiles = new Map()

/**
 * Every path a compiler host was asked to *read*, absolute, with the content hash it returned.
 *
 * This is what "tracked reads" means for a provider that reads through `tsc`. A program's
 * `getSourceFiles()` is not the same set: a `tsconfig.json`, every file in its `extends` chain
 * and every `package.json` module resolution consulted decide what the compiler answers and
 * appear in no program. Flipping `strict` used to change `typeOf` with a byte-identical
 * listing, which is a warm run answering the previous configuration.
 *
 * Absence probes — `fileExists`, `directoryExists` — are deliberately not recorded, and that is
 * sound because the key is recomputed from the current run's read set: a file that appears and
 * changes what resolution finds changes what is *read*, and so changes the key.
 *
 * The `typescript` package's own directory is excluded, here rather than at the listing, because
 * those bytes are a function of the compiler version, which `TscProvider::identity` already
 * folds — and listing eight megabytes of `lib.*.d.ts` per run would dominate the hash for no
 * information.
 */
const reads = new Map()

/**
 * The read set of the program currently being built, or `null` outside a build.
 *
 * This is how a read is attributed to a program. `reads` above is one map for the whole
 * driver, so on its own it cannot say *which* config's answers a file decides — and a refresh
 * that rebuilt every program whenever any recorded file moved would make one package's edit
 * cost a rebuild of every other config in the corpus. Every read a build makes happens
 * synchronously inside `buildProgram`, which is what makes a single module-level slot enough:
 * `optionsFor`'s config chain, module resolution's `package.json`s and `getSourceFile`'s
 * sources all land in the map of the build that caused them.
 */
let buildReads = null

/**
 * The absences of the program currently being built, or `null` outside a build.
 *
 * `{ files, directories }`, two sets of absolute paths: every path a compiler host asked this
 * build about and was *denied* — `fileExists` false, `directoryExists` false, `readFile`
 * undefined. Kept apart by which question was asked, because a path probed as a file that
 * later appears as a directory is not an answer that changed, and merging the two would
 * rebuild the program on every later call for as long as the directory existed.
 *
 * **Why an absence has to be remembered at all.** A refresh compares what a program *read*, so
 * a resolution that changes because a file appeared is invisible to it: nothing the program
 * read has moved, the listing and the run key are byte-identical, and the held program keeps
 * answering out of the failed resolution while `lanekeep check` over the same bytes answers
 * from the new one. `npm install`, a `.d.ts` landing beside a `.js`, a link flipped — all three
 * are that shape. This is the tracked-absence the builtin provider already has, spelled for a
 * provider whose reads happen inside a compiler.
 *
 * Bounded by `withinBoundary` — everything but the `typescript` package's own directory, whose
 * contents are a function of the compiler version that `TscProvider::identity` already folds.
 * Not the project root: a monorepo's hoisted install lands one level above it, in the
 * workspace's shared `node_modules`, and a boundary that discarded that denial left it
 * unrecorded (see `withinBoundary`).
 *
 * The set's size is linear in the number of imports a build resolves, not flat: each import
 * that does not resolve costs one denied file probe per candidate extension and ancestor
 * `node_modules` directory tried along the way, and every import adds to that. Measured on an
 * 80-package fixture — 5, 20 and 80 imports gave 15, 60 and 240 absent files, while the denied
 * *directories* stayed flat at 66 regardless, because ancestor `node_modules` lookup walks the
 * same directory chain whatever is being resolved.
 */
let buildAbsences = null

/**
 * The reads made answering the request in flight, or `null` between requests.
 *
 * A query resolves specifiers of its own — `exportedType`'s module, `complete`'s every import
 * — outside any program's host, and a `package.json` consulted there decides what the query
 * answers exactly as one consulted during `createProgram` does. Recorded into the driver-wide
 * `reads` those became listing rows: the run key then carried a file no program built, so a
 * held session's key differed from `lanekeep check`'s over the same request and depended on
 * which files happened to be cache misses last time. They are reported back with the answer
 * instead, and `TscProvider` records each through the asking query's own `FileAccess` — a
 * per-entry tracked read, the way every builtin-provider read already is, so the *file's* cache
 * entry depends on them and nothing else does.
 *
 * A read a query makes inside a build is a build's read and lands in `reads` as before:
 * `buildReads` wins below, because that file is a program's input and belongs in the listing.
 */
let queryReads = null

/**
 * Whether a path is one this driver is willing to remember anything about.
 *
 * `recordRead`'s own rule: excludes only the `typescript` package's own directory, whose contents are
 * a function of the compiler version that `TscProvider::identity` already folds. Not confined
 * to the project root — a monorepo's hoisted `npm install` lands the dependency a sibling
 * package's build was denied one level *above* the project root, in the workspace's shared
 * `node_modules`, and a boundary that discarded that denial left it unrecorded: `refreshAbsences`
 * had nothing to re-probe, so a held session kept answering out of the failed resolution after
 * the very install a fresh run would see.
 */
function withinBoundary(absolute) {
  return typescriptRoot === '' || !absolute.startsWith(typescriptRoot + path.sep)
}

/** Record one read and hand back what was read, so this can wrap a `readFile` in place. */
function recordRead(fileName, text) {
  if (typeof text !== 'string') return text
  const absolute = boundaryPath(fileName)
  if (typescriptRoot !== '' && absolute.startsWith(typescriptRoot + path.sep)) return text
  const hash = digest(text)
  if (buildReads) {
    reads.set(absolute, hash)
    buildReads.set(absolute, hash)
  } else if (queryReads) {
    queryReads.set(absolute, hash)
  } else {
    reads.set(absolute, hash)
  }
  return text
}

/**
 * Record one denied probe, and hand back the denial so this can wrap a predicate in place.
 *
 * Outside a build this is nothing at all: a query's own probes are not a program's, and
 * remembering them would rebuild a program for a resolution no program made. Absences are not
 * listing rows either — the run key is recomputed from the fresh build's read set once the
 * rebuild happens, so an absence has no hash to carry and needs none.
 */
function recordAbsence(kind, fileName, answer) {
  if (answer) return answer
  if (!buildAbsences) return answer
  const absolute = boundaryPath(fileName)
  if (withinBoundary(absolute)) buildAbsences[kind].add(absolute)
  return answer
}

/**
 * A path as the listing spells it: relative to the project root, forward slashes everywhere.
 *
 * Unconditionally relative, `..` segments and all. A file outside the root is a real case —
 * a monorepo package importing a sibling, and under pnpm most of a listing — and spelling it
 * absolutely would put the checkout's location into the cache key.
 */
function listedPath(absolute) {
  return path.relative(projectRoot, absolute).split(path.sep).join('/')
}

/**
 * `ts.sys` as a `ModuleResolutionHost`, with its `readFile` recorded.
 *
 * `exportedType` and `complete` resolve a specifier of their own, outside any program's host,
 * and a `package.json` consulted there decides what they answer exactly as one consulted
 * during `createProgram` does. Handed bare `ts.sys` those reads were recorded nowhere.
 */
function recordingResolutionHost() {
  return {
    fileExists: (name) => recordAbsence('files', name, ts.sys.fileExists(name)),
    readFile: (name) => recordRead(name, recordAbsence('files', name, ts.sys.readFile(name))),
    directoryExists: (name) =>
      recordAbsence('directories', name, ts.sys.directoryExists(name)),
    getCurrentDirectory: () => ts.sys.getCurrentDirectory(),
    getDirectories: (name) => ts.sys.getDirectories(name),
    realpath: ts.sys.realpath ? (name) => ts.sys.realpath(name) : undefined,
    useCaseSensitiveFileNames: ts.sys.useCaseSensitiveFileNames,
  }
}

/** `ts.sys` with its `readFile` recorded, for the config parser's own reads. */
function recordingParseHost() {
  return {
    useCaseSensitiveFileNames: ts.sys.useCaseSensitiveFileNames,
    readDirectory: (rootDir, extensions, excludes, includes, depth) =>
      ts.sys.readDirectory(rootDir, extensions, excludes, includes, depth),
    fileExists: (name) => recordAbsence('files', name, ts.sys.fileExists(name)),
    readFile: (name) => recordRead(name, recordAbsence('files', name, ts.sys.readFile(name))),
  }
}

/**
 * `configPath` (`''` for the ad-hoc program) -> the program and what it was built from.
 *
 * `{ options, fileNames, configFileNames, configPaths, hashes, reads, host, program }`:
 * `hashes` is the content hash each root was built with and `reads` is every file this build
 * read, which is the set a refresh compares. They are not the same set — a root the compiler
 * declined has no hash, and a declaration file reached by resolution is a read and never a
 * root. `configFileNames` is the subset of `fileNames` the config's own `include` produced,
 * which is what survives a request list that shrank; the rest of `fileNames` arrived because
 * some request named it, and leaves with the request that stops naming it.
 */
const programs = new Map()

/**
 * How many times `ts.createProgram` has been called, for the `stats` op below.
 *
 * A counter rather than a timing, because the cost this bounds is per *program*: `programs`
 * used to hand the driver one file at a time and rebuild every program the moment the root
 * count moved, so a corpus of forty TypeScript files and two hundred markdown ones cost two
 * hundred and forty-three builds. A benchmark on wall clock would have measured the machine;
 * this measures the shape.
 */
let createProgramCalls = 0

/**
 * Every file `programs` was asked about that no `tsconfig.json` under the root claims.
 *
 * Such a file is typed by the ad-hoc program, whose options are this driver's own — `strict`
 * is off, and so is everything else the project configured — so its answers depend on where
 * the project root was pointed rather than on the project. Silence about that is the defect:
 * the same file, checked from one directory up, is typed differently and nothing says so.
 * Reported to the host, which prints one line naming the count.
 *
 * Cleared at the top of every `programs` call rather than accumulated: the request's file list
 * is the run's own discovery list, so it is the whole answer and not an addition to the
 * previous one. A held driver that kept the union named a file deleted two runs ago, which is
 * a session keying differently from `lanekeep check` over identical bytes — and the notice
 * itself naming a file that is not there.
 */
const adhocFiles = new Set()

/**
 * The `ts.ScriptKind` a file is parsed as, which is also the list of languages this driver can
 * answer for.
 *
 * Kept in step by hand with `TSC_LANGUAGES` in `crates/lanekeep-engine/src/lib.rs`, which is
 * what decides whether a rule declaring `requires: ['types']` gets `ctx.types` for a file at
 * all. An extension added here without being added there is a file the driver would type and
 * no rule is ever asked about.
 */
function scriptKindOf(fileName) {
  if (fileName.endsWith('.tsx')) return ts.ScriptKind.TSX
  if (fileName.endsWith('.jsx')) return ts.ScriptKind.JSX
  if (fileName.endsWith('.js') || fileName.endsWith('.mjs') || fileName.endsWith('.cjs')) {
    return ts.ScriptKind.JS
  }
  return ts.ScriptKind.TS
}

function makeHost(options) {
  const host = ts.createCompilerHost(options, true)
  // Module resolution reads through `host.readFile` — every `package.json` it consults on the
  // way to a specifier — and none of those files is in any program. Wrapped rather than
  // replaced, so whatever the compiler host does with them is unchanged.
  const readFile = host.readFile.bind(host)
  host.readFile = (fileName) => recordRead(fileName, recordAbsence('files', fileName, readFile(fileName)))
  // And the two predicates module resolution asks before it reads anything. A denial here is
  // what decides where a specifier lands, so it is as much an input to the answer as any byte
  // — see `buildAbsences`.
  const fileExists = host.fileExists.bind(host)
  host.fileExists = (fileName) => recordAbsence('files', fileName, fileExists(fileName))
  if (typeof host.directoryExists === 'function') {
    const directoryExists = host.directoryExists.bind(host)
    host.directoryExists = (name) => recordAbsence('directories', name, directoryExists(name))
  }
  host.getSourceFile = (fileName, languageVersionOrOptions) => {
    // The compiler's own reader, so the text a source file is built from — and the hash the
    // refresh compares it against — is the one canonical form `readCanonical` describes.
    const text = readCanonical(fileName)
    if (text === undefined) return recordAbsence('files', fileName, undefined)
    recordRead(fileName, text)
    const hash = digest(text)
    const cached = sourceFiles.get(fileName)
    if (cached && cached.hash === hash) return cached.sourceFile
    const sourceFile = ts.createSourceFile(
      fileName,
      text,
      languageVersionOrOptions,
      true,
      scriptKindOf(fileName),
    )
    sourceFiles.set(fileName, { hash, text, sourceFile })
    return sourceFile
  }
  return host
}

// A function rather than a top-level constant, because it dereferences `ts`. Evaluated at
// module scope it throws before `hello` can be answered whenever `ts` is null (the package did
// not load) or lacks these enums (a package without the compiler API) — which is precisely the
// pair of cases the load above exists to *report*, so the reader would get a stack trace on a
// dead pipe instead of the version and the missing function. Nothing is recomputed that
// matters: it is called once per ad-hoc program.
function adhocOptions() {
  return {
    allowJs: true,
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    noEmit: true,
    skipLibCheck: true,
  }
}

/**
 * The `tsconfig.json` that claims a file, or `''` for none.
 *
 * Bounded by the project root: a config above it would pull a program together out of files
 * lanekeep is not checking, and lanekeep's own confinement stops at that directory.
 */
function configFor(file) {
  const found = ts.findConfigFile(path.dirname(file), (candidate) =>
    ts.sys.fileExists(candidate),
  )
  if (!found) return ''
  const resolved = boundaryPath(found)
  const inside = resolved.startsWith(projectRoot + path.sep)
  return inside ? resolved : ''
}

function optionsFor(configPath, files) {
  if (configPath === '') {
    return { options: adhocOptions(), fileNames: files, configFileNames: [] }
  }
  // Both reads are recorded: `readConfigFile` sees the `tsconfig.json` itself, and the parse
  // host sees every file its `extends` chain pulls in. Neither ends up in a program.
  const read = ts.readConfigFile(configPath, (name) => recordRead(name, ts.sys.readFile(name)))
  if (read.error) {
    throw new Error(ts.flattenDiagnosticMessageText(read.error.messageText, '\n'))
  }
  const parsed = ts.parseJsonConfigFileContent(
    read.config,
    recordingParseHost(),
    path.dirname(configPath),
    undefined,
    configPath,
  )
  // The config's own file list plus whatever was asked about, so a file a config does not
  // `include` is still typed in that config's options rather than falling to the ad-hoc
  // program with different ones.
  const fileNames = [...new Set([...parsed.fileNames, ...files])].sort()
  // `configFileNames` is the first half alone, and it is what `ensureProgram` keeps when a
  // request's list shrinks: those roots are the config's own and belong to the program however
  // few of them this run names, where a root that arrived only because some request named it
  // must leave with that request.
  return {
    options: { ...parsed.options, noEmit: true },
    fileNames,
    configFileNames: parsed.fileNames,
  }
}

/**
 * The content hash each root was last read with, taken from the `getSourceFile` cache.
 *
 * Read out of that cache rather than by reading every root again: the build these are recorded
 * after has just read each one and hashed it. A root the compiler never asked for has no entry
 * and gets none here — an extension the config does not admit is dropped before `getSourceFile`
 * — which is why `refreshRoots` compares only the roots the program actually holds rather than
 * treating a missing hash as a difference no rebuild could ever close.
 *
 * The cache is not pruned by a rebuild, so an entry may outlive the root it was made for. The
 * one case where that matters is a root deleted between two runs, and `forgetFile` handles it
 * where the deletion is noticed.
 */
function builtRootHashes(fileNames) {
  const hashes = new Map()
  for (const name of fileNames) {
    const cached = sourceFiles.get(name)
    if (cached) hashes.set(name, cached.hash)
  }
  return hashes
}

/**
 * The hash of a file's text on disk now, or `undefined` where it cannot be read.
 *
 * `memo` is one refresh's own map, so a path that is both a root and a read of the same program
 * is read and hashed once rather than twice — `refreshRoots` and `refreshReads` between them
 * name every root twice on a project whose config `include`s its sources, which measured +50%
 * of a refresh's reads at four thousand roots. Per call rather than driver-wide: the whole
 * point of the read is to see what is on disk *now*.
 */
function diskHash(fileName, memo) {
  if (memo && memo.has(fileName)) return memo.get(fileName)
  const text = readCanonical(fileName)
  const hash = text === undefined ? undefined : digest(text)
  if (memo) memo.set(fileName, hash)
  return hash
}

/**
 * Forget a file that is no longer on disk, so nothing remembered about it outlives it.
 *
 * Both maps are keyed by absolute path and neither is pruned anywhere else: `sourceFiles`
 * would otherwise hand the next `builtRootHashes` the hash of a file that is gone, and `reads`
 * would keep listing it — putting bytes that no longer exist into the run's own cache key. A
 * path that comes back is read afresh and recorded again, as any first read is.
 *
 * A root reaches this from `refreshRoots` and a file the program merely read from
 * `refreshReads`; the bookkeeping is the same either way, which is why this is not named for
 * roots alone.
 */
function forgetFile(name) {
  sourceFiles.delete(name)
  reads.delete(boundaryPath(name))
}

/**
 * Forget a program whose `tsconfig.json` has left the disk.
 *
 * Every held program is refreshed where a run begins, including one this request never named,
 * and the config that decides a program's options is a file like any other. Deleted or
 * renamed, the refresh calls it moved; `ensureProgram` then re-parses the options for the
 * rebuild, and `ts.readConfigFile` cannot read it. That throws — on this request and on every
 * later one, for the sidecar's whole life — while `lanekeep check` over the same bytes
 * succeeds, because a fresh provider holds no program to refresh. A program whose config is
 * gone contributes no rows and can never be built again, since `configFor` cannot return a
 * path that is not there, so it is dropped rather than rebuilt.
 *
 * Only what no *other* held program still names is forgotten — see `forgetUnclaimed`.
 */
function dropProgram(configPath) {
  const entry = programs.get(configPath)
  if (!entry) return
  programs.delete(configPath)
  forgetUnclaimed([...entry.reads.keys(), ...entry.fileNames])
}

/** Every path some held program still names, as a root or as a read. */
function heldPaths() {
  const held = new Set()
  for (const entry of programs.values()) {
    for (const name of entry.reads.keys()) held.add(boundaryPath(name))
    for (const name of entry.fileNames) held.add(boundaryPath(name))
  }
  return held
}

/**
 * Forget each of these paths that no held program still names.
 *
 * The guard is the whole point. The project's own `package.json` is read by every config's
 * build, and dropping its row because one config went away — or because one program stopped
 * naming it as a root — would move the run key over a file nothing touched. Called after the
 * program that was giving these up is already out of `programs`, or already rebuilt without
 * them, so `heldPaths` answers about the state that remains.
 *
 * The scan is per call and not an index kept in step: K drops in one run cost K walks of every
 * remaining program's reads. That is the trade taken deliberately — a run where no config and
 * no root leaves pays nothing at all, drops are the rare case, and a second structure mirroring
 * `reads` would be one more thing to leave stale exactly where staleness is the bug.
 */
function forgetUnclaimed(names) {
  if (names.length === 0) return
  const held = heldPaths()
  for (const name of names) {
    if (!held.has(boundaryPath(name))) forgetFile(name)
  }
}

/**
 * A program's roots as they are now: the ones still on disk, and whether any of them moved.
 *
 * Three kinds of root, and only one of them is a difference:
 *
 * - Gone from disk. Dropped from the root set and forgotten, which rebuilds the program once
 *   and then settles — keeping it would compare a hash against nothing forever.
 * - Held by the program. Compared, and a different hash on disk is what `refresh` exists to
 *   catch.
 * - Present on disk and *not* held. The compiler declined it before `getSourceFile` — a `.js`
 *   root under a config without `allowJs`, or a file whose extension is not a language at all
 *   — so it has no built hash and never will. Comparing one rebuilt the program on every
 *   single `programs` call.
 *
 * One read of each root, not recorded through `recordRead`: this is staleness detection rather
 * than a read the compiler's answer depends on, and the read set is rebuilt by the build it
 * triggers.
 */
function refreshRoots(entry, memo) {
  const kept = []
  let moved = false
  for (const name of entry.fileNames) {
    const disk = diskHash(name, memo)
    if (disk === undefined) {
      forgetFile(name)
      moved = true
      continue
    }
    kept.push(name)
    if (entry.program.getSourceFile(name) && entry.hashes.get(name) !== disk) moved = true
  }
  return { kept, moved }
}

/**
 * Whether anything this program *read* has moved since it was built.
 *
 * The roots are only the files a config names. Everything else a program's answers depend on
 * is reached by resolution: the declaration file behind an import, the `package.json`s
 * resolution consulted on the way to it, the `tsconfig.json` and its `extends` chain. Root-only
 * comparison left every one of them invisible to a refresh, so a held driver — a session holds
 * one across runs — kept answering out of the old declaration while a fresh provider, having
 * no cached program to return early from, answered the new one. That divergence is `lanekeep
 * server` disagreeing with `lanekeep check` about the same bytes.
 *
 * `entry.reads` is this program's own read set rather than the driver-wide one, so an edit
 * under one `tsconfig.json` rebuilds that config's program and leaves the others alone.
 *
 * A moved file's hash is written back here and into `reads`, rather than left for the rebuild
 * to re-record. A rebuild does not necessarily read every file again: `buildProgram` carries
 * the previous read set forward so that nothing is silently unwatched, and a read the new
 * build does not make — a manifest whose import the same edit removed, a `tsconfig.json` whose
 * options are reused — keeps whatever hash the entry holds for it. Left at the pre-edit hash
 * it answers "moved" on every later call, rebuilding the program forever for an edit already
 * absorbed. `programs settles when the rebuild does not re-read the moved file` is the fixture
 * that pins this; the two rebuild fixtures beside it do not, because there the rebuild reads
 * the moved file again and its own `collected` writes the fresh hash back regardless. The
 * listing takes the fresh hash for the same reason: those are the bytes on disk now.
 *
 * `optionsMoved` is the subset of `moved` that matters to `buildProgram`: whether any of
 * `entry.configPaths` — the `tsconfig.json` and its `extends` chain, recorded by `optionsFor`
 * — is among what moved. A rebuild otherwise reuses `entry.options` outright, so a config edit
 * moved the run key and rebuilt the program with the options it had before the edit.
 */
function refreshReads(entry, memo) {
  let moved = false
  let optionsMoved = false
  let resolutionMoved = false
  const noteMover = (name) => {
    if (entry.configPaths.has(name)) optionsMoved = true
    // A moved read the program does not hold as a source file is something resolution
    // consulted on its way to one — a `package.json`, the config chain — rather than a file
    // whose text the checker reads. See `buildProgram`'s `reuseOldProgram`.
    if (!entry.program.getSourceFile(name)) resolutionMoved = true
  }
  for (const [name, hash] of entry.reads) {
    const disk = diskHash(name, memo)
    if (disk === undefined) {
      noteMover(name)
      forgetFile(name)
      entry.reads.delete(name)
      moved = true
      continue
    }
    if (disk === hash) continue
    noteMover(name)
    entry.reads.set(name, disk)
    reads.set(name, disk)
    moved = true
  }
  return { moved, optionsMoved, resolutionMoved }
}

/**
 * Whether anything this program was *denied* is there now.
 *
 * The half of a refresh that no comparison of read bytes can do. `npm install` puts a package
 * where resolution found none; a `.d.ts` lands beside the `.js` resolution fell back to; a
 * link flips. In every one of them nothing the program read has moved, so without this the
 * held program keeps answering out of the failed resolution — `typeOf` stays unknown while
 * `complete()` flips to `true`, because that op re-resolves per question — and `lanekeep
 * check` over identical bytes answers from the new file.
 *
 * Probed with the predicate the build asked, not with a general "is it there": a path probed
 * as a file that appears as a directory is the same denial it always was, and answering
 * `moved` for it would rebuild the program on every later call.
 *
 * An appearance leaves the set as it is noticed. The rebuild it forces re-probes whatever it
 * still asks about, so an absence that is genuinely still an absence comes back, and one that
 * has been absorbed does not. Nothing here reaches the listing: an absence has no bytes to
 * hash, and the key is recomputed from the rebuild's own read set.
 */
function refreshAbsences(entry) {
  let moved = false
  for (const name of entry.absences.files) {
    if (!ts.sys.fileExists(name)) continue
    entry.absences.files.delete(name)
    moved = true
  }
  for (const name of entry.absences.directories) {
    if (!ts.sys.directoryExists(name)) continue
    entry.absences.directories.delete(name)
    moved = true
  }
  return moved
}

/** A program's roots and reads as they are now: what is kept, and whether anything moved. */
function refreshProgram(entry) {
  // One hash per distinct path per refresh: a config that `include`s its sources names every
  // root in both halves below, and hashing each of them twice is a read of the whole corpus
  // for nothing.
  const memo = new Map()
  const roots = refreshRoots(entry, memo)
  // Both, always: `refreshReads` prunes a vanished read and writes back a moved hash, and
  // short-circuiting on a moved root would leave that bookkeeping undone until the next call.
  const readsRefresh = refreshReads(entry, memo)
  // And the appearances, always for the same reason. An appearance is a resolution input by
  // definition — it is what resolution asked for and was refused — so it costs the
  // `oldProgram` reuse exactly as a moved `package.json` does: reusing the previous
  // resolutions would rebuild the program straight back onto the fallback it took before.
  const appeared = refreshAbsences(entry)
  return {
    kept: roots.kept,
    moved: roots.moved || readsRefresh.moved || appeared,
    optionsMoved: readsRefresh.optionsMoved,
    resolutionMoved: readsRefresh.resolutionMoved || appeared,
  }
}

/** Whether two sorted root lists name the same files. */
function sameRoots(a, b) {
  return a.length === b.length && a.every((name, index) => name === b[index])
}

/**
 * The program for a config, built if needed and rebuilt when the file set widens — or, when
 * `refresh` says a run is beginning, when any root's bytes have moved since it was built or a
 * root has gone from disk.
 *
 * A rebuild passes the previous program as `oldProgram`, which lets the checker keep every
 * source file whose content hash has not moved — the `getSourceFile` cache above is what
 * makes that identity hold.
 *
 * `refresh` is false for a query and true for `programs`, and the split is the point rather
 * than an optimization. This driver outlives a run — `check --fix` re-checks through the same
 * sidecar, and a session holds one across runs — and a cached program answering out of bytes
 * that are gone is both a wrong `typeOf` and a listing carrying the previous hash, which is
 * the run's own cache key. Comparing hashes costs a read of everything the program read — the
 * roots, and every file it reached by resolution — so it happens once where a run begins and
 * never on the path a rule's question takes.
 */
function ensureProgram(configPath, files, refresh = false) {
  const existing = programs.get(configPath)
  if (!existing) return buildProgram(configPath, null, files)
  // A refresh is also where a deleted root leaves the set, so the roots this compares against
  // are the refreshed ones rather than the ones the program was built from.
  const refreshed = refresh ? refreshProgram(existing) : null
  // The root set follows the request list, and only on a refresh — a `programs` call, whose
  // file list is the run's own discovery list. It used to be a union with what the program
  // already held, so a root named by one run stayed one for the sidecar's life; a held session
  // then compiled a file a fresh provider over the same request would not have, and an extra
  // root is not merely an extra row in the key — a `declare global` in it augments what every
  // other root sees, so the two disagreed about the *type*. What the config itself names stays
  // whatever this run asks about: those roots are the program's own, and a fresh build over the
  // same request would expand the same `include`.
  //
  // A query (`refresh` false) widens as it always did: `contextFor` asks about one file, and
  // shrinking to it would rebuild the program on every question.
  const wanted = [
    ...new Set(
      refreshed
        ? [...refreshed.kept.filter((name) => existing.configFileNames.has(name)), ...files]
        : [...existing.fileNames, ...files],
    ),
  ].sort()
  // Element-wise rather than by length: a refresh that drops one root while the caller asks
  // about another leaves the count alone and the set different.
  const changed = !sameRoots(wanted, existing.fileNames)
  if (!changed && !(refreshed && refreshed.moved)) return existing
  // The tsconfig chain moving is what forces `buildProgram` to re-parse options rather than
  // reuse them — everything else about a rebuild (roots widened, a dependency's bytes moved)
  // leaves the options exactly as they were. A moved *resolution* input costs the `oldProgram`
  // reuse instead: see `buildProgram`.
  const rebuilt = buildProgram(configPath, existing, wanted, {
    refreshOptions: Boolean(refreshed && refreshed.optionsMoved),
    reuseOldProgram: !(refreshed && refreshed.resolutionMoved),
  })
  // A root that left the set is forgotten unless the rebuilt program reached it anyway — an
  // import from a root that stayed makes it a source file still, and one the checker holds is
  // one whose bytes the answers depend on. Without this the row survives in `reads`, which
  // `buildProgram` carries forward on purpose, and the listing — the run's own key — keeps
  // naming a file this run did not compile.
  const dropped = existing.fileNames.filter(
    (name) => !wanted.includes(name) && !rebuilt.program.getSourceFile(name),
  )
  for (const name of dropped) rebuilt.reads.delete(boundaryPath(name))
  forgetUnclaimed(dropped)
  return rebuilt
}

/**
 * Build a config's program, recording what the build read.
 *
 * `optionsFor` runs inside the same attribution window as `ts.createProgram`, and not only to
 * keep the window in one place: it is what puts the `tsconfig.json` and its `extends` chain
 * into the program's own read set, and those are files a refresh has to watch as much as any
 * source. It is called once — it used to be called twice for every program built from scratch,
 * once for the file list and once for the options, parsing the config and expanding its
 * `include` globs both times.
 *
 * A rebuild carries the previous read set forward and lets this build's reads win. Carrying it
 * is what keeps the watched set from narrowing: `oldProgram` reuse means the compiler need not
 * ask for every file again, and a dependency dropped from the set because one rebuild happened
 * not to re-read it would stop being watched from then on. The cost of carrying is a file that
 * is no longer imported staying watched until it is deleted, which is an extra rebuild and
 * never a wrong answer.
 *
 * `refreshOptions` is `ensureProgram`'s `refreshed.optionsMoved` — whether the tsconfig chain
 * itself is among what moved. A rebuild otherwise reuses `existing.options` outright: without
 * this, an edit to the `tsconfig.json` moved the run key (its hash reaches `entry.reads` and
 * the listing) and rebuilt the program, but with the options it had before the edit — a `strict`
 * flipped on stayed off for the life of the held program. `configPaths` is snapshotted right
 * after `optionsFor` returns, before `ts.createProgram` adds source reads to the same
 * `collected` map, so it names exactly the config chain and nothing a rebuild would re-read for
 * an unrelated reason.
 *
 * `reuseOldProgram` is the other half of that, and it is about resolution rather than options.
 * `oldProgram` carries the previous build's *resolved modules* forward, which is most of what
 * makes a rebuild cheap and is exactly wrong when the file that decided a resolution is the one
 * that moved: a `package.json` whose `types` now names a different declaration rebuilt the
 * program straight back onto the file it named before, so the held driver answered `number`
 * where a fresh one answered `string`. A moved read the program holds as a source file is not
 * such a case — the checker re-reads it, and `getSourceFile` is hash-checked — so a source-only
 * move keeps the reuse and only a resolution input pays for a build from nothing.
 */
function buildProgram(configPath, existing, files, how = {}) {
  // Named `how` rather than `options`, which in this function means the compiler's.
  const { refreshOptions = false, reuseOldProgram = true } = how
  const previous = buildReads
  const previousAbsences = buildAbsences
  const collected = new Map()
  const denied = { files: new Set(), directories: new Set() }
  buildReads = collected
  buildAbsences = denied
  try {
    let options
    let fileNames
    let configPaths
    let configFileNames
    if (existing && !refreshOptions) {
      options = existing.options
      fileNames = files
      configPaths = existing.configPaths
      // Not re-expanded here, because the config was not re-parsed: the `include` globs this
      // came from are the ones still in force.
      configFileNames = existing.configFileNames
    } else {
      let named
      ;({ options, fileNames, configFileNames: named } = optionsFor(configPath, files))
      configPaths = new Set(collected.keys())
      configFileNames = new Set(named)
    }
    createProgramCalls += 1
    const host = makeHost(options)
    const program = ts.createProgram({
      rootNames: fileNames,
      options,
      host,
      oldProgram: existing && reuseOldProgram ? existing.program : undefined,
    })
    const entry = {
      options,
      fileNames,
      configPaths,
      configFileNames,
      hashes: builtRootHashes(fileNames),
      reads: new Map([...(existing ? existing.reads : []), ...collected]),
      // Carried forward for the reason the read set is, and with the same cost: `oldProgram`
      // reuse means a rebuild need not re-probe every candidate it probed before, and an
      // absence dropped because one rebuild happened not to ask about it would stop being
      // watched from then on. What a refresh finds has appeared leaves the set there, so a
      // carried absence cannot re-trigger once it is absorbed.
      absences: {
        files: new Set([...(existing ? existing.absences.files : []), ...denied.files]),
        directories: new Set([
          ...(existing ? existing.absences.directories : []),
          ...denied.directories,
        ]),
      },
      host,
      program,
    }
    programs.set(configPath, entry)
    return entry
  } finally {
    buildReads = previous
    buildAbsences = previousAbsences
  }
}

/** The program, source file and checker a query is answered from, or `null`. */
function contextFor(file) {
  const absolute = boundaryPath(file)
  const configPath = configFor(absolute)
  let entry = ensureProgram(configPath, [absolute])
  let sourceFile = entry.program.getSourceFile(absolute)
  if (!sourceFile && configPath !== '') {
    entry = ensureProgram('', [absolute])
    sourceFile = entry.program.getSourceFile(absolute)
  }
  if (!sourceFile) return null
  return { entry, sourceFile, checker: entry.program.getTypeChecker() }
}

/**
 * The smallest node spanning `[start, end)`.
 *
 * Descends by containment, so a child that does not contain the range prunes its subtree. Ties
 * go to the deepest node, which is the one a rule pointing at an expression means.
 */
function nodeAt(sourceFile, start, end) {
  let best = null
  const visit = (node) => {
    if (node.getStart(sourceFile) > start || node.getEnd() < end) return
    if (
      best === null ||
      node.getEnd() - node.getStart(sourceFile) <=
        best.getEnd() - best.getStart(sourceFile)
    ) {
      best = node
    }
    ts.forEachChild(node, visit)
  }
  ts.forEachChild(sourceFile, visit)
  return best
}

/**
 * The primitive a type is, by flags, in a fixed order.
 *
 * Order is part of the contract rather than an implementation detail: `NumberLike` includes
 * `EnumLike`, and `BooleanLike` includes the `true | false` union `boolean` actually is, so
 * this runs before the union test below and the sequence decides overlaps.
 */
function primitiveOf(type) {
  const flags = type.flags
  const F = ts.TypeFlags
  if (flags & F.StringLike) return 'string'
  if (flags & F.NumberLike) return 'number'
  if (flags & F.BigIntLike) return 'bigint'
  if (flags & F.BooleanLike) return 'boolean'
  if (flags & F.ESSymbolLike) return 'symbol'
  if (flags & F.Null) return 'null'
  if (flags & (F.Undefined | F.Void)) return 'undefined'
  return undefined
}

/** The `ImportDeclaration` a declaration sits under, and the name it imports. */
function importOf(declaration) {
  let exported
  if (ts.isImportSpecifier(declaration)) {
    exported = (declaration.propertyName ?? declaration.name).text
  } else if (ts.isImportClause(declaration)) {
    exported = 'default'
  } else if (ts.isNamespaceImport(declaration)) {
    exported = '*'
  } else {
    return undefined
  }

  let node = declaration
  while (node && !ts.isImportDeclaration(node)) node = node.parent
  if (!node || !ts.isStringLiteral(node.moduleSpecifier)) return undefined
  // As written, never resolved: a resolved path carries the checkout's location, and two
  // machines checking one commit must agree.
  return { module: node.moduleSpecifier.text, exported }
}

/** The symbol a site names, walked back to the import that brought it in. */
function symbolAt(checker, node) {
  let symbol = checker.getSymbolAtLocation(node)
  if (!symbol && node.name) symbol = checker.getSymbolAtLocation(node.name)
  if (!symbol && ts.isNewExpression(node)) {
    symbol = checker.getSymbolAtLocation(node.expression)
  }
  if (!symbol) return undefined

  const name = symbol.getName()
  for (const declaration of symbol.getDeclarations() ?? []) {
    const imported = importOf(declaration)
    if (imported) return { name, module: imported.module, exported: imported.exported }
  }

  // Not an import at this site. `getAliasedSymbol` is what turns a re-export chain into the
  // declaration that ends it; if that declaration is itself an import, the module it names is
  // still the one written in this project's own source.
  if (symbol.flags & ts.SymbolFlags.Alias) {
    const target = checker.getAliasedSymbol(symbol)
    for (const declaration of target.getDeclarations() ?? []) {
      const imported = importOf(declaration)
      if (imported) return { name, module: imported.module, exported: imported.exported }
    }
  }
  return { name }
}

/**
 * `typeToString`'s output with any rendered path made root-relative, or `undefined`.
 *
 * `typeToString` is display text and it is not path-free: a module namespace object renders as
 * `typeof import("/private/tmp/acme/node_modules/dep/index")`, an absolute path into whoever's
 * checkout the compiler happened to be run in. That text reaches a rule, a violation message
 * and the cache entry the message is stored in, so two checkouts of one commit produce two
 * different messages for one file and a cached one names the directory a project used to live
 * in. It is the same defect `symbol.module` was given the same treatment for.
 *
 * Every rendered path is rewritten root-relative, exactly as `listedPath` spells a listing
 * row — forward slashes, `..` segments and all — so the two checkouts agree. A path outside
 * the realpathed root gets the same treatment as one inside it: `..` depth is a property of
 * the layout (how far a monorepo package sits from the project root), not of the machine, so
 * `path.relative` is machine-independent for it exactly as it is for an in-root path. Nothing
 * here needs the drop-the-answer fallback `listedPath`'s own comment already argues against
 * for the sibling case — an *absolute* prefix like `/private/var/…` is what would leak the
 * checkout's location, and this function never emits one.
 */
function normalizeText(text) {
  if (!text.includes('import(')) return text
  return text.replace(/import\((["'])(.*?)\1\)/g, (whole, quote, spelled) => {
    if (!path.isAbsolute(spelled)) return whole
    return `import(${quote}${listedPath(boundaryPath(spelled))}${quote})`
  })
}

/**
 * A type, normalized to what crosses the boundary.
 *
 * `normalizeText` now rewrites every rendered path root-relative rather than dropping the
 * answer for one it cannot reach, so `text` is not expected to come back `undefined` here.
 * The guard below is kept anyway: were that ever to change, an absent `text` still has to be
 * spelled as an absent key rather than an explicit `null`, because `JSON.stringify` drops an
 * `undefined` value and the Rust side reads the absence as "no answer", which is what it
 * would mean.
 */
function normalizeType(checker, type, node, depth = 0) {
  const text = normalizeText(checker.typeToString(type))
  const primitive = primitiveOf(type)
  // A primitive's rendering is a keyword and carries no path, so this branch cannot lose one —
  // and if it somehow did, dropping `text` here would leave `primitive` behind as a complete
  // answer, so the guard is stated rather than assumed.
  if (primitive && text !== undefined) return { text, primitive }
  if (text === undefined) return {}

  if (depth === 0 && type.isUnion && type.isUnion()) {
    const members = type.types.map((member) => normalizeType(checker, member, undefined, 1))
    members.sort((a, b) => (a.text < b.text ? -1 : a.text > b.text ? 1 : 0))
    const seen = new Set()
    const union = members.filter((member) => {
      if (seen.has(member.text)) return false
      seen.add(member.text)
      return true
    })
    return { text, union }
  }

  const symbol = node ? symbolAt(checker, node) : undefined
  return symbol ? { text, symbol } : { text }
}

const MAX_HERITAGE_DEPTH = 16

/** The declared type of `name` exported by `module`, as seen from `file`. */
function exportedType(context, moduleName, name) {
  const resolved = ts.resolveModuleName(
    moduleName,
    context.sourceFile.fileName,
    context.entry.options,
    recordingResolutionHost(),
  )
  if (!resolved.resolvedModule) return undefined
  const declaring = context.entry.program.getSourceFile(
    resolved.resolvedModule.resolvedFileName,
  )
  if (!declaring) return undefined
  const moduleSymbol = context.checker.getSymbolAtLocation(declaring)
  if (!moduleSymbol) return undefined
  const exported = context.checker
    .getExportsOfModule(moduleSymbol)
    .find((candidate) => candidate.getName() === name)
  if (!exported) return undefined
  return context.checker.getDeclaredTypeOfSymbol(exported)
}

/**
 * The fallback for a TypeScript too old to expose `isTypeAssignableTo`.
 *
 * Nominal, matching what the built-in provider promises: a walk up the heritage comparing
 * symbol identity, depth-bounded with a visited set. A union is assignable only if every
 * member is; a primitive is not.
 */
function nominallyAssignable(checker, source, target) {
  const wanted = target.getSymbol()
  if (!wanted) return false
  if (source.isUnion && source.isUnion()) {
    return source.types.every((member) => nominallyAssignable(checker, member, target))
  }
  const seen = new Set()
  let frontier = [source]
  for (let depth = 0; depth < MAX_HERITAGE_DEPTH && frontier.length > 0; depth += 1) {
    const next = []
    for (const type of frontier) {
      const symbol = type.getSymbol()
      if (symbol === wanted) return true
      if (!symbol || seen.has(symbol)) continue
      seen.add(symbol)
      for (const base of type.getBaseTypes?.() ?? []) next.push(base)
    }
    frontier = next
  }
  return false
}

/**
 * `[relativePath, contentHash]` over every file the *contributing* programs read, sorted,
 * `lib/` excluded.
 *
 * `contributing` is the set of configs at least one file of this request falls under, and the
 * listing is theirs alone. A fresh driver over the same request holds no program for any other
 * config, so rows from one a held driver still has are rows `lanekeep check` would not have —
 * and the listing is the run key, which the two have to agree on. A held program outside the
 * set keeps its rows; they return with the request that next names one of its files.
 *
 * The union of two sets, not one. `reads` is every path a host was asked for — the configs,
 * the `extends` chain, the `package.json`s resolution consulted, and the source files, since
 * `getSourceFile` records too. Each contributing program's `getSourceFiles()` is folded in as
 * well, so a file the checker kept from an `oldProgram` without re-reading it is still listed.
 *
 * Withholding is by path and subtractive rather than by taking the contributing programs'
 * `reads` maps directly, so that a path claimed by both a contributing and a non-contributing
 * program — the project's own `package.json`, read by every config's build — keeps its row. A
 * path is dropped only when some non-contributing program claims it and no contributing one
 * does.
 *
 * A query's own resolution reads belong to no build and are not in `reads` at all: they are
 * reported with the answer and recorded as per-entry tracked reads (see `queryReads`).
 */
function programListing(contributing) {
  const withheld = new Set()
  for (const [configPath, entry] of programs) {
    if (contributing.has(configPath)) continue
    for (const name of entry.reads.keys()) withheld.add(boundaryPath(name))
  }
  for (const configPath of contributing) {
    const entry = programs.get(configPath)
    if (!entry) continue
    for (const name of entry.reads.keys()) withheld.delete(boundaryPath(name))
  }
  const listing = new Map()
  for (const [absolute, hash] of reads) {
    if (withheld.has(absolute)) continue
    listing.set(listedPath(absolute), hash)
  }
  for (const configPath of [...contributing].sort()) {
    const entry = programs.get(configPath)
    if (!entry) continue
    for (const sourceFile of entry.program.getSourceFiles()) {
      const absolute = boundaryPath(sourceFile.fileName)
      if (typescriptRoot !== '' && absolute.startsWith(typescriptRoot + path.sep)) continue
      listing.set(listedPath(absolute), digest(sourceFile.text))
    }
  }
  return [...listing.entries()].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
}

const handlers = {
  hello() {
    if (loadError) return { error: loadError }
    if (unsupported.length > 0) return { typescript: ts.version, unsupported }
    return { typescript: ts.version }
  },

  // One program per `tsconfig.json`, with every file that config claims handed over at once,
  // and each one rebuilt here if a root's bytes have moved since it was built.
  //
  // It used to be one `ensureProgram(configFor(f), [f])` per file, and `ensureProgram` rebuilds
  // whenever the root set widens — so N files under one config cost N `createProgram` calls,
  // each one throwing away and rebuilding the last. Grouping first makes it one per config,
  // whatever the corpus size.
  programs(request) {
    // See `adhocFiles`: this list is the run's discovery list, not an addition to the last.
    adhocFiles.clear()
    const byConfig = new Map()
    for (const file of request.files ?? []) {
      const absolute = boundaryPath(file)
      const configPath = configFor(absolute)
      if (configPath === '') adhocFiles.add(listedPath(absolute))
      const group = byConfig.get(configPath)
      if (group) group.push(absolute)
      else byConfig.set(configPath, [absolute])
    }
    // The configs at least one file of this request falls under, and no others. The listing
    // below is the run's own cache key, and a fresh driver over the same request builds
    // exactly these programs — so a held driver that refreshed every program it holds put
    // rows into the key that `lanekeep check` over the same request has no way to produce,
    // which is the divergence the whole refresh exists to close. A held program outside the
    // set stays cached, contributes no rows and is not refreshed; the request that next names
    // one of its files refreshes it then, and rebuilds it if something moved meanwhile.
    //
    // Sorted, so two runs over one corpus build their programs in one order — `oldProgram`
    // reuse and the read set both depend on which program was built first.
    for (const configPath of [...new Set([...programs.keys(), ...byConfig.keys()])].sort()) {
      const files = byConfig.get(configPath)
      if (!files) {
        // Held, and outside the set: nothing to refresh — except that a program whose
        // `tsconfig.json` has gone from disk can never be reached by a later request, since
        // `configFor` cannot return a path that is not there, so it would be held for the
        // sidecar's whole life. It is dropped instead, which is also what frees the reads no
        // other held program still names. The probe is not recorded, as no absence probe is:
        // the key is recomputed from the reads of the run that is beginning.
        if (configPath !== '' && !ts.sys.fileExists(configPath)) dropProgram(configPath)
        continue
      }
      // `refresh`: this op is where a run begins, and a run may be the second one this
      // sidecar serves — see `ensureProgram`.
      ensureProgram(configPath, files, true)
    }
    return { listing: programListing(new Set(byConfig.keys())), adhoc: [...adhocFiles].sort() }
  },

  // Test-only, and answering nothing about a project: what it reports is this process's own
  // bookkeeping, so nothing in it reaches a cache key or a rule. It exists because the cost
  // `programs` is arranged around is a count of `createProgram` calls, and a benchmark that
  // measured wall clock instead would be measuring the machine.
  stats() {
    return { createProgram: createProgramCalls, programs: programs.size }
  },

  typeOf(request) {
    const context = contextFor(request.file)
    if (!context) return null
    const node = nodeAt(context.sourceFile, request.start, request.end)
    if (!node) return null
    return normalizeType(context.checker, context.checker.getTypeAtLocation(node), node)
  },

  symbolOf(request) {
    const context = contextFor(request.file)
    if (!context) return null
    const node = nodeAt(context.sourceFile, request.start, request.end)
    if (!node) return null
    return symbolAt(context.checker, node) ?? null
  },

  returnTypeOf(request) {
    const context = contextFor(request.file)
    if (!context) return null
    const node = nodeAt(context.sourceFile, request.start, request.end)
    if (!node) return null
    const callee = ts.isCallExpression(node) || ts.isNewExpression(node) ? node.expression : node
    const type = context.checker.getTypeAtLocation(callee)
    const signatures = context.checker.getSignaturesOfType(type, ts.SignatureKind.Call)
    if (signatures.length === 0) return null
    const returns = signatures.map((signature) =>
      context.checker.getReturnTypeOfSignature(signature),
    )
    const texts = new Set(returns.map((each) => context.checker.typeToString(each)))
    // Overloads that disagree have no single answer, and picking the first would make the
    // verdict depend on declaration order rather than on the program.
    if (texts.size !== 1) return null
    return normalizeType(context.checker, returns[0], undefined)
  },

  isAssignableTo(request) {
    const context = contextFor(request.file)
    if (!context) return null
    const node = nodeAt(context.sourceFile, request.start, request.end)
    if (!node) return null
    const source = context.checker.getTypeAtLocation(node)
    const target = exportedType(context, request.module, request.name)
    if (!target) return null
    if (typeof context.checker.isTypeAssignableTo === 'function') {
      return context.checker.isTypeAssignableTo(source, target)
    }
    return nominallyAssignable(context.checker, source, target)
  },

  complete(request) {
    const context = contextFor(request.file)
    if (!context) return false
    const specifiers = []
    ts.forEachChild(context.sourceFile, (node) => {
      const isModuleRef =
        (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
        node.moduleSpecifier &&
        ts.isStringLiteral(node.moduleSpecifier)
      if (isModuleRef) specifiers.push(node.moduleSpecifier.text)
    })
    return specifiers.every(
      (specifier) =>
        ts.resolveModuleName(
          specifier,
          context.sourceFile.fileName,
          context.entry.options,
          recordingResolutionHost(),
        ).resolvedModule !== undefined,
    )
  },
}

const input = createInterface({ input: process.stdin })
input.on('line', (line) => {
  if (line.trim() === '') return
  let request
  try {
    request = JSON.parse(line)
  } catch (error) {
    process.stdout.write(`${JSON.stringify({ id: 0, ok: false, error: String(error) })}\n`)
    return
  }

  delayIfAsked()

  // `Object.hasOwn` rather than a truthiness test on the lookup: `handlers` is an object
  // literal, so `op: "constructor"` or `op: "toString"` finds a function on `Object.prototype`
  // and calls it with a request. Nothing reachable does anything useful with that, and a
  // protocol that answers a prototype method at all is one nobody should have to reason about.
  const handler = Object.hasOwn(handlers, request.op) ? handlers[request.op] : undefined
  if (!handler) {
    const error = `unknown op \`${request.op}\``
    process.stdout.write(`${JSON.stringify({ id: request.id, ok: false, error })}\n`)
    return
  }

  const previousQueryReads = queryReads
  const collected = new Map()
  queryReads = collected
  try {
    const value = handler(request)
    // Every op, not only the query ones: `programs` makes its reads inside builds, where
    // `buildReads` claims them, so its collected set is empty and the field is an empty list
    // the host ignores. Uniform is one rule rather than a table of which ops report.
    const answered = [...collected.keys()].map(listedPath).sort()
    process.stdout.write(
      `${JSON.stringify({ id: request.id, ok: true, value: value ?? null, reads: answered })}\n`,
    )
  } catch (error) {
    // The stack, because a failure here is lanekeep's bug or the project's tsconfig, and both
    // are things a maintainer has to be able to locate. The host prints it verbatim.
    const detail = error && error.stack ? error.stack : String(error)
    process.stdout.write(
      `${JSON.stringify({ id: request.id, ok: false, error: detail })}\n`,
    )
  } finally {
    queryReads = previousQueryReads
  }
})
