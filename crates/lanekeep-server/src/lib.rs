//! The LSP and MCP servers, over stdio.
//!
//! §12 specifies `lanekeep server`, "launched by editors/agent hosts". Both protocols are
//! JSON-RPC 2.0 over stdio, so [`jsonrpc`] is shared and each protocol contributes only its
//! framing and its method set.
//!
//! # No executor
//!
//! `deny.toml` denies `tokio` outright, which rules out every async LSP crate. That is the
//! right constraint here rather than an obstacle worked around: a server that reads a
//! message, answers it, and reads the next has nothing to schedule, and §13's "minimal
//! dependency surface" is easier to hold with no runtime at all.
//!
//! The cost is that a long check blocks the next message. For a tool whose warm run is tens
//! of milliseconds that is not a real cost, and the alternative buys concurrency this server
//! has no use for.

pub mod jsonrpc;
pub mod lsp;
pub mod mcp;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::jsonrpc::{Framing, Incoming, Outgoing, codes};

/// What a check produced, in the only shape the server needs.
pub type Checked = Result<Vec<lanekeep_core::Violation>, String>;

/// Run the LSP server against `input` and `output` until the client disconnects.
///
/// `check` is supplied rather than built here so the loop can be driven in a test without a
/// project on disk. The binary passes the real engine.
///
/// # Errors
///
/// Propagates an I/O failure on the transport. A failing *check* is not one: it becomes a
/// diagnostic-free publish and a log line, because an editor session should survive a rule
/// that throws.
pub fn serve_lsp(
    input: &mut impl BufRead,
    output: &mut impl Write,
    root: &Path,
    mut check: impl FnMut() -> Checked,
) -> std::io::Result<()> {
    let mut open: Vec<Document> = Vec::new();
    let mut shutting_down = false;

    while let Some(raw) = jsonrpc::read(input, Framing::Headers)? {
        let Ok(message) = serde_json::from_str::<Incoming>(&raw) else {
            // A client that sent one bad frame has not stopped being a client.
            jsonrpc::write(
                output,
                Framing::Headers,
                &Outgoing::error(None, codes::PARSE_ERROR, "not a JSON-RPC message"),
            )?;
            continue;
        };

        match message.method.as_str() {
            "initialize" => {
                reply(output, &message, Ok(lsp::capabilities()))?;
            }

            "initialized" => {}

            "textDocument/didOpen" | "textDocument/didSave" => {
                if let Some(document) = document(&message.params)
                    && !open.iter().any(|candidate| candidate.uri == document.uri)
                {
                    open.push(document);
                }
                publish(output, root, &open, &mut check)?;
            }

            "textDocument/didClose" => {
                // Diagnostics for a closed document are the client's to forget, and a server
                // that kept publishing them would grow its list without bound.
                if let Some(uri) = document_uri(&message.params) {
                    open.retain(|candidate| candidate.uri != uri);
                }
            }

            "shutdown" => {
                shutting_down = true;
                reply(output, &message, Ok(Value::Null))?;
            }

            "exit" => break,

            other => {
                // Only a request gets told; a notification nobody knows is not an error the
                // client can act on, and answering one is itself a protocol violation.
                if message.expects_reply() {
                    reply(
                        output,
                        &message,
                        Err((codes::METHOD_NOT_FOUND, format!("no method `{other}`"))),
                    )?;
                }
            }
        }

        if shutting_down && message.method == "exit" {
            break;
        }
    }

    Ok(())
}

/// Answer a request, and say nothing to a notification.
fn reply(
    output: &mut impl Write,
    message: &Incoming,
    outcome: Result<Value, (i32, String)>,
) -> std::io::Result<()> {
    if !message.expects_reply() {
        return Ok(());
    }
    let response = match outcome {
        Ok(result) => Outgoing::result(message.id.clone(), result),
        Err((code, text)) => Outgoing::error(message.id.clone(), code, text),
    };
    jsonrpc::write(output, Framing::Headers, &response)
}

/// Re-check and publish diagnostics for every open document.
///
/// Every open document, not only the one that changed: a cross-file rule can move a
/// violation from the file being edited to one that was not, and publishing only the edited
/// file would leave that one stale.
fn publish(
    output: &mut impl Write,
    root: &Path,
    open: &[Document],
    check: &mut impl FnMut() -> Checked,
) -> std::io::Result<()> {
    let violations = match check() {
        Ok(violations) => violations,
        Err(error) => {
            // Say so once and clear the squiggles, rather than leaving diagnostics from a
            // run that no longer describes the code.
            jsonrpc::write(
                output,
                Framing::Headers,
                &Outgoing::notification(
                    "window/logMessage",
                    json!({ "type": 1, "message": format!("lanekeep: {error}") }),
                ),
            )?;
            Vec::new()
        }
    };

    let grouped = lsp::by_file(root, &violations);

    for document in open {
        let diagnostics = grouped.get(&document.path).cloned().unwrap_or_default();
        jsonrpc::write(
            output,
            Framing::Headers,
            &Outgoing::notification(
                "textDocument/publishDiagnostics",
                json!({
                    "uri": document.uri,
                    "diagnostics": diagnostics,
                }),
            ),
        )?;
    }

    Ok(())
}

/// An open document: the URI the client used for it, and the file it names.
///
/// Both, because each serves a different side. A document is *identified* by its URI — that
/// is what the client opens, closes and matches a publish against, so one file open under two
/// spellings is two documents, each published to. It is *matched* to violations by its path,
/// canonicalized so that it meets the root's spelling: `lanekeep server` canonicalizes the
/// root at startup, so the keys `by_file` builds carry `/private/var` where the editor said
/// `/var`, and on Windows a `\\?\` prefix and a long name where it said `C:\Users\RUNNER~1`.
struct Document {
    uri: String,
    path: PathBuf,
}

/// The URI a `textDocument` parameter names.
fn document_uri(params: &Value) -> Option<&str> {
    params["textDocument"]["uri"].as_str()
}

/// The document a `textDocument` parameter refers to.
///
/// A path that cannot be canonicalized — one that does not exist, or that a test made up —
/// keeps its given spelling, since there is nothing on disk to resolve it against.
fn document(params: &Value) -> Option<Document> {
    let uri = document_uri(params)?;
    let path = lsp::path_from_uri(uri)?;
    let path = path.canonicalize().unwrap_or(path);
    Some(Document {
        uri: uri.to_owned(),
        path,
    })
}

#[cfg(test)]
mod tests {
    use lanekeep_core::{FilePath, Location, Position, RuleId, Severity, Violation};

    use super::*;

    fn framed(messages: &[Value]) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        for message in messages {
            let body = message.to_string();
            let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
        }
        out
    }

    /// Every message the server wrote back, parsed.
    fn exchange(messages: &[Value], check: impl FnMut() -> Checked) -> Vec<Value> {
        exchange_at(Path::new("/project"), messages, check)
    }

    /// [`exchange`], against a root of the test's choosing.
    fn exchange_at(root: &Path, messages: &[Value], check: impl FnMut() -> Checked) -> Vec<Value> {
        let wire = framed(messages);
        let mut input = std::io::BufReader::new(wire.as_bytes());
        let mut output = Vec::new();
        serve_lsp(&mut input, &mut output, root, check).expect("serves");

        let text = String::from_utf8(output).expect("utf-8");
        let mut cursor = std::io::BufReader::new(text.as_bytes());
        let mut out = Vec::new();
        while let Ok(Some(raw)) = jsonrpc::read(&mut cursor, Framing::Headers) {
            out.push(serde_json::from_str(&raw).expect("parses"));
        }
        out
    }

    fn a_violation() -> Violation {
        Violation {
            rule_id: "local/example".parse::<RuleId>().expect("valid"),
            location: Location::new(FilePath::new("src/a.ts"), Position::new(3, 5)),
            message: "something".to_owned(),
            remediation: "do this".to_owned(),
            severity: Severity::Error,
            fix: None,
        }
    }

    fn open(uri: &str) -> Value {
        json!({
            "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": uri } }
        })
    }

    /// A document opened under another spelling of a real file is matched to the root's
    /// spelling, and published under the client's own.
    ///
    /// The root is canonical because `lanekeep server` canonicalizes it, and the editor's
    /// spelling is whatever it had: `/var/folders/...` on macOS for a root that resolves under
    /// `/private/var`, `C:\Users\RUNNER~1\...` on Windows for a root spelled
    /// `\\?\C:\Users\runneradmin\...`, a symlinked checkout anywhere. Without the match the
    /// server publishes an empty list for every document, which reads as a clean file — that
    /// is what `server_agreement.rs` found on Windows, as a read that never came back.
    #[test]
    fn an_alias_of_an_open_file_is_matched_to_the_root_and_published_as_sent() {
        let dir =
            std::env::temp_dir().join(format!("lanekeep-server-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real/src")).expect("creates the tree");
        std::fs::write(dir.join("real/src/a.ts"), "").expect("writes the file");
        let root = dir.join("real").canonicalize().expect("a canonical root");

        // On Unix the alias is a symlink to the project — the shape a symlinked home or
        // checkout hands an editor — and on Windows the temp path as given, which differs from
        // the canonical spelling by its `\\?\` prefix and, on a hosted runner, by its short
        // name. Either way it must differ, or the test asserts nothing about aliasing.
        #[cfg(unix)]
        let alias = {
            let link = dir.join("link");
            std::os::unix::fs::symlink(dir.join("real"), &link).expect("links the alias");
            link
        };
        #[cfg(not(unix))]
        let alias = dir.join("real");
        assert_ne!(
            alias, root,
            "the alias has to be another spelling of the root"
        );

        let uri = lsp::uri_from_path(&alias.join("src/a.ts"));
        let replies = exchange_at(&root, &[open(&uri)], || Ok(vec![a_violation()]));
        let _ = std::fs::remove_dir_all(&dir);

        let published = replies
            .iter()
            .find(|m| m["method"] == "textDocument/publishDiagnostics")
            .expect("published");
        assert_eq!(
            published["params"]["uri"], uri,
            "published under the client's spelling"
        );
        assert_eq!(
            published["params"]["diagnostics"]
                .as_array()
                .expect("an array")
                .len(),
            1,
            "the alias met the root's spelling of the file"
        );
    }

    /// One file open under two spellings is two documents to the client, and each gets its
    /// list — matched by the path they share, published under the URI each was opened with.
    #[test]
    fn a_file_open_under_two_spellings_is_published_under_each() {
        let replies = exchange(
            &[
                open("file:///project/src/a.ts"),
                open("file:///project/./src/a.ts"),
            ],
            || Ok(vec![a_violation()]),
        );

        let published: Vec<&Value> = replies
            .iter()
            .filter(|m| m["method"] == "textDocument/publishDiagnostics")
            .collect();
        // One publish after the first open, two after the second.
        assert_eq!(published.len(), 3);
        assert_eq!(published[1]["params"]["uri"], "file:///project/src/a.ts");
        assert_eq!(published[2]["params"]["uri"], "file:///project/./src/a.ts");
        for message in &published[1..] {
            assert_eq!(
                message["params"]["diagnostics"]
                    .as_array()
                    .expect("an array")
                    .len(),
                1
            );
        }
    }

    #[test]
    fn initialize_is_answered_with_capabilities() {
        let replies = exchange(
            &[json!({ "id": 1, "method": "initialize", "params": {} })],
            || Ok(Vec::new()),
        );
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "lanekeep");
    }

    #[test]
    fn opening_a_document_publishes_its_diagnostics() {
        let replies = exchange(&[open("file:///project/src/a.ts")], || {
            Ok(vec![a_violation()])
        });

        let published = replies
            .iter()
            .find(|m| m["method"] == "textDocument/publishDiagnostics")
            .expect("published");
        assert_eq!(published["params"]["uri"], "file:///project/src/a.ts");

        let diagnostics = published["params"]["diagnostics"]
            .as_array()
            .expect("an array");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["range"]["start"]["line"], 2);
        assert_eq!(diagnostics[0]["code"], "local/example");
    }

    #[test]
    fn a_clean_file_is_published_with_an_empty_list() {
        // The only way to clear a squiggle the author already fixed. Skipping the publish
        // leaves the old diagnostic on screen forever.
        let replies = exchange(&[open("file:///project/src/a.ts")], || Ok(Vec::new()));
        let published = replies
            .iter()
            .find(|m| m["method"] == "textDocument/publishDiagnostics")
            .expect("published");
        assert_eq!(
            published["params"]["diagnostics"]
                .as_array()
                .expect("an array")
                .len(),
            0
        );
    }

    #[test]
    fn every_open_document_is_republished_when_one_changes() {
        // A cross-file rule can move a violation into a file nobody touched. Publishing only
        // the edited one leaves that file's diagnostics describing an older corpus.
        let replies = exchange(
            &[
                open("file:///project/src/a.ts"),
                open("file:///project/src/b.ts"),
            ],
            || Ok(Vec::new()),
        );

        let published: Vec<&Value> = replies
            .iter()
            .filter(|m| m["method"] == "textDocument/publishDiagnostics")
            .collect();
        // One for the first open, two for the second.
        assert_eq!(published.len(), 3);
        assert_eq!(published[2]["params"]["uri"], "file:///project/src/b.ts");
    }

    #[test]
    fn closing_a_document_stops_publishing_for_it() {
        let replies = exchange(
            &[
                open("file:///project/src/a.ts"),
                json!({
                    "method": "textDocument/didClose",
                    "params": { "textDocument": { "uri": "file:///project/src/a.ts" } }
                }),
                open("file:///project/src/b.ts"),
            ],
            || Ok(Vec::new()),
        );

        let uris: Vec<&str> = replies
            .iter()
            .filter(|m| m["method"] == "textDocument/publishDiagnostics")
            .filter_map(|m| m["params"]["uri"].as_str())
            .collect();
        assert!(
            !uris[1..].contains(&"file:///project/src/a.ts"),
            "a closed document was still published: {uris:?}"
        );
    }

    #[test]
    fn a_failing_check_logs_and_clears_rather_than_ending_the_session() {
        let replies = exchange(&[open("file:///project/src/a.ts")], || {
            Err("rule threw".to_owned())
        });

        assert!(
            replies.iter().any(|m| m["method"] == "window/logMessage"
                && m["params"]["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("rule threw"))),
            "the failure should be logged: {replies:?}"
        );
        assert!(
            replies
                .iter()
                .any(|m| m["method"] == "textDocument/publishDiagnostics"),
            "and diagnostics still published"
        );
    }

    #[test]
    fn an_unknown_request_is_refused_and_an_unknown_notification_is_not() {
        let replies = exchange(
            &[
                json!({ "id": 7, "method": "textDocument/formatting" }),
                json!({ "method": "$/setTrace", "params": {} }),
            ],
            || Ok(Vec::new()),
        );
        assert_eq!(
            replies.len(),
            1,
            "only the request is answered: {replies:?}"
        );
        assert_eq!(replies[0]["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn a_malformed_message_is_answered_and_the_session_continues() {
        let body = "not json at all";
        let wire = format!(
            "Content-Length: {}\r\n\r\n{body}{}",
            body.len(),
            framed(&[json!({ "id": 1, "method": "initialize" })])
        );
        let mut input = std::io::BufReader::new(wire.as_bytes());
        let mut output = Vec::new();
        serve_lsp(&mut input, &mut output, Path::new("/project"), || {
            Ok(Vec::new())
        })
        .expect("serves");

        let text = String::from_utf8(output).expect("utf-8");
        assert!(text.contains("-32700"), "a parse error is reported: {text}");
        assert!(
            text.contains("lanekeep"),
            "and initialize is still answered: {text}"
        );
    }

    #[test]
    fn exit_ends_the_loop() {
        let replies = exchange(
            &[
                json!({ "id": 1, "method": "shutdown" }),
                json!({ "method": "exit" }),
                json!({ "id": 2, "method": "initialize" }),
            ],
            || Ok(Vec::new()),
        );
        assert_eq!(
            replies.len(),
            1,
            "nothing after exit is served: {replies:?}"
        );
        assert_eq!(replies[0]["id"], 1);
    }
}
