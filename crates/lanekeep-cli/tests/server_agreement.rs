//! The server's diagnostics equal `lanekeep check`'s, over the same tree state.
//!
//! Today the server cannot drift: `crates/lanekeep-server/src/lib.rs:135` runs one full
//! project check per open or save and filters by open file. An incremental oracle is what
//! would put that at risk, so this test is written *before* any state is held — it is the
//! thing the held provider has to keep true, not evidence gathered after the fact.
//!
//! The edit in the middle is the point. A `.d.ts` under `node_modules` is outside the checked
//! set: discovery never looks at it, the oracle reads it, and the session caches it. A test
//! that only opened a file and compared once would pass against a provider that revalidated
//! nothing at all.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below, and the `corpus` helpers this \
              file also compiles, are neither, so the grant it already makes for unit \
              tests has to be restated for them."
)]
#![expect(
    clippy::print_stderr,
    reason = "the `tsc` case has to say why it did nothing when `typescript` is absent: the \
              alternative is a suite reporting a pass for a test it did not run"
)]

mod corpus;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use corpus::tsc_available;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// This repository's own authoring `typescript`, absolute and forward-slashed.
///
/// Spelled with forward slashes: `validate_specifier` and JSON escaping both have history
/// with backslashes on Windows, and Rust accepts either separator there, so `C:/Users/...`
/// is still absolute and carries nothing either gate refuses — the same reasoning
/// `tsc_provider.rs`'s `types_block` documents for its own copy of this path.
fn typescript_package() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript")
        .canonicalize()
        .expect("the authoring package is installed")
        .to_string_lossy()
        .replace('\\', "/")
}

/// A rule that asks the oracle for the type of an imported value.
///
/// `requires: ['types']`, so the engine hands it `ctx.types`; without the declaration file it
/// gets `undefined` and says nothing, which is the silence posture every type-aware rule
/// takes. That silence is also what makes the second half of the test meaningful: the edit
/// changes the answer from `number` to `string`, and the violations disappear.
const RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-number-money',
  severity: 'error',
  requires: ['types'],
  card: {
    message: 'a money value must not be a bare number',
    remediation: 'give it the Money type the package exports',
    examples: { bad: 'const total = rate', good: 'const total: Money = rate' },
  },
  query: '(variable_declarator name: (identifier) @name value: (identifier) @value)',
  check(ctx, m) {
    const type = ctx.types.typeOf(m.value)
    // `undefined` means the oracle could not be sure. Reporting on it would accuse code it
    // could not read.
    if (type === undefined) return
    if (type.primitive !== 'number') return
    ctx.report(m.name)
  },
})
";

/// The fixture package the rule resolves through.
///
/// Kept byte-identical to plan 4's resolver fixture: two fixtures for one resolution shape
/// would drift, and the one that drifted would be the one nobody ran. If plan 4's differs,
/// take plan 4's and change this.
const PACKAGE_JSON: &str = r#"{
  "name": "@acme/rates",
  "version": "1.0.0",
  "types": "index.d.ts"
}
"#;

const DECLARATION_NUMBER: &str = "export declare const rate: number\n";
const DECLARATION_STRING: &str = "export declare const rate: string\n";

/// A project on disk, removed on drop.
struct Tree {
    dir: PathBuf,
}

impl Tree {
    fn new(name: &str, provider: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-agree-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let tree = Self { dir };
        tree.write("package.json", "{}\n");
        tree.write("lanekeep/rules/no-number-money.ts", RULE);
        tree.write("node_modules/@acme/rates/package.json", PACKAGE_JSON);
        tree.write("node_modules/@acme/rates/index.d.ts", DECLARATION_NUMBER);
        tree.write(
            "src/a.ts",
            "import { rate } from '@acme/rates'\nexport const total = rate\n",
        );
        tree.write(
            "src/b.ts",
            "import { rate } from '@acme/rates'\nexport const fee = rate\n",
        );
        // Ten minutes in milliseconds on both budgets, far above what the work needs. A
        // loaded machine tripping a budget here would be reported as a misbehaving rule,
        // which is the failure AGENTS.md's limits invariant exists to keep out of the output.
        //
        // `tsc` also needs `types.typescript`: the fixture tree is a throwaway directory in
        // the system temp with no `node_modules` of its own, so it is pointed at this
        // repository's own authoring package by absolute path — the same thing
        // `tsc_provider.rs`'s `types_block` does, and for the same reason. `builtin` needs
        // nothing extra, and the field is only emitted when it applies.
        let types_block = if provider == "tsc" {
            format!(
                r#"{{"provider": "tsc", "typescript": "{}"}}"#,
                typescript_package()
            )
        } else {
            format!(r#"{{"provider": "{provider}"}}"#)
        };
        tree.write(
            "lanekeep.json",
            &format!(
                r#"{{"include": ["src/**"],
 "timeouts": {{"rule": 600000, "global": 600000}},
 "types": {types_block},
 "rules": ["./lanekeep/rules/no-number-money.ts"]}}
"#
            ),
        );
        tree
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// `lanekeep check --format json`, normalized to `(file, line, column, message)`.
    fn checked(&self) -> Vec<(String, u64, u64, String)> {
        let output = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .args(["--format", "json", "--timeout", "600000"])
            .output()
            .expect("runs the binary");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_ne!(
            output.status.code(),
            Some(2),
            "the run failed:\n{}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );

        let document: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad json ({e}): {stdout}"));
        let mut found: Vec<_> = document["violations"]
            .as_array()
            .unwrap_or_else(|| panic!("no violations array in: {stdout}"))
            .iter()
            .map(|v| {
                let at = &v["location"];
                (
                    at["file"].as_str().unwrap_or("?").to_owned(),
                    at["position"]["line"].as_u64().unwrap_or(0),
                    at["position"]["column"].as_u64().unwrap_or(0),
                    v["message"].as_str().unwrap_or("?").to_owned(),
                )
            })
            .collect();
        found.sort();
        found
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How long a test waits for one message from the server.
///
/// A deadline rather than a blocking read, for the reason `watch_dependency_paths.rs` gives
/// for its own: without one, a server that never publishes — a held provider that cannot serve
/// another request, a check that hangs — makes this suite wait for the whole harness timeout
/// and fail with nothing that names which read never came back. Far above what the work needs:
/// the `tsc` case builds a real program, and a loaded machine tripping this would be a fixture
/// reporting a lanekeep bug it did not find.
///
/// And under the harness's own wall, which `.config/nextest.toml` sets at four 30-second
/// periods: a deadline that cannot fire first names nothing. Two minutes would sit at that wall
/// and lose to it — on Windows CI a publish that never matched surfaced as nextest's own bare
/// `TIMEOUT [120.037s]`, with nothing about which read never came back.
const READ_DEADLINE: Duration = Duration::from_mins(1);

/// A running `lanekeep server --protocol lsp`, spoken to over its own pipes.
struct Session {
    child: Child,
    stdin: ChildStdin,
    /// Framed messages, read by a thread so that every read here can carry a deadline.
    /// `BufReader::read_line` cannot be given one, which is the same reason
    /// `lanekeep_types`'s sidecar reads its answers through a channel.
    messages: std::sync::mpsc::Receiver<serde_json::Value>,
    next_id: i64,
}

/// One message from a framed stream, or `None` at end of input.
///
/// Hand-read rather than through `lanekeep_server::jsonrpc::read` so that the test does not
/// agree with the server about framing by construction.
fn read_framed(stdout: &mut BufReader<ChildStdout>) -> Option<serde_json::Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = stdout.read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0_u8; length?];
    std::io::Read::read_exact(stdout, &mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

impl Session {
    fn start(tree: &Tree) -> Self {
        // stderr inherited: `prepare` writes notes there under `types.provider: 'tsc'`, and a
        // piped stderr nobody drains is a deadlock waiting for a long enough note.
        let mut child = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("server")
            .arg(&tree.dir)
            .args(["--protocol", "lsp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawns the server");
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let (sender, messages) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Some(message) = read_framed(&mut stdout) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            messages,
            next_id: 0,
        }
    }

    /// Header framing, exactly as `crates/lanekeep-server/src/jsonrpc.rs:216` writes it.
    fn send(&mut self, message: &serde_json::Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("writes a frame");
        self.stdin.flush().expect("flushes");
    }

    fn request(&mut self, method: &str, params: &serde_json::Value) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
    }

    fn notify(&mut self, method: &str, params: &serde_json::Value) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }));
    }

    /// One message from the server, or a panic past [`READ_DEADLINE`].
    ///
    /// The deadline is the point: a server that stops publishing is exactly what a held
    /// provider that cannot serve another request looks like, and a blocking read turns that
    /// into a suite that hangs rather than one that fails naming the read.
    fn read(&mut self) -> serde_json::Value {
        self.messages
            .recv_timeout(READ_DEADLINE)
            .expect("the server sends a message before the deadline")
    }

    /// Read until every named file has been published for, and answer with the last publish
    /// seen for each.
    ///
    /// The last, because opening the second document republishes the first — a cross-file
    /// rule can move a violation into a file nobody touched, and
    /// `every_open_document_is_republished_when_one_changes` in `lanekeep-server` pins that
    /// behavior. Taking the first publish for each file would compare a whole-project check
    /// against a partial one.
    fn diagnostics(&mut self, uris: &[String]) -> Vec<(String, u64, u64, String)> {
        let mut latest: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();
        while latest.len() < uris.len() || uris.iter().any(|uri| !latest.contains_key(uri)) {
            let message = self.read();
            if message["method"] != "textDocument/publishDiagnostics" {
                continue;
            }
            let uri = message["params"]["uri"].as_str().expect("a uri").to_owned();
            let list = message["params"]["diagnostics"]
                .as_array()
                .expect("an array")
                .clone();
            latest.insert(uri, list);
        }

        let mut out = Vec::new();
        for (uri, list) in latest {
            let file = uri.rsplit_once("/src/").expect("a src path").1.to_owned();
            for diagnostic in list {
                let start = &diagnostic["range"]["start"];
                out.push((
                    format!("src/{file}"),
                    start["line"].as_u64().unwrap_or(0) + 1,
                    start["character"].as_u64().unwrap_or(0) + 1,
                    diagnostic["message"]
                        .as_str()
                        .unwrap_or("?")
                        .lines()
                        .next()
                        .unwrap_or("?")
                        .to_owned(),
                ));
            }
        }
        out.sort();
        out
    }

    fn finish(mut self) {
        self.request("shutdown", &serde_json::json!({}));
        self.notify("exit", &serde_json::json!({}));
        let _ = self.child.wait();
    }
}

fn uri(tree: &Tree, relative: &str) -> String {
    let path = tree
        .dir
        .canonicalize()
        .unwrap_or_else(|_| tree.dir.clone())
        .join(relative);
    format!("file://{}", path.to_string_lossy().replace('\\', "/"))
}

/// The whole agreement, for one provider.
fn agreement_under(provider: &str) {
    let tree = Tree::new(provider, provider);
    let a = uri(&tree, "src/a.ts");
    let b = uri(&tree, "src/b.ts");
    let opened = vec![a.clone(), b.clone()];

    let mut session = Session::start(&tree);
    session.request("initialize", &serde_json::json!({}));
    let _ = session.read();
    session.notify("initialized", &serde_json::json!({}));

    for target in &opened {
        session.notify(
            "textDocument/didOpen",
            &serde_json::json!({ "textDocument": { "uri": target } }),
        );
    }
    let d1 = session.diagnostics(&opened);
    let j1 = tree.checked();

    assert!(
        !d1.is_empty(),
        "the fixture reports before the edit: {d1:?}"
    );
    assert_eq!(
        d1, j1,
        "the server and `lanekeep check` disagree before the edit"
    );

    // The edit the session has to notice: a declaration file outside the checked set.
    tree.write("node_modules/@acme/rates/index.d.ts", DECLARATION_STRING);

    session.notify(
        "textDocument/didSave",
        &serde_json::json!({ "textDocument": { "uri": &a } }),
    );
    let d2 = session.diagnostics(&opened);
    let j2 = tree.checked();

    assert_eq!(
        d2, j2,
        "the server and `lanekeep check` disagree after the edit"
    );
    assert_ne!(
        d1, d2,
        "the declaration edit changed nothing, so this asserts equality of two stale answers"
    );

    session.finish();
}

#[test]
fn the_servers_diagnostics_equal_lanekeep_checks_under_the_builtin_provider() {
    agreement_under("builtin");
}

#[test]
fn the_servers_diagnostics_equal_lanekeep_checks_under_tsc() {
    if !tsc_available() {
        eprintln!(
            "skipped: no `packages/lanekeep/node_modules/.bin/tsc`; run `npm ci` in \
             packages/lanekeep, or read this on the Linux gate job"
        );
        return;
    }
    agreement_under("tsc");
}

/// A rule that reports on every declaration and asks the oracle nothing.
///
/// It **reports**, deliberately: a rule that found nothing would make the two halves of the
/// comparison below two empty lists, and a server that failed the request outright publishes
/// an empty list too — the failure this pins would pass. Its `requires` is the parameter: with
/// `['types']` the capability gate refuses the run when no provider could be built, and
/// without it the same failed spawn is something nobody asked about.
fn reporting_rule(requires: &str) -> String {
    format!(
        r"import {{ defineRule }} from 'lanekeep'

export default defineRule({{
  id: 'local/every-declaration',
  severity: 'error',
  {requires}
  card: {{
    message: 'a declaration',
    remediation: 'nothing to do',
    examples: {{ bad: 'const a = 1', good: 'nothing' }},
  }},
  query: '(variable_declarator name: (identifier) @it)',
  check(ctx, m) {{
    ctx.report(m.it)
  }},
}})
"
    )
}

/// A project whose `types.provider` is `tsc` and whose command cannot be started.
///
/// The command is a name no `PATH` has, which is the shape a user reaches by having no Node —
/// the realistic case, and the one `lanekeep check` already handles by asking whether anything
/// enabled needs `types` at all.
fn unstartable_tree(name: &str, requires: &str) -> Tree {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "lanekeep-unstartable-{name}-{}-{seq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let tree = Tree { dir };
    tree.write("package.json", "{}\n");
    tree.write("lanekeep/rules/reporting.ts", &reporting_rule(requires));
    tree.write("src/a.ts", "export const total = 1\n");
    tree.write(
        "lanekeep.json",
        r#"{"include": ["src/**"],
 "timeouts": {"rule": 600000, "global": 600000},
 "types": {"provider": "tsc", "command": ["lanekeep-no-such-command-x9"]},
 "rules": ["./lanekeep/rules/reporting.ts"]}
"#,
    );
    tree
}

/// `lanekeep server` answers every request `lanekeep check` answers.
///
/// A session that could not build a provider used to propagate that as a hard error, so with
/// `types.provider: 'tsc'` and an unstartable command the server failed *every* request while
/// `lanekeep check` over the same project exited 0 and reported normally. Only the engine's
/// capability gate can decide whether a failed spawn matters — it is the thing that knows
/// whether any enabled rule asked for `types` — so a session that cannot build one arrives at
/// the engine with none and lets it reach the same verdict.
#[test]
fn the_server_answers_where_check_answers_with_an_unstartable_provider() {
    let tree = unstartable_tree("nobody-asked", "");
    let a = uri(&tree, "src/a.ts");
    let opened = vec![a.clone()];

    let checked = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
        .arg("check")
        .arg(&tree.dir)
        .args(["--format", "json", "--timeout", "600000"])
        .output()
        .expect("runs the binary");
    // One, not zero: the rule reports, so a clean run exits with violations found. What
    // matters is that it is not two — the exit a run that could not be prepared takes.
    assert_eq!(
        checked.status.code(),
        Some(1),
        "the control is wrong: `check` must reach the corpus for this to pin anything\n{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        !tree.checked().is_empty(),
        "the fixture reports nothing, so the comparison below is two empty lists and a server \
         that failed the request outright would satisfy it"
    );

    let mut session = Session::start(&tree);
    session.request("initialize", &serde_json::json!({}));
    let _ = session.read();
    session.notify("initialized", &serde_json::json!({}));
    session.notify(
        "textDocument/didOpen",
        &serde_json::json!({ "textDocument": { "uri": &a } }),
    );

    assert_eq!(
        session.diagnostics(&opened),
        tree.checked(),
        "the server and `lanekeep check` disagree about a project neither needs a provider for"
    );
    session.finish();
}

/// And where `check` refuses, the server refuses with the same words.
///
/// The other half of the same decision: one enabled rule declaring `requires: ['types']` makes
/// the failed spawn matter, and both paths have to say so — the server by logging it, which is
/// what it does with any check that cannot run, rather than by publishing stale squiggles.
#[test]
fn the_server_refuses_where_check_refuses_and_says_the_same_thing() {
    let tree = unstartable_tree("someone-asked", "requires: ['types'],");

    let checked = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
        .arg("check")
        .arg(&tree.dir)
        .args(["--format", "json", "--timeout", "600000"])
        .output()
        .expect("runs the binary");
    assert_eq!(
        checked.status.code(),
        Some(2),
        "a rule that needs `types` and a provider that cannot start is a refusal"
    );
    let refusal = String::from_utf8_lossy(&checked.stderr).into_owned();
    assert!(
        refusal.contains("lanekeep-no-such-command-x9"),
        "the refusal does not name the command that could not be started: {refusal}"
    );

    let a = uri(&tree, "src/a.ts");
    let mut session = Session::start(&tree);
    session.request("initialize", &serde_json::json!({}));
    let _ = session.read();
    session.notify("initialized", &serde_json::json!({}));
    session.notify(
        "textDocument/didOpen",
        &serde_json::json!({ "textDocument": { "uri": &a } }),
    );

    let logged = loop {
        let message = session.read();
        if message["method"] == "window/logMessage" {
            break message["params"]["message"]
                .as_str()
                .expect("a log message")
                .to_owned();
        }
    };
    assert!(
        logged.contains("local/every-declaration")
            && logged.contains("lanekeep-no-such-command-x9"),
        "the server's refusal does not say what `check`'s does\n  server: {logged}\n  check: \
         {refusal}"
    );
    session.finish();
}
