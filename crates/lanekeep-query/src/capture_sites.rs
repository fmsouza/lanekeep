//! Where each capture in a query's *text* is bound: the pattern it decorates and the field
//! slot that pattern fills in its parent.
//!
//! Lexical on purpose. Neither this crate's [`CompiledQuery`](crate::CompiledQuery) nor
//! tree-sitter's own `Query` can say which node a capture binds — the API exposes capture
//! *names*, pattern byte ranges and predicates, and nothing about the pattern under a
//! capture — so the text is the only place the answer exists. That keeps this usable with no
//! grammar in hand, which is what `lanekeep-config` needs: it validates a rule's `flow`
//! queries at load, before any language has been chosen to compile them against.

/// One capture bound in a query's text, and the slot it is bound in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSite {
    /// The capture's name, without its `@`.
    pub name: String,
    /// The field label on the pattern this capture decorates — `function` for the `@s` in
    /// `(call_expression function: (identifier) @s)` — or `None` when that pattern is an
    /// unlabeled child or a top-level pattern.
    ///
    /// Only the decorated pattern's own slot: a capture nested inside a labeled child reports
    /// the slot *its* pattern fills, not its ancestor's, so the `@x` in `(a b: (c (d) @x))`
    /// has no field.
    pub field: Option<String>,
}

/// Every capture bound in `query`, in text order, each with the field slot of the pattern it
/// decorates.
///
/// A capture named inside a predicate — the `@x` of `(#eq? @x "y")` — is a reference, not a
/// binding, and is not listed. Quantifiers between a pattern and its captures are transparent
/// (`(c)* @x` binds `@x` to `(c)`); an alternation, an anonymous `"token"` and a wildcard `_`
/// are patterns like any other; a `!field` negation and a `.` anchor label nothing.
///
/// Total over any text: a malformed query yields whatever sites its text does bind, and
/// [`CompiledQuery::compile`](crate::CompiledQuery::compile) is where the syntax error is
/// reported. This function's only obligation on the way there is to neither lose nor invent a
/// site.
#[must_use]
pub fn capture_sites(query: &str) -> Vec<CaptureSite> {
    let mut scanner = Scanner {
        text: query.as_bytes(),
        pos: 0,
        sites: Vec::new(),
    };
    scanner.sequence(None);
    scanner.sites
}

/// A cursor over the query's bytes. Byte-wise rather than char-wise so an unknown byte can be
/// stepped over without ever slicing inside a multi-byte character: the only slices taken are
/// identifiers, which are ASCII.
struct Scanner<'a> {
    text: &'a [u8],
    pos: usize,
    sites: Vec<CaptureSite>,
}

/// A byte that may appear in a node kind, a field name, a supertype path or a capture name.
///
/// `.` is in the set for capture names like `@x.y`, and it is also the anchor token — which
/// is why [`Scanner::sequence`] checks for an anchor before it reads an identifier. `?` and
/// `!`, which tree-sitter's own identifier scanner admits, are left out so that `@x?` binds
/// `x` and then reads `?` as the quantifier it was meant as.
const fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.pos).copied()
    }

    /// Parse items until `close` — or the end of the text when `None` — consuming the closer.
    ///
    /// A `field:` label is held until the next pattern at this level and handed to it; a
    /// pattern is a `(…)` group, a `[…]` alternation, a `"token"` or a bare `_`, and each
    /// takes its trailing quantifiers and captures on its way out.
    fn sequence(&mut self, close: Option<u8>) {
        let mut pending_field: Option<String> = None;
        loop {
            self.skip_trivia();
            let Some(byte) = self.peek() else {
                return;
            };
            match byte {
                b')' | b']' => {
                    self.pos += 1;
                    // A closer this level did not open is a malformed query. Step over it
                    // and keep scanning; the compiler names the fault.
                    if Some(byte) == close {
                        return;
                    }
                }
                b'(' => {
                    self.pos += 1;
                    self.skip_trivia();
                    if self.peek() == Some(b'#') {
                        self.skip_predicate();
                        continue;
                    }
                    // A node pattern `(kind …)`, a wildcard `(_ …)`, or a bare grouping
                    // `((a) (b))`. The kind, when present, does not decide where a capture
                    // binds, so it is read and dropped.
                    self.ident();
                    self.sequence(Some(b')'));
                    self.trailing(pending_field.take());
                }
                b'[' => {
                    self.pos += 1;
                    self.sequence(Some(b']'));
                    self.trailing(pending_field.take());
                }
                b'"' => {
                    self.skip_string();
                    self.trailing(pending_field.take());
                }
                // An anchor. Checked before the identifier arm because `.` is an identifier
                // byte too.
                b'.' => self.pos += 1,
                b'!' => {
                    self.pos += 1;
                    self.ident();
                    pending_field = None;
                }
                // A capture with no pattern in front of it binds nothing this scanner can
                // name; recorded without a slot rather than dropped, so a count of sites
                // still matches a count of `@`s.
                b'@' => {
                    self.pos += 1;
                    let name = self.ident();
                    self.push(name, None);
                }
                b'*' | b'+' | b'?' => self.pos += 1,
                _ if is_ident(byte) => {
                    let ident = self.ident();
                    if self.peek() == Some(b':') {
                        self.pos += 1;
                        pending_field = Some(ident);
                    } else {
                        // A bare `_` wildcard — or, in malformed text, a bare word — stands
                        // as a pattern of its own.
                        self.trailing(pending_field.take());
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Consume the quantifiers and captures that follow a pattern, binding each capture to
    /// `field` — the slot the pattern just closed fills.
    fn trailing(&mut self, field: Option<String>) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'@') => {
                    self.pos += 1;
                    let name = self.ident();
                    self.push(name, field.clone());
                }
                Some(b'*' | b'+' | b'?') => self.pos += 1,
                _ => return,
            }
        }
    }

    fn push(&mut self, name: String, field: Option<String>) {
        if !name.is_empty() {
            self.sites.push(CaptureSite { name, field });
        }
    }

    /// Read an identifier at the cursor; empty when there is none.
    fn ident(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(is_ident) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.text[start..self.pos]).into_owned()
    }

    /// Skip whitespace and `;` comments, which run to the end of their line.
    fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek() {
            match byte {
                b';' => {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.pos += 1;
                    }
                }
                _ if byte.is_ascii_whitespace() => self.pos += 1,
                _ => return,
            }
        }
    }

    /// Skip a `"…"` literal at the cursor, honoring `\"` escapes.
    fn skip_string(&mut self) {
        self.pos += 1;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                b'"' => return,
                _ => {}
            }
        }
    }

    /// Skip a predicate whose `(` is already consumed and whose `#` is at the cursor, up to
    /// and including its closing `)`. Parentheses inside its string arguments do not count.
    fn skip_predicate(&mut self) {
        let mut depth = 1_usize;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => self.skip_string(),
                b'(' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureSite, capture_sites};

    fn sites(query: &str) -> Vec<(String, Option<String>)> {
        capture_sites(query)
            .into_iter()
            .map(|CaptureSite { name, field }| (name, field))
            .collect()
    }

    fn site(name: &str, field: Option<&str>) -> (String, Option<String>) {
        (name.to_owned(), field.map(str::to_owned))
    }

    #[test]
    fn a_capture_on_a_labeled_child_records_the_field() {
        assert_eq!(
            sites("(call_expression function: (identifier) @s)"),
            vec![site("s", Some("function"))]
        );
    }

    #[test]
    fn a_capture_on_an_unlabeled_child_has_no_field() {
        assert_eq!(
            sites("(call_expression (arguments) @a)"),
            vec![site("a", None)]
        );
    }

    #[test]
    fn a_capture_on_the_whole_pattern_has_no_field() {
        assert_eq!(
            sites("(call_expression function: (identifier) @fn) @call"),
            vec![site("fn", Some("function")), site("call", None)]
        );
    }

    #[test]
    fn two_captures_on_one_pattern_share_its_slot() {
        assert_eq!(
            sites("(a b: (c) @x @y)"),
            vec![site("x", Some("b")), site("y", Some("b"))]
        );
    }

    #[test]
    fn the_field_reaches_only_the_pattern_that_fills_it() {
        assert_eq!(sites("(a b: (c) (d) @x)"), vec![site("x", None)]);
    }

    #[test]
    fn a_capture_nested_inside_a_labeled_child_reports_its_own_slot() {
        assert_eq!(sites("(a b: (c (d) @x))"), vec![site("x", None)]);
    }

    #[test]
    fn an_alternation_in_a_slot_carries_the_field() {
        assert_eq!(sites("(a b: [(c) (d)] @x)"), vec![site("x", Some("b"))]);
    }

    #[test]
    fn a_quantifier_between_pattern_and_capture_is_transparent() {
        assert_eq!(sites("(a b: (c)* @x)"), vec![site("x", Some("b"))]);
        assert_eq!(sites("(a b: (c)+ @x)"), vec![site("x", Some("b"))]);
        assert_eq!(sites("(a b: (c)? @x)"), vec![site("x", Some("b"))]);
    }

    #[test]
    fn an_anonymous_node_and_a_wildcard_fill_a_slot_too() {
        assert_eq!(
            sites(r#"(a b: "tok" @x c: _ @y)"#),
            vec![site("x", Some("b")), site("y", Some("c"))]
        );
    }

    #[test]
    fn a_negated_field_labels_nothing() {
        assert_eq!(sites("(a !b (c) @x)"), vec![site("x", None)]);
    }

    #[test]
    fn an_anchor_labels_nothing() {
        assert_eq!(sites("(a . (c) @x)"), vec![site("x", None)]);
        assert_eq!(sites("(a b: (c) . (d) @x)"), vec![site("x", None)]);
    }

    #[test]
    fn a_predicate_is_not_a_binding_site() {
        // `@x` appears three times; only the first is a binding.
        assert_eq!(
            sites(r#"(a b: (c) @x (#eq? @x "y") (#match? @x "\\)"))"#),
            vec![site("x", Some("b"))]
        );
    }

    #[test]
    fn a_comment_is_skipped() {
        assert_eq!(
            sites("; (z: (q) @not)\n(a (b) @x) ; @nope\n"),
            vec![site("x", None)]
        );
    }

    #[test]
    fn a_string_containing_a_paren_does_not_unbalance() {
        assert_eq!(sites(r#"(a b: "(" @x)"#), vec![site("x", Some("b"))]);
        assert_eq!(
            sites(r#"(a b: "\"" @x c: (d) @y)"#),
            vec![site("x", Some("b")), site("y", Some("c"))]
        );
    }

    #[test]
    fn a_grouped_pattern_records_the_inner_capture() {
        assert_eq!(sites(r#"((a) @x (#eq? @x "y"))"#), vec![site("x", None)]);
    }

    #[test]
    fn two_top_level_patterns_are_scanned_in_order() {
        assert_eq!(
            sites("(a) @x\n(b c: (d) @y)"),
            vec![site("x", None), site("y", Some("c"))]
        );
    }

    #[test]
    fn a_supertype_pattern_is_one_pattern() {
        assert_eq!(sites("(expression/identifier) @x"), vec![site("x", None)]);
        assert_eq!(
            sites("(a b: (expression/identifier) @x)"),
            vec![site("x", Some("b"))]
        );
    }

    #[test]
    fn a_capture_name_may_carry_dots() {
        assert_eq!(sites("(a) @x.y"), vec![site("x.y", None)]);
    }

    #[test]
    fn malformed_text_still_returns_what_was_found() {
        // The grammar's compiler reports the syntax error; this just must not lose or invent
        // a site on the way there.
        assert_eq!(sites("(a b: (c) @x"), vec![site("x", Some("b"))]);
        assert_eq!(sites(") @x"), vec![site("x", None)]);
        assert_eq!(sites(""), Vec::new());
    }
}
