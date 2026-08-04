import { defineRule } from 'lanekeep'
import { inTestCode } from '../modules/rust'

/**
 * A tree-sitter parser constructed outside the one shared per-file parse.
 *
 * §2's "run compiled queries (one pass)" and §7's "single shared parse". The cost of breaking
 * this is invisible: parsing per rule produces identical output and simply runs N times
 * slower. It did exactly that until measured — a file admitted by twenty rules was parsed
 * twenty times, which was most of a cold run and showed up in `--profile` as *query* time,
 * making rules that matched nothing look expensive to match.
 *
 * This replaces a unit test that counted occurrences of the substring
 * `tree_sitter::Parser::new()` in one file. Matching the AST instead covers the whole
 * workspace, distinguishes test code structurally rather than by splitting on `#[cfg(test)]`,
 * and cannot be defeated by a line break inside the call.
 */
export default function oneParserPerFile(options) {
  const allow = options?.allow ?? []

  return defineRule({
    id: 'local/one-parser-per-file',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'tree-sitter parser constructed outside the shared parse',
      remediation:
        'take the tree from the file-level parse — parsing per rule is identical output, N times slower',
      examples: {
        bad: 'let mut parser = tree_sitter::Parser::new();',
        good: 'let tree = file_tree.clone();',
      },
    },

    gates: { fileContains: ['Parser::new'] },

    query: '(call_expression function: (scoped_identifier) @callee) @call',

    check(ctx, m) {
      if (!ctx.text(m.callee).endsWith('Parser::new')) return
      if (allow.includes(ctx.filePath)) return
      if (inTestCode(ctx, m.call)) return

      ctx.report(m.call, {
        message:
          'a second parser means this file is parsed more than once, which is invisible except as time',
      })
    },
  })
}
