//! `lanekeep/no-unused-exports`, run through the real engine.
//!
//! A cross-file rule needs more than one file, so these build a project directly rather
//! than going through `RuleTester`, which runs one subject at a time.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The `corpus` helpers are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

mod corpus;

use corpus::Corpus;

fn corpus(options: &str, files: &[(&str, &str)]) -> Corpus {
    Corpus::new("no-unused-exports", options, files)
}

#[test]
fn an_imported_export_is_not_reported() {
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export function used() {}\n"),
            ("src/b.ts", "import { used } from './a';\nused();\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_export_nobody_imports_is_reported() {
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "export function used() {}\nexport function orphan() {}\n",
            ),
            ("src/b.ts", "import { used } from './a';\nused();\n"),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec!["src/a.ts:2:1 'orphan' is exported but nothing imports it"]
    );
}

#[test]
fn a_symbol_of_the_same_name_from_another_module_does_not_count() {
    // The reason matching is by (module, symbol) rather than by name. `parse` is a common
    // name; a rule that matched on it alone would go quiet on any real codebase.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export function parse() {}\n"),
            ("src/b.ts", "export function parse() {}\n"),
            ("src/c.ts", "import { parse } from './b';\nparse();\n"),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec!["src/a.ts:1:1 'parse' is exported but nothing imports it"]
    );
}

#[test]
fn classes_and_constants_are_covered_too() {
    let found = corpus(
        "{}",
        &[(
            "src/a.ts",
            "export class Parser {}\nexport const VERSION = 1;\n",
        )],
    )
    .run();
    assert_eq!(
        found,
        vec![
            "src/a.ts:1:1 'Parser' is exported but nothing imports it",
            "src/a.ts:2:1 'VERSION' is exported but nothing imports it",
        ]
    );
}

#[test]
fn an_import_without_an_extension_resolves() {
    // `./a` has to find `a.ts`. Without this the rule reports everything.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const x = 1;\n"),
            ("src/b.ts", "import { x } from './a';\nconsole.log(x);\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_directory_import_resolves_to_its_index() {
    let found = corpus(
        "{}",
        &[
            ("src/thing/index.ts", "export const x = 1;\n"),
            (
                "src/b.ts",
                "import { x } from './thing';\nconsole.log(x);\n",
            ),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_import_that_walks_up_resolves() {
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const x = 1;\n"),
            (
                "src/deep/b.ts",
                "import { x } from '../a';\nconsole.log(x);\n",
            ),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_namespace_import_covers_everything_in_the_target() {
    // `import * as a` does not name what it takes, so anything in `a` is demonstrably
    // reachable. Reporting those would be plainly wrong.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const x = 1;\nexport const y = 2;\n"),
            ("src/b.ts", "import * as a from './a';\nconsole.log(a);\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_star_re_export_covers_everything_in_the_target() {
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const x = 1;\nexport const y = 2;\n"),
            ("src/index.ts", "export * from './a';\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_entry_point_is_exempt() {
    // A library's public API is imported from outside the corpus. Without this the rule
    // reports every exported symbol of every package, which is true and useless.
    let found = corpus(
        "{ entryPoints: ['src/index.ts'] }",
        &[("src/index.ts", "export function publicApi() {}\n")],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_non_entry_point_is_still_reported_when_entry_points_are_configured() {
    let found = corpus(
        "{ entryPoints: ['src/index.ts'] }",
        &[
            ("src/index.ts", "export function publicApi() {}\n"),
            ("src/internal.ts", "export function hidden() {}\n"),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec!["src/internal.ts:1:1 'hidden' is exported but nothing imports it"]
    );
}

#[test]
fn a_package_import_is_ignored_rather_than_resolved() {
    // `lodash` names nothing in the corpus. The export below is still unused, and the
    // unresolvable import must not accidentally mark it used.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const merge = 1;\n"),
            (
                "src/b.ts",
                "import { merge } from 'lodash';\nconsole.log(merge);\n",
            ),
        ],
    )
    .run();
    assert_eq!(
        found,
        vec!["src/a.ts:1:1 'merge' is exported but nothing imports it"]
    );
}

#[test]
fn an_aliased_import_is_matched_on_the_exported_name() {
    // `import { x as y }` consumes `x`. Matching on `y` would report `x` as unused.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export const x = 1;\n"),
            (
                "src/b.ts",
                "import { x as y } from './a';\nconsole.log(y);\n",
            ),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_aliased_export_is_reported_under_the_name_it_publishes() {
    let found = corpus(
        "{}",
        &[(
            "src/a.ts",
            "const internal = 1;\nexport { internal as published };\n",
        )],
    )
    .run();
    assert_eq!(
        found,
        vec!["src/a.ts:2:1 'published' is exported but nothing imports it"]
    );
}

#[test]
fn the_same_corpus_reports_the_same_thing_every_run() {
    let corpus = corpus(
        "{}",
        &[
            ("src/a.ts", "export const a1 = 1;\nexport const a2 = 2;\n"),
            ("src/b.ts", "export const b1 = 1;\nexport const b2 = 2;\n"),
            ("src/c.ts", "export const c1 = 1;\nexport const c2 = 2;\n"),
            ("src/d.ts", "import { a1 } from './a';\nconsole.log(a1);\n"),
        ],
    );
    let first = corpus.run();
    assert!(!first.is_empty(), "the fixture should report something");
    for attempt in 0..4 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
}
