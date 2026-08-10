/**
 * Module resolution for rule files, at build time.
 *
 * A port of `crates/lanekeep-js/src/loader.rs`'s `RuleRoot::resolve`. That file is the
 * specification and this one follows it; where the two disagree, it is right and this is
 * wrong. Its unit tests are ported one for one into `resolve.test.js`, which names each
 * source test, so the two can be audited against each other rather than trusted.
 *
 * # Why there are two
 *
 * A rule's imports have always been resolved at *run time*, by the resolver `loader.rs`
 * installs into the sandbox. A rule compiled ahead of time into a WebAssembly component has
 * no such moment: a bundler decides what every import means before anything runs, so the
 * same rules have to be enforced there or they are not enforced at all.
 *
 * # Confinement
 *
 * These are not ergonomics. Getting one wrong means a rule reads a file it must not, or a
 * file in the project shadows a built-in and quietly replaces the rule that was asked for.
 *
 * The rules root is canonicalized before it is compared against, and every resolved file is
 * canonicalized and checked again. Canonicalizing rather than comparing strings is what makes
 * the check hold against symlinks: a link inside the root pointing at `/etc/passwd` resolves
 * to a path outside it and is rejected, where a lexical comparison would see an
 * innocent-looking relative path and allow it.
 *
 * Traversal is *also* rejected lexically, before the filesystem is touched, so `../../secrets`
 * produces a message about escaping the root rather than a confusing "not found" that depends
 * on whether the file happened to be there.
 *
 * And `lanekeep` and `lanekeep/<name>` are answered before the root is even canonicalized,
 * which is what makes shadowing impossible rather than merely unlikely.
 */

import { realpathSync, statSync } from 'node:fs'
import { basename, dirname, isAbsolute, parse, sep } from 'node:path'

/** The specifier that resolves to lanekeep's own module. */
const HOST_MODULE = 'lanekeep'

/** The prefix a built-in specifier carries, as in `lanekeep/no-default-export`. */
const BUILTIN_PREFIX = 'lanekeep/'

/**
 * Extensions tried for a specifier that does not name one, in order.
 *
 * `loader.rs`'s `EXTENSIONS`, and deliberately not `modules/paths.ts`'s list, which is one
 * longer. That one resolves import edges *within a checked corpus*, where a `.cjs` file is
 * something a project may well have; this one resolves a rule's own imports, and a rule is an
 * ES module. The two lists answer different questions and are not to be unified.
 */
const EXTENSIONS = ['ts', 'tsx', 'js', 'jsx', 'mjs']

/** Separators, as the platform spells them. Windows accepts both; Unix accepts one. */
const SEPARATORS = sep === '\\' ? /[\\/]+/ : /\/+/

/**
 * Resolve a specifier written in `importer`.
 *
 * `importer` is the canonical absolute path of the importing module, `'lanekeep'` for the
 * host module, or `''` for an entry module that nothing imported. It has to be canonical for
 * the same reason the root is: the containment check compares them.
 *
 * `builtins` names what `lanekeep/<name>` may mean — `{ modules, components }`, each an array
 * or a `Set`, each optional. Both resolve under the one prefix, because which table a
 * built-in is in is an authoring detail and a specifier must mean one thing. A `components`
 * entry is refused rather than resolved: a component has no JavaScript to import.
 *
 * Returns exactly one of:
 *
 * - `{ path }` — an absolute, canonical path inside the rules root.
 * - `{ builtin }` — the specifier itself, for something the host supplies rather than a file.
 * - `{ error }` — `{ code, message, ... }`, where `code` is one of `bare-specifier`,
 *   `escapes-root`, `not-found`, `unreadable` or `not-a-module`. The messages are
 *   `ResolveError`'s, word for word, so a rule refused by a bundler and the same rule refused
 *   by the sandbox read identically.
 *
 * Never throws for anything a caller can cause. A refusal is a value here, because a bundler
 * plugin has to be able to render one.
 */
export function resolve(specifier, importer, options = {}) {
  const { rulesRoot, builtins } = options

  // Ahead of everything, including canonicalizing the root: the host module and the built-ins
  // are answered without the filesystem being consulted at all. A resolver that stat'd the
  // root first would still pass a shadowing test — it would simply have made the answer
  // depend on the state of a directory that has no say in it.
  if (specifier === HOST_MODULE) return { builtin: HOST_MODULE }

  if (specifier.startsWith(BUILTIN_PREFIX)) {
    const name = specifier.slice(BUILTIN_PREFIX.length)
    if (contains(builtins?.modules, name)) return { builtin: specifier }
    // Asked after the module lookup because a name is never both, and a module that exists
    // is the answer. A built-in that ships as a component is spelled correctly and is still
    // not importable, so it is refused on its own terms — "no built-in rule by that name"
    // would send its author hunting for a misspelling that is not there.
    if (contains(builtins?.components, name)) return { error: notAModule(name) }
    return { error: notFound(specifier, ['no built-in rule by that name']) }
  }

  const root = canonicalize(rulesRoot)
  if (root.error !== undefined) return { error: unreadable(rulesRoot, root.error) }

  const base = importer ?? ''

  // The entry module arrives as an already-resolved absolute path, because that is what a
  // caller hands the bundler. Accepting one is therefore necessary, but only for the entry:
  // an empty importer means nothing imported this.
  //
  // A rule writing `import '/etc/passwd'` always has an importer — its own path — so it falls
  // through to the bare-specifier refusal rather than through this door. Containment is
  // checked either way.
  if (isAbsolute(specifier)) {
    if (base !== '') return { error: bareSpecifier(specifier) }
    return within(root.path, specifier, normalize(specifier))
  }

  if (!specifier.startsWith('.')) return { error: bareSpecifier(specifier) }

  const directory = base === HOST_MODULE || base === '' ? root.path : parentOf(base, root.path)
  return within(root.path, specifier, normalize(concat(directory, specifier)))
}

/**
 * Find a file for an already-joined path, enforcing containment.
 *
 * The lexical check is repeated here as well as inside `confine`, because a traversal that
 * matches no file at all must still be reported as an escape rather than as "nothing found".
 */
function within(root, specifier, joined) {
  if (!isWithin(root, joined)) return { error: escapesRoot(specifier) }

  const tried = []
  for (const candidate of candidates(joined)) {
    tried.push(candidate)
    if (!isFile(candidate)) continue
    return confine(root, specifier, candidate)
  }

  return { error: notFound(specifier, tried) }
}

/**
 * Confine a path that was found to the root, and hand back its canonical form.
 *
 * Both checks, in this order, and the order is the point. The lexical one fires whatever is on
 * disk, so `../../secrets` is refused identically whether or not it is there. The canonical
 * one is what sees through a symlink, and it can only be made once the file is known to exist.
 */
function confine(root, specifier, joined) {
  if (!isWithin(root, joined)) return { error: escapesRoot(specifier) }

  const canonical = canonicalize(joined)
  if (canonical.error !== undefined) return { error: unreadable(joined, canonical.error) }
  if (!isWithin(root, canonical.path)) return { error: escapesRoot(specifier) }

  return { path: canonical.path }
}

/**
 * Candidate files for a specifier, in resolution order.
 *
 * Every extension before every `index.<extension>`, which is what makes a file beat a
 * same-named directory — `./a` beside both `a.ts` and `a/index.ts` means the file, as it does
 * in every bundler and in TypeScript itself.
 *
 * Only ever called with a normalized path, which is why the string arithmetic in
 * `withExtension` can assume no trailing separator and no `.` component.
 */
function candidates(base) {
  const out = []

  // An explicit extension is taken at face value.
  if (extensionOf(base) !== undefined) out.push(base)

  for (const extension of EXTENSIONS) out.push(withExtension(base, extension))
  for (const extension of EXTENSIONS) out.push(concat(base, `index.${extension}`))

  return out
}

// --- paths ----------------------------------------------------------------------------
//
// `node:path` cannot be used for these three. `path.join` resolves `..` while joining and
// clamps it at the root, `path.dirname` answers `'.'` where Rust answers `''`, and neither
// `extname` nor any public helper reproduces `Path::extension`'s treatment of a leading dot.
// Each difference is small and each would move the boundary, so they are ported rather than
// approximated.

/**
 * Resolve `.` and `..` lexically, as `lanekeep_core::files::normalize` does.
 *
 * `..` past the start of a relative path is kept rather than dropped, so the result stays
 * outside the root and is refused — dropping it would silently turn `../../secrets` into
 * `secrets`, which is a file inside the root that the author did not ask for.
 */
function normalize(path) {
  const { root } = parse(path)
  const out = []
  let depth = root === '' ? 0 : 1

  for (const segment of path.slice(root.length).split(SEPARATORS)) {
    if (segment === '' || segment === '.') continue
    if (segment === '..') {
      if (depth === 0) {
        out.push('..')
      } else {
        // A root cannot be popped, and `depth` still falls: `/a/../..` is `/`, not `/..`.
        out.pop()
        depth -= 1
      }
      continue
    }
    out.push(segment)
    depth += 1
  }

  return root + out.join(sep)
}

/**
 * Whether `path` is at or below `root`, by component rather than by prefix.
 *
 * `Path::starts_with`'s rule. A string prefix would accept `/root-extra` as being inside
 * `/root`, which is a directory the author has no claim on.
 */
function isWithin(root, path) {
  if (path === root) return true
  return path.startsWith(root.endsWith(sep) ? root : root + sep)
}

/** The parent of `path`, or `fallback` for a path that has none. `Path::parent`'s rule. */
function parentOf(path, fallback) {
  const parent = dirname(path)
  return parent === path ? fallback : parent
}

/** Append a relative segment, as `PathBuf::join` does: concatenation, with no normalizing. */
function concat(directory, relative) {
  if (directory === '') return relative
  return directory.endsWith(sep) ? directory + relative : directory + sep + relative
}

/**
 * The extension of `path`, or `undefined` for one that has none. `Path::extension`'s rule.
 *
 * A name that begins with a dot and holds no other has no extension — `.hidden` is a name,
 * not an extension — and `undefined` is distinct from `''`, which is what `a.` has.
 */
function extensionOf(path) {
  const name = basename(path)
  if (name === '' || name === '.' || name === '..') return undefined
  const at = name.lastIndexOf('.')
  return at <= 0 ? undefined : name.slice(at + 1)
}

/** Replace the extension, or append one where there is none. `Path::with_extension`'s rule. */
function withExtension(path, extension) {
  const name = basename(path)
  if (name === '') return path

  const current = extensionOf(path)
  const stem = current === undefined ? name : name.slice(0, name.length - current.length - 1)

  // The directory is taken as the literal prefix rather than through `dirname`, which would
  // turn `a` into `./a`.
  return path.slice(0, path.length - name.length) + (extension === '' ? stem : `${stem}.${extension}`)
}

// --- the filesystem -------------------------------------------------------------------

/** Whether a file — not a directory — is there. Follows symlinks, as `Path::is_file` does. */
function isFile(path) {
  try {
    const info = statSync(path, { throwIfNoEntry: false })
    return info !== undefined && info.isFile()
  } catch {
    // A path that cannot be stat'd at all is not a file. `Path::is_file` says the same.
    return false
  }
}

/** `{ path }` or `{ error }`. The OS realpath, which is what `Path::canonicalize` uses. */
function canonicalize(path) {
  if (typeof path !== 'string' || path === '') {
    return { error: new Error('no rules root was given') }
  }
  try {
    return { path: realpathSync.native(path) }
  } catch (cause) {
    return { error: cause }
  }
}

// --- refusals -------------------------------------------------------------------------
//
// The messages are `ResolveError`'s renderings, word for word. A rule refused by a bundler
// and the same rule refused by the sandbox have to read identically, or an author who hits
// one and then searches for the other finds nothing.

/** A bare specifier, which would be an npm package. */
function bareSpecifier(specifier) {
  return {
    code: 'bare-specifier',
    specifier,
    message:
      `cannot import \`${specifier}\`\n  ` +
      'rule modules run in a sandbox with no package resolution, so only `lanekeep` and ' +
      'relative paths starting with `./` or `../` can be imported\n  ' +
      'if this needs a package, inline what you need from it instead',
  }
}

/** The specifier resolves outside the rules root. */
function escapesRoot(specifier) {
  return {
    code: 'escapes-root',
    specifier,
    message:
      `cannot import \`${specifier}\`\n  ` +
      'it resolves outside the rules directory, and rule modules may only import from ' +
      'within it',
  }
}

/** Nothing exists at the specifier. `tried` is carried as data as well as in the message. */
function notFound(specifier, tried) {
  return {
    code: 'not-found',
    specifier,
    tried,
    message: `cannot find module \`${specifier}\`\n  tried: ${tried.join(', ')}`,
  }
}

/** The module exists but could not be read. */
function unreadable(path, cause) {
  return {
    code: 'unreadable',
    path,
    message: `cannot read module \`${path}\`: ${cause.message}`,
  }
}

/** The specifier names a built-in that ships as a component rather than as a module. */
function notAModule(name) {
  return {
    code: 'not-a-module',
    name,
    message: `\`lanekeep/${name}\` is a rule component; name it in a \`lanekeep.json\``,
  }
}

/** Membership in an array or a `Set`, and `false` for a table that was not supplied. */
function contains(names, name) {
  if (names === undefined || names === null) return false
  if (typeof names.has === 'function') return names.has(name)
  return Array.prototype.indexOf.call(names, name) !== -1
}
