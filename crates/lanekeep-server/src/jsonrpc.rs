//! JSON-RPC 2.0, and the two ways it arrives on stdin.
//!
//! LSP and MCP are the same protocol. Both are JSON-RPC 2.0 over stdio; they differ in how a
//! message is delimited and in which methods exist. That is why this module is shared rather
//! than duplicated per protocol — the parts that differ are [`Framing`] and the dispatch
//! table, and nothing else.
//!
//! Written by hand because `tokio` is denied outright by `deny.toml`, which rules out every
//! async LSP crate. That constraint turned out to be the right shape anyway: a language
//! server that reads a message, answers it, and reads the next one has no use for an
//! executor, and §13's "minimal dependency surface" is easier to hold with none.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How messages are delimited on the wire.
///
/// The one place the two protocols genuinely differ at the transport level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// LSP: `Content-Length: N\r\n\r\n` then N bytes.
    Headers,
    /// MCP over stdio: one JSON object per line.
    Lines,
}

/// A request or notification arriving from the client.
///
/// One type for both, because they differ only in whether `id` is present — a notification is
/// a request nobody is waiting on. Splitting them into two types would mean writing every
/// dispatch arm twice.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    /// Absent for a notification, which must not be answered.
    #[serde(default)]
    pub id: Option<Value>,
    /// Which method was called.
    pub method: String,
    /// Arguments, defaulting to null so a method that takes none still parses.
    #[serde(default)]
    pub params: Value,
}

impl Incoming {
    /// Whether a reply is expected. A notification answered anyway is a protocol violation.
    #[must_use]
    pub const fn expects_reply(&self) -> bool {
        self.id.is_some()
    }
}

/// A reply, successful or not.
#[derive(Debug, Clone, Serialize)]
pub struct Outgoing {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// Echoes the request's id; absent on a notification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The answer, when the call succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why it did not, when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
    /// Set only on a server-initiated notification, which carries a method and no id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The notification's payload, alongside `method`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC error.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorObject {
    /// One of [`codes`].
    pub code: i32,
    /// What went wrong, for a human reading the client's log.
    pub message: String,
}

/// The codes this server uses, from the JSON-RPC 2.0 specification.
pub mod codes {
    /// The message was not valid JSON.
    pub const PARSE_ERROR: i32 = -32700;
    /// Valid JSON, but not a valid request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// No such method.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// The method exists; the arguments do not work.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Anything the handler itself failed at.
    pub const INTERNAL_ERROR: i32 = -32603;
}

impl Outgoing {
    /// A successful reply to a request.
    #[must_use]
    pub fn result(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
            method: None,
            params: None,
        }
    }

    /// A failed reply to a request.
    #[must_use]
    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
            }),
            method: None,
            params: None,
        }
    }

    /// A notification the server sends unprompted — diagnostics, most of the time.
    #[must_use]
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: None,
            result: None,
            error: None,
            method: Some(method.into()),
            params: Some(params),
        }
    }
}

/// Read one message, or `None` at end of input.
///
/// # Errors
///
/// Returns an error only for an I/O failure. A malformed *message* is not an error here: the
/// caller answers it with a parse error and reads the next one, because a client that sends
/// one bad frame has not necessarily stopped being a client.
pub fn read(input: &mut impl BufRead, framing: Framing) -> std::io::Result<Option<String>> {
    match framing {
        Framing::Lines => {
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let line = line.trim().to_owned();
            // A blank line between messages is not a message.
            if line.is_empty() {
                return read(input, framing);
            }
            Ok(Some(line))
        }

        Framing::Headers => {
            let mut length: Option<usize> = None;

            loop {
                let mut line = String::new();
                if input.read_line(&mut line)? == 0 {
                    return Ok(None);
                }
                let line = line.trim_end_matches(['\r', '\n']);

                // The blank line ends the headers.
                if line.is_empty() {
                    break;
                }

                // Case-insensitive: the header name is not required to be spelled one way,
                // and a client that sends `content-length` is not sending a bad message.
                if let Some((name, value)) = line.split_once(':')
                    && name.trim().eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().ok();
                }
            }

            // A body with no length is unreadable — there is no way to know where it ends,
            // so the stream is no longer parseable and stopping is the honest answer.
            let Some(length) = length else {
                return Ok(None);
            };

            let mut body = vec![0_u8; length];
            std::io::Read::read_exact(input, &mut body)?;
            Ok(Some(String::from_utf8_lossy(&body).into_owned()))
        }
    }
}

/// Write one message.
///
/// # Errors
///
/// Propagates any I/O failure.
pub fn write(output: &mut impl Write, framing: Framing, message: &Outgoing) -> std::io::Result<()> {
    let body = serde_json::to_string(message).unwrap_or_else(|_| {
        // Serializing our own reply cannot fail on any value this crate builds, and a panic
        // here would take down an editor session over a formatting problem.
        String::from(r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"unserializable"}}"#)
    });

    match framing {
        Framing::Lines => writeln!(output, "{body}")?,
        Framing::Headers => write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?,
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &str, framing: Framing) -> Vec<String> {
        let mut cursor = std::io::BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        while let Ok(Some(message)) = read(&mut cursor, framing) {
            out.push(message);
        }
        out
    }

    #[test]
    fn reads_a_header_framed_message() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let wire = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        assert_eq!(read_all(&wire, Framing::Headers), [body]);
    }

    #[test]
    fn reads_several_header_framed_messages() {
        let a = r#"{"id":1}"#;
        let b = r#"{"id":2}"#;
        let wire = format!(
            "Content-Length: {}\r\n\r\n{a}Content-Length: {}\r\n\r\n{b}",
            a.len(),
            b.len()
        );
        assert_eq!(read_all(&wire, Framing::Headers), [a, b]);
    }

    #[test]
    fn the_header_name_is_case_insensitive() {
        // Not every client spells it the way the specification's examples do, and one that
        // sends `content-length` has not sent a bad message.
        let body = r#"{"id":1}"#;
        let wire = format!("content-length: {}\r\n\r\n{body}", body.len());
        assert_eq!(read_all(&wire, Framing::Headers), [body]);
    }

    #[test]
    fn other_headers_are_ignored() {
        let body = r#"{"id":1}"#;
        let wire = format!(
            "Content-Type: application/vscode-jsonrpc\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(read_all(&wire, Framing::Headers), [body]);
    }

    #[test]
    fn a_body_with_no_length_ends_the_stream() {
        // There is no way to know where the body ends, so nothing after it can be trusted.
        assert!(read_all("Content-Type: x\r\n\r\n{}", Framing::Headers).is_empty());
    }

    #[test]
    fn reads_line_framed_messages() {
        let wire = "{\"id\":1}\n{\"id\":2}\n";
        assert_eq!(
            read_all(wire, Framing::Lines),
            [r#"{"id":1}"#, r#"{"id":2}"#]
        );
    }

    #[test]
    fn blank_lines_between_messages_are_skipped() {
        let wire = "{\"id\":1}\n\n\n{\"id\":2}\n";
        assert_eq!(
            read_all(wire, Framing::Lines),
            [r#"{"id":1}"#, r#"{"id":2}"#]
        );
    }

    #[test]
    fn empty_input_reads_nothing() {
        assert!(read_all("", Framing::Headers).is_empty());
        assert!(read_all("", Framing::Lines).is_empty());
    }

    #[test]
    fn a_notification_expects_no_reply() {
        let notification: Incoming =
            serde_json::from_str(r#"{"method":"initialized","params":{}}"#).expect("parses");
        assert!(!notification.expects_reply());

        let request: Incoming =
            serde_json::from_str(r#"{"id":1,"method":"initialize"}"#).expect("parses");
        assert!(request.expects_reply());
    }

    #[test]
    fn params_default_to_null_when_absent() {
        // `shutdown` carries none, and a missing field must not fail the parse.
        let message: Incoming =
            serde_json::from_str(r#"{"id":1,"method":"shutdown"}"#).expect("parses");
        assert!(message.params.is_null());
    }

    #[test]
    fn a_written_message_round_trips_through_the_reader() {
        for framing in [Framing::Headers, Framing::Lines] {
            let mut buffer = Vec::new();
            write(
                &mut buffer,
                framing,
                &Outgoing::result(Some(Value::from(7)), serde_json::json!({"ok": true})),
            )
            .expect("writes");

            let text = String::from_utf8(buffer).expect("utf-8");
            let read_back = read_all(&text, framing);
            assert_eq!(read_back.len(), 1, "{framing:?}");
            let parsed: Value = serde_json::from_str(&read_back[0]).expect("parses");
            assert_eq!(parsed["id"], 7, "{framing:?}");
            assert_eq!(parsed["result"]["ok"], true, "{framing:?}");
            assert_eq!(parsed["jsonrpc"], "2.0", "{framing:?}");
        }
    }

    #[test]
    fn a_header_framed_write_states_the_byte_length_not_the_character_count() {
        // A multi-byte character makes the two differ, and a client reading N bytes when the
        // header said N characters desynchronizes the stream for good.
        let mut buffer = Vec::new();
        write(
            &mut buffer,
            Framing::Headers,
            &Outgoing::result(None, serde_json::json!({"m": "café — ✓"})),
        )
        .expect("writes");

        let text = String::from_utf8(buffer).expect("utf-8");
        let (header, body) = text.split_once("\r\n\r\n").expect("framed");
        let declared: usize = header
            .trim_start_matches("Content-Length:")
            .trim()
            .parse()
            .expect("a number");
        assert_eq!(declared, body.len());
        assert_ne!(
            declared,
            body.chars().count(),
            "the test needs a multi-byte body"
        );
    }

    #[test]
    fn an_error_reply_carries_a_code_and_no_result() {
        let message = Outgoing::error(Some(Value::from(1)), codes::METHOD_NOT_FOUND, "nope");
        let rendered = serde_json::to_value(&message).expect("serializes");
        assert_eq!(rendered["error"]["code"], codes::METHOD_NOT_FOUND);
        assert!(rendered.get("result").is_none());
    }

    #[test]
    fn a_notification_carries_a_method_and_no_id() {
        let message = Outgoing::notification("textDocument/publishDiagnostics", Value::Null);
        let rendered = serde_json::to_value(&message).expect("serializes");
        assert_eq!(rendered["method"], "textDocument/publishDiagnostics");
        assert!(rendered.get("id").is_none());
    }
}
