//! The `ctx` object rule code receives.
//!
//! This is the trust boundary made concrete. Everything a rule can do to the outside world
//! it does through a function installed here; anything not installed does not exist. Adding
//! to this surface widens what a rule may reach and bumps the host API version that feeds
//! the cache key.
//!
//! # What is here so far
//!
//! Reporting, tree navigation, binding resolution, facts, and tracked file reads.
//!
//! # Two contexts, not one
//!
//! [`HostContext`] serves the per-file pass and [`ReduceContext`] serves the reduce phase,
//! and neither is a subset of the other by accident:
//!
//! - `emitFact` exists only in the per-file context. A reduce phase that could emit facts
//!   could feed itself, and there is no second pass for the result to reach.
//! - `facts` and `files` exist only in the reduce context. If `check` could read the corpus,
//!   a file's result would depend on files other than itself, and caching that result
//!   against its own content would be unsound. This is not a stylistic split — it is what
//!   makes per-file cache entries mean anything.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Object, Value};

use lanekeep_lang::binding::{Binding, BindingResolver, ImportedName};

use crate::files::FileAccess;
use crate::nodes::{Handle, NodeArena};

/// The version of the `ctx` surface this build exposes.
///
/// **Bump this whenever `ctx` gains, loses or changes a function.** It is a cache-key input,
/// and it has to be: a result computed by a build where `ctx.readFile` did not exist is not
/// a valid result for a build where it does — the rule could not have called something that
/// was not there, so its cached verdict was reached without evidence it would have used.
///
/// Nothing detects this automatically. A function added without bumping it serves stale
/// results silently, which is the failure mode the whole cache design is arranged against.
///
/// History:
/// - `1` — reporting, navigation, binding resolution, `emitFact`, `readFile`, `fileExists`.
pub const HOST_API_VERSION: u32 = 1;

/// A fact a rule emitted, before the engine attaches the file and rule it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedFact {
    /// The `kind` field, lifted out so the reduce phase can filter without parsing.
    pub kind: String,
    /// The whole fact, as `JSON.stringify` rendered it. Always a JSON object.
    pub data: String,
}

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
    facts: Rc<RefCell<Vec<EmittedFact>>>,
    file_path: Rc<str>,
    resolver: Option<Arc<dyn BindingResolver>>,
    files: Option<Rc<FileAccess>>,
}

impl std::fmt::Debug for HostContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostContext")
            .field("file_path", &self.file_path)
            .field("interned_nodes", &self.arena.borrow().len())
            .field("reports", &self.reports.borrow().len())
            .field("facts", &self.facts.borrow().len())
            .field("has_resolver", &self.resolver.is_some())
            .field("has_file_access", &self.files.is_some())
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
            facts: Rc::new(RefCell::new(Vec::new())),
            file_path: Rc::from(file_path),
            resolver: None,
            files: None,
        }
    }

    /// Attach whatever resolver a language provides, if any.
    #[must_use]
    pub fn with_resolver_from(self, language: &dyn lanekeep_lang::Language) -> Self {
        match language.resolver() {
            Some(resolver) => self.with_resolver(resolver),
            None => self,
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

    /// Allow tracked reads of other files in the project.
    ///
    /// Without one, `readFile` and `fileExists` are absent rather than present-and-failing.
    /// A rule reaching for them then gets a `TypeError` naming the function, which is the
    /// truthful answer — where a stub returning `undefined` would look like an empty project
    /// and produce a rule that silently checks nothing.
    #[must_use]
    pub fn with_file_access(mut self, files: Rc<FileAccess>) -> Self {
        self.files = Some(files);
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

    /// Take everything emitted so far, in emission order, leaving the context empty.
    #[must_use]
    pub fn take_facts(&self) -> Vec<EmittedFact> {
        std::mem::take(&mut self.facts.borrow_mut())
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
        self.install_facts(ctx, &object)?;
        self.install_reads(ctx, &object)?;

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

    /// Emitting facts for the reduce phase.
    fn install_facts<'js>(&self, ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
        // --- facts -----------------------------------------------------------------------

        let facts = Rc::clone(&self.facts);
        object.set(
            "emitFact",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, fact: Value<'js>| -> rquickjs::Result<()> {
                    let Some(fact_object) = fact.as_object() else {
                        return Err(throw(&ctx, "ctx.emitFact expects an object"));
                    };

                    // `kind` is what `ctx.facts('export')` filters on. A fact without one
                    // could never be retrieved, so emitting it is always a mistake — and a
                    // silent one, since the rule would look like it was working right up
                    // until `reduce` found nothing.
                    let kind = match fact_object.get::<_, String>("kind") {
                        Ok(kind) if !kind.is_empty() => kind,
                        _ => {
                            return Err(throw(
                                &ctx,
                                "ctx.emitFact requires a non-empty string `kind` — it is what \
                                 ctx.facts(kind) selects on, so a fact without one can never \
                                 be read back",
                            ));
                        }
                    };

                    // `JSON.stringify` is the definition of serializable here rather than a
                    // check alongside it, so what a rule can emit is exactly what it could
                    // write to a file — and exactly what a cache entry can hold. A cycle
                    // throws from inside stringify and surfaces as the rule's error.
                    let Some(json) = ctx.json_stringify(fact)? else {
                        return Err(throw(
                            &ctx,
                            "ctx.emitFact could not serialize this fact — facts are cached, \
                             so they have to survive JSON",
                        ));
                    };

                    facts.borrow_mut().push(EmittedFact {
                        kind,
                        data: json.to_string()?,
                    });
                    Ok(())
                },
            )?,
        )?;

        Ok(())
    }

    /// Reading other files in the project.
    fn install_reads<'js>(&self, ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
        // --- tracked reads -----------------------------------------------------------------

        let Some(files) = self.files.clone() else {
            return Ok(());
        };

        let reader = Rc::clone(&files);
        object.set(
            "readFile",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, path: String| -> rquickjs::Result<Option<String>> {
                    // A refusal throws; an absence returns `undefined`. The distinction is
                    // the point: "not there" is an ordinary answer a rule should handle,
                    // and "you tried to leave the project" is a bug in the rule.
                    reader.read(&path).map_err(|e| throw(&ctx, &e.to_string()))
                },
            )?,
        )?;

        let reader = Rc::clone(&files);
        object.set(
            "fileExists",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, path: String| -> rquickjs::Result<bool> {
                    reader
                        .exists(&path)
                        .map_err(|e| throw(&ctx, &e.to_string()))
                },
            )?,
        )?;

        Ok(())
    }
}

/// A violation a reduce phase asked for.
///
/// Carries a file of its own: a cross-file rule reports at the site the fact came from,
/// which is by definition not "the file being checked" — there isn't one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceReport {
    /// Path of the file to report against, as the rule gave it.
    pub file: String,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// A message overriding the rule card's, when the rule supplied one.
    pub message: Option<String>,
}

/// One fact as the reduce phase will see it: payload plus the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceFact {
    /// The `kind`, for filtering.
    pub kind: String,
    /// The payload as JSON, with `file` merged in.
    pub json: String,
}

/// Host state for the reduce phase of one rule.
///
/// Built per rule rather than per run, because a rule sees only its own facts — letting one
/// rule read another's would turn a private payload shape into a contract between rules.
#[derive(Debug, Clone)]
pub struct ReduceContext {
    files: Rc<[String]>,
    facts: Rc<[ReduceFact]>,
    reports: Rc<RefCell<Vec<ReduceReport>>>,
}

impl ReduceContext {
    /// Build a context over one rule's facts and the discovered file list.
    #[must_use]
    pub fn new(files: Vec<String>, facts: Vec<ReduceFact>) -> Self {
        Self {
            files: files.into(),
            facts: facts.into(),
            reports: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Take everything reported so far, leaving the context empty.
    #[must_use]
    pub fn take_reports(&self) -> Vec<ReduceReport> {
        std::mem::take(&mut self.reports.borrow_mut())
    }

    /// Build the `ctx` object the reduce phase receives.
    ///
    /// # Errors
    ///
    /// Returns an engine error if a property cannot be defined, which would mean a broken
    /// build rather than anything about a rule.
    pub fn build<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
        let object = Object::new(ctx.clone())?;

        object.set("files", &*self.files)?;

        // No `filePath`, no `root`, no `fileText`, no navigation. There is no file and no
        // tree here — that is invariant 1, and a context that offered them would be lying
        // about what the phase can see.

        let facts = Rc::clone(&self.facts);
        object.set(
            "facts",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, kind: Opt<String>| -> rquickjs::Result<Value<'js>> {
                    // One array, one parse. Handing back N separately-parsed objects would
                    // cost N crossings for a phase whose whole job is bulk work.
                    let wanted = kind.0;
                    let mut json = String::from("[");
                    for fact in facts
                        .iter()
                        .filter(|f| wanted.as_ref().is_none_or(|k| *k == f.kind))
                    {
                        if json.len() > 1 {
                            json.push(',');
                        }
                        json.push_str(&fact.json);
                    }
                    json.push(']');

                    ctx.json_parse(json)
                },
            )?,
        )?;

        let reports = Rc::clone(&self.reports);
        object.set(
            "report",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      at: Value<'js>,
                      message: Opt<String>|
                      -> rquickjs::Result<()> {
                    let Some(at) = at.as_object() else {
                        return Err(throw(
                            &ctx,
                            "ctx.report in a reduce phase expects { file, line, column } — \
                             there is no parse tree here, so there are no nodes to report at",
                        ));
                    };

                    let (Ok(file), Ok(line), Ok(column)) = (
                        at.get::<_, String>("file"),
                        at.get::<_, u32>("line"),
                        at.get::<_, u32>("column"),
                    ) else {
                        return Err(throw(
                            &ctx,
                            "ctx.report in a reduce phase needs `file`, `line` and `column` — \
                             emit them on the fact during the per-file pass, where the node \
                             positions are still available",
                        ));
                    };

                    reports.borrow_mut().push(ReduceReport {
                        file,
                        line,
                        column,
                        message: message.0,
                    });
                    Ok(())
                },
            )?,
        )?;

        Ok(object)
    }
}

/// Throw a `TypeError` carrying a message meant for a rule author.
///
/// A real `Error` object, not a thrown string: the sandbox reports a thrown string by
/// telling the author to throw an `Error` instead, which is sound advice about their code
/// and nonsense when the host is the one that threw. It would also lose the message.
fn throw(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    rquickjs::Exception::throw_type(ctx, message)
}

/// Merge a `file` into a fact's serialized payload.
///
/// Textual because the input is `JSON.stringify` output — always an object, always with the
/// braces at the ends — so this is one allocation rather than a parse, an insert and a
/// re-serialize, per fact, for a phase whose input is the whole corpus.
///
/// `file` goes **last** on purpose. A rule is free to put its own `file` in a fact, and JSON
/// parsing takes the last of duplicate keys — so the host's value wins, and a rule cannot
/// misattribute a violation by shadowing it.
#[must_use]
pub fn merge_file(data: &str, file: &str) -> String {
    let inner = data
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or_default()
        .trim();

    let mut out = String::with_capacity(data.len() + file.len() + 12);
    out.push('{');
    if !inner.is_empty() {
        out.push_str(inner);
        out.push(',');
    }
    out.push_str("\"file\":");
    escape_json_string(file, &mut out);
    out.push('}');
    out
}

/// Append `text` as a quoted JSON string.
fn escape_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Writing rather than formatting into a temporary: this runs once per
                // character of every fact's file path.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
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

    // --- facts -------------------------------------------------------------------------

    /// Run source with a per-file `ctx` and return the facts it emitted.
    fn emitted(source: &str) -> Vec<EmittedFact> {
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        sandbox
            .eval_with_host::<()>(&host, source)
            .expect("evaluates");
        host.take_facts()
    }

    #[test]
    fn a_fact_is_captured_with_its_kind_and_payload() {
        let facts = emitted("ctx.emitFact({ kind: 'export', symbol: 'parse' })");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, "export");
        assert!(
            facts[0].data.contains(r#""symbol":"parse""#),
            "{:?}",
            facts[0]
        );
    }

    #[test]
    fn facts_are_kept_in_emission_order() {
        // Within a file, the order a rule emitted in is the only order it can have meant.
        let facts = emitted(
            "ctx.emitFact({ kind: 'a', n: 1 }); \
             ctx.emitFact({ kind: 'b', n: 2 }); \
             ctx.emitFact({ kind: 'a', n: 3 });",
        );
        assert_eq!(
            facts.iter().map(|f| f.kind.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "a"]
        );
    }

    #[test]
    fn a_fact_without_a_kind_is_rejected() {
        // Not dropped. A fact with no kind can never be selected by `ctx.facts(kind)`, so
        // accepting it would leave the rule looking correct until reduce found nothing.
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let error = sandbox
            .eval_with_host::<()>(&host, "ctx.emitFact({ symbol: 'parse' })")
            .expect_err("is rejected");
        assert!(error.to_string().contains("kind"), "{error}");
        assert!(host.take_facts().is_empty());
    }

    #[test]
    fn a_fact_with_an_empty_kind_is_rejected() {
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        assert!(
            sandbox
                .eval_with_host::<()>(&host, "ctx.emitFact({ kind: '' })")
                .is_err()
        );
    }

    #[test]
    fn a_fact_that_is_not_an_object_is_rejected() {
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        for bad in ["'export'", "42", "null", "undefined"] {
            assert!(
                sandbox
                    .eval_with_host::<()>(&host, &format!("ctx.emitFact({bad})"))
                    .is_err(),
                "`{bad}` should not be emittable"
            );
        }
    }

    #[test]
    fn a_cyclic_fact_is_rejected_rather_than_hanging() {
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let error = sandbox
            .eval_with_host::<()>(
                &host,
                "const f = { kind: 'x' }; f.self = f; ctx.emitFact(f)",
            )
            .expect_err("is rejected");
        // JSON.stringify's own error. Letting it through unchanged is right: it says
        // "circular structure", which is more specific than anything this layer knows.
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn the_reduce_surface_is_absent_from_the_per_file_context() {
        // The invariant that makes per-file cache entries mean anything: a file's result
        // must not depend on any other file.
        let host = host("const a = 1;");
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        for absent in ["ctx.facts", "ctx.files"] {
            let present: bool = sandbox
                .eval_with_host(&host, &format!("{absent} !== undefined"))
                .expect("evaluates");
            assert!(
                !present,
                "`{absent}` must not exist during the per-file pass"
            );
        }
    }

    // --- the reduce context ------------------------------------------------------------

    fn reduce_fact(kind: &str, json: &str) -> ReduceFact {
        ReduceFact {
            kind: kind.to_owned(),
            json: json.to_owned(),
        }
    }

    /// The reduce phase's budget in these tests. Generous: nothing here is timing-sensitive.
    fn budget() -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }

    #[test]
    fn facts_come_back_as_objects() {
        let context = ReduceContext::new(
            vec!["a.ts".to_owned()],
            vec![reduce_fact(
                "export",
                r#"{"kind":"export","symbol":"parse","file":"a.ts"}"#,
            )],
        );
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let symbol: String = sandbox
            .eval_with_reduce_host(&context, "ctx.facts('export')[0].symbol", budget())
            .expect("evaluates");
        assert_eq!(symbol, "parse");
    }

    #[test]
    fn facts_filter_by_kind_and_default_to_everything() {
        let context = ReduceContext::new(
            vec![],
            vec![
                reduce_fact("export", r#"{"kind":"export","file":"a.ts"}"#),
                reduce_fact("import", r#"{"kind":"import","file":"b.ts"}"#),
                reduce_fact("export", r#"{"kind":"export","file":"c.ts"}"#),
            ],
        );
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let counts: Vec<i32> = sandbox
            .eval_with_reduce_host(
                &context,
                "[ctx.facts('export').length, ctx.facts('import').length, ctx.facts().length]",
                budget(),
            )
            .expect("evaluates");
        assert_eq!(counts, vec![2, 1, 3]);
    }

    #[test]
    fn an_unknown_kind_yields_an_empty_array_rather_than_undefined() {
        // So `for (const f of ctx.facts('nope'))` is a no-op instead of a TypeError.
        let context = ReduceContext::new(vec![], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let length: i32 = sandbox
            .eval_with_reduce_host(&context, "ctx.facts('nope').length", budget())
            .expect("evaluates");
        assert_eq!(length, 0);
    }

    #[test]
    fn the_file_list_is_visible() {
        let context = ReduceContext::new(vec!["a.ts".to_owned(), "b.ts".to_owned()], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let files: Vec<String> = sandbox
            .eval_with_reduce_host(&context, "ctx.files", budget())
            .expect("evaluates");
        assert_eq!(files, vec!["a.ts".to_owned(), "b.ts".to_owned()]);
    }

    #[test]
    fn reporting_names_a_file_of_its_own() {
        let context = ReduceContext::new(vec![], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        sandbox
            .eval_with_reduce_host::<()>(
                &context,
                "ctx.report({ file: 'b.ts', line: 4, column: 2 }, 'unused export')",
                budget(),
            )
            .expect("evaluates");
        assert_eq!(
            context.take_reports(),
            vec![ReduceReport {
                file: "b.ts".to_owned(),
                line: 4,
                column: 2,
                message: Some("unused export".to_owned()),
            }]
        );
    }

    #[test]
    fn the_message_is_optional() {
        let context = ReduceContext::new(vec![], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        sandbox
            .eval_with_reduce_host::<()>(
                &context,
                "ctx.report({ file: 'b.ts', line: 1, column: 1 })",
                budget(),
            )
            .expect("evaluates");
        let reports = context.take_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].message, None);
    }

    #[test]
    fn reporting_without_a_position_is_rejected() {
        // A cross-file violation with no site is unactionable, and defaulting to 1:1 would
        // point a reader at an unrelated line.
        let context = ReduceContext::new(vec![], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        for bad in [
            "ctx.report({ file: 'b.ts' })",
            "ctx.report({ line: 1, column: 1 })",
            "ctx.report(3)",
            "ctx.report('b.ts')",
        ] {
            let error = sandbox
                .eval_with_reduce_host::<()>(&context, bad, budget())
                .expect_err("is rejected");
            assert!(!error.to_string().is_empty(), "`{bad}` should be rejected");
        }
        assert!(context.take_reports().is_empty());
    }

    #[test]
    fn the_per_file_surface_is_absent_from_the_reduce_context() {
        // Invariant 1: the reduce phase never touches parse trees. A context that offered
        // navigation would be lying about what the phase can see.
        let context = ReduceContext::new(vec![], vec![]);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        for absent in [
            "ctx.emitFact",
            "ctx.text",
            "ctx.kind",
            "ctx.parent",
            "ctx.namedChildren",
            "ctx.filePath",
            "ctx.fileText",
            "ctx.root",
        ] {
            let present: bool = sandbox
                .eval_with_reduce_host(&context, &format!("{absent} !== undefined"), budget())
                .expect("evaluates");
            assert!(
                !present,
                "`{absent}` must not exist during the reduce phase"
            );
        }
    }

    // --- merging the file into a payload -------------------------------------------------

    #[test]
    fn merge_file_adds_the_field() {
        assert_eq!(
            merge_file(r#"{"kind":"export"}"#, "src/a.ts"),
            r#"{"kind":"export","file":"src/a.ts"}"#
        );
    }

    #[test]
    fn merge_file_handles_an_empty_payload() {
        assert_eq!(merge_file("{}", "a.ts"), r#"{"file":"a.ts"}"#);
    }

    #[test]
    fn merge_file_overrides_a_file_the_rule_supplied() {
        // Last duplicate key wins in JSON, so the host's value is the one that survives —
        // a rule cannot misattribute a violation to a file it did not come from.
        let merged = merge_file(r#"{"kind":"export","file":"lies.ts"}"#, "truth.ts");
        assert!(merged.ends_with(r#""file":"truth.ts"}"#), "{merged}");

        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let file: String = sandbox
            .eval(&format!("JSON.parse({merged:?}).file"))
            .expect("parses");
        assert_eq!(file, "truth.ts");
    }

    #[test]
    fn merge_file_escapes_the_path() {
        // A path is untrusted input as far as this function is concerned: it comes from the
        // filesystem, and a quote in it would otherwise produce a payload that no longer
        // parses — or worse, one that parses into something else.
        let awkward = "a\"b\\c\nd.ts";
        let merged = merge_file("{}", awkward);
        let sandbox = Sandbox::with_limits(Limits::default()).expect("builds");
        let file: String = sandbox
            .eval(&format!("JSON.parse({merged:?}).file"))
            .expect("parses");
        assert_eq!(file, awkward);
    }
}
