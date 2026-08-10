//! A throwaway multi-file project, for testing cross-file rules.
//!
//! `RuleTester` runs one subject file at a time, which is exactly what a cross-file rule
//! cannot be tested with. This drives the binary over a real directory instead.
//!
//! # It generates a `lanekeep.json`, and it has to
//!
//! This wrote a `lanekeep.config.ts` importing `lanekeep/<rule>` until the four TypeScript
//! built-ins were compiled into a component. A component is not a value a module can import —
//! `lanekeep_js::ResolveError::NotAModule` says so, and says to name it in a `lanekeep.json` —
//! so the config format is forced by what the rules now are, exactly as it already was for
//! `RuleTester::for_component`.
//!
//! **The change is what makes the two test files that use this meaningful.** They are held to
//! passing unmodified across the migration, and a helper that kept writing a `.config.ts` could
//! only have done that by keeping the rules resolvable as modules — which is to say by having
//! them still run in QuickJS. Those files would then have been green before the change and green
//! after it, while asserting nothing about the thing that changed.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes projects built in the same process.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A project on disk with one built-in rule configured, removed on drop.
pub(crate) struct Corpus {
    dir: PathBuf,
}

impl Corpus {
    /// Build a project whose one rule is `<rule>` configured with `<options>`.
    ///
    /// `options` is written the way a `.config.ts` would have spelled it — a JavaScript object
    /// literal — because that is what the callers pass and they do not change. See [`as_json`].
    pub(crate) fn new(rule: &str, options: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-corpus-{rule}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let config = serde_json::json!({
            "include": ["src/**"],
            "rules": [{ "rule": format!("lanekeep/{rule}"), "options": as_json(options) }],
        });

        let corpus = Self { dir };
        // `to_string` rather than `to_string_pretty`: nothing reads this by eye except when a
        // test fails, and a failure prints the tool's error rather than the config.
        corpus.write("lanekeep.json", &config.to_string());
        for (path, contents) in files {
            corpus.write(path, contents);
        }
        corpus
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes file");
    }

    /// Violations as `file:line:column message`, in the order the tool reported them.
    ///
    /// Rendered rather than structured, because the ordering *is* part of what these tests
    /// assert — a cross-file rule that reports the same set in a different order every run
    /// has failed the determinism invariant even though the set is right.
    pub(crate) fn run(&self) -> Vec<String> {
        let output = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .arg("--format")
            .arg("json")
            // Milliseconds, and far above what the work needs.
            //
            // The rules these tests drive live in a 12.4 MiB component, and every corpus is a
            // fresh directory, so the first `check` in each one compiles that component from
            // scratch — measured at 14.4 s on an otherwise idle machine, and several times that
            // when nextest is running two dozen of these at once. The default 15 s budget is a
            // limit on *rule execution*, and tripping it here would be reporting a machine's
            // load as a rule's misbehavior, which is the failure `AGENTS.md`'s limits invariant
            // exists to keep out of the output.
            //
            // Raised rather than removed: a rule that genuinely hangs still fails, in under a
            // minute, rather than wedging the suite.
            .arg("--timeout")
            .arg("600000")
            .output()
            .expect("runs the binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.code() != Some(2),
            "the run failed:\n{stderr}\n{stdout}"
        );

        let document: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));

        document["violations"]
            .as_array()
            .unwrap_or_else(|| panic!("no violations array in: {stdout}"))
            .iter()
            .map(|v| {
                let at = &v["location"];
                format!(
                    "{}:{}:{} {}",
                    at["file"].as_str().unwrap_or("?"),
                    at["position"]["line"].as_u64().unwrap_or(0),
                    at["position"]["column"].as_u64().unwrap_or(0),
                    v["message"].as_str().unwrap_or("?"),
                )
            })
            .collect()
    }
}

impl Drop for Corpus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A JavaScript object literal as JSON.
///
/// The callers spell their options the way a `lanekeep.config.ts` would — `{}`,
/// `{ maxDepth: 24 }`, `{ entryPoints: ['src/index.ts'] }` — and they are held to passing
/// unmodified, so the conversion happens here. Two differences from JSON and no others: a key
/// may be a bare identifier, and a string may be single-quoted.
///
/// **It validates rather than approximating.** A shape this does not handle produces something
/// `serde_json` refuses, and the panic names the input and what it became. That matters more
/// than the conversion being general: `AGENTS.md` records that a fixture refused by an earlier
/// gate than the one under test either passes for the wrong reason or fails with a message
/// pointing somewhere else — a silently mangled option would leave a rule configured with
/// something nobody wrote, and an over-reporting rule reads exactly like a rule that works.
fn as_json(options: &str) -> serde_json::Value {
    let mut out = String::with_capacity(options.len() + 16);
    let mut chars = options.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // A single-quoted string becomes a double-quoted one. No escape handling: none of
            // these fixtures contains one, and `serde_json` refuses whatever it would produce.
            '\'' => {
                out.push('"');
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    out.push(c);
                }
                out.push('"');
            }
            // A bare identifier is a key if a colon follows it, and a literal otherwise —
            // `true`, `false` and `null` are spelled the same in both languages, so they are
            // left alone by not being followed by one.
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphanumeric() || next == '_' || next == '$' {
                        word.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let mut spacing = String::new();
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() {
                        spacing.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&':') {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                } else {
                    out.push_str(&word);
                }
                out.push_str(&spacing);
            }
            _ => out.push(c),
        }
    }

    serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!("`{options}` is not a rule's options that this helper can put in a JSON config\n  it became `{out}`, which {e}")
    })
}
