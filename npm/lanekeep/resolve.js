/**
 * Find the platform binary this machine should run.
 *
 * lanekeep ships one npm package per platform and a launcher — this one — that depends on
 * all of them as `optionalDependencies`. npm installs only the one whose `os` and `cpu` match,
 * so a developer downloads one binary rather than five.
 *
 * **Node is not required to run lanekeep.** It is required only to install it this way. The
 * binary has the JavaScript engine compiled in; this file exists to pick which binary.
 */

const { existsSync } = require('node:fs')

/**
 * The platform packages, keyed by what Node reports.
 *
 * Written out rather than composed from `${platform}-${arch}`, so a platform lanekeep does
 * not publish for produces a message naming what is available instead of a confusing
 * "cannot find module @lanekeep/sunos-sparc".
 */
const PACKAGES = {
  'darwin-arm64': '@lanekeep/darwin-arm64',
  'darwin-x64': '@lanekeep/darwin-x64',
  'linux-arm64': '@lanekeep/linux-arm64',
  'linux-x64': '@lanekeep/linux-x64',
  'win32-x64': '@lanekeep/win32-x64',
}

/** Where the binary sits inside a platform package. */
function binaryName(platform) {
  return platform === 'win32' ? 'lanekeep.exe' : 'lanekeep'
}

/**
 * The path to this machine's lanekeep binary.
 *
 * @throws if this platform has no package, or the package is missing.
 */
function resolveBinary(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`
  const pkg = PACKAGES[key]

  if (!pkg) {
    throw new Error(
      `lanekeep does not ship a binary for ${key}\n` +
        `  available: ${Object.keys(PACKAGES).sort().join(', ')}\n` +
        `  build from source with: cargo install lanekeep-cli`,
    )
  }

  let binary
  try {
    // Resolved through Node rather than joined by hand, so it works with npm, pnpm's
    // symlinked store, and Yarn's zero-install layout — three arrangements that agree on
    // `require.resolve` and on nothing else.
    binary = require.resolve(`${pkg}/bin/${binaryName(platform)}`)
  } catch {
    throw new Error(
      `lanekeep's binary for ${key} is not installed\n` +
        `  expected the optional dependency ${pkg}\n` +
        `  this usually means the install ran with --no-optional, or on a different platform\n` +
        `  reinstall with: npm install lanekeep`,
    )
  }

  if (!existsSync(binary)) {
    throw new Error(`lanekeep's binary for ${key} is missing at ${binary}`)
  }

  return binary
}

module.exports = { PACKAGES, binaryName, resolveBinary }
