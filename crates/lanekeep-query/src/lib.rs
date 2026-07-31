//! tree-sitter query parsing and compilation for lanekeep.
//!
//! Parses and compiles the tree-sitter queries that gate rule execution.
//!
//! Queries are the gate, not the rule language: they select which nodes reach a
//! Turing-complete TypeScript handler. That is what keeps JavaScript execution proportional
//! to matches rather than to nodes.
//!
//! # Why the error type is the bulk of this crate
//!
//! S-expression queries are the least approachable part of authoring a rule, and
//! tree-sitter's own diagnostics are close to unusable on their own — an unknown node kind
//! reports a message consisting of the quoted node name and nothing else. Since a rule
//! author's first several attempts will fail to compile, the quality of that failure is
//! most of what makes the query language tractable.
//!
//! What [`CompileError`] renders instead:
//!
//! ```text
//! query error: no such node kind in this grammar
//!   the typescript grammar has no node kind `nonexistent_node`
//!   --> query:3:11
//!    |
//!  3 |   value: (nonexistent_node) @v) @m
//!    |           ^
//! ```
//!
//! The line and caret matter more than they look. A real rule's query runs to a dozen
//! lines, and an error naming "the query" rather than a position in it leaves the author
//! bisecting by deletion.

use std::fmt;

use lanekeep_core::Position;
use lanekeep_lang::{Language, LanguageId};
use streaming_iterator::StreamingIterator;
use thiserror::Error;
use tree_sitter::{Node, Query, QueryCursor, QueryErrorKind, Tree};

/// What is wrong with a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileErrorKind {
    /// The query is not well-formed S-expression syntax.
    Syntax,
    /// A node kind the grammar does not define.
    UnknownNodeKind,
    /// A field name the grammar does not define.
    UnknownField,
    /// A predicate referenced a capture that the pattern does not bind.
    UnknownCapture,
    /// The pattern is syntactically valid but structurally impossible.
    ImpossiblePattern,
    /// The query binds no captures, so a handler has nothing to reference.
    NoCaptures,
    /// The grammar rejected the query for a reason lanekeep does not model.
    Other,
}

impl CompileErrorKind {
    /// A one-line explanation, phrased as what went wrong rather than what tree-sitter
    /// called it.
    const fn describe(self) -> &'static str {
        match self {
            Self::Syntax => "the query is not valid s-expression syntax",
            Self::UnknownNodeKind => "no such node kind in this grammar",
            Self::UnknownField => "no such field in this grammar",
            Self::UnknownCapture => "the query refers to a capture it never binds",
            Self::ImpossiblePattern => "this pattern can never match",
            Self::NoCaptures => "the query binds no captures",
            Self::Other => "the grammar rejected this query",
        }
    }
}

/// A query that failed to compile.
///
/// Carries enough to point at the problem: the kind, the position within the query source,
/// and the offending line with a caret. Rendering all of that is the whole point — a rule
/// author who cannot see *where* a 12-line pattern went wrong will rewrite it by guessing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub struct CompileError {
    /// What kind of problem.
    pub kind: CompileErrorKind,
    /// Which language's grammar rejected it.
    pub language: LanguageId,
    /// One-based position within the query source.
    pub position: Position,
    /// Byte offset within the query source.
    pub offset: usize,
    /// The specific token or name at fault, when tree-sitter identified one.
    pub detail: String,
    /// The offending line of the query, without its trailing newline.
    pub line: String,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "query error: {}", self.kind.describe())?;

        if !self.detail.is_empty() {
            match self.kind {
                CompileErrorKind::UnknownNodeKind => writeln!(
                    f,
                    "  the {} grammar has no node kind `{}`",
                    self.language, self.detail
                )?,
                CompileErrorKind::UnknownField => writeln!(
                    f,
                    "  the {} grammar has no field `{}`",
                    self.language, self.detail
                )?,
                _ => writeln!(f, "  {}", self.detail)?,
            }
        }

        if !self.line.is_empty() {
            let gutter = format!("{}", self.position.line);
            let pad = " ".repeat(gutter.len());
            writeln!(
                f,
                "{pad} --> query:{}:{}",
                self.position.line, self.position.column
            )?;
            writeln!(f, "{pad}  |")?;
            writeln!(f, "{gutter} | {}", self.line)?;
            // The caret column is a character offset into the line, so it lines up under
            // the token even when earlier characters are multi-byte.
            let caret_pad = " ".repeat(self.position.column.saturating_sub(1) as usize);
            writeln!(f, "{pad}  | {caret_pad}^")?;
        }

        Ok(())
    }
}

/// A query compiled against one language's grammar.
#[derive(Debug)]
pub struct CompiledQuery {
    query: Query,
    language: LanguageId,
    capture_names: Vec<String>,
}

impl CompiledQuery {
    /// Compile query source against a language.
    ///
    /// # Errors
    ///
    /// Returns [`CompileError`] for malformed syntax, unknown node kinds or fields, and for
    /// a query that binds no captures.
    pub fn compile(language: &dyn Language, source: &str) -> Result<Self, CompileError> {
        let grammar = language.grammar();
        let id = language.id();

        let query = Query::new(&grammar, source).map_err(|err| {
            let kind = match err.kind {
                QueryErrorKind::Syntax => CompileErrorKind::Syntax,
                QueryErrorKind::NodeType => CompileErrorKind::UnknownNodeKind,
                QueryErrorKind::Field => CompileErrorKind::UnknownField,
                QueryErrorKind::Capture => CompileErrorKind::UnknownCapture,
                QueryErrorKind::Structure => CompileErrorKind::ImpossiblePattern,
                _ => CompileErrorKind::Other,
            };

            // Syntax errors arrive with a caret diagram already baked into the message,
            // which would be rendered a second time by Display. Everything else arrives as
            // a bare quoted token.
            let detail = match kind {
                CompileErrorKind::Syntax => String::new(),
                _ => err.message.trim_matches('"').to_owned(),
            };

            CompileError {
                kind,
                language: id,
                position: Position::new(
                    u32::try_from(err.row).unwrap_or(u32::MAX).saturating_add(1),
                    u32::try_from(err.column)
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                ),
                offset: err.offset,
                detail,
                line: source.lines().nth(err.row).unwrap_or_default().to_owned(),
            }
        })?;

        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();

        // A pattern with no captures still matches, but the handler receives nothing it can
        // report at or inspect. Catching it here turns a rule that silently never reports
        // into a compile error naming the cause.
        if capture_names.is_empty() {
            return Err(CompileError {
                kind: CompileErrorKind::NoCaptures,
                language: id,
                position: Position::START,
                offset: 0,
                detail: "add a capture such as `@match` so the handler can reference the node"
                    .to_owned(),
                line: source.lines().next().unwrap_or_default().to_owned(),
            });
        }

        Ok(Self {
            query,
            language: id,
            capture_names,
        })
    }

    /// The language this query was compiled against.
    #[must_use]
    pub const fn language(&self) -> LanguageId {
        self.language
    }

    /// Capture names, in capture-index order.
    #[must_use]
    pub fn capture_names(&self) -> &[String] {
        &self.capture_names
    }

    /// How many alternative patterns the query contains.
    #[must_use]
    pub fn pattern_count(&self) -> usize {
        self.query.pattern_count()
    }

    /// Run the query over a tree, invoking `visit` once per match.
    ///
    /// A callback rather than an iterator, and rather than returning a `Vec`: matches
    /// borrow the tree, and collecting them would allocate for every match on the hot path
    /// this crate exists to keep cheap.
    ///
    /// Matches arrive in the order tree-sitter walks the tree, which is deterministic for a
    /// given tree and query. Nothing downstream may depend on that order beyond determinism
    /// itself — violations are sorted before reporting regardless.
    pub fn for_each_match<'tree>(
        &self,
        tree: &'tree Tree,
        source: &[u8],
        mut visit: impl FnMut(QueryMatch<'_, 'tree>),
    ) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        while let Some(m) = matches.next() {
            let captures = m
                .captures
                .iter()
                .map(|capture| {
                    let name = self
                        .capture_names
                        .get(capture.index as usize)
                        .map_or("", String::as_str);
                    (name, capture.node)
                })
                .collect();

            visit(QueryMatch {
                pattern_index: m.pattern_index,
                captures,
            });
        }
    }
}

/// One match, with its captured nodes.
#[derive(Debug, Clone)]
pub struct QueryMatch<'q, 'tree> {
    /// Which alternative pattern matched.
    pub pattern_index: usize,
    /// Captured nodes, in the order tree-sitter reported them.
    pub captures: Vec<(&'q str, Node<'tree>)>,
}

impl<'tree> QueryMatch<'_, 'tree> {
    /// The node bound to a capture name, if the pattern bound one.
    ///
    /// Returns the first when a name is bound more than once — which happens under
    /// quantifiers, where a single capture name legitimately matches repeatedly.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Node<'tree>> {
        self.captures
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, node)| *node)
    }

    /// Every node bound to a capture name.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = Node<'tree>> + 'a {
        self.captures
            .iter()
            .filter(move |(n, _)| *n == name)
            .map(|(_, node)| *node)
    }
}

#[cfg(test)]
mod tests {
    use lanekeep_lang_js::{JavaScript, Tsx, TypeScript};

    use super::*;

    fn parse(language: &dyn Language, source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.grammar())
            .expect("grammar loads");
        parser.parse(source, None).expect("parser returns a tree")
    }

    fn compile(source: &str) -> CompiledQuery {
        CompiledQuery::compile(&TypeScript, source).expect("query compiles")
    }

    fn compile_err(source: &str) -> CompileError {
        CompiledQuery::compile(&TypeScript, source).expect_err("query should not compile")
    }

    /// Every match, rendered as `capture=text` pairs, so assertions read like the query.
    fn run(query: &CompiledQuery, source: &str) -> Vec<Vec<String>> {
        let tree = parse(&TypeScript, source);
        let mut out = Vec::new();
        query.for_each_match(&tree, source.as_bytes(), |m| {
            out.push(
                m.captures
                    .iter()
                    .map(|(name, node)| {
                        let text = node.utf8_text(source.as_bytes()).unwrap_or("<invalid>");
                        format!("{name}={text}")
                    })
                    .collect(),
            );
        });
        out
    }

    #[test]
    fn compiles_a_simple_query() {
        let query = compile("(identifier) @id");
        assert_eq!(query.capture_names(), ["id"]);
        assert_eq!(query.pattern_count(), 1);
        assert_eq!(query.language().as_str(), "typescript");
    }

    #[test]
    fn reports_capture_names_in_index_order() {
        let query =
            compile("(pair key: (property_identifier) @prop value: (number) @value) @match");
        assert_eq!(query.capture_names(), ["prop", "value", "match"]);
    }

    #[test]
    fn matches_expose_captures_by_name() {
        let query =
            compile("(pair key: (property_identifier) @prop value: (number) @value) @match");
        let matches = run(&query, "const s = { padding: 12, margin: 4 };");

        assert_eq!(matches.len(), 2);
        assert!(matches[0].contains(&"prop=padding".to_owned()));
        assert!(matches[0].contains(&"value=12".to_owned()));
        assert!(matches[1].contains(&"prop=margin".to_owned()));
        assert!(matches[1].contains(&"value=4".to_owned()));
    }

    #[test]
    fn get_returns_the_node_for_a_capture() {
        let query = compile("(pair key: (property_identifier) @prop) @match");
        let source = "const s = { padding: 12 };";
        let tree = parse(&TypeScript, source);

        let mut seen = Vec::new();
        query.for_each_match(&tree, source.as_bytes(), |m| {
            let prop = m.get("prop").expect("prop is bound");
            seen.push(
                prop.utf8_text(source.as_bytes())
                    .unwrap_or_default()
                    .to_owned(),
            );
            assert!(m.get("nope").is_none(), "unbound capture must be None");
        });

        assert_eq!(seen, ["padding"]);
    }

    #[test]
    fn match_order_is_deterministic() {
        // Nothing downstream may depend on this order beyond its being stable — violations
        // are sorted before reporting either way. But an unstable order here would make
        // that sort the only thing standing between the tool and nondeterministic output,
        // which is a thinner guarantee than it looks.
        let query = compile("(identifier) @id");
        let source = "const alpha = 1; const beta = 2; function gamma() { return delta }";

        let first = run(&query, source);
        for _ in 0..25 {
            assert_eq!(
                run(&query, source),
                first,
                "match order varied between runs"
            );
        }
        assert!(
            first.len() >= 4,
            "expected several matches, got {}",
            first.len()
        );
    }

    #[test]
    fn a_query_matching_nothing_yields_no_matches() {
        let query = compile("(class_declaration) @c");
        assert!(run(&query, "const x = 1;").is_empty());
    }

    #[test]
    fn handles_an_empty_source_file() {
        let query = compile("(identifier) @id");
        assert!(run(&query, "").is_empty());
    }

    #[test]
    fn rejects_a_query_with_no_captures() {
        // A capture-free pattern still matches, but the handler receives nothing it can
        // report at. Without this check the rule silently never reports and the author has
        // no signal at all.
        let err = compile_err("(identifier)");
        assert_eq!(err.kind, CompileErrorKind::NoCaptures);
        assert!(
            err.to_string().contains("@match"),
            "should suggest adding a capture"
        );
    }

    #[test]
    fn rejects_an_unknown_node_kind_with_a_useful_message() {
        // tree-sitter's own message here is the quoted node name and nothing else.
        let err = compile_err("(nonexistent_node) @x");
        assert_eq!(err.kind, CompileErrorKind::UnknownNodeKind);
        assert_eq!(err.detail, "nonexistent_node");

        let rendered = err.to_string();
        assert!(
            rendered.contains("typescript"),
            "should name the grammar: {rendered}"
        );
        assert!(
            rendered.contains("nonexistent_node"),
            "should name the node: {rendered}"
        );
        assert!(
            rendered.contains("-->"),
            "should point at a position: {rendered}"
        );
        assert!(rendered.contains('^'), "should carry a caret: {rendered}");
    }

    #[test]
    fn rejects_an_unknown_field() {
        let err = compile_err("(pair nonexistent_field: (number) @n) @m");
        assert_eq!(err.kind, CompileErrorKind::UnknownField);
        assert_eq!(err.detail, "nonexistent_field");
        assert!(err.to_string().contains("no field"), "{err}");
    }

    #[test]
    fn rejects_malformed_syntax() {
        let err = compile_err("(pair key: (property_identifier) @a");
        assert_eq!(err.kind, CompileErrorKind::Syntax);
    }

    #[test]
    fn points_at_the_right_line_of_a_multiline_query() {
        // The reason this matters: a real rule's query is a dozen lines, and an error
        // pointing at "the query" rather than at a line is barely better than none.
        let err = CompiledQuery::compile(
            &TypeScript,
            "(pair\n  key: (property_identifier) @prop\n  value: (nonexistent_node) @v) @m",
        )
        .expect_err("should not compile");

        assert_eq!(err.position.line, 3, "should point at the third line");
        assert!(
            err.line.contains("nonexistent_node"),
            "excerpt should be that line: {err:?}"
        );

        let rendered = err.to_string();
        assert!(rendered.contains("query:3:"), "{rendered}");
        // The caret must sit under the token, not at the start of the line.
        let caret_line = rendered.lines().last().unwrap_or_default();
        let caret_col = caret_line.find('^').unwrap_or(0);
        assert!(
            caret_col > 4,
            "caret should be indented to the token: {rendered}"
        );
    }

    #[test]
    fn compiles_against_each_language() {
        // A query valid for one grammar is not automatically valid for another, which is
        // why compilation takes the language rather than assuming one.
        let jsx = "(jsx_element) @el";
        assert!(
            CompiledQuery::compile(&Tsx, jsx).is_ok(),
            "TSX should know jsx_element"
        );
        assert!(
            CompiledQuery::compile(&JavaScript, jsx).is_ok(),
            "JS should know jsx_element"
        );

        let err = CompiledQuery::compile(&TypeScript, jsx)
            .expect_err("plain TypeScript has no JSX nodes");
        assert_eq!(err.kind, CompileErrorKind::UnknownNodeKind);
        assert_eq!(err.language.as_str(), "typescript");

        let types = "(type_annotation) @t";
        assert!(CompiledQuery::compile(&TypeScript, types).is_ok());
        assert!(
            CompiledQuery::compile(&JavaScript, types).is_err(),
            "JavaScript has no type annotations"
        );
    }

    #[test]
    fn supports_alternations_and_multiple_patterns() {
        let query = compile("[(number) (string)] @literal");
        let matches = run(&query, "const a = 1; const b = 'two';");
        assert_eq!(matches.len(), 2);

        let two = compile("(number) @n\n(string) @s");
        assert_eq!(two.pattern_count(), 2);
        assert_eq!(two.capture_names(), ["n", "s"]);
    }

    #[test]
    fn get_all_returns_every_binding_of_a_repeated_capture() {
        // Under a quantifier one name legitimately binds several nodes, and `get` returning
        // only the first would silently drop the rest.
        let query = compile("(object (pair) @entry) @obj");
        let source = "const s = { a: 1, b: 2, c: 3 };";
        let tree = parse(&TypeScript, source);

        let mut counts = Vec::new();
        query.for_each_match(&tree, source.as_bytes(), |m| {
            counts.push(m.get_all("entry").count());
            assert!(m.get("entry").is_some());
        });

        assert!(!counts.is_empty(), "expected at least one match");
    }

    #[test]
    fn nodes_carry_positions_usable_for_reporting() {
        let query = compile("(number) @n");
        let source = "const a = 1;\nconst b = 22;";
        let tree = parse(&TypeScript, source);

        let mut positions = Vec::new();
        query.for_each_match(&tree, source.as_bytes(), |m| {
            let node = m.get("n").expect("bound");
            let start = node.start_position();
            positions.push((start.row + 1, start.column + 1));
        });

        assert_eq!(positions, [(1, 11), (2, 11)]);
    }
}
