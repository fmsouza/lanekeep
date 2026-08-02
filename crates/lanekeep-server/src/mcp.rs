//! The Model Context Protocol surface.
//!
//! Three tools, one per thing the CLI already does: run the checks, list the configured
//! rules, explain one. An agent host launches `lanekeep server --protocol mcp` and calls
//! them; the transport is the same JSON-RPC this crate already speaks, line-delimited
//! instead of header-delimited.
//!
//! # What the tools return
//!
//! The `agent` reporter's text, unchanged. That format exists precisely for this consumer —
//! it groups by rule, states the remediation once rather than per occurrence, and shows a
//! good and bad example — and re-rendering violations into a bespoke JSON shape here would
//! be a second answer to a question §11 already answered.
//!
//! # A failing tool is not a failing call
//!
//! MCP separates the two, and the distinction is load-bearing. A rule that throws, or a
//! config that will not load, is a *result* the model should see and act on — it comes back
//! as `isError: true` with the message as content. A JSON-RPC error is for the host, not the
//! model: no such tool, malformed arguments. Reporting a rule failure as a JSON-RPC error
//! hides it from the thing best placed to fix it.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::jsonrpc::{self, Framing, Incoming, Outgoing, codes};

/// The protocol revision this server implements.
///
/// A client asking for a different one is answered with this rather than refused: the
/// specification has the server state what it speaks, and a host that cannot work with it
/// will say so. Refusing outright would turn a version skew into a dead session.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// What the host can ask lanekeep to do.
///
/// A trait rather than an engine, for the same reason the LSP loop takes a closure: this
/// crate stays protocol-only, and its tests need no project on disk.
pub trait Tools {
    /// Run the project's rules and describe what they found.
    ///
    /// # Errors
    ///
    /// Returns the message to show the model when the run could not happen at all.
    fn check(&mut self) -> Result<String, String>;

    /// List the rules the project has configured.
    ///
    /// # Errors
    ///
    /// As [`Tools::check`].
    fn rules(&mut self) -> Result<String, String>;

    /// Explain one rule: what it checks and what to do about it.
    ///
    /// # Errors
    ///
    /// As [`Tools::check`], including when no such rule is configured.
    fn explain(&mut self, rule: &str) -> Result<String, String>;
}

/// The tool catalogue, as `tools/list` returns it.
#[must_use]
pub fn catalogue() -> Value {
    json!({
        "tools": [
            {
                "name": "lanekeep_check",
                "description": "Check the project against its architectural rules. \
                                Returns every violation, grouped by rule, with the \
                                remediation for each and a good and bad example. Run this \
                                after editing code to find conventions the change broke.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "lanekeep_rules",
                "description": "List the rules this project has configured, with what each \
                                one enforces. Use it to find out which conventions apply \
                                here before writing code, rather than after.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "lanekeep_explain",
                "description": "Explain one rule: what it checks, why, and what to do \
                                instead, with a good and bad example. Call it with the id \
                                from a violation to find out how to fix it.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "rule": {
                            "type": "string",
                            "description": "Namespaced rule id, as it appears in a \
                                            violation — for example `lanekeep/no-default-export`.",
                        },
                    },
                    "required": ["rule"],
                },
            },
        ],
    })
}

/// A tool result, successful or not.
///
/// Both are a *successful* JSON-RPC reply; `isError` is what tells the model which it got.
#[must_use]
pub fn content(text: &str, failed: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": failed,
    })
}

/// Serve MCP against `input` and `output` until the host disconnects.
///
/// # Errors
///
/// Propagates an I/O failure on the transport. A failing tool is not one — see the module
/// docs.
pub fn serve(
    input: &mut impl BufRead,
    output: &mut impl Write,
    tools: &mut impl Tools,
) -> std::io::Result<()> {
    while let Some(raw) = jsonrpc::read(input, Framing::Lines)? {
        let Ok(message) = serde_json::from_str::<Incoming>(&raw) else {
            jsonrpc::write(
                output,
                Framing::Lines,
                &Outgoing::error(None, codes::PARSE_ERROR, "not a JSON-RPC message"),
            )?;
            continue;
        };

        let outcome = match message.method.as_str() {
            "initialize" => Some(Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "lanekeep",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }))),

            // Acknowledgements and keepalives.
            "notifications/initialized" | "initialized" => None,
            "ping" => Some(Ok(json!({}))),

            "tools/list" => Some(Ok(catalogue())),

            "tools/call" => Some(call(&message.params, tools)),

            other => Some(Err((
                codes::METHOD_NOT_FOUND,
                format!("no method `{other}`"),
            ))),
        };

        let Some(outcome) = outcome else { continue };
        if !message.expects_reply() {
            continue;
        }

        let response = match outcome {
            Ok(result) => Outgoing::result(message.id.clone(), result),
            Err((code, text)) => Outgoing::error(message.id.clone(), code, text),
        };
        jsonrpc::write(output, Framing::Lines, &response)?;
    }

    Ok(())
}

/// Dispatch one `tools/call`.
fn call(params: &Value, tools: &mut impl Tools) -> Result<Value, (i32, String)> {
    let Some(name) = params["name"].as_str() else {
        return Err((codes::INVALID_PARAMS, "`name` is required".to_owned()));
    };

    let outcome = match name {
        "lanekeep_check" => tools.check(),
        "lanekeep_rules" => tools.rules(),
        "lanekeep_explain" => {
            // A missing argument is the host's mistake, not something the model should be
            // shown as a rule failure — so it is a JSON-RPC error rather than `isError`.
            let Some(rule) = params["arguments"]["rule"].as_str() else {
                return Err((
                    codes::INVALID_PARAMS,
                    "`lanekeep_explain` needs a `rule` argument".to_owned(),
                ));
            };
            tools.explain(rule)
        }
        other => {
            return Err((codes::INVALID_PARAMS, format!("no tool `{other}`")));
        }
    };

    Ok(match outcome {
        Ok(text) => content(&text, false),
        Err(text) => content(&text, true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in that records what it was asked and answers as told.
    struct Fake {
        answer: Result<String, String>,
        explained: Option<String>,
        called: Vec<&'static str>,
    }

    impl Fake {
        fn ok() -> Self {
            Self {
                answer: Ok("nothing found".to_owned()),
                explained: None,
                called: Vec::new(),
            }
        }

        fn failing() -> Self {
            Self {
                answer: Err("rule threw".to_owned()),
                explained: None,
                called: Vec::new(),
            }
        }
    }

    impl Tools for Fake {
        fn check(&mut self) -> Result<String, String> {
            self.called.push("check");
            self.answer.clone()
        }

        fn rules(&mut self) -> Result<String, String> {
            self.called.push("rules");
            self.answer.clone()
        }

        fn explain(&mut self, rule: &str) -> Result<String, String> {
            self.called.push("explain");
            self.explained = Some(rule.to_owned());
            self.answer.clone()
        }
    }

    fn exchange(messages: &[Value], tools: &mut impl Tools) -> Vec<Value> {
        use std::fmt::Write as _;

        let mut wire = String::new();
        for message in messages {
            let _ = writeln!(wire, "{message}");
        }
        let mut input = std::io::BufReader::new(wire.as_bytes());
        let mut output = Vec::new();
        serve(&mut input, &mut output, tools).expect("serves");

        String::from_utf8(output)
            .expect("utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parses"))
            .collect()
    }

    #[test]
    fn initialize_states_the_protocol_version_and_the_tools_capability() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })],
            &mut Fake::ok(),
        );
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "lanekeep");
    }

    #[test]
    fn the_catalogue_lists_three_tools_each_with_a_schema() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
            &mut Fake::ok(),
        );
        let tools = replies[0]["result"]["tools"].as_array().expect("an array");
        assert_eq!(tools.len(), 3);

        for tool in tools {
            assert!(tool["name"].is_string(), "{tool}");
            // The description is what a model reads to decide whether to call it, so an
            // empty one makes the tool invisible in practice.
            let description = tool["description"].as_str().expect("a description");
            assert!(
                description.len() > 40,
                "too terse to choose by: {description}"
            );
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        }
    }

    #[test]
    fn explain_declares_its_required_argument() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
            &mut Fake::ok(),
        );
        let explain = replies[0]["result"]["tools"]
            .as_array()
            .expect("an array")
            .iter()
            .find(|tool| tool["name"] == "lanekeep_explain")
            .expect("present");
        assert_eq!(explain["inputSchema"]["required"][0], "rule");
    }

    #[test]
    fn calling_check_returns_its_text_as_content() {
        let mut tools = Fake::ok();
        let replies = exchange(
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "lanekeep_check", "arguments": {} }
            })],
            &mut tools,
        );
        assert_eq!(tools.called, ["check"]);
        assert_eq!(replies[0]["result"]["content"][0]["type"], "text");
        assert_eq!(replies[0]["result"]["content"][0]["text"], "nothing found");
        assert_eq!(replies[0]["result"]["isError"], false);
    }

    #[test]
    fn explain_receives_the_rule_it_was_given() {
        let mut tools = Fake::ok();
        exchange(
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "name": "lanekeep_explain",
                    "arguments": { "rule": "lanekeep/no-default-export" }
                }
            })],
            &mut tools,
        );
        assert_eq!(
            tools.explained.as_deref(),
            Some("lanekeep/no-default-export")
        );
    }

    #[test]
    fn a_failing_tool_is_a_successful_call_marked_as_an_error() {
        // The distinction that matters: a rule that threw is a result the model should see
        // and act on. Reporting it as a JSON-RPC error hides it from the thing best placed
        // to fix it.
        let replies = exchange(
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "lanekeep_check", "arguments": {} }
            })],
            &mut Fake::failing(),
        );
        assert!(
            replies[0].get("error").is_none(),
            "should not be a protocol error: {}",
            replies[0]
        );
        assert_eq!(replies[0]["result"]["isError"], true);
        assert_eq!(replies[0]["result"]["content"][0]["text"], "rule threw");
    }

    #[test]
    fn a_missing_argument_is_a_protocol_error_not_a_tool_error() {
        // The host built the call wrong. That is not something the model can fix by writing
        // different code, so it goes back as a JSON-RPC error.
        let replies = exchange(
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "lanekeep_explain", "arguments": {} }
            })],
            &mut Fake::ok(),
        );
        assert_eq!(replies[0]["error"]["code"], codes::INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_tool_is_refused() {
        let replies = exchange(
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "lanekeep_deploy", "arguments": {} }
            })],
            &mut Fake::ok(),
        );
        assert_eq!(replies[0]["error"]["code"], codes::INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_method_is_refused() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" })],
            &mut Fake::ok(),
        );
        assert_eq!(replies[0]["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn the_initialized_notification_is_not_answered() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })],
            &mut Fake::ok(),
        );
        assert!(replies.is_empty(), "{replies:?}");
    }

    #[test]
    fn ping_is_answered() {
        let replies = exchange(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })],
            &mut Fake::ok(),
        );
        assert_eq!(replies.len(), 1);
        assert!(replies[0]["result"].is_object());
    }

    #[test]
    fn a_malformed_line_is_answered_and_the_session_continues() {
        let wire = "not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut input = std::io::BufReader::new(wire.as_bytes());
        let mut output = Vec::new();
        serve(&mut input, &mut output, &mut Fake::ok()).expect("serves");

        let replies: Vec<Value> = String::from_utf8(output)
            .expect("utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parses"))
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["error"]["code"], codes::PARSE_ERROR);
        assert_eq!(replies[1]["id"], 1);
    }

    #[test]
    fn every_reply_is_one_line() {
        // Line-delimited framing: a reply containing a raw newline would be read as two
        // messages, and every message after it would be off by one.
        let mut tools = Fake::ok();
        tools.answer = Ok("two\nlines".to_owned());
        let wire = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "lanekeep_check", "arguments": {} }
        })
        .to_string()
            + "\n";

        let mut input = std::io::BufReader::new(wire.as_bytes());
        let mut output = Vec::new();
        serve(&mut input, &mut output, &mut tools).expect("serves");

        let text = String::from_utf8(output).expect("utf-8");
        assert_eq!(text.trim_end().lines().count(), 1, "{text}");
        let parsed: Value = serde_json::from_str(text.trim_end()).expect("parses");
        assert_eq!(parsed["result"]["content"][0]["text"], "two\nlines");
    }
}
