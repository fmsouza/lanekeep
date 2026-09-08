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
// server case affordable and what makes a re-answered `programs` cheap.

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
  return createHash('sha256').update(text, 'utf8').digest('hex')
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

/** Record one read and hand back what was read, so this can wrap a `readFile` in place. */
function recordRead(fileName, text) {
  if (typeof text !== 'string') return text
  const absolute = boundaryPath(fileName)
  if (typescriptRoot !== '' && absolute.startsWith(typescriptRoot + path.sep)) return text
  reads.set(absolute, digest(text))
  return text
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
    fileExists: (name) => ts.sys.fileExists(name),
    readFile: (name) => recordRead(name, ts.sys.readFile(name)),
    directoryExists: (name) => ts.sys.directoryExists(name),
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
    fileExists: (name) => ts.sys.fileExists(name),
    readFile: (name) => recordRead(name, ts.sys.readFile(name)),
  }
}

/** `configPath` (`''` for the ad-hoc program) -> `{ options, fileNames, host, program }`. */
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
  host.readFile = (fileName) => recordRead(fileName, readFile(fileName))
  host.getSourceFile = (fileName, languageVersionOrOptions) => {
    let text
    try {
      text = fs.readFileSync(fileName, 'utf8')
    } catch {
      return undefined
    }
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
  if (configPath === '') return { options: adhocOptions(), fileNames: files }
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
  return { options: { ...parsed.options, noEmit: true }, fileNames }
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
 * one case where that matters is a root deleted between two runs, and `forgetRoot` handles it
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

/** The hash of a file's bytes on disk now, or `undefined` where it cannot be read. */
function diskHash(fileName) {
  try {
    return digest(fs.readFileSync(fileName, 'utf8'))
  } catch {
    return undefined
  }
}

/**
 * Forget a root that is no longer on disk, so nothing remembered about it outlives it.
 *
 * Both maps are keyed by absolute path and neither is pruned anywhere else: `sourceFiles`
 * would otherwise hand the next `builtRootHashes` the hash of a file that is gone, and `reads`
 * would keep listing it — putting bytes that no longer exist into the run's own cache key. A
 * path that comes back is read afresh and recorded again, as any first read is.
 */
function forgetRoot(name) {
  sourceFiles.delete(name)
  reads.delete(boundaryPath(name))
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
function refreshRoots(entry) {
  const kept = []
  let moved = false
  for (const name of entry.fileNames) {
    const disk = diskHash(name)
    if (disk === undefined) {
      forgetRoot(name)
      moved = true
      continue
    }
    kept.push(name)
    if (entry.program.getSourceFile(name) && entry.hashes.get(name) !== disk) moved = true
  }
  return { kept, moved }
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
 * the run's own cache key. Comparing hashes costs a read of every root, so it happens once
 * where a run begins and never on the path a rule's question takes.
 */
function ensureProgram(configPath, files, refresh = false) {
  const existing = programs.get(configPath)
  // Hoisted, and computed at most once. It used to be called twice for every program built
  // from scratch — once for the file list and once for the options — which parses the
  // `tsconfig.json`, walks its whole `extends` chain and expands its `include` globs twice.
  const fresh = existing ? null : optionsFor(configPath, files)
  // A refresh is also where a deleted root leaves the set, so the roots this compares against
  // are the refreshed ones rather than the ones the program was built from.
  const refreshed = existing && refresh ? refreshRoots(existing) : null
  const wanted = existing
    ? [...new Set([...(refreshed ? refreshed.kept : existing.fileNames), ...files])].sort()
    : fresh.fileNames
  // Element-wise rather than by length: a refresh that drops one root while the caller asks
  // about another leaves the count alone and the set different.
  const changed = existing && !sameRoots(wanted, existing.fileNames)
  if (existing && !changed && !(refreshed && refreshed.moved)) return existing

  const options = existing ? existing.options : fresh.options
  createProgramCalls += 1
  const host = makeHost(options)
  const program = ts.createProgram({
    rootNames: wanted,
    options,
    host,
    oldProgram: existing ? existing.program : undefined,
  })
  const entry = {
    options,
    fileNames: wanted,
    hashes: builtRootHashes(wanted),
    host,
    program,
  }
  programs.set(configPath, entry)
  return entry
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
 * `[relativePath, contentHash]` over every file the compiler read, sorted, `lib/` excluded.
 *
 * The union of two sets, not one. `reads` is every path a host was asked for — the configs,
 * the `extends` chain, the `package.json`s resolution consulted, and the source files, since
 * `getSourceFile` records too. Every program's `getSourceFiles()` is folded in as well, so a
 * file the checker kept from an `oldProgram` without re-reading it is still listed.
 */
function programListing() {
  const listing = new Map()
  for (const [absolute, hash] of reads) {
    listing.set(listedPath(absolute), hash)
  }
  for (const configPath of [...programs.keys()].sort()) {
    const entry = programs.get(configPath)
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
    const byConfig = new Map()
    for (const file of request.files ?? []) {
      const absolute = boundaryPath(file)
      const configPath = configFor(absolute)
      if (configPath === '') adhocFiles.add(listedPath(absolute))
      const group = byConfig.get(configPath)
      if (group) group.push(absolute)
      else byConfig.set(configPath, [absolute])
    }
    // Sorted, so two runs over one corpus build their programs in one order — `oldProgram`
    // reuse and the read set both depend on which program was built first.
    for (const configPath of [...byConfig.keys()].sort()) {
      // `refresh`: this op is where a run begins, and a run may be the second one this
      // sidecar serves — see `ensureProgram`.
      ensureProgram(configPath, byConfig.get(configPath), true)
    }
    return { listing: programListing(), adhoc: [...adhocFiles].sort() }
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

  try {
    const value = handler(request)
    process.stdout.write(
      `${JSON.stringify({ id: request.id, ok: true, value: value ?? null })}\n`,
    )
  } catch (error) {
    // The stack, because a failure here is lanekeep's bug or the project's tsconfig, and both
    // are things a maintainer has to be able to locate. The host prints it verbatim.
    const detail = error && error.stack ? error.stack : String(error)
    process.stdout.write(
      `${JSON.stringify({ id: request.id, ok: false, error: detail })}\n`,
    )
  }
})
