//! `lanekeep/no-circular-imports`, run through the binary over a real corpus.

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
    Corpus::new("no-circular-imports", options, files)
}

#[test]
fn an_acyclic_graph_reports_nothing() {
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import { b } from './b';\nexport const a = b;\n",
            ),
            (
                "src/b.ts",
                "import { c } from './c';\nexport const b = c;\n",
            ),
            ("src/c.ts", "export const c = 1;\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_two_file_cycle_is_reported() {
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import { b } from './b';\nexport const a = b;\n",
            ),
            (
                "src/b.ts",
                "import { a } from './a';\nexport const b = a;\n",
            ),
        ],
    )
    .run();
    assert_eq!(found.len(), 1, "one cycle, one violation: {found:?}");
    assert!(
        found[0].contains("circular import: src/a.ts → src/b.ts → src/a.ts"),
        "{found:?}"
    );
}

#[test]
fn a_longer_cycle_is_reported_once() {
    // Three members, one problem. Reporting per member would turn one cycle into three
    // violations and leave the reader to work out they are the same thing.
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import { b } from './b';\nexport const a = b;\n",
            ),
            (
                "src/b.ts",
                "import { c } from './c';\nexport const b = c;\n",
            ),
            (
                "src/c.ts",
                "import { a } from './a';\nexport const c = a;\n",
            ),
        ],
    )
    .run();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("src/a.ts → src/b.ts → src/c.ts → src/a.ts"),
        "{found:?}"
    );
}

#[test]
fn two_independent_cycles_are_both_reported() {
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import { b } from './b';\nexport const a = b;\n",
            ),
            (
                "src/b.ts",
                "import { a } from './a';\nexport const b = a;\n",
            ),
            (
                "src/x.ts",
                "import { y } from './y';\nexport const x = y;\n",
            ),
            (
                "src/y.ts",
                "import { x } from './x';\nexport const y = x;\n",
            ),
        ],
    )
    .run();
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn a_self_import_is_not_a_cycle() {
    // A module importing itself is a different mistake, and reporting it here would be
    // confusing advice — there is no second module to extract anything into.
    let found = corpus(
        "{}",
        &[(
            "src/a.ts",
            "import { a } from './a';\nexport const b = a;\n",
        )],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_diamond_is_not_a_cycle() {
    // Two paths to the same module is ordinary structure. A search that confused a
    // revisited node with a node on the current path would report this.
    let found = corpus(
        "{}",
        &[
            (
                "src/top.ts",
                "import { l } from './left';\nimport { r } from './right';\nexport const t = l + r;\n",
            ),
            ("src/left.ts", "import { s } from './shared';\nexport const l = s;\n"),
            ("src/right.ts", "import { s } from './shared';\nexport const r = s;\n"),
            ("src/shared.ts", "export const s = 1;\n"),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_re_export_edge_counts() {
    // `export ... from` is an import edge with different syntax, and a cycle through one
    // fails at runtime exactly the same way.
    let found = corpus(
        "{}",
        &[
            ("src/a.ts", "export { b } from './b';\n"),
            (
                "src/b.ts",
                "import { a } from './a';\nexport const b = a;\n",
            ),
        ],
    )
    .run();
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_package_import_is_not_an_edge() {
    let found = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import merge from 'lodash';\nexport const a = merge;\n",
            ),
            (
                "src/b.ts",
                "import path from 'node:path';\nexport const b = path;\n",
            ),
        ],
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn max_depth_bounds_the_search() {
    // A cycle longer than the bound is not reported. Deliberate: an unbounded search on a
    // pathological graph is the one way this rule could dominate a run.
    let files: Vec<(String, String)> = (0..6)
        .map(|i| {
            let next = (i + 1) % 6;
            (
                format!("src/m{i}.ts"),
                format!("import {{ m{next} }} from './m{next}';\nexport const m{i} = m{next};\n"),
            )
        })
        .collect();
    let layout: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();

    let generous = Corpus::new("no-circular-imports", "{ maxDepth: 24 }", &layout).run();
    assert_eq!(
        generous.len(),
        1,
        "the six-file cycle should be found: {generous:?}"
    );

    let tight = Corpus::new("no-circular-imports", "{ maxDepth: 3 }", &layout).run();
    assert_eq!(
        tight,
        Vec::<String>::new(),
        "the bound should stop the search"
    );
}

#[test]
fn the_same_corpus_reports_the_same_thing_every_run() {
    // Determinism at the level a reader sees: same cycles, same order, same anchors.
    let corpus = corpus(
        "{}",
        &[
            (
                "src/a.ts",
                "import { b } from './b';\nexport const a = b;\n",
            ),
            (
                "src/b.ts",
                "import { a } from './a';\nexport const b = a;\n",
            ),
            (
                "src/x.ts",
                "import { y } from './y';\nexport const x = y;\n",
            ),
            (
                "src/y.ts",
                "import { z } from './z';\nexport const y = z;\n",
            ),
            (
                "src/z.ts",
                "import { x } from './x';\nexport const z = x;\n",
            ),
        ],
    );
    let first = corpus.run();
    assert_eq!(first.len(), 2, "{first:?}");
    for attempt in 0..4 {
        assert_eq!(corpus.run(), first, "output changed on attempt {attempt}");
    }
}
