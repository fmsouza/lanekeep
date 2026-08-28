//! A guest that exercises the cross-file phase, and tries to escape it.
//!
//! **It is a probe, not a rule and not an assertion**, on the terms
//! `tests/fixtures/facts/src/lib.rs` sets out: every observation is encoded into the message of
//! a `report`, and the host asserts on the recorded reports. A guest that asserted for itself
//! could only fail by trapping, and a trap says nothing about which observation was wrong.
//!
//! # The probe channel is the file list, because `reduce` has no other argument
//!
//! `check` is handed a `match`, so the check fixtures take their probe name from the first
//! capture and their arguments from the rest. `reduce` is handed nothing but the context, so
//! the only list of host-chosen strings it can read is `files()`. The first entry is the probe
//! and every entry after it is an argument.
//!
//! That is the same borrowing the check fixtures already do — "the host is passing a list of
//! strings and the order it passed them in is the whole content" — and it costs the `files`
//! probe nothing: it reports the list back **verbatim, probe name included**, so the host can
//! assert the list it supplied is the list that crossed, in order and unedited.
//!
//! # Forging a handle, from both directions
//!
//! Both contexts live in one WIT interface, so the generated bindings contain both types and a
//! `pub unsafe fn from_handle(handle: u32)` for each. A rule can therefore *name* the other
//! phase's context. What it cannot do is obtain a valid handle to one, and the two `forge-*`
//! probes are what shows it: each takes the handle it is holding for its own phase — zero — and
//! reads it as the other phase's, which is the most plausible forgery there is.
//!
//! Each announces itself first and claims success afterwards. Neither claim should ever be
//! recorded: the announcement proves the guest reached the forgery, and the absence of the
//! claim, together with the absence of any trace on the other phase's host state, proves the
//! call died in the runtime before the host was reached.
//!
//! **The announcement is load-bearing rather than decorative, proven by removing it.** With it
//! deleted, the trap assertions still pass — the message still matches and the other phase's
//! reports are still empty — so a fixture that died *before* reaching the forgery would be
//! indistinguishable from one that forged and trapped. The announcement is what tells the two
//! apart, and it is why this fixture is evidence rather than a hopeful arrangement.
//!
//! The `unsafe` is the point rather than an accident. This crate is its own workspace and does
//! not inherit the engine's `unsafe_code = "deny"`, which is what lets a fixture do the thing a
//! rule author must not.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::{
    QueryFor,
    ReduceLocation, RuleCard, RuleError, RuleExamples, RuleGates, RuleMetadata,
};
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

/// The file every observation is reported against.
///
/// A reduce report needs a site or the host refuses it, and these reports are about the run
/// rather than about any file in it — so the site is a name no corpus contains. The host reads
/// the message and ignores this.
const SAY: &str = "<probe>";

impl Guest for Component {
    /// The one rule this component hosts. Every other export takes its index.
    fn rules() -> Vec<String> {
        vec!["fixture/reduce".to_owned()]
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `metadata` itself is tested.
    fn metadata(rule: u32) -> RuleMetadata {
        only(rule);
        RuleMetadata {
            id: "fixture/reduce".to_owned(),
            languages: vec!["rust".to_owned()],
            severity: "error".to_owned(),
            card: RuleCard {
                message: String::new(),
                remediation: String::new(),
                examples: RuleExamples {
                    bad: String::new(),
                    good: String::new(),
                },
            },

            queries: vec![QueryFor { language: "rust".to_owned(), query: String::new() }],
            gates: RuleGates {
                path_matches: Vec::new(),
                path_not_matches: Vec::new(),
                file_contains: Vec::new(),
                file_not_contains: Vec::new(),
            },
            timeout: None,
        }
    }

    /// Not exercised by any test — every export is mandatory because a WIT world has no
    /// optional ones. `tests/fixtures/metadata/` is where `configure` itself is tested.
    ///
    /// Refuses unconditionally rather than accepting anything, so a caller that reached this
    /// export on this fixture fails loudly instead of passing on a vacuous success.
    fn configure(rule: u32, _options_json: String) -> Result<(), String> {
        only(rule);
        Err("fixture/reduce does not implement configure".to_owned())
    }

    fn has_check(rule: u32) -> bool {
        only(rule);
        true
    }

    /// True, unlike every other fixture here: this one has a cross-file pass and it is the
    /// whole subject.
    fn has_reduce(rule: u32) -> bool {
        only(rule);
        true
    }

    fn check(rule: u32, ctx: &CheckContext, m: Match) -> Result<(), RuleError> {
        only(rule);
        match m.first().map_or("", |entry| entry.name.as_str()) {
            // A real per-file call that does nothing but succeed. It exists so a host can put
            // one *before* a forgery from the other phase — the adversarial ordering, where the
            // guest's `check-context` table has held a live handle at the index the forgery
            // guesses.
            "warm" => report(ctx, "warm"),
            "forge-reduce" => forge_reduce(ctx),
            other => report(ctx, &format!("unknown probe `{other}`")),
        }
        Ok(())
    }

    fn reduce(rule: u32, ctx: &ReduceContext) -> Result<(), RuleError> {
        only(rule);
        let files = ctx.files();
        let args: Vec<&str> = files.iter().skip(1).map(String::as_str).collect();

        match files.first().map_or("", String::as_str) {
            // Verbatim, and joined rather than reported one per line: what is under test is the
            // list, and one message carrying all of it fails on an edit anywhere in it.
            "files" => say(ctx, &format!("files={}", files.join(","))),
            "fact-fields" => fact_fields(ctx),
            "counts" => counts(ctx, &args),
            "report" => report_at(ctx, &args),
            "report-no-line" => positionless(ctx, &args, None, Some(2)),
            "report-no-column" => positionless(ctx, &args, Some(4), None),
            "report-no-position" => positionless(ctx, &args, None, None),
            "forge-check" => forge_check(ctx),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
        Ok(())
    }
}

/// Report a message at the root, for observations the per-file phase makes.
fn report(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// Report a message at [`SAY`], for observations the cross-file phase makes.
fn say(ctx: &ReduceContext, message: &str) {
    ctx.report(
        &ReduceLocation {
            file: SAY.to_owned(),
            line: Some(1),
            column: Some(1),
        },
        Some(message),
    );
}

/// Every fact, field by field.
///
/// `kind`, `file` and `data` separately rather than as one rendering, because they arrive as
/// three record fields and a host that filled the wrong one would still produce a plausible
/// line. `file` in particular is the field that does *not* exist under QuickJS, where the path
/// is spliced into the payload instead — so a host that merged rather than filled would show up
/// here as a `data` carrying a `"file"` key the guest never sent.
fn fact_fields(ctx: &ReduceContext) {
    for fact in ctx.facts(None) {
        say(ctx, &format!("{}|{}|{}", fact.kind, fact.file, fact.data));
    }
}

/// How many facts each named kind selects, and how many there are altogether.
///
/// The `none` count comes last and is asked for separately: "every kind the host named" and
/// "everything" are different questions, and a host that ignored the filter would answer both
/// with the same number.
fn counts(ctx: &ReduceContext, args: &[&str]) {
    for kind in args {
        say(ctx, &format!("{kind}={}", ctx.facts(Some(kind)).len()));
    }
    say(ctx, &format!("all={}", ctx.facts(None).len()));
}

/// One cross-file report, with the position the host supplied.
///
/// Three arguments report without a message and four report with one, so one probe covers both
/// halves of `message: option<string>`.
fn report_at(ctx: &ReduceContext, args: &[&str]) {
    let (file, line, column, message) = match args {
        [file, line, column] => (file, line, column, None),
        [file, line, column, message] => (file, line, column, Some(*message)),
        _ => {
            return say(
                ctx,
                &format!(
                    "shape: expected a file, a line, a column and an optional message, got {}",
                    args.len()
                ),
            );
        }
    };

    let (Ok(line), Ok(column)) = (line.parse::<u32>(), column.parse::<u32>()) else {
        return say(ctx, "shape: the line and the column must be numbers");
    };

    ctx.report(
        &ReduceLocation {
            file: (*file).to_owned(),
            line: Some(line),
            column: Some(column),
        },
        message,
    );
}

/// A report that names a file and leaves out one or both halves of the position.
///
/// The host must refuse it. The claim below is never reported if it does — and the probe says
/// nothing beforehand on purpose, so "no report was recorded" is the assertion the host gets to
/// make, exactly as `reporting_without_a_position_is_rejected` makes it under QuickJS.
fn positionless(ctx: &ReduceContext, args: &[&str], line: Option<u32>, column: Option<u32>) {
    let Some(file) = args.first() else {
        return say(ctx, "shape: expected a file");
    };

    ctx.report(
        &ReduceLocation {
            file: (*file).to_owned(),
            line,
            column,
        },
        Some("a cross-file violation with no site"),
    );
    say(ctx, "the host accepted a report with no position");
}

/// From the per-file phase, forge a `reduce-context` and read the corpus through it.
///
/// The direction that matters most: a per-file rule that could reach `files` or `facts` would
/// make a file's result depend on files other than itself, and caching that result against its
/// own content would be unsound. `report` is what is called rather than `files`, because a
/// `report` that got through leaves a trace on the host the test can find — where a `files`
/// that got through would return a value only this guest could see.
fn forge_reduce(ctx: &CheckContext) {
    report(ctx, "forging a reduce-context handle");

    // Zero: the handle this call is holding for its own phase, read as the other phase's. The
    // component instance keeps a table per resource type, and this one's `reduce-context` table
    // is empty for the length of a `check`.
    let forged = unsafe { ReduceContext::from_handle(0) };
    forged.report(
        &ReduceLocation {
            file: "forged.ts".to_owned(),
            line: Some(1),
            column: Some(1),
        },
        Some("a per-file rule reached the corpus"),
    );

    report(ctx, "the forged reduce-context handle was accepted");
}

/// From the cross-file phase, forge a `check-context` and emit a fact through it.
///
/// The mirror image, and the other half of the split: a reduce phase that could emit facts
/// could feed itself, and there is no second pass for the result to reach. `emit-fact` is what
/// is called for the reason `report` is called above — it is the reduce-side method that leaves
/// a trace on the host if it lands.
fn forge_check(ctx: &ReduceContext) {
    say(ctx, "forging a check-context handle");

    let forged = unsafe { CheckContext::from_handle(0) };
    let _ = forged.emit_fact("forged", "{}");

    say(ctx, "the forged check-context handle was accepted");
}

/// The one rule this component hosts.
///
/// A component hosts a *list* of rules and every export but `rules` takes an index into it.
/// This one hosts a single rule, so zero is the only index that answers — and a host asking
/// for another has disagreed with what `rules` reported, which is worth trapping on rather
/// than answering with the one rule's data under another rule's name.
fn only(rule: u32) {
    assert_eq!(rule, 0, "this component hosts one rule");
}

bindings::export!(Component with_types_in bindings);
