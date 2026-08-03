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
use std::path::{Path, PathBuf};
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
    /// `Some` when the caller chose the subject's extension explicitly (`new`,
    /// `with_extension`). `None` for `configured`, which has no parameter for one — `run`
    /// then derives it from the rule's own declared `language` instead.
    extension: Option<String>,
}

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
        Self::build(name, rule_source, Some(extension), "rule")
    }

    /// Build a tester for a *factory* rule — one whose default export returns a rule when
    /// called with options — using the given options expression.
    ///
    /// `options` is JavaScript, spliced into the generated config as `rule(<options>)`.
    /// Passing the options as source rather than as a serialized value is deliberate: a
    /// factory takes whatever its author designed, and a harness that only accepted JSON
    /// could not test one taking a function or a regular expression.
    ///
    /// No extension parameter, unlike [`RuleTester::with_extension`] — `run` derives the
    /// subject's extension from the rule's own declared `language` instead, once the config
    /// is loaded. A factory rule is exactly the case most likely to target a non-default
    /// language, and threading a fourth parameter through would change the shape of every
    /// existing call for the sake of the ones that need it.
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
        Self::build(name, rule_source, None, &format!("rule({options})"))
    }

    /// Write the throwaway project.
    ///
    /// `rule_expr` is what goes in the config's `rules` array — the imported module for a
    /// plain rule, a call for a factory. `extension` is `None` only from `configured`; see
    /// its documentation for why `run` — not `build` — is where that gets resolved.
    fn build(
        name: &str,
        rule_source: &str,
        extension: Option<&str>,
        rule_expr: &str,
    ) -> Result<Self, TestError> {
        // Unique per tester: the counter separates testers in one process, the process id
        // separates the processes nextest spawns per test.
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-ruletest-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let tester = Self {
            dir,
            extension: extension.map(str::to_owned),
        };
        // Nested one level rather than sitting at the fixture's own top, so a rule that
        // imports a sibling module the way this repository's local rules do — `../modules/x`
        // from `lanekeep/rules/some-rule.ts` — resolves inside the fixture instead of
        // escaping it. `mirror_modules` is what makes `../modules/x` resolve to something
        // real rather than merely legal.
        tester.write("rules/rule.ts", rule_source)?;
        tester.mirror_modules()?;
        tester.write(
            "lanekeep.config.ts",
            &format!(
                "import {{ defineConfig }} from 'lanekeep';\n\
                 import rule from './rules/rule';\n\
                 export default defineConfig({{ include: ['subject/**'], rules: [{rule_expr}] }});\n"
            ),
        )?;
        Ok(tester)
    }

    /// Copy this repository's own `lanekeep/modules/` into the fixture, as `modules/` — a
    /// sibling of `rules/rule.ts`, one level up from it, exactly as `lanekeep/modules/` sits
    /// relative to `lanekeep/rules/` in the real repository.
    ///
    /// A rule tested here is given as a source string, not a path, so there is no file on
    /// disk this crate could otherwise learn the rule's real location from — and no way to
    /// thread one through without changing the shape every existing caller of `new`,
    /// `with_extension` and `configured` already depends on. Locating the source directory
    /// from this crate's own manifest directory keeps that shape unchanged.
    ///
    /// A no-op when the source directory does not exist, which is true for everything other
    /// than this workspace's own tests: a rule with no relative import never reads the copy,
    /// and a project outside this repository that depends on this crate to test its own
    /// rules has no such directory to find.
    fn mirror_modules(&self) -> Result<(), TestError> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lanekeep/modules");
        let Ok(read_dir) = std::fs::read_dir(&source) else {
            return Ok(());
        };

        // Read-dir order is not guaranteed; fixed order keeps a failure in here reproducible.
        let mut paths = read_dir
            .map(|entry| {
                entry
                    .map(|e| e.path())
                    .map_err(|e| TestError::Setup(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();

        for path in paths {
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let contents =
                std::fs::read_to_string(&path).map_err(|e| TestError::Setup(e.to_string()))?;
            self.write(&format!("modules/{name}"), &contents)?;
        }
        Ok(())
    }

    fn write(&self, path: &str, contents: &str) -> Result<(), TestError> {
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

        let root = RuleRoot::new(&self.dir).map_err(|e| TestError::Setup(e.to_string()))?;
        let config_path = self.dir.join("lanekeep.config.ts");

        let sandbox =
            lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                .map_err(|e| TestError::Load(e.to_string()))?;
        let config = lanekeep_config::load(&sandbox, &root, &config_path)
            .map_err(|e| TestError::Load(e.to_string()))?;

        // Loaded before the subject is written: `include` is a glob string the config holds,
        // not something checked against the filesystem yet, so `load` has no dependency on
        // `subject/` already existing — which is what leaves room to pick the subject's own
        // extension from what the loaded rule declares.
        let extension = match &self.extension {
            Some(extension) => extension.clone(),
            None => extension_for(&config),
        };
        self.write(&format!("subject/input.{extension}"), source)?;

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

/// The extension matching the first language the config's one rule declared, for
/// [`RuleTester::configured`], which has no parameter of its own to say.
///
/// Reads `languages` off the already-loaded [`lanekeep_config::Config`] rather than the raw
/// rule source, so this agrees with whatever the engine itself will run the rule against —
/// including the default the `Rule` interface documents when a rule declares no `language`
/// at all. Falls back to `"ts"` if the registry does not recognize the id, which `load`
/// tolerates (only `Engine::prepare` validates a rule's language against the registry) and
/// which is the same extension `configured` used unconditionally before this existed.
fn extension_for(config: &lanekeep_config::Config) -> String {
    config
        .rules
        .first()
        .and_then(|rule| rule.languages.first())
        .and_then(|id| lanekeep_languages::registry().by_id(id).cloned())
        .and_then(|language| language.extensions().first().copied())
        .map_or_else(|| "ts".to_owned(), str::to_owned)
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

    #[test]
    fn a_configured_rule_is_tested_against_the_language_it_declares() {
        // `configured` has no extension parameter of its own — unlike `with_extension` above,
        // there is no argument through which a caller could ask for anything but the
        // default. The subject's extension has to come from somewhere, and the only thing
        // left to read is the rule's own declared `language`.
        let rule = "import { defineRule } from 'lanekeep';\n\
            export default function factory(options) {\n\
              return defineRule({\n\
                id: 'local/rust-only',\n\
                language: 'rust',\n\
                query: '(function_item name: (identifier) @name) @fn',\n\
                card: { message: 'm', remediation: 'r', examples: { bad: 'a', good: 'b' } },\n\
                check(ctx, m) { ctx.report(m.fn); },\n\
              });\n\
            }\n";

        RuleTester::configured("rust-language", rule, "{}")
            .expect("builds")
            .reports_at("fn go() {}\n", &[(1, 1)])
            .expect(
                "a rust-only factory rule should be tested against a rust subject, not the \
                 typescript one every configured rule used before this",
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
