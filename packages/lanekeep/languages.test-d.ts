/**
 * Type-level check that `language` and a per-language `query` accept every registered
 * language, and nothing else.
 *
 * `LanguageId` is rendered by `crates/lanekeep-types-gen` from a hand-kept list, not read off
 * the registry, so a language registered in `crates/lanekeep-languages` and missing here would
 * load and run while every TypeScript author was told it does not exist (#283).
 */

import { defineRule } from './index'

const card = { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } }

defineRule({
  id: 'local/stylesheets-and-manifests',
  severity: 'error',
  language: ['css', 'json', 'toml', 'yaml'],
  query: {
    css: '(declaration) @n',
    json: '(pair) @n',
    toml: '(pair) @n',
    yaml: '(block_mapping_pair) @n',
  },
  card,
  check(ctx, match) {
    if (match.n !== undefined) ctx.report(match.n)
  },
})

defineRule({
  id: 'local/one-language',
  severity: 'error',
  language: 'yaml',
  query: '(comment) @n',
  card,
  check() {},
})

defineRule({
  id: 'local/a-preprocessor',
  severity: 'error',
  // @ts-expect-error Sass is a different grammar and is not registered
  language: ['scss'],
  query: '(declaration) @n',
  card,
  check() {},
})
