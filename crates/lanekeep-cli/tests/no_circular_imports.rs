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

// --- tsconfig `compilerOptions.paths` and `baseUrl` (#281) ----------------------------------
//
// Each of these pairs a `tsconfig.json` with a two-file cycle written through an alias. The
// relative-specifier tests above are the control: an aliased cycle is the same cycle, and the
// rule reporting one and not the other is the bug these exist to keep fixed.

/// `src/a.ts` and `src/b.ts`, importing each other through `prefix` + the module name.
fn aliased_cycle(prefix: &str) -> [(String, String); 2] {
    [
        (
            "src/a.ts".to_owned(),
            format!("import {{ b }} from '{prefix}b';\nexport const a = b;\n"),
        ),
        (
            "src/b.ts".to_owned(),
            format!("import {{ a }} from '{prefix}a';\nexport const b = a;\n"),
        ),
    ]
}

/// A corpus of `files` plus a `tsconfig.json` (or any other extra files) beside them.
fn with_config(extra: &[(&str, &str)], files: &[(String, String)]) -> Corpus {
    let mut layout: Vec<(&str, &str)> = extra.to_vec();
    layout.extend(files.iter().map(|(p, c)| (p.as_str(), c.as_str())));
    corpus("{}", &layout)
}

const CYCLE: &str = "circular import: src/a.ts → src/b.ts → src/a.ts";

fn assert_one_cycle(found: &[String]) {
    assert_eq!(found.len(), 1, "one cycle, one violation: {found:?}");
    assert!(found[0].contains(CYCLE), "{found:?}");
}

#[test]
fn an_aliased_cycle_is_reported_like_a_relative_one() {
    // The issue's own reproduction, verbatim.
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "~/*": ["src/*"] } } }"#,
        )],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn paths_without_base_url_resolve_against_the_config_directory() {
    // TypeScript 4.1 and later: with no `baseUrl`, substitutions are relative to the config
    // that declares `paths`.
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@app/*": ["./src/*"] } } }"#,
        )],
        &aliased_cycle("@app/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn base_url_alone_resolves_a_bare_specifier() {
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
        )],
        &aliased_cycle(""),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn the_longest_matching_prefix_wins() {
    // `*` matches everything, and listing it first must not let it shadow the more specific
    // pattern — TypeScript picks the pattern with the longest prefix, not the first one.
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "*": ["nowhere/*"], "~/*": ["src/*"] } } }"#,
        )],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn the_first_substitution_that_exists_wins() {
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "~/*": ["generated/*", "src/*"] } } }"#,
        )],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn an_exact_key_wins_over_a_wildcard() {
    let found = with_config(
        &[(
            "tsconfig.json",
            r##"{ "compilerOptions": { "paths": {
                "#b": ["src/b.ts"], "#a": ["src/a"], "#*": ["nowhere/*"]
            } } }"##,
        )],
        &aliased_cycle("#"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn comments_and_trailing_commas_in_the_tsconfig_are_accepted() {
    // What `tsc --init` writes, and what nearly every real tsconfig looks like. The `$schema`
    // URL is a `//` inside a string, which a comment stripper that ignores strings would cut.
    let found = with_config(
        &[(
            "tsconfig.json",
            "\u{feff}{\n  \"$schema\": \"https://json.schemastore.org/tsconfig\",\n  // aliases\n  \"compilerOptions\": {\n    /* the base */ \"baseUrl\": \".\",\n    \"paths\": { \"~/*\": [\"src/*\",], },\n  },\n}\n",
        )],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn paths_from_an_extended_config_resolve_against_its_own_directory() {
    let found = with_config(
        &[
            ("tsconfig.json", r#"{ "extends": "./config/base" }"#),
            (
                "config/base.json",
                r#"{ "compilerOptions": { "paths": { "~/*": ["../src/*"] } } }"#,
            ),
        ],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn an_extending_config_replaces_the_base_paths_wholesale() {
    // `compilerOptions` merges key by key, and `paths` is one key: the child's map replaces
    // the base's rather than being merged into it.
    let found = with_config(
        &[
            (
                "tsconfig.json",
                r#"{ "extends": ["./base.json"], "compilerOptions": { "paths": { "@x/*": ["x/*"] } } }"#,
            ),
            (
                "base.json",
                r#"{ "compilerOptions": { "paths": { "~/*": ["src/*"] } } }"#,
            ),
        ],
        &aliased_cycle("~/"),
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn the_nearest_tsconfig_wins() {
    let found = with_config(
        &[
            (
                "tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "~/*": ["nowhere/*"] } } }"#,
            ),
            (
                "src/tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "~/*": ["./*"] } } }"#,
            ),
        ],
        &aliased_cycle("~/"),
    )
    .run();
    assert_one_cycle(&found);
}

#[test]
fn an_unmapped_bare_specifier_is_still_not_an_edge() {
    let found = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "~/*": ["src/*"] } } }"#,
        )],
        &aliased_cycle(""),
    )
    .run();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn editing_paths_invalidates_a_warm_cache() {
    // The tsconfig is read through `ctx.readFile`, so it is a tracked dependency of every file
    // whose facts used it. Without that, the second run would replay the first run's facts
    // from the cache and keep reporting a cycle the configuration no longer produces.
    let mapped = r#"{ "compilerOptions": { "paths": { "~/*": ["src/*"] } } }"#;
    let unmapped = r#"{ "compilerOptions": { "paths": { "~/*": ["nowhere/*"] } } }"#;
    let project = with_config(&[("tsconfig.json", mapped)], &aliased_cycle("~/"));

    assert_one_cycle(&project.run());

    project.write("tsconfig.json", unmapped);
    assert_eq!(
        project.run(),
        Vec::<String>::new(),
        "the edit must invalidate"
    );

    project.write("tsconfig.json", mapped);
    assert_one_cycle(&project.run());
}

#[test]
fn creating_a_nearer_tsconfig_invalidates_a_warm_cache() {
    // The other half of tracking: a `tsconfig.json` that was *absent* in `src/` was depended on
    // too, so creating one has to change the answer.
    let project = with_config(
        &[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "~/*": ["nowhere/*"] } } }"#,
        )],
        &aliased_cycle("~/"),
    );
    assert_eq!(project.run(), Vec::<String>::new());

    project.write(
        "src/tsconfig.json",
        r#"{ "compilerOptions": { "paths": { "~/*": ["./*"] } } }"#,
    );
    assert_one_cycle(&project.run());
}

#[test]
fn a_tsconfig_that_is_not_json_cancels_the_run() {
    // Silently ignoring it would be exactly the silent miss this support exists to end, and
    // `tsc` refuses such a config too.
    let said = with_config(
        &[("tsconfig.json", r#"{ "compilerOptions": { "paths": "#)],
        &aliased_cycle("~/"),
    )
    .run_failing();
    assert!(said.contains("tsconfig.json"), "{said}");
}
