/**
 * Resolving relative module specifiers against the corpus.
 *
 * A shared module rather than a copy in each rule that needs it. Two rules resolving
 * `./a` differently would not look like a bug — each would be individually plausible —
 * so the resolution has to have one definition.
 *
 * There is no `node_modules` lookup and no `tsconfig` path mapping here. A specifier that
 * does not start with `.` names something outside the corpus, and a rule reasoning about
 * the corpus has nothing to say about it.
 */

/** Extensions tried for a specifier that does not name one, in order. */
const EXTENSIONS = ['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs']

/**
 * Resolve `specifier`, as written in `fromFile`, to a path in `files`.
 *
 * Returns the resolved path, or `undefined` when the specifier is not relative or names
 * nothing in the corpus. `undefined` is a normal answer: importing a package, or a file
 * the walker excluded, is not an error.
 *
 * `files` should be a `Set` for anything corpus-sized — this is called once per import
 * edge, and a linear scan per call would make a cross-file rule quadratic.
 */
export function resolveImport(fromFile, specifier, files) {
  if (!specifier.startsWith('.')) return undefined

  const base = join(dirname(fromFile), specifier)
  const has = (path) => (files.has ? files.has(path) : files.includes(path))

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
