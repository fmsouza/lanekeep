// The driver's own tests, run by `node --test` through `just test-js`.
//
// Skipped where the authoring package's `typescript` is not installed, on `test-js-types`'
// terms and for its reasons: `npm ci --prefix packages/lanekeep` is not part of the Rust
// toolchain a contributor already has. CI's gate job installs it, so this is a real check
// there. A skip prints why — a suite that quietly checks nothing is the failure `just
// test-js`'s three guards exist to prevent.
import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import {
  existsSync,
  mkdtempSync,
  writeFileSync,
  readFileSync,
  realpathSync,
  rmSync,
  mkdirSync,
  symlinkSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const repo = path.resolve(here, '../../../..')
const typescript = path.join(repo, 'packages/lanekeep/node_modules/typescript')
const driver = path.join(here, 'driver.mjs')
const available = existsSync(path.join(typescript, 'package.json'))

// `node:test` has no default timeout, and every request here is a promise that only ever
// settles when a *matching* answer arrives. A driver that dies at module scope — the exact
// regression the last two tests pin — therefore hangs the suite instead of failing it, and a
// hung suite in CI reads as an infrastructure problem rather than as this bug. Every test
// carries this, and `spawnDriver` below rejects whatever is in flight when its child exits, so
// the common case fails in milliseconds and this is only the backstop.
const TIMEOUT = { timeout: 30_000 }

let project
let child
let lines
let pending
let nextId = 0
let closingShared = false

function ask(op, extra = {}) {
  const id = ++nextId
  const wait = new Promise((resolve, reject) => pending.set(id, { resolve, reject }))
  child.stdin.write(`${JSON.stringify({ id, op, ...extra })}\n`)
  return wait
}

before(() => {
  if (!available) return
  project = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-'))
  mkdirSync(path.join(project, 'src'))
  writeFileSync(path.join(project, 'package.json'), '{"name":"fixture","private":true}\n')
  writeFileSync(
    path.join(project, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  writeFileSync(
    path.join(project, 'src/lib.ts'),
    'export class Decimal {}\nexport type Amount = number\n',
  )
  writeFileSync(
    path.join(project, 'src/a.ts'),
    'import { Decimal } from "./lib"\n' +
      'const money = new Decimal()\n' +
      'const count: number = 1\n' +
      'declare const mixed: string | number\n' +
      'export { money, count, mixed }\n',
  )
  child = spawn(process.execPath, [driver, project, typescript], {
    stdio: ['pipe', 'pipe', 'inherit'],
  })
  pending = new Map()
  lines = createInterface({ input: child.stdout })
  lines.on('line', (line) => {
    const response = JSON.parse(line)
    const entry = pending.get(response.id)
    pending.delete(response.id)
    if (entry) entry.resolve(response)
  })
  // As in `spawnDriver` below: a dead child answers nothing, so anything in flight is rejected
  // rather than left pending forever.
  child.on('exit', (code, signal) => {
    if (closingShared) return
    for (const [id, entry] of pending) {
      entry.reject(new Error(`the shared driver exited (code ${code}, signal ${signal}) with request ${id} in flight`))
    }
    pending.clear()
  })
})

after(() => {
  if (!available) return
  closingShared = true
  child.kill()
  rmSync(project, { recursive: true, force: true })
})

function spanOf(source, needle) {
  const start = source.indexOf(needle)
  return { start, end: start + needle.length }
}

test('hello answers the loaded typescript version', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const response = await ask('hello')
  assert.equal(response.ok, true)
  assert.match(response.value.typescript, /^\d+\.\d+\.\d+/)
})

test('typeOf answers a primitive', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const file = path.join(project, 'src/a.ts')
  const source = readFileSync(file, 'utf8')
  const response = await ask('typeOf', { file, ...spanOf(source, 'count: number') })
  assert.equal(response.ok, true, JSON.stringify(response))
  assert.equal(response.value.primitive, 'number')
})

test('typeOf flattens and sorts a union', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const file = path.join(project, 'src/a.ts')
  const source = readFileSync(file, 'utf8')
  const response = await ask('typeOf', { file, ...spanOf(source, 'mixed: string | number') })
  assert.equal(response.ok, true, JSON.stringify(response))
  assert.deepEqual(
    response.value.union.map((member) => member.primitive),
    ['number', 'string'],
    'sorted by normalized text, so the answer does not depend on how the union was written',
  )
})

test('symbolOf resolves an import to the module as written', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const file = path.join(project, 'src/a.ts')
  const source = readFileSync(file, 'utf8')
  const response = await ask('symbolOf', { file, ...spanOf(source, 'Decimal()') })
  assert.equal(response.ok, true, JSON.stringify(response))
  assert.equal(response.value.name, 'Decimal')
  assert.equal(response.value.module, './lib')
  assert.equal(response.value.exported, 'Decimal')
})

test('programs is stable across two calls', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const files = [path.join(project, 'src/a.ts'), path.join(project, 'src/lib.ts')]
  const first = await ask('programs', { files })
  const second = await ask('programs', { files })
  assert.equal(first.ok, true, JSON.stringify(first))
  assert.deepEqual(first.value, second.value)
  assert.ok(first.value.listing.length >= 2, 'both fixture files are in some program')
  assert.deepEqual(
    first.value.listing.map(([p]) => p),
    [...first.value.listing.map(([p]) => p)].sort(),
    'sorted by path',
  )
  assert.ok(
    !first.value.listing.some(([p]) => p.includes('node_modules/typescript/lib/')),
    "the typescript package's own lib is excluded — the version covers it",
  )
})

test('complete is false when an import resolves to nothing', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const missing = path.join(project, 'src/broken.ts')
  writeFileSync(missing, 'import { X } from "./absent"\nexport const x: typeof X = X\n')
  const response = await ask('complete', { file: missing })
  assert.equal(response.ok, true, JSON.stringify(response))
  assert.equal(response.value, false)
})

// A driver of its own, for a test that needs a different project root or a different
// `typescript` specifier than the shared fixture's. The shared child above is reused by every
// test that can share it; these cannot, because the two arguments are what is under test.
function spawnDriver(root, specifier = typescript) {
  const proc = spawn(process.execPath, [driver, root, specifier], {
    stdio: ['pipe', 'pipe', 'inherit'],
  })
  const waiting = new Map()
  let id = 0
  let closing = false
  const reader = createInterface({ input: proc.stdout })
  reader.on('line', (line) => {
    const response = JSON.parse(line)
    const entry = waiting.get(response.id)
    waiting.delete(response.id)
    if (entry) entry.resolve(response)
  })
  // A child that dies never answers, so a request waiting on it would wait forever. Rejecting
  // here turns "the driver threw at module scope" into a named failure rather than into the
  // suite's timeout, which says nothing about which child died or why.
  const abandon = (why) => {
    if (closing) return
    for (const [pending, entry] of waiting) {
      entry.reject(new Error(`the driver at ${root} ${why} with request ${pending} in flight`))
    }
    waiting.clear()
  }
  proc.on('exit', (code, signal) => abandon(`exited (code ${code}, signal ${signal})`))
  proc.on('error', (error) => abandon(`could not be started: ${error.message}`))
  return {
    ask(op, extra = {}) {
      const next = ++id
      const wait = new Promise((resolve, reject) => waiting.set(next, { resolve, reject }))
      proc.stdin.write(`${JSON.stringify({ id: next, op, ...extra })}\n`)
      return wait
    },
    close() {
      closing = true
      proc.kill()
    },
  }
}

test('programs lists tsconfig.json and its extends target', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The compiler's answer depends on these two files and on nothing that appears in
  // `getSourceFiles()`, so a listing built from the program alone leaves `strict: true` out of
  // the run key and a warm run answers the previous configuration.
  const dir = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-extends-'))
  const session = spawnDriver(dir)
  try {
    mkdirSync(path.join(dir, 'src'))
    writeFileSync(path.join(dir, 'package.json'), '{"name":"extends","private":true}\n')
    writeFileSync(
      path.join(dir, 'base.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022"}}\n',
    )
    writeFileSync(path.join(dir, 'tsconfig.json'), '{"extends":"./base.json","include":["src"]}\n')
    writeFileSync(path.join(dir, 'src/a.ts'), 'export const n: number = 1\n')
    const response = await session.ask('programs', { files: [path.join(dir, 'src/a.ts')] })
    assert.equal(response.ok, true, JSON.stringify(response))
    const listed = response.value.listing.map(([p]) => p)
    assert.ok(listed.includes('tsconfig.json'), `no tsconfig.json in ${JSON.stringify(listed)}`)
    assert.ok(listed.includes('base.json'), `no extends target in ${JSON.stringify(listed)}`)
  } finally {
    session.close()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a project root that does not resolve is refused loudly', TIMEOUT, async () => {
  // No `available` guard: the driver refuses before it loads anything, so this holds whether
  // or not the authoring package's `typescript` is installed.
  //
  // The regression this pins: `realpath` falls back to the path it was given when nothing is
  // there, and for the *root* that fallback silently gives up the guarantee the realpathing
  // exists for — every `node_modules` resolution then lands outside the root as it was
  // spelled and the listing carries the checkout's location. A root nobody can resolve is not
  // a project, so the driver says so and exits rather than answering with paths that key
  // differently on every machine.
  const missing = path.join(tmpdir(), 'lanekeep-driver-absent-root-does-not-exist')
  rmSync(missing, { recursive: true, force: true })
  const proc = spawn(process.execPath, [driver, missing, typescript], {
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  let said = ''
  proc.stderr.on('data', (chunk) => {
    said += chunk
  })
  // Killed if it is still there after five seconds, so a driver that goes on serving reports
  // as a named failure rather than as a suite that never ends: nothing else here ever reads
  // this child's stdout, so an unrefused root would leave it alive until `node --test` gave up.
  const killer = setTimeout(() => proc.kill('SIGKILL'), 5_000)
  const [code, signal] = await new Promise((resolve) =>
    proc.on('exit', (c, sig) => resolve([c, sig])),
  )
  clearTimeout(killer)
  assert.equal(signal, null, 'the driver had to be killed: a root that does not resolve was served')
  assert.notEqual(code, 0, `the driver survived a root that does not resolve: ${said}`)
  assert.ok(said.includes(missing), `the refusal does not name the root: ${JSON.stringify(said)}`)
})

test('a program file outside the project root is listed relatively', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // An absolute path here would be the first machine-dependent term in the run key, and under
  // pnpm it would be most of the listing.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-outside-'))
  const root = path.join(outer, 'app')
  // Created before the driver is spawned, which is load-bearing rather than tidiness: the
  // driver realpaths its root at startup, so a root that does not exist yet is refused now and
  // used to be served unrealpathed — and an unrealpathed root is exactly the defect the
  // assertion below is looking for, so spawning first made this test unable to see it.
  mkdirSync(path.join(outer, 'shared'), { recursive: true })
  mkdirSync(path.join(root, 'src'), { recursive: true })
  const first = spawnDriver(root)
  try {
    writeFileSync(path.join(outer, 'shared/b.ts'), 'export const b: number = 2\n')
    writeFileSync(path.join(root, 'package.json'), '{"name":"app","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    writeFileSync(
      path.join(root, 'src/a.ts'),
      'import { b } from "../../shared/b"\nexport const n: number = b\n',
    )
    const files = [path.join(root, 'src/a.ts')]
    const answer = await first.ask('programs', { files })
    assert.equal(answer.ok, true, JSON.stringify(answer))
    const listed = answer.value.listing.map(([p]) => p)
    assert.ok(
      listed.includes('../shared/b.ts'),
      `no relative path out of the root in ${JSON.stringify(listed)}`,
    )
    // The one path that leaves the root is the one this test is about. An unrealpathed root
    // puts the whole listing outside itself as `../../../private/var/…`, which `isAbsolute`
    // never sees — every row of that listing is relative and every row carries the machine.
    assert.deepEqual(
      listed.filter((p) => p.startsWith('..')),
      ['../shared/b.ts'],
      `a path climbs out of the root that should not: ${JSON.stringify(listed)}`,
    )
    assert.ok(
      !listed.some((p) => path.isAbsolute(p)),
      `an absolute path survives in ${JSON.stringify(listed)}`,
    )
  } finally {
    first.close()
    rmSync(outer, { recursive: true, force: true })
  }
})

/** A project that imports out of its own `node_modules`, under `dir`. */
function writeDependentProject(dir) {
  mkdirSync(path.join(dir, 'src'), { recursive: true })
  mkdirSync(path.join(dir, 'node_modules/dep'), { recursive: true })
  writeFileSync(path.join(dir, 'package.json'), '{"name":"app","private":true}\n')
  writeFileSync(
    path.join(dir, 'node_modules/dep/package.json'),
    '{"name":"dep","version":"1.0.0","types":"index.d.ts"}\n',
  )
  writeFileSync(path.join(dir, 'node_modules/dep/index.d.ts'), 'export declare const d: number\n')
  writeFileSync(
    path.join(dir, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  writeFileSync(
    path.join(dir, 'src/a.ts'),
    'import { d } from "dep"\nexport const n: number = d\n',
  )
}

test('two byte-identical projects list identically whatever their directories are called', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // TypeScript resolves a `node_modules` specifier through `realpath` (`preserveSymlinks` is
  // false), and on macOS `tmpdir()` is reached through a symlink — so an unrealpathed root
  // puts every resolved dependency *outside* itself and lists it as
  // `../../../private/var/.../<the directory's own name>/node_modules/dep/index.d.ts`. That is
  // the checkout's location in the run key, and under pnpm it is most of a listing. The two
  // roots below differ only in the length of their names, so a listing that carries either
  // name cannot be equal.
  const shortRoot = mkdtempSync(path.join(tmpdir(), 'lk-s-'))
  const longRoot = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-a-much-longer-name-'))
  const first = spawnDriver(shortRoot)
  const second = spawnDriver(longRoot)
  try {
    writeDependentProject(shortRoot)
    writeDependentProject(longRoot)
    const one = await first.ask('programs', { files: [path.join(shortRoot, 'src/a.ts')] })
    const two = await second.ask('programs', { files: [path.join(longRoot, 'src/a.ts')] })
    assert.equal(one.ok, true, JSON.stringify(one))
    assert.equal(two.ok, true, JSON.stringify(two))
    const listed = one.value.listing.map(([p]) => p)
    assert.ok(
      listed.includes('node_modules/dep/index.d.ts'),
      `the dependency is not listed inside the root: ${JSON.stringify(listed)}`,
    )
    assert.ok(
      !listed.some((p) => p.startsWith('..')),
      `a path climbs out of the root, which encodes where the project lives: ${JSON.stringify(listed)}`,
    )
    assert.deepEqual(two.value, one.value)
  } finally {
    first.close()
    second.close()
    rmSync(shortRoot, { recursive: true, force: true })
    rmSync(longRoot, { recursive: true, force: true })
  }
})

test('a root reached through a symlink lists as its realpath does', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  if (process.platform === 'win32') return t.skip('a directory symlink needs a privilege here')
  // The sibling of the test above, and the case that cannot be spelled with two names of one
  // directory: `path.resolve` collapses `.` and `..` but resolves no link, so only a real
  // symlink distinguishes resolving from realpathing.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-symlink-'))
  const real = path.join(outer, 'real')
  const link = path.join(outer, 'link')
  mkdirSync(real)
  symlinkSync(real, link, 'dir')
  const viaReal = spawnDriver(realpathSync(real))
  const viaLink = spawnDriver(link)
  try {
    writeDependentProject(real)
    const one = await viaReal.ask('programs', { files: [path.join(real, 'src/a.ts')] })
    const two = await viaLink.ask('programs', { files: [path.join(link, 'src/a.ts')] })
    assert.equal(one.ok, true, JSON.stringify(one))
    assert.equal(two.ok, true, JSON.stringify(two))
    assert.deepEqual(two.value, one.value)
  } finally {
    viaReal.close()
    viaLink.close()
    rmSync(outer, { recursive: true, force: true })
  }
})

test('a namespace type answers the same text from two differently named roots', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // `typeToString` renders a module namespace object as `typeof import("<resolved path>")`, and
  // the resolved path is absolute. That text reaches a rule, a violation message and the cache
  // entry the message is stored in — so two checkouts of one commit produced two different
  // messages for one file, and a cached one named a directory the project no longer lives in.
  // The two roots differ only in the length of their names, so a `text` carrying either one
  // cannot be equal.
  const shortRoot = mkdtempSync(path.join(tmpdir(), 'lk-ns-'))
  const longRoot = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-namespace-much-longer-'))
  const first = spawnDriver(shortRoot)
  const second = spawnDriver(longRoot)
  const source = 'import * as ns from "dep"\nexport const held = ns\n'
  try {
    writeDependentProject(shortRoot)
    writeDependentProject(longRoot)
    writeFileSync(path.join(shortRoot, 'src/a.ts'), source)
    writeFileSync(path.join(longRoot, 'src/a.ts'), source)
    // The *use* of the namespace, not the import that binds it, and spelled without the
    // trailing newline: `nodeAt` wants the smallest node spanning `[start, end)`, and a range
    // that runs one byte past the identifier is spanned by nothing at all.
    const use = source.lastIndexOf('ns')
    const span = { start: use, end: use + 2 }
    const one = await first.ask('typeOf', { file: path.join(shortRoot, 'src/a.ts'), ...span })
    const two = await second.ask('typeOf', { file: path.join(longRoot, 'src/a.ts'), ...span })
    assert.equal(one.ok, true, JSON.stringify(one))
    assert.equal(two.ok, true, JSON.stringify(two))
    assert.ok(
      one.value.text.includes('import("node_modules/dep/index")'),
      `the rendered path is not root-relative: ${JSON.stringify(one.value)}`,
    )
    assert.ok(
      !one.value.text.includes(shortRoot) && !one.value.text.includes(tmpdir()),
      `the rendered type carries the checkout: ${JSON.stringify(one.value)}`,
    )
    assert.deepEqual(two.value, one.value)
  } finally {
    first.close()
    second.close()
    rmSync(shortRoot, { recursive: true, force: true })
    rmSync(longRoot, { recursive: true, force: true })
  }
})

test('a rendered path outside the root is rewritten root-relative, not dropped', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The other half of the ruling, reconciled with `listedPath`'s: a `..` row is not what makes
  // a path machine-dependent, an absolute prefix is — `path.relative` is exactly as stable for
  // a sibling outside the root as it is for one inside it. So this rewrites rather than drops,
  // the same way `listedPath` already does for a listing row.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-ns-outside-'))
  const root = path.join(outer, 'app')
  mkdirSync(path.join(outer, 'shared'), { recursive: true })
  mkdirSync(path.join(root, 'src'), { recursive: true })
  const session = spawnDriver(root)
  const source = 'import * as ns from "../../shared/b"\nexport const held = ns\n'
  try {
    writeFileSync(path.join(outer, 'shared/b.ts'), 'export const b: number = 2\n')
    writeFileSync(path.join(root, 'package.json'), '{"name":"app","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    writeFileSync(path.join(root, 'src/a.ts'), source)
    const answer = await session.ask('typeOf', {
      file: path.join(root, 'src/a.ts'),
      ...(() => {
        const use = source.lastIndexOf('ns')
        return { start: use, end: use + 2 }
      })(),
    })
    assert.equal(answer.ok, true, JSON.stringify(answer))
    assert.ok(
      answer.value.text?.includes('import("../shared/b")'),
      `the rendered path is not root-relative: ${JSON.stringify(answer.value)}`,
    )
    assert.ok(
      !answer.value.text.includes(outer) && !answer.value.text.includes(tmpdir()),
      `the rendered type carries the checkout: ${JSON.stringify(answer.value)}`,
    )
  } finally {
    session.close()
    rmSync(outer, { recursive: true, force: true })
  }
})

test('programs builds one program per config, whatever the corpus size', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The cost this is arranged around, counted rather than timed. `programs` used to call
  // `ensureProgram(configFor(f), [f])` once per file, and `ensureProgram` rebuilds whenever the
  // root set widens — so forty TypeScript files under one config cost forty builds, each one
  // throwing the last away, and two hundred and forty-two markdown files added two hundred and
  // forty-two more for rows no listing ever carried.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-batch-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"batch","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    // The corpus the count above was measured on: forty TypeScript files and two hundred and
    // forty-two markdown ones, all of them under the one config. Forty `.ts` files alone would
    // pass against the driver this replaces for the wrong half of the reason — they are all
    // claimed by the config, so a per-file driver would still have built one program per file
    // and the count would be forty either way. The markdown is what makes the ratio between
    // corpus size and program count visible.
    const files = []
    for (let i = 0; i < 40; i += 1) {
      const file = path.join(root, `src/f${i}.ts`)
      writeFileSync(file, `export const n${i}: number = ${i}\n`)
      files.push(file)
    }
    for (let i = 0; i < 242; i += 1) {
      const file = path.join(root, `src/d${i}.md`)
      writeFileSync(file, `# ${i}\n`)
      files.push(file)
    }
    const answer = await session.ask('programs', { files })
    assert.equal(answer.ok, true, JSON.stringify(answer))
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 1, JSON.stringify(stats.value))
    assert.equal(stats.value.programs, 1, JSON.stringify(stats.value))
    assert.equal(
      answer.value.listing.length,
      41,
      `the forty sources and the tsconfig.json, and nothing else: ${JSON.stringify(answer.value.listing.map(([p]) => p))}`,
    )
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs rebuilds a program whose roots changed under it', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // A held driver answers a *second* run, and `check --fix` is the shape that already does it:
  // the fixes are written and the re-check reuses the same sidecar. `ensureProgram` used to
  // return the cached entry whenever the root set had not widened, so the second run answered
  // out of the pre-fix program — `typeOf` gave the old type and the listing carried the old
  // hash, which means the cache key that run committed under was the previous bytes'.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-rebuild-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"rebuild","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    const file = path.join(root, 'src/a.ts')
    const before = 'export const v = 1\n'
    const after = 'export const v = "x"\n'
    writeFileSync(file, before)

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const firstType = await session.ask('typeOf', { file, ...spanOf(before, 'v') })
    assert.equal(firstType.value.primitive, 'number', JSON.stringify(firstType))

    writeFileSync(file, after)
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const row = ([p]) => p === 'src/a.ts'
    assert.notDeepEqual(
      second.value.listing.find(row),
      first.value.listing.find(row),
      'the listing carries the pre-fix hash, so the run keys on bytes that are gone',
    )
    const secondType = await session.ask('typeOf', { file, ...spanOf(after, 'v') })
    assert.equal(
      secondType.value.primitive,
      'string',
      `answered from the pre-fix program: ${JSON.stringify(secondType)}`,
    )
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 2, JSON.stringify(stats.value))
    assert.equal(stats.value.programs, 1, 'rebuilt in place, not added beside')
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs leaves a program alone when nothing moved', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The negative half, and it is what keeps the rebuild above from being "rebuild always":
  // a second run over an unchanged corpus must not pay for every program again.
  //
  // Two of the three roots are ones the compiler declines: this `tsconfig.json` does not set
  // `allowJs`, so the `.js` is dropped before `getSourceFile` ever sees it, and the `.md` is
  // not a language the compiler admits at all. Neither has a built hash, so comparing one
  // against its bytes on disk finds a difference that no rebuild can ever close — which was
  // three `createProgram` calls for three `programs` calls, forever.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-norebuild-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"norebuild","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    const file = path.join(root, 'src/a.ts')
    writeFileSync(file, 'export const v = 1\n')
    const declined = path.join(root, 'src/b.js')
    writeFileSync(declined, 'export const w = 2\n')
    const notCode = path.join(root, 'src/notes.md')
    writeFileSync(notCode, '# notes\n')
    const files = [file, declined, notCode]
    const first = await session.ask('programs', { files })
    const second = await session.ask('programs', { files })
    const third = await session.ask('programs', { files })
    assert.deepEqual(second.value, first.value)
    assert.deepEqual(third.value, first.value)
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 1, JSON.stringify(stats.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs forgets a root that has been deleted', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The deletion half of the staleness the rebuild above covers, and it is the one a session
  // reaches: a driver outlives a run, so a file removed between two runs would otherwise keep
  // its pre-deletion hash in the listing — the run's own cache key naming bytes that are gone
  // — and, once nothing on disk can ever match that hash again, rebuild the program on every
  // call.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-deleted-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"deleted","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    const gone = path.join(root, 'src/a.ts')
    const kept = path.join(root, 'src/keep.ts')
    writeFileSync(gone, 'export const v = 1\n')
    writeFileSync(kept, 'export const k = 2\n')

    const first = await session.ask('programs', { files: [gone, kept] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.ok(
      first.value.listing.some(([p]) => p === 'src/a.ts'),
      `the deleted-to-be root is listed while it exists: ${JSON.stringify(first.value.listing)}`,
    )

    rmSync(gone)
    // The file list a run hands over comes from discovery, so a file that is gone is not in it.
    const second = await session.ask('programs', { files: [kept] })
    assert.equal(second.ok, true, JSON.stringify(second))
    assert.ok(
      !second.value.listing.some(([p]) => p === 'src/a.ts'),
      `the listing still names a file that is gone: ${JSON.stringify(second.value.listing)}`,
    )
    const third = await session.ask('programs', { files: [kept] })
    assert.deepEqual(third.value, second.value, 'settled: the deletion is not re-noticed')
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 2, JSON.stringify(stats.value))
    assert.equal(stats.value.programs, 1, 'rebuilt in place, not added beside')
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('a file no tsconfig under the root claims is reported as ad-hoc', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // A `tsconfig.json` *above* the root is refused — lanekeep's confinement stops at the root —
  // so the file is typed with this driver's own options and not the project's, `strict` off.
  // Nothing was wrong with the run and nothing said so, which is what makes it a notice rather
  // than an error: the same file checked from one directory up is typed differently.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-adhoc-'))
  const root = path.join(outer, 'app')
  mkdirSync(path.join(root, 'src'), { recursive: true })
  const session = spawnDriver(root)
  try {
    writeFileSync(
      path.join(outer, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true},"include":["app/src"]}\n',
    )
    writeFileSync(path.join(root, 'package.json'), '{"name":"app","private":true}\n')
    writeFileSync(path.join(root, 'src/a.ts'), 'export const n: number = 1\n')
    const answer = await session.ask('programs', { files: [path.join(root, 'src/a.ts')] })
    assert.equal(answer.ok, true, JSON.stringify(answer))
    assert.deepEqual(answer.value.adhoc, ['src/a.ts'], JSON.stringify(answer.value))
  } finally {
    session.close()
    rmSync(outer, { recursive: true, force: true })
  }
})

test('a file a tsconfig under the root claims is not reported as ad-hoc', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The negative half, which is what makes the notice information: the ordinary project says
  // nothing at all.
  const files = [path.join(project, 'src/a.ts')]
  const answer = await ask('programs', { files })
  assert.equal(answer.ok, true, JSON.stringify(answer))
  assert.deepEqual(answer.value.adhoc, [], JSON.stringify(answer.value))
})

test('an op naming a prototype method is unknown, not called', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // `handlers` is an object literal, so a plain `handlers[request.op]` finds
  // `Object.prototype.constructor` for `op: "constructor"` and calls it with a request object.
  const response = await ask('constructor')
  assert.equal(response.ok, false, JSON.stringify(response))
  assert.match(response.error, /unknown op/)
})

test('hello answers on the protocol when the typescript specifier resolves to nothing', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The regression this pins: anything that dereferences `ts` at module scope kills the driver
  // before it can say *why* the package did not load, and the Rust side gets a dead pipe
  // instead of a message naming the specifier.
  const dir = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-absent-'))
  writeFileSync(path.join(dir, 'package.json'), '{"name":"absent","private":true}\n')
  const session = spawnDriver(dir, 'definitely-not-typescript')
  try {
    const response = await session.ask('hello')
    assert.equal(response.ok, true, JSON.stringify(response))
    assert.match(response.value.error, /definitely-not-typescript/)
  } finally {
    session.close()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('hello answers on the protocol when the specifier is not the compiler', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  const dir = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-notcompiler-'))
  writeFileSync(path.join(dir, 'package.json'), '{"name":"notcompiler","private":true}\n')
  // A directory rather than a package: `require` of it throws, which is a load failure the
  // driver must report rather than die of.
  const session = spawnDriver(dir, dir)
  try {
    const response = await session.ask('hello')
    assert.equal(response.ok, true, JSON.stringify(response))
    assert.ok(
      typeof response.value.error === 'string' || Array.isArray(response.value.unsupported),
      `neither a load error nor a missing-API list: ${JSON.stringify(response.value)}`,
    )
  } finally {
    session.close()
    rmSync(dir, { recursive: true, force: true })
  }
})
