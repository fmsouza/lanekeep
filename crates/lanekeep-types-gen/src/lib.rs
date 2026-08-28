//! Renders `packages/lanekeep/index.d.ts` from `crates/lanekeep-wasm/wit/world.wit`.
//!
//! The WIT world is the authoritative description of the host API every rule reaches: the
//! component rules bind against it, and the TypeScript rules reach the same functions in a
//! camelCase spelling under QuickJS. This crate keeps the published TypeScript definitions
//! honest about that by rendering the world's `types` interface into the QuickJS-shaped
//! `index.d.ts`, so the definitions can no longer drift from the world.
//!
//! # What is derived, and what is not
//!
//! Two things genuinely drift as the world grows, and both are derived here:
//!
//! - the `BindingKind` union, rendered from the `binding-kind` enum; and
//! - the `RuleContext` / `ReduceContext` member lists, rendered from the `check-context` /
//!   `reduce-context` resources.
//!
//! Everything else is either a record whose QuickJS shape is a fixed convention (`Node`,
//! `Match`, `Fix`, the card and location records), or an authoring type with no WIT source at
//! all (`Rule`, `Config`, `defineRule`, `defineConfig`, the gates and fact shapes). Those are
//! fixed templates, and the only thing the world can do to them is fail this renderer's
//! expectations loudly rather than silently fall out of step.
//!
//! # Why a renderer rather than `jco types`
//!
//! `jco types` describes the *component* boundary — `list<node>` as `Uint32Array`, `result`
//! as a `{ tag }` union, resources as classes with `readFile(): string | undefined` — which is
//! not the surface a TypeScript rule author reaches under QuickJS. That surface is an interface
//! with camelCase methods, `Match` as a `Record`, `Node` as a branded number, and `report`
//! taking a `ReportOptions` object. The mapping between the two is the point of this crate, and
//! it is hand-written here rather than a second hand-written description that can drift.

// A generator over a fixed WIT file: a malformed world or a missing declaration is a programmer
// error, and panicking with a message that names the missing item is the actionable failure — the
// same contract `crates/lanekeep-rules/build.rs` states. The workspace `[lints]` forbid these, so
// they are relaxed here, not silenced.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a malformed world or missing declaration is a programmer error; the panic names it"
)]

use std::fmt::Write as _;

use wit_parser::{Function, FunctionKind, Interface, Resolve, Type, TypeDefKind, World, WorldItem};

/// The path the world is parsed under. Only used in diagnostics.
const WORLD_PATH: &str = "world.wit";

/// Render the TypeScript authoring definitions for the given `world.wit` source.
///
/// The input is the complete text of `crates/lanekeep-wasm/wit/world.wit`; the output is the
/// complete text of `packages/lanekeep/index.d.ts`.
pub fn render_index_dts(wit: &str) -> String {
    let resolve = parse(wit);
    let world = world(&resolve);
    let interface = types_interface(&resolve, world);

    let binding_kinds = enum_cases(&resolve, interface, "binding-kind");
    let check_context = resource_methods(interface, "check-context");
    let reduce_context = resource_methods(interface, "reduce-context");

    // Each block ends with a newline; joining with `"\n"` leaves one blank line between
    // declarations, matching the hand-written file this replaces.
    let blocks = [
        HEADER.to_owned(),
        render_node(),
        render_binding_kind(&binding_kinds),
        render_language_id(),
        render_severity(),
        render_match(),
        render_rule_card(),
        render_gates(),
        render_fix(),
        render_report_options(),
        render_fact(),
        render_emitted_fact(),
        render_node_location(),
        render_context(&resolve, "RuleContext", &check_context, true),
        render_reduce_location(),
        render_context(&resolve, "ReduceContext", &reduce_context, false),
        render_rule(),
        render_config(),
        render_define_rule(),
        render_define_config(),
    ];
    blocks.join("\n")
}

/// Parse the world, failing loudly rather than guessing at a partial resolve.
fn parse(wit: &str) -> Resolve {
    let mut resolve = Resolve::new();
    resolve
        .push_str(WORLD_PATH, wit)
        .expect("`world.wit` parses");
    resolve
}

/// The single world this file declares: `rule`.
fn world(resolve: &Resolve) -> &World {
    resolve
        .worlds
        .iter()
        .find(|(_, world)| world.name == "rule")
        .map(|(_, world)| world)
        .expect("the `rule` world is present")
}

/// The `types` interface the world imports, which holds every type and context this renderer
/// reads.
fn types_interface<'a>(resolve: &'a Resolve, world: &World) -> &'a Interface {
    // `import types;` imports the interface without an `as` name, so wit-parser keys it as
    // `WorldKey::Interface(id)` rather than `WorldKey::Name("types")`. It is the world's only
    // interface import, so finding the one `WorldItem::Interface` is finding `types`.
    let id = world
        .imports
        .values()
        .find_map(|item| match item {
            WorldItem::Interface { id, .. } => Some(*id),
            _ => None,
        })
        .expect("the `types` interface is imported by the world");
    let interface = resolve
        .interfaces
        .get(id)
        .expect("the `types` interface resolves");
    assert_eq!(
        interface.name.as_deref(),
        Some("types"),
        "the world's interface import is `types`"
    );
    interface
}

/// The cases of a named enum in the interface, in declaration order.
fn enum_cases(resolve: &Resolve, interface: &Interface, name: &str) -> Vec<String> {
    let id = interface.types.get(name).unwrap_or_else(|| {
        panic!("`{name}` is declared in the `types` interface");
    });
    match &resolve.types[*id].kind {
        TypeDefKind::Enum(enumeration) => enumeration
            .cases
            .iter()
            .map(|case| case.name.clone())
            .collect(),
        _ => panic!("`{name}` is an enum"),
    }
}

/// The methods of a named resource in the interface, in declaration order.
///
/// Resource methods live in the interface's `functions` map with a `FunctionKind::Method`
/// pointing at their resource, so this filters by that resource's `TypeId`.
fn resource_methods<'a>(interface: &'a Interface, name: &str) -> Vec<&'a Function> {
    let id = interface.types.get(name).unwrap_or_else(|| {
        panic!("`{name}` is declared in the `types` interface");
    });
    interface
        .functions
        .values()
        .filter(|function| function.kind == FunctionKind::Method(*id))
        .collect()
}

/// Kebab-case to camelCase, the spelling QuickJS hands a TypeScript rule author.
fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for ch in name.chars() {
        if ch == '-' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The QuickJS shape of a WIT type in *result* position.
fn render_type(resolve: &Resolve, ty: &Type) -> String {
    match ty {
        Type::String | Type::Char => "string".to_owned(),
        Type::Bool => "boolean".to_owned(),
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::S64
        | Type::F32
        | Type::F64 => "number".to_owned(),
        Type::Id(id) => render_type_id(resolve, *id),
        Type::ErrorContext => "unknown".to_owned(),
    }
}

/// The QuickJS shape of a named WIT type.
fn render_type_id(resolve: &Resolve, id: wit_parser::TypeId) -> String {
    let def = &resolve.types[id];
    if let Some(name) = def.name.as_deref() {
        match name {
            "node" => return "Node".to_owned(),
            "match" => return "Match".to_owned(),
            "binding-kind" => return "BindingKind".to_owned(),
            "node-location" => return "NodeLocation".to_owned(),
            "reduce-location" => return "ReduceLocation".to_owned(),
            "fix" => return "Fix".to_owned(),
            "rule-card" => return "RuleCard".to_owned(),
            "emitted-fact" => return "EmittedFact".to_owned(),
            "check-context" => return "CheckContext".to_owned(),
            "reduce-context" => return "ReduceContext".to_owned(),
            _ => {}
        }
    }
    match &def.kind {
        // A bare type alias: `type node = u32` and `type match = list<match-entry>` are caught
        // by name above, so an unnamed alias here just resolves through.
        TypeDefKind::Type(inner) => render_type(resolve, inner),
        TypeDefKind::List(inner) => format!("{}[]", render_type(resolve, inner)),
        TypeDefKind::Option(inner) => format!("{} | undefined", render_type(resolve, inner)),
        // QuickJS throws on the error half rather than returning a `result`, so the error is
        // dropped and the ok half is all a rule author sees.
        TypeDefKind::Result(result) => result
            .ok
            .as_ref()
            .map_or_else(|| "void".to_owned(), |ok| render_type(resolve, ok)),
        // A resource reached through `borrow<...>` or an `own<...>`, and every kind that does
        // not appear in a context method signature, all render as the same honest non-type.
        TypeDefKind::Resource
        | TypeDefKind::Handle(_)
        | TypeDefKind::Record(_)
        | TypeDefKind::Enum(_)
        | TypeDefKind::Flags(_)
        | TypeDefKind::Variant(_)
        | TypeDefKind::Tuple(_)
        | TypeDefKind::Map(_, _)
        | TypeDefKind::FixedLengthList(..)
        | TypeDefKind::Future(_)
        | TypeDefKind::Stream(_)
        | TypeDefKind::Unknown => "unknown".to_owned(),
    }
}

/// The QuickJS shape of one method parameter.
///
/// `option<T>` in parameter position is an optional parameter (`name?: T`), the JavaScript
/// spelling the current definitions use for `resolvesToImport`'s `name` and `facts`' `kind`.
fn render_param(resolve: &Resolve, name: &str, ty: &Type) -> String {
    if let Type::Id(id) = ty
        && let TypeDefKind::Option(inner) = &resolve.types[*id].kind
    {
        return format!("{name}?: {}", render_type(resolve, inner));
    }
    format!("{name}: {}", render_type(resolve, ty))
}

/// Render one context interface from the resource it names.
///
/// `quickjs_only` controls whether the `facts` method — which QuickJS gives a per-file rule
/// and the WIT world does not — is appended. It is present on `RuleContext` and absent from
/// `ReduceContext`, because the world puts `facts` only on the cross-file context.
fn render_context(
    resolve: &Resolve,
    name: &str,
    methods: &[&Function],
    quickjs_only: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "/** A rule's {name} surface. */");
    let _ = writeln!(out, "export interface {name} {{");
    for method in methods {
        if member_name(method) == "today" {
            // Omitted deliberately. The world declares `today` as a method returning
            // `option<string>`; QuickJS instead exposes `ctx.today` as a *conditional property* —
            // a getter present only when the run is permitted to observe the date — which is a
            // different shape this renderer cannot state honestly from the world alone. Typing
            // that property accurately is a separate concern, and nothing here should emit a
            // method signature that no TypeScript rule could call.
            continue;
        }
        out.push_str(&render_member(resolve, method));
    }
    if quickjs_only {
        // QuickJS hands a per-file rule `facts`, which the world keeps on the cross-file context
        // alone. It is the one member this renderer adds that the world does not declare, and it
        // is added so the published surface keeps describing what a TypeScript rule can call.
        out.push_str("  /** Facts emitted so far, optionally filtered by `kind`. */\n");
        out.push_str("  facts(kind?: string): EmittedFact[]\n");
    }
    out.push_str("}\n");
    out
}

/// The method's own name, stripped of the `[method]check-context.` mangling wit-parser stores
/// resource methods under.
fn member_name(method: &Function) -> &str {
    method.name.rsplit('.').next().unwrap_or(&method.name)
}

/// Render one member of a context interface.
///
/// The three fields a rule reaches as plain properties rather than calls (`filePath`,
/// `fileText`, `root`), and the two methods whose JavaScript signature folds several WIT
/// parameters into an object (`report`, `emitFact`), are special-cased; every other member is
/// rendered straight from the resource's own signature.
fn render_member(resolve: &Resolve, method: &Function) -> String {
    let name = member_name(method);
    match name {
        "file-path" => return "  readonly filePath: string\n".to_owned(),
        "file-text" => return "  readonly fileText: string\n".to_owned(),
        "root" => return "  readonly root: Node\n".to_owned(),
        "files" => return "  readonly files: string[]\n".to_owned(),
        "emit-fact" => return "  emitFact(fact: Fact): void\n".to_owned(),
        "report" => {
            // `report` folds the two optional WIT parameters into the JavaScript
            // `ReportOptions` idiom; the first real parameter is the site it reports at —
            // `Node` on the per-file context, `ReduceLocation` on the cross-file one.
            let at = method.params.get(1).map_or_else(
                || "Node".to_owned(),
                |param| render_type(resolve, &param.ty),
            );
            return format!("  report(at: {at}, message?: string | ReportOptions): void\n");
        }
        _ => {}
    }

    // The first parameter is the component model's implicit `self` borrow, which a rule author
    // never names.
    let params = method
        .params
        .iter()
        .skip(1)
        .map(|param| render_param(resolve, &param.name, &param.ty))
        .collect::<Vec<_>>()
        .join(", ");
    let result = method
        .result
        .as_ref()
        .map_or_else(|| "void".to_owned(), |ty| render_type(resolve, ty));
    format!("  {}({params}): {result}\n", camel(name))
}

const HEADER: &str = "\
/**
 * Type definitions for authoring lanekeep rules.
 *
 * **Generated from `crates/lanekeep-wasm/wit/world.wit` by `crates/lanekeep-types-gen`.** Do not
 * edit by hand — run `just generate-index-dts` and commit the result.
 *
 * These describe the host API a rule reaches inside lanekeep's sandbox. Nothing here runs in
 * Node: `defineRule` and `defineConfig` are identity functions whose only job is to give the
 * compiler something to check against, and `RuleContext` is provided by lanekeep at run time.
 * The world is the single source of truth for every member the renderer emits straight from it.
 * Two members deviate from the world on purpose, and both are QuickJS-shaped: `today` is omitted
 * from `RuleContext` because QuickJS exposes it as a conditional property rather than a callable,
 * a shape this renderer cannot state honestly from the world; and `facts` is added to
 * `RuleContext` because QuickJS hands a per-file rule `facts` that the world declares only on
 * `reduce-context`. Nothing else is added or omitted by hand.
 */
";

const NODE: &str = "\
/**
 * A node in the parse tree.
 *
 * Deliberately opaque. Nodes cross into the sandbox as integer handles rather than objects,
 * and the reason this is a branded type rather than `number` is that **the root node's handle
 * is `0`** — written as a plain number, `if (!node)` looks like a null check and silently
 * discards the root. Compare against `undefined` explicitly.
 */
export type Node = number & { readonly __lanekeepNode: unique symbol }
";

const LANGUAGE_ID: &str = "\
/** A language lanekeep can parse. */
export type LanguageId = 'typescript' | 'tsx' | 'javascript' | 'python' | 'go' | 'rust'
";

const SEVERITY: &str = "\
/** How serious a violation is. */
export type Severity = 'error' | 'warn' | 'off'
";

const MATCH: &str = "\
/**
 * The captures of one query match, keyed by capture name without the `@`.
 *
 * A capture that did not participate in the match is absent, which is why the values are
 * optional — an alternation like `[(a) (b)] @x` binds `@x` either way, but two separate
 * patterns capturing different names do not.
 */
export type Match = Record<string, Node | undefined>
";

const RULE_CARD: &str = "\
/**
 * What a rule tells whoever has to act on the violation — increasingly an agent.
 *
 * Not documentation, and not optional. `remediation` is the field worth the effort: it should
 * say what to do, not restate the problem.
 */
export interface RuleCard {
  /** What is wrong, in a few words. */
  message: string
  /** What to do about it. */
  remediation: string
  /** One example each way. */
  examples: {
    bad: string
    good: string
  }
}
";

const GATES: &str = "\
/** Cheap rejections applied before a file is read or parsed. */
export interface Gates {
  /**
   * Glob patterns a file's path must match for the rule to consider it.
   *
   * The path is relative to the project root, and the pattern must match the whole path —
   * anchored, not a substring search. Patterns use the `globset` dialect, matched
   * case-sensitively: `*` matches any run of characters (including `/`), `?` any single
   * character, `[ab]`/`[!ab]` character classes and `{a,b}` alternates work, and `**`
   * recurses directories — `src/**` admits everything under `src`, and `**` in front
   * of `*.test.ts` admits a test file at any depth.
   */
  pathMatches?: string[]
  /**
   * Glob patterns that skip a file — a path matching any of these is never parsed. Checked
   * before `pathMatches` and winning over it: a path a `pathMatches` pattern would have
   * admitted is still skipped when a `pathNotMatches` pattern matches it.
   *
   * Same dialect and anchoring as `pathMatches`.
   */
  pathNotMatches?: string[]
  /**
   * Literal substrings a file's raw bytes must contain. A file missing any one of them is
   * never parsed.
   *
   * **This is an *and*, not an *or*.** A rule matching either of two tokens cannot express
   * its gate as `['a', 'b']` — that rejects any file containing only one, which is usually
   * most of them, and the rule then reports nothing while looking healthy. There is no `or`
   * form; omit the gate when no single substring covers every case.
   */
  fileContains?: string[]
  /**
   * Literal substrings that skip a file — a file whose raw bytes contain **any** of them is
   * never parsed. The mirror image of `fileContains`'s *and*: where that gate requires every
   * listed substring, this one rejects on the first that is present.
   */
  fileNotContains?: string[]
}
";

const FIX: &str = "\
/** A replacement a rule offers for a violation. */
export interface Fix {
  /** The node whose text is replaced. */
  node: Node
  /** What to replace it with. */
  text: string
  /**
   * Whether the fix preserves behavior.
   *
   * Only a fix marked `true` is applied by `--fix`. Anything else is a suggestion — shown,
   * never written — because the cautious mistake costs a manual edit and the other one
   * rewrites someone's code silently.
   */
  safe?: boolean
}
";

const REPORT_OPTIONS: &str = "\
/** Options for a single report. */
export interface ReportOptions {
  /** Overrides the card's `message` for this one violation. */
  message?: string
  /** A replacement to offer. */
  fix?: Fix
}
";

const FACT: &str = "\
/**
 * A fact a rule emits for the reduce phase.
 *
 * `kind` is required and must be non-empty, because it is what `ctx.facts('...')` filters on.
 * A fact without one could never be retrieved, so emitting it is always a mistake — and a
 * silent one, since the rule would look like it was working right up until `reduce` found
 * nothing. lanekeep throws rather than accept it.
 */
export interface Fact {
  kind: string
  [key: string]: unknown
}
";

const EMITTED_FACT: &str = "\
/** A fact as `reduce` receives it, with the file that emitted it. */
export interface EmittedFact extends Fact {
  /** Path of the file this came from, relative to the project root. */
  file: string
}
";

const NODE_LOCATION: &str = "\
/**
 * A node's location: the file, line and column `ctx.loc` returns.
 *
 * `line` and `column` are required here, unlike on `ReduceLocation`: `ctx.loc` either
 * resolves the node and returns all three together, or the node does not resolve and the
 * call returns `undefined` entirely — there is no partial state to leave room for.
 */
export interface NodeLocation {
  /** Path relative to the project root. */
  file: string
  /** One-based. */
  line: number
  /** One-based. */
  column: number
}
";

const REDUCE_LOCATION: &str = "\
/** A violation the reduce phase reports, which has no node to point at. */
export interface ReduceLocation {
  /** Path relative to the project root. */
  file: string
  /** One-based. */
  line?: number
  /** One-based. */
  column?: number
}
";

const RULE: &str = "\
/** A rule, as `defineRule` takes it. */
export interface Rule {
  /**
   * Namespaced identifier, as `namespace/name`.
   *
   * `local/` needs no declaration and `lanekeep/` is reserved for built-ins; any other
   * namespace must be listed in the config's `namespaces`.
   */
  id: string
  /**
   * Which languages this rule applies to.
   *
   * **Defaults to `['typescript', 'tsx']`**, and this is the field most worth getting right
   * on a rule for anything else. The grammar is chosen by the file, not by the rule, and a
   * rule does not run on a file whose language it does not name — so omitting this on a Go
   * or Rust rule means it silently never fires.
   */
  language?: LanguageId | LanguageId[]
  /** How serious a violation is, before any config override. */
  severity: Severity
  /** What the rule tells whoever has to act on it. */
  card: RuleCard
  /** Cheap rejections before parsing. */
  gates?: Gates
  /**
   * The tree-sitter query gating the handler.
   *
   * Rust matches it across a single shared parse and only matches reach `check`, which is
   * what keeps a JavaScript rule affordable. Write the narrowest query that captures what
   * you need; `check` then only refines.
   *
   * Text predicates filter matches in Rust before the handler, so a predicate can only
   * narrow, never widen, what `check` sees: `#eq?`, `#not-eq?`, `#match?`, `#not-match?`,
   * `#any-of?` and `#not-any-of?` are supported (plus the `any-` forms of `eq?`/`match?`).
   * `#match?`/`#not-match?` run on the `regex` crate, which is deterministic and supports
   * no backreferences or lookaround. `#is?`, `#is-not?`, `#set!`, or an operator the
   * binding does not know is refused at compile time.
   */
  query: string
  /** A per-invocation budget overriding the default, in milliseconds. */
  timeout?: number
  /** Called once per query match. */
  check?(ctx: RuleContext, match: Match): void
  /** Called once per run, after every file, with facts only. */
  reduce?(ctx: ReduceContext): void
}
";

const CONFIG: &str = "\
/** A lanekeep configuration, as `defineConfig` takes it. */
export interface Config {
  /** Globs selecting files to check, relative to the project root. */
  include?: string[]
  /** Globs removing files from that selection. */
  exclude?: string[]
  /** Rule-id namespaces this project uses beyond `local`. */
  namespaces?: string[]
  /** Override a rule's own severity, by id. */
  severity?: Record<string, Severity>
  /** Execution budgets, in milliseconds. */
  timeouts?: {
    /** Per rule invocation. */
    rule?: number
    /** Wall-clock, for the whole run. */
    global?: number
  }
  /** Policy for suppression directives. All off by default. */
  suppressions?: {
    /** A valid directive with no `expires:` is reported. */
    requireExpiry?: boolean
    /** An expiry more than this many days after today is reported. */
    maxExpiryDays?: number
    /** Any whole-file directive is reported. */
    forbidFileScope?: boolean
  }
  /** The rules to run, in order. */
  rules: Rule[]
}
";

const DEFINE_RULE: &str = "\
/**
 * Define a rule.
 *
 * An identity function. It exists so the compiler checks the object against {@link Rule}
 * where it is written, rather than reporting a mismatch from wherever it is imported.
 */
export declare function defineRule(rule: Rule): Rule
";

const DEFINE_CONFIG: &str = "\
/**
 * Define a configuration.
 *
 * An identity function, for the same reason as {@link defineRule}. Most projects will write
 * `lanekeep.json` instead — configuration is data, and only rules need to be programs.
 */
export declare function defineConfig(config: Config): Config
";

fn render_node() -> String {
    NODE.to_owned()
}

fn render_binding_kind(cases: &[String]) -> String {
    let mut out = String::from(
        "/** How a name was introduced, as `ctx.bindingKind` reports it. */\nexport type BindingKind =\n",
    );
    for case in cases {
        let _ = writeln!(out, "  | '{case}'");
    }
    out
}

fn render_language_id() -> String {
    LANGUAGE_ID.to_owned()
}

fn render_severity() -> String {
    SEVERITY.to_owned()
}

fn render_match() -> String {
    MATCH.to_owned()
}

fn render_rule_card() -> String {
    RULE_CARD.to_owned()
}

fn render_gates() -> String {
    GATES.to_owned()
}

fn render_fix() -> String {
    FIX.to_owned()
}

fn render_report_options() -> String {
    REPORT_OPTIONS.to_owned()
}

fn render_fact() -> String {
    FACT.to_owned()
}

fn render_emitted_fact() -> String {
    EMITTED_FACT.to_owned()
}

fn render_node_location() -> String {
    NODE_LOCATION.to_owned()
}

fn render_reduce_location() -> String {
    REDUCE_LOCATION.to_owned()
}

fn render_rule() -> String {
    RULE.to_owned()
}

fn render_config() -> String {
    CONFIG.to_owned()
}

fn render_define_rule() -> String {
    DEFINE_RULE.to_owned()
}

fn render_define_config() -> String {
    DEFINE_CONFIG.to_owned()
}
