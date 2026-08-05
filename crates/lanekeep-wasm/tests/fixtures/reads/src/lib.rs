//! A guest that calls the two tracked-read methods and reports exactly what came back.
//!
//! **It is a probe, not a rule and not an assertion**, on the terms
//! `tests/fixtures/queries/src/lib.rs` sets out: every observation is encoded into the message
//! of a `report`, and the host asserts on the recorded reports. A guest that asserted for
//! itself could only fail by trapping, and a trap says nothing about which observation was
//! wrong.
//!
//! The probe to run is the name of the **first** capture in the `match` handed to `check`, and
//! every capture after it is an argument — a path to try. Paths arrive from the host rather
//! than being written here for one reason that is not convenience: `Path::is_absolute` is
//! platform-specific, so the absolute path this exercises has to be built from
//! `std::env::temp_dir()` on the machine running the test. A literal would take a different
//! branch on Windows than on Unix and the test would assert nothing on one of them.
//!
//! # A refusal is reported, never unwrapped
//!
//! Both methods return a `result` whose error is the world's `read-error`, and the whole reason
//! that channel exists is that a rule can *handle* a read it was not allowed to make. A probe
//! that unwrapped would turn every refusal into a trap and prove the opposite of the point, so
//! every call below is matched and rendered — including the successes, where an unexpected
//! error is reported rather than swallowed.
//!
//! `sweep` keeps going after a refusal and reports the paths that follow it, which is the part
//! that shows a refusal is a value: a rule that was told no about one file is still running.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::ReadError;
use bindings::{CheckContext, Guest, Match, ReduceContext};

struct Component;

impl Guest for Component {
    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        false
    }

    fn check(ctx: &CheckContext, m: Match) {
        // Every entry after the first is an argument. By position and not by name, because
        // these are not query captures: the host is passing a list of paths and the order it
        // passed them in is the whole content.
        let args: Vec<&str> = m.iter().skip(1).map(|entry| entry.name.as_str()).collect();

        match m.first().map_or("", |entry| entry.name.as_str()) {
            "read" => read(ctx, &args),
            "exists" => exists(ctx, &args),
            "sweep" => sweep(ctx, &args),
            other => say(ctx, &format!("unknown probe `{other}`")),
        }
    }

    /// A check-only rule still exports `reduce`, because a WIT world has no optional exports.
    fn reduce(_ctx: &ReduceContext) {}
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// What `read-file` answered, in a form the host can compare against `FileAccess` directly.
///
/// The three outcomes are rendered as three different shapes on purpose. `none` and
/// `text()` must never be confusable — "the file is not there" and "the file held nothing"
/// are different answers, and an empty file is a real thing a project can contain.
fn read_outcome(outcome: Result<Option<String>, ReadError>) -> String {
    match outcome {
        Ok(Some(text)) => format!("text({text})"),
        Ok(None) => "none".to_owned(),
        Err(problem) => refusal(&problem),
    }
}

/// What `file-exists` answered.
fn exists_outcome(outcome: Result<bool, ReadError>) -> String {
    match outcome {
        Ok(true) => "yes".to_owned(),
        Ok(false) => "no".to_owned(),
        Err(problem) => refusal(&problem),
    }
}

/// Which `read-error` case came back, and the path it carried.
///
/// The case *and* the payload, because they fail independently: a host that mapped every
/// refusal onto one case would still carry the right path, and one that carried the wrong
/// path would still name the right case.
fn refusal(problem: &ReadError) -> String {
    match problem {
        ReadError::EscapesRoot(path) => format!("escapes-root({path})"),
        ReadError::Absolute(path) => format!("absolute({path})"),
        ReadError::NotText(path) => format!("not-text({path})"),
    }
}

/// One `read-file`, reported.
///
/// Called twice by the host over one `check-context`, with the file rewritten in between: the
/// answer has to be the same both times, because a rule that saw a file change under it could
/// report differently on two runs over identical input.
fn read(ctx: &CheckContext, args: &[&str]) {
    let Some(path) = args.first() else {
        return say(ctx, "shape: no path argument");
    };
    say(
        ctx,
        &format!("read={}", read_outcome(ctx.read_file(path))),
    );
}

/// One `file-exists`, reported.
fn exists(ctx: &CheckContext, args: &[&str]) {
    let Some(path) = args.first() else {
        return say(ctx, "shape: no path argument");
    };
    say(
        ctx,
        &format!("exists={}", exists_outcome(ctx.file_exists(path))),
    );
}

/// Both methods over every path the host passed, one report each.
///
/// The loop does not stop at the first refusal, which is the part that shows a refusal is a
/// value rather than the end of the invocation: the paths after a rejected one are still
/// tried, and the host asserts it saw a line for every one of them.
fn sweep(ctx: &CheckContext, args: &[&str]) {
    if args.is_empty() {
        return say(ctx, "shape: no paths to sweep");
    }
    for path in args {
        say(
            ctx,
            &format!(
                "{path} read={} exists={}",
                read_outcome(ctx.read_file(path)),
                exists_outcome(ctx.file_exists(path))
            ),
        );
    }
}

bindings::export!(Component with_types_in bindings);
