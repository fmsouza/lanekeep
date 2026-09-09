//! One parsed declaration file, and what it says about a name.
//!
//! Every node kind read here was checked against `tree-sitter-typescript` 0.23.2's
//! `node-types.json`, which is where the fields are *declared*. A hand-written sample cannot
//! stand in for that: a sample with zero `ERROR` nodes still omits whatever the author did not
//! think to write, and AGENTS.md records four wrong claims about this grammar produced exactly
//! that way.

use std::fmt;

use lanekeep_core::FilePath;
use lanekeep_core::tracked::ContentHash;
use tree_sitter::{Node, Tree};

/// A declaration file this run has read and parsed.
pub struct Declaration {
    /// Where it was read from, relative to the project root.
    pub path: FilePath,
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
    /// Whether its parse carries an `ERROR` node anywhere.
    ///
    /// `tree_sitter::Parser::parse` answers a tree for any UTF-8 input, so a file this
    /// provider could not really read is indistinguishable from one it read cleanly unless
    /// the question is asked here. The arms still answer whatever the tree does hold — a
    /// declaration outside the `ERROR` span is a real declaration — and it is `complete()`
    /// that turns this into the honest label on a partial answer.
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
    pub fn parse(path: FilePath, source: String, parser: &mut tree_sitter::Parser) -> Option<Self> {
        let hash = ContentHash::new(*blake3::hash(source.as_bytes()).as_bytes());
        let tree = parser.parse(&source, None)?;
        let has_error = tree.root_node().has_error();
        Some(Self {
            path,
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

        // A local re-export, `export { A }` or `export { A as B }`.
        if let Some(clause) = named_child_of_kind(statement, "export_clause")
            && let Some(local) = local_clause_name(decl, clause, name)
            && let Some(node) = declared_here(decl, &local)
        {
            return Some(Exported::Here(node));
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
    declared_in(&decl.tree, &decl.source, name)
}

/// The declaration of `name` at a parsed file's top level, exported or not.
///
/// [`declared_here`] is this over a [`Declaration`]; this is the same walk over a tree the
/// provider does not own — the file under check, which the engine already parsed and which
/// must not be parsed a second time.
#[must_use]
pub(crate) fn declared_in<'t>(tree: &'t Tree, source: &'t str, name: &str) -> Option<Node<'t>> {
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
        if let Some(found) = declares_in(source, candidate, name) {
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
#[must_use]
pub fn declared_name(decl: &Declaration, node: Node<'_>) -> Option<String> {
    let node = unwrap_ambient(node);
    node.child_by_field_name("name")
        .map(|name| unquote(text(decl, name)).to_owned())
}

/// Whether `declaration` declares `name`, and where.
fn declares<'d>(decl: &'d Declaration, declaration: Node<'d>, name: &str) -> Option<Node<'d>> {
    declares_in(&decl.source, declaration, name)
}

/// Whether `declaration` declares `name`, and where — over a source string rather than a
/// [`Declaration`], so [`declared_here`] and [`declared_in`] share one walk.
fn declares_in<'t>(source: &'t str, declaration: Node<'t>, name: &str) -> Option<Node<'t>> {
    let declaration = unwrap_ambient(declaration);
    match declaration.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = declaration.walk();
            declaration
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
                .find(|declarator| {
                    declarator
                        .child_by_field_name("name")
                        .is_some_and(|bound| text_of(source, bound) == name)
                })
        }
        "function_signature"
        | "function_declaration"
        | "generator_function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration"
        | "module"
        | "internal_module" => declaration
            .child_by_field_name("name")
            .is_some_and(|bound| unquote(text_of(source, bound)) == name)
            .then_some(declaration),
        _ => None,
    }
}

/// Step through `ambient_declaration`, which wraps the declaration `declare` applies to.
fn unwrap_ambient(node: Node<'_>) -> Node<'_> {
    if node.kind() == "ambient_declaration" {
        node.named_child(0).unwrap_or(node)
    } else {
        node
    }
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

/// The local name behind `export { A }` or `export { A as B }`, asked about the visible one.
fn local_clause_name(decl: &Declaration, clause: Node<'_>, name: &str) -> Option<String> {
    let mut cursor = clause.walk();
    for specifier in clause.named_children(&mut cursor) {
        if specifier.kind() != "export_specifier" {
            continue;
        }
        let local = specifier.child_by_field_name("name")?;
        let visible = specifier.child_by_field_name("alias").unwrap_or(local);
        if unquote(text(decl, visible)) == name {
            return Some(unquote(text(decl, local)).to_owned());
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
#[must_use]
pub(crate) fn target_node<'d>(decl: &'d Declaration, name: &str) -> Option<Node<'d>> {
    declared_here(decl, name).or_else(|| match find_export(decl, name) {
        Some(Exported::Here(node)) => Some(node),
        _ => None,
    })
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
/// [`Declaration`] — what [`declares_in`] and [`declared_in`] need over a tree they do not
/// own.
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

/// Every module specifier this file imports from, in source order.
///
/// `import_statement`'s `source` field, plus `export_statement`'s: a barrel file re-exporting
/// what it never imports is exactly as dependent on those modules, and a completeness answer
/// that ignored them would call such a file complete while knowing nothing about it.
///
/// `import x = require('m')` needs its own fallback: `node-types.json` marks
/// `import_statement`'s own `source` field `required: false` and puts the field this shape
/// actually carries on its `import_require_clause` child instead — so a bare
/// `child_by_field_name("source")` on the statement itself answers nothing for exactly this
/// one shape, silently dropping it from the count.
#[must_use]
pub(crate) fn import_specifiers(tree: &Tree, source: &str) -> Vec<String> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|statement| matches!(statement.kind(), "import_statement" | "export_statement"))
        .filter_map(|statement| {
            statement.child_by_field_name("source").or_else(|| {
                named_child_of_kind(statement, "import_require_clause")
                    .and_then(|clause| clause.child_by_field_name("source"))
            })
        })
        .map(|node| unquote(text_of(source, node)).to_owned())
        .collect()
}
