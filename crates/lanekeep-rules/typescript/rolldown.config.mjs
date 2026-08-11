/**
 * How `jco componentize` is allowed to resolve and read the modules it bundles.
 *
 * Merged into jco's own rolldown invocation by `--bundle-config`; jco owns `input`, `cwd`,
 * `format` and `platform`, so what this file contributes is two plugins and one output option.
 *
 * # The second plugin writes the source map, and the option is what produces one
 *
 * A rule compiled ahead of time throws from a position in the flattened bundle —
 * `matches@entry.js:957:16` — and `entry.js` is not a file anybody can open. The sidecar map
 * beside the component is what turns that back into a line in the author's TypeScript, and it is
 * written here because this is the only place the mapping exists: jco returns the bundled *code*
 * and discards everything else rolldown produced.
 *
 * That makes this configuration single-purpose, which it already was — `resolveId` below refuses
 * any entry but `entry.ts` — so the output path is written out rather than passed in.
 *
 * # Why a bundler needs a resolver of lanekeep's own
 *
 * A rule's imports have always been resolved at *run time*, by
 * `crates/lanekeep-js/src/loader.rs`, whose rules are the point rather than the mechanism: only
 * `lanekeep`, `lanekeep/<built-in>` and relative paths inside the rules root resolve, a
 * built-in cannot be shadowed by a file in the project, and nothing outside the root can be
 * reached even through a symlink. A rule compiled ahead of time into a component has no run
 * time to enforce any of that in — the bundler decides what every import means before anything
 * runs — so the same rules are enforced here or they are not enforced at all.
 *
 * They are not restated here. `packages/lanekeep/runtime/resolve.js` is the port, its refusal
 * messages are `ResolveError`'s word for word, and its tests are ported from `loader.rs`'s one
 * for one. This file is that resolver wired into rolldown's two hooks.
 *
 * # `load` re-confines rather than trusting `resolveId`
 *
 * `RuleRoot::read` re-checks containment rather than trusting its own resolver, on the grounds
 * that the operation which actually touches a file should be the one enforcing the boundary —
 * otherwise the guarantee holds only while every caller goes through the resolver first, which
 * is an assumption that survives exactly until someone adds a second caller. rolldown will hand
 * `load` any id another plugin resolved, so the same reasoning applies with more force here.
 * `confine` is exported for this.
 *
 * # Two roots, and only one of them is confined
 *
 * The *rules* root is `crates/lanekeep-rules/`, and everything under it — the entry, the four
 * rule sources, `modules/paths.ts` — is resolved and read through the resolver above.
 *
 * `packages/lanekeep/` is lanekeep's own authoring package rather than rule code: the runtime
 * that `withhold()` and the `ctx` translation live in, and the `defineRule` identity function
 * whose only job is to give a type checker somewhere to hang an inference. Its modules resolve
 * by ordinary Node resolution, because confining lanekeep's own runtime to the rules directory
 * would be a category error — it is not in it and must not be.
 *
 * What keeps that from being a hole is that *rule* code cannot name it. `lanekeep/runtime/entry`
 * is admitted for the entry module alone; a rule that imports it is refused by the resolver like
 * any other unknown built-in, which is what the sandbox does with the same specifier.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, relative, resolve as resolvePath, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

import { confine, resolve } from '../../../packages/lanekeep/runtime/resolve.js'

/** This directory: `crates/lanekeep-rules/typescript/`. */
const here = dirname(fileURLToPath(import.meta.url))

/**
 * The rules root every rule source is confined to.
 *
 * `crates/lanekeep-rules/`, so that `../rules/no-default-export.ts` and `modules/paths.ts` are
 * inside it and everything else in the repository is outside — including the package below.
 */
const rulesRoot = resolvePath(here, '..')

/** lanekeep's own authoring package, which is not rule code. */
const packageRoot = resolvePath(here, '../../../packages/lanekeep')

/**
 * The repository root, which every path in the written map is relative to.
 *
 * Not rolldown's own choice, which is the output directory — `crates/lanekeep-rules/typescript`,
 * giving `../../rules/no-restricted-imports.ts`. A frame shown to a user has to name a path that
 * means the same thing to them, to a `git grep` and to an editor, and the only such spelling is
 * relative to the repository. Rewritten at the build rather than at the read, so the shipped map
 * says what it means and nothing in Rust has to know where this directory sits.
 */
const repositoryRoot = resolvePath(here, '../../..')

/**
 * Where the sidecar map is written.
 *
 * Beside the component, named after it, which is the convention `lanekeep-wasm` reads and the one
 * every bundler already uses. `just typescript-builtins` records both files' digests.
 */
const sourceMapOut = join(rulesRoot, 'components/typescript-builtins.wasm.map')

/** The entry module, which is the only module allowed to import the runtime. */
const entry = join(here, 'entry.ts')

/**
 * The specifiers the entry module may use that a rule may not.
 *
 * One, and it is the runtime the generated entry exists to register rules with. A rule that
 * writes this specifier gets the resolver's ordinary refusal, which is what the sandbox gives it
 * too.
 */
const entryOnly = { 'lanekeep/runtime/entry': join(packageRoot, 'runtime/entry.js') }

/**
 * What `lanekeep/<name>` may mean, as `name -> file`.
 *
 * `lanekeep_rules::BUILT_IN_MODULES` is the same table on the Rust side, and it has the same one
 * entry. A shared module is not a rule: it has no id, no card and no query, and it is here
 * because two of the four built-ins import `resolveImport` from it.
 *
 * **Rules are deliberately absent**, and `builtins.components` is deliberately not supplied
 * either. Nothing in this bundle may import a rule — the entry names its four by relative path,
 * because they are files in this repository rather than things the host provides — so every
 * other `lanekeep/<name>` is refused. Naming the rules here would be a second copy of a table
 * that already exists in Rust, kept honest by nothing, in order to admit an import that must
 * not resolve.
 */
const modules = { paths: join(rulesRoot, 'modules/paths.ts') }

/**
 * The host module: `defineRule` and `defineConfig`, which are identity functions.
 *
 * The published package's own `index.js`, rather than a synthetic module written here to match
 * `loader.rs`'s `HOST_MODULE_SOURCE`. That file exists for exactly this — its own comment says
 * it is there "so that a tool which *does* load a rule under Node (a bundler, a test runner, an
 * editor's type server following the import) finds something coherent" — and a third copy of two
 * identity functions is a third thing to keep in step. It is CommonJS, which rolldown handles;
 * the bundle carries its body and calls `defineRule` through it.
 */
const hostModule = join(packageRoot, 'index.js')

/**
 * Whether a path is inside lanekeep's own authoring package.
 *
 * By component rather than by prefix, for the reason `resolve.js` compares that way: a string
 * prefix would accept a sibling directory whose name merely starts the same.
 */
function inPackage(path) {
  return typeof path === 'string' && (path === packageRoot || path.startsWith(packageRoot + sep))
}

/** lanekeep's resolution rules, as a rolldown plugin. */
const lanekeepRules = {
  name: 'lanekeep-rules',

  resolveId(specifier, importer) {
    // lanekeep's own runtime resolves the way any Node package does. It is not rule code and
    // it does not live in the rules root, so putting it through the confinement check below
    // would refuse `./host.js` for being outside a directory it was never in.
    if (inPackage(importer)) return null

    // The one call with no importer is the one rolldown makes for its own input, and `entry`
    // is what everything below is relative to. Checked rather than assumed: pointed at another
    // file, this config refuses `lanekeep/runtime/entry` — a refusal that is correct, since a
    // rule may not import the runtime, and that reads as though the specifier were misspelled.
    if (importer === undefined && specifier !== entry) {
      throw new Error(
        `this configuration is written for ${entry} and jco was given ${specifier}. The entry ` +
          'module is the only one allowed to import `lanekeep/runtime/entry`, so building a ' +
          'different file with it fails later, and about the wrong thing.',
      )
    }

    if ((importer === undefined || importer === entry) && specifier in entryOnly) {
      return entryOnly[specifier]
    }

    const resolved = resolve(specifier, importer ?? '', {
      rulesRoot,
      builtins: { modules: Object.keys(modules) },
    })

    if (resolved.error !== undefined) {
      // Verbatim, because these messages are `ResolveError`'s: a rule refused by this build and
      // the same rule refused by the sandbox have to read identically, or an author who hits
      // one and searches for the other finds nothing.
      throw new Error(resolved.error.message)
    }

    if (resolved.builtin === 'lanekeep') return hostModule
    if (resolved.builtin !== undefined) return modules[resolved.builtin.slice('lanekeep/'.length)]

    return resolved.path
  },

  load(id) {
    if (inPackage(id)) return null

    const confined = confine(id, { rulesRoot })
    if (confined.error !== undefined) throw new Error(confined.error.message)

    return readFileSync(confined.path, 'utf8')
  },
}

/**
 * One source path, as the shipped map spells it: relative to the repository root, with `/`.
 *
 * Rolldown's own spelling is relative to the output directory, which for a `generate()` with no
 * `dir` is `<cwd>/dist` — a directory that does not exist, two levels from anything real, and not
 * something to derive by counting. `sourcemapPathTransform` is handed the map's own path, so the
 * base is read rather than guessed.
 *
 * @param {string} relativeSourcePath As rolldown wrote it.
 * @param {string} sourcemapPath The absolute path rolldown would have written the map to.
 */
function repositoryRelative(relativeSourcePath, sourcemapPath) {
  const absolute = resolvePath(dirname(sourcemapPath), relativeSourcePath)
  const fromRoot = relative(repositoryRoot, absolute)
  if (fromRoot.startsWith('..')) {
    throw new Error(
      `the source map names ${absolute}, which is outside this repository — a frame pointing ` +
        'there would name a path the reader does not have',
    )
  }
  return fromRoot.split(sep).join('/')
}

/**
 * The map rolldown produced, written beside the component and taken out of the bundle.
 *
 * Two things, and the second is not optional. jco refuses an output containing any asset —
 * "Component bundling produced unsupported assets" — and enabling a source map produces exactly
 * one, `entry.js.map`. So the asset is deleted here rather than emitted: the map leaves through
 * this plugin's own write or it does not leave at all.
 *
 * `sourcesContent` is dropped. The sources are files in this repository, so embedding a second
 * copy of 56 KB of them would put a rule's text somewhere it can go stale against the file the
 * frame names — and the frame naming a real path is the whole point.
 */
const lanekeepSourceMap = {
  name: 'lanekeep-source-map',

  generateBundle(_options, bundle) {
    let written = false

    for (const [fileName, item] of Object.entries(bundle)) {
      if (fileName.endsWith('.map')) {
        delete bundle[fileName]
        continue
      }
      if (item.type !== 'chunk' || !item.isEntry || !item.map) continue

      const map =
        typeof item.map.toString === 'function' ? JSON.parse(item.map.toString()) : item.map
      delete map.sourcesContent

      writeFileSync(sourceMapOut, `${JSON.stringify(map)}\n`)
      written = true
    }

    // A silent absence here is a component whose failures report positions in `entry.js` for
    // the rest of its life, with nothing red — the build is the only place that can notice.
    if (!written) {
      throw new Error(
        `no entry chunk carried a source map, so ${sourceMapOut} was not written. ` +
          'The `output.sourcemap` below is what produces one.',
      )
    }
  },
}

export default {
  plugins: [lanekeepRules, lanekeepSourceMap],
  output: { sourcemap: 'hidden', sourcemapPathTransform: repositoryRelative },
}
