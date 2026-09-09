//! One parsed declaration file, and what it says about a name.
//!
//! Every node kind read here was checked against `tree-sitter-typescript` 0.23.2's
//! `node-types.json`, which is where the fields are *declared*. A hand-written sample cannot
//! stand in for that: a sample with zero `ERROR` nodes still omits whatever the author did not
//! think to write, and AGENTS.md records four wrong claims about this grammar produced exactly
//! that way. The kinds a statement can declare with are read through the resolver's
//! [`BindingResolver::declares`], which owns that walk — this file's own table, kept in
//! parallel with it, drifted once already (#229) — and the resolver is reached through the
//! trait, the way the oracle reaches `declaration_of`, so this crate names no language crate.

use std::fmt;
use std::sync::Arc;

use lanekeep_core::FilePath;
use lanekeep_core::tracked::ContentHash;
use lanekeep_lang::binding::{Binding, BindingResolver, ImportedName};
use tree_sitter::{Node, Tree};

/// A declaration file this run has read and parsed.
pub struct Declaration {
    /// Where it was read from, relative to the project root.
    pub path: FilePath,
    /// The resolver this file's declarations are read with: the one the provider was probed
    /// with, so a declaration file and the file that imported it agree on what declares a
    /// name.
    resolver: Arc<dyn BindingResolver>,
    /// Its text, which every byte range in `tree` indexes.
    pub source: String,
    /// Its parse.
    pub tree: Tree,
    /// What its bytes hashed to when it was read.
    ///
    /// Compared, within a run, against the hash of the bytes the *asking* file's own
    /// `FileAccess` reads: two accesses over one path can see two different files when the
    /// file is rewritten mid-run, which is routine under `--watch`, and serving this parse
    /// against the other access's hash would write a cache entry that describes neither
    /// version. See `BuiltinProvider::declaration`. Load-bearing across requests too: a
    /// provider held for a session (#191) drops an entry whose hash moved.
    pub hash: ContentHash,
    /// Whether its parse carries a fault anywhere — an `ERROR` node, or a `MISSING` token
    /// the parser inserted where one was expected. `Node::has_error` at the root counts
    /// both; an unclosed brace produces the second and no `ERROR` at all.
    ///
    /// `tree_sitter::Parser::parse` answers a tree for any UTF-8 input, so a file this
    /// provider could not really read is indistinguishable from one it read cleanly unless
    /// the question is asked here. The arms still answer whatever the tree does hold — a
    /// declaration outside the damaged span is a real declaration — and it is `complete()`
    /// that turns this into the honest label on a partial answer.
    ///
    /// **The per-declaration granularity is the reached node's own `has_error()`**, asked
    /// by the walk of the declaration it ends at; this whole-file flag is what a nameless
    /// import keeps, and what a name the walk cannot follow to a declaration falls back to.
    pub has_error: bool,
}

impl fmt::Debug for Declaration {
    /// `source` is summarized rather than printed, the same call the oracle's own `Debug`
    /// makes: a whole declaration file in every log line the value appears in is not one
    /// anybody can read, and its length identifies which file this is as well as the bytes do.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Declaration")
            .field("path", &self.path)
            .field("source_len", &self.source.len())
            .field("hash", &self.hash)
            .field("has_error", &self.has_error)
            .finish_non_exhaustive()
    }
}

impl Declaration {
    /// Parse a declaration file that has already been read through a tracked access.
    ///
    /// Takes the parser rather than making one: a provider owns exactly one for the run, and
    /// a constructor that made its own would put a second parser behind an API that reads
    /// like it could not.
    #[must_use]
    pub fn parse(
        path: FilePath,
        source: String,
        parser: &mut tree_sitter::Parser,
        resolver: Arc<dyn BindingResolver>,
    ) -> Option<Self> {
        let hash = ContentHash::new(*blake3::hash(source.as_bytes()).as_bytes());
        let tree = parser.parse(&source, None)?;
        let has_error = tree.root_node().has_error();
        Some(Self {
            path,
            resolver,
            source,
            tree,
            hash,
            has_error,
        })
    }
}

/// Where a file that declares an export sends the name it was asked about.
///
/// Four variants rather than a bare node, because three of the shapes the grammar produces
/// are not nodes in this file at all: a named re-export and a star re-export both name another
/// module, and a namespace re-export names a whole module object that has no declaration to
/// point at.
#[derive(Debug)]
pub enum Exported<'d> {
    /// Declared in this file, at this node.
    Here(Node<'d>),
    /// Re-exported by name: `export { A as B } from './x'`, asked about `B`, yields `A`.
    From {
        /// The module specifier exactly as written.
        specifier: String,
        /// The name to ask that module for.
        name: String,
    },
    /// `export * as ns from './x'` — bound to the whole module object, which has no single
    /// declaration and no type this oracle can build.
    Namespace {
        /// The module specifier exactly as written.
        specifier: String,
    },
    /// Not named here. Each of these `export * from` sources may have it, in source order.
    Star(Vec<String>),
}

/// Where the file's own file-level export named `name` leads.
///
/// Explicit exports first, in source order, and the `export *` fallback only when nothing
/// explicit answered — which is what TypeScript itself does, and what keeps a name a file
/// declares from being answered by a different declaration elsewhere that happens to share
/// its spelling.
#[must_use]
pub fn find_export<'d>(decl: &'d Declaration, name: &str) -> Option<Exported<'d>> {
    let root = decl.tree.root_node();
    let mut cursor = root.walk();
    let mut stars = Vec::new();

    for statement in root.named_children(&mut cursor) {
        if statement.kind() != "export_statement" {
            continue;
        }
        let source = statement
            .child_by_field_name("source")
            .map(|node| unquote(text(decl, node)).to_owned());

        if let Some(specifier) = source {
            if let Some(clause) = named_child_of_kind(statement, "export_clause") {
                if let Some(exported) = clause_target(decl, clause, name, &specifier) {
                    return Some(exported);
                }
                continue;
            }
            if let Some(namespace) = named_child_of_kind(statement, "namespace_export") {
                if namespace
                    .named_child(0)
                    .is_some_and(|n| unquote(text(decl, n)) == name)
                {
                    return Some(Exported::Namespace { specifier });
                }
                continue;
            }
            // `export * from 'm'`: a source, no clause, no namespace. Collected rather than
            // followed, so an explicit export later in the file still wins.
            stars.push(specifier);
            continue;
        }

        // A local re-export, `export { A }` or `export { A as B }`. The local name is either
        // declared in this file or bound by one of its imports — `import { A } from './a';
        // export { A };` is the two-statement barrel, and the chain continues into `./a`
        // under the module's own spelling of the name. The resolver says which, the way it
        // answers a use anywhere else: a walk outward from the specifier's own identifier.
        if let Some(clause) = named_child_of_kind(statement, "export_clause")
            && let Some(local) = local_clause_node(decl, clause, name)
        {
            if let Some(node) = declared_here(decl, unquote(text(decl, local))) {
                return Some(Exported::Here(node));
            }
            if let Some(Binding::Import {
                module,
                name: imported,
            }) = decl.resolver.resolve(&decl.tree, &decl.source, local)
            {
                return Some(match imported {
                    ImportedName::Named(exported) => Exported::From {
                        specifier: module,
                        name: exported,
                    },
                    ImportedName::Default => Exported::From {
                        specifier: module,
                        name: "default".to_owned(),
                    },
                    ImportedName::Namespace => Exported::Namespace { specifier: module },
                });
            }
        }

        let is_default = anonymous_child(statement, "default");
        // `export = X`, whose only marker is the `=` token: no field, an `expression` child.
        // Treated as the default export, which is what an `import D from 'm'` binds under
        // `esModuleInterop` — the spelling every consumer of such a module writes.
        let is_export_assignment = anonymous_child(statement, "=");

        if let Some(declaration) = statement.child_by_field_name("declaration") {
            let wanted = if is_default { "default" } else { name };
            if is_default && name == "default" {
                return Some(Exported::Here(unwrap_ambient(declaration)));
            }
            if !is_default && let Some(node) = declares(decl, declaration, wanted) {
                return Some(Exported::Here(node));
            }
            continue;
        }

        if (is_default || is_export_assignment) && name == "default" {
            let value = statement
                .child_by_field_name("value")
                .or_else(|| statement.named_children(&mut statement.walk()).next())?;
            // `export default Big` names a declaration; `export default 1` is the value
            // itself, which the oracle types directly.
            if value.kind() == "identifier"
                && let Some(node) = declared_here(decl, text(decl, value))
            {
                return Some(Exported::Here(node));
            }
            return Some(Exported::Here(value));
        }
    }

    (!stars.is_empty()).then_some(Exported::Star(stars))
}

/// The declaration of `name` at this file's top level, exported or not.
///
/// Both spellings, because a chain ends at whichever one the declaring file used:
/// `export declare class Big {}` and `declare class Big {}` + `export default Big` name the
/// same thing, and a walk that reached only through `export_statement` would find the first
/// and lose the second.
#[must_use]
pub fn declared_here<'d>(decl: &'d Declaration, name: &str) -> Option<Node<'d>> {
    declared_in(decl.resolver.as_ref(), &decl.tree, &decl.source, name)
}

/// The declaration of `name` at a parsed file's top level, exported or not.
///
/// [`declared_here`] is this over a [`Declaration`]; this is the same walk over a tree the
/// provider does not own — the file under check, which the engine already parsed and which
/// must not be parsed a second time — so the resolver comes in from the caller, which holds
/// the provider's.
#[must_use]
pub(crate) fn declared_in<'t>(
    resolver: &dyn BindingResolver,
    tree: &'t Tree,
    source: &'t str,
    name: &str,
) -> Option<Node<'t>> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let candidate = if statement.kind() == "export_statement" {
            match statement.child_by_field_name("declaration") {
                Some(declaration) => declaration,
                // A re-export carries no declaration of its own; the name may still be
                // declared further down, so skip rather than give up on the file.
                None => continue,
            }
        } else {
            statement
        };
        if let Some(found) = resolver.declares(source, candidate, name) {
            return Some(found);
        }
    }
    None
}

/// The name a declaration node declares, when it declares one.
///
/// `None` for an anonymous default export — `export default 1`, `export default () => {}` —
/// which is a real shape rather than a gap: it has no name, so a `symbolOf` following a chain
/// to it has nothing better to report than `default`.
///
/// `None` for a destructured declarator too: `export const { e } = { e: 2 }` binds a
/// *pattern*, whose text is `{ e }` — and reporting a shape where a spelling belongs would
/// put `exported: "{ e }"` on a `Symbol`, which a rule comparing against a required export
/// name would read as a mismatch on conforming code. `walk_export` falls back to the name
/// the chain was asked for, which is what a shorthand destructuring exports.
///
/// A dotted namespace's name is its first segment: `namespace A.B {}` declares `A`, the
/// same name the resolver binds for it, and `A.B` is a spelling no `import` can write.
#[must_use]
pub fn declared_name(decl: &Declaration, node: Node<'_>) -> Option<String> {
    let node = unwrap_ambient(node);
    node.child_by_field_name("name")
        .filter(|name| {
            matches!(
                name.kind(),
                "identifier" | "type_identifier" | "nested_identifier" | "string"
            )
        })
        .map(|name| unquote(text(decl, first_segment(name))).to_owned())
}

/// The node whose text is the declared name: the leftmost segment of a `nested_identifier`.
///
/// `namespace A.B {}` is shorthand for `namespace A { namespace B {} }` — the enclosing
/// scope sees `A`, and `lanekeep-lang-js` binds it so. The grammar aliases every inner level
/// of a dotted name to `member_expression`, so `google.maps.places` is a `nested_identifier`
/// over the member expression `google.maps`; both kinds carry an `object` field. Anything
/// that is not nested is its own name.
fn first_segment(name: Node<'_>) -> Node<'_> {
    let mut node = name;
    while matches!(node.kind(), "nested_identifier" | "member_expression") {
        match node.child_by_field_name("object") {
            Some(object) => node = object,
            None => break,
        }
    }
    node
}

/// Whether `declaration` declares `name`, and where.
///
/// The resolver's walk — one table for both crates, where a second once drifted (#229).
/// Imports are filtered out on the resolver's side, so an `import_statement` never answers
/// as a declaration here.
fn declares<'d>(decl: &'d Declaration, declaration: Node<'d>, name: &str) -> Option<Node<'d>> {
    decl.resolver.declares(&decl.source, declaration, name)
}

/// Step through `ambient_declaration`, which wraps the declaration `declare` applies to.
///
/// The first named child that is not a `comment`: a comment is a named extra in this
/// grammar, so `declare /** doc */ class C {}` puts one ahead of the class, and a `.d.ts`
/// writes that shape all the time.
fn unwrap_ambient(node: Node<'_>) -> Node<'_> {
    if node.kind() != "ambient_declaration" {
        return node;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() != "comment")
        .unwrap_or(node)
}

/// The re-export target for `name` inside `export { … } from 'm'`.
fn clause_target<'d>(
    decl: &'d Declaration,
    clause: Node<'d>,
    name: &str,
    specifier: &str,
) -> Option<Exported<'d>> {
    let mut cursor = clause.walk();
    for specifier_node in clause.named_children(&mut cursor) {
        if specifier_node.kind() != "export_specifier" {
            continue;
        }
        let exported = specifier_node.child_by_field_name("name")?;
        let visible = specifier_node
            .child_by_field_name("alias")
            .unwrap_or(exported);
        if unquote(text(decl, visible)) == name {
            return Some(Exported::From {
                specifier: specifier.to_owned(),
                name: unquote(text(decl, exported)).to_owned(),
            });
        }
    }
    None
}

/// The `name` node of the `export_specifier` in a local `export { … }` that exports `name`.
///
/// The node rather than its text, because the resolver resolves a *node* by walking outward
/// from it — which is what lets [`find_export`] ask whether the local name is an import
/// binding rather than a declaration.
fn local_clause_node<'d>(decl: &Declaration, clause: Node<'d>, name: &str) -> Option<Node<'d>> {
    let mut cursor = clause.walk();
    for specifier in clause.named_children(&mut cursor) {
        if specifier.kind() != "export_specifier" {
            continue;
        }
        let local = specifier.child_by_field_name("name")?;
        let visible = specifier.child_by_field_name("alias").unwrap_or(local);
        if unquote(text(decl, visible)) == name {
            return Some(local);
        }
    }
    None
}

/// The file and name a chain of re-exports ends at.
///
/// `name` is the name the *declaring* file uses, which is what `symbolOf` reports as
/// `exported` — so `export { Decimal as Big }` asked about `Big` answers `Decimal`, and a
/// rule comparing against a required export name compares against the real one rather than
/// against whatever spelling the last hop chose.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExportTarget {
    /// The declaring file, relative to the project root.
    pub file: FilePath,
    /// The name it declares the export under.
    pub name: String,
}

/// The node an already-resolved [`ExportTarget`] names, in its own file.
///
/// `declared_here` first, because a chain ends at whichever spelling the declaring file
/// used and `declare class Big {}` is not an `export_statement` at all. `find_export` is
/// the fallback for the one shape that has no name to look up: an anonymous default.
///
/// A node the parser only partly read — `has_error()`, which counts a `MISSING` token as
/// well as an `ERROR` — answers `None` rather than the node: a damaged declaration has no
/// shape worth typing. `walk_export` makes the same refusal for the chain it walks; this is
/// the refusal for the callers that look a name up here directly, so the two cannot
/// disagree about one node.
#[must_use]
pub(crate) fn target_node<'d>(decl: &'d Declaration, name: &str) -> Option<Node<'d>> {
    declared_here(decl, name)
        .or_else(|| match find_export(decl, name) {
            Some(Exported::Here(node)) => Some(node),
            _ => None,
        })
        .filter(|node| !node.has_error())
}

/// The first named child of `kind`, when there is one.
fn named_child_of_kind<'d>(node: Node<'d>, kind: &str) -> Option<Node<'d>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

/// Whether an anonymous token of this text is a direct child.
///
/// `default` and `=` are the only markers separating three otherwise identical shapes of
/// `export_statement`, and neither is a field — the grammar writes them as bare tokens
/// (`common/define-grammar.js:329`, `tree-sitter-javascript`'s `grammar.js:185`).
fn anonymous_child(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| !child.is_named() && child.kind() == token)
}

/// The source text of a node.
fn text<'d>(decl: &'d Declaration, node: Node<'_>) -> &'d str {
    text_of(&decl.source, node)
}

/// The source text of a node, read from a source string directly rather than a
/// [`Declaration`] — what [`imports_with_names`] needs over a tree it does not own.
fn text_of<'t>(source: &'t str, node: Node<'_>) -> &'t str {
    source.get(node.byte_range()).unwrap_or("")
}

/// Drop the quotes a string module name or a string export name carries.
///
/// `export_specifier`'s `name` and `alias` may be a `string` as well as an `identifier`
/// (`node-types.json`), and so may a module's `source`, so every read of one goes through
/// this rather than through four places that each remember to.
fn unquote(text: &str) -> &str {
    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'"' | b'\''), Some(b'"' | b'\'')) if text.len() >= 2 => &text[1..text.len() - 1],
        _ => text,
    }
}

/// One import statement's specifier, with the names it binds.
///
/// `complete`'s per-name walk needs more than the bare specifier the old whole-file walk
/// returned: an `ERROR` verdict is now decided per *reached* declaration, and which
/// declarations an import reaches is exactly its name list. A nameless import —
/// side-effect, `import * as ns`, `export *` — binds no single declaration to reach (a
/// namespace binds the whole module object, which the caller judges the same way) and
/// keeps the whole-file verdict; see the caller.
#[derive(Debug)]
pub(crate) struct ImportedSpecifier {
    /// The module specifier exactly as written, quotes stripped.
    pub specifier: String,
    /// The names the statement binds, in source order — empty for a nameless one.
    pub names: Vec<ImportedName>,
}

/// Every module specifier this file imports from, in source order, with the names each
/// statement binds.
///
/// `import_statement`'s `source` field, plus `export_statement`'s: a barrel file re-exporting
/// what it never imports is exactly as dependent on those modules, and a completeness answer
/// that ignored them would call such a file complete while knowing nothing about it.
///
/// The name lists are read off the grammar the same way `JsBindingResolver`'s
/// `import_binding` reads them — the two were dumped side by side against
/// `tree-sitter-typescript` 0.23.2 rather than trusted from a sample, which is where the
/// default/namespace/named split below comes from.
///
/// `import x = require('m')` needs its own fallback: `node-types.json` marks
/// `import_statement`'s own `source` field `required: false` and puts the field this shape
/// actually carries on its `import_require_clause` child instead — so a bare
/// `child_by_field_name("source")` on the statement itself answers nothing for exactly this
/// one shape, silently dropping it from the count. It binds one name by assignment rather
/// than by an `import_clause`, so it contributes an empty name list.
#[must_use]
pub(crate) fn imports_with_names(tree: &Tree, source: &str) -> Vec<ImportedSpecifier> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|statement| matches!(statement.kind(), "import_statement" | "export_statement"))
        .filter_map(|statement| {
            let specifier = statement.child_by_field_name("source").or_else(|| {
                named_child_of_kind(statement, "import_require_clause")
                    .and_then(|clause| clause.child_by_field_name("source"))
            })?;
            Some(ImportedSpecifier {
                specifier: unquote(text_of(source, specifier)).to_owned(),
                names: bound_names(statement, source),
            })
        })
        .collect()
}

/// The names one `import_statement` or `export_statement` binds from its `source` module.
///
/// A statement without a source module — a local `export { A }` — binds nothing from
/// anywhere, and never reaches this function.
fn bound_names(statement: Node<'_>, source: &str) -> Vec<ImportedName> {
    match statement.kind() {
        "import_statement" => {
            let Some(clause) = named_child_of_kind(statement, "import_clause") else {
                return Vec::new();
            };
            let mut names = Vec::new();
            let mut cursor = clause.walk();
            for child in clause.children(&mut cursor) {
                match child.kind() {
                    // `import d from 'm'` — and the `d` of `import d, * as ns from 'm'`.
                    "identifier" => names.push(ImportedName::Default),
                    // `import * as ns from 'm'`
                    "namespace_import" => names.push(ImportedName::Namespace),
                    // `import { a, b as c } from 'm'`
                    "named_imports" => {
                        let mut inner = child.walk();
                        for specifier in child
                            .children(&mut inner)
                            .filter(|s| s.kind() == "import_specifier")
                        {
                            // The module's own spelling, not the local alias: the walk that
                            // follows asks the declaring module what it exports.
                            if let Some(exported) = specifier.child_by_field_name("name") {
                                names.push(ImportedName::Named(
                                    unquote(text_of(source, exported)).to_owned(),
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
            names
        }
        "export_statement" => {
            // `export { A as B } from 'm'` — `A` is the name in the *other* module, which is
            // the one a walk from here asks it for.
            if let Some(clause) = named_child_of_kind(statement, "export_clause") {
                let mut names = Vec::new();
                let mut cursor = clause.walk();
                for specifier in clause
                    .named_children(&mut cursor)
                    .filter(|s| s.kind() == "export_specifier")
                {
                    if let Some(exported) = specifier.child_by_field_name("name") {
                        names.push(ImportedName::Named(
                            unquote(text_of(source, exported)).to_owned(),
                        ));
                    }
                }
                return names;
            }
            // `export * as ns from 'm'`
            if named_child_of_kind(statement, "namespace_export").is_some() {
                return vec![ImportedName::Namespace];
            }
            // Bare `export * from 'm'`: nothing is bound, so nothing is reached.
            Vec::new()
        }
        _ => Vec::new(),
    }
}
