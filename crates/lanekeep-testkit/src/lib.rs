//! Fixture-based rule testing harness for lanekeep.
//!
//! `RuleTester`: the harness every rule is tested through.
//!
//! This is not optional infrastructure. Without it, community rule contributions are
//! unreviewable — a reviewer cannot tell a rule that works from one that happens not to
//! have been tried on the case it gets wrong.
//!
//! # It runs the real path
//!
//! A tester builds a throwaway project on disk and runs the actual engine over it: real
//! config loading, real gates, real sandbox, real query matching. Nothing is stubbed.
//!
//! That costs more than calling a handler directly, and buys the thing that matters. A rule
//! can be correct in isolation and still never fire because its gate excludes the file, its
//! query does not compile against the language it named, or its card fails validation. A
//! harness that skipped those would pass rules that do nothing.
//!
//! # Usage
//!
//! ```no_run
//! use lanekeep_testkit::RuleTester;
//!
//! let tester = RuleTester::new("no-debugger", RULE_SOURCE).expect("builds");
//! tester.accepts("const a = 1;").expect("clean code passes");
//! tester.reports_at("debugger;", &[(1, 1)]).expect("violations are found");
//! # const RULE_SOURCE: &str = "";
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lanekeep_core::Violation;
use lanekeep_engine::Engine;
use lanekeep_js::RuleRoot;
use lanekeep_lang_js::{JavaScript, TypeScript};
use thiserror::Error;

/// Why a rule test could not run, or did not hold.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TestError {
    /// The harness could not set up its temporary project.
    #[error("could not set up the rule test: {0}")]
    Setup(String),

    /// The rule or its config failed to load.
    ///
    /// Distinct from a failed assertion: the rule never ran, so nothing was proven either
    /// way, and reporting it as "no violations found" would be actively misleading.
    #[error("rule failed to load:\n{0}")]
    Load(String),

    /// The run aborted — a rule threw, or breached a budget.
    #[error("rule failed while running:\n{0}")]
    Run(String),

    /// The rule reported something other than what was expected.
    #[error("{0}")]
    Mismatch(String),
}

/// Distinguishes testers built in the same process.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A rule under test, with a throwaway project to run it in.
///
/// The project is removed when the tester is dropped.
#[derive(Debug)]
pub struct RuleTester {
    dir: PathBuf,
    extension: String,
    /// The config file the project was written with, relative to [`RuleTester::dir`].
    ///
    /// A field rather than a constant because a component tester writes a `lanekeep.json` and a
    /// source tester writes a `lanekeep.config.ts` — see [`RuleTester::for_component`] for why
    /// that is forced rather than chosen.
    config: &'static str,
}

/// What a source rule's project is configured by.
const TS_CONFIG: &str = "lanekeep.config.ts";

/// What a component rule's project is configured by.
const JSON_CONFIG: &str = "lanekeep.json";

/// Where a component tester puts the rule, relative to the project root.
///
/// Inside the rules root, which is the project root here, because `lanekeep-config` confines a
/// component reference to it — a `.wasm` outside is refused before it is read.
const COMPONENT_PATH: &str = "rules/rule.wasm";

impl RuleTester {
    /// Build a tester for a rule's source.
    ///
    /// `name` labels the temporary directory and need not be unique — every tester gets its
    /// own directory regardless, so a test file with a `fn tester()` helper shared across
    /// cases works. It has to: two testers sharing a directory would delete each other's
    /// project mid-run, and the resulting error would point at the config rather than at
    /// the collision.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Setup`] if the temporary project cannot be written.
    pub fn new(name: &str, rule_source: &str) -> Result<Self, TestError> {
        Self::with_extension(name, rule_source, "ts")
    }

    /// Build a tester whose subject files use a given extension.
    ///
    /// Needed for a rule targeting `tsx`, since which grammar parses a file is decided by
    /// its extension — a TSX rule tested against a `.ts` file would never match.
    ///
    /// # Errors
    ///
    /// As [`RuleTester::new`].
    pub fn with_extension(
        name: &str,
        rule_source: &str,
        extension: &str,
    ) -> Result<Self, TestError> {
        Self::build(name, rule_source, extension, "rule")
    }

    /// Build a tester for a *factory* rule — one whose default export returns a rule when
    /// called with options — using the given options expression.
    ///
    /// `options` is JavaScript, spliced into the generated config as `rule(<options>)`.
    /// Passing the options as source rather than as a serialized value is deliberate: a
    /// factory takes whatever its author designed, and a harness that only accepted JSON
    /// could not test one taking a function or a regular expression.
    ///
    /// ```no_run
    /// # use lanekeep_testkit::RuleTester;
    /// let tester = RuleTester::configured(
    ///     "restricted",
    ///     RULE_SOURCE,
    ///     "{ restrictions: [{ module: 'lodash' }] }",
    /// )
    /// .expect("builds");
    /// # const RULE_SOURCE: &str = "";
    /// ```
    ///
    /// # Errors
    ///
    /// As [`RuleTester::new`].
    pub fn configured(name: &str, rule_source: &str, options: &str) -> Result<Self, TestError> {
        Self::configured_with_extension(name, rule_source, "ts", options)
    }

    /// Build a tester for a factory rule whose subject files use a given extension.
    ///
    /// [`RuleTester::configured`] and [`RuleTester::with_extension`] each vary one axis, and
    /// a rule that is both parameterized and non-TypeScript needs both — every built-in
    /// targeting Rust, Go or Python is in that position the moment it takes an option. The
    /// absence was not a decision: `configured` predates any parameterized rule outside
    /// TypeScript, and the two Rust-targeting built-ins that document an `allow` option had
    /// no test reaching them here at all.
    ///
    /// # Errors
    ///
    /// As [`RuleTester::new`].
    pub fn configured_with_extension(
        name: &str,
        rule_source: &str,
        extension: &str,
        options: &str,
    ) -> Result<Self, TestError> {
        Self::build(name, rule_source, extension, &format!("rule({options})"))
    }

    /// Build a tester for a rule compiled to a WebAssembly component.
    ///
    /// `bytes` are the component itself — `lanekeep_rules::component("no-unwrap")`, or whatever
    /// a project's own build produced. They are written to `rules/rule.wasm` inside the
    /// throwaway project, because a component reference is confined to the rules root.
    ///
    /// # It generates a `lanekeep.json`, and it has to
    ///
    /// Every other constructor writes a `lanekeep.config.ts` that imports the rule. A `.wasm` is
    /// not a value a TypeScript module can import: a component is resolved in Rust, by path, and
    /// answers `metadata` for itself. So the component path is the JSON config path, and that is
    /// forced by what a component is rather than chosen for convenience.
    ///
    /// # `extension` is required rather than defaulted
    ///
    /// [`RuleTester::new`] defaults to `ts` and [`RuleTester::with_extension`] varies it, which
    /// is right when TypeScript is the overwhelmingly common case. It is not the common case
    /// here — a rule authored as a component is one written in the language it inspects, and the
    /// two that exist target Rust — so a default would be wrong more often than not, and the
    /// pair of `*_with_extension` variants it would need doubles the constructor count for
    /// nothing.
    ///
    /// # Errors
    ///
    /// As [`RuleTester::new`].
    pub fn for_component(name: &str, bytes: &[u8], extension: &str) -> Result<Self, TestError> {
        Self::build_component(name, bytes, extension, None)
    }

    /// Build a tester for a component rule configured with options.
    ///
    /// `options` is **JSON**, and the difference from [`RuleTester::configured`] is the whole
    /// point rather than an inconvenience. That one splices JavaScript source into a config, so
    /// a factory rule can be handed a function or a regular expression. A component cannot close
    /// over a host-supplied value at all: its options cross the boundary as data, through the
    /// world's `configure(options-json)`. Accepting source here would suggest otherwise, and
    /// would test a shape no real config can produce.
    ///
    /// # Errors
    ///
    /// As [`RuleTester::new`], plus [`TestError::Setup`] if `options` is not valid JSON — which
    /// is caught here rather than left to surface as a config parse error naming a generated
    /// file the caller never wrote.
    pub fn for_component_configured(
        name: &str,
        bytes: &[u8],
        extension: &str,
        options: &str,
    ) -> Result<Self, TestError> {
        let options: serde_json::Value = serde_json::from_str(options)
            .map_err(|e| TestError::Setup(format!("`options` is not valid JSON: {e}")))?;
        Self::build_component(name, bytes, extension, Some(options))
    }

    /// Write the throwaway project.
    ///
    /// `rule_expr` is what goes in the config's `rules` array — the imported module for a
    /// plain rule, a call for a factory.
    fn build(
        name: &str,
        rule_source: &str,
        extension: &str,
        rule_expr: &str,
    ) -> Result<Self, TestError> {
        let tester = Self::empty(name, extension, TS_CONFIG);
        tester.write("rule.ts", rule_source)?;
        tester.write(
            TS_CONFIG,
            &format!(
                "import {{ defineConfig }} from 'lanekeep';\n\
                 import rule from './rule';\n\
                 export default defineConfig({{ include: ['subject/**'], rules: [{rule_expr}] }});\n"
            ),
        )?;
        Ok(tester)
    }

    /// Write the throwaway project for a component rule.
    ///
    /// `options` is `None` for the bare form and `Some` for the object form, and the two are not
    /// the same config: `lanekeep-config` reads a bare string as a rule used as it comes and
    /// `{ "rule", "options" }` as one configured, which is the distinction a rule author already
    /// makes between a rule and a rule factory. `Some(Value::Null)` is therefore a third thing
    /// again — a rule explicitly configured with nothing — and is reachable from here on
    /// purpose.
    fn build_component(
        name: &str,
        bytes: &[u8],
        extension: &str,
        options: Option<serde_json::Value>,
    ) -> Result<Self, TestError> {
        let tester = Self::empty(name, extension, JSON_CONFIG);
        tester.write_bytes(COMPONENT_PATH, bytes)?;

        let reference = format!("./{COMPONENT_PATH}");
        let rule = match options {
            None => serde_json::Value::String(reference),
            Some(options) => serde_json::json!({ "rule": reference, "options": options }),
        };
        let config = serde_json::json!({ "include": ["subject/**"], "rules": [rule] });

        // `to_string` rather than `to_string_pretty`: nothing reads this by eye except when a
        // test fails, and a failure prints the error rather than the config.
        tester.write(JSON_CONFIG, &config.to_string())?;
        Ok(tester)
    }

    /// A tester with its own directory and nothing in it yet.
    fn empty(name: &str, extension: &str, config: &'static str) -> Self {
        // Unique per tester: the counter separates testers in one process, the process id
        // separates the processes nextest spawns per test.
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-ruletest-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        Self {
            dir,
            extension: extension.to_owned(),
            config,
        }
    }

    fn write(&self, path: &str, contents: &str) -> Result<(), TestError> {
        self.write_bytes(path, contents.as_bytes())
    }

    fn write_bytes(&self, path: &str, contents: &[u8]) -> Result<(), TestError> {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TestError::Setup(e.to_string()))?;
        }
        std::fs::write(full, contents).map_err(|e| TestError::Setup(e.to_string()))
    }

    /// Run the rule over a single source file and return what it reported.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Load`] if the rule does not load, or [`TestError::Run`] if it
    /// throws or breaches a budget.
    pub fn run(&self, source: &str) -> Result<Vec<Violation>, TestError> {
        // A fresh subject each time, so one case cannot see another's file.
        let _ = std::fs::remove_dir_all(self.dir.join("subject"));
        self.write(&format!("subject/input.{}", self.extension), source)?;

        let root = RuleRoot::new(&self.dir).map_err(|e| TestError::Setup(e.to_string()))?;
        let config_path = self.dir.join(self.config);

        let sandbox =
            lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                .map_err(|e| TestError::Load(e.to_string()))?;
        let config = lanekeep_config::load(&sandbox, &root, &config_path)
            .map_err(|e| TestError::Load(e.to_string()))?;

        let engine = Engine::prepare(
            &config,
            &self.dir,
            root,
            &config_path,
            &lanekeep_languages::registry(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .map_err(|e| TestError::Load(e.to_string()))?;

        engine
            .run()
            .map(|outcome| outcome.violations)
            .map_err(|e| TestError::Run(e.to_string()))
    }

    /// Assert the rule reports nothing for this source.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Mismatch`] listing what was reported, since "expected none,
    /// got some" is only actionable if you can see which.
    pub fn accepts(&self, source: &str) -> Result<(), TestError> {
        let violations = self.run(source)?;
        if violations.is_empty() {
            return Ok(());
        }

        let mut message = format!(
            "expected no violations, but the rule reported {}:\n",
            violations.len()
        );
        for violation in &violations {
            let _ = writeln!(
                message,
                "  {}:{} {}",
                violation.location.position.line,
                violation.location.position.column,
                violation.message
            );
        }
        let _ = write!(message, "\nsource:\n{}", indent(source));
        Err(TestError::Mismatch(message))
    }

    /// Assert the rule reports at exactly these one-based positions, in order.
    ///
    /// Positions rather than a count, because a rule reporting the right number of
    /// violations in the wrong places is a rule that is wrong — and a count-only assertion
    /// is exactly what lets that through.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Mismatch`] showing expected and actual side by side.
    pub fn reports_at(&self, source: &str, expected: &[(u32, u32)]) -> Result<(), TestError> {
        let violations = self.run(source)?;
        let actual: Vec<(u32, u32)> = violations
            .iter()
            .map(|v| (v.location.position.line, v.location.position.column))
            .collect();

        if actual == expected {
            return Ok(());
        }

        Err(TestError::Mismatch(format!(
            "reported positions did not match\n  expected: {expected:?}\n  actual:   {actual:?}\n\nsource:\n{}",
            indent(source)
        )))
    }

    /// Assert the rule reports exactly these messages, in order.
    ///
    /// For a rule that substitutes its own message per match — the position alone would not
    /// show whether the right one was chosen.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::Mismatch`] showing both lists.
    pub fn reports_messages(&self, source: &str, expected: &[&str]) -> Result<(), TestError> {
        let violations = self.run(source)?;
        let actual: Vec<&str> = violations.iter().map(|v| v.message.as_str()).collect();

        if actual == expected {
            return Ok(());
        }

        Err(TestError::Mismatch(format!(
            "reported messages did not match\n  expected: {expected:?}\n  actual:   {actual:?}\n\nsource:\n{}",
            indent(source)
        )))
    }
}

impl Drop for RuleTester {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Indent source for inclusion in a failure message, so it is visibly quoted rather than
/// running together with the assertion text.
fn indent(source: &str) -> String {
    source.lines().fold(String::new(), |mut out, line| {
        // Writing into a String cannot fail; swallowing the Result keeps this a fold
        // rather than a loop with an unreachable error arm.
        let _ = writeln!(out, "  | {line}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBUGGER: &str = "import { defineRule } from 'lanekeep';\n\
        export default defineRule({\n\
          id: 'local/no-debugger',\n\
          query: '(debugger_statement) @stmt',\n\
          card: {\n\
            message: 'debugger statement',\n\
            remediation: 'remove it',\n\
            examples: { bad: 'debugger;', good: 'log();' },\n\
          },\n\
          check(ctx, m) { ctx.report(m.stmt); },\n\
        });\n";

    fn tester(name: &str) -> RuleTester {
        RuleTester::new(name, DEBUGGER).expect("builds")
    }

    #[test]
    fn accepts_clean_source() {
        tester("accepts")
            .accepts("const a = 1;\n")
            .expect("should accept");
    }

    #[test]
    fn reports_at_the_expected_positions() {
        tester("positions")
            .reports_at("const a = 1;\ndebugger;\n", &[(2, 1)])
            .expect("should report");
    }

    #[test]
    fn reports_several_in_order() {
        tester("several")
            .reports_at("debugger;\nconst a = 1;\ndebugger;\n", &[(1, 1), (3, 1)])
            .expect("should report both");
    }

    #[test]
    fn accepts_fails_loudly_and_shows_what_was_found() {
        // A harness that said only "expected none, got some" would leave the author
        // guessing which case tripped.
        let err = tester("accepts-fail")
            .accepts("debugger;\n")
            .expect_err("should not accept");

        let rendered = err.to_string();
        assert!(rendered.contains("expected no violations"), "{rendered}");
        assert!(
            rendered.contains("debugger statement"),
            "should show the message: {rendered}"
        );
        assert!(rendered.contains("1:1"), "should show where: {rendered}");
    }

    #[test]
    fn a_position_mismatch_shows_both_sides() {
        let err = tester("position-fail")
            .reports_at("debugger;\n", &[(5, 5)])
            .expect_err("should not match");

        let rendered = err.to_string();
        assert!(rendered.contains("expected: [(5, 5)]"), "{rendered}");
        assert!(rendered.contains("actual:   [(1, 1)]"), "{rendered}");
    }

    #[test]
    fn checks_messages_when_a_rule_substitutes_its_own() {
        let rule = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/named',\n\
              query: '(variable_declarator name: (identifier) @name)',\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check(ctx, m) { ctx.report(m.name, `saw ${ctx.text(m.name)}`); },\n\
            });\n";

        RuleTester::new("messages", rule)
            .expect("builds")
            .reports_messages(
                "const alpha = 1;\nconst beta = 2;\n",
                &["saw alpha", "saw beta"],
            )
            .expect("should match");
    }

    #[test]
    fn a_rule_that_does_not_load_is_distinguished_from_one_that_found_nothing() {
        // The distinction that matters most. Reporting a load failure as "no violations"
        // would make a broken rule look like a passing one — which is the same failure
        // mode the config's has_check test exists to prevent, one layer up.
        let broken = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/broken',\n\
              query: '(no_such_node) @x',\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check() {},\n\
            });\n";

        let err = RuleTester::new("broken", broken)
            .expect("builds")
            .accepts("const a = 1;\n")
            .expect_err("must not pass");

        assert!(matches!(err, TestError::Load(_)), "{err:?}");
        assert!(err.to_string().contains("no_such_node"), "{err}");
    }

    #[test]
    fn a_throwing_rule_is_reported_as_a_run_failure() {
        let throwing = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/throws',\n\
              query: '(debugger_statement) @s',\n\
              card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
              check() { throw new Error('boom'); },\n\
            });\n";

        let err = RuleTester::new("throwing", throwing)
            .expect("builds")
            .accepts("debugger;\n")
            .expect_err("must not pass");

        assert!(matches!(err, TestError::Run(_)), "{err:?}");
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn cases_do_not_leak_into_each_other() {
        // Each case rewrites the subject directory. Without that, a violation from an
        // earlier case would still be on disk and show up in the next one.
        let tester = tester("isolation");
        tester
            .reports_at("debugger;\n", &[(1, 1)])
            .expect("first case");
        tester
            .accepts("const a = 1;\n")
            .expect("second case must not see the first");
    }

    #[test]
    fn a_tsx_rule_can_be_tested_against_tsx() {
        // Which grammar parses a file is decided by its extension, so a TSX rule tested
        // against a `.ts` subject would silently never match.
        let rule = "import { defineRule } from 'lanekeep';\n\
            export default defineRule({\n\
              id: 'local/no-jsx',\n\
              language: 'tsx',\n\
              query: '(jsx_element) @el',\n\
              card: { message: 'jsx', remediation: 'do not', examples: { bad: '<a/>', good: 'a()' } },\n\
              check(ctx, m) { ctx.report(m.el); },\n\
            });\n";

        RuleTester::with_extension("tsx", rule, "tsx")
            .expect("builds")
            .reports_at("const a = <div>hi</div>;\n", &[(1, 11)])
            .expect("should report the element");
    }

    /// Not a component. These cases assert what the harness *writes*, which is the whole of what
    /// the component constructors do; running one is `crates/lanekeep-rules/tests/no_unwrap.rs`'s
    /// job, and it has real components to do it with. Four bytes of magic keep the fixture from
    /// looking like something that could be loaded.
    const NOT_A_COMPONENT: &[u8] = b"\0asm not really";

    /// The generated config, parsed back.
    fn written_config(tester: &RuleTester) -> serde_json::Value {
        let text = std::fs::read_to_string(tester.dir.join(JSON_CONFIG)).expect("config written");
        serde_json::from_str(&text).expect("the generated config is JSON")
    }

    #[test]
    fn a_component_tester_writes_the_bytes_and_points_a_json_config_at_them() {
        let tester = RuleTester::for_component("component", NOT_A_COMPONENT, "rs").expect("builds");

        assert_eq!(
            std::fs::read(tester.dir.join(COMPONENT_PATH)).expect("component written"),
            NOT_A_COMPONENT,
            "the bytes must reach disk unchanged — a component is identified by them"
        );
        assert_eq!(
            written_config(&tester)["rules"][0],
            serde_json::json!("./rules/rule.wasm"),
            "the bare form is a string, which is what `lanekeep-config` reads as a rule used \
             as it comes"
        );
        assert!(
            !tester.dir.join(TS_CONFIG).exists(),
            "a component project must not also carry a TypeScript config"
        );
    }

    #[test]
    fn a_configured_component_tester_embeds_its_options_as_data() {
        // The distinction `configured` cannot make: those options are spliced into JavaScript
        // source, and these have to survive as a value, because `configure(options-json)` is
        // where they arrive. A harness that stringified them would hand the rule `"{...}"`.
        let tester = RuleTester::for_component_configured(
            "configured",
            NOT_A_COMPONENT,
            "rs",
            r#"{"allow": ["subject/input.rs"]}"#,
        )
        .expect("builds");

        assert_eq!(
            written_config(&tester)["rules"][0],
            serde_json::json!({
                "rule": "./rules/rule.wasm",
                "options": { "allow": ["subject/input.rs"] },
            })
        );
    }

    #[test]
    fn explicit_null_options_are_a_different_config_from_the_bare_form() {
        // Three states, not two. `lanekeep-config` reads a bare string as a rule used as it
        // comes and the object form as one configured, so a rule named with `null` options is
        // not the same config as a rule named with none — and a harness collapsing them would
        // make the bare case untestable.
        let configured =
            RuleTester::for_component_configured("null-options", NOT_A_COMPONENT, "rs", "null")
                .expect("builds");
        let bare = RuleTester::for_component("bare", NOT_A_COMPONENT, "rs").expect("builds");

        assert_eq!(
            written_config(&configured)["rules"][0],
            serde_json::json!({ "rule": "./rules/rule.wasm", "options": null })
        );
        assert_ne!(
            written_config(&configured)["rules"][0],
            written_config(&bare)["rules"][0]
        );
    }

    #[test]
    fn options_that_are_not_json_are_refused_naming_the_options() {
        // The failure mode this branch exists to prevent: spliced into the config unchecked, a
        // malformed value becomes a parse error against a generated file the caller never wrote,
        // pointing at a line number that means nothing to them.
        let err = RuleTester::for_component_configured(
            "bad-options",
            NOT_A_COMPONENT,
            "rs",
            "{ allow: ['subject/input.rs'] }",
        )
        .expect_err("JavaScript object syntax is not JSON");

        assert!(matches!(err, TestError::Setup(_)), "{err:?}");
        assert!(
            err.to_string().contains("`options` is not valid JSON"),
            "{err}"
        );
    }

    #[test]
    fn the_temporary_project_is_cleaned_up() {
        let path = {
            let tester = tester("cleanup");
            tester.accepts("const a = 1;\n").expect("runs");
            tester.dir.clone()
        };
        assert!(
            !path.exists(),
            "the tester should remove its project on drop"
        );
    }
}
