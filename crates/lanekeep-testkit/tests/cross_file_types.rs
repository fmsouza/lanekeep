//! #189's acceptance criteria, as two project rules over real fixture packages.
//!
//! Project rules rather than built-ins on purpose: neither is a convention lanekeep ships an
//! opinion about, and what is under test is the *oracle* rather than a rule. Here rather than
//! in `lanekeep-rules` for the same reason `javascript_dataflow.rs` is — the rule sources are
//! test data, and this crate is where a `RuleTester` case that ships to nobody belongs.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it already \
              makes for unit tests has to be restated for them."
)]

use lanekeep_testkit::RuleTester;

/// An exported `use*` hook must return the query library's own result type.
///
/// The convention: a hook that leaks the raw response instead of `UseQueryResult` puts the
/// library's caching contract in the caller's hands, and nothing in the code says so. This is
/// the shape §3.5 designs `returnTypeOf` for — the answer comes from the library's
/// `function_signature`, by name, with type arguments dropped.
const NO_QUERY_RESULT_LEAK: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/no-query-result-leak',\n\
      language: 'typescript',\n\
      requires: ['types'],\n\
      query: {\n\
        typescript: '(export_statement declaration: (function_declaration \
                      name: (identifier) @name) @fn)',\n\
      },\n\
      card: {\n\
        message: 'a hook must return the query library result type',\n\
        remediation: 'return what useQuery returned, rather than unwrapping it',\n\
        examples: {\n\
          bad: 'export function useOrder() { return fetchOrder(); }',\n\
          good: 'export function useOrder() { return useQuery({}); }',\n\
        },\n\
      },\n\
      check(ctx, m) {\n\
        if (!ctx.text(m.name).startsWith('use')) return;\n\
        const returned = ctx.types.returnTypeOf(m.fn);\n\
        // Silence on `undefined`: the oracle would rather say nothing, and so does this.\n\
        if (returned === undefined) return;\n\
        if (returned.text === 'UseQueryResult') return;\n\
        ctx.report(m.name);\n\
      },\n\
    });\n";

/// The same convention, asked about the *returned call* rather than the function.
///
/// Why a second rule rather than a second case: the rule above asks `returnTypeOf` about a
/// `function_declaration`, which the oracle answers from the annotation or from the body — and
/// a body returning a call to an imported function types as nothing, because `typeOf` of a
/// call is deliberately not its return type. So every case of that rule passes with
/// `imported_return_type` deleted outright: the accepting ones because the rule is silent on
/// `undefined`, the reporting one because a `String(...)` call is typed within the file.
/// Measured, not assumed. Asking about the `call_expression` itself is the shape that has to
/// cross the file boundary — callee, import, the library's own `function_signature` — so it is
/// the one that pins the hook.
const NO_FOREIGN_RESULT_RETURNED: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/no-foreign-result-returned',\n\
      language: 'typescript',\n\
      requires: ['types'],\n\
      query: {\n\
        typescript: '(export_statement declaration: (function_declaration \
                      name: (identifier) @name body: (statement_block \
                      (return_statement (call_expression) @call))))',\n\
      },\n\
      card: {\n\
        message: 'a hook must return the query library result type',\n\
        remediation: 'return what useQuery returned, rather than another result type',\n\
        examples: {\n\
          bad: 'export function useOrder() { return useMutation(); }',\n\
          good: 'export function useOrder() { return useQuery({}); }',\n\
        },\n\
      },\n\
      check(ctx, m) {\n\
        if (!ctx.text(m.name).startsWith('use')) return;\n\
        const returned = ctx.types.returnTypeOf(m.call);\n\
        if (returned === undefined) return;\n\
        if (returned.text === 'UseQueryResult') return;\n\
        ctx.report(m.name);\n\
      },\n\
    });\n";

/// A `bigint` may only be produced on a designated boundary path.
///
/// The other half of the acceptance: a type that arrives through a *helper* in another file,
/// so a within-file oracle answers nothing and the cross-file one answers `bigint`.
const BIGINT_AT_BOUNDARY_ONLY: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/bigint-at-boundary-only',\n\
      language: 'typescript',\n\
      requires: ['types'],\n\
      query: { typescript: '(variable_declarator name: (identifier) @name) @decl' },\n\
      card: {\n\
        message: 'a bigint outside the boundary',\n\
        remediation: 'convert at the boundary and carry a string inland',\n\
        examples: { bad: 'const id = BigInt(raw)', good: 'const id = String(raw)' },\n\
      },\n\
      check(ctx, m) {\n\
        if (ctx.filePath.includes('/boundary/')) return;\n\
        const type = ctx.types.typeOf(m.name);\n\
        if (type?.primitive !== 'bigint') return;\n\
        ctx.report(m.name);\n\
      },\n\
    });\n";

/// Install the `@tanstack/react-query` fixture package into a tester's project.
///
/// Two result types and two functions, because a convention naming one of them is only tested
/// by a case that returns the *other*: a fixture with a single type cannot distinguish "the
/// oracle read the signature" from "the oracle answered nothing and the rule stayed silent".
fn write_query_package(tester: &RuleTester) {
    tester
        .write_fixture(
            "node_modules/@tanstack/react-query/package.json",
            "{\"exports\": {\".\": {\"types\": \"./build/index.d.ts\"}}}",
        )
        .expect("writes the manifest");
    tester
        .write_fixture(
            "node_modules/@tanstack/react-query/build/index.d.ts",
            "export declare class UseQueryResult {}\n\
             export declare function useQuery(options: unknown): UseQueryResult;\n\
             export declare class UseMutationResult {}\n\
             export declare function useMutation(): UseMutationResult;\n",
        )
        .expect("writes the declarations");
}

/// A tester with the `@tanstack/react-query` fixture package installed.
fn query_tester() -> RuleTester {
    let tester = RuleTester::new("no-query-result-leak", NO_QUERY_RESULT_LEAK).expect("builds");
    write_query_package(&tester);
    tester
}

/// A hook that returns the library's own result type is silent.
#[test]
fn a_hook_returning_the_query_result_is_accepted() {
    query_tester()
        .accepts(
            "import { useQuery } from '@tanstack/react-query';\n\
             export function useOrder() { return useQuery({}); }\n",
        )
        .expect("the return resolves through the library's signature to UseQueryResult");
}

/// A hook that unwraps it is reported.
///
/// The pair the test above needs: without it, a rule that never reported anything would pass.
#[test]
fn a_hook_that_unwraps_the_query_result_is_reported() {
    query_tester()
        .reports_at(
            "import { useQuery } from '@tanstack/react-query';\n\
             export function useOrder(): string { return String(useQuery({})); }\n",
            &[(2, 17)],
        )
        .expect("a string is not the library's result type");
}

/// And a package that is not installed leaves the hook alone.
///
/// The silence posture end to end: no declarations, no answer, no report — rather than a run
/// that accuses every hook in a project whose dependencies were not fetched.
#[test]
fn a_hook_is_left_alone_when_the_library_is_not_installed() {
    RuleTester::new("no-query-result-leak-bare", NO_QUERY_RESULT_LEAK)
        .expect("builds")
        .accepts(
            "import { useQuery } from '@tanstack/react-query';\n\
             export function useOrder() { return useQuery({}); }\n",
        )
        .expect("nothing readable said what useQuery returns");
}

/// A tester for [`NO_FOREIGN_RESULT_RETURNED`], over the same fixture package.
fn returned_call_tester() -> RuleTester {
    let tester =
        RuleTester::new("no-foreign-result-returned", NO_FOREIGN_RESULT_RETURNED).expect("builds");
    write_query_package(&tester);
    tester
}

/// Returning the library's own result is accepted, through the library's signature.
#[test]
fn a_returned_call_into_the_library_is_accepted() {
    returned_call_tester()
        .accepts(
            "import { useQuery } from '@tanstack/react-query';\n\
             export function useOrder() { return useQuery({}); }\n",
        )
        .expect("the call resolves through the library's signature to UseQueryResult");
}

/// And returning a *different* type from the same library is reported.
///
/// The row nothing else in this file supplies: the only thing that can say `useMutation()` is
/// a `UseMutationResult` is the library's own `function_signature`, read across the file
/// boundary through `imported_return_type`.
#[test]
fn a_returned_call_yielding_a_second_library_type_is_reported() {
    returned_call_tester()
        .reports_at(
            "import { useMutation } from '@tanstack/react-query';\n\
             export function useThing() { return useMutation(); }\n",
            &[(2, 17)],
        )
        .expect("the mutation result is not the query result");
}

/// A tester with a local helper module whose declared return type is a `bigint`.
///
/// The helper lives at `lib/ids.ts`, outside `subject/`, because `RuleTester::run` clears the
/// whole `subject/` directory before writing each case's own `subject/input.ts` — a fixture
/// written under `subject/` by `write_fixture` would not survive to the run that is supposed
/// to read it. Every other `write_fixture` caller in this repository writes outside
/// `subject/` for the same reason (`node_modules/...` in the query-result fixtures above).
///
/// **The type assertion above the export is load-bearing.** `<string>raw` parses under the
/// TypeScript grammar and is an `ERROR` under the TSX one, where `<string>` opens a JSX
/// element — and the `ERROR` swallows the rest of the file, so the `parsed` export below it
/// is not there at all. A provider that picked its grammar by taking the first language that
/// probes (`tsx` sorts before `typescript`) therefore answers nothing about `parsed` and the
/// report disappears. Measured rather than assumed: the generic arrow the review proposed
/// for this — `<T>(x: T): T => x` — parses identically under both grammars, so it would have
/// pinned nothing. A `.ts` rather than a `.d.ts` because that is the file a project source
/// import really names, and because `RELATIVE_SUFFIXES` probes `.ts` first.
fn bigint_tester() -> RuleTester {
    let tester =
        RuleTester::new("bigint-at-boundary-only", BIGINT_AT_BOUNDARY_ONLY).expect("builds");
    tester
        .write_fixture(
            "lib/ids.ts",
            "declare const raw: unknown;\n\
             export const label = <string>raw;\n\
             export declare const parsed: bigint;\n",
        )
        .expect("writes the helper declarations");
    tester
}

/// A `bigint` arriving through a helper in another file is reported off the boundary.
#[test]
fn a_bigint_from_a_helper_module_is_reported_off_the_boundary() {
    bigint_tester()
        .reports_at(
            "import { parsed } from '../lib/ids';\nconst id = parsed;\n",
            &[(2, 7)],
        )
        .expect("the helper declares a bigint, and this file is not the boundary");
}

/// The literal form, which needs no cross-file resolution — the control that says the rule
/// works at all, so the test above is about the *oracle* rather than about the rule.
#[test]
fn a_bigint_literal_is_reported_off_the_boundary() {
    bigint_tester()
        .reports_at("const id = 1n;\n", &[(1, 7)])
        .expect("a bigint literal is a bigint");
}

/// And a designated path is exempt, which is the convention's whole point.
///
/// `RuleTester` writes the subject at `subject/input.ts`, so the exemption is asserted through
/// a rule whose predicate cannot match it — the fixture's own helper import is the same, and
/// the report is what changes.
#[test]
fn the_designated_boundary_path_is_exempt() {
    let tester = RuleTester::new(
        "bigint-boundary",
        BIGINT_AT_BOUNDARY_ONLY
            .replace("/boundary/", "subject/")
            .as_str(),
    )
    .expect("builds");
    tester
        .accepts("const id = 1n;\n")
        .expect("the subject path is the designated boundary in this fixture");
}
