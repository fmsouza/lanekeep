//! The `ctx` object rule code receives.
//!
//! This is the trust boundary made concrete. Everything a rule can do to the outside world
//! it does through a function installed here; anything not installed does not exist. Adding
//! to this surface widens what a rule may reach and bumps the host API version that feeds
//! the cache key.
//!
//! # What is here so far
//!
//! Reporting and tree navigation. Binding resolution, tracked file reads and facts arrive
//! in later milestones.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Object};

use lanekeep_lang::binding::{Binding, BindingResolver, ImportedName};

use crate::nodes::{Handle, NodeArena};

/// A violation a rule asked for.
///
/// Deliberately not a `lanekeep_core::Violation`. A rule supplies a position and optionally
/// a message; the rule's identity, severity and card come from the engine, which is what
/// stops a rule from reporting under someone else's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The node reported at, when the rule passed one.
    pub node: Option<Handle>,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// A message overriding the rule card's, when the rule supplied one.
    pub message: Option<String>,
}

/// Host state for one file, shared with the functions installed on `ctx`.
///
/// `Debug` is hand-written: requiring it on `BindingResolver` would burden every language
/// implementation for the sake of one derive here.
#[derive(Clone)]
pub struct HostContext {
    arena: Rc<RefCell<NodeArena>>,
    reports: Rc<RefCell<Vec<Report>>>,
    file_path: Rc<str>,
    resolver: Option<Arc<dyn BindingResolver>>,
}

impl std::fmt::Debug for HostContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostContext")
            .field("file_path", &self.file_path)
            .field("interned_nodes", &self.arena.borrow().len())
            .field("reports", &self.reports.borrow().len())
            .field("has_resolver", &self.resolver.is_some())
            .finish()
    }
}

impl HostContext {
    /// Build a context over a parsed file.
    #[must_use]
    pub fn new(tree: tree_sitter::Tree, source: String, file_path: &str) -> Self {
        Self {
            arena: Rc::new(RefCell::new(NodeArena::new(tree, source))),
            reports: Rc::new(RefCell::new(Vec::new())),
            file_path: Rc::from(file_path),
            resolver: None,
        }
    }

    /// Attach a binding resolver, enabling the import-resolution functions.
    ///
    /// Without one, those functions return `false` or `undefined` rather than being
    /// absent. A rule written against a language that has no resolver then behaves as
    /// though nothing resolves, which is a truthful answer — where a missing function
    /// would be a `TypeError` blamed on the rule.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn BindingResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// The arena, for interning query captures before invoking a handler.
    #[must_use]
    pub fn arena(&self) -> &Rc<RefCell<NodeArena>> {
        &self.arena
    }

    /// Take everything reported so far, leaving the context empty.
    #[must_use]
    pub fn take_reports(&self) -> Vec<Report> {
        std::mem::take(&mut self.reports.borrow_mut())
    }

    /// Build the `ctx` object.
    ///
    /// # Errors
    ///
    /// Returns an engine error if a property cannot be defined, which would mean a broken
    /// build rather than anything about a rule.
    pub fn build<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
        let object = Object::new(ctx.clone())?;

        object.set("filePath", &*self.file_path)?;
        object.set("root", NodeArena::ROOT)?;
        {
            let arena = self.arena.borrow();
            object.set("fileText", arena.source())?;
        }

        self.install_navigation(ctx, &object)?;
        self.install_bindings(ctx, &object)?;
        self.install_reporting(ctx, &object)?;

        Ok(object)
    }

    /// Reading the tree.
    fn install_navigation<'js>(
        &self,
        ctx: &Ctx<'js>,
        object: &Object<'js>,
    ) -> rquickjs::Result<()> {
        // --- tree navigation -----------------------------------------------------------
        //
        // Every one of these takes a handle and returns plain data. A handle that does not
        // resolve yields `undefined` or an empty array rather than throwing: rule code is
        // arbitrary and may pass any number, and a thrown error there would be reported as
        // a rule bug when the real cause is a typo in a handle variable.

        let arena = Rc::clone(&self.arena);
        object.set(
            "kind",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow().kind(handle).map(ToOwned::to_owned)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "text",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow().text(handle).map(ToOwned::to_owned)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "isNamed",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow().is_named(handle)
            })?,
        )?;

        // Position is exposed as two primitives rather than as a `{line, column}` object.
        //
        // Building the object in Rust needs the `Ctx`, and a host function can neither
        // capture one — cloning a `Ctx` into a `'static` closure keeps the context alive
        // past `JS_FreeRuntime` and aborts the process on an unfreed-objects assertion —
        // nor take one as a parameter, because the returned object's lifetime cannot be
        // named inside a closure.
        //
        // A rule that wants the pair writes `{ line: ctx.line(n), column: ctx.column(n) }`,
        // which is a fair trade for two fewer ways to get the boundary wrong.
        let arena = Rc::clone(&self.arena);
        object.set(
            "line",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow().position(handle).map(|(line, _)| line)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "column",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow().position(handle).map(|(_, column)| column)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "parent",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow_mut().parent(handle)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "children",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow_mut().children(handle)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "namedChildren",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow_mut().named_children(handle)
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        object.set(
            "ancestors",
            Function::new(ctx.clone(), move |handle: Handle| {
                arena.borrow_mut().ancestors(handle)
            })?,
        )?;

        Ok(())
    }

    /// Resolving what an identifier refers to.
    fn install_bindings<'js>(&self, ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
        // --- binding resolution ----------------------------------------------------------
        //
        // The light semantic layer of §6.4. A rule matching `makeStyles(...)` on identifier
        // text alone is wrong twice: it misses `import { makeStyles as ms }`, and it fires
        // on a local `const makeStyles` that has nothing to do with the import.

        let arena = Rc::clone(&self.arena);
        let resolver = self.resolver.clone();
        object.set(
            "resolvesToImport",
            Function::new(
                ctx.clone(),
                move |handle: Handle, module: String, name: Opt<String>| {
                    let Some(resolver) = resolver.as_deref() else {
                        return false;
                    };
                    match arena.borrow().resolve_binding(handle, resolver) {
                        Some(Binding::Import {
                            module: from,
                            name: imported,
                        }) => {
                            from == module
                                && name.0.is_none_or(|wanted| match &imported {
                                    ImportedName::Named(actual) => *actual == wanted,
                                    ImportedName::Default => wanted == "default",
                                    ImportedName::Namespace => wanted == "*",
                                })
                        }
                        _ => false,
                    }
                },
            )?,
        )?;

        let arena = Rc::clone(&self.arena);
        let resolver = self.resolver.clone();
        object.set(
            "isImportedFrom",
            Function::new(ctx.clone(), move |handle: Handle, pattern: String| {
                let Some(resolver) = resolver.as_deref() else {
                    return false;
                };
                match arena.borrow().resolve_binding(handle, resolver) {
                    Some(Binding::Import { module, .. }) => glob_matches(&pattern, &module),
                    _ => false,
                }
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        let resolver = self.resolver.clone();
        object.set(
            "bindingKind",
            Function::new(ctx.clone(), move |handle: Handle| {
                let resolver = resolver.as_deref()?;
                arena
                    .borrow()
                    .resolve_binding(handle, resolver)
                    .map(|binding| binding.kind_str().to_owned())
            })?,
        )?;

        let arena = Rc::clone(&self.arena);
        let resolver = self.resolver.clone();
        object.set(
            "isShadowed",
            Function::new(ctx.clone(), move |handle: Handle| {
                resolver
                    .as_deref()
                    .is_some_and(|resolver| arena.borrow().is_shadowed(handle, resolver))
            })?,
        )?;

        Ok(())
    }

    /// Recording violations.
    fn install_reporting<'js>(&self, ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
        // --- reporting -------------------------------------------------------------------

        let arena = Rc::clone(&self.arena);
        let reports = Rc::clone(&self.reports);
        object.set(
            "report",
            // `Opt` rather than `Option`: an `Option` parameter still requires the caller
            // to pass something, so `ctx.report(node)` would fail on arity. `Opt` is what
            // makes the argument genuinely optional.
            Function::new(ctx.clone(), move |handle: Handle, message: Opt<String>| {
                // A report at an unresolvable handle is dropped rather than recorded at a
                // made-up position. Reporting at 1:1 would point a reader at an unrelated
                // line, which is worse than the rule appearing not to fire.
                if let Some((line, column)) = arena.borrow().position(handle) {
                    reports.borrow_mut().push(Report {
                        node: Some(handle),
                        line,
                        column,
                        message: message.0,
                    });
                }
            })?,
        )?;

        Ok(())
    }
}

/// Match a module specifier against a pattern where `*` stands for any run of characters.
///
/// Written out rather than pulled in, because the whole need is `@scope/*` and `*/themed`.
/// A glob crate would bring a dependency and a dialect — character classes, `**`, escapes —
/// for a surface this small.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return true;
    };
    if !text.starts_with(first) {
        return false;
    }

    let mut rest = &text[first.len()..];
    let segments: Vec<&str> = parts.collect();

    // No `*` at all: the pattern has to account for the whole specifier.
    if segments.is_empty() {
        return rest.is_empty();
    }

    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        // The final segment has to sit at the end, or `@scope/*` would match
        // `@scope/pkg/nested` on a pattern the author meant to be exact after the star.
        if index == segments.len() - 1 {
            return rest.ends_with(segment);
        }
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }

    // The pattern ended with `*`, so whatever is left is matched.
    true
}

#[cfg(test)]
mod tests {
    use lanekeep_lang::Language;
    use lanekeep_lang_js::TypeScript;

    use super::*;
    use crate::{Limits, Sandbox};

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&TypeScript.grammar())
            .expect("grammar loads");
        parser.parse(source, None).expect("parses")
    }

    fn host(source: &str) -> HostContext {
        HostContext::new(parse(source), source.to_owned(), "src/example.ts")
            .with_resolver(Arc::new(lanekeep_lang_js::binding::JsBindingResolver))
    }

    /// A host with no resolver, for the degraded path.
    fn host_without_resolver(source: &str) -> HostContext {
        HostContext::new(parse(source), source.to_owned(), "src/example.ts")
    }

    /// The handle of the last identifier reading `name`, the way a query capture arrives.
    fn handle_of(host: &HostContext, name: &str) -> Handle {
        let mut arena = host.arena().borrow_mut();
        let source = arena.source().to_owned();

        let path = {
            let mut best: Option<tree_sitter::Node<'_>> = None;
            let mut stack = vec![arena.tree().root_node()];
            while let Some(node) = stack.pop() {
                if node.kind() == "identifier"
                    && source.get(node.byte_range()) == Some(name)
                    && best.is_none_or(|b| node.start_byte() > b.start_byte())
                {
                    best = Some(node);
                }
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
            arena
                .path_of(best.unwrap_or_else(|| panic!("no identifier `{name}`")))
                .expect("has a path")
        };

        arena.intern_path(path).expect("interns")
    }

    /// Evaluate rule-shaped code with `ctx` in scope.
    fn run<T>(host: &HostContext, code: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js> + Default,
    {
        let sandbox = Sandbox::with_limits(Limits::default()).expect("sandbox builds");
        sandbox.eval_with_host(host, code).expect("evaluates")
    }

    #[test]
    fn exposes_the_file_path_and_text() {
        let host = host("const x = 1;");
        assert_eq!(run::<String>(&host, "ctx.filePath"), "src/example.ts");
        assert_eq!(run::<String>(&host, "ctx.fileText"), "const x = 1;");
    }

    #[test]
    fn navigates_from_the_root() {
        let host = host("const x = 1;\nconst y = 2;");
        assert_eq!(run::<String>(&host, "ctx.kind(ctx.root)"), "program");
        assert_eq!(run::<u32>(&host, "ctx.namedChildren(ctx.root).length"), 2);
        assert_eq!(
            run::<String>(&host, "ctx.kind(ctx.namedChildren(ctx.root)[0])"),
            "lexical_declaration"
        );
    }

    #[test]
    fn reads_text_and_position() {
        let host = host("const x = 1;\nconst y = 2;");
        assert_eq!(
            run::<String>(&host, "ctx.text(ctx.namedChildren(ctx.root)[1])"),
            "const y = 2;"
        );
        assert_eq!(
            run::<u32>(&host, "ctx.line(ctx.namedChildren(ctx.root)[1])"),
            2
        );
        assert_eq!(
            run::<u32>(&host, "ctx.column(ctx.namedChildren(ctx.root)[1])"),
            1
        );
    }

    #[test]
    fn walks_up_and_back_down() {
        let host = host("const x = 1;");
        assert!(run::<bool>(
            &host,
            "const d = ctx.namedChildren(ctx.root)[0];
             const inner = ctx.namedChildren(d)[0];
             ctx.parent(inner) === d && ctx.parent(d) === ctx.root"
        ));
    }

    #[test]
    fn handles_compare_equal_for_the_same_node() {
        // Rules use `===` on handles. If the same node interned twice produced two
        // numbers, a rule asking "is this capture the same node as that one" would
        // silently always say no.
        let host = host("const x = 1;");
        assert!(run::<bool>(
            &host,
            "ctx.namedChildren(ctx.root)[0] === ctx.namedChildren(ctx.root)[0]"
        ));
    }

    #[test]
    fn ancestors_end_at_the_root() {
        let host = host("function f() { return 1; }");
        assert!(run::<bool>(
            &host,
            "const fn = ctx.namedChildren(ctx.root)[0];
             const body = ctx.namedChildren(fn).at(-1);
             const stmt = ctx.namedChildren(body)[0];
             const a = ctx.ancestors(stmt);
             a[0] === body && a.at(-1) === ctx.root"
        ));
    }

    #[test]
    fn named_children_omits_anonymous_tokens() {
        let host = host("const x = 1;");
        assert!(run::<bool>(
            &host,
            "const d = ctx.namedChildren(ctx.root)[0];
             ctx.children(d).length > ctx.namedChildren(d).length"
        ));
    }

    #[test]
    fn an_unresolvable_handle_returns_nothing_rather_than_throwing() {
        // Rule code is arbitrary and will pass stale or invented numbers. Throwing here
        // would be reported as a rule bug when the cause is a mistyped variable.
        let host = host("const x = 1;");
        assert!(run::<bool>(
            &host,
            "ctx.kind(9999) === undefined &&
             ctx.text(9999) === undefined &&
             ctx.line(9999) === undefined &&
             ctx.column(9999) === undefined &&
             ctx.parent(9999) === undefined &&
             ctx.children(9999).length === 0 &&
             ctx.ancestors(9999).length === 0"
        ));
    }

    // --- reporting ---------------------------------------------------------------------

    #[test]
    fn records_a_report_at_the_node_position() {
        let host = host("const x = 1;\nconst y = 2;");
        let _: () = run(&host, "ctx.report(ctx.namedChildren(ctx.root)[1])");

        let reports = host.take_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].line, 2);
        assert_eq!(reports[0].column, 1);
        assert_eq!(reports[0].message, None);
    }

    #[test]
    fn records_an_overriding_message() {
        let host = host("const x = 1;");
        let _: () = run(&host, "ctx.report(ctx.root, 'something specific')");

        let reports = host.take_reports();
        assert_eq!(reports[0].message.as_deref(), Some("something specific"));
    }

    #[test]
    fn records_every_report_in_order() {
        let host = host("const a = 1;\nconst b = 2;\nconst c = 3;");
        let _: () = run(
            &host,
            "for (const d of ctx.namedChildren(ctx.root)) { ctx.report(d, ctx.text(d)); }",
        );

        let reports = host.take_reports();
        let lines: Vec<u32> = reports.iter().map(|r| r.line).collect();
        assert_eq!(lines, [1, 2, 3]);
        assert_eq!(reports[2].message.as_deref(), Some("const c = 3;"));
    }

    #[test]
    fn a_report_at_an_unresolvable_handle_is_dropped() {
        // Recording it at 1:1 would point a reader at an unrelated line, which is worse
        // than the rule appearing not to have fired.
        let host = host("const x = 1;");
        let _: () = run(&host, "ctx.report(9999)");
        assert!(host.take_reports().is_empty());
    }

    #[test]
    fn taking_reports_empties_the_context() {
        let host = host("const x = 1;");
        let _: () = run(&host, "ctx.report(ctx.root)");

        assert_eq!(host.take_reports().len(), 1);
        assert!(
            host.take_reports().is_empty(),
            "reports must not be reported twice"
        );
    }

    #[test]
    fn a_rule_that_throws_still_leaves_earlier_reports() {
        // A handler may report several times and then hit a bug. What it already found is
        // still true, and discarding it would make the failure harder to diagnose rather
        // than easier.
        let host = host("const x = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("sandbox builds");
        let result: Result<(), _> =
            sandbox.eval_with_host(&host, "ctx.report(ctx.root); throw new Error('later')");

        assert!(result.is_err());
        assert_eq!(host.take_reports().len(), 1);
    }

    #[test]
    fn navigation_is_bounded_by_the_rule_timeout() {
        // The host API is reachable from arbitrary rule code, so a loop calling into it
        // must still be interruptible. Nothing here may take a lock the interrupt handler
        // needs, or the run would hang instead of failing.
        let host = host("const x = 1;");
        let sandbox = Sandbox::with_limits(
            Limits::default().with_rule_timeout(std::time::Duration::from_millis(120)),
        )
        .expect("sandbox builds");

        let result: Result<(), _> = sandbox.eval_with_host(
            &host,
            "for (;;) { ctx.kind(ctx.root); ctx.children(ctx.root); }",
        );
        assert!(
            matches!(result, Err(crate::SandboxError::RuleTimeout { .. })),
            "expected a timeout, got {result:?}"
        );
    }

    #[test]
    fn the_sandbox_still_withholds_everything_it_did_before() {
        // Installing `ctx` must not have widened the surface as a side effect.
        let host = host("const x = 1;");
        assert!(run::<bool>(
            &host,
            "typeof Date === 'undefined' &&
             typeof performance === 'undefined' &&
             typeof Math.random === 'undefined' &&
             typeof fetch === 'undefined' &&
             typeof process === 'undefined'"
        ));
    }

    // --- binding resolution -------------------------------------------------------------

    #[test]
    fn resolves_an_import_through_its_alias() {
        // The case §6.4 exists for. A rule looking for `makeStyles` has to find `ms`.
        let host = host("import { makeStyles as ms } from '@rneui/themed';\nms();");
        let handle = handle_of(&host, "ms");

        assert!(run::<bool>(
            &host,
            &format!("ctx.resolvesToImport({handle}, '@rneui/themed', 'makeStyles')")
        ));
        assert!(!run::<bool>(
            &host,
            &format!("ctx.resolvesToImport({handle}, 'somewhere-else', 'makeStyles')")
        ));
        assert!(!run::<bool>(
            &host,
            &format!("ctx.resolvesToImport({handle}, '@rneui/themed', 'notThatOne')")
        ));
    }

    #[test]
    fn a_local_declaration_does_not_resolve_to_the_import_it_shadows() {
        // The false positive this prevents: a rule keyed on the name firing on a local
        // that has nothing to do with the import.
        let host = host(
            "import { makeStyles } from '@rneui/themed';\n\
             function f() { const makeStyles = () => {}; return makeStyles(); }",
        );
        let handle = handle_of(&host, "makeStyles");

        assert!(!run::<bool>(
            &host,
            &format!("ctx.resolvesToImport({handle}, '@rneui/themed', 'makeStyles')")
        ));
        assert_eq!(
            run::<String>(&host, &format!("ctx.bindingKind({handle})")),
            "const"
        );
        assert!(run::<bool>(&host, &format!("ctx.isShadowed({handle})")));
    }

    #[test]
    fn omitting_the_name_matches_any_export_of_the_module() {
        let host = host("import { a } from 'm';\na();");
        let handle = handle_of(&host, "a");
        assert!(run::<bool>(
            &host,
            &format!("ctx.resolvesToImport({handle}, 'm')")
        ));
    }

    #[test]
    fn matches_a_module_by_glob() {
        let host = host("import { a } from '@scope/pkg';\na();");
        let handle = handle_of(&host, "a");

        assert!(run::<bool>(
            &host,
            &format!("ctx.isImportedFrom({handle}, '@scope/*')")
        ));
        assert!(run::<bool>(
            &host,
            &format!("ctx.isImportedFrom({handle}, '*/pkg')")
        ));
        assert!(run::<bool>(
            &host,
            &format!("ctx.isImportedFrom({handle}, '@scope/pkg')")
        ));
        assert!(!run::<bool>(
            &host,
            &format!("ctx.isImportedFrom({handle}, '@other/*')")
        ));
    }

    #[test]
    fn reports_binding_kinds() {
        for (source, name, expected) in [
            ("import { a } from 'm';\na();", "a", "import"),
            ("const b = 1;\nb;", "b", "const"),
            ("let c = 1;\nc;", "c", "let"),
            ("function d() {}\nd();", "d", "function"),
            ("class E {}\nnew E();", "E", "class"),
            ("function f(p) { return p; }", "p", "param"),
        ] {
            let host = host(source);
            let handle = handle_of(&host, name);
            assert_eq!(
                run::<String>(&host, &format!("ctx.bindingKind({handle})")),
                expected,
                "for {name} in {source}"
            );
        }
    }

    #[test]
    fn an_undeclared_name_has_no_binding_kind() {
        let host = host("globalThing();");
        let handle = handle_of(&host, "globalThing");
        assert!(run::<bool>(
            &host,
            &format!("ctx.bindingKind({handle}) === undefined")
        ));
    }

    #[test]
    fn without_a_resolver_nothing_resolves_rather_than_throwing() {
        // A language with no resolver should make rules see "nothing resolves", not a
        // TypeError blamed on the rule for calling a function that is missing.
        let host = host_without_resolver("import { a } from 'm';\na();");
        assert!(run::<bool>(
            &host,
            "ctx.resolvesToImport(0, 'm', 'a') === false &&
             ctx.isImportedFrom(0, '*') === false &&
             ctx.isShadowed(0) === false &&
             ctx.bindingKind(0) === undefined"
        ));
    }

    #[test]
    fn glob_matching_handles_the_shapes_that_appear_in_rules() {
        assert!(glob_matches("m", "m"));
        assert!(!glob_matches("m", "mm"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("@scope/*", "@scope/pkg"));
        assert!(!glob_matches("@scope/*", "@other/pkg"));
        assert!(glob_matches("*/themed", "@rneui/themed"));
        assert!(!glob_matches("*/themed", "@rneui/other"));
        assert!(glob_matches("@a/*/c", "@a/b/c"));
        assert!(!glob_matches("@a/*/c", "@a/b/d"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "x"));
    }

    #[test]
    fn navigation_stays_lazy() {
        // The arena must not have materialized the tree just because `ctx` exists.
        let host = host("const a = 1; const b = 2; function c() { return [1,2,3] }");
        assert!(
            host.arena().borrow().is_empty(),
            "nothing should be interned yet"
        );

        let _: () = run(&host, "ctx.kind(ctx.root)");
        assert!(
            host.arena().borrow().is_empty(),
            "reading the root's kind should not intern anything new"
        );
    }
}
