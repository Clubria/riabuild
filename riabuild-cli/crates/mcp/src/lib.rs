//! Codex, offered to Claude Code as a subagent.
//!
//! `riabuild internal mcp-codex` is a stdio MCP server that Claude Code starts
//! for itself, one per session, and talks JSON-RPC 2.0 to over a pipe. It
//! exposes two tools — `codex` and `codex_reply` — and what they do is open a
//! Codex session in `riabuild agents`' own store, run one turn in it, and hand
//! back **the last thing Codex said** and nothing else.
//!
//! # Why riabuild's own server and not `codex mcp-server`
//!
//! Codex ships one. Its `codex` tool streams the session back, which is the
//! whole problem: a subagent is worth having because its working — the file
//! reads, the shell commands, the reasoning — stays out of the context of the
//! agent that asked. A tool that returns the transcript is a delegation that
//! costs more context than doing the work directly.
//!
//! The filter has to live somewhere, and riabuild already owns every piece it
//! needs: `riabuild-harness` decodes Codex's NDJSON, `riabuild-agents` runs a
//! turn and writes its spool. So the discarded transcript is not discarded at
//! all — it goes to a real session in the store, and the developer watches the
//! whole thing in `riabuild agents` while the calling agent holds one paragraph.
//! `docs/superpowers/specs/2026-08-24-riabuild-agents-design.md` predicted this
//! exact mechanism under "Cross-provider delegation"; this is it.
//!
//! # Which session asked
//!
//! MCP has no field for it. `initialize` carries the client's name and version
//! and says nothing about which conversation it is serving, so a server cannot
//! ask. What it can do is *read its own environment*: Claude Code passes its
//! whole environment to the stdio servers it spawns — verified against 2.1.260
//! — and `riabuild_agents::turn` sets [`riabuild_agents::DELEGATING_SESSION`] on
//! every turn it runs. So a Claude session riabuild started names itself, and
//! the Codex session opened here is recorded as its child.
//!
//! A Claude Code that riabuild did **not** start — `~/.riabuild/bin/claude` in a
//! terminal — sets no such variable, and delegating from one is a session with
//! no parent rather than a failure.
//!
//! # One request at a time
//!
//! The loop reads a line, answers it, and reads the next. A second `tools/call`
//! arriving while a turn is running waits for it, which is a real limit and a
//! deliberate one: turns are serialised per session by the store's lock anyway,
//! and a concurrent reader would need this crate to own a task per request, a
//! shared writer, and an interleaving nobody can reproduce from a bug report.
//!
//! # stdout is the wire
//!
//! Nothing here may print. `riabuild-ui` writes with `println!`, and one line of
//! it on this process's stdout is a parse error in Claude Code with no
//! indication of where it came from. Diagnostics go to stderr, which Claude Code
//! collects into its own MCP log.

// `unwrap_used`, `panic` and `expect_used` are denied workspace-wide. In a test
// a panic *is* how a failed precondition is reported, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, for the reason
// `riabuild-theme` writes out in full.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

mod delegate;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use delegate::{Delegate, Reply};

/// The MCP revision this server answers with when a client names none.
///
/// A client that *does* name one gets its own back, which is what the
/// specification asks for and what keeps this working across a Claude Code that
/// moves faster than riabuild releases.
const PROTOCOL: &str = "2025-06-18";

/// Serves one Claude Code session until its stdin closes.
///
/// Returns `Ok` on EOF, which is what a client disconnecting looks like and is
/// not a failure: Claude Code closes the pipe when the session ends.
pub async fn serve(runner: &dyn riabuild_runner::CommandRunner, delegate: Delegate) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            // A line that is not JSON has no id to answer against, so there is
            // nothing to reply to. Said on stderr and skipped, rather than
            // ending a session over one bad frame.
            eprintln!("riabuild mcp: ignoring a line that is not JSON");
            continue;
        };

        let Some(answer) = respond(runner, &delegate, &request).await else {
            // A notification. The specification forbids answering one, and
            // `notifications/initialized` is the one Claude Code always sends.
            continue;
        };
        let mut text = serde_json::to_string(&answer)?;
        text.push('\n');
        out.write_all(text.as_bytes()).await?;
        out.flush().await?;
    }
    Ok(())
}

/// One request, answered — or `None` where the frame was a notification.
async fn respond(
    runner: &dyn riabuild_runner::CommandRunner,
    delegate: &Delegate,
    request: &Value,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    // A frame with no id is a notification, whatever its method. Answering one
    // is a protocol violation, and the client is entitled to close the pipe.
    id.as_ref()?;

    let result = match method {
        "initialize" => initialize(request),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => call(runner, delegate, request).await,
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no method {method}") }
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// The handshake.
///
/// The client's own protocol revision is echoed where it sent one. Answering
/// with a fixed string instead would make every Claude Code release that moves
/// the revision a version negotiation this server loses.
fn initialize(request: &Value) -> Value {
    let protocol = request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL);
    json!({
        "protocolVersion": protocol,
        // Tools and nothing else. This server has no resources, no prompts and
        // no sampling, and claiming a capability it does not implement is how a
        // client comes to call something that is not there.
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "codex", "version": riabuild_version::VERSION },
    })
}

/// What this server offers.
///
/// The descriptions are the interface: they are what the calling model reads
/// when it decides whether to delegate, so they say what delegation *costs* and
/// what comes back, not merely what the tool is called.
fn tools() -> Value {
    json!([
        {
            "name": "codex",
            "description": "Delegate a self-contained task to Codex, OpenAI's coding agent, \
                running as a subagent in this checkout. Codex works in its own context — it \
                reads files, runs commands and edits code — and only its final answer comes \
                back to you. Its reasoning, tool calls and file reads never enter your context, \
                so this is cheap for work whose conclusion matters more than its steps. It runs \
                with approvals bypassed, like every other agent riabuild starts, and edits real \
                files. The full transcript is visible to the developer in `riabuild agents`. \
                Returns the answer and a session id to continue with `codex_reply`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "What Codex should do. Write it as a complete brief: \
                            the subagent shares no context with you and cannot ask you \
                            anything once it has started."
                    }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }
        },
        {
            "name": "codex_reply",
            "description": "Ask an existing Codex session something else. It keeps everything \
                from its earlier turns, so this is the cheap way to follow up — you do not have \
                to restate the brief. Takes the session id an earlier `codex` call returned. \
                Only the final answer comes back, as before.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {
                        "type": "string",
                        "description": "The session id returned by an earlier `codex` call."
                    },
                    "prompt": { "type": "string", "description": "The follow-up." }
                },
                "required": ["session", "prompt"],
                "additionalProperties": false
            }
        }
    ])
}

/// `tools/call`, which is the whole of the work.
///
/// A refusal comes back as a tool result with `isError` rather than as a
/// JSON-RPC error, which is the difference between the calling model *reading*
/// why it could not delegate and seeing an opaque failure it will retry. A
/// protocol error is for a frame the server could not understand; not being
/// signed in to Codex is an answer.
async fn call(
    runner: &dyn riabuild_runner::CommandRunner,
    delegate: &Delegate,
    request: &Value,
) -> Value {
    let params = request.get("params");
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params.and_then(|params| params.get("arguments"));
    let argument = |key: &str| {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    let Some(prompt) = argument("prompt").filter(|text| !text.trim().is_empty()) else {
        return failed("a prompt is required, and an empty one is not a task");
    };

    let outcome = match name {
        "codex" => delegate.start(runner, &prompt).await,
        "codex_reply" => match argument("session") {
            Some(session) => delegate.resume(runner, &session, &prompt).await,
            None => return failed("`session` is required — pass the id an earlier call returned"),
        },
        other => return failed(&format!("no tool {other}")),
    };

    match outcome {
        // `{:#}` rather than `{}`: the chain is where the reason is, and a
        // caller told only "could not open a Codex session" has been told
        // nothing it can act on.
        Err(error) => failed(&format!("{error:#}")),
        Ok(reply) => {
            let trailer = trailer(&reply);
            match &reply.said {
                Some(said) => text(&format!("{said}\n\n{trailer}")),
                // A turn that exited without saying anything. The trouble lines
                // are the only account of it there is, and an empty *success*
                // would have the calling agent report that Codex agreed with it.
                None => failed(&format!(
                    "Codex ended the turn without answering.{}\n\n{trailer}",
                    if reply.trouble.is_empty() {
                        String::new()
                    } else {
                        format!(" It reported: {}", reply.trouble.join("; "))
                    },
                )),
            }
        }
    }
}

/// The one line of bookkeeping that rides back with an answer.
///
/// Short on purpose. Everything on it is something the calling agent may need —
/// the id to follow up with, and what the delegation cost — and everything else
/// about the turn is in the window and on the spool.
fn trailer(reply: &delegate::Reply) -> String {
    format!(
        "— codex session `{}` · {} in / {} out tokens · pass that id to `codex_reply` to \
         continue, or open `riabuild agents` to read the whole transcript",
        reply.session, reply.input, reply.output
    )
}

fn text(body: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": body }], "isError": false })
}

fn failed(body: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": body }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(method: &str, id: i64) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method })
    }

    #[test]
    fn the_handshake_echoes_the_clients_revision() {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        });
        let answer = initialize(&request);
        assert_eq!(answer["protocolVersion"], "2024-11-05");
        assert_eq!(answer["serverInfo"]["name"], "codex");
        assert!(answer["capabilities"]["tools"].is_object());
    }

    #[test]
    fn a_client_that_names_no_revision_gets_ours() {
        assert_eq!(
            initialize(&ask("initialize", 1))["protocolVersion"],
            PROTOCOL
        );
    }

    #[test]
    fn both_tools_are_offered_with_a_schema() {
        let tools = tools();
        let names: Vec<&str> = tools
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|tool| tool["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(names, vec!["codex", "codex_reply"]);
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"]["prompt"].is_object());
        }
    }

    #[test]
    fn a_refusal_is_a_tool_error_and_not_a_protocol_error() {
        let answer = failed("nope");
        assert_eq!(answer["isError"], true);
        assert_eq!(answer["content"][0]["text"], "nope");
    }

    #[test]
    fn the_trailer_carries_the_session_and_the_cost() {
        let reply = delegate::Reply {
            session: "abc".into(),
            said: None,
            input: 12,
            output: 3,
            trouble: Vec::new(),
        };
        let line = trailer(&reply);
        assert!(line.contains("abc"), "{line}");
        assert!(line.contains("12 in / 3 out"), "{line}");
    }
}
