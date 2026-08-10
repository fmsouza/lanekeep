/**
 * The entry module of the benchmark's JavaScript component.
 *
 * `crates/lanekeep-rules/typescript/entry.ts` is the shipped one and this is its one-rule
 * sibling: same runtime, same `register`, same seven exports. It exists because
 * `benches/crossings.rs` needs `lanekeep/no-unwrap` in all three of the forms it compares, and
 * the shipped component hosts the four built-ins rather than this rule.
 *
 * **The runtime import is first, and that ordering is load-bearing** — see `entry.ts` and
 * `packages/lanekeep/runtime/entry.js` for why. It is copied here rather than assumed: a
 * benchmark whose component was built differently from the shipped one would be measuring a
 * component nothing ships.
 *
 * The artifact this builds is **not committed**. It is 13 MB, every crate in this workspace is
 * published, and crates.io refuses a package over 10 MiB — so `just bench-js-component` writes
 * it under `target/`, which is gitignored, and `crossings.rs` runs two arms when it is not
 * there and says so.
 */

// First. Not import hygiene, the sandbox.
import { register } from 'lanekeep/runtime/entry'

import noUnwrap from './no-unwrap.ts'

register([noUnwrap])

export { rules, metadata, configure, hasCheck, hasReduce, check, reduce } from 'lanekeep/runtime/entry'
