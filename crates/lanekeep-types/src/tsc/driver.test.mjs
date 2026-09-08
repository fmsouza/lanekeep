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
import { createHash } from 'node:crypto'
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

/** The one root of the fixture below, whose import is the non-root read the tests move. */
const DEPENDENT_SOURCE = "import { rate } from '../types/dep'\nexport const v = rate\n"

/** A project whose one root imports a declaration file that no config lists as a root. */
function writeDependentRoot(root, declaration) {
  mkdirSync(path.join(root, 'src'), { recursive: true })
  mkdirSync(path.join(root, 'types'), { recursive: true })
  writeFileSync(path.join(root, 'package.json'), '{"name":"dependency","private":true}\n')
  // `include` names `src` alone, so `types/dep.d.ts` is reached by module resolution and is
  // never a root — which is the whole shape of the drift below.
  writeFileSync(
    path.join(root, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  writeFileSync(path.join(root, 'types/dep.d.ts'), declaration)
  writeFileSync(path.join(root, 'src/a.ts'), DEPENDENT_SOURCE)
}

test('programs rebuilds a program whose dependency moved under it', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The root-only comparison this replaces was blind to every file a program reaches by
  // resolution rather than by root membership: a `.d.ts` outside `include`, the
  // `tsconfig.json` itself, a `package.json` resolution consulted. A held driver — the plan-6
  // server holds one across runs — kept answering out of the old declaration, so `lanekeep
  // server`'s diagnostics drifted from `lanekeep check`'s, whose provider is fresh every run
  // and so has no cached program to return early from.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-dependency-'))
  const session = spawnDriver(root)
  try {
    writeDependentRoot(root, 'export declare const rate: number\n')
    const file = path.join(root, 'src/a.ts')

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const firstType = await session.ask('typeOf', { file, ...spanOf(DEPENDENT_SOURCE, 'v') })
    assert.equal(firstType.value.primitive, 'number', JSON.stringify(firstType))

    writeFileSync(path.join(root, 'types/dep.d.ts'), 'export declare const rate: string\n')
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const row = ([p]) => p === 'types/dep.d.ts'
    assert.notDeepEqual(
      second.value.listing.find(row),
      first.value.listing.find(row),
      'the listing carries the pre-edit hash, so the run keys on bytes that are gone',
    )
    const secondType = await session.ask('typeOf', { file, ...spanOf(DEPENDENT_SOURCE, 'v') })
    assert.equal(
      secondType.value.primitive,
      'string',
      `answered from the pre-edit program: ${JSON.stringify(secondType)}`,
    )
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 2, JSON.stringify(stats.value))
    assert.equal(stats.value.programs, 1, 'rebuilt in place, not added beside')

    // Settled: a third call over the same bytes finds nothing to do. What this pins is the
    // rebuild absorbing the edit and not repeating — not `refreshReads`' write-back, which it
    // cannot: the rebuild here re-reads the moved declaration, so its own `collected` writes
    // the fresh hash into the entry whatever the refresh wrote. `programs settles when the
    // rebuild does not re-read the moved file`, below, is the fixture for that.
    const third = await session.ask('programs', { files: [file] })
    assert.deepEqual(third.value, second.value, 'the settled listing disagrees with the second')
    const settled = await session.ask('stats')
    assert.equal(
      settled.value.createProgram,
      2,
      `the absorbed edit is still being re-noticed: ${JSON.stringify(settled.value)}`,
    )
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs leaves a program alone when its dependency did not move', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The negative twin of the rebuild above, and what keeps widening the comparison from every
  // root to every read from becoming "rebuild always": the read set holds the `tsconfig.json`,
  // the project's `package.json` and the declaration file, and an unchanged corpus must pay
  // for none of them a second time.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-dependency-still-'))
  const session = spawnDriver(root)
  try {
    writeDependentRoot(root, 'export declare const rate: number\n')
    const file = path.join(root, 'src/a.ts')
    const first = await session.ask('programs', { files: [file] })
    const second = await session.ask('programs', { files: [file] })
    const third = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.deepEqual(second.value, first.value)
    assert.deepEqual(third.value, first.value)
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 1, JSON.stringify(stats.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs re-parses compiler options when the tsconfig itself moved', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // `ensureProgram`'s rebuild reused `entry.options` unconditionally: a `tsconfig.json` edit
  // moves the run key — its hash reaches `entry.reads` and the listing, both fixed above — and
  // rebuilds the program, but with the *previous* options. A held session's provider then kept
  // answering out of a `strict: false` build after the project turned `strict` on, which no
  // fresh provider — `lanekeep check`'s, built once per run — could ever do, since it has no
  // stale options to carry forward.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-options-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"options","private":true}\n')
    const tsconfig = path.join(root, 'tsconfig.json')
    const configFor = (strict) =>
      `{"compilerOptions":{"strict":${strict},"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n`
    writeFileSync(tsconfig, configFor(false))
    const SOURCE = 'declare const a: string | undefined\nexport const b = a\n'
    const file = path.join(root, 'src/a.ts')
    writeFileSync(file, SOURCE)

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const firstType = await session.ask('typeOf', { file, ...spanOf(SOURCE, 'b') })
    // `strict: false` turns `strictNullChecks` off, and TypeScript drops `undefined` from a
    // union under that setting — this is the pre-flip answer the fix has to move away from.
    assert.equal(firstType.value.primitive, 'string', JSON.stringify(firstType))

    writeFileSync(tsconfig, configFor(true))
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const secondType = await session.ask('typeOf', { file, ...spanOf(SOURCE, 'b') })
    assert.equal(
      secondType.value.text,
      'string | undefined',
      `answered from the pre-edit options: ${JSON.stringify(secondType)}`,
    )
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

/**
 * A project whose `tsconfig.json` and whose resolved `package.json` both begin with a UTF-8
 * byte-order mark.
 *
 * The root `package.json` is plain: nothing resolves through it here, so a mark on it would be
 * dead weight in a fixture whose whole point is that both marked files are read.
 */
function writeMarkedRoot(root) {
  mkdirSync(path.join(root, 'src'), { recursive: true })
  writeFileSync(path.join(root, 'package.json'), '{"name":"marked","private":true}\n')
  writeFileSync(
    path.join(root, 'tsconfig.json'),
    '﻿{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  // A directory package rather than a relative file, so module resolution really does read a
  // `package.json` on the way to the declaration — a relative `./lib` reads none, and a fixture
  // whose marked file is never read settles for the uninteresting reason that nothing watches
  // it.
  mkdirSync(path.join(root, 'src/pkg'), { recursive: true })
  writeFileSync(path.join(root, 'src/pkg/package.json'), '\ufeff{"name":"pkg","types":"index.d.ts"}\n')
  writeFileSync(path.join(root, 'src/pkg/index.d.ts'), 'export declare const rate: number\n')
  writeFileSync(path.join(root, 'src/a.ts'), "import { rate } from './pkg'\nexport const v = rate\n")
}

test('programs settles over a tsconfig and package.json carrying a byte-order mark', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // Two readers, two answers. `recordRead` hashed the text the compiler's reader returned —
  // `ts.sys.readFile` strips a leading BOM — while the refresh hashed the raw bytes, so a
  // marked `tsconfig.json` or `package.json` never matched its recorded hash and every
  // `programs` call rebuilt every program, forever. Nothing is wrong with the answers, so the
  // only symptom is a session that pays a full build per request.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-bom-'))
  const session = spawnDriver(root)
  try {
    writeMarkedRoot(root)
    const file = path.join(root, 'src/a.ts')
    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    // The fixture is only a fixture if both marked files are actually read: a listing without
    // them would settle for the uninteresting reason that neither is watched at all.
    const listed = first.value.listing.map(([p]) => p)
    assert.ok(listed.includes('tsconfig.json'), JSON.stringify(listed))
    assert.ok(listed.includes('src/pkg/package.json'), JSON.stringify(listed))
    for (let call = 0; call < 4; call += 1) {
      const again = await session.ask('programs', { files: [file] })
      assert.deepEqual(again.value, first.value, `call ${call + 2} disagrees with the first`)
    }
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 1, JSON.stringify(stats.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

/** `text` as UTF-16LE with a byte-order mark, which is the encoding `ts.sys.readFile` decodes. */
function utf16le(text) {
  return Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')])
}

test('programs settles over a UTF-16LE tsconfig.json', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The other half of the byte-order-mark case above, and the reason `readCanonical` hashes
  // what `ts.sys.readFile` returns rather than a BOM strip written by hand: a UTF-16 file is
  // text to the compiler's reader and mojibake to a raw UTF-8 one, so a driver hashing the
  // raw bytes never matches the hash it recorded and rebuilds every program on every call.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-utf16-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"utf16","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      utf16le(
        '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
      ),
    )
    const SOURCE = 'declare const a: string | undefined\nexport const b = a\n'
    const file = path.join(root, 'src/a.ts')
    writeFileSync(file, SOURCE)

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    // The fixture is only a fixture if the marked config is both read and in force: `strict`
    // is what keeps `undefined` in the union, and a config that failed to decode would be no
    // config at all.
    assert.ok(
      first.value.listing.some(([p]) => p === 'tsconfig.json'),
      JSON.stringify(first.value.listing),
    )
    const typed = await session.ask('typeOf', { file, ...spanOf(SOURCE, 'b') })
    assert.equal(typed.value.text, 'string | undefined', JSON.stringify(typed))

    for (let call = 0; call < 4; call += 1) {
      const again = await session.ask('programs', { files: [file] })
      assert.deepEqual(again.value, first.value, `call ${call + 2} disagrees with the first`)
    }
    const stats = await session.ask('stats')
    assert.equal(stats.value.createProgram, 1, JSON.stringify(stats.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

/** The one root of the redirect fixture below, importing a directory package by its manifest. */
const REDIRECT_SOURCE = "import { rate } from './pkg'\nexport const v = rate\n"

/** A project whose root reaches its declaration through a `package.json` `types` field. */
function writeRedirectRoot(root, types) {
  mkdirSync(path.join(root, 'src'), { recursive: true })
  mkdirSync(path.join(root, 'src/pkg'), { recursive: true })
  writeFileSync(path.join(root, 'package.json'), '{"name":"redirect","private":true}\n')
  writeFileSync(
    path.join(root, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  writeFileSync(path.join(root, 'src/pkg/package.json'), `{"name":"pkg","types":"${types}"}\n`)
  writeFileSync(path.join(root, 'src/pkg/index.d.ts'), 'export declare const rate: number\n')
  writeFileSync(path.join(root, 'src/pkg/alt.d.ts'), 'export declare const rate: string\n')
  writeFileSync(path.join(root, 'src/a.ts'), REDIRECT_SOURCE)
}

test('programs follows a package.json that redirects resolution elsewhere', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // A moved read the refresh notices and the rebuild then ignores. `oldProgram` carries the
  // previous build's *resolutions*, so a `package.json` whose `types` now names a different
  // declaration file rebuilt the program straight back onto the old answer — the held driver
  // says `number` where a fresh one says `string`, which is `lanekeep server` disagreeing with
  // `lanekeep check` about the same bytes. A moved read that is not a source file of the
  // program is a resolution input, and a rebuild for one cannot reuse the old resolutions.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-redirect-'))
  const session = spawnDriver(root)
  try {
    writeRedirectRoot(root, 'index.d.ts')
    const file = path.join(root, 'src/a.ts')
    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const firstType = await session.ask('typeOf', { file, ...spanOf(REDIRECT_SOURCE, 'v') })
    assert.equal(firstType.value.primitive, 'number', JSON.stringify(firstType))

    writeFileSync(path.join(root, 'src/pkg/package.json'), '{"name":"pkg","types":"alt.d.ts"}\n')
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const held = await session.ask('typeOf', { file, ...spanOf(REDIRECT_SOURCE, 'v') })

    // The comparison that makes this a drift test rather than a guess about TypeScript: a
    // provider with no cached program is what `lanekeep check` runs, and its answer is the
    // one the held session has to match.
    const fresh = spawnDriver(root)
    try {
      await fresh.ask('programs', { files: [file] })
      const freshType = await fresh.ask('typeOf', { file, ...spanOf(REDIRECT_SOURCE, 'v') })
      assert.equal(freshType.value.primitive, 'string', JSON.stringify(freshType))
      assert.equal(
        held.value.primitive,
        freshType.value.primitive,
        `the held driver answers ${JSON.stringify(held.value)} where a fresh one answers ${JSON.stringify(freshType.value)}`,
      )
    } finally {
      fresh.close()
    }
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs settles when the rebuild does not re-read the moved file', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // What pins `refreshReads`' write-back, which neither rebuild test above does: there the
  // rebuild re-reads the moved file — `oldProgram` reuse compares every source file it kept,
  // and a build from nothing re-resolves the same import — so `buildProgram`'s own `collected`
  // writes the fresh hash back whatever the refresh did, and both settle either way.
  //
  // Here the import goes in the same window as the edit, so the rebuild resolves nothing
  // through `src/pkg/package.json` and never asks for it again. `buildProgram` carries the
  // previous read set forward — deliberately, so a watched file is not silently unwatched —
  // and an entry left holding the pre-edit hash answers "moved" on every later call, rebuilding
  // the program forever for one edit that has already been absorbed.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-writeback-'))
  const session = spawnDriver(root)
  try {
    writeRedirectRoot(root, 'index.d.ts')
    const file = path.join(root, 'src/a.ts')
    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.ok(
      first.value.listing.some(([p]) => p === 'src/pkg/package.json'),
      `the manifest is not watched at all: ${JSON.stringify(first.value.listing)}`,
    )

    // Both at once: the root stops importing the package and the manifest moves. The rebuild
    // the root's own edit forces resolves nothing through the manifest, so nothing re-reads it.
    writeFileSync(file, 'export const v = 1\n')
    const moved = '{"name":"pkg","types":"alt.d.ts"}\n'
    writeFileSync(path.join(root, 'src/pkg/package.json'), moved)
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    // The write-back is a value and not only a settle. `refreshReads` puts the fresh hash into
    // the driver-wide `reads`, which is what the listing — the run's own cache key — is built
    // from, and this manifest is in no program's `getSourceFiles()`, so the listing has no
    // second source for its row. Left at the pre-edit hash, the key names bytes that are not on
    // disk: a warm run keyed on a file it has already re-read and found different.
    assert.deepEqual(
      second.value.listing.find(([p]) => p === 'src/pkg/package.json'),
      ['src/pkg/package.json', createHash('sha256').update(moved, 'utf8').digest('hex')],
      `the row carries a hash that is not the bytes on disk: ${JSON.stringify(second.value.listing)}`,
    )
    const built = await session.ask('stats')
    assert.equal(built.value.createProgram, 2, JSON.stringify(built.value))

    const third = await session.ask('programs', { files: [file] })
    assert.deepEqual(third.value, second.value)
    const settled = await session.ask('stats')
    assert.equal(
      settled.value.createProgram,
      2,
      `the absorbed edit is re-noticed on every call: ${JSON.stringify(settled.value)}`,
    )
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

/** The one root of the appearance fixtures below: an import that does not resolve yet. */
const APPEARING_SOURCE = "import { rate } from '@acme/rates'\nexport const v = rate\n"

/** A project importing a package that is not installed. */
function writeAppearingRoot(root) {
  mkdirSync(path.join(root, 'src'), { recursive: true })
  writeFileSync(path.join(root, 'package.json'), '{"name":"appearing","private":true}\n')
  writeFileSync(
    path.join(root, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
  )
  writeFileSync(path.join(root, 'src/a.ts'), APPEARING_SOURCE)
}

/** Install the package the fixture above imports, as `npm install` would. */
function installAppearingPackage(root, declared) {
  const dir = path.join(root, 'node_modules/@acme/rates')
  mkdirSync(dir, { recursive: true })
  writeFileSync(path.join(dir, 'package.json'), '{"name":"@acme/rates","types":"index.d.ts"}\n')
  writeFileSync(path.join(dir, 'index.d.ts'), declared)
}

test('programs notices a package installed between two runs', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The mirror image of every refresh fixture above, and invisible to all of them: a
  // resolution that changes because a file *appeared*. `refreshProgram` compares what the
  // program read, and a package that was not installed was never read — so the listing and the
  // run key are byte-identical, the held program keeps answering out of the failed resolution,
  // and `lanekeep check` over the same bytes answers `number`. `complete()` is the loud half:
  // it re-resolves per question and flips to `true` while `typeOf` stays unknown, so one
  // request says every import resolved and cannot say to what.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-appearing-'))
  const session = spawnDriver(root)
  try {
    writeAppearingRoot(root)
    const file = path.join(root, 'src/a.ts')
    const at = spanOf(APPEARING_SOURCE, 'v')

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const before = await session.ask('complete', { file })
    assert.equal(before.value, false, 'the import resolves before it is installed')

    installAppearingPackage(root, 'export declare const rate: number\n')

    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const held = await session.ask('typeOf', { file, ...at })
    const heldComplete = await session.ask('complete', { file })

    // A provider with no history is what `lanekeep check` runs, and its answers are the ones
    // the held session has to match — all three of them.
    const fresh = spawnDriver(root)
    try {
      const clean = await fresh.ask('programs', { files: [file] })
      const freshType = await fresh.ask('typeOf', { file, ...at })
      assert.equal(freshType.value.primitive, 'number', JSON.stringify(freshType))
      assert.equal(
        held.value?.primitive,
        freshType.value.primitive,
        `the held driver answers ${JSON.stringify(held.value)} where a fresh one answers ${JSON.stringify(freshType.value)}`,
      )
      assert.equal(heldComplete.value, true, 'complete disagrees with typeOf about one file')
      assert.deepEqual(
        second.value.listing,
        clean.value.listing,
        `the held driver lists ${JSON.stringify(second.value.listing)} where a fresh one lists ${JSON.stringify(clean.value.listing)}`,
      )
    } finally {
      fresh.close()
    }

    // Settled: the appearance is absorbed once, not re-noticed on every later call.
    const third = await session.ask('programs', { files: [file] })
    assert.deepEqual(third.value, second.value, 'the absorbed appearance is re-noticed')
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs notices a declaration file appearing beside its source', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The same fault reached without a package manager: a `.d.ts` landing where resolution had
  // fallen back. Nothing about the root changed and nothing the program read moved, so only a
  // re-probe of what the build was denied can see it.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-beside-'))
  const session = spawnDriver(root)
  try {
    mkdirSync(path.join(root, 'src'), { recursive: true })
    writeFileSync(path.join(root, 'package.json'), '{"name":"beside","private":true}\n')
    writeFileSync(
      path.join(root, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    writeFileSync(path.join(root, 'src/dep.js'), 'export const rate = 1\n')
    const source = "import { rate } from './dep'\nexport const v = rate\n"
    writeFileSync(path.join(root, 'src/a.ts'), source)
    const file = path.join(root, 'src/a.ts')
    const at = spanOf(source, 'v')

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))

    writeFileSync(path.join(root, 'src/dep.d.ts'), 'export declare const rate: string\n')
    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const held = await session.ask('typeOf', { file, ...at })

    const fresh = spawnDriver(root)
    try {
      await fresh.ask('programs', { files: [file] })
      const freshType = await fresh.ask('typeOf', { file, ...at })
      assert.equal(freshType.value.primitive, 'string', JSON.stringify(freshType))
      assert.equal(
        held.value?.primitive,
        freshType.value.primitive,
        `the held driver answers ${JSON.stringify(held.value)} where a fresh one answers ${JSON.stringify(freshType.value)}`,
      )
    } finally {
      fresh.close()
    }
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

/** Two configs side by side, each owning one root, so a refresh can be shown to be per config. */
function writeTwoConfigs(root) {
  for (const name of ['a', 'b']) {
    mkdirSync(path.join(root, name, 'src'), { recursive: true })
    writeFileSync(
      path.join(root, name, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"bundler","module":"ESNext"},"include":["src"]}\n',
    )
    writeFileSync(path.join(root, name, `src/${name}.ts`), `export const ${name} = 1\n`)
  }
  writeFileSync(path.join(root, 'package.json'), '{"name":"two","private":true}\n')
  // A directory package under `b`, reached by no import at all: its declaration is a root
  // because the config `include`s `src`, and its manifest is read only when something asks a
  // question that resolves `./pkg`. That is what makes a query's own reads visible in the test
  // below — a build never touches this file.
  mkdirSync(path.join(root, 'b/src/pkg'), { recursive: true })
  writeFileSync(path.join(root, 'b/src/pkg/package.json'), '{"name":"pkg","types":"index.d.ts"}\n')
  writeFileSync(path.join(root, 'b/src/pkg/index.d.ts'), 'export declare const rate: number\n')
}

/** A hoisted-workspace fixture: `<mono>/proj` importing a package installed at `<mono>/node_modules`. */
function writeHoistedRoot(monoRoot, projRoot) {
  mkdirSync(path.join(projRoot, 'src'), { recursive: true })
  writeFileSync(path.join(projRoot, 'package.json'), '{"name":"proj","private":true}\n')
  writeFileSync(
    path.join(projRoot, 'tsconfig.json'),
    '{"compilerOptions":{"strict":true,"target":"ES2022","moduleResolution":"node10","module":"CommonJS"},"include":["src"]}\n',
  )
  writeFileSync(path.join(projRoot, 'src/a.ts'), APPEARING_SOURCE)
}

test('programs notices a package hoisted above the project root', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The monorepo shape `withinBoundary` used to miss: `npm install` at the workspace root
  // hoists a shared dependency into `<mono>/node_modules`, one level above the project root a
  // session is held for. The build's own resolution probes there and is denied, but the old
  // `withinBoundary` discarded every absence outside `projectRoot` — so the denial was never
  // remembered, `refreshAbsences` had nothing to re-probe, and the held session kept answering
  // `any` after the very install a fresh run would see. `recordRead` never had this narrowing:
  // reads outside the root are recorded (this is the sibling-import fixture's own case above),
  // and the ruling is to give absences the same rule, typescript's own lib/ aside.
  const monoRoot = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-hoisted-'))
  const projRoot = path.join(monoRoot, 'proj')
  mkdirSync(projRoot, { recursive: true })
  const session = spawnDriver(projRoot)
  try {
    writeHoistedRoot(monoRoot, projRoot)
    const file = path.join(projRoot, 'src/a.ts')
    const at = spanOf(APPEARING_SOURCE, 'v')

    const first = await session.ask('programs', { files: [file] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const before = await session.ask('complete', { file })
    assert.equal(before.value, false, 'the import resolves before it is installed')

    // A hoisted install: the package lands beside the workspace root, not under `projRoot`.
    installAppearingPackage(monoRoot, 'export declare const rate: number\n')

    const second = await session.ask('programs', { files: [file] })
    assert.equal(second.ok, true, JSON.stringify(second))
    const held = await session.ask('typeOf', { file, ...at })

    const fresh = spawnDriver(projRoot)
    try {
      await fresh.ask('programs', { files: [file] })
      const freshType = await fresh.ask('typeOf', { file, ...at })
      assert.equal(freshType.value.primitive, 'number', JSON.stringify(freshType))
      assert.equal(
        held.value?.primitive,
        freshType.value.primitive,
        `the held driver answers ${JSON.stringify(held.value)} where a fresh one answers ${JSON.stringify(freshType.value)}`,
      )
    } finally {
      fresh.close()
    }
  } finally {
    session.close()
    rmSync(monoRoot, { recursive: true, force: true })
  }
})

test('the listing names only the configs the request names', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The listing is the run's own cache key, and the key a held session commits under has to be
  // the one `lanekeep check` would compute over the same request. A fresh driver holds no
  // program for a config no file of the request falls under, so a held one contributes no rows
  // for it either: it stays cached, is not refreshed, and comes back — refreshed, and rebuilt
  // if something moved — with the request that next names one of its files. Refreshing every
  // held program instead kept the key current for a config the run never asked about, which is
  // a row `lanekeep check` over the same request does not have at all.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-unnamed-'))
  const session = spawnDriver(root)
  try {
    writeTwoConfigs(root)
    const fileA = path.join(root, 'a/src/a.ts')
    const fileB = path.join(root, 'b/src/b.ts')
    const first = await session.ask('programs', { files: [fileA, fileB] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.ok(
      first.value.listing.some(([p]) => p === 'a/src/a.ts'),
      `config a never reached the listing, so its absence below pins nothing: ${JSON.stringify(first.value.listing)}`,
    )
    const built = await session.ask('stats')
    assert.equal(built.value.createProgram, 2, JSON.stringify(built.value))

    // Edited while config `a` is out of the request. What the held driver does with this is the
    // whole question: nothing at all now, and its own rows when a request next names it.
    const edited = 'export const a = "two"\n'
    writeFileSync(fileA, edited)

    // A query between the two `programs` calls, which is what an editor session does all day.
    // Its own resolution reads a `package.json` no program built — residue in the driver-wide
    // read map — and a listing built from that map carries a row `lanekeep check` over the same
    // request has no way to produce, and which depends on which questions the session happened
    // to be asked. Query-time reads are per-entry tracked reads instead; the listing is
    // program builds' reads and nothing else.
    const queried = await session.ask('isAssignableTo', {
      file: fileB,
      ...spanOf('export const b = 1\n', 'b'),
      module: './pkg',
      name: 'rate',
    })
    assert.equal(queried.ok, true, JSON.stringify(queried))
    assert.deepEqual(
      queried.reads,
      ['b/src/pkg/package.json'],
      `the answer does not report what answering it read: ${JSON.stringify(queried)}`,
    )

    const held = await session.ask('programs', { files: [fileB] })
    assert.equal(held.ok, true, JSON.stringify(held))
    // A provider with no history is what `lanekeep check` runs, and its listing is the one the
    // held session has to match.
    const fresh = spawnDriver(root)
    try {
      const clean = await fresh.ask('programs', { files: [fileB] })
      assert.equal(clean.ok, true, JSON.stringify(clean))
      assert.ok(
        !clean.value.listing.some(([p]) => p.startsWith('a/')),
        `the fresh driver already names config a, so the comparison below is not discriminating: ${JSON.stringify(clean.value.listing)}`,
      )
      assert.deepEqual(
        held.value.listing,
        clean.value.listing,
        `the held driver lists ${JSON.stringify(held.value.listing)} where a fresh one lists ${JSON.stringify(clean.value.listing)}`,
      )
    } finally {
      fresh.close()
    }
    const untouched = await session.ask('stats')
    assert.equal(
      untouched.value.createProgram,
      2,
      `a program no file of the request falls under was rebuilt: ${JSON.stringify(untouched.value)}`,
    )

    // Named again: the rows come back, carrying the bytes on disk now rather than the ones the
    // program was built from, and the program is rebuilt because a root moved under it.
    const back = await session.ask('programs', { files: [fileA, fileB] })
    assert.equal(back.ok, true, JSON.stringify(back))
    assert.deepEqual(
      back.value.listing.find(([p]) => p === 'a/src/a.ts'),
      ['a/src/a.ts', createHash('sha256').update(edited, 'utf8').digest('hex')],
      `the returning rows do not carry the bytes on disk: ${JSON.stringify(back.value.listing)}`,
    )
    const rebuilt = await session.ask('stats')
    assert.equal(rebuilt.value.createProgram, 3, JSON.stringify(rebuilt.value))

    // Rebuilt only where something moved: the same request again refreshes and builds nothing.
    const again = await session.ask('programs', { files: [fileA, fileB] })
    assert.deepEqual(again.value, back.value, 'settled: the edit is absorbed')
    const settled = await session.ask('stats')
    assert.equal(settled.value.createProgram, 3, JSON.stringify(settled.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs answers after a tsconfig.json has left the disk', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // Refreshing every held program means refreshing one whose `tsconfig.json` has since been
  // deleted or renamed, and that config is what `optionsFor` re-parses when the refresh says
  // the config chain moved — `ts.readConfigFile` cannot read it, the driver throws, and every
  // later `programs` call throws the same way for the sidecar's whole life. A session then
  // fails every run while `lanekeep check`, whose provider is fresh, succeeds. A program whose
  // config is gone contributes no rows, so it is dropped rather than rebuilt.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-configgone-'))
  const session = spawnDriver(root)
  try {
    writeTwoConfigs(root)
    const fileA = path.join(root, 'a/src/a.ts')
    const fileB = path.join(root, 'b/src/b.ts')
    const first = await session.ask('programs', { files: [fileA, fileB] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.ok(
      first.value.listing.some(([p]) => p === 'a/tsconfig.json'),
      JSON.stringify(first.value.listing),
    )

    rmSync(path.join(root, 'a/tsconfig.json'))
    const second = await session.ask('programs', { files: [fileB] })
    assert.equal(
      second.ok,
      true,
      `a deleted tsconfig.json killed the op: ${JSON.stringify(second)}`,
    )
    assert.ok(
      !second.value.listing.some(([p]) => p.startsWith('a/')),
      `the run key still names the gone config's program: ${JSON.stringify(second.value.listing)}`,
    )
    const third = await session.ask('programs', { files: [fileB] })
    assert.deepEqual(third.value, second.value, 'settled: the drop is not re-noticed')
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('programs answers after a whole config directory has been removed', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The same fault reached the other way, which is the way a branch switch reaches it: the
  // config goes with its sources rather than alone, so the refresh finds no roots left *and*
  // no config to re-parse.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-configdir-'))
  const session = spawnDriver(root)
  try {
    writeTwoConfigs(root)
    const fileA = path.join(root, 'a/src/a.ts')
    const fileB = path.join(root, 'b/src/b.ts')
    const first = await session.ask('programs', { files: [fileA, fileB] })
    assert.equal(first.ok, true, JSON.stringify(first))

    rmSync(path.join(root, 'a'), { recursive: true, force: true })
    const second = await session.ask('programs', { files: [fileB] })
    assert.equal(
      second.ok,
      true,
      `a removed config directory killed the op: ${JSON.stringify(second)}`,
    )
    assert.ok(
      !second.value.listing.some(([p]) => p.startsWith('a/')),
      `the run key still names files that are gone: ${JSON.stringify(second.value.listing)}`,
    )
    const third = await session.ask('programs', { files: [fileB] })
    assert.deepEqual(third.value, second.value, 'settled: the drop is not re-noticed')
    const stats = await session.ask('stats')
    assert.equal(stats.value.programs, 1, JSON.stringify(stats.value))
  } finally {
    session.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test('an edit under one tsconfig rebuilds that program alone', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // Per-config isolation, which is what `entry.reads` being per program buys: refreshing on
  // the driver-wide read map instead would make one package's edit cost a rebuild of every
  // other config in the corpus.
  const root = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-isolation-'))
  const session = spawnDriver(root)
  try {
    writeTwoConfigs(root)
    const fileA = path.join(root, 'a/src/a.ts')
    const fileB = path.join(root, 'b/src/b.ts')
    const files = [fileA, fileB]
    await session.ask('programs', { files })
    const built = await session.ask('stats')
    assert.equal(built.value.programs, 2, JSON.stringify(built.value))
    assert.equal(built.value.createProgram, 2, JSON.stringify(built.value))

    writeFileSync(fileA, 'export const a = "moved"\n')
    await session.ask('programs', { files })
    const after = await session.ask('stats')
    assert.equal(
      after.value.createProgram,
      3,
      `one edit rebuilt more than its own config's program: ${JSON.stringify(after.value)}`,
    )
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

test('the ad-hoc notice does not outlive the file it named', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // `adhocFiles` is a set that only ever grew, so a held session's notice — and the `adhoc`
  // half of what the host folds into the run key — named a file that had since been deleted,
  // while `lanekeep check` over the identical bytes named it not at all. The request's file
  // list is the run's discovery list, so it is the whole answer and not an addition to the
  // previous one.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-adhoc-stale-'))
  const root = path.join(outer, 'app')
  mkdirSync(path.join(root, 'src'), { recursive: true })
  const session = spawnDriver(root)
  try {
    // A config above the root, which lanekeep's confinement refuses — so both files below fall
    // to the ad-hoc program, as in the notice test further down.
    writeFileSync(
      path.join(outer, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true},"include":["app/src"]}\n',
    )
    writeFileSync(path.join(root, 'package.json'), '{"name":"app","private":true}\n')
    const kept = path.join(root, 'src/a.ts')
    const gone = path.join(root, 'src/b.ts')
    writeFileSync(kept, 'export const n: number = 1\n')
    writeFileSync(gone, 'export const m: number = 2\n')
    const first = await session.ask('programs', { files: [kept, gone] })
    assert.equal(first.ok, true, JSON.stringify(first))
    assert.deepEqual(first.value.adhoc, ['src/a.ts', 'src/b.ts'], JSON.stringify(first.value))

    rmSync(gone)
    const held = await session.ask('programs', { files: [kept] })
    assert.equal(held.ok, true, JSON.stringify(held))
    // A provider with no history is what `lanekeep check` runs, and its answer is the one the
    // held session has to match.
    const fresh = spawnDriver(root)
    try {
      const clean = await fresh.ask('programs', { files: [kept] })
      assert.deepEqual(clean.value.adhoc, ['src/a.ts'], JSON.stringify(clean.value))
      assert.deepEqual(
        held.value.adhoc,
        clean.value.adhoc,
        `the held driver reports ${JSON.stringify(held.value.adhoc)} where a fresh one reports ${JSON.stringify(clean.value.adhoc)}`,
      )
    } finally {
      fresh.close()
    }
  } finally {
    session.close()
    rmSync(outer, { recursive: true, force: true })
  }
})

test('a root the request no longer names leaves the program', TIMEOUT, async (t) => {
  if (!available) return t.skip('no packages/lanekeep/node_modules/typescript')
  // The root set only ever widened: `ensureProgram` unioned the request's files into the held
  // ones, so a file named by one run stayed a root for the sidecar's whole life even though a
  // later run's discovery list no longer named it. That is not merely a stale row in the key —
  // an extra root changes answers, because a `declare global` in it augments what every other
  // root sees. So the held session typed `a.ts` out of a file `lanekeep check` over the same
  // request would not have compiled at all.
  const outer = mkdtempSync(path.join(tmpdir(), 'lanekeep-driver-rootshrink-'))
  const root = path.join(outer, 'app')
  mkdirSync(path.join(root, 'src'), { recursive: true })
  const session = spawnDriver(root)
  try {
    // A config above the root, which lanekeep's confinement refuses, so both files fall to the
    // ad-hoc program — where the root set is the request's list and nothing else.
    writeFileSync(
      path.join(outer, 'tsconfig.json'),
      '{"compilerOptions":{"strict":true},"include":["app/src"]}\n',
    )
    writeFileSync(path.join(root, 'package.json'), '{"name":"app","private":true}\n')
    const kept = path.join(root, 'src/a.ts')
    const dropped = path.join(root, 'src/b.ts')
    const source = 'export const v = lanekeepMark\n'
    writeFileSync(kept, source)
    writeFileSync(dropped, 'export {}\ndeclare global {\n  var lanekeepMark: string\n}\n')

    const first = await session.ask('programs', { files: [kept, dropped] })
    assert.equal(first.ok, true, JSON.stringify(first))
    const augmented = await session.ask('typeOf', { file: kept, ...spanOf(source, 'v') })
    assert.equal(
      augmented.value.primitive,
      'string',
      `the augmentation never reached the answer, so its removal pins nothing: ${JSON.stringify(augmented)}`,
    )

    // The second request names one of the two, and `b.ts` is still on disk — this is a
    // discovery list that shrank, not a deletion.
    const held = await session.ask('programs', { files: [kept] })
    assert.equal(held.ok, true, JSON.stringify(held))
    const heldType = await session.ask('typeOf', { file: kept, ...spanOf(source, 'v') })

    // A provider with no history is what `lanekeep check` runs, and its answer is the one the
    // held session has to match — in the listing, which is the run key, and in the type.
    const fresh = spawnDriver(root)
    try {
      const clean = await fresh.ask('programs', { files: [kept] })
      assert.equal(clean.ok, true, JSON.stringify(clean))
      const freshType = await fresh.ask('typeOf', { file: kept, ...spanOf(source, 'v') })
      assert.ok(
        !clean.value.listing.some(([p]) => p === 'src/b.ts'),
        `the fresh driver already names the dropped root: ${JSON.stringify(clean.value.listing)}`,
      )
      assert.notEqual(
        freshType.value?.primitive,
        'string',
        `the augmentation still reaches a fresh run, so the comparison below is not discriminating: ${JSON.stringify(freshType)}`,
      )
      assert.deepEqual(
        heldType.value,
        freshType.value,
        `the held driver answers ${JSON.stringify(heldType.value)} where a fresh one answers ${JSON.stringify(freshType.value)}`,
      )
      assert.deepEqual(
        held.value.listing,
        clean.value.listing,
        `the held driver lists ${JSON.stringify(held.value.listing)} where a fresh one lists ${JSON.stringify(clean.value.listing)}`,
      )
    } finally {
      fresh.close()
    }
  } finally {
    session.close()
    rmSync(outer, { recursive: true, force: true })
  }
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
