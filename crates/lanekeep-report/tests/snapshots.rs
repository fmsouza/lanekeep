//! Snapshot-verified output for every reporter — §16's M0 acceptance criterion.
//!
//! The four formats are user-facing surfaces. A human reads one, a CI annotation is built
//! from another, an agent acts on a third, and a script parses the fourth. Any change to
//! them is a change to something outside this repository, and the assertions elsewhere in
//! this crate check *properties* — that a rule appears once, that the fix precedes the
//! locations — not the shape of the whole document.
//!
//! A property test says the grouping is right. A snapshot says the output is what someone
//! last looked at and approved. Both matter, and only the second catches an accidental
//! extra blank line, a reordered field, or a summary line that quietly changed wording.
//!
//! Run `just snapshot` to review a pending change, `just snapshot-accept` to take it — and
//! read the diff first, because accepting is how a wrong change becomes the expected one.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use lanekeep_core::fix::Fix;
use lanekeep_core::{
    Examples, FilePath, Location, Position, RuleCard, RuleId, Severity, Violation,
};
use lanekeep_report::{Cards, Color, Format, Summary, render};

fn violation(rule: &str, file: &str, line: u32, column: u32, severity: Severity) -> Violation {
    Violation {
        rule_id: rule.parse::<RuleId>().expect("valid rule id"),
        location: Location::new(FilePath::new(file), Position::new(line, column)),
        message: "default export".to_owned(),
        remediation: "use a named export, so the symbol has one name every importer must use"
            .to_owned(),
        severity,
        fix: None,
    }
}

/// A corpus with the shapes that make the formats differ from one another.
///
/// Two rules so the agent format has something to group; two files per rule so it has
/// something to collapse; a per-violation message that differs from the card's, because
/// keeping that is a decision worth pinning; a fix, because it appears in JSON and nowhere
/// else; and one warning among the errors, because severity renders differently in all four.
fn corpus() -> Vec<Violation> {
    let mut specific = violation(
        "lanekeep/no-restricted-imports",
        "src/b.ts",
        1,
        1,
        Severity::Warn,
    );
    "importing 'lodash/merge' is restricted — use the standard library"
        .clone_into(&mut specific.message);
    "import something permitted here, or move this code where it is allowed"
        .clone_into(&mut specific.remediation);

    let mut fixable = violation(
        "lanekeep/no-default-export",
        "src/a.ts",
        9,
        1,
        Severity::Error,
    );
    fixable.fix = Some(Fix {
        start: 120,
        end: 134,
        replacement: "export function".to_owned(),
        safe: true,
    });

    let mut violations = vec![
        violation(
            "lanekeep/no-default-export",
            "src/a.ts",
            2,
            1,
            Severity::Error,
        ),
        fixable,
        violation(
            "lanekeep/no-default-export",
            "src/deep/c.ts",
            4,
            7,
            Severity::Error,
        ),
        specific,
    ];
    lanekeep_core::sort(&mut violations);
    violations
}

fn cards() -> Cards {
    let mut cards = Cards::new();
    cards.insert(
        "lanekeep/no-default-export"
            .parse::<RuleId>()
            .expect("valid rule id"),
        RuleCard {
            message: "default export".to_owned(),
            remediation: "use a named export, so the symbol has one name every importer must use"
                .to_owned(),
            examples: Examples {
                bad: "export default function parse() {}".to_owned(),
                good: "export function parse() {}".to_owned(),
            },
        },
    );
    cards.insert(
        "lanekeep/no-restricted-imports"
            .parse::<RuleId>()
            .expect("valid rule id"),
        RuleCard {
            message: "restricted import".to_owned(),
            remediation: "import something permitted here, or move this code where it is allowed"
                .to_owned(),
            examples: Examples {
                bad: "import Stripe from 'stripe'".to_owned(),
                good: "import { charge } from '@app/payments'".to_owned(),
            },
        },
    );
    cards
}

fn summary() -> Summary {
    Summary {
        files_discovered: 12,
        files_parsed: 3,
        warn_only: false,
    }
}

/// Render without color, which is what every non-terminal consumer sees.
fn rendered(format: Format) -> String {
    render(format, Color::Never, &corpus(), summary(), &cards())
}

#[test]
fn human() {
    insta::assert_snapshot!(rendered(Format::Human));
}

#[test]
fn human_when_clean() {
    insta::assert_snapshot!(render(
        Format::Human,
        Color::Never,
        &[],
        summary(),
        &Cards::new()
    ));
}

#[test]
fn human_with_color() {
    // The escape sequences are part of the output. A snapshot is the only assertion that
    // notices if one moves.
    insta::assert_snapshot!(render(
        Format::Human,
        Color::Always,
        &corpus(),
        summary(),
        &cards()
    ));
}

#[test]
fn json() {
    insta::assert_snapshot!(rendered(Format::Json));
}

#[test]
fn json_when_clean() {
    // A clean run is still a full document — anything parsing this is entitled to the same
    // shape whether or not anything was found.
    insta::assert_snapshot!(render(
        Format::Json,
        Color::Never,
        &[],
        summary(),
        &Cards::new()
    ));
}

#[test]
fn sarif() {
    insta::assert_snapshot!(rendered(Format::Sarif));
}

#[test]
fn agent() {
    insta::assert_snapshot!(rendered(Format::Agent));
}

#[test]
fn agent_when_clean() {
    insta::assert_snapshot!(render(
        Format::Agent,
        Color::Never,
        &[],
        summary(),
        &Cards::new()
    ));
}

#[test]
fn warn_only_changes_the_summary() {
    insta::assert_snapshot!(render(
        Format::Human,
        Color::Never,
        &corpus(),
        Summary {
            warn_only: true,
            ..summary()
        },
        &cards()
    ));
}
