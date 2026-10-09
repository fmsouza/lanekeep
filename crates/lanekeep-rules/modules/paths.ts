/**
 * Resolving module specifiers against the corpus.
 *
 * A shared module rather than a copy in each rule that needs it. Two rules resolving
 * `./a` differently would not look like a bug — each would be individually plausible —
 * so the resolution has to have one definition.
 *
 * Relative specifiers resolve on their own. A bare one — `~/b`, `@app/b`, `lib/b` —
 * resolves only through the nearest `tsconfig.json`'s `compilerOptions.paths` and
 * `baseUrl`, and that takes two phases, because the two halves of the work are allowed in
 * different places:
 *
 * - **Reading the tsconfig happens in `check`**, through `aliasTargets(ctx, specifier)`.
 *   Only the per-file phase has `ctx.readFile`, and its reads are tracked: each file whose
 *   answer used the tsconfig records it — and every `tsconfig.json` it looked for and did
 *   not find — as a cache dependency, so editing `paths`, or creating a nearer config,
 *   invalidates exactly those files. A `reduce` has no reads at all; see
 *   `docs/cross-file-rules.md`.
 * - **Choosing a file happens in `reduce`**, through `resolveImport`, because only the
 *   reduce phase has the corpus. The candidates travel between the two in a fact.
 *
 * There is no `node_modules` lookup. A specifier no alias maps names something outside
 * the corpus, and a rule reasoning about the corpus has nothing to say about it.
 */

/** Extensions tried for a specifier that does not name one, in order. */
const EXTENSIONS = ['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs']

/**
 * Resolve `specifier`, as written in `fromFile`, to a path in `files`.
 *
 * Returns the resolved path, or `undefined` when the specifier names nothing in the
 * corpus. `undefined` is a normal answer: importing a package, or a file the walker
 * excluded, is not an error.
 *
 * `aliases` is what `aliasTargets` answered for this specifier during `check`, carried
 * here in a fact. Each is tried in order with the same extension and `index` rules a
 * relative specifier gets, and the first that names a file in the corpus wins — which is
 * how TypeScript treats a `paths` entry with several substitutions. Without it, a bare
 * specifier resolves to nothing, as it always has.
 *
 * `files` should be a `Set` for anything corpus-sized — this is called once per import
 * edge, and a linear scan per call would make a cross-file rule quadratic.
 */
export function resolveImport(fromFile, specifier, files, aliases) {
  const has = (path) => (files.has ? files.has(path) : files.includes(path))

  if (specifier.startsWith('.')) return probe(join(dirname(fromFile), specifier), has)
  if (!Array.isArray(aliases)) return undefined

  for (const base of aliases) {
    const found = probe(base, has)
    if (found !== undefined) return found
  }
  return undefined
}

/** The file `base` names in the corpus, as written, by extension, or as a directory. */
function probe(base, has) {
  // Exactly as written, for a specifier that already names its extension.
  if (has(base)) return base

  for (const extension of EXTENSIONS) {
    const candidate = `${base}.${extension}`
    if (has(candidate)) return candidate
  }

  // A directory import. Checked after the file candidates, because `./a` next to both
  // `a.ts` and `a/index.ts` means the file — same as every bundler and TypeScript itself.
  for (const extension of EXTENSIONS) {
    const candidate = `${base}/index.${extension}`
    if (has(candidate)) return candidate
  }

  return undefined
}

/**
 * The project-relative paths TypeScript would try for a bare `specifier`, in order.
 *
 * **Per-file phase only**: it reads the `tsconfig.json` nearest the file being checked,
 * walking up from its directory to the project root, plus any relative `extends` it names.
 * Every read goes through `ctx.readFile`, so every one is a tracked dependency of this file,
 * absences included.
 *
 * The answer follows TypeScript's own resolution. An exact `paths` key wins; otherwise the
 * wildcard pattern with the longest prefix that matches; its substitutions are tried in
 * order, relative to `baseUrl` when one is set and to the directory of the config that
 * declared `paths` when not. A `baseUrl` then adds `<baseUrl>/<specifier>` last. The
 * candidates are base paths, without extensions — `resolveImport` probes those against the
 * corpus.
 *
 * Returns `[]`, and reads nothing, for a relative or rooted specifier: `paths` never
 * applies to one. A package-name `extends` (`@tsconfig/node20`) is not followed, and
 * `jsconfig.json` is not consulted. A `tsconfig.json` that is not valid JSON — comments and
 * trailing commas are, as in TypeScript — throws, naming the file: a silently ignored
 * config would leave every aliased import unresolved without a word.
 */
export function aliasTargets(ctx, specifier) {
  if (specifier.startsWith('.') || specifier.startsWith('/')) return []

  const options = nearestOptions(ctx, dirname(ctx.filePath))
  if (options === undefined) return []

  const targets = []

  if (options.paths !== undefined) {
    const matched = matchPattern(options.paths, specifier)
    const substitutions = matched === undefined ? undefined : options.paths[matched.key]
    if (Array.isArray(substitutions)) {
      // TypeScript resolves `paths` against `baseUrl` when there is one, and against the
      // directory of the config that declared `paths` when there is not.
      const base = options.baseUrl ?? options.pathsBase
      for (const substitution of substitutions) {
        if (typeof substitution !== 'string') continue
        // `matched.star` empty is TypeScript's own behavior too: the substitution is used
        // as written, `*` and all, and names nothing.
        const path = matched.star ? replaceStar(substitution, matched.star) : substitution
        const target = within(base, path)
        if (target !== undefined) targets.push(target)
      }
    }
  }

  if (options.baseUrl !== undefined) {
    const target = within(options.baseUrl, specifier)
    if (target !== undefined) targets.push(target)
  }

  return targets
}

/**
 * The `baseUrl` and `paths` in force for a file in `directory`, or `undefined` when no
 * `tsconfig.json` between it and the project root exists.
 */
function nearestOptions(ctx, directory) {
  for (let at = directory; ; at = dirname(at)) {
    const path = at === '' ? 'tsconfig.json' : `${at}/tsconfig.json`
    const text = ctx.readFile(path)
    if (typeof text === 'string') return loadOptions(ctx, path, text, [path])
    if (at === '') return undefined
  }
}

/**
 * One config's `baseUrl` and `paths`, after its `extends` chain, each made
 * project-relative against the config that declared it.
 *
 * `compilerOptions` merges key by key, and later wins: each `extends` entry in order, then
 * the config itself. So a `paths` in the extending config replaces the base's wholesale,
 * exactly as TypeScript does. `chain` holds the configs already being read, so a cycle in
 * `extends` stops at the repeat rather than recursing until the budget runs out.
 */
function loadOptions(ctx, path, text, chain) {
  const config = parse(text, path)
  const directory = dirname(path)
  const options = { baseUrl: undefined, paths: undefined, pathsBase: undefined }

  const parents = Array.isArray(config.extends) ? config.extends : [config.extends]
  for (const parent of parents) {
    const inherited = readExtended(ctx, directory, parent, chain)
    if (inherited === undefined) continue
    if (inherited.baseUrl !== undefined) options.baseUrl = inherited.baseUrl
    if (inherited.paths !== undefined) {
      options.paths = inherited.paths
      options.pathsBase = inherited.pathsBase
    }
  }

  const own = config.compilerOptions
  if (isObject(own)) {
    if (typeof own.baseUrl === 'string') {
      const baseUrl = within(directory, own.baseUrl)
      if (baseUrl !== undefined) options.baseUrl = baseUrl
    }
    if (isObject(own.paths)) {
      options.paths = own.paths
      options.pathsBase = directory
    }
  }

  return options
}

/**
 * The options of the config a relative `extends` names, or `undefined`.
 *
 * As written first and then with `.json` appended, which is the order TypeScript tries.
 * A package name, a rooted path or a path leaving the project root is not followed.
 */
function readExtended(ctx, directory, specifier, chain) {
  if (typeof specifier !== 'string') return undefined
  if (!(specifier.startsWith('./') || specifier.startsWith('../'))) return undefined

  const written = within(directory, specifier)
  if (written === undefined) return undefined

  const candidates = written.endsWith('.json') ? [written] : [written, `${written}.json`]
  for (const path of candidates) {
    if (chain.includes(path)) return undefined
    const text = ctx.readFile(path)
    if (typeof text === 'string') return loadOptions(ctx, path, text, [...chain, path])
  }
  return undefined
}

/**
 * The `paths` key `specifier` matches, and what its `*` stood for.
 *
 * An exact key wins outright. Among wildcard keys, the longest prefix wins and the first
 * of equals — TypeScript's `findBestPatternMatch`. A key with more than one `*` is not a
 * pattern TypeScript accepts, and matches nothing here either.
 */
function matchPattern(paths, specifier) {
  let best
  for (const key in paths) {
    const star = key.indexOf('*')
    if (star === -1) {
      if (key === specifier) return { key, star: undefined }
      continue
    }
    if (key.indexOf('*', star + 1) !== -1) continue

    const prefix = key.slice(0, star)
    const suffix = key.slice(star + 1)
    if (specifier.length < prefix.length + suffix.length) continue
    if (!specifier.startsWith(prefix) || !specifier.endsWith(suffix)) continue
    if (best !== undefined && prefix.length <= best.prefix) continue

    best = {
      key,
      prefix: prefix.length,
      star: specifier.slice(prefix.length, specifier.length - suffix.length),
    }
  }
  return best
}

/**
 * `substitution` with its first `*` replaced by `star`.
 *
 * By slicing rather than `String.prototype.replace`, whose replacement string gives `$&`
 * and friends a meaning a module name has no business carrying.
 */
function replaceStar(substitution, star) {
  const at = substitution.indexOf('*')
  return at === -1 ? substitution : substitution.slice(0, at) + star + substitution.slice(at + 1)
}

/**
 * Parsed tsconfig text, keyed by the text itself.
 *
 * Keyed by **content** and never by path, and that is what makes holding it across files
 * sound. Every file still makes its own tracked read — the memo only saves parsing the
 * same bytes again. A path-keyed memo would skip the read, and the file whose answer
 * depended on the config would then not record that it did.
 */
const parsedConfigs = new Map()

function parse(text, path) {
  const known = parsedConfigs.get(text)
  if (known !== undefined) return known

  let config
  try {
    config = JSON.parse(stripJsonc(text))
  } catch (error) {
    throw new Error(`lanekeep/paths: ${path} is not valid JSON: ${error.message}`)
  }
  if (!isObject(config)) throw new Error(`lanekeep/paths: ${path} is not a JSON object`)

  parsedConfigs.set(text, config)
  return config
}

/**
 * JSON with comments and trailing commas, which is what a tsconfig is, as plain JSON.
 *
 * Two passes, both aware of string literals: a `//` inside `"https://..."` is text, not a
 * comment. The first drops comments; the second drops a comma whose next non-blank
 * character closes an object or an array, which it can only see once the comments between
 * them are gone. A leading byte-order mark is dropped too.
 */
function stripJsonc(text) {
  let uncommented = ''
  let i = text.charCodeAt(0) === 0xfeff ? 1 : 0
  while (i < text.length) {
    const c = text[i]
    if (c === '"') {
      const end = endOfString(text, i)
      uncommented += text.slice(i, end)
      i = end
    } else if (c === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i += 1
    } else if (c === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2)
      i = end === -1 ? text.length : end + 2
      uncommented += ' '
    } else {
      uncommented += c
      i += 1
    }
  }

  let out = ''
  i = 0
  while (i < uncommented.length) {
    const c = uncommented[i]
    if (c === '"') {
      const end = endOfString(uncommented, i)
      out += uncommented.slice(i, end)
      i = end
      continue
    }
    if (c === ',') {
      let next = i + 1
      while (next < uncommented.length && /\s/.test(uncommented[next])) next += 1
      if (uncommented[next] === '}' || uncommented[next] === ']') {
        i += 1
        continue
      }
    }
    out += c
    i += 1
  }
  return out
}

/** The index just past the string literal opening at `start`. */
function endOfString(text, start) {
  let i = start + 1
  while (i < text.length && text[i] !== '"') i += text[i] === '\\' ? 2 : 1
  return i + 1
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/**
 * `relative` joined onto `directory`, or `undefined` when it leaves the project root.
 *
 * Unlike `join`, which pops nothing at the root, this refuses: a path from a tsconfig is
 * handed to `ctx.readFile`, which throws on one that escapes, and a `baseUrl` of `..` names
 * nothing a rule may see.
 */
function within(directory, relative) {
  if (relative.startsWith('/')) return undefined

  const segments = directory === '' ? [] : directory.split('/')
  for (const segment of relative.split('/')) {
    if (segment === '' || segment === '.') continue
    if (segment === '..') {
      if (segments.length === 0) return undefined
      segments.pop()
      continue
    }
    segments.push(segment)
  }
  return segments.join('/')
}

/** The directory part of a path, or `''` for a path with no directory. */
export function dirname(path) {
  const at = path.lastIndexOf('/')
  return at === -1 ? '' : path.slice(0, at)
}

/**
 * Join a directory and a relative specifier, resolving `.` and `..` lexically.
 *
 * Lexical rather than filesystem-backed because rules have no filesystem: the corpus is a
 * list of paths, and `..` has to be resolved before anything can be looked up in it.
 */
export function join(directory, specifier) {
  const segments = directory === '' ? [] : directory.split('/')

  for (const segment of specifier.split('/')) {
    if (segment === '' || segment === '.') continue
    if (segment === '..') {
      // Popping past the root leaves the path outside the corpus, where nothing will
      // match — which is the right answer, and better than pretending it stopped at the
      // root and matching something unrelated.
      segments.pop()
      continue
    }
    segments.push(segment)
  }

  return segments.join('/')
}
