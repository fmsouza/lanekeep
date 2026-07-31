//! Turning TypeScript rule modules into JavaScript the engine can run.
//!
//! # Blanking, not rewriting
//!
//! Type syntax is overwritten with spaces in place rather than removed. Every byte that
//! survives keeps its original offset, and newlines inside a blanked range are preserved,
//! so a line and column in the generated JavaScript is the same line and column in the
//! author's TypeScript.
//!
//! That is the whole reason for this approach. A stack trace from a rule that threw points
//! at the author's source directly, with no source map to generate, ship, parse or get
//! subtly wrong. For a tool whose value rests on the quality of what it tells you when
//! something is wrong, that is worth more than it costs.
//!
//! The alternative was a full TypeScript transformer, which would handle every construct
//! but add roughly eighty crates to a dependency graph that is currently thirty-six — on a
//! tool that runs as a pre-commit hook and is therefore a supply-chain target. See
//! `docs/architecture.md` §13.
//!
//! # What is not supported
//!
//! Blanking works only for syntax that has no runtime meaning. Four TypeScript features
//! generate code, so there is nothing to blank and they are rejected with an explanation:
//! `enum`, `namespace`, decorators, and constructor parameter properties.
//!
//! Rule modules are small and self-contained, and each of these has a plain alternative.
//! Rejecting loudly is much better than emitting JavaScript that silently means something
//! else.
//!
//! # The safety net
//!
//! Stripping is verified rather than trusted: the result is parsed as JavaScript, and a
//! syntax error means the stripper is wrong, not the author. That check turns a whole class
//! of subtle stripping bugs into a loud failure at the point of the mistake.

use lanekeep_lang::Language;
use thiserror::Error;
use tree_sitter::{Node, Parser, Tree};

/// A TypeScript construct that cannot be stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// `enum E { A }` — emits a runtime object.
    Enum,
    /// `namespace N {}` or `module N {}` — emits a runtime object.
    Namespace,
    /// `@decorator` — calls a function at class definition time.
    Decorator,
    /// `constructor(private x: T)` — assigns a field at construction time.
    ParameterProperty,
}

impl Unsupported {
    const fn describe(self) -> &'static str {
        match self {
            Self::Enum => "`enum` declarations",
            Self::Namespace => "`namespace` and `module` declarations",
            Self::Decorator => "decorators",
            Self::ParameterProperty => "constructor parameter properties",
        }
    }

    const fn alternative(self) -> &'static str {
        match self {
            Self::Enum => "use a plain object with `as const`, or a union of string literals",
            Self::Namespace => "use a module — a rule file is already one",
            Self::Decorator => "call the function directly instead",
            Self::ParameterProperty => "declare the field and assign it in the constructor body",
        }
    }
}

/// Why a rule module could not be prepared for execution.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StripError {
    /// The source uses a construct that generates runtime code.
    #[error(
        "{} are not supported in rule files\n  \
         at line {line}, column {column}\n  \
         they generate runtime code, so there is no type syntax to remove — {}",
        .construct.describe(),
        .construct.alternative()
    )]
    Unsupported {
        /// Which construct.
        construct: Unsupported,
        /// One-based line.
        line: u32,
        /// One-based column.
        column: u32,
    },

    /// The source does not parse as TypeScript.
    #[error("rule module is not valid TypeScript\n  at line {line}, column {column}")]
    Syntax {
        /// One-based line.
        line: u32,
        /// One-based column.
        column: u32,
    },

    /// The stripped output does not parse as JavaScript, which means this stripper has a
    /// bug rather than the rule having one.
    #[error(
        "internal error: type stripping produced invalid JavaScript at line {line}, \
         column {column}\n  this is a bug in lanekeep, not in the rule — please report it \
         with the rule source"
    )]
    StripperBug {
        /// One-based line in the generated output.
        line: u32,
        /// One-based column.
        column: u32,
    },
}

/// Node kinds whose entire span is type-only.
const BLANK_WHOLE: &[&str] = &[
    "type_annotation",
    "omitting_type_annotation",
    "adding_type_annotation",
    "opting_type_annotation",
    "asserts_annotation",
    "type_predicate_annotation",
    "type_parameters",
    "type_arguments",
    "interface_declaration",
    "type_alias_declaration",
    "ambient_declaration",
    "implements_clause",
    "abstract_method_signature",
    "method_signature",
    "property_signature",
    "construct_signature",
    "index_signature",
    "call_signature",
];

/// Keywords that are type-only when they appear as a bare token.
const BLANK_KEYWORD: &[&str] = &["abstract", "declare", "override", "readonly"];

/// Strip TypeScript type syntax, returning JavaScript with identical byte offsets.
///
/// # Errors
///
/// Returns [`StripError::Unsupported`] for a construct that generates runtime code,
/// [`StripError::Syntax`] if the input is not valid TypeScript, and
/// [`StripError::StripperBug`] if the output fails to parse as JavaScript.
pub fn strip_types(
    typescript: &dyn Language,
    javascript: &dyn Language,
    source: &str,
) -> Result<String, StripError> {
    let tree = parse(typescript, source)?;
    if let Some(node) = first_error(tree.root_node()) {
        let position = node.start_position();
        return Err(StripError::Syntax {
            line: one_based(position.row),
            column: one_based(position.column),
        });
    }

    let mut output: Vec<u8> = source.as_bytes().to_vec();
    strip_node(tree.root_node(), source, &mut output)?;

    let stripped = String::from_utf8(output).unwrap_or_else(|_| source.to_owned());

    // Verification, not decoration. Every stripping bug that produces syntactically broken
    // JavaScript is caught here, at the point of the mistake, instead of surfacing later as
    // an incomprehensible error from inside the engine.
    let check = parse(javascript, &stripped)?;
    if let Some(node) = first_error(check.root_node()) {
        let position = node.start_position();
        return Err(StripError::StripperBug {
            line: one_based(position.row),
            column: one_based(position.column),
        });
    }

    Ok(stripped)
}

fn parse(language: &dyn Language, source: &str) -> Result<Tree, StripError> {
    let mut parser = Parser::new();
    // A grammar that will not load is a broken build, and the caller has no better
    // response than the one the verification path already gives.
    if parser.set_language(&language.grammar()).is_err() {
        return Err(StripError::Syntax { line: 1, column: 1 });
    }
    parser
        .parse(source, None)
        .ok_or(StripError::Syntax { line: 1, column: 1 })
}

fn first_error(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    if !node.has_error() {
        return None;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).find_map(first_error)
}

fn one_based(zero_based: usize) -> u32 {
    u32::try_from(zero_based)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

fn reject(node: Node<'_>, construct: Unsupported) -> StripError {
    let position = node.start_position();
    StripError::Unsupported {
        construct,
        line: one_based(position.row),
        column: one_based(position.column),
    }
}

/// Overwrite a byte range with spaces, keeping newlines so line numbers do not move.
fn blank(output: &mut [u8], range: std::ops::Range<usize>) {
    for byte in &mut output[range] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn strip_node(node: Node<'_>, source: &str, output: &mut Vec<u8>) -> Result<(), StripError> {
    let kind = node.kind();

    // Constructs that generate runtime code. There is no type syntax to remove, so
    // stripping would change what the module means rather than only how it is annotated.
    match kind {
        "enum_declaration" => return Err(reject(node, Unsupported::Enum)),
        "internal_module" | "module" => return Err(reject(node, Unsupported::Namespace)),
        "decorator" => return Err(reject(node, Unsupported::Decorator)),
        _ => {}
    }

    if BLANK_WHOLE.contains(&kind) {
        blank(output, node.byte_range());
        return Ok(());
    }

    match kind {
        // `import type {...}` and `export type {...}` are entirely type-only. A `type`
        // marker on individual specifiers is handled when those specifiers are visited.
        "import_statement" | "export_statement" if has_leading_type_keyword(node) => {
            blank(output, node.byte_range());
            return Ok(());
        }

        // `x as T`, `x satisfies T`, `x!` — all of the form "an expression followed by
        // type-only syntax". Keep the expression, blank everything after it, and keep
        // descending so nested assertions inside it are stripped too.
        "as_expression" | "satisfies_expression" | "non_null_expression" => {
            if let Some(expression) = node.named_child(0) {
                blank(output, expression.end_byte()..node.end_byte());
                return strip_node(expression, source, output);
            }
        }

        // An accessibility modifier on a constructor parameter is a parameter property:
        // it declares and assigns a field. Anywhere else it is type-only.
        // `function f(this: Window, a: number)` — a `this` parameter is TypeScript-only,
        // and `this` is not a valid binding name in JavaScript. Blanking only its type
        // annotation would leave `function f(this , a)`, which does not parse. The
        // separating comma has to go with it, since `(, a)` does not parse either.
        "required_parameter" if is_this_parameter(node) => {
            let mut end = node.end_byte();
            if let Some(next) = node.next_sibling() {
                if next.kind() == "," {
                    end = next.end_byte();
                }
            }
            blank(output, node.start_byte()..end);
            return Ok(());
        }

        "required_parameter" | "optional_parameter" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "accessibility_modifier" && in_constructor(node, source) {
                    return Err(reject(child, Unsupported::ParameterProperty));
                }
            }
            // `b?: T` — the marker is an anonymous `?` token.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "?" {
                    blank(output, child.byte_range());
                }
            }
        }

        _ => {}
    }

    // Bare type-only keywords: `abstract class`, `declare`, `readonly x`, `override m()`.
    if BLANK_KEYWORD.contains(&kind) && !node.is_named() {
        blank(output, node.byte_range());
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Anonymous tokens carry the type-only keywords, and `type` inside a specifier.
        if !child.is_named() {
            let text = &source[child.byte_range()];
            if BLANK_KEYWORD.contains(&text)
                || (text == "type" && matches!(kind, "import_specifier" | "export_specifier"))
            {
                blank(output, child.byte_range());
                continue;
            }
        }
        if child.kind() == "accessibility_modifier" {
            if in_constructor(node, source) {
                return Err(reject(child, Unsupported::ParameterProperty));
            }
            blank(output, child.byte_range());
            continue;
        }
        strip_node(child, source, output)?;
    }

    Ok(())
}

/// Whether this parameter is TypeScript's `this` parameter rather than a real binding.
fn is_this_parameter(parameter: Node<'_>) -> bool {
    parameter
        .named_child(0)
        .is_some_and(|first| first.kind() == "this")
}

/// Whether an `import`/`export` statement is wholly type-only, as in `import type {...}`.
fn has_leading_type_keyword(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .nth(1)
        .is_some_and(|second| !second.is_named() && second.kind() == "type")
}

/// Whether a parameter belongs to a constructor, which is what makes an accessibility
/// modifier on it a runtime field declaration rather than an annotation.
fn in_constructor(parameter: Node<'_>, source: &str) -> bool {
    let mut current = parameter.parent();
    while let Some(node) = current {
        match node.kind() {
            // Only a constructor's parameters declare fields. `private` on any other
            // method's parameter is not valid TypeScript in the first place, and treating
            // it as a parameter property would produce a misleading diagnostic for what is
            // really a type error.
            "method_definition" => {
                return node
                    .child_by_field_name("name")
                    .is_some_and(|name| &source[name.byte_range()] == "constructor");
            }
            "formal_parameters" | "required_parameter" | "optional_parameter" => {
                current = node.parent();
            }
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use lanekeep_lang_js::{JavaScript, TypeScript};

    use super::*;

    fn strip(source: &str) -> Result<String, StripError> {
        strip_types(&TypeScript, &JavaScript, source)
    }

    fn stripped(source: &str) -> String {
        strip(source).expect("should strip")
    }

    /// Collapse runs of spaces so assertions read as intent rather than as whitespace.
    fn normalized(source: &str) -> String {
        stripped(source)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn positions_are_preserved_exactly() {
        // The property the whole approach exists for. Byte length identical, newlines
        // untouched, so a line and column in the output is the same one in the input.
        let source = "const x: number = 1;\ninterface A { b: string }\nconst y: A = { b: 'q' };\n";
        let out = stripped(source);

        assert_eq!(out.len(), source.len(), "byte length must not change");
        assert_eq!(
            out.lines().count(),
            source.lines().count(),
            "line count must not change"
        );
        for (index, (before, after)) in source.lines().zip(out.lines()).enumerate() {
            assert_eq!(
                before.len(),
                after.len(),
                "line {} changed length",
                index + 1
            );
        }
    }

    #[test]
    fn strips_type_annotations() {
        assert_eq!(normalized("const x: number = 1;"), "const x = 1;");
        assert_eq!(
            normalized("function f(a: string, b: number): void {}"),
            "function f(a , b ) {}"
        );
    }

    #[test]
    fn strips_interfaces_and_type_aliases() {
        assert_eq!(
            normalized("interface A { b: string }\nconst c = 1;"),
            "const c = 1;"
        );
        assert_eq!(
            normalized("type B = string | null;\nconst c = 1;"),
            "const c = 1;"
        );
    }

    #[test]
    fn strips_generics() {
        assert_eq!(
            normalized("function f<T>(a: T): T { return a }"),
            "function f (a ) { return a }"
        );
        assert_eq!(
            normalized("const m = new Map<string, number>();"),
            "const m = new Map ();"
        );
    }

    #[test]
    fn strips_assertions_but_keeps_the_expression() {
        assert_eq!(normalized("const y = z as Foo;"), "const y = z ;");
        assert_eq!(normalized("const w = v satisfies Bar;"), "const w = v ;");
        assert_eq!(normalized("const u = t!;"), "const u = t ;");
        // Nested, to prove the inner expression is still visited.
        assert_eq!(normalized("const a = (b as C).d;"), "const a = (b ).d;");
    }

    #[test]
    fn strips_optional_parameter_markers() {
        assert_eq!(normalized("function f(a?: string) {}"), "function f(a ) {}");
    }

    #[test]
    fn strips_type_only_imports_and_exports() {
        assert_eq!(
            normalized("import type { A } from './a';\nconst c = 1;"),
            "const c = 1;"
        );
        assert_eq!(
            normalized("export type { Z };\nconst c = 1;"),
            "const c = 1;"
        );
    }

    #[test]
    fn strips_inline_type_specifiers_but_keeps_the_value_import() {
        // `import { type B, C }` must keep C — blanking the whole statement would delete a
        // real binding and produce a ReferenceError at runtime.
        let out = normalized("import { type B, C } from './b';");
        assert!(out.contains('C'), "value import must survive: {out}");
        assert!(
            out.contains("from './b'"),
            "the module specifier must survive: {out}"
        );
        assert!(!out.contains("type"), "the type marker must go: {out}");
    }

    #[test]
    fn strips_declare_and_ambient_declarations() {
        assert_eq!(
            normalized("declare const g: number;\nconst c = 1;"),
            "const c = 1;"
        );
    }

    #[test]
    fn strips_class_type_syntax() {
        let out = normalized("class K implements I { readonly n: number = 1; }");
        assert!(!out.contains("implements"), "{out}");
        assert!(!out.contains("readonly"), "{out}");
        assert!(
            out.contains("n = 1"),
            "the field initializer must survive: {out}"
        );
    }

    #[test]
    fn strips_abstract_classes() {
        let out = normalized("abstract class M { go() { return 1 } }");
        assert!(!out.contains("abstract"), "{out}");
        assert!(out.contains("class M"), "{out}");
    }

    #[test]
    fn strips_type_predicates() {
        let out = normalized("function isFoo(x: unknown): x is Foo { return true }");
        assert!(!out.contains(" is Foo"), "{out}");
        assert!(out.contains("return true"), "{out}");
    }

    #[test]
    fn strips_this_parameters_entirely() {
        // `this` is not a valid binding name in JavaScript, so blanking only its type
        // annotation would leave source that does not parse. The safety net catches that,
        // but the right behavior is to remove the parameter and its comma.
        let out = normalized("function f(this: Window, a: number) { return a }");
        assert!(!out.contains("this"), "{out}");
        assert!(out.contains("function f("), "{out}");
        assert!(out.contains("return a"), "{out}");

        let only = normalized("function g(this: Window) { return 1 }");
        assert!(!only.contains("this"), "{only}");
    }

    #[test]
    fn leaves_plain_javascript_untouched() {
        for source in [
            "const a = 1;",
            "export default function () { return [1,2,3].map(x => x * 2) }",
            "class A extends B { #p = 1; static s() {} }",
            "const { a, ...rest } = obj; const [x, y] = arr;",
            "async function f() { for await (const x of y) {} }",
        ] {
            assert_eq!(
                stripped(source),
                source,
                "plain JavaScript should be unchanged"
            );
        }
    }

    // --- rejections ------------------------------------------------------------------

    #[test]
    fn rejects_enums() {
        let err = strip("enum E { A, B }").expect_err("enums generate runtime code");
        assert!(matches!(
            err,
            StripError::Unsupported {
                construct: Unsupported::Enum,
                ..
            }
        ));

        let rendered = err.to_string();
        assert!(
            rendered.contains("as const"),
            "should suggest the alternative: {rendered}"
        );
        assert!(rendered.contains("line 1"), "should say where: {rendered}");
    }

    #[test]
    fn rejects_namespaces() {
        let err = strip("namespace N { export const q = 1 }").expect_err("namespaces emit code");
        assert!(matches!(
            err,
            StripError::Unsupported {
                construct: Unsupported::Namespace,
                ..
            }
        ));
    }

    #[test]
    fn rejects_parameter_properties() {
        // The subtle one: `private` here declares and assigns a field, so blanking it
        // would silently produce a class whose field is never set.
        let err = strip("class K { constructor(private p: string) {} }")
            .expect_err("parameter properties emit code");
        assert!(matches!(
            err,
            StripError::Unsupported {
                construct: Unsupported::ParameterProperty,
                ..
            }
        ));
    }

    #[test]
    fn an_accessibility_modifier_outside_a_constructor_is_type_only() {
        // The counterpart to the case above: `private` on a field is an annotation, and
        // rejecting it too would be over-broad.
        let out = normalized("class K { private n = 1; }");
        assert!(!out.contains("private"), "{out}");
        assert!(out.contains("n = 1"), "{out}");
    }

    #[test]
    fn reports_the_line_of_the_offending_construct() {
        let err = strip("const a = 1;\nconst b = 2;\nenum E { X }").expect_err("rejects");
        match err {
            StripError::Unsupported { line, .. } => assert_eq!(line, 3),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_source_that_is_not_typescript() {
        let err = strip("function ( { ] }").expect_err("does not parse");
        assert!(matches!(err, StripError::Syntax { .. }), "{err:?}");
    }

    #[test]
    fn handles_empty_input() {
        assert_eq!(stripped(""), "");
        assert_eq!(stripped("\n\n"), "\n\n");
    }

    // --- the safety net ------------------------------------------------------------------

    #[test]
    fn every_stripped_result_parses_as_javascript() {
        // strip_types verifies this internally, so reaching Ok here means the check passed.
        // Running a realistic module through it is what makes that check meaningful.
        let source = r"
import type { Rule } from 'lanekeep';
import { defineRule } from 'lanekeep';

interface Options {
  readonly max: number;
}

type Names = 'a' | 'b';

export default defineRule({
  id: 'local/example',
  query: '(identifier) @id',
  check(ctx: unknown, m: { id: unknown }): void {
    const names: Names[] = ['a', 'b'];
    const n = (ctx as Options).max;
    for (const name of names) {
      if (n! > 0) { (ctx as { report(x: unknown): void }).report(m.id); }
    }
  },
});
";
        let out = stripped(source);
        assert_eq!(
            out.len(),
            source.len(),
            "positions must survive a realistic module"
        );
        assert!(!out.contains("interface"), "{out}");
        assert!(!out.contains(": number"), "{out}");
        assert!(out.contains("defineRule"), "the runtime code must survive");
        assert!(
            out.contains("report(m.id)"),
            "the runtime code must survive"
        );
    }
}
