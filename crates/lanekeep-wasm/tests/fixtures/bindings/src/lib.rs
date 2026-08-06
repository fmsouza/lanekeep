//! A guest that asks what an identifier refers to and reports the answer.
//!
//! **It is a probe, not a rule and not an assertion**, on the same terms as
//! `tests/fixtures/navigation/`: every observation is encoded into the message of a `report`
//! and the host asserts on the recorded reports. A guest that asserted for itself could only
//! fail by trapping, and a trap says nothing about which of four answers was wrong.
//!
//! The probe to run is named by the first capture of the `match` handed to `check`; the
//! identifier under test arrives as a capture named `target`, interned by the host exactly as
//! a query capture would be. Nothing here searches the tree for an identifier by text — that
//! is the host's job in a real run, and doing it here would put the part under test on both
//! sides of the assertion.
//!
//! # What a rule can tell apart, and what it cannot
//!
//! `binding-kind` returns `option<binding-kind>`, so "the name does not resolve" and "it
//! resolves to a `const`" are different answers. The other three return plain `bool`, where
//! "no resolver for this language" and "it is not that import" are the same answer — which is
//! deliberate and is what `crates/lanekeep-js/src/host.rs` already does. The `all` probe below
//! is run twice by the host, over one source, with and without a resolver attached, so the
//! difference between the two is visible in the recorded messages rather than argued about.
//!
//! # Nothing here may index a slice
//!
//! A panic in a guest is a trap, and a trap aborts the call before any report crosses back, so
//! a wrong shape assumption would be reported as "the host recorded no violations" — which is
//! also what a broken `report` looks like. Every lookup is a `get` or an iterator, with an
//! explicit `shape` report when the match is not what this file expects.

#[allow(warnings)]
mod bindings;

use bindings::lanekeep::host::types::BindingKind;
use bindings::{CheckContext, Guest, Match, ReduceContext};

/// A handle no arena in these tests will have issued.
///
/// Rule code is arbitrary and may pass any number at all; what it must not do is take the
/// engine down with it. Binding resolution's answer for one is the same as its answer for a
/// name nothing declares — `false` and `none` — because not finding a binding is an ordinary
/// outcome rather than an error.
const UNRESOLVABLE: u32 = 9999;

struct Component;

impl Guest for Component {
    fn has_check() -> bool {
        true
    }

    fn has_reduce() -> bool {
        false
    }

    fn check(ctx: &CheckContext, m: Match) {
        let probe = m.first().map_or("", |entry| entry.name.as_str());

        // The handle the probe asks about. Absent only for `unresolvable`, which supplies its
        // own; every other probe needs one and says so rather than quietly doing nothing.
        let target = m
            .iter()
            .find(|entry| entry.name == "target")
            .map(|entry| entry.node);

        match (probe, target) {
            ("alias", Some(node)) => alias(ctx, node),
            ("shadow", Some(node)) => shadow(ctx, node),
            ("any-export", Some(node)) => any_export(ctx, node),
            ("glob", Some(node)) => glob(ctx, node),
            ("kind", Some(node)) => kind(ctx, node),
            ("all", Some(node)) => all(ctx, node),
            ("unresolvable", _) => all(ctx, UNRESOLVABLE),
            (other, None) => say(ctx, &format!("shape: probe `{other}` has no target capture")),
            (other, Some(_)) => say(ctx, &format!("unknown probe `{other}`")),
        }
    }

    /// A check-only rule still exports `reduce`, because a WIT world has no optional exports.
    fn reduce(_ctx: &ReduceContext) {}
}

/// Report a message at the root, for observations that are not about a particular node.
fn say(ctx: &CheckContext, message: &str) {
    ctx.report(ctx.root(), Some(message), None);
}

/// How a rule spells a binding kind.
///
/// The same seventeen strings `packages/lanekeep/index.d.ts`'s `BindingKind` union lists and
/// `lanekeep_lang::binding::Binding::kind_str` returns, so a message below is comparable by
/// inspection with what the identical TypeScript rule reads. Written out rather than derived:
/// `Debug` would render `BindingKind::CatchParam`, which is a Rust spelling of a value whose
/// published spelling is `catch-param`.
///
/// Exhaustive on purpose. A case added to the world's enum stops this fixture from building,
/// which is the cheapest place for that to be noticed.
fn kind_str(kind: BindingKind) -> &'static str {
    match kind {
        BindingKind::Import => "import",
        BindingKind::Const => "const",
        BindingKind::Let => "let",
        BindingKind::Var => "var",
        BindingKind::Param => "param",
        BindingKind::Function => "function",
        BindingKind::Class => "class",
        BindingKind::CatchParam => "catch-param",
        BindingKind::Assignment => "assignment",
        BindingKind::Loop => "loop",
        BindingKind::ContextManager => "context-manager",
        BindingKind::Comprehension => "comprehension",
        BindingKind::Type => "type",
        BindingKind::Receiver => "receiver",
        BindingKind::TypeParam => "type-param",
        BindingKind::Module => "module",
        BindingKind::Trait => "trait",
    }
}

/// The kind, or the word a rule sees when nothing declares the name.
fn rendered_kind(ctx: &CheckContext, node: u32) -> &'static str {
    ctx.binding_kind(node).map_or("none", kind_str)
}

/// An import reached through an alias, which is the case §6.4 exists for.
///
/// Three questions rather than one: the right module and export, then each half wrong on its
/// own. A host that ignored its arguments and answered `true` would pass the first and fail
/// the other two.
fn alias(ctx: &CheckContext, node: u32) {
    ctx.report(
        node,
        Some(&format!(
            "exact={} wrong-module={} wrong-name={}",
            ctx.resolves_to_import(node, "@rneui/themed", Some("makeStyles")),
            ctx.resolves_to_import(node, "somewhere-else", Some("makeStyles")),
            ctx.resolves_to_import(node, "@rneui/themed", Some("notThatOne")),
        )),
        None,
    );
}

/// A local declaration that hides an import of the same name.
///
/// The false positive binding resolution exists to prevent: a rule keyed on the text
/// `makeStyles` firing on a local that has nothing to do with the import.
fn shadow(ctx: &CheckContext, node: u32) {
    ctx.report(
        node,
        Some(&format!(
            "resolves-to-import={} kind={} shadowed={}",
            ctx.resolves_to_import(node, "@rneui/themed", Some("makeStyles")),
            rendered_kind(ctx, node),
            ctx.is_shadowed(node),
        )),
        None,
    );
}

/// Omitting the export name, against naming it right and naming it wrong.
///
/// `default` is asked for as well, because it is a name a rule can write for an export this
/// import does not have: a host that dropped the `option` and matched any name would answer
/// `true` to it.
fn any_export(ctx: &CheckContext, node: u32) {
    ctx.report(
        node,
        Some(&format!(
            "omitted={} named={} wrong-name={} default={}",
            ctx.resolves_to_import(node, "m", None),
            ctx.resolves_to_import(node, "m", Some("a")),
            ctx.resolves_to_import(node, "m", Some("b")),
            ctx.resolves_to_import(node, "m", Some("default")),
        )),
        None,
    );
}

/// Matching the module by pattern rather than exactly.
fn glob(ctx: &CheckContext, node: u32) {
    ctx.report(
        node,
        Some(&format!(
            "scope-star={} star-pkg={} exact={} other-scope={}",
            ctx.is_imported_from(node, "@scope/*"),
            ctx.is_imported_from(node, "*/pkg"),
            ctx.is_imported_from(node, "@scope/pkg"),
            ctx.is_imported_from(node, "@other/*"),
        )),
        None,
    );
}

/// How the name was introduced, on its own.
fn kind(ctx: &CheckContext, node: u32) {
    ctx.report(node, Some(rendered_kind(ctx, node)), None);
}

/// All four questions about one handle.
///
/// Run by the host over one source three ways — with a resolver, without one, and against a
/// handle no arena issued — so that "nothing resolves" is visible as an answer rather than as
/// a missing call. There is no fourth shape where a rule reaches a function that is not there:
/// a component imports what the world declares, and the world declares all four.
fn all(ctx: &CheckContext, node: u32) {
    ctx.report(
        ctx.root(),
        Some(&format!(
            "resolves-to-import={} imported-from={} shadowed={} kind={}",
            ctx.resolves_to_import(node, "m", Some("a")),
            ctx.is_imported_from(node, "*"),
            ctx.is_shadowed(node),
            rendered_kind(ctx, node),
        )),
        None,
    );
}

bindings::export!(Component with_types_in bindings);
