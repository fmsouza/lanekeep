//! A throwaway multi-file project, for testing cross-file rules.
//!
//! `RuleTester` runs one subject file at a time, which is exactly what a cross-file rule
//! cannot be tested with. This drives the binary over a real directory instead.

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
    /// Build a project whose config is `<rule>(<options>)`.
    pub(crate) fn new(rule: &str, options: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-corpus-{rule}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let corpus = Self { dir };
        corpus.write(
            "lanekeep.config.ts",
            &format!(
                "import {{ defineConfig }} from 'lanekeep';\n\
                 import rule from 'lanekeep/{rule}';\n\
                 export default defineConfig({{ include: ['src/**'], rules: [rule({options})] }});\n"
            ),
        );
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
