/**
 * Type-level check that a rule declaring `requires: ['types']` can read `ctx.types` against
 * the shipped `index.d.ts`, and that `TypeInfo`'s shape rejects what it must.
 *
 * Hand-written sibling of the generated `types.test-d.ts` — note the naming collision: that
 * file gates built-in subpath imports (`crates/lanekeep-package-gen`) and has nothing to do
 * with `ctx.types`. `TypeApi`/`TypeInfo`/`SymbolInfo` are fixed templates in
 * `crates/lanekeep-types-gen`, not derived field-by-field from `world.wit` (the world has no
 * presence for any of them — see `RuleContext.types`'s own doc comment), so nothing but a
 * compile guards their shape.
 */

import { defineRule, TypeInfo } from './index'

defineRule({
  id: 'local/ctx-types',
  severity: 'error',
  query: '(identifier) @id',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  requires: ['types'],
  check(ctx, match) {
    // A capture is `Node | undefined` — a capture that did not participate is absent.
    if (match.id === undefined) return

    // `typeOf` returning `undefined` is a first-class answer, not a failure: a rule is
    // expected to check for it and quietly stay silent, the same posture a dead-handle read
    // takes everywhere else on `ctx`.
    const info = ctx.types.typeOf(match.id)
    if (info === undefined) return

    // `text` is display-only — fine to read, but a rule branches on the fields below.
    info.text.toUpperCase()

    // No trailing `else`: an unresolvable nominal type (a global or ambient type used with
    // no local declaration or import, `Date` being the ordinary example) sets none of the
    // three, so falling through having read only `info.text` above is a real, valid outcome
    // here — not a case a rule can treat as unreachable.
    if (info.primitive !== undefined) {
      info.primitive.toUpperCase()
    } else if (info.symbol !== undefined) {
      info.symbol.name.toUpperCase()
      if (info.symbol.module !== undefined) info.symbol.module.toUpperCase()
    } else if (info.union !== undefined) {
      info.union.map((member) => member.text)
    }

    // `symbolOf` is the other half of the oracle, on the same `undefined`-is-an-answer terms.
    const symbol = ctx.types.symbolOf(match.id)
    if (symbol !== undefined) symbol.name.toUpperCase()
  },
})

// The primitive union is closed to exactly what the oracle recognizes — `any` and `unknown`
// are deliberately absent, and so is anything else TypeScript itself would not call a
// primitive.
// @ts-expect-error 'array' is not one of the seven primitives the oracle recognizes
const wrongPrimitive: TypeInfo = { text: 'number[]', primitive: 'array' }

// There is deliberately no `complete` field: nothing in this milestone can make the oracle's
// answer partial, and a field that never varies would only teach a rule to ignore it.
// @ts-expect-error TypeInfo has no `complete` field
const noCompleteField: TypeInfo = { text: 'number', primitive: 'number', complete: true }

// Not a `@ts-expect-error` case — this one is expected to compile. All three of `primitive`,
// `symbol` and `union` are optional, so a value with none of them set already satisfies the
// type; this line pins that down rather than leaving it implicit, so a future edit that made
// any one of the three required would fail here. What a `.test-d.ts` file cannot do is assert
// *when* a real `ctx.types.typeOf(...)` call produces this shape — that a nominal type the
// resolver could not attribute (an unresolvable, global or ambient type, `Date` being the
// ordinary example) renders this way is runtime behavior, checked by tsc not at all.
//
// That half is covered by exactly two tests, named rather than gestured at because the
// earlier version of this comment gestured at `render_type` and the oracle tests while
// neither of them actually asserted this shape — every `Nominal` assertion in both carried a
// `symbol`:
// `types_renders_an_ambient_nominal_with_no_symbol_at_all` in `crates/lanekeep-js/src/host.rs`
// asserts the rendered object is `{"text":"Date"}` and carries no `symbol` at all, and
// `an_ambient_type_is_nominal_with_no_symbol` in `crates/lanekeep-types/tests/oracle.rs`
// asserts the `Type` underneath it. Both use `Date` for the reason above, and both had to be
// written as whole-value assertions: the bug they exist to catch is a `symbol` being
// *present*, which no assertion about a field's value can see.
const unresolvableNominal: TypeInfo = { text: 'Date' }
