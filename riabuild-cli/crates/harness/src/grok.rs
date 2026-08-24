//! Grok Build's `--output-format streaming-json`, which its own help describes
//! as "NDJSON of the agent native ACP session updates".
//!
//! That sentence is the whole design note. Grok Build is the only one of the
//! three that speaks the Agent Client Protocol natively, so its headless stream
//! is not a format xAI invented — it is ACP's `session/update` payloads, one per
//! line. Decoding it therefore means decoding ACP, and the arms below are named
//! after ACP's own `sessionUpdate` discriminants rather than after anything
//! Grok-specific.
//!
//! `grok agent stdio` is the fuller version of the same protocol — bidirectional
//! ACP over stdio, one process for the session — and is not used yet for the
//! reason `codex app-server` is not: it is beta, and two independent integrators
//! report gaps in resume and in managed MCP injection. When it stabilises it
//! replaces this module's transport and keeps its decoder, which is the point of
//! having written the decoder against ACP's names.
//!
//! Read out of Grok Build 1.0.5. Only the **error** frame is captured from that
//! binary — see `tests::UNAUTHENTICATED`, which is a real transcript from a
//! machine with no xAI sign-in. Everything else is written from the ACP
//! specification and is marked `INFERRED`. Because Grok wraps its updates in a
//! way this crate could not observe, the decoder deliberately accepts **both**
//! the bare update object and one nested under a JSON-RPC `params.update`, and
//! finds its session id at either level. That tolerance is not defensive
//! programming for its own sake: it is the difference between a session that
//! degrades to plain text and one that shows nothing at all.

use serde_json::Value;

use super::{Decoder, Encoder, Event, Kind, Launch};

pub(super) struct Grok;

impl Encoder for Grok {
    fn argv(&self, launch: &Launch, thread: Option<&str>, prompt: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        // Root options only, and there is no subcommand here — but the order is
        // kept anyway, because `--always-approve` after a subcommand is
        // `unexpected argument` and this argv is the template anything later
        // will copy.
        args.extend(Kind::Grok.bypass().iter().map(|flag| (*flag).to_string()));
        args.push("--output-format".into());
        args.push("streaming-json".into());
        // Grok Build updates itself in place unless told not to. A provisioner
        // that owns the binary — riabuild downloads it from its own mirror and
        // verifies a pinned digest — must never let it replace itself, or the
        // digest describes bytes that are no longer on disk.
        args.push("--no-auto-update".into());
        if let Some(thread) = thread {
            args.push("--resume".into());
            args.push(thread.to_string());
        }
        if let Some(prompt) = prompt {
            args.push("-p".into());
            args.push(prompt.to_string());
        }
        let _ = launch;
        args
    }
}

#[derive(Default)]
pub(super) struct Reader {
    thread: Option<String>,
}

impl Decoder for Reader {
    fn read(&mut self, line: &str) -> Vec<Event> {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };

        // VERIFIED against 1.0.5: an unauthenticated run writes exactly this.
        if frame.get("type").and_then(Value::as_str) == Some("error") {
            return vec![Event::Trouble(
                frame
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("grok reported an error")
                    .to_string(),
            )];
        }

        // An ACP update may arrive bare or wrapped in a JSON-RPC envelope. Both
        // are accepted because which one Grok writes could not be observed here.
        let params = frame.get("params");
        let update = params
            .and_then(|params| params.get("update"))
            .or_else(|| frame.get("update"))
            .unwrap_or(&frame);

        let mut events = Vec::new();
        // Announced once, not on every chunk: every ACP update carries the
        // session id, and re-emitting `Ready` would reset the pane header on
        // every token.
        if let Some(id) = session_id(&frame).or_else(|| params.and_then(session_id))
            && self.thread.as_deref() != Some(id.as_str())
        {
            self.thread = Some(id.clone());
            events.push(Event::Ready {
                thread: Some(id),
                model: frame
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        events.extend(self.update(update));
        events
    }
}

impl Reader {
    /// One ACP `session/update`. INFERRED throughout.
    fn update(&mut self, update: &Value) -> Vec<Event> {
        let discriminant = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match discriminant {
            // Chunks, not whole messages: ACP streams prose. They are emitted
            // as they arrive and the pane concatenates, because a chunk held
            // back until a message completed would leave the session looking
            // idle for the whole of a long answer.
            "agent_message_chunk" => text_of(update)
                .map(|text| vec![Event::Said(text)])
                .unwrap_or_default(),
            "agent_thought_chunk" => text_of(update)
                .map(|text| vec![Event::Thought(text)])
                .unwrap_or_default(),
            // The developer's own words, echoed back. Already on screen.
            "user_message_chunk" => Vec::new(),
            "tool_call" => {
                let id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                vec![Event::ToolStarted {
                    id,
                    name: update
                        .get("kind")
                        .or_else(|| update.get("title"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    detail: update
                        .get("title")
                        .and_then(Value::as_str)
                        .map(super::claude::one_line),
                }]
            }
            "tool_call_update" => {
                let id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // ACP's statuses are `pending`, `in_progress`, `completed` and
                // `failed`. Only the last two end a call; an update that merely
                // reports progress must not be read as a finish, or every tool
                // call appears to succeed the moment it starts.
                match update.get("status").and_then(Value::as_str) {
                    Some("completed") => vec![Event::ToolFinished { id, ok: true }],
                    Some("failed") => vec![Event::ToolFinished { id, ok: false }],
                    _ => Vec::new(),
                }
            }
            // A plan, and the command list a client would render as a menu.
            // Neither is something this TUI shows.
            "plan" | "available_commands_update" | "current_mode_update" => Vec::new(),
            _ => Vec::new(),
        }
    }
}

/// ACP spells it `sessionId`; a `-p` run may spell it `session_id`. Both are
/// accepted rather than one being guessed at.
fn session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// ACP content blocks are `{"type":"text","text":"…"}`, and a chunk carries one.
fn text_of(update: &Value) -> Option<String> {
    let content = update.get("content")?;
    // A single block, or an array of them.
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return (!text.is_empty()).then(|| text.to_string());
    }
    let joined: String = content
        .as_array()?
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from Grok Build 1.0.5 by running
    /// `grok -p "say only the word ok" --output-format streaming-json
    /// --always-approve` on a machine with no xAI sign-in. stdout verbatim.
    const UNAUTHENTICATED: &str = r#"{"type":"error","message":"Not signed in. To authenticate without a browser, run:\n  grok login --device-code"}"#;

    fn decode(transcript: &str) -> Vec<Event> {
        let mut reader = Reader::default();
        transcript
            .lines()
            .flat_map(|line| reader.read(line))
            .collect()
    }

    #[test]
    fn a_real_unauthenticated_transcript_says_what_is_wrong() {
        let events = decode(UNAUTHENTICATED);
        assert_eq!(events.len(), 1);
        let Some(Event::Trouble(text)) = events.first() else {
            panic!("expected trouble, got {events:?}");
        };
        assert!(text.starts_with("Not signed in."), "{text}");
        // The remedy is in the message and must survive intact — it is the only
        // thing that tells a developer how to fix this.
        assert!(text.contains("grok login --device-code"), "{text}");
    }

    #[test]
    fn an_update_is_read_bare_or_wrapped_in_a_json_rpc_envelope() {
        // Which of these Grok writes could not be observed on a machine with no
        // sign-in, so both are accepted. If that is ever pinned down, the loser
        // can be deleted — but not before.
        let bare = r#"{"sessionId":"s1","sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}"#;
        let wrapped = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#;

        let expected = vec![
            Event::Ready {
                thread: Some("s1".into()),
                model: None,
            },
            Event::Said("hi".into()),
        ];
        assert_eq!(decode(bare), expected);
        assert_eq!(decode(wrapped), expected);
    }

    #[test]
    fn the_session_is_announced_once_and_not_on_every_chunk() {
        // Every ACP update carries the session id. Emitting `Ready` each time
        // would reset the pane's header on every token.
        let stream = r#"{"sessionId":"s1","sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"a"}}
{"sessionId":"s1","sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"b"}}"#;
        let events = decode(stream);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Ready { .. }))
                .count(),
            1
        );
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn a_tool_call_only_finishes_when_acp_says_it_finished() {
        // `in_progress` is an update, not a completion. Reading it as one makes
        // every tool call appear to succeed the instant it starts.
        let stream = r#"{"sessionUpdate":"tool_call","toolCallId":"t1","kind":"execute","title":"ls -la"}
{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress"}
{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"failed"}"#;
        assert_eq!(
            decode(stream),
            vec![
                Event::ToolStarted {
                    id: "t1".into(),
                    name: "execute".into(),
                    detail: Some("ls -la".into()),
                },
                Event::ToolFinished {
                    id: "t1".into(),
                    ok: false
                },
            ]
        );
    }

    #[test]
    fn content_arrives_as_one_block_or_as_a_list_of_them() {
        let list = r#"{"sessionUpdate":"agent_message_chunk","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#;
        assert_eq!(decode(list), vec![Event::Said("ab".into())]);
    }

    #[test]
    fn the_echo_of_the_developers_own_prompt_is_not_shown_twice() {
        let echo =
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"}}"#;
        assert_eq!(decode(echo), Vec::new());
    }

    #[test]
    fn the_launch_bypasses_approvals_and_refuses_to_self_update() {
        let launch = Launch {
            kind: Kind::Grok,
            program: "/opt/grok".into(),
            cwd: "/work".into(),
            prompt: None,
        };
        let args = Grok.argv(&launch, None, Some("hello"));
        assert!(args.iter().any(|a| a == "--always-approve"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--output-format", "streaming-json"])
        );
        // riabuild verified a pinned digest for the binary on disk. A harness
        // that replaces itself makes that digest describe bytes that are gone.
        assert!(args.iter().any(|a| a == "--no-auto-update"));
        assert!(args.windows(2).any(|w| w == ["-p", "hello"]));

        let resumed = Grok.argv(&launch, Some("s1"), Some("again"));
        assert!(resumed.windows(2).any(|w| w == ["--resume", "s1"]));
    }

    #[test]
    fn an_unknown_update_loses_detail_and_nothing_else() {
        assert_eq!(decode(r#"{"sessionUpdate":"something_new"}"#), Vec::new());
        assert_eq!(decode("not json"), Vec::new());
    }
}
