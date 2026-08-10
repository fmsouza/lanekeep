// The resolver's specification, ported from `crates/lanekeep-js/src/loader.rs`.
//
// Every test here names the `loader.rs` test it came from. That crate's unit tests are the
// authority: the sandbox has enforced these rules at run time since the beginning, and this
// resolver exists to enforce the same ones at build time, where a bundler decides what a
// rule's imports mean. Two sets of confinement rules would be two things to keep right, and
// the second one is always the one that is wrong.
//
// Where a name here has no `loader.rs` counterpart it says so, and says what it is derived
// from instead — `candidates()`'s ordering, or the property the task brief names.

import { strict as assert } from 'node:assert'
import { existsSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { dirname, join, sep } from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'

import { confine, resolve } from './resolve.js'
import * as host from '../index.js'

/**
 * A rules directory laid out for a test, removed when the test ends.
 *
 * Named after the test rather than after its contents, because two tests writing the same
 * bytes still race: `writeFileSync` truncates before it writes, so a sibling reading a
 * content-keyed path can see an empty file. `AGENTS.md` carries the measurement.
 */
function fixture(t, name, files = {}) {
  const dir = join(tmpdir(), `lanekeep-resolve-${name}`)
  rmSync(dir, { recursive: true, force: true })
  mkdirSync(dir, { recursive: true })
  for (const [path, contents] of Object.entries(files)) {
    const full = join(dir, path)
    mkdirSync(dirname(full), { recursive: true })
    writeFileSync(full, contents)
  }
  t.after(() => rmSync(dir, { recursive: true, force: true }))

  return {
    dir,
    // Canonical, because an importer is compared against the canonical root. This is what
    // `Fixture::entry` does.
    entry: (relative) => realpathSync.native(join(dir, relative)),
  }
}

/** Stands in for the real built-in table, so these tests do not depend on what ships. */
const BUILTINS = { modules: ['always'], components: [] }

/** The same, with a built-in that ships as a component rather than as a module. */
const BUILTINS_WITH_COMPONENT = { modules: ['always'], components: ['compiled'] }

// --- built-ins ----------------------------------------------------------------------

// from `resolves_a_built_in_by_specifier`
test('resolves a built-in by specifier', (t) => {
  const f = fixture(t, 'builtin-resolve', { 'a.ts': 'export const a = 1;' })
  assert.deepEqual(resolve('lanekeep/always', '', { rulesRoot: f.dir, builtins: BUILTINS }), {
    builtin: 'lanekeep/always',
  })
})

// from `an_unknown_built_in_is_not_found`
test('an unknown built-in is not found', (t) => {
  // Not "bare specifier": the `lanekeep/` prefix says what the author meant, and an error
  // about npm resolution would send them somewhere useless.
  const f = fixture(t, 'builtin-unknown', { 'a.ts': 'export const a = 1;' })
  const { error } = resolve('lanekeep/no-such-rule', '', {
    rulesRoot: f.dir,
    builtins: BUILTINS,
  })
  assert.equal(error.code, 'not-found')
  assert.match(error.message, /built-in/)
})

// from `a_built_in_that_is_a_component_is_refused_as_a_module`
test('a built-in that is a component is refused as a module', (t) => {
  // And refused *as itself*. A component-backed built-in is spelled correctly, so the "no
  // built-in rule by that name" message would send its author looking for a misspelling that
  // is not there. The two facts are different and only one is a typo.
  const f = fixture(t, 'builtin-component', { 'a.ts': 'export const a = 1;' })
  const { error } = resolve('lanekeep/compiled', '', {
    rulesRoot: f.dir,
    builtins: BUILTINS_WITH_COMPONENT,
  })
  assert.equal(error.code, 'not-a-module')
  assert.equal(error.name, 'compiled')
  assert.match(error.message, /lanekeep\/compiled/)
  assert.match(error.message, /component/)
  assert.match(
    error.message,
    /lanekeep\.json/,
    'the message has to name the format that can reach it',
  )
})

// from `an_unknown_name_is_still_not_found_when_components_ship`
test('an unknown name is still not found when components ship', (t) => {
  // The component lookup must not turn every miss into "it is a component".
  const f = fixture(t, 'builtin-component-miss', { 'a.ts': 'export const a = 1;' })
  const { error } = resolve('lanekeep/no-such-rule', '', {
    rulesRoot: f.dir,
    builtins: BUILTINS_WITH_COMPONENT,
  })
  assert.equal(error.code, 'not-found')
})

// from `a_file_cannot_shadow_a_built_in`
test('a file cannot shadow a built-in', (t) => {
  // A rules directory containing `lanekeep/always.ts` must not change what the specifier
  // means. A rule whose behavior depended on whether a same-named file happened to exist
  // would be unreasonable to debug.
  const f = fixture(t, 'builtin-shadow', {
    'lanekeep/always.ts': "export default 'the wrong one';",
  })
  assert.deepEqual(resolve('lanekeep/always', '', { rulesRoot: f.dir, builtins: BUILTINS }), {
    builtin: 'lanekeep/always',
  })
})

// from `built_ins_are_absent_unless_provided`
test('built-ins are absent unless provided', (t) => {
  // The default. A caller wiring this resolver without a built-in table resolves project
  // modules only, rather than silently resolving names to nothing.
  //
  // `built_ins_are_absent_unless_provided` asserts only that this is an error, which any
  // resolver that fails at everything satisfies — so the code is named here as well.
  const f = fixture(t, 'builtin-default', { 'a.ts': 'export const a = 1;' })
  const { error } = resolve('lanekeep/always', '', { rulesRoot: f.dir })
  assert.equal(error?.code, 'not-found', JSON.stringify(error))
})

// from `resolves_the_host_module`
test('resolves the host module', (t) => {
  const f = fixture(t, 'host', { 'a.ts': 'export const a = 1;' })
  assert.deepEqual(resolve('lanekeep', '', { rulesRoot: f.dir }), { builtin: 'lanekeep' })
})

// No `loader.rs` counterpart: there, "before the filesystem" is a property of where the
// built-in branch sits in `RuleRoot::resolve`, and `a_file_cannot_shadow_a_built_in` is as
// close as it gets to asserting it. Stated directly here because it is the property the task
// brief names, and because a resolver that stats the root first would pass that test.
test('lanekeep and lanekeep/* resolve before the filesystem is consulted', (t) => {
  const missing = join(tmpdir(), 'lanekeep-resolve-no-such-root')
  rmSync(missing, { recursive: true, force: true })
  t.after(() => rmSync(missing, { recursive: true, force: true }))
  assert.equal(existsSync(missing), false)

  assert.deepEqual(resolve('lanekeep', '', { rulesRoot: missing }), { builtin: 'lanekeep' })
  assert.deepEqual(resolve('lanekeep/always', '', { rulesRoot: missing, builtins: BUILTINS }), {
    builtin: 'lanekeep/always',
  })

  // And a project file named exactly like the host module does not get a look in either.
  const f = fixture(t, 'host-shadow', { 'lanekeep.ts': "export default 'the wrong one';" })
  assert.deepEqual(resolve('lanekeep', '', { rulesRoot: f.dir }), { builtin: 'lanekeep' })
})

// from `the_host_module_exports_the_authoring_helpers`
test('the host module exports the authoring helpers', () => {
  // `loader.rs` reads `HOST_MODULE_SOURCE`; under Node the same module is `index.js`, which
  // is what a bundler following `import { defineRule } from 'lanekeep'` reaches.
  assert.equal(typeof host.defineRule, 'function')
  assert.equal(typeof host.defineConfig, 'function')
})

// --- what resolves ------------------------------------------------------------------

// from `resolves_a_relative_import`
test('resolves a relative import', (t) => {
  const f = fixture(t, 'relative', {
    'main.ts': "import './helper';",
    'helper.ts': 'export const h = 1;',
  })
  const { path } = resolve('./helper', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.ok(path.endsWith(`${sep}helper.ts`), path)
})

// from `tries_extensions_in_order`
test('tries extensions in order', (t) => {
  // A `.ts` file wins over a `.js` file of the same name, because a rule directory
  // containing both is almost always a stale build artifact next to its source.
  const f = fixture(t, 'extensions', {
    'main.ts': '',
    'dup.ts': "export const from = 'ts';",
    'dup.js': "export const from = 'js';",
  })
  const { path } = resolve('./dup', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.ok(path.endsWith(`${sep}dup.ts`), `expected the TypeScript file: ${path}`)
})

// No `loader.rs` counterpart: `tries_extensions_in_order` pins only `ts` over `js`, so three
// of the five could be reordered with nothing red. The order is `EXTENSIONS` in `loader.rs`.
test('every extension is tried in the fixed order', (t) => {
  const order = ['ts', 'tsx', 'js', 'jsx', 'mjs']
  const files = { 'main.ts': '' }
  for (const extension of order) files[`dup.${extension}`] = `export const from = '${extension}';`

  const f = fixture(t, 'extension-order', files)
  for (const expected of order) {
    const { path } = resolve('./dup', f.entry('main.ts'), { rulesRoot: f.dir })
    assert.ok(path.endsWith(`${sep}dup.${expected}`), `expected dup.${expected}, got ${path}`)
    // Remove the winner and the next one down must take its place.
    rmSync(path)
  }
})

// from `resolves_a_directory_index`
test('resolves a directory index', (t) => {
  const f = fixture(t, 'index', { 'main.ts': '', 'rules/index.ts': 'export const r = 1;' })
  const { path } = resolve('./rules', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.ok(path.endsWith(`${sep}index.ts`), path)
})

// No `loader.rs` counterpart: `candidates()` puts every extension ahead of every
// `index.<ext>`, and no test holds it there. The brief names it as a property to preserve.
test('a file beats a same-named directory index', (t) => {
  const f = fixture(t, 'file-over-index', {
    'main.ts': '',
    'rules.ts': "export const from = 'file';",
    'rules/index.ts': "export const from = 'index';",
  })
  const { path } = resolve('./rules', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.ok(path.endsWith(`${sep}rules.ts`), `expected the file, not the directory: ${path}`)
})

// from `resolves_an_explicit_extension`
test('resolves an explicit extension', (t) => {
  const f = fixture(t, 'explicit', { 'main.ts': '', 'helper.ts': 'export const h = 1;' })
  const { path } = resolve('./helper.ts', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.ok(path.endsWith(`${sep}helper.ts`), path)
})

// --- what must not resolve ----------------------------------------------------------

// from `rejects_bare_specifiers`
test('rejects bare specifiers', (t) => {
  const f = fixture(t, 'bare', { 'main.ts': '' })
  for (const specifier of ['lodash', 'react', 'node:fs', 'fs', '@scope/pkg', 'typescript']) {
    const { error } = resolve(specifier, f.entry('main.ts'), { rulesRoot: f.dir })
    assert.equal(error?.code, 'bare-specifier', `${specifier} gave ${JSON.stringify(error)}`)
  }
})

// from `a_bare_specifier_explains_why`
test('a bare specifier explains why', (t) => {
  const f = fixture(t, 'bare-msg', { 'main.ts': '' })
  const { error } = resolve('lodash', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.equal(error.code, 'bare-specifier')
  assert.match(error.message, /no package resolution/)
  assert.match(error.message, /lanekeep/)
})

// from `rejects_traversal_out_of_the_root`
test('rejects traversal out of the root', (t) => {
  const f = fixture(t, 'traversal', { 'main.ts': '' })
  const importer = f.entry('main.ts')
  for (const specifier of ['../outside', '../../etc/passwd', './../../secrets', '../']) {
    const { error } = resolve(specifier, importer, { rulesRoot: f.dir })
    assert.equal(error?.code, 'escapes-root', `${specifier} gave ${JSON.stringify(error)}`)
  }
})

// from `traversal_is_rejected_even_when_the_target_exists`
test('traversal is rejected even when the target exists', (t) => {
  // The lexical check has to fire regardless of what is on disk, or the error a reader sees
  // depends on whether the file they tried to reach happened to be there.
  const f = fixture(t, 'traversal-real', {
    'nested/main.ts': '',
    'secret.ts': 'export const s = 1;',
  })
  const { error } = resolve('../secret', f.entry('nested/main.ts'), {
    rulesRoot: join(f.dir, 'nested'),
  })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))
})

// from `rejects_a_symlink_pointing_outside_the_root`
test('rejects a symlink pointing outside the root', { skip: process.platform === 'win32' }, (t) => {
  // The case a lexical check cannot see. `./link` looks entirely innocent; only
  // canonicalizing the file that was found reveals where it goes.
  const f = fixture(t, 'symlink', { 'nested/main.ts': '', 'outside.ts': 'export const o = 1;' })
  symlinkSync(join(f.dir, 'outside.ts'), join(f.dir, 'nested', 'link.ts'))

  const { error } = resolve('./link', f.entry('nested/main.ts'), {
    rulesRoot: join(f.dir, 'nested'),
  })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))
})

// from `a_rule_may_not_import_an_absolute_path`
test('a rule may not import an absolute path', (t) => {
  // The entry module legitimately arrives as an absolute path, so the resolver has to accept
  // one. This checks that door is only open for the entry — a module with an importer of its
  // own is refused.
  //
  // The absolute path is built from `tmpdir()` rather than written literally, because what
  // counts as absolute is platform-specific.
  const f = fixture(t, 'absolute', { 'main.ts': '', 'rule.ts': 'export const r = 1;' })
  const importer = f.entry('main.ts')
  const outside = join(tmpdir(), 'lanekeep-absolute-probe.ts')

  // Written by a rule: refused, whichever way the platform classifies it.
  for (const specifier of [outside, '/etc/passwd', 'C:\\Windows\\System32\\x']) {
    assert.ok(
      resolve(specifier, importer, { rulesRoot: f.dir }).error,
      `a rule must not import \`${specifier}\``,
    )
  }

  // As an entry point: still refused, because it is outside the root.
  const { error } = resolve(outside, '', { rulesRoot: f.dir })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))

  // And the door the entry actually needs: an absolute path inside the root, with nothing
  // importing it, resolves.
  const { path } = resolve(f.entry('rule.ts'), '', { rulesRoot: f.dir })
  assert.ok(path?.endsWith(`${sep}rule.ts`), path)
})

// No `loader.rs` counterpart, and there could not usefully be one: Rust gets this from
// `Path::starts_with`, which compares components. In JavaScript it is code, and code regresses
// — a plain `path.startsWith(root)` passes every other test in this file while resolving
// `../root-evil/secret` from a root named `root` to a file outside it.
test('a sibling directory whose name extends the root is outside it', (t) => {
  const f = fixture(t, 'sibling-prefix', {
    'root/main.ts': '',
    'root-evil/secret.ts': 'export const s = 1;',
  })
  const root = join(f.dir, 'root')

  const resolved = resolve('../root-evil/secret', f.entry('root/main.ts'), { rulesRoot: root })
  assert.equal(resolved.error?.code, 'escapes-root', JSON.stringify(resolved))

  // The same trap through the other entry point, whose lexical gate is its own.
  const confined = confine(join(f.dir, 'root-evil', 'secret.ts'), { rulesRoot: root })
  assert.equal(confined.error?.code, 'escapes-root', JSON.stringify(confined))
})

// No `loader.rs` counterpart: `RuleRoot` canonicalizes its root once at construction, so a
// base and a root spelled differently cannot arise there. Here every call takes a string.
test('the root and the importer may be spelled either way', (t) => {
  const f = fixture(t, 'spelling', { 'entry.ts': '', 'helper.ts': 'export const h = 1;' })

  // `f.dir` is `/var/…` on macOS and canonically `/private/var/…`. A caller who configured
  // the first and built an entry path from it must not be told it escapes.
  assert.notEqual(f.dir, realpathSync.native(f.dir), 'this fixture needs the two to differ')

  const entry = resolve(join(f.dir, 'entry.ts'), '', { rulesRoot: f.dir })
  assert.equal(entry.path, f.entry('entry.ts'), JSON.stringify(entry))

  // And an importer spelled the same way the caller spelled the root.
  const sibling = resolve('./helper', join(f.dir, 'entry.ts'), { rulesRoot: f.dir })
  assert.equal(sibling.path, f.entry('helper.ts'), JSON.stringify(sibling))

  // Whatever the spelling, a traversal out is still refused.
  const out = resolve('../elsewhere', join(f.dir, 'entry.ts'), { rulesRoot: f.dir })
  assert.equal(out.error?.code, 'escapes-root', JSON.stringify(out))
})

// from `reports_what_it_tried_when_nothing_matches`
test('reports what it tried when nothing matches', (t) => {
  const f = fixture(t, 'missing', { 'main.ts': '' })
  const { error } = resolve('./nope', f.entry('main.ts'), { rulesRoot: f.dir })
  assert.equal(error.code, 'not-found')
  assert.match(error.message, /nope\.ts/, `should list candidates: ${error.message}`)
  assert.match(error.message, /index\.ts/, `should list index candidates: ${error.message}`)
})

// --- confinement on its own ---------------------------------------------------------
//
// `confine` is the containment rules with the resolution taken away, for whatever ends up
// reading a rule's bytes. `RuleRoot::read` re-checks rather than trusting `RuleRoot::resolve`,
// because the operation that touches a file should be the one enforcing the boundary; the
// build-time reader has to be able to do the same without reimplementing it.

// from `RuleRoot::confine`'s success path, which `resolve` reaches through every time
test('confine returns the canonical path of a file inside the root', (t) => {
  const f = fixture(t, 'confine-ok', { 'a.ts': 'export const a = 1;' })
  const { path } = confine(join(f.dir, 'a.ts'), { rulesRoot: f.dir })
  assert.equal(path, f.entry('a.ts'))
})

// from `reading_refuses_a_path_outside_the_root_even_if_resolution_was_skipped`
test('confine refuses a path outside the root even if resolution was skipped', (t) => {
  // Defense in depth. `resolve` already enforces this, but a caller that builds a path some
  // other way must not be able to read past the boundary.
  const f = fixture(t, 'confine-escape', {
    'nested/main.ts': '',
    'outside.ts': 'export const o = 1;',
  })
  const { error } = confine(join(f.dir, 'outside.ts'), { rulesRoot: join(f.dir, 'nested') })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))
})

// from `rejects_a_symlink_pointing_outside_the_root`, applied to `confine` rather than to
// `resolve`: the canonical check is the half of this that a lexical one cannot do.
test('confine sees through a symlink out of the root', { skip: process.platform === 'win32' }, (t) => {
  const f = fixture(t, 'confine-symlink', {
    'nested/main.ts': '',
    'outside.ts': 'export const o = 1;',
  })
  const link = join(f.dir, 'nested', 'link.ts')
  symlinkSync(join(f.dir, 'outside.ts'), link)

  const { error } = confine(link, { rulesRoot: join(f.dir, 'nested') })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))
})

// No `loader.rs` counterpart: `RuleRoot::confine` leaves normalizing to its callers, and every
// one of them does it. The exported function does it itself — see the note at its definition.
test('confine refuses a traversal lexically, whatever is on disk', (t) => {
  const f = fixture(t, 'confine-traversal', { 'nested/main.ts': '' })
  const root = join(f.dir, 'nested')

  // Concatenated rather than `join`ed, and that is the whole test. `path.join` normalizes its
  // own arguments, so `join(root, '..', 'secret.ts')` hands over an already-resolved path and
  // asserts nothing about `confine` doing it — a mutation removing the normalizing left this
  // green until the fixture was built by hand.
  const traversal = `${root}${sep}..${sep}secret.ts`
  assert.ok(traversal.includes(`${sep}..${sep}`), 'the fixture must still carry the traversal')

  // Nothing is there, so only the lexical check can answer. Unnormalized, this path starts
  // with `<root>/` as a *string*, so without normalizing it reaches the canonical check and
  // comes back `unreadable` — a fact about the filesystem rather than about the mistake.
  const { error } = confine(traversal, { rulesRoot: root })
  assert.equal(error?.code, 'escapes-root', JSON.stringify(error))
})

test('confine reports a missing file inside the root as unreadable', (t) => {
  const f = fixture(t, 'confine-missing', { 'a.ts': '' })
  const { error } = confine(join(f.dir, 'nope.ts'), { rulesRoot: f.dir })
  assert.equal(error?.code, 'unreadable', JSON.stringify(error))
})

// No `loader.rs` counterpart: `RuleRoot::confine` canonicalizes the path it was given, so this
// cannot arise there. It arises here only because this function normalizes, and it is why the
// normalized copy goes to the lexical gate and nowhere else.
test('confine canonicalizes the path as written, not a lexically resolved copy', {
  skip: process.platform === 'win32',
}, (t) => {
  // Both files exist and they are different files. Resolving `..` lexically picks the one
  // inside the root and returns it; resolving it through `realpath` picks the one outside and
  // refuses. Handing back the wrong file quietly is worse than either.
  const f = fixture(t, 'confine-symlink-traversal', {
    'root/secret.ts': "export const from = 'inside';",
    'outside/dir/placeholder.ts': '',
    'outside/secret.ts': "export const from = 'outside';",
  })
  const root = join(f.dir, 'root')
  symlinkSync(join(f.dir, 'outside', 'dir'), join(root, 'link'))

  const answer = confine(`${root}${sep}link${sep}..${sep}secret.ts`, { rulesRoot: root })
  assert.equal(
    answer.error?.code,
    'escapes-root',
    `resolving .. across a symlink must not hand back the file inside: ${JSON.stringify(answer)}`,
  )
})

// The deliberate difference from `RuleRoot::confine`, pinned so it is a decision rather than
// an accident. See the note at `confine`'s definition.
test('confine refuses a traversal that only lands inside by way of a symlink', {
  skip: process.platform === 'win32',
}, (t) => {
  const f = fixture(t, 'confine-symlink-inward', {
    'root/a/b/placeholder.ts': '',
    'root/x.ts': "export const from = 'inside';",
  })
  const root = join(f.dir, 'root')
  symlinkSync(join(root, 'a', 'b'), join(root, 'link'))

  // `realpath` lands on `<root>/x.ts`, which is inside, so `RuleRoot::confine` would accept.
  // Normalizing lexically lands outside, and this refuses. Fail-closed, and on purpose.
  const answer = confine(`${root}${sep}link${sep}..${sep}..${sep}x.ts`, { rulesRoot: root })
  assert.equal(answer.error?.code, 'escapes-root', JSON.stringify(answer))
})
