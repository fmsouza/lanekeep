//! The taint (data-flow) analysis for TypeScript, TSX and JavaScript.
//!
//! A value-level may-taint: a `@source` value that reaches a `@sink` with no intervening
//! `@sanitizer` is one [`FlowPath`]. It is resolved on demand — from each sink backward
//! through the def-use chain (declarators and reassignments), following local identifier
//! aliases and cutting at sanitizers.
//!
//! Flow-sensitivity is **reaching-definitions with kill**: at each read of a binding, only
//! the definitions that actually reach that read taint it — the nearest definition on each
//! control-flow path, with no later definition of the same binding between it and the read.
//! Intra-block statement order decides within a block (a later write clobbers an earlier
//! one); the per-function [`Cfg`](crate::cfg::Cfg)'s reachability decides across blocks,
//! avoiding every block that redefines the binding. A definition to a sanitizer's result or
//! to any other clean value therefore *kills* an earlier tainted definition — the in-place
//! sanitizer `s = redact(s)` and the clean reassignment `s = "public"` both cut the flow,
//! unifying the sanitizer-cut and reassignment-kill into one mechanism. Path-insensitively:
//! a kill on one branch does not kill on another, so if any path reaches the read with a
//! tainted nearest definition, the flow is reported.
//!
//! **Field-sensitive per access path to depth three; index-insensitive (#225).** Taint is
//! tracked per [`Path`] — `o.secret` distinctly from `o.public` — and a write contributes to a
//! read only when the written and read paths are prefix-comparable in either direction. So
//! `o.secret = s` leaves `o.public` clean, while `log(o)` and `log(o.secret.raw)` both report:
//! a read above a write covers it, and a write above a read covers it. Every subscript
//! collapses to [`Seg::Index`], so index sensitivity is explicitly not bought and
//! `a[0] = s; log(a[1])` still reports. A path longer than [`MAX_PATH_LEN`] is truncated and
//! the truncated path is top for its subtree, matching every extension — the widening is keyed
//! on path length, a syntactic property of the read, never on a visit count, so a cyclic
//! object graph terminates at a stated depth and reports the same thing every run. A read of a
//! [`SHAPE_PROPERTIES`] name — `length`, `byteLength`, `byteOffset`, `size` — is clean off any
//! base, which is the one false positive the corpus calibration measured.
//!
//! None of that changes what a field write *is*. It remains a **weak update**: it adds taint
//! from that point forward and never *kills* a prior definition, since it mutates the object
//! rather than rebinding the name. So field/index writes form a separate additive union
//! outside the reaching-defs-with-kill machinery above (which governs only strong identifier
//! updates); a weak write is admitted whenever it may reach the read *and* lands at a path the
//! read can observe. The analysis still over-approximates — a subscript write taints every
//! subscript read, and, because a subscript is an unknown key, every named-field read of the
//! same base too, and a path past the widening bound taints every sibling below it — which is
//! the sound direction for a taint tool.
//!
//! **A wrapping expression or a literal carries its operands' taint (#246).** A transparent
//! wrapper — `(e)`, `e!`, `await e`, `e as T`, `e satisfies T` — *is* its inner value, so it
//! carries that value's taint at the same path (there is no promise model, so `await` is
//! transparent too). An object, array or conditional carries the union of its members' taint,
//! each at the path the read observes it under: `log({ cause: s })` reports, and `log(o.public)`
//! where `o = { cause: s }` stays silent. A source *textually inside* any of these was always
//! caught by the containment scan regardless of path; what #246 adds is the taint that reaches
//! them held by a *binding*, which the containment scan cannot see.
//!
//! **Augmented assignment is a weak update too.** `x += rhs` (and `-=`, `||=`, `??=`, …)
//! desugars to `x = x op rhs`, whose result is tainted if *either* the prior `x` or `rhs` is
//! tainted. So an `op=` joins the same additive, non-killing union: it contributes `rhs`'s taint
//! and never *kills* the prior definition of `x`, which keeps reaching the read. Modeling it as
//! a strong def would kill that prior taint and silence `let s = getSecret(); s += "clean";
//! log(s)` — a false negative. An identifier target adds taint to the name; a member/subscript
//! target (`o.total += rhs`) is a field write to the base, exactly like `o.total = rhs`.
//! tree-sitter parses `s += x` as `augmented_assignment_expression`, distinct from the
//! `assignment_expression` the strong path matches.
//!
//! v1 is intra-procedural, path-insensitive, and does not follow taint through a call's
//! arguments; nor does it track a binding introduced by **destructuring** (`const { x } =
//! getSecret()`) or by a **`for...of`** header (`for (const x of getSecret())`) — both v1 false
//! negatives. See
//! `docs/superpowers/specs/2026-09-05-taint-analysis-flow-checkflow-design.md` §5.

use std::cell::RefCell;
use std::collections::BTreeSet;

use lanekeep_lang::binding::BindingResolver;
use lanekeep_lang::flow::{FlowAnalyzer, FlowPath};
use tree_sitter::{Node, Tree};

use crate::binding::JsBindingResolver;
use crate::cfg::{BlockId, Cfg};

/// The depth ceiling on the alias walk, copied from the type oracle
/// (`crates/lanekeep-types/src/oracle.rs`). Bounds a cyclic `const a = b; const b = a`.
///
/// A loop of self-referential field writes (`while (c) { o.f0 = o; o.f1 = o; }`) asks the same
/// question at every level of this bound, once per write — exponential in the depth, measured
/// before the cut at seconds for two writes and effectively unbounded at three (2026-09-07, the
/// M3 Max `docs/taint-calibration.md` names) — and no budget reaches the analyzer, which runs
/// in plain Rust between the run clock's polls. `Taint::in_progress` cuts a repeated question;
/// that, not a limit, is what bounds the walk.
const MAX_DEPTH: u32 = 16;

/// Property names that describe a value's *shape* rather than carrying the value. A read of
/// one off a tainted base yields a clean value: `log(s.length)` is silent where `log(s)`,
/// `log(s.buffer)` and `log(s.mnemonic)` report.
///
/// This is the whole of what the #220 calibration measured — `${seed.length}` at
/// `extensions/keystore-chrome/src/keystore/sign.ts:112`, one logical site reported three
/// times (`docs/taint-calibration.md`). It cleans the *read*, not the base: the binding stays
/// tainted for every other read of it.
///
/// **Engine-owned and not configurable**, the same standing as [`MAX_DEPTH`]: a stated
/// discrete bound rather than a similarity knob (#155). **Sorted**, because
/// [`is_shape_property_read`] searches it with `binary_search`, which answers wrongly and
/// silently on an unsorted slice — `the_shape_property_table_is_sorted` is what holds it.
///
/// **The unsoundness is documented and deliberate, and it has no project-facing lever.** A
/// project that names a secret field `size` or `length` gets a false negative there —
/// `o.length = getSecret(); log(o.length)` is silent — and nothing in a `flow` rule can undo
/// it: a `@sanitizer` only ever cuts, and `checkFlow` runs once per flow the engine found, so a
/// flow this table suppressed never reaches it. Reporting such a site takes a separate
/// `query`/`check` beside the `flow` in the same rule. The table is engine-owned so the
/// tradeoff is one stated fact rather than a knob (#155).
const SHAPE_PROPERTIES: &[&str] = &["byteLength", "byteOffset", "length", "size"];

/// One step of an access path.
///
/// `Index` collapses every subscript — `a[0]`, `a[i]` and `o["k"]` are one segment — so index
/// sensitivity is explicitly *not* bought by C1: `a[0] = s; log(a[1])` still reports, which is
/// the promise `docs/architecture.md` §4 keeps at "Index-sensitive: no". It is also comparable
/// with every named field, since an unknown key may be any key; see [`prefix_comparable`].
///
/// `Field` is declared first, so the derived `Ord` puts `Index` after every `Field`. That is a
/// stated order rather than an incidental one, because [`canonicalize`]'s key ends in a
/// [`Path`] and a total key is what makes two runs byte-identical.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Seg {
    /// A named property: `o.secret` contributes `Field("secret")`.
    Field(String),
    /// Any subscript, and any property the grammar does not spell as a plain name.
    Index,
}

/// An access path relative to a base binding, outermost segment first: `o.a.b` is
/// `[Field("a"), Field("b")]` and `o[i].c` is `[Index, Field("c")]`.
type Path = Vec<Seg>;

/// The widening bound on an access path.
///
/// A path longer than this is truncated, and the truncated path is **top for its subtree**: it
/// compares equal to every extension of itself, so `o.a.b.c.d = getSecret()` taints
/// `o.a.b.c.e` — today's whole-object behavior, restored locally at depth. Keyed on path
/// length, a syntactic property of the read, never on a visit count: a cyclic object graph
/// therefore terminates at a stated depth and reports the same thing every run, rather than at
/// whatever the walk happened to visit.
///
/// Three is where the evidence stops. The corpus's deepest secret-bearing access is two
/// segments, and every segment past the bound costs a comparison on every weak definition
/// without distinguishing anything the corpus contains.
const MAX_PATH_LEN: usize = 3;

/// Widen `path` to [`MAX_PATH_LEN`] segments, keeping the outermost ones.
///
/// The outermost segments are the discriminating ones — `o.a.b` and `o.c.d` differ at the
/// first — so dropping the tail loses the least. Applied where a path is *built*
/// ([`base_and_path`]) and where two are joined, so nothing downstream has to remember to.
fn truncate(mut path: Path) -> Path {
    path.truncate(MAX_PATH_LEN);
    path
}

/// Whether two access paths are **prefix-comparable in either direction**: one is a prefix of
/// the other, so a write at one may be observed by a read at the other.
///
/// The empty path compares with everything, in both roles: a root read covers every path under
/// it (`o.a = s; log(o)` reports) and a root write taints every path under it (`s` tainted
/// whole makes `s.mnemonic` tainted). `[f]` compares with `[f, g]` — a write at `o.f` is read
/// by `o.f.g` and by `o.f` alike. `[f]` and `[g]` do not compare, which is the whole of what
/// field sensitivity buys.
///
/// **A subscript is an unknown key, and an unknown key may be any key.** [`Seg::Index`]
/// therefore compares with every segment: `o[k] = s` taints `o.secret` and `o.secret = s` is
/// read by `o[k]`, exactly as the field-insensitive analysis before #225 answered — what field
/// sensitivity buys is `o.secret` against `o.public`, two *known* keys, never a known key
/// against a computed one. Treating `Index` as a distinct known segment instead silenced a
/// computed-key write read by name, which is the ordinary `config[name] = secret` shape.
///
/// **Truncated-top falls out of this rather than needing a case.** Both arguments were
/// [`truncate`]d where they were built, so two paths that differ only past [`MAX_PATH_LEN`]
/// arrive here already equal and compare — which is exactly "the truncated path is top for its
/// subtree, matching every extension".
fn prefix_comparable(a: &Path, b: &Path) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| matches!((x, y), (Seg::Index, _) | (_, Seg::Index)) || x == y)
}

/// The JS/TS taint analyzer, returned from [`crate::TypeScript::flow_analyzer`] and its
/// siblings.
pub(crate) struct JsFlowAnalyzer;

impl FlowAnalyzer for JsFlowAnalyzer {
    fn analyze<'t>(
        &self,
        tree: &'t Tree,
        source: &str,
        sources: &[Node<'t>],
        sinks: &[Node<'t>],
        sanitizers: &[Node<'t>],
    ) -> Vec<FlowPath<'t>> {
        let mut flows: Vec<(FlowPath<'t>, Path)> = Vec::new();
        for &sink in sinks {
            // Each sink is analyzed in its own enclosing function's CFG. Rebuilding per
            // sink keeps the borrow simple; the fixtures hold one or two functions.
            let root_and_cfg = enclosing_root_and_cfg(source, sink);
            let (root, cfg) = match &root_and_cfg {
                Some((root, cfg)) => (Some(*root), Some(cfg)),
                None => (None, None),
            };
            let taint = Taint {
                tree,
                source,
                sources,
                sanitizers,
                cfg,
                root,
                in_progress: RefCell::new(BTreeSet::new()),
            };
            for fact in taint.taint_of(sink, &Path::new(), 0) {
                flows.push((
                    FlowPath {
                        source: fact.source,
                        sink,
                        steps: fact.steps,
                    },
                    fact.path,
                ));
            }
        }
        canonicalize(flows)
    }
}

/// One reason a value is tainted: the originating source, the alias hops between it and the
/// value, in flow order (source → value), and the access path the taint was found at.
struct Fact<'t> {
    source: Node<'t>,
    steps: Vec<Node<'t>>,
    /// The access path in force where this fact was produced — `[]` for a root taint, `[b]`
    /// for a taint found while asking a value about its `.b`. Carried only so
    /// [`canonicalize`]'s key is total: two facts that agree on source, sink and every step
    /// but were found at different paths would otherwise be separated by input order alone.
    path: Path,
}

/// A definition of a binding: the assignment site (declarator or `=` expression) and the
/// right-hand-side value it stores.
#[derive(Clone, Copy)]
struct Def<'t> {
    /// The `variable_declarator` or `assignment_expression` — the step recorded for an alias
    /// hop.
    site: Node<'t>,
    /// The value expression assigned. For the parameter-origin definition (#217) this is the
    /// parameter node itself — equal to `site` — which is what [`Def::is_parameter_origin`] tests.
    rhs: Node<'t>,
}

impl Def<'_> {
    /// Whether this is the parameter-origin definition [`Taint::definitions_of`] seeds for a
    /// `@source` captured on a parameter (#217). Its `rhs` *is* its `site` — the parameter node —
    /// a shape no ordinary definition has (a declarator's `rhs` is its `value` child, an
    /// assignment's its `right`). Such a def is the taint origin, not an alias hop, and lives at
    /// the function's entry block rather than in a body block.
    fn is_parameter_origin(&self) -> bool {
        self.rhs.id() == self.site.id()
    }
}

/// The immutable context for one sink's taint walk.
struct Taint<'a, 't> {
    tree: &'t Tree,
    source: &'a str,
    sources: &'a [Node<'t>],
    sanitizers: &'a [Node<'t>],
    /// The enclosing function's CFG, or `None` when the sink owns no flow graph. A missing
    /// graph makes reachability unanswerable, so the walk then admits every definition —
    /// the may-analysis's correct over-approximating bias.
    cfg: Option<&'a Cfg<'t>>,
    /// The enclosing function's root node, whose subtree is scanned for reassignments.
    root: Option<Node<'t>>,
    /// Every `(use start byte, path)` question currently being answered up the stack.
    ///
    /// A loop of self-referential field writes — `while (c) { o.f0 = o; o.f1 = o; }` — asks
    /// "`o` at `[]`" from inside the answer to "`o` at `[]`", once per write, at every level of
    /// [`MAX_DEPTH`]: exponential in the depth, and outside every budget, since the analyzer
    /// runs in plain Rust between the run clock's polls. A repeated question is cut instead. It
    /// loses nothing: a fact is a (source, sink) pair, and every source the repeat could reach is
    /// reached by the instance already answering it; `canonicalize` keeps the shortest chain
    /// either way. `BTreeSet` rather than a hash set so the walk stays deterministic.
    in_progress: RefCell<BTreeSet<(usize, Path)>>,
}

impl<'t> Taint<'_, 't> {
    /// The taint facts an expression carries when read at the sink, **at access path `path`**.
    ///
    /// `path` is what the caller wants to know about the value `expr` produces: `[]` at a sink
    /// that reads the value whole, `[secret]` when the caller is really asking about
    /// `<expr>.secret`. It is threaded rather than resolved eagerly because the walk is
    /// demand-driven: only the paths a sink actually reads are ever asked about, which is what
    /// makes C1 need no lattice and no fixpoint.
    ///
    /// Value-level: a `@sanitizer` call yields a clean value regardless of its arguments, an
    /// arbitrary non-source call carries nothing (v1 does not track taint through a call), and
    /// only a direct `@source` or a local alias of a tainted binding is tainted. A
    /// taint-transparent wrapper — `(e)`, `e!`, `await e`, `e as T`, `e satisfies T` — carries
    /// its inner value's taint unchanged, and an object, array or ternary carries the union of
    /// its members' taint at the path each is read under (#246).
    ///
    /// A **direct** source taints whatever path is asked of it. That downward closure is
    /// load-bearing: dropping it would silence `const s = getSecret(); log(s.mnemonic)`, the
    /// shape the corpus's own source query exists to catch.
    fn taint_of(&self, expr: Node<'t>, path: &Path, depth: u32) -> Vec<Fact<'t>> {
        if depth >= MAX_DEPTH {
            return Vec::new();
        }
        // A sanitizer call's result is clean — cut the value — regardless of its arguments,
        // and before any source it textually wraps is considered. This is what makes
        // `redact(getSecret())` silent where `foo(getSecret())` is not.
        if is_member(expr, self.sanitizers) {
            return Vec::new();
        }
        // A `@source` appearing within the expression taints it: identity when the sink or
        // value *is* the source call, containment when it wraps one (`getSecret() + x`).
        // Taint carried by a *binding* through a call is a different question, answered
        // below by def-use — and `identity(a)` wraps no source, so it stays clean (the v1
        // alias-through-call false negative).
        //
        // A contained source wrapped by a sanitizer is cut (#218). The `is_member` cut above
        // fires only when `expr` *itself* is the sanitizer call; a source nested in a sanitizer
        // *inside* a larger sink expression — `log(redact(getSecret()) + "x")`, or a
        // `` `…${describeBytes(secret)}…` `` template — would otherwise report on syntactic
        // containment alone, a false positive on correctly-sanitized code (#195).
        //
        // The cut requires the sanitizer to sit *between* the source and `expr` — `source ⊆
        // sanitizer ⊆ expr` — so the source's contribution to `expr`'s value passes through it.
        // Restricting the sanitizer to `expr`'s own range is load-bearing: a sanitizer that merely
        // *contains* `expr` cleans its own result, not the side effects of its arguments, so a
        // secret leaking through a side effect — `redact(o.x = getSecret())`, then reading `o` —
        // reaches its own sink by a def-use edge that never crosses the sanitizer, and must still
        // report. A bare (unwrapped) contained source is untouched either way, so
        // `log(getSecret() + "x")` still reports.
        let direct: Vec<Node<'t>> = self
            .sources_within(expr)
            .into_iter()
            .filter(|source| !self.sanitizer_between(*source, expr))
            .collect();
        if !direct.is_empty() {
            return direct
                .into_iter()
                .map(|source| Fact {
                    source,
                    steps: Vec::new(),
                    path: path.clone(),
                })
                .collect();
        }
        match expr.kind() {
            "identifier" => self.taint_of_identifier(expr, path, depth),
            // A field or index read. C1 (#225): resolve it to its base *and the path it reads
            // at*, compose that with what the caller asked about, and put the question to the
            // base binding. `const p = o.inner; log(p.secret)` asks `o` about
            // `[inner, secret]` — the alias's own path and the caller's, in that order,
            // because the alias sits closer to the base. A read whose base is not a plain
            // identifier (a call result, `this`) has no binding to consult, so it carries
            // nothing.
            //
            // A shape-property read is clean whatever the base carries (#225, C2), cut here
            // rather than above the containment scan so a source textually inside the read —
            // `log(getSecret().length)` — is still reported.
            "member_expression" | "subscript_expression" => {
                if is_shape_property_read(expr, self.source) {
                    return Vec::new();
                }
                match base_and_path(expr, self.source) {
                    Some((base, mut read)) => {
                        read.extend(path.iter().cloned());
                        self.taint_of_identifier(base, &truncate(read), depth)
                    }
                    None => Vec::new(),
                }
            }
            // Taint-transparent wrappers (#246): the result *is* the inner expression's value,
            // so the caller's path passes straight through. `(e)`, `e!`, `e as T`,
            // `e satisfies T`, and — with no promise model to confuse — `await e`. Depth is
            // unchanged, as for the member read above: this is syntactic descent within one
            // expression, bounded by tree height, not a def-use hop that could cycle.
            "parenthesized_expression"
            | "non_null_expression"
            | "await_expression"
            | "as_expression"
            | "satisfies_expression" => match transparent_inner(expr) {
                Some(inner) => self.taint_of(inner, path, depth),
                None => Vec::new(),
            },
            // A literal propagates the taint of its members, at the path each is read under
            // (#246) — real propagation rather than passthrough, so `log({ cause: secret })`
            // reports and `log({ cause: secret }); … o.other` does not.
            "object" => self.taint_of_object(expr, path, depth),
            "array" => self.taint_of_array(expr, path, depth),
            // A conditional yields one branch or the other; its value is the union of the two,
            // each asked at the caller's path. The condition does not carry the value.
            "ternary_expression" => ["consequence", "alternative"]
                .into_iter()
                .filter_map(|field| expr.child_by_field_name(field))
                .flat_map(|branch| self.taint_of(branch, path, depth))
                .collect(),
            // A non-source, non-sanitizer call is opaque: v1 does not follow taint through a
            // call's arguments (the alias-through-call false negative, spec §13). Only a
            // direct source or a local identifier alias carries taint.
            _ => Vec::new(),
        }
    }

    /// The taint an object literal carries at `path`: the union over its members of the taint
    /// each carries at the path the caller's read observes it under (#246).
    ///
    /// A `pair`'s key is one path segment ([`key_segment`]); a `shorthand_property_identifier`
    /// (`{ name }`) is `{ name: name }`, its key `Field(name)` and its value the binding it
    /// names, resolved as an identifier; a `spread_element` (`{ ...o }`) exposes the same paths
    /// as the object it spreads, so it is asked at the caller's path unchanged; a
    /// `method_definition` carries no value. The residual asked of a keyed member is what
    /// remains of `path` after its key ([`read_under`]): a read at `[]` observes every member,
    /// a read at `[k, …rest]` observes the members whose key is comparable to `k`.
    fn taint_of_object(&self, obj: Node<'t>, path: &Path, depth: u32) -> Vec<Fact<'t>> {
        let mut facts = Vec::new();
        let mut cursor = obj.walk();
        for member in obj.named_children(&mut cursor) {
            match member.kind() {
                "pair" => {
                    let key = member
                        .child_by_field_name("key")
                        .map_or(Seg::Index, |key| key_segment(key, self.source));
                    if let Some(value) = member.child_by_field_name("value")
                        && let Some(residual) = read_under(path, &key)
                    {
                        facts.extend(self.taint_of(value, &residual, depth));
                    }
                }
                "shorthand_property_identifier" => {
                    let key = Seg::Field(self.source[member.byte_range()].to_owned());
                    if let Some(residual) = read_under(path, &key) {
                        facts.extend(self.taint_of_identifier(member, &residual, depth));
                    }
                }
                "spread_element" => {
                    if let Some(inner) = member.named_child(0) {
                        facts.extend(self.taint_of(inner, path, depth));
                    }
                }
                // A `method_definition` binds a function, not a value that can carry taint.
                _ => {}
            }
        }
        facts
    }

    /// The taint an array literal carries at `path`: the union over its elements, each sitting at
    /// [`Seg::Index`] (index-insensitive, #225). A `spread_element` (`[...a]`) exposes the same
    /// paths as what it spreads, asked at the caller's path unchanged.
    fn taint_of_array(&self, arr: Node<'t>, path: &Path, depth: u32) -> Vec<Fact<'t>> {
        let mut facts = Vec::new();
        let mut cursor = arr.walk();
        for element in arr.named_children(&mut cursor) {
            if element.kind() == "spread_element" {
                if let Some(inner) = element.named_child(0) {
                    facts.extend(self.taint_of(inner, path, depth));
                }
            } else if let Some(residual) = read_under(path, &Seg::Index) {
                facts.extend(self.taint_of(element, &residual, depth));
            }
        }
        facts
    }

    /// The taint facts an identifier read carries **at access path `path`**: resolve it to its
    /// declaration and follow the definitions that *reach this read*, cutting later-clobbered
    /// ones.
    ///
    /// A strong definition replaces the whole binding, so the caller's path passes through it
    /// unchanged — `const s = X; log(s.a)` asks `X` about `[a]`. A weak (field or index) write
    /// lands at a path of its own, so it contributes only when the two are prefix-comparable,
    /// and what it is asked about is the *residual*: a write at `[a]` read at `[a, b]` is
    /// asked about `[b]`, because `o.a.b` is `rhs.b`.
    fn taint_of_identifier(&self, ident: Node<'t>, path: &Path, depth: u32) -> Vec<Fact<'t>> {
        let Some(decl) = JsBindingResolver.declaration_of(self.tree, self.source, ident) else {
            return Vec::new();
        };
        let question = (ident.start_byte(), path.clone());
        if !self.in_progress.borrow_mut().insert(question.clone()) {
            // Already being answered further up the stack: a cycle of weak definitions. The
            // outer instance enumerates every source this one could, so answering again only
            // re-walks the same definitions — which is what made the walk exponential.
            return Vec::new();
        }
        let mut facts = Vec::new();
        for def in self.reaching_defs(decl, ident) {
            self.collect_def_facts(def, path, depth, &mut facts);
        }
        for (def, residual) in self.weak_reaching_defs(decl, ident, path) {
            self.collect_def_facts(def, &residual, depth, &mut facts);
        }
        self.in_progress.borrow_mut().remove(&question);
        facts
    }

    /// Fold the taint carried by one definition's right-hand side into `facts`, asking it about
    /// `path`: recurse into the value, recording an alias hop (`const b = a`, `b = a`,
    /// `o.x = a`) as one step and a direct value assignment as none. Shared by the strong
    /// ([`Self::reaching_defs`]) and weak ([`Self::weak_reaching_defs`]) definition walks.
    fn collect_def_facts(&self, def: Def<'t>, path: &Path, depth: u32, facts: &mut Vec<Fact<'t>>) {
        // An alias hop is a def whose value is *another* binding (`b = a`, `s = a`): recorded
        // as one step. The parameter-origin def (#217) is the origin, not a hop — its `rhs` is
        // its own `site` — so it records no step, matching a tainted declarator read directly.
        let alias = def.rhs.kind() == "identifier" && !def.is_parameter_origin();
        for mut fact in self.taint_of(def.rhs, path, depth.saturating_add(1)) {
            if alias {
                fact.steps.push(def.site);
            }
            facts.push(fact);
        }
    }

    /// Every definition of the binding `decl` declares: its declarator's own initializer,
    /// plus every `x = <rhs>` in the enclosing function whose target resolves back to `decl`.
    /// Path-insensitive — a binding assigned in two branches has two definitions, and both
    /// are followed.
    fn definitions_of(&self, decl: Node<'t>) -> Vec<Def<'t>> {
        let mut defs = Vec::new();
        if decl.kind() == "variable_declarator"
            && let Some(value) = decl.child_by_field_name("value")
        {
            defs.push(Def {
                site: decl,
                rhs: value,
            });
        }
        // A parameter captured as a `@source` is tainted from function entry (#217): the
        // callback-delivered secret `withSecret((sk) => …)`, the corpus's dominant pattern
        // (docs/taint-calibration.md). A parameter has no declarator or assignment to seed from,
        // so model it as a strong definition whose value is the parameter node itself — which
        // `sources_within` recognizes as the captured source — positioned at the parameter,
        // before every read in the body. `reaching_defs` maps it to the entry block, so a later
        // reassignment or in-place sanitizer kills it exactly as it kills a tainted declarator
        // (Stage 1 within a block, Stage 2's avoid-set across branches), and the parameter's own
        // reassignments (below) still apply. Disjoint from the declarator branch: a node is never
        // both a `variable_declarator` and a parameter.
        if is_value_parameter(decl) && !self.sources_within(decl).is_empty() {
            defs.push(Def {
                site: decl,
                rhs: decl,
            });
        }
        for assignment in self.assignments_to(decl) {
            if let Some(rhs) = assignment.child_by_field_name("right") {
                defs.push(Def {
                    site: assignment,
                    rhs,
                });
            }
        }
        defs
    }

    /// Every `assignment_expression` in the enclosing function whose left-hand identifier
    /// resolves to `decl`, in source order. Empty without a root to scan.
    fn assignments_to(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "assignment_expression"
                && let Some(left) = node.child_by_field_name("left")
                && left.kind() == "identifier"
                && JsBindingResolver
                    .declaration_of(self.tree, self.source, left)
                    .is_some_and(|target| target.id() == decl.id())
            {
                found.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        found.sort_by_key(Node::start_byte);
        found
    }

    /// Every assignment in the enclosing function whose left-hand side is a `member_expression`
    /// or `subscript_expression` (`o.secret = …`, `a[0] = …`, `o.total += …`) whose base
    /// identifier resolves to `decl`, in source order. These are the field/index writes: weak
    /// updates, each carrying the access path it writes at (#225), so a read is tainted by the
    /// writes whose path it can observe and by nothing else. Both plain `=` and
    /// augmented `op=` targets count — a field write mutates the object in place either way and
    /// so never kills a prior definition. Empty without a root to scan.
    ///
    /// Disjoint from [`Self::assignments_to`] by left-hand-side kind — an identifier target is a
    /// strong update, a member/subscript target a weak one, and no assignment is both — and from
    /// [`Self::augmented_assignments_to`] by left-hand-side kind for the same reason.
    ///
    /// Each entry is the definition and **the access path it writes at**, relative to `decl`'s
    /// binding: `o.a.b = rhs` yields `[Field("a"), Field("b")]`. The path is what
    /// [`Self::weak_reaching_defs`] compares against the read's path; a write that lands
    /// nowhere comparable to the read contributes nothing (#225).
    fn field_writes_to(&self, decl: Node<'t>) -> Vec<(Def<'t>, Path)> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "assignment_expression" | "augmented_assignment_expression"
            ) && let Some((base, path)) = write_target(node, self.source)
                && let Some(rhs) = node.child_by_field_name("right")
                && JsBindingResolver
                    .declaration_of(self.tree, self.source, base)
                    .is_some_and(|target| target.id() == decl.id())
            {
                found.push((Def { site: node, rhs }, path));
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        found.sort_by_key(|(def, _)| def.site.start_byte());
        found
    }

    /// Every `augmented_assignment_expression` in the enclosing function whose left-hand side is
    /// a plain identifier resolving to `decl` (`x += …`, `x -=`, `x ||=`, `x ??=`, …), in source
    /// order. These are **weak** updates: `x op= rhs` desugars to `x = x op rhs`, whose result is
    /// tainted if *either* the prior `x` or `rhs` is tainted. So such a def contributes `rhs`'s
    /// taint but never *kills* the prior definition of `x` — which keeps reaching the read
    /// through [`Self::reaching_defs`]. Modeling `op=` as a strong (killing) def would silence
    /// `let s = getSecret(); s += "clean"; log(s)`, a false negative and the worst outcome for a
    /// may-taint analysis. Empty without a root to scan.
    ///
    /// Disjoint from [`Self::assignments_to`] by node kind: that finds `assignment_expression`
    /// (a strong `=` update that kills), this finds `augmented_assignment_expression` (a weak
    /// `op=` update). An `op=` with a member/subscript target (`o.x += …`) is left to
    /// [`Self::field_writes_to`] instead, matched there by left-hand-side kind.
    fn augmented_assignments_to(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "augmented_assignment_expression"
                && let Some(left) = node.child_by_field_name("left")
                && left.kind() == "identifier"
                && JsBindingResolver
                    .declaration_of(self.tree, self.source, left)
                    .is_some_and(|target| target.id() == decl.id())
            {
                found.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        found.sort_by_key(Node::start_byte);
        found
    }

    /// The `@source` nodes lying within `expr`'s byte range, in source order. A source that
    /// *is* `expr` is included (its range covers itself), so this subsumes the direct case.
    ///
    /// Attribution: when a source is textually contained in the sink expression, the [`FlowPath`]
    /// `source` reported is that contained source node itself — the sink's other operands (an
    /// identifier alias in `getSecret() + x`) are not separately walked here. Invisible to the
    /// shipped rule, which reports at `path.sink`; relevant to any future rule that reports at
    /// `path.source`.
    fn sources_within(&self, expr: Node<'t>) -> Vec<Node<'t>> {
        let mut found: Vec<Node<'t>> = self
            .sources
            .iter()
            .copied()
            .filter(|source| {
                expr.start_byte() <= source.start_byte() && source.end_byte() <= expr.end_byte()
            })
            .collect();
        found.sort_by_key(Node::start_byte);
        found
    }

    /// Whether a `@sanitizer` call sits between `source` and `expr` — `source ⊆ sanitizer ⊆ expr`
    /// by byte range — so `source`'s contribution to `expr`'s value passes through it and is clean
    /// (#218). Used to cut a sanitizer-wrapped source contained in a compound sink expression,
    /// which `is_member` — matching only when the whole expression *is* the sanitizer call — does
    /// not catch.
    ///
    /// The `sanitizer ⊆ expr` bound is load-bearing, not cosmetic. A sanitizer that merely
    /// *contains* `expr` (an ancestor) cleans its own result, not the side effects of its
    /// arguments: in `redact(o.x = getSecret())` the assignment taints `o` as a side effect, and a
    /// later read of `o` reaches its sink by a def-use edge that never crosses `redact`. That read
    /// evaluates `expr = getSecret()` (the field write's rhs), which no sanitizer lies *within*, so
    /// it is not cut — the may-taint bias is preserved. Nodes nest in a tree, so byte containment
    /// is exact syntactic nesting; no cross-subtree coincidence is possible.
    fn sanitizer_between(&self, source: Node<'t>, expr: Node<'t>) -> bool {
        self.sanitizers.iter().any(|sanitizer| {
            expr.start_byte() <= sanitizer.start_byte()
                && sanitizer.end_byte() <= expr.end_byte()
                && sanitizer.start_byte() <= source.start_byte()
                && source.end_byte() <= sanitizer.end_byte()
        })
    }

    /// The definitions of `decl` that reach a read at `use_node`, by reaching-definitions
    /// with kill: the nearest definition on each control-flow path with no later definition
    /// of the same binding between it and the read.
    ///
    /// Two stages. **Within the read's own block**, the nearest preceding definition is the
    /// unique reaching one — control flowing to the read passes through it, clobbering any
    /// earlier definition in the block and any definition entering from outside, so nothing
    /// else reaches. This is the intra-block-order kill (a later same-block write wins) and
    /// it is what makes the in-place sanitizer and the clean reassignment cut. **Across
    /// blocks**, when no definition precedes the read inside its block, a definition reaches
    /// only along a path that redefines the binding nowhere in between — every *other* block
    /// holding a definition of `decl` is avoided. Path-insensitively: a kill on one branch
    /// does not kill on another, since a single surviving path is a witness.
    ///
    /// Absent a CFG, or a block for the read, every definition is admitted — the
    /// over-approximating may-taint bias, never a false negative.
    fn reaching_defs(&self, decl: Node<'t>, use_node: Node<'t>) -> Vec<Def<'t>> {
        let all = self.definitions_of(decl);
        let use_block = self.cfg.and_then(|cfg| cfg.block_of(use_node));
        let use_pos = use_node.start_byte();

        // A definition "precedes" the read when its store completes before the read: the
        // right-hand side ends at or before the read's first byte. Distinct statements order
        // by position; the one subtlety this closes is a self-referential store like
        // `s = f(s)`, whose right-hand side *contains* the inner read of `s` — that read
        // sees the prior definition, and `rhs.end > use.start` correctly excludes this one.
        // The parameter-origin def (#217) has no CFG block — the parameter sits in the function
        // header, which `cfg_build` attributes to nothing — so map it to the entry block. That
        // makes reaching-definitions treat it as a top-of-body definition: a cross-block
        // sanitizer or reassignment on every path kills it via Stage 2's avoid-set, exactly as
        // it kills a tainted declarator, rather than a block-less def bypassing the kill and
        // being admitted on every path.
        let def_block = |def: &Def<'t>| {
            self.cfg.and_then(|cfg| {
                if def.is_parameter_origin() {
                    Some(cfg.entry())
                } else {
                    cfg.block_of(def.rhs)
                }
            })
        };
        let precedes = |def: &Def<'t>| def.rhs.end_byte() <= use_pos;

        // Stage 1 — the nearest definition preceding the read inside its own block wins
        // outright: it is on every path to the read (a block is straight-line), so it
        // clobbers both earlier in-block definitions and every definition from another block.
        if let Some(use_block) = use_block {
            let nearest = all
                .iter()
                .filter(|def| precedes(def) && def_block(def) == Some(use_block))
                .max_by_key(|def| def.rhs.start_byte());
            if let Some(&nearest) = nearest {
                return vec![nearest];
            }
        }

        // Stage 2 — no definition precedes the read in its block, so reaching definitions
        // arrive from other blocks (or, for a definition at or after the read in the read's
        // own block, only around a loop back-edge). A definition's block must reach the read
        // along a path redefining the binding nowhere between: avoid every other block that
        // holds a definition of `decl`.
        let mut def_blocks: Vec<BlockId> = all.iter().filter_map(def_block).collect();
        def_blocks.sort_unstable();
        def_blocks.dedup();

        all.iter()
            .copied()
            .filter(|def| self.definition_reaches(def_block(def), use_block, &def_blocks))
            .collect()
    }

    /// Whether a definition in block `db` reaches a read in block `ub` without the binding
    /// being redefined in between — the Stage 2 test of [`Self::reaching_defs`].
    fn definition_reaches(
        &self,
        db: Option<BlockId>,
        ub: Option<BlockId>,
        def_blocks: &[BlockId],
    ) -> bool {
        let (Some(cfg), Some(db), Some(ub)) = (self.cfg, db, ub) else {
            // No graph, or an unattributed definition or read: admit it (may-taint bias).
            return true;
        };
        if db == ub {
            // Same block, and Stage 1 already claimed any definition preceding the read, so
            // this one is at or after it — reachable only by a back-edge. Admitted as the
            // sound over-approximation for the loop-carried case (no fixture exercises it;
            // the may-analysis keeps it rather than risk a false negative on a real loop).
            return true;
        }
        // Every *other* definition block is a kill on the way from `db` to `ub`; `db` itself
        // is excluded so a loop back through it re-generates, not kills, and `ub` is excluded
        // because Stage 1 established it holds no definition preceding the read.
        let avoid: Vec<BlockId> = def_blocks
            .iter()
            .copied()
            .filter(|&block| block != db && block != ub)
            .collect();
        cfg.reaches_avoiding(db, &avoid, ub)
    }

    /// The weak (additive) definitions of `decl`'s binding that may reach a read at `use_node`
    /// **and land at a path the read can observe**: the field/index writes
    /// ([`Self::field_writes_to`]) and the identifier augmented assignments
    /// ([`Self::augmented_assignments_to`]). Unlike [`Self::reaching_defs`], a weak update
    /// neither kills nor is killed: every write that may reach the read contributes its
    /// right-hand side's taint, because a field write mutates the object in place rather than
    /// rebinding the name, and an `op=` reads the prior value it adds to.
    ///
    /// Each entry carries the **residual** path to ask the right-hand side about: `read`
    /// with the write's own path stripped off the front, or `[]` when the write is at least as
    /// deep as the read (the read then covers the write entirely). An identifier `op=` writes
    /// at the root, so its residual is the whole read path — `let msg = ""; msg += getSecret();
    /// log(msg.x)` still reports, because a direct source taints every path asked of it.
    fn weak_reaching_defs(
        &self,
        decl: Node<'t>,
        use_node: Node<'t>,
        read: &Path,
    ) -> Vec<(Def<'t>, Path)> {
        self.field_writes_to(decl)
            .into_iter()
            .chain(
                self.augmented_assignments_to(decl)
                    .into_iter()
                    .filter_map(|assignment| {
                        let rhs = assignment.child_by_field_name("right")?;
                        Some((
                            Def {
                                site: assignment,
                                rhs,
                            },
                            Path::new(),
                        ))
                    }),
            )
            .filter_map(|(def, written)| {
                if !prefix_comparable(&written, read) {
                    return None;
                }
                if !self.weak_def_reaches(&def, use_node) {
                    return None;
                }
                let residual = read.get(written.len()..).unwrap_or_default().to_vec();
                Some((def, residual))
            })
            .collect()
    }

    /// Whether a weak (field/index) write may reach a read — the may-reach test for
    /// [`Self::weak_reaching_defs`], with no kill set because a weak update is never clobbered.
    ///
    /// Within the read's own block a write *preceding* the read reaches; a write at or after
    /// the read reaches only when the block sits on a cycle, so a back-edge can carry the value
    /// around — a straight-line later write must not taint an earlier read, the
    /// flow-sensitivity spec §2 promises. Across blocks, plain CFG reachability decides. Absent
    /// a CFG, or for an unattributed read or write, the write is admitted — the may-taint
    /// over-approximation, never a false negative.
    fn weak_def_reaches(&self, def: &Def<'t>, use_node: Node<'t>) -> bool {
        let Some(cfg) = self.cfg else {
            return true;
        };
        match (cfg.block_of(def.rhs), cfg.block_of(use_node)) {
            (Some(db), Some(ub)) if db == ub => {
                def.rhs.end_byte() <= use_node.start_byte() || block_in_cycle(cfg, db)
            }
            (Some(db), Some(ub)) => cfg.reaches(db, ub),
            _ => true,
        }
    }
}

/// Whether `node` is one of `set`, by tree-unique node identity.
fn is_member(node: Node<'_>, set: &[Node<'_>]) -> bool {
    set.iter().any(|member| member.id() == node.id())
}

/// The base identifier a member or subscript access is rooted at, and the access path from it
/// to `node`: `o.a.b` is `(o, [Field("a"), Field("b")])`, `o[i].c` is `(o, [Index,
/// Field("c")])`, and a bare `o` is `(o, [])`.
///
/// A taint-transparent wrapper in the base chain is peeled (#246): `(o).secret`,
/// `(o as T).secret`, `o!.token` and `(await o).x` root at the same binding and path as the
/// unwrapped form, so a cast-then-access is not silently opaque. This matches [`taint_of`]'s
/// transparency for a wrapper read whole — a wrapper is its inner value as a base too.
///
/// `None` when the base is not a plain identifier and is not one of those wrappers — a call
/// result, `this`, an object or array literal used directly as a base (`{ … }.k`, a rare shape)
/// — because there is then no binding for taint to attach to.
///
/// The result is [`truncate`]d here, at the one place a path is built from syntax, so no
/// caller can hold a path longer than [`MAX_PATH_LEN`] and no comparison downstream has to
/// re-apply the widening.
fn base_and_path<'t>(node: Node<'t>, source: &str) -> Option<(Node<'t>, Path)> {
    let mut segments = Path::new();
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" => {
                // Collected innermost-first while walking `object` links outward-in; the
                // path is stated outermost-first.
                segments.reverse();
                return Some((current, truncate(segments)));
            }
            "member_expression" => {
                segments.push(property_segment(current, source));
                current = current.child_by_field_name("object")?;
            }
            "subscript_expression" => {
                segments.push(Seg::Index);
                current = current.child_by_field_name("object")?;
            }
            // A wrapper contributes no segment; unwrap it and keep walking the base chain. Each
            // step moves strictly inward, so the loop still terminates on the finite tree.
            "parenthesized_expression"
            | "non_null_expression"
            | "await_expression"
            | "as_expression"
            | "satisfies_expression" => current = transparent_inner(current)?,
            _ => return None,
        }
    }
}

/// The base binding an assignment writes *through*, and the access path it writes at:
/// `o.a.b = x` is `(o, [Field("a"), Field("b")])` and `a[0] = x` is `(a, [Index])`.
///
/// `None` for an identifier target, which is a strong update belonging to
/// [`Taint::assignments_to`] or [`Taint::augmented_assignments_to`], and `None` for a target
/// not rooted at a plain identifier. Splitting weak from strong by left-hand-side kind is
/// what keeps the two sets disjoint, exactly as before C1.
fn write_target<'t>(assignment: Node<'t>, source: &str) -> Option<(Node<'t>, Path)> {
    let left = assignment.child_by_field_name("left")?;
    if !matches!(left.kind(), "member_expression" | "subscript_expression") {
        return None;
    }
    base_and_path(left, source)
}

/// The segment a `member_expression`'s property contributes: [`key_segment`] of its `property`
/// child, and [`Seg::Index`] when it has none.
fn property_segment(member: Node<'_>, source: &str) -> Seg {
    member
        .child_by_field_name("property")
        .map_or(Seg::Index, |property| key_segment(property, source))
}

/// The segment a named key contributes — a `member_expression`'s property or an object literal
/// `pair`'s key.
///
/// A plain `property_identifier` is the name. Anything else the grammar can put in that slot — a
/// private name `o.#k`, a string or number key, a computed `[k]` — folds to [`Seg::Index`],
/// which compares equal to every other segment: an over-approximation, and the sound direction,
/// rather than inventing a name that two different constructs might collide on. In particular a
/// string key `{ "cause": … }` folds to `Index` because the matching subscript read `o["cause"]`
/// does too, so the two still meet.
fn key_segment(key: Node<'_>, source: &str) -> Seg {
    if key.kind() == "property_identifier" {
        Seg::Field(source[key.byte_range()].to_owned())
    } else {
        Seg::Index
    }
}

/// The value expression of a taint-transparent wrapper — `(e)`, `e!`, `await e`, `e as T`,
/// `e satisfies T` — whose result *is* that inner expression's value.
///
/// For a cast the type child follows the value in source order, so the value is the first named
/// child; for the others there is one named child (a `parenthesized_expression`'s optional `type`
/// field aside, which is skipped so a type annotation is never mistaken for the value).
fn transparent_inner(expr: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = expr.walk();
    expr.named_children(&mut cursor)
        .find(|child| child.kind() != "type_annotation")
}

/// The residual path with which a literal member sitting at segment `key` is read, given the
/// caller's `path` — or `None` when the read cannot observe that member (#246).
///
/// A read at `[]` observes the member whole, residual `[]`. A read at `[head, …rest]` observes
/// the member iff `head` is comparable to `key` by the [`Seg::Index`]-wildcard rule of
/// [`prefix_comparable`], with residual `rest`. The residual is a suffix of `path`, so no path
/// grows and no truncation is needed.
fn read_under(path: &Path, key: &Seg) -> Option<Path> {
    match path.split_first() {
        None => Some(Vec::new()),
        Some((head, rest)) => seg_comparable(head, key).then(|| rest.to_vec()),
    }
}

/// Whether two single segments are comparable — equal, or either an [`Seg::Index`] wildcard. The
/// per-segment core [`prefix_comparable`] applies down a whole path.
fn seg_comparable(a: &Seg, b: &Seg) -> bool {
    matches!((a, b), (Seg::Index, _) | (_, Seg::Index)) || a == b
}

/// Whether `expr` is a `member_expression` whose property names one of [`SHAPE_PROPERTIES`].
///
/// The guard on `property_identifier` is what keeps this to the dotted form. A
/// `subscript_expression` — `s["length"]` — is not cleaned: its index is an arbitrary
/// expression rather than a name, and reading a name out of it would mean partially
/// evaluating the program. Pinned by `a_subscripted_shape_property_still_reports`. Only the
/// outermost read is examined: `s.length.raw` is a read at `[length, raw]` and is not cut.
fn is_shape_property_read(expr: Node<'_>, source: &str) -> bool {
    if expr.kind() != "member_expression" {
        return false;
    }
    let Some(property) = expr.child_by_field_name("property") else {
        return false;
    };
    if property.kind() != "property_identifier" {
        return false;
    }
    SHAPE_PROPERTIES
        .binary_search(&&source[property.byte_range()])
        .is_ok()
}

/// Whether `decl` is a value-parameter binding node — the declaration [`JsBindingResolver`] returns
/// for a parameter use. In the TS/TSX grammar that is a `required_parameter`/`optional_parameter`
/// (a bare `identifier` in plain JS) directly under a `formal_parameters` list, or the bare
/// identifier of an unparenthesized arrow's `parameter` field. These are the only bindings seeded
/// as tainted-from-entry when captured as a `@source` (#217). The check excludes a body-spanning
/// binding such as a function name, whose declaration node would let `sources_within` mistake any
/// source in the body for the binding itself; a parameter's node covers only the parameter.
fn is_value_parameter(decl: Node<'_>) -> bool {
    decl.parent().is_some_and(|parent| match parent.kind() {
        "formal_parameters" => true,
        // The singular `parameter` field of an unparenthesized arrow (`sk => …`). Guarding on the
        // field id keeps the arrow *body* — also a child of `arrow_function` — from matching.
        "arrow_function" => {
            parent.child_by_field_name("parameter").map(|p| p.id()) == Some(decl.id())
        }
        _ => false,
    })
}

/// Whether `block` sits on a cycle: control can return to it from one of its own successors.
/// [`Cfg::reaches`] is reflexive, so it cannot answer this directly — the walk instead starts
/// one step out, at each successor, and asks whether that successor reaches `block` again.
fn block_in_cycle(cfg: &Cfg<'_>, block: BlockId) -> bool {
    cfg.block(block)
        .successors
        .iter()
        .any(|edge| cfg.reaches(edge.target, block))
}

/// The nearest enclosing function's root node and its CFG, walking ancestors until
/// [`Cfg::build`] accepts one as a root kind. `program` is a root, so a match is always found
/// for a node in a parsed tree.
fn enclosing_root_and_cfg<'t>(source: &str, node: Node<'t>) -> Option<(Node<'t>, Cfg<'t>)> {
    let mut current = Some(node);
    while let Some(node) = current {
        if let Some(cfg) = Cfg::build(source, node) {
            return Some((node, cfg));
        }
        current = node.parent();
    }
    None
}

/// The total order [`canonicalize`] reduces raw flows by: the `(source, sink)` pair, then the
/// chain length, then the chain's positions, then the access path the fact was found at.
type CanonicalKey = ((usize, usize, usize, usize), usize, Vec<usize>, Path);

/// One raw flow's canonical key.
///
/// The trailing [`Path`] separates two flows that share source, sink and positions but were
/// reached at different paths; a flow that ties on all of it is ordered by the walk, which is
/// itself deterministic. Two facts agreeing on source, sink, chain length and every step
/// position but found at different access paths were previously separated by input order alone
/// — `sort_by` is stable, so whichever the walk produced first won. That is not a difference
/// any output can show, since [`FlowPath`] carries no path; it is a difference in what the
/// *code states*, and stating it is cheap. `two_flows_differing_only_in_their_access_path_
/// have_different_keys` is what holds it.
fn canonical_key(entry: &(FlowPath<'_>, Path)) -> CanonicalKey {
    let (flow, path) = entry;
    (
        pair_key(flow),
        flow.steps.len(),
        step_positions(flow),
        path.clone(),
    )
}

/// Reduce raw flows to one canonical [`FlowPath`] per `(source, sink)`: keep the shortest
/// `steps` chain (ties by the first differing step's start byte, then by access path), drop
/// exact duplicates, and emit in `(sink start, source start)` order. Determinism rests on this
/// sort and on nothing here iterating a hash container.
fn canonicalize(mut flows: Vec<(FlowPath<'_>, Path)>) -> Vec<FlowPath<'_>> {
    // Group identical `(source, sink)` pairs together with the shortest chain first, so the
    // dedup below keeps the shortest. Node ranges identify source and sink; `steps` positions
    // and the access path give a total order for the two-runs-identical guarantee.
    flows.sort_by_cached_key(canonical_key);
    flows.dedup_by(|a, b| pair_key(&a.0) == pair_key(&b.0));
    let mut flows: Vec<FlowPath<'_>> = flows.into_iter().map(|(flow, _)| flow).collect();
    // Final output order: sink position, then source position (ranges break ties totally).
    flows.sort_by_key(|flow| {
        (
            flow.sink.start_byte(),
            flow.source.start_byte(),
            flow.sink.end_byte(),
            flow.source.end_byte(),
        )
    });
    flows
}

/// The `(source range, sink range)` identity of a flow — what makes two flows the same
/// `(source, sink)` pair.
fn pair_key(flow: &FlowPath<'_>) -> (usize, usize, usize, usize) {
    (
        flow.source.start_byte(),
        flow.source.end_byte(),
        flow.sink.start_byte(),
        flow.sink.end_byte(),
    )
}

/// A flow's step start bytes, for a deterministic tie-break between equal-length chains.
fn step_positions(flow: &FlowPath<'_>) -> Vec<usize> {
    flow.steps.iter().map(Node::start_byte).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::testing::{find_all, parse};

    /// The identifier a call's callee names, when it is a plain identifier.
    fn callee_name<'a>(call: Node<'_>, source: &'a str) -> Option<&'a str> {
        let callee = call.child_by_field_name("function")?;
        (callee.kind() == "identifier").then(|| &source[callee.byte_range()])
    }

    /// Every `call_expression` whose callee identifier is exactly `name`, source order.
    fn calls_named<'t>(tree: &'t Tree, source: &str, name: &str) -> Vec<Node<'t>> {
        find_all(tree, "call_expression")
            .into_iter()
            .filter(|call| callee_name(*call, source) == Some(name))
            .collect()
    }

    /// The first-argument expression of every call to `name`, source order.
    fn sink_args<'t>(tree: &'t Tree, source: &str, name: &str) -> Vec<Node<'t>> {
        calls_named(tree, source, name)
            .into_iter()
            .filter_map(|call| {
                call.child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0))
            })
            .collect()
    }

    /// Parse `src`, pick source/sink/sanitizer nodes by callee name, run the analyzer, and
    /// return `(source text, sink text)` for each reported flow. Stable across 4a–4e.
    fn run(
        src: &str,
        source_name: &str,
        sink_name: &str,
        sanitizer_name: &str,
    ) -> Vec<(String, String)> {
        let tree = parse(src);
        let sources = calls_named(&tree, src, source_name);
        let sinks = sink_args(&tree, src, sink_name);
        let sanitizers = calls_named(&tree, src, sanitizer_name);
        JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| {
                (
                    src[flow.source.byte_range()].to_owned(),
                    src[flow.sink.byte_range()].to_owned(),
                )
            })
            .collect()
    }

    /// Like [`run`], but also reporting each flow's `steps` length — for the canonical-path
    /// assertions where the number of hops matters.
    fn run_full(
        src: &str,
        source_name: &str,
        sink_name: &str,
        sanitizer_name: &str,
    ) -> Vec<(String, String, usize)> {
        let tree = parse(src);
        let sources = calls_named(&tree, src, source_name);
        let sinks = sink_args(&tree, src, sink_name);
        let sanitizers = calls_named(&tree, src, sanitizer_name);
        JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| {
                (
                    src[flow.source.byte_range()].to_owned(),
                    src[flow.sink.byte_range()].to_owned(),
                    flow.steps.len(),
                )
            })
            .collect()
    }

    #[test]
    fn two_flows_differing_only_in_their_access_path_have_different_keys() {
        // Totality of the canonical order. Before the path joined the key these two compared
        // equal, and which survived dedup was whichever the walk produced first — a choice
        // nothing stated. Asserted on the key rather than on `canonicalize`'s output, because
        // `FlowPath` carries no path and the output cannot show the difference.
        let src = "function f(){ const o = {}; o.a = getSecret(); log(o.a); }";
        let tree = parse(src);
        let source = calls_named(&tree, src, "getSecret")
            .into_iter()
            .next()
            .expect("one source");
        let sink = sink_args(&tree, src, "log")
            .into_iter()
            .next()
            .expect("one sink");
        let flow = || FlowPath {
            source,
            sink,
            steps: Vec::new(),
        };
        let root = (flow(), Path::new());
        let nested = (flow(), vec![Seg::Field(String::from("a"))]);
        assert_ne!(canonical_key(&root), canonical_key(&nested));
    }

    #[test]
    fn a_source_used_directly_as_a_sink_argument_reports() {
        // log(getSecret()) — the sink argument *is* the source call.
        let flows = run(
            "function f() { log(getSecret()); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn taint_through_one_assignment_reports() {
        let flows = run(
            "function f() { const s = getSecret(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn taint_through_two_assignments_reports() {
        let flows = run(
            "function f() { const s = getSecret(); const t = s; log(t); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_clean_local_does_not_report() {
        // const s = clean(); log(s); — clean() is neither a source nor a sanitizer.
        let flows = run(
            "function f() { const s = clean(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    #[test]
    fn a_sanitizer_before_the_sink_cuts() {
        // reads the *clean* value c, not s → silent.
        let flows = run(
            "function f() { const s = getSecret(); const c = redact(s); log(c); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    #[test]
    fn a_sanitizer_after_the_sink_does_not_cut() {
        // log reads s while it is still tainted; redact runs afterward.
        let flows = run(
            "function f() { const s = getSecret(); log(s); const c = redact(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn a_sanitizer_wrapping_a_source_cuts() {
        // redact(getSecret()) textually wraps the source; the cut must win over containment,
        // or c is tainted. This is what makes the sanitizer check load-bearing: without it,
        // the wrapped source would taint c.
        let flows = run(
            "function f() { const c = redact(getSecret()); log(c); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty());
    }

    // --- #218: a sanitizer wrapping a source inside a compound sink expression cuts ---------
    //
    // The `is_member` cut fires only when the sink node *itself* is the sanitizer call. A source
    // wrapped by a sanitizer *inside* a larger sink expression — `log(redact(getSecret()) + "x")`,
    // or `` `…${describeBytes(account.secretKey)}…` `` in the corpus — bypassed it and reported on
    // syntactic containment alone: a false positive on correctly-sanitized code (#195,
    // migrateLegacyAccount.ts:81). `taint_of` now drops a contained source lying within a sanitizer
    // call's range.

    #[test]
    fn a_sanitizer_wrapping_a_source_in_a_binary_sink_cuts() {
        // `log(redact(getSecret()) + "x")`: the sink is a binary expression containing the
        // sanitizer call, so `is_member` does not fire — the wrapped source must still be cut.
        let flows = run(
            "function f() { log(redact(getSecret()) + \"x\"); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a sanitizer-wrapped source in a compound sink must not report"
        );
    }

    #[test]
    fn a_sanitizer_wrapping_a_source_in_a_template_sink_cuts() {
        // The migrateLegacyAccount.ts:81 shape: the sink is a template literal containing
        // `redact(getSecret())`; the sanitizer's result is what reaches the string, not the secret.
        let flows = run(
            "function f() { log(`x=${redact(getSecret())}`); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a sanitizer-wrapped source in a template sink must not report"
        );
    }

    #[test]
    fn a_bare_source_beside_a_sanitized_one_in_a_compound_sink_reports_the_bare_one() {
        // Per-source: `redact(getSecret()) + getSecret()` — the wrapped source is cut, the bare
        // one is not. Exactly one flow, and it must be the *unwrapped* read — pinned by byte
        // offset, since the two sources render identical text and a len==1 assert alone would pass
        // even if the wrong (wrapped) source survived.
        let src = "function f() { log(redact(getSecret()) + getSecret()); }";
        let tree = parse(src);
        let sources = calls_named(&tree, src, "getSecret");
        assert_eq!(sources.len(), 2, "two source reads in the fixture");
        let sanitizers = calls_named(&tree, src, "redact");
        let bare = *sources
            .iter()
            .find(|s| {
                !sanitizers
                    .iter()
                    .any(|z| z.start_byte() <= s.start_byte() && s.end_byte() <= z.end_byte())
            })
            .expect("one getSecret is outside redact");
        let sinks = sink_args(&tree, src, "log");
        let flows = JsFlowAnalyzer.analyze(&tree, src, &sources, &sinks, &sanitizers);
        assert_eq!(flows.len(), 1, "only the unwrapped source reports");
        assert_eq!(
            flows[0].source.start_byte(),
            bare.start_byte(),
            "the surviving source is the bare one, not the sanitizer-wrapped one"
        );
    }

    #[test]
    fn a_source_leaking_out_of_a_sanitizer_via_a_side_effect_still_reports() {
        // Soundness (found in adversarial review of #218): the sanitizer cleans redact's *return*,
        // but the assignment `o.secret = getSecret()` nested in its argument taints `o` as a side
        // effect — a def-use edge that bypasses the sanitizer's result. A global "source is
        // lexically inside a sanitizer" cut wrongly silences this; the cut must require the
        // sanitizer to sit between the source and the expression being evaluated, which a
        // self-contained `getSecret()` (the field write's rhs) has no room for.
        // The sink reads `o.secret` — the field the side effect wrote — rather than `o.public`.
        // Under C1 (#225) an `o.public` read is silent because `[public]` and `[secret]` are
        // incomparable paths, which would leave this test green for a reason that has nothing
        // to do with #218: the sanitizer cut it was written to catch would be untested.
        // Reading the written path keeps the property, and the property is the point.
        let flows = run(
            "function f() { const o = {}; redact(o.secret = getSecret()); log(o.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a secret leaking via a side effect inside a sanitizer argument still reports"
        );
    }

    #[test]
    fn a_source_leaking_via_an_identifier_assignment_in_a_sanitizer_still_reports() {
        // The strong-update sibling of the side-effect case: `x = getSecret()` nested in the
        // sanitizer argument rebinds `x` to the raw secret (a reaching-definition, not a weak
        // field write), which reaches `log(x)` without crossing the sanitizer's result.
        let flows = run(
            "function f() { let x; redact(x = getSecret()); log(x); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "an identifier rebind inside a sanitizer argument still reports"
        );
    }

    #[test]
    fn an_unsanitized_source_in_a_binary_sink_still_reports() {
        // No-false-negative guard: no sanitizer wraps the source, so containment still reports.
        let flows = run(
            "function f() { log(getSecret() + \"x\"); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "an unsanitized contained source still reports"
        );
    }

    #[test]
    fn an_unsanitized_source_in_a_template_sink_still_reports() {
        // No-false-negative guard for the template shape.
        let flows = run(
            "function f() { log(`${getSecret()}`); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "an unsanitized template source still reports"
        );
    }

    #[test]
    fn a_const_alias_propagates_taint() {
        let flows = run(
            "function f() { const a = getSecret(); const b = a; log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn aliasing_through_a_call_does_not_propagate() {
        // Documented v1 false NEGATIVE: identity(a) is not tracked.
        let flows = run(
            "function f() { const a = getSecret(); const b = identity(a); log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "v1 does not follow taint through a call");
    }

    #[test]
    fn two_sources_into_one_sink_dedup_deterministically() {
        // both branches taint s; path-insensitive → both reach log(s).
        let src = "function f(c) { let s; if (c) { s = getSecret(); } else { s = getSecret(); } log(s); }";
        let flows = run(src, "getSecret", "log", "redact");
        // One canonical flow per (source, sink); ordered by source position; no duplicates.
        assert_eq!(flows.len(), 2);
        assert!(flows[0].1 == flows[1].1, "same sink");
        // The two sources are the two distinct getSecret() calls, in source order.
        assert!(
            flows[0].0.starts_with("getSecret") && flows[1].0.starts_with("getSecret"),
            "both flows originate at a source"
        );
        // Determinism: running twice gives identical ordering.
        let again = run(src, "getSecret", "log", "redact");
        assert_eq!(flows, again);
    }

    #[test]
    fn one_source_reaching_a_sink_two_ways_is_deduplicated() {
        // Both branches alias the same `a = getSecret()` into `s`, so one (source, sink)
        // pair is reached by two chains. Path-insensitive union then dedup → a single flow.
        let src = "function f(c) { const a = getSecret(); let s; if (c) { s = a; } else { s = a; } log(s); }";
        let flows = run(src, "getSecret", "log", "redact");
        assert_eq!(
            flows.len(),
            1,
            "duplicate (source, sink) pairs collapse to one"
        );
        assert_eq!(flows[0].0, "getSecret()");
        assert_eq!(
            run(src, "getSecret", "log", "redact"),
            flows,
            "deterministic"
        );
    }

    #[test]
    fn steps_are_the_shortest_chain() {
        // When two def-use chains reach the sink, `steps` is the shortest; tie broken by
        // position. Here log(a) reads the source directly (empty steps) while log(b) reads
        // it through one alias (one step).
        let flows = run_full(
            "function f() { const a = getSecret(); const b = a; log(a); log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 2);
        // Canonical order is by sink position: log(a) precedes log(b).
        assert_eq!(flows[0].1, "a");
        assert_eq!(flows[0].2, 0, "log(a) reads the source directly");
        assert_eq!(flows[1].1, "b");
        assert_eq!(flows[1].2, 1, "log(b) reads it through one alias");
    }

    #[test]
    fn sanitize_in_place_kills_the_declarator() {
        // Case (a): reassigning `s` to `redact(s)` clobbers the tainted declarator. The sink
        // reads the sanitized value, so this is silent — matching the new-variable form
        // `const c = redact(s); log(c)`, which was already clean. The two forms must agree.
        let flows = run(
            "function f() { let s = getSecret(); s = redact(s); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "an in-place sanitizer must kill the taint"
        );
    }

    #[test]
    fn reassign_to_a_clean_value_kills_taint() {
        // Case (b): a later clean redefinition kills taint (spec §5).
        let flows = run(
            "function f() { let s = getSecret(); s = \"public\"; log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "a clean reassignment must kill the taint");
    }

    #[test]
    fn a_tainted_def_last_in_the_block_is_not_killed() {
        // Soundness guard 1: the tainted def is the LAST in the block, so it reaches the
        // use. A kill that ignored intra-block statement order would wrongly drop it.
        let flows = run(
            "function f() { let s = \"public\"; s = getSecret(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the last def before the use must survive");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn sanitize_then_retaint_in_one_block_reports() {
        // Soundness guard 2: sanitize, then re-taint, in one block. The re-taint is the
        // nearest preceding def of the use and must report.
        let flows = run(
            "function f(x) { let s = redact(x); s = getSecret(); log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "the re-taint must survive the earlier sanitize"
        );
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_partial_sanitize_on_one_branch_still_reports() {
        // Path-insensitivity: the path skipping the `if` reaches the sink with the tainted
        // declarator, so a sanitize on only one branch does not kill.
        let flows = run(
            "function f(c) { let s = getSecret(); if (c) { s = redact(s); } log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the branch-skipping path keeps the taint");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn sanitizing_both_branches_kills_taint() {
        // Every path from the tainted declarator to the sink passes a sanitizer, so the
        // reaching-defs kill removes it on both branches.
        let flows = run(
            "function f(c) { let s = getSecret(); if (c) { s = redact(s); } else { s = redact(s); } log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a sanitizer on every branch kills the taint"
        );
    }

    #[test]
    fn steps_are_the_shorter_of_two_unequal_chains() {
        // One (source, sink) pair reached by a one-hop chain (then) and a two-hop chain
        // (else). Canonicalization keeps the shorter, locking "strictly shorter wins" at
        // unequal lengths — `steps_are_the_shortest_chain` only exercises equal lengths.
        let flows = run_full(
            "function f(c) { const a = getSecret(); let s; if (c) { s = a; } else { const b = a; s = b; } log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "the two chains collapse to one (source, sink)"
        );
        assert_eq!(flows[0].0, "getSecret()");
        assert_eq!(flows[0].2, 1, "the shorter (one-hop) chain wins");
    }

    #[test]
    fn a_propagating_reassignment_on_every_branch_still_reports() {
        // Soundness of the avoid-based kill. Both branches redefine `s`, so the declarator's
        // block is avoided on every path to the sink and does not itself reach it. The kill
        // must not lose the flow: each `s = t` reassignment is a def that *reaches* the sink
        // and carries taint (t aliases the source), so reaching-definitions surfaces it
        // through the intervening def. A kill that only asked "does the original source
        // survive" would drop this — a false negative.
        let flows = run(
            "function f(c) { let s = getSecret(); let t = s; if (c) { s = t; } else { s = t; } log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a taint-propagating reassignment on every branch still reaches the sink"
        );
        assert_eq!(flows[0].0, "getSecret()");
    }

    // --- Field- and index-sensitivity (#194 §11, then #225) --------------------------------
    //
    // Field-sensitive to depth three and index-insensitive (#225): a field write taints the
    // path it writes at and every path above or below it; a subscript is an unknown key and so
    // meets every path; two known keys that differ are incomparable. A field write is a *weak*
    // update — it adds taint from that point forward and never kills a prior definition, unlike
    // a full identifier reassignment (a strong update / reaching-def kill).

    #[test]
    fn a_field_write_does_not_taint_a_sibling_field_read() {
        // Was `a_field_write_taints_the_object_and_a_later_field_read_reports`, the #194 §11
        // over-approximation. C1 (#225) makes `[secret]` and `[public]` incomparable paths, so
        // the sibling read is clean. This is the promise change the sensitivity table and
        // `docs/built-in-rules.md` now state.
        let flows = run(
            "function f(){ const o = {}; o.secret = getSecret(); log(o.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "`o.public` and `o.secret` are incomparable paths"
        );
    }

    #[test]
    fn reading_the_written_field_reports() {
        let flows = run(
            "function f(){ const o = {}; o.secret = getSecret(); log(o.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_base_that_is_never_tainted_does_not_report() {
        // Negative: no write taints `o`, so a field read is clean. Guards the read side
        // against reporting on any member access.
        let flows = run(
            "function f(){ const o = {}; log(o.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "an untainted base must not report");
    }

    #[test]
    fn a_clean_field_write_does_not_kill_an_unrelated_taint() {
        // Weak-update soundness: the clean field write `o.x = "clean"` must not touch the
        // taint on the separate binding `s`.
        let flows = run(
            "function f(){ let s = getSecret(); const o = {}; o.x = \"clean\"; log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a field write to o must not kill s's taint");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_field_write_does_not_kill_a_prior_write_to_the_same_object() {
        // Weak-update soundness: `o.a = getSecret()` taints o; the later clean `o.b = "clean"`
        // is a weak update and must NOT clobber it (a strong reassignment would). Reading
        // `o.a` still reports.
        let flows = run(
            "function f(){ const o = {}; o.a = getSecret(); o.b = \"clean\"; log(o.a); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a weak field write must not kill prior taint"
        );
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn an_index_write_taints_the_whole_array() {
        // Index-insensitive (spec §2): `a[0] = getSecret()` taints `a`; reading `a[1]` is
        // tainted.
        //
        // C1 (#225) does not change this, and that is the design rather than an oversight:
        // every subscript collapses to `Seg::Index`, so `a[0]` and `a[1]` are one abstract
        // path and compare equal. Buying index sensitivity would mean evaluating the
        // subscript expression, which is a different analysis; `docs/architecture.md` §4
        // still says "Index-sensitive: no" and this fixture is what holds it there.
        let flows = run(
            "function f(){ const a = []; a[0] = getSecret(); log(a[1]); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a subscript write taints the whole array");
        assert_eq!(flows[0].0, "getSecret()");
    }

    // --- C1 (#225): taint per access path --------------------------------------------------

    #[test]
    fn a_nested_write_reports_at_its_own_path() {
        // GUARD, green before C1 and after: the write and the read are the same path.
        let flows = run(
            "function f(){ const o = { a: {} }; o.a.b = getSecret(); log(o.a.b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_nested_write_is_silent_at_a_sibling_of_its_last_segment() {
        // `[a, b]` and `[a, c]` share a prefix and then diverge, so they are incomparable.
        let flows = run(
            "function f(){ const o = { a: {} }; o.a.b = getSecret(); log(o.a.c); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "the paths diverge at the second segment");
    }

    #[test]
    fn a_read_above_a_nested_write_reports() {
        // Upward closure: reading `o.a` reads a value that has the secret at `.b`, so the
        // shorter read path is prefix-comparable with the longer written one and reports.
        let flows = run(
            "function f(){ const o = { a: {} }; o.a.b = getSecret(); log(o.a); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a read above a write covers it");
    }

    #[test]
    fn a_read_below_a_write_reports() {
        // Downward closure, the half `docs/superpowers/specs/...` §4.1 says must not be
        // dropped: a root taint covers every path under it, or `log(s.mnemonic)` would go
        // silent and C2's control fixture with it.
        let flows = run(
            "function f(){ const o = {}; o.a = getSecret(); log(o.a.b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a write above a read covers it");
    }

    #[test]
    fn an_alias_reads_the_written_path_and_not_its_sibling() {
        // The alias pair: `p` is `o`, so a read through `p` asks `o` the same path question.
        let tainted = run(
            "function f(){ const o = {}; o.secret = getSecret(); const p = o; log(p.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            tainted.len(),
            1,
            "a read through `p` asks `o` at `[secret]`"
        );
        let clean = run(
            "function f(){ const o = {}; o.secret = getSecret(); const p = o; log(p.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            clean.is_empty(),
            "a read through `p` asks `o` at `[public]`"
        );
    }

    #[test]
    fn an_alias_to_a_member_composes_the_two_paths() {
        // Composition: `p` is `o.inner`, so `p.public` is `o.inner.public` — incomparable with
        // the write at `o.inner.secret`. Without composing the alias's own path onto the
        // read's, the question asked of `o` would be `[inner]` alone, which *is* comparable
        // with `[inner, secret]`, and this would report. That is what makes this fixture
        // discriminating where its sibling below is only a guard.
        let clean = run(
            "function f(){ const o = { inner: {} }; o.inner.secret = getSecret(); \
             const p = o.inner; log(p.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(clean.is_empty(), "`p.public` is `o.inner.public`");
        let tainted = run(
            "function f(){ const o = { inner: {} }; o.inner.secret = getSecret(); \
             const p = o.inner; log(p.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(tainted.len(), 1, "`p.secret` is `o.inner.secret`");
    }

    #[test]
    fn an_alias_taken_before_the_write_does_not_see_it() {
        // Documented v1 false NEGATIVE, and pre-existing rather than introduced by C1: the
        // def-use walk asks the weak-reaching-definitions question at the inner `o` read
        // inside `const p = o`, and a write that comes after that read does not reach it. In
        // JavaScript `p` and `o` are the same object and the write *is* visible through `p`.
        // Pinned so nobody reads the alias fixtures above as a claim that ordering is free.
        let flows = run(
            "function f(){ const o = {}; const p = o; o.secret = getSecret(); log(p.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "an alias taken before the write is a known intra-procedural limit"
        );
    }

    #[test]
    fn a_write_through_an_alias_does_not_reach_the_base() {
        // Pre-existing limit, pinned rather than discovered: `field_writes_to` resolves the
        // write's own base identifier, so `p.secret = …` is a write on `p`'s binding and a read
        // of `o.secret` never sees it — while `log(p.secret)` in the same program reports. The
        // alias fixtures above read *through* `p`; that direction is the one they assert.
        let through_alias = run(
            "function f(){ const o = {}; const p = o; p.secret = getSecret(); log(p.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(through_alias.len(), 1);
        let at_base = run(
            "function f(){ const o = {}; const p = o; p.secret = getSecret(); log(o.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            at_base.is_empty(),
            "a write through the alias is not a write on `o`"
        );
    }

    #[test]
    fn a_write_past_the_widening_bound_matches_every_sibling_below_it() {
        // `o.a.b.c.d` and `o.a.b.c.e` both truncate to `[a, b, c]`, so the write matches the
        // read: today's whole-object behavior, restored locally at depth. This is what a
        // stated widening buys — an answer that is the same every run, rather than one that
        // depends on how far the walk got.
        let flows = run(
            "function f(){ const o = { a: { b: { c: {} } } }; o.a.b.c.d = getSecret(); \
             log(o.a.b.c.e); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "both paths truncate to [a, b, c]");
    }

    #[test]
    fn the_widening_bound_is_three_and_not_four() {
        // The companion to the fixture above; alone this rules out a bound of two or fewer,
        // where both would truncate to `[a, b]` and report; the companion
        // `a_write_past_the_widening_bound_matches_every_sibling_below_it` rules out four or
        // more, and the pair pins three.
        let flows = run(
            "function f(){ const o = { a: { b: { c: {} } } }; o.a.b.c.d = getSecret(); \
             log(o.a.b.x); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "the paths diverge at the third segment");
    }

    #[test]
    fn a_cyclic_object_graph_terminates_and_reports_identically_twice() {
        // `o.next = o` inside a loop is the only shape that can recur without bound: the
        // write reaches its own right-hand side through the back edge, and the read path it
        // is asked about stops shrinking once it reaches `[]`. `MAX_DEPTH` is the backstop,
        // and the answer is the same every run because nothing here counts visits.
        //
        // A straight-line `o.next = o` cannot recur at all — `weak_def_reaches` refuses a
        // write that does not precede the read — so the loop is what makes this a cycle.
        let src = "function f(c){ const o = {}; o.secret = getSecret(); \
                   while (c) { o.next = o; } log(o.next); }";
        let flows = run(src, "getSecret", "log", "redact");
        assert_eq!(
            flows.len(),
            1,
            "the root taint is covered by the read, once, through the cycle"
        );
        assert_eq!(
            run(src, "getSecret", "log", "redact"),
            flows,
            "a cyclic graph reports identically on a second run"
        );
    }

    #[test]
    fn a_loop_of_self_referential_field_writes_terminates_promptly() {
        // Three self-referential writes in a loop were exponential in `MAX_DEPTH` (3^16
        // recursions) before the repeated-question cut, and no budget reaches the analyzer.
        // Ten seconds is not a performance claim — the cut makes this milliseconds — it is the
        // line between "terminates" and "hangs the run".
        let started = std::time::Instant::now();
        let flows = run(
            "function f(c){ const o = {}; o.secret = getSecret(); \
             while (c) { o.f0 = o; o.f1 = o; o.f2 = o; o.f3 = o; } log(o); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the walk did not terminate promptly"
        );
        assert_eq!(
            flows.len(),
            1,
            "the secret written before the loop still reaches the sink"
        );
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_computed_key_write_is_read_by_name() {
        // The shape the pre-C1 analysis caught and a known-segment `Index` silenced: a secret
        // stored under a computed key and read by its name. `Index` is an unknown key, so it
        // meets every field.
        for source in [
            "function f(){ const o = {}; o[\"secret\"] = getSecret(); log(o.secret); }",
            "function f(ks){ const o = {}; for (const k of ks) { o[k] = getSecret(); } log(o.secret); }",
            "function f(k){ const o = { a: {} }; o.a[k] = getSecret(); log(o.a.secret); }",
        ] {
            let flows = run(source, "getSecret", "log", "redact");
            assert_eq!(flows.len(), 1, "{source}");
        }
    }

    #[test]
    fn a_named_write_is_read_through_a_subscript() {
        for source in [
            "function f(){ const o = {}; o.secret = getSecret(); log(o[\"secret\"]); }",
            "function f(){ const o = {}; o.secret = getSecret(); const p = o[\"secret\"]; log(p); }",
        ] {
            let flows = run(source, "getSecret", "log", "redact");
            assert_eq!(flows.len(), 1, "{source}");
        }
    }

    #[test]
    fn a_computed_key_write_still_taints_a_sibling_field() {
        // The cost of the wildcard, stated: an unknown key may be `public` too, so this reports
        // — the same over-approximation the field-insensitive analysis made, kept on purpose.
        let flows = run(
            "function f(k){ const o = {}; o[k] = getSecret(); log(o.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "an unknown key may be any key");
    }

    #[test]
    fn an_object_literal_initializer_is_opaque_to_paths() {
        // Documented over-approximation: `taint_of` answers containment before it consults the
        // path, so a source anywhere inside an initializer taints every path asked of the
        // binding — `{ secret: getSecret() }` read at `.public` reports. The assignment form is
        // where field sensitivity lives; a literal is one expression to this analysis.
        let flows = run(
            "function f(){ const o = { secret: getSecret() }; log(o.public); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a contained source taints every path of the literal"
        );
    }

    #[test]
    fn an_alias_composition_is_truncated_at_the_join() {
        // `p = o.a`, read `p.b.c.zz`: the joined path `[a, b, c, zz]` truncates to `[a, b, c]`
        // and meets the write at `o.a.b.c`, whose value carries `inner.q`. Without the truncate
        // at the join the residual `[zz]` is incomparable with `[q]` and this goes silent — the
        // one fixture that separates the join's widening from `base_and_path`'s.
        let flows = run(
            "function f(){ const inner = {}; inner.q = getSecret(); \
             const o = { a: { b: { c: {} } } }; o.a.b.c = inner; const p = o.a; log(p.b.c.zz); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the join widens to the bound");
    }

    #[test]
    fn a_truncated_write_is_seen_by_a_read_above_it() {
        // The write at `o.a.b.c.d` truncates to `[a, b, c]`; reads at `[a]` and `[a, b]` sit
        // above it and cover it, with the residual empty in both.
        for source in [
            "function f(){ const o = {}; o.a.b.c.d = getSecret(); log(o.a); }",
            "function f(){ const o = {}; o.a.b.c.d = getSecret(); log(o.a.b); }",
        ] {
            let flows = run(source, "getSecret", "log", "redact");
            assert_eq!(flows.len(), 1, "{source}");
        }
    }

    // --- #246: taint carried by a binding through a wrapping expression or a literal ------
    //
    // Before #246 `taint_of` had two arms — `identifier` and the member/subscript read — and a
    // `_ => Vec::new()` that silently dropped taint held by a *binding* inside every other
    // expression kind. A source textually *inside* the expression was still caught by the
    // containment scan; taint reaching the expression through a binding was not. These pin the
    // shapes that fall through to `_`, in both directions.

    #[test]
    fn taint_passes_through_a_parenthesized_expression() {
        let flows = run(
            "function f(){ const s = getSecret(); log((s)); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "parentheses do not change the value");
    }

    #[test]
    fn taint_passes_through_a_non_null_assertion() {
        let flows = run(
            "function f(){ const s = getSecret(); log(s!); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`s!` is `s`");
    }

    #[test]
    fn taint_passes_through_an_as_expression() {
        let flows = run(
            "function f(){ const s = getSecret(); log(s as string); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a cast does not change the value");
    }

    #[test]
    fn an_as_expression_bound_first_still_carries_taint() {
        // The ticket's own row: `const b = a as string; log(b)` — the cast sits on a def's rhs,
        // reached through def-use rather than at the sink.
        let flows = run(
            "function f(){ const a = getSecret(); const b = a as string; log(b); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a cast on a binding's initializer is transparent"
        );
    }

    #[test]
    fn taint_passes_through_a_satisfies_expression() {
        let flows = run(
            "function f(){ const s = getSecret(); log(s satisfies string); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`satisfies` does not change the value");
    }

    #[test]
    fn taint_passes_through_an_await_expression() {
        let flows = run(
            "async function f(){ const s = getSecret(); log(await s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "await is transparent: there is no promise model to confuse"
        );
    }

    #[test]
    fn taint_follows_an_awaited_binding() {
        // The ticket's row `const x = await p; log(x)`: await composing through def-use.
        let flows = run(
            "async function f(){ const p = getSecret(); const x = await p; log(x); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the awaited binding carries its taint");
    }

    #[test]
    fn a_tainted_binding_in_an_object_literal_reaches_the_sink() {
        // The headline row: `log({ cause: secret })`, taint held by the binding, not textually
        // inside the literal. The difference from the reporting `log({ cause: getSecret() })`
        // was only whether the author inlined the source.
        let flows = run(
            "function f(){ const secret = getSecret(); log({ cause: secret }); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a tainted field taints the object whole");
    }

    #[test]
    fn a_tainted_object_literal_field_reaches_a_matching_read() {
        let flows = run(
            "function f(){ const s = getSecret(); const o = { cause: s }; log(o.cause); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the read is the field that was tainted");
    }

    #[test]
    fn an_untainted_object_literal_field_is_silent() {
        // Field sensitivity: reading a *different*, known field of the literal carries nothing.
        let flows = run(
            "function f(){ const s = getSecret(); const o = { cause: s }; log(o.other); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "o.cause and o.other are incomparable paths"
        );
    }

    #[test]
    fn a_shorthand_property_carries_taint() {
        // `{ secret }` is `{ secret: secret }` — a `shorthand_property_identifier`, resolved as
        // a reference to the binding.
        let flows = run(
            "function f(){ const secret = getSecret(); log({ secret }); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a shorthand names the binding it carries");
    }

    #[test]
    fn a_spread_of_a_tainted_object_reaches_the_sink() {
        let flows = run(
            "function f(){ const s = getSecret(); const o = { x: s }; log({ ...o }); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a spread exposes the fields it copies");
    }

    #[test]
    fn a_tainted_binding_in_an_array_literal_reaches_the_sink() {
        let flows = run(
            "function f(){ const secret = getSecret(); log([secret]); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "an array element taints the array");
    }

    #[test]
    fn a_ternary_branch_carries_taint() {
        // `log(c ? secret : x)` — the alternative is tainted, the consequence is not; the union
        // reports once.
        let flows = run(
            "function f(cond){ const secret = getSecret(); log(cond ? \"\" : secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a tainted branch taints the ternary");
    }

    #[test]
    fn a_sanitized_ternary_branch_composes_with_the_cut() {
        // Both branches are clean: the consequence is sanitized, the alternative is a literal.
        // The ternary arm sits below the sanitizer cut, so `redact(s)` is still cut inside it.
        let flows = run(
            "function f(cond){ const s = getSecret(); log(cond ? redact(s) : \"\"); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a sanitized branch stays clean inside a ternary"
        );
    }

    #[test]
    fn a_clean_object_literal_is_silent() {
        let flows = run(
            "function f(){ const c = \"x\"; log({ cause: c }); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "no source, no flow");
    }

    // A transparent wrapper is transparent as a *base* too (#246): `(o).secret`,
    // `(o as T).secret`, `o!.token` and `(await o).x` read the same field of the same binding
    // as the unwrapped form. Without peeling the base, `base_and_path` bottoms out at the
    // wrapper and the read carries nothing — the asymmetry a cast-then-access
    // (`(config as Secrets).apiKey`) would defeat trivially.

    #[test]
    fn a_parenthesized_member_base_is_peeled() {
        let flows = run(
            "function f(){ const s = getSecret(); const o = { secret: s }; log((o).secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`(o).secret` is `o.secret`");
    }

    #[test]
    fn a_cast_member_base_is_peeled() {
        let flows = run(
            "function f(){ const s = getSecret(); const o = { token: s }; log((o as any).token); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`(o as T).token` is `o.token`");
    }

    #[test]
    fn a_non_null_member_base_is_peeled() {
        let flows = run(
            "function f(){ const s = getSecret(); const o = { token: s }; log(o!.token); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`o!.token` is `o.token`");
    }

    #[test]
    fn a_wrapped_member_base_keeps_field_precision() {
        // Peeling the wrapper must not cost field sensitivity: a different, known field is silent.
        let flows = run(
            "function f(){ const s = getSecret(); const o = { secret: s }; log((o).other); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "(o).secret and (o).other are incomparable"
        );
    }

    #[test]
    fn a_write_through_a_wrapped_base_taints() {
        // The peel serves the write side too: `(o).secret = …` is a field write to `o`.
        let flows = run(
            "function f(){ const o = {}; (o).secret = getSecret(); log(o.secret); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a write through a wrapped base lands on the binding"
        );
    }

    // --- C2 (#225): a shape-property read off a tainted base is clean ----------------------
    //
    // The one false positive the #220 calibration measured: `${seed.length}` at
    // extensions/keystore-chrome/src/keystore/sign.ts:112, where `seed` is tainted at its root
    // and the sink reads its length. `length`, `byteLength`, `byteOffset` and `size` describe
    // the shape of a value rather than carrying it, so the read is clean — the base is not.

    #[test]
    fn a_write_to_a_shape_named_property_is_a_documented_false_negative() {
        // The sharpest case of the deliberate unsoundness above: the read is cut before the
        // path is consulted, so what the write landed at makes no difference to the answer.
        // The two rows do not even land at the same segment — `o.length` writes
        // `Field("length")` and `o["length"]` writes `Seg::Index`, since every subscript
        // collapses to it — which is exactly why the cut has to be at the read.
        // Pinned so the cost is a stated fact; the control keeps the base itself reported.
        for source in [
            "function f(){ const o = {}; o.length = getSecret(); log(o.length); }",
            "function f(){ const o = {}; o[\"length\"] = getSecret(); log(o.length); }",
        ] {
            let flows = run(source, "getSecret", "log", "redact");
            assert!(flows.is_empty(), "{source}");
        }
        let control = run(
            "function f(){ const o = {}; o.length = getSecret(); log(o); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(control.len(), 1, "the base still carries the value");
    }

    #[test]
    fn a_length_read_off_a_tainted_base_is_clean() {
        let flows = run(
            "function f(){ const s = getSecret(); log(s.length); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "`.length` of a secret is not the secret");
    }

    #[test]
    fn the_corpus_ternary_form_of_the_length_read_is_clean() {
        // The measured site's own shape: `seed` is declared from a ternary whose branches are
        // both sources, so it is tainted at its root by `sources_within` on the initializer —
        // and the sink still reads only its length.
        let flows = run(
            "function f(c){ const s = c ? getSecret().subarray(0, 32) : getSecret(); \
             log(s.length); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a root taint read through `.length` is still clean"
        );
    }

    #[test]
    fn a_byte_length_read_off_a_tainted_base_is_clean() {
        let flows = run(
            "function f(){ const s = getSecret(); log(s.byteLength); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "`byteLength` is a shape property");
    }

    #[test]
    fn a_named_property_read_off_a_tainted_base_still_reports() {
        // CONTROL, and the reason the table is four names rather than "any property": this is
        // the shape the corpus's own source query exists to catch. If it went silent, C2 would
        // have closed the finding by silencing the analysis.
        let flows = run(
            "function f(){ const s = getSecret(); log(s.mnemonic); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a named field of a secret carries it");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_buffer_read_off_a_tainted_base_still_reports() {
        // `buffer` is deliberately absent from the table: it is the bytes, not their shape.
        let flows = run(
            "function f(){ const s = getSecret(); log(s.buffer); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "`.buffer` carries the value");
    }

    #[test]
    fn a_subscripted_shape_property_still_reports() {
        // Documented boundary: the cut is on `member_expression`'s `property` field, so the
        // computed form `s["length"]` is not cleaned. Pinned so the asymmetry is a decision
        // rather than a discovery.
        let flows = run(
            "function f(){ const s = getSecret(); log(s[\"length\"]); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a computed shape property is not cut");
    }

    #[test]
    fn a_source_contained_in_a_shape_property_read_still_reports() {
        // Documented boundary: `taint_of` answers containment before it reaches the member
        // arm, so a source textually inside the read is not cleaned by C2. The may-taint bias
        // wins where the two disagree.
        let flows = run(
            "function f(){ log(getSecret().length); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            flows.len(),
            1,
            "a directly contained source is reported whatever is read off it"
        );
    }

    #[test]
    fn the_shape_property_table_is_sorted() {
        // `is_shape_property_read` searches it with `binary_search`, which answers wrongly and
        // silently on an unsorted slice. This is the only thing holding the table in order.
        let mut sorted = SHAPE_PROPERTIES.to_vec();
        sorted.sort_unstable();
        assert_eq!(SHAPE_PROPERTIES, sorted.as_slice());
    }

    #[test]
    fn an_optional_chain_shape_read_is_clean() {
        // PIN: `s?.length` still parses as a `member_expression` in the TS/TSX grammar — the
        // `?.` lives on the operator, not the node kind — so `is_shape_property_read`'s guard
        // on `member_expression` catches it exactly as the unconditional form.
        let flows = run(
            "function f(){ const s = getSecret(); log(s?.length); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "an optional chain is still a member_expression"
        );
    }

    #[test]
    fn a_shape_read_off_a_nested_tainted_member_is_clean() {
        // PIN: the cut in `taint_of`'s member arm is on the *outer* read's own property, by
        // design — `s.mnemonic.length` asks whether `.length` is a shape property of
        // `s.mnemonic`, and it is, so the read is clean regardless of what `s.mnemonic` itself
        // carries. Nothing walks back down into the nested path to ask if `.length` might be
        // read differently there.
        let flows = run(
            "function f(){ const s = getSecret(); log(s.mnemonic.length); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "the cut is on the outer read's own property"
        );
    }

    #[test]
    fn a_read_below_a_shape_property_still_reports() {
        // The cut is on the outer read's own property and nowhere else: `s.length.raw` is a
        // read at `[length, raw]`, and nothing describes a shape there. Pinned beside its
        // mirror so the asymmetry is a decision rather than a discovery.
        let flows = run(
            "function f(){ const s = getSecret(); log(s.length.raw); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "only the outermost shape read is cut");
    }

    // --- Augmented assignment as a weak update (#194 final review) -------------------------
    //
    // tree-sitter parses `s += x` as `augmented_assignment_expression`, distinct from the
    // `assignment_expression` the strong path matches. `x op= rhs` desugars to `x = x op rhs`:
    // the result is tainted if *either* the prior `x` or `rhs` is tainted, so it is a WEAK
    // update — tainted-iff-RHS, never killing a prior def of `x`. It rides the same additive,
    // non-killing path as a field/index write.

    #[test]
    fn an_augmented_assignment_from_a_source_reports() {
        // FLAGSHIP: `msg += getSecret()` taints `msg` — the string-concatenation pattern
        // `no-secret-in-string` exists to catch. Silent before augmented assignment was
        // modeled, because the strong path never saw the `+=`.
        let flows = run(
            "function f(){ let msg = \"\"; msg += getSecret(); log(msg); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the += taints msg");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn a_prior_taint_survives_an_augmented_assignment() {
        // SOUNDNESS: `s = getSecret(); s += "clean"` desugars to `s = s + "clean"`, whose
        // result is tainted because the *old* s was. A weak update does not kill, so the
        // tainted declarator still reaches the sink. Modeling `+=` as a strong def would
        // clobber it and wrongly silence this — the worst outcome for a may-taint tool.
        let flows = run(
            "function f(){ let s = getSecret(); s += \"clean\"; log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a += must not kill the prior taint on s");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn an_augmented_assignment_of_clean_values_does_not_report() {
        // NEGATIVE: neither the declarator nor the `+=` RHS is tainted, so nothing reports.
        let flows = run(
            "function f(){ let s = \"a\"; s += \"b\"; log(s); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(flows.is_empty(), "a += of clean values must stay silent");
    }

    #[test]
    fn a_non_string_augmented_operator_still_reports() {
        // `n += getSecret()` on a number is still reported: v1 does not reason about numeric
        // coercion, and over-approximating is the sound direction for a taint tool.
        let flows = run(
            "function f(){ let n = 0; n += getSecret(); log(n); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "a non-string += is over-approximated");
        assert_eq!(flows[0].0, "getSecret()");
    }

    #[test]
    fn an_augmented_write_to_a_field_does_not_taint_a_sibling_field() {
        // Was `an_augmented_write_to_a_field_taints_the_object`. A member-target `op=` is a
        // field write at the same path a plain `=` writes at, so C1 scopes it the same way:
        // `o.total` tainted leaves `o.other` clean. The weak-update semantics are untouched —
        // what changed is where the write lands, not whether it kills.
        let flows = run(
            "function f(){ const o = {}; o.total += getSecret(); log(o.other); }",
            "getSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "`o.total` and `o.other` are incomparable paths"
        );
        // CONTROL: the written path itself still reports, so the silence above is the path
        // comparison and not an `op=` that stopped being modeled at all.
        let same = run(
            "function f(){ const o = {}; o.total += getSecret(); log(o.total); }",
            "getSecret",
            "log",
            "redact",
        );
        assert_eq!(
            same.len(),
            1,
            "the augmented write still taints its own path"
        );
    }

    // --- #217: taint seeded at a source captured on a parameter binding --------------------
    //
    // A `@source` that lands on a callback *parameter* (`withSecret((sk) => …)`) must taint the
    // parameter's uses in its own arrow/function body. The v1 analyzer seeded a source only at an
    // *expression* node, so a capture on a parameter binding produced no tainted definition — the
    // corpus's dominant secret pattern, a v1 false negative (#195, docs/taint-calibration.md).
    //
    // These mirror the #195 source queries 1a/1b: the captured node is the parameter *identifier*
    // — inside `(required_parameter (identifier))` for a parenthesized param, or the bare
    // `parameter` field of an unparenthesized arrow.

    /// The binding identifier of an arrow's single parameter: the `parameter` field
    /// (unparenthesized `sk => …`) or the first named child of `formal_parameters`, unwrapped
    /// through a `required_parameter`/`optional_parameter` wrapper (parenthesized `(sk) => …`).
    fn arrow_param_identifier(arrow: Node<'_>) -> Option<Node<'_>> {
        if let Some(param) = arrow.child_by_field_name("parameter") {
            return (param.kind() == "identifier").then_some(param);
        }
        let params = arrow.child_by_field_name("parameters")?;
        let mut cursor = params.walk();
        let first = params.children(&mut cursor).find(Node::is_named)?;
        match first.kind() {
            "identifier" => Some(first),
            "required_parameter" | "optional_parameter" => {
                let mut inner = first.walk();
                first
                    .children(&mut inner)
                    .find(|c| c.kind() == "identifier")
            }
            _ => None,
        }
    }

    /// The parameter identifier of the first arrow-function argument to each call named `wrapper`
    /// — the #195 `withSecret`-family `@source` capture (queries 1a/1b).
    fn callback_param_sources<'t>(tree: &'t Tree, source: &str, wrapper: &str) -> Vec<Node<'t>> {
        calls_named(tree, source, wrapper)
            .into_iter()
            .filter_map(|call| {
                let args = call.child_by_field_name("arguments")?;
                let mut cursor = args.walk();
                let arrow = args
                    .children(&mut cursor)
                    .find(|child| child.kind() == "arrow_function")?;
                arrow_param_identifier(arrow)
            })
            .collect()
    }

    /// Run the analyzer with the callback parameter of `wrapper` as the `@source`, `sink_name`'s
    /// first argument as the `@sink`, and calls to `sanitizer_name` as sanitizers. The
    /// non-empty-sources guard keeps a RED failure meaning "the analyzer did not seed the
    /// parameter", never "the harness captured nothing".
    fn run_param(
        src: &str,
        wrapper: &str,
        sink_name: &str,
        sanitizer_name: &str,
    ) -> Vec<(String, String)> {
        let tree = parse(src);
        let sources = callback_param_sources(&tree, src, wrapper);
        assert!(
            !sources.is_empty(),
            "no @source captured — test harness bug"
        );
        let sinks = sink_args(&tree, src, sink_name);
        let sanitizers = calls_named(&tree, src, sanitizer_name);
        JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| {
                (
                    src[flow.source.byte_range()].to_owned(),
                    src[flow.sink.byte_range()].to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn a_source_captured_on_a_parenthesized_parameter_reports() {
        // Acceptance: `withSecret((sk) => { log(sk); })` — the parameter is the secret.
        let flows = run_param(
            "function _(){ withSecret((sk) => { log(sk); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the captured parameter taints its use");
        assert_eq!(flows[0].0, "sk");
        assert_eq!(flows[0].1, "sk");
    }

    #[test]
    fn a_source_captured_on_a_bare_arrow_parameter_reports() {
        // Unparenthesized, expression body: `withSecret(sk => log(sk))` — the real corpus form.
        let flows = run_param(
            "function _(){ withSecret(sk => log(sk)); }",
            "withSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "sk");
    }

    #[test]
    fn a_captured_parameter_through_one_assignment_reports() {
        // Indirection: `const t = sk; log(t)` inside the callback body.
        let flows = run_param(
            "function _(){ withSecret((sk) => { const t = sk; log(t); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "sk");
    }

    #[test]
    fn a_captured_parameter_with_the_callback_first_reports() {
        // The wrapper takes the callback as its first of several arguments.
        let flows = run_param(
            "function _(){ withSecret((sk) => { log(sk); }, id); }",
            "withSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].0, "sk");
    }

    #[test]
    fn a_captured_parameter_into_a_template_literal_sink_reports() {
        // The #195 template sink query is `(template_substitution (_) @sink)`: the sink node is
        // the interpolated expression `sk`, which `taint_of_identifier` handles — no
        // `template_string` arm needed.
        let src = "function _(){ withSecret((sk) => { log(`x${sk}`); }); }";
        let tree = parse(src);
        let sources = callback_param_sources(&tree, src, "withSecret");
        assert!(
            !sources.is_empty(),
            "no @source captured — test harness bug"
        );
        let sinks: Vec<Node<'_>> = find_all(&tree, "template_substitution")
            .into_iter()
            .filter_map(|sub| sub.named_child(0))
            .collect();
        let sanitizers = calls_named(&tree, src, "redact");
        let flows: Vec<_> = JsFlowAnalyzer
            .analyze(&tree, src, &sources, &sinks, &sanitizers)
            .into_iter()
            .map(|flow| src[flow.source.byte_range()].to_owned())
            .collect();
        assert_eq!(
            flows.len(),
            1,
            "the interpolated parameter taints the template sink"
        );
        assert_eq!(flows[0], "sk");
    }

    #[test]
    fn a_captured_parameter_used_only_after_a_sanitizer_stays_silent() {
        // Kill semantics still apply: `sk = redact(sk)` before the sink clobbers the entry taint,
        // exactly as an in-place sanitizer kills a tainted declarator (Stage 1 within the block).
        let flows = run_param(
            "function _(){ withSecret((sk) => { sk = redact(sk); log(sk); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "an in-place sanitizer must kill the parameter taint"
        );
    }

    #[test]
    fn a_captured_parameter_sanitized_on_one_branch_still_reports() {
        // Path-insensitive soundness: the branch-skipping path reaches the sink with the
        // parameter still tainted, so a one-branch sanitize must not kill.
        let flows = run_param(
            "function _(c){ withSecret((sk) => { if (c) { sk = redact(sk); } log(sk); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert_eq!(flows.len(), 1, "the branch-skipping path keeps the taint");
        assert_eq!(flows[0].0, "sk");
    }

    #[test]
    fn a_captured_parameter_sanitized_on_every_branch_stays_silent() {
        // Cross-block kill parity with a tainted declarator (`sanitizing_both_branches_kills_taint`):
        // every path from function entry to the sink passes a sanitizer, so the parameter's
        // entry-origin taint is killed on every branch and nothing reports. Guards the entry-block
        // attribution: without it the block-less parameter def bypasses the avoid-set kill.
        let flows = run_param(
            "function _(c){ withSecret((sk) => { if (c) { sk = redact(sk); } else { sk = redact(sk); } log(sk); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a sanitizer on every branch must kill the parameter taint"
        );
    }

    #[test]
    fn a_captured_parameter_does_not_taint_an_unrelated_local() {
        // Seeding is scoped to the parameter binding: a clean local read in the same callback
        // must stay silent, or the fix would taint the whole function.
        let flows = run_param(
            "function _(){ withSecret((sk) => { const p = other(); log(p); }); }",
            "withSecret",
            "log",
            "redact",
        );
        assert!(
            flows.is_empty(),
            "a clean local must not be tainted by the parameter source"
        );
    }

    // --- C1 (#225): access paths -----------------------------------------------------------

    #[test]
    fn a_field_segment_orders_before_every_index_segment() {
        // The canonical sort is total only if `Seg` has a stated order, and `Index` after
        // every `Field` is the one this analysis states. Declaration order in the enum is
        // what derives it, so a reordered enum is a silently different sort.
        assert!(Seg::Field(String::from("zzz")) < Seg::Index);
        assert!(Seg::Field(String::from("a")) < Seg::Field(String::from("b")));
        assert_eq!(Seg::Index.cmp(&Seg::Index), std::cmp::Ordering::Equal);
    }

    #[test]
    fn a_path_longer_than_the_bound_is_truncated_to_it() {
        let deep: Path = vec![
            Seg::Field(String::from("a")),
            Seg::Field(String::from("b")),
            Seg::Field(String::from("c")),
            Seg::Field(String::from("d")),
        ];
        assert_eq!(
            truncate(deep),
            vec![
                Seg::Field(String::from("a")),
                Seg::Field(String::from("b")),
                Seg::Field(String::from("c")),
            ],
            "the outermost MAX_PATH_LEN segments survive, the tail is dropped"
        );
    }

    #[test]
    fn a_path_at_or_under_the_bound_is_untouched() {
        let shallow: Path = vec![Seg::Field(String::from("a")), Seg::Index];
        assert_eq!(truncate(shallow.clone()), shallow);
        assert_eq!(truncate(Path::new()), Path::new());
    }

    #[test]
    fn two_paths_differing_only_past_the_bound_truncate_together() {
        // The widening, stated as an equality: `o.a.b.c.d` and `o.a.b.c.e` are one abstract
        // path, which is what makes a deep or cyclic graph terminate at a stated depth.
        let field = |name: &str| Seg::Field(String::from(name));
        let left: Path = vec![field("a"), field("b"), field("c"), field("d")];
        let right: Path = vec![field("a"), field("b"), field("c"), field("e")];
        assert_eq!(truncate(left), truncate(right));
    }

    #[test]
    fn the_empty_path_is_comparable_with_everything() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert!(prefix_comparable(&Path::new(), &Path::new()));
        assert!(prefix_comparable(
            &Path::new(),
            &vec![field("f"), Seg::Index]
        ));
        assert!(prefix_comparable(&vec![field("f")], &Path::new()));
    }

    #[test]
    fn a_prefix_is_comparable_in_both_directions() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert!(prefix_comparable(
            &vec![field("f")],
            &vec![field("f"), field("g")]
        ));
        assert!(prefix_comparable(
            &vec![field("f"), field("g")],
            &vec![field("f")]
        ));
    }

    #[test]
    fn two_paths_that_diverge_are_not_comparable() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert!(!prefix_comparable(&vec![field("f")], &vec![field("g")]));
        assert!(!prefix_comparable(
            &vec![field("a"), field("b")],
            &vec![field("a"), field("c")]
        ));
        assert!(
            prefix_comparable(&vec![field("a")], &vec![Seg::Index]),
            "a subscript is an unknown key, so it compares with every named field"
        );
    }

    #[test]
    fn a_subscript_compares_with_every_field_in_either_position() {
        // The may-analysis reading of an unknown key: `o[k]` may be `o.secret`, so a write at
        // either is observed by a read at the other. Pinned in both positions because the two
        // arms of `prefix_comparable` are distinct code.
        let field = |name: &str| Seg::Field(String::from(name));
        assert!(prefix_comparable(&vec![Seg::Index], &vec![field("secret")]));
        assert!(prefix_comparable(
            &vec![field("a"), Seg::Index],
            &vec![field("a"), field("secret")]
        ));
        assert!(prefix_comparable(
            &vec![field("a"), field("secret")],
            &vec![field("a"), Seg::Index, field("x")]
        ));
    }

    #[test]
    fn paths_differing_only_past_the_bound_compare_once_truncated() {
        // The widening as the comparison sees it: truncation happens where a path is built, so
        // by the time two reach `prefix_comparable` the difference past the bound is gone.
        let field = |name: &str| Seg::Field(String::from(name));
        let write = truncate(vec![field("a"), field("b"), field("c"), field("d")]);
        let read = truncate(vec![field("a"), field("b"), field("c"), field("e")]);
        assert!(prefix_comparable(&write, &read));
        let elsewhere = truncate(vec![field("a"), field("b"), field("x")]);
        assert!(
            !prefix_comparable(&write, &elsewhere),
            "a divergence inside the bound survives it"
        );
    }

    /// Parse `src`, take its first `member_expression`/`subscript_expression`/`identifier`
    /// under the sink call, and render `base_and_path` as `(base text, path)`.
    fn read_path(src: &str) -> Option<(String, Path)> {
        let tree = parse(src);
        let read = sink_args(&tree, src, "log").into_iter().next()?;
        let (base, path) = base_and_path(read, src)?;
        Some((src[base.byte_range()].to_owned(), path))
    }

    #[test]
    fn a_bare_identifier_has_an_empty_access_path() {
        assert_eq!(
            read_path("function f(){ log(o); }"),
            Some((String::from("o"), Path::new()))
        );
    }

    #[test]
    fn a_dotted_read_accumulates_one_field_per_segment() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert_eq!(
            read_path("function f(){ log(o.a); }"),
            Some((String::from("o"), vec![field("a")]))
        );
        assert_eq!(
            read_path("function f(){ log(o.a.b); }"),
            Some((String::from("o"), vec![field("a"), field("b")])),
            "outermost segment first, so `o.a.b` is [a, b] and not [b, a]"
        );
    }

    #[test]
    fn every_subscript_collapses_to_one_index_segment() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert_eq!(
            read_path("function f(){ log(o[i].c); }"),
            Some((String::from("o"), vec![Seg::Index, field("c")]))
        );
        assert_eq!(
            read_path("function f(){ log(o.a[0]); }"),
            Some((String::from("o"), vec![field("a"), Seg::Index]))
        );
    }

    #[test]
    fn a_read_deeper_than_the_bound_is_truncated_where_it_is_built() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert_eq!(
            read_path("function f(){ log(o.a.b.c.d); }"),
            Some((String::from("o"), vec![field("a"), field("b"), field("c")])),
            "the widening applies at construction, so nothing downstream sees a longer path"
        );
    }

    #[test]
    fn a_read_whose_base_is_not_an_identifier_has_no_path() {
        // A call result or `this` has no binding for taint to attach to, exactly as
        // `base_identifier` refused it.
        assert_eq!(read_path("function f(){ log(g().a); }"), None);
        assert_eq!(read_path("function f(){ log(this.a); }"), None);
    }

    #[test]
    fn a_private_name_folds_to_an_index_segment() {
        // `#k` is not a `property_identifier`, so `property_segment` folds it to `Seg::Index`
        // exactly like a subscript — an over-approximation rather than inventing a name that
        // could collide with an ordinary field.
        assert_eq!(
            read_path("class C { #k = 1; f(o: C){ log(o.#k); } }"),
            Some((String::from("o"), vec![Seg::Index])),
            "a private name reads as an opaque index segment"
        );
    }

    /// The write target of `src`'s first assignment, as `(base text, path)`.
    fn write_path(src: &str) -> Option<(String, Path)> {
        let tree = parse(src);
        let assignment = find_all(&tree, "assignment_expression")
            .into_iter()
            .chain(find_all(&tree, "augmented_assignment_expression"))
            .min_by_key(Node::start_byte)?;
        let (base, path) = write_target(assignment, src)?;
        Some((src[base.byte_range()].to_owned(), path))
    }

    #[test]
    fn a_field_write_carries_the_path_it_writes_at() {
        let field = |name: &str| Seg::Field(String::from(name));
        assert_eq!(
            write_path("function f(){ o.secret = x; }"),
            Some((String::from("o"), vec![field("secret")]))
        );
        assert_eq!(
            write_path("function f(){ o.a.b = x; }"),
            Some((String::from("o"), vec![field("a"), field("b")]))
        );
    }

    #[test]
    fn an_index_write_carries_one_index_segment() {
        assert_eq!(
            write_path("function f(){ a[0] = x; }"),
            Some((String::from("a"), vec![Seg::Index]))
        );
    }

    #[test]
    fn an_augmented_field_write_carries_the_same_path_as_a_plain_one() {
        // `o.total += x` mutates `o.total` exactly as `o.total = x` does, so it writes at the
        // same path. The two differ in whether prior taint survives, not in where they land.
        let field = |name: &str| Seg::Field(String::from(name));
        assert_eq!(
            write_path("function f(){ o.total += x; }"),
            Some((String::from("o"), vec![field("total")]))
        );
    }

    #[test]
    fn an_identifier_target_is_not_a_field_write() {
        // A strong update, handled by `assignments_to`/`augmented_assignments_to`. Refusing it
        // here is what keeps the two sets disjoint.
        assert_eq!(write_path("function f(){ s = x; }"), None);
        assert_eq!(write_path("function f(){ s += x; }"), None);
    }
}
