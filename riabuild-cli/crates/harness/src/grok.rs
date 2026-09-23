//! Grok Build's `--output-format streaming-json`.
//!
//! Its `--help` calls this "NDJSON of the agent native ACP session updates",
//! and this module was first written from that sentence — decoding ACP's
//! `sessionUpdate` discriminants. That was wrong in the way that matters: the
//! user guide Grok Build 1.0.5 installs beside itself
//! (`docs/user-guide/14-headless-mode.md`) says the stream is *derived from*
//! ACP, not ACP. Every line is one object tagged by `type` — `text`, `thought`,
//! `tool_call`, `tool_call_update`, `usage`, `plan`, `available_commands`,
//! `end`, `error` — and only the *leaf* field names (`toolCallId`, `kind`,
//! `rawInput`, `rawOutput`) are ACP's. A decoder switching on `sessionUpdate`
//! found none of it, so a real Grok turn showed nothing but its errors.
//!
//! Two consequences of that shape drive the code below:
//!
//! - **The session id arrives last.** Only `end` carries `sessionId`, so
//!   [`Event::Ready`] is emitted at the end of the first turn rather than the
//!   start. Nothing needs it sooner: a record needs its thread to *resume*, and
//!   the next turn cannot start until this one has ended.
//! - **`usage` is per model response, not per turn.** A turn that calls tools
//!   makes several responses, each reporting its own counts, so they are summed
//!   here into the cumulative-for-the-turn [`Event::Usage`] the rest of the
//!   crate expects. `end` then carries the prompt's own total — including
//!   subagents that finished in time — and has the last word.
//!
//! The ACP reading is kept as a fallback for any line with no `type`, because
//! `grok agent stdio` really is ACP and a future `--output-format` could be
//! too; it costs a match arm and turns a format change into lost detail rather
//! than a blank pane.
//!
//! Read out of Grok Build 1.0.5. The **error** frame is captured from that
//! binary — see `tests::UNAUTHENTICATED`, a real transcript from a machine with
//! no xAI sign-in. The rest is from that bundled guide, whose example stream is
//! `tests::DOCUMENTED`; no signed-in transcript has been captured yet.

use serde_json::Value;

use super::{Decode, Event, Kind};

/// One turn.
///
/// `-p/--single` is documented as "single-turn prompt … and exits", so like
/// Codex this was always one process per turn; continuity is `--resume <id>`.
pub(super) fn argv(thread: Option<&str>, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // Root options only, and there is no subcommand here — but the order is
    // kept anyway, because `--always-approve` after a subcommand is
    // `unexpected argument` and this argv is the template anything later
    // will copy.
    args.extend(Kind::Grok.bypass().iter().map(|flag| (*flag).to_string()));
    args.push("--output-format".into());
    args.push("streaming-json".into());
    // Grok Build updates itself in place unless told not to. A provisioner that
    // owns the binary — riabuild downloads it from its own mirror and verifies a
    // pinned digest — must never let it replace itself, or the digest describes
    // bytes that are no longer on disk.
    args.push("--no-auto-update".into());
    if let Some(thread) = thread {
        args.push("--resume".into());
        args.push(thread.to_string());
    }
    args.push("-p".into());
    args.push(prompt.to_string());
    args
}

#[derive(Default)]
pub(super) struct Reader {
    thread: Option<String>,
    /// This turn's `usage` lines so far, summed.
    input: u64,
    output: u64,
}

impl Decode for Reader {
    fn read(&mut self, line: &str) -> Vec<Event> {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match frame.get("type").and_then(Value::as_str) {
            Some(kind) => self.tagged(kind, &frame),
            None => self.acp(&frame),
        }
    }
}

impl Reader {
    /// One line of the documented `streaming-json` stream.
    fn tagged(&mut self, kind: &str, frame: &Value) -> Vec<Event> {
        match kind {
            // Chunks, not whole messages. They are emitted as they arrive and
            // the pane concatenates, because a chunk held back until a message
            // completed would leave the session looking idle for the whole of a
            // long answer.
            "text" => data(frame).map(Event::Said).into_iter().collect(),
            "thought" => data(frame).map(Event::Thought).into_iter().collect(),
            "tool_call" => {
                let id = tool_call_id(frame);
                let mut events = vec![Event::ToolStarted {
                    id: id.clone(),
                    // `toolName` is xAI's addition and the only field that
                    // names the tool itself; `title` is a display string and
                    // `kind` is ACP's coarse category (`read`, `execute`).
                    name: ["toolName", "title", "kind"]
                        .iter()
                        .find_map(|key| frame.get(*key).and_then(Value::as_str))
                        .unwrap_or("tool")
                        .to_string(),
                    detail: super::claude::summarise(frame.get("rawInput")).or_else(|| {
                        frame
                            .get("title")
                            .and_then(Value::as_str)
                            .map(super::claude::one_line)
                    }),
                }];
                // A call can be reported already settled.
                events.extend(finished(id, frame));
                events
            }
            "tool_call_update" => finished(tool_call_id(frame), frame).into_iter().collect(),
            "usage" => {
                let (input, output) = counts(frame.get("usage"));
                self.input += input;
                self.output += output;
                vec![Event::Usage {
                    input: self.input,
                    output: self.output,
                }]
            }
            "end" => self.end(frame),
            // A prompt that failed may still have spent tokens, and the process
            // exits after it, so the turn is over either way.
            "error" => {
                let mut events = Vec::new();
                if frame.get("usage").is_some() {
                    let (input, output) = counts(frame.get("usage"));
                    events.push(Event::Usage { input, output });
                }
                events.push(Event::Trouble(
                    frame
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("grok reported an error")
                        .to_string(),
                ));
                events.push(Event::Idle);
                self.input = 0;
                self.output = 0;
                events
            }
            // A plan, the command list a client would render as a menu, and the
            // documented-but-unlisted `max_turns_reached` / `auto_compact_*`.
            // None is something this TUI shows.
            _ => Vec::new(),
        }
    }

    /// `end` is always the last line of a turn.
    fn end(&mut self, frame: &Value) -> Vec<Event> {
        let mut events = Vec::new();
        // The prompt's own total, which counts subagents the summed `usage`
        // lines never saw. Absent when the prompt never reached the model.
        if frame.get("usage").is_some() {
            let (input, output) = counts(frame.get("usage"));
            events.push(Event::Usage {
                input: input.max(self.input),
                output: output.max(self.output),
            });
        }
        self.input = 0;
        self.output = 0;
        // Announced once: `end` repeats the id on every resumed turn, and
        // re-emitting `Ready` would reset the pane header each time.
        if let Some(id) = session_id(frame)
            && self.thread.as_deref() != Some(id.as_str())
        {
            self.thread = Some(id.clone());
            events.push(Event::Ready {
                thread: Some(id),
                // `modelUsage` is keyed by model; the first is the main agent's.
                model: frame
                    .get("modelUsage")
                    .and_then(Value::as_object)
                    .and_then(|models| models.keys().next().cloned()),
                // Grok Build takes `--reasoning-effort` and reports it nowhere
                // in `streaming-json`.
                effort: None,
            });
        }
        // `end_turn` is the ordinary finish and `cancelled` is the developer's
        // own interrupt. Anything else — `max_tokens`, `max_turn_requests`,
        // `refusal` — cut the answer short, and a pane that just stopped would
        // look like one that had finished.
        if let Some(reason) = frame.get("stopReason").and_then(Value::as_str)
            && !matches!(reason, "end_turn" | "cancelled")
        {
            events.push(Event::Trouble(format!("grok stopped: {reason}")));
        }
        events.push(Event::Idle);
        events
    }

    /// A line with no `type`: an ACP `session/update`, bare or wrapped in a
    /// JSON-RPC envelope. Not what 1.0.5 writes — see the module note.
    fn acp(&mut self, frame: &Value) -> Vec<Event> {
        let params = frame.get("params");
        let update = params
            .and_then(|params| params.get("update"))
            .or_else(|| frame.get("update"))
            .unwrap_or(frame);

        let mut events = Vec::new();
        if let Some(id) = session_id(frame).or_else(|| params.and_then(session_id))
            && self.thread.as_deref() != Some(id.as_str())
        {
            self.thread = Some(id.clone());
            events.push(Event::Ready {
                thread: Some(id),
                model: frame
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                // Grok Build takes `--reasoning-effort` and reports it nowhere
                // in `streaming-json`, which is documented rather than observed.
                effort: None,
            });
        }
        let discriminant = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        events.extend(match discriminant {
            "agent_message_chunk" => text_of(update).map(Event::Said).into_iter().collect(),
            "agent_thought_chunk" => text_of(update).map(Event::Thought).into_iter().collect(),
            "tool_call" => vec![Event::ToolStarted {
                id: tool_call_id(update),
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
            }],
            "tool_call_update" => finished(tool_call_id(update), update).into_iter().collect(),
            // The developer's own words echoed back, a plan, a command menu.
            _ => Vec::new(),
        });
        events
    }
}

/// ACP's statuses are `pending`, `in_progress`, `completed` and `failed`. Only
/// the last two end a call; an update that merely reports progress must not be
/// read as a finish, or every tool call appears to succeed the moment it starts.
fn finished(id: String, frame: &Value) -> Option<Event> {
    match frame.get("status").and_then(Value::as_str) {
        Some("completed") => Some(Event::ToolFinished { id, ok: true }),
        Some("failed") => Some(Event::ToolFinished { id, ok: false }),
        _ => None,
    }
}

fn tool_call_id(frame: &Value) -> String {
    frame
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `text` and `thought` carry their chunk as a bare string under `data`.
fn data(frame: &Value) -> Option<String> {
    frame
        .get("data")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Input and output tokens, with cache reads and writes counted as input as
/// Claude's are: they were billed differently, not absent. The guide is
/// explicit that Grok's `input_tokens` is the *uncached* part only.
fn counts(usage: Option<&Value>) -> (u64, u64) {
    let field = |name: &str| {
        usage
            .and_then(|usage| usage.get(name))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    (
        field("input_tokens")
            + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens"),
        field("output_tokens"),
    )
}

/// `end` spells it `sessionId`; ACP does too, and a `-p` run may spell it
/// `session_id`. Both are accepted rather than one being guessed at.
fn session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// ACP content blocks are `{"type":"text","text":"…"}`, one or a list of them.
fn text_of(update: &Value) -> Option<String> {
    let content = update.get("content")?;
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

    /// The example stream in the user guide Grok Build 1.0.5 installs,
    /// `docs/user-guide/14-headless-mode.md`, with its elided `{...}` filled in
    /// with the shapes the same page documents for the `json` format.
    const DOCUMENTED: &str = r#"{"type":"thought","data":"Analyzing the directory structure..."}
{"type":"tool_call","toolCallId":"call_1","title":"Read","kind":"read","status":"in_progress","toolName":"read_file","rawInput":{"path":"src/main.rs"},"content":[],"locations":[]}
{"type":"tool_call_update","toolCallId":"call_1","status":"completed","content":[],"rawOutput":{"lines":42},"locations":[]}
{"type":"text","data":"Here's a summary"}
{"type":"usage","messageId":"resp_1","stopReason":"end_turn","usage":{"input_tokens":812,"output_tokens":45,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0},"signature":"..."}
{"type":"end","stopReason":"end_turn","sessionId":"abc123","requestId":"xyz789","usage":{"input_tokens":812,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":45},"num_turns":1,"modelUsage":{"grok-build":{"inputTokens":812,"outputTokens":45,"modelCalls":1}}}"#;

    fn decode(transcript: &str) -> Vec<Event> {
        let mut reader = Reader::default();
        transcript
            .lines()
            .flat_map(|line| reader.read(line))
            .collect()
    }

    #[test]
    fn the_documented_stream_reads_as_a_whole_turn() {
        assert_eq!(
            decode(DOCUMENTED),
            vec![
                Event::Thought("Analyzing the directory structure...".into()),
                Event::ToolStarted {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    detail: Some("src/main.rs".into()),
                },
                Event::ToolFinished {
                    id: "call_1".into(),
                    ok: true
                },
                Event::Said("Here's a summary".into()),
                Event::Usage {
                    input: 812,
                    output: 45
                },
                Event::Usage {
                    input: 812,
                    output: 45
                },
                Event::Ready {
                    thread: Some("abc123".into()),
                    model: Some("grok-build".into()),
                    effort: None,
                },
                Event::Idle,
            ]
        );
    }

    #[test]
    fn a_real_unauthenticated_transcript_says_what_is_wrong() {
        let events = decode(UNAUTHENTICATED);
        let Some(Event::Trouble(text)) = events.first() else {
            panic!("expected trouble, got {events:?}");
        };
        assert!(text.starts_with("Not signed in."), "{text}");
        // The remedy is in the message and must survive intact — it is the only
        // thing that tells a developer how to fix this.
        assert!(text.contains("grok login --device-code"), "{text}");
        // The process exits after an error, so the turn is over.
        assert_eq!(events.last(), Some(&Event::Idle));
    }

    #[test]
    fn usage_is_summed_across_the_responses_of_one_turn_and_starts_again_after_it() {
        // One `usage` line per model response: a turn that called a tool made
        // two. Cache hits are input, as they are for Claude.
        let turn = r#"{"type":"usage","usage":{"input_tokens":100,"cache_read_input_tokens":900,"output_tokens":10}}
{"type":"usage","usage":{"input_tokens":50,"cache_read_input_tokens":1000,"output_tokens":20}}
{"type":"end","stopReason":"end_turn","sessionId":"s1"}
{"type":"usage","usage":{"input_tokens":5,"output_tokens":1}}"#;
        let usage: Vec<_> = decode(turn)
            .into_iter()
            .filter(|event| matches!(event, Event::Usage { .. }))
            .collect();
        assert_eq!(
            usage,
            vec![
                Event::Usage {
                    input: 1000,
                    output: 10
                },
                Event::Usage {
                    input: 2050,
                    output: 30
                },
                Event::Usage {
                    input: 5,
                    output: 1
                },
            ]
        );
    }

    #[test]
    fn the_session_is_announced_once_and_not_on_every_resumed_turn() {
        let two_turns = r#"{"type":"end","stopReason":"end_turn","sessionId":"s1"}
{"type":"end","stopReason":"end_turn","sessionId":"s1"}"#;
        let events = decode(two_turns);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Ready { .. }))
                .count(),
            1
        );
        assert_eq!(
            events.iter().filter(|e| matches!(e, Event::Idle)).count(),
            2
        );
    }

    #[test]
    fn a_turn_cut_short_says_so_and_an_interrupted_one_does_not() {
        let cut = decode(r#"{"type":"end","stopReason":"max_tokens","sessionId":"s1"}"#);
        assert!(cut.contains(&Event::Trouble("grok stopped: max_tokens".into())));
        let interrupted = decode(r#"{"type":"end","stopReason":"cancelled","sessionId":"s1"}"#);
        assert!(!interrupted.iter().any(|e| matches!(e, Event::Trouble(_))));
    }

    #[test]
    fn a_tool_call_only_finishes_when_grok_says_it_finished() {
        // `in_progress` is an update, not a completion. Reading it as one makes
        // every tool call appear to succeed the instant it starts.
        let stream = r#"{"type":"tool_call","toolCallId":"t1","kind":"execute","toolName":"bash","status":"in_progress","rawInput":{"command":"ls -la"}}
{"type":"tool_call_update","toolCallId":"t1","status":"in_progress"}
{"type":"tool_call_update","toolCallId":"t1","status":"failed"}"#;
        assert_eq!(
            decode(stream),
            vec![
                Event::ToolStarted {
                    id: "t1".into(),
                    name: "bash".into(),
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
    fn an_acp_update_is_still_read_bare_or_wrapped_in_a_json_rpc_envelope() {
        let bare = r#"{"sessionId":"s1","sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}"#;
        let wrapped = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":[{"type":"text","text":"h"},{"type":"text","text":"i"}]}}}"#;
        let expected = vec![
            Event::Ready {
                thread: Some("s1".into()),
                model: None,
                effort: None,
            },
            Event::Said("hi".into()),
        ];
        assert_eq!(decode(bare), expected);
        assert_eq!(decode(wrapped), expected);
    }

    #[test]
    fn the_launch_bypasses_approvals_and_refuses_to_self_update() {
        let args = argv(None, "hello");
        assert!(args.iter().any(|a| a == "--always-approve"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--output-format", "streaming-json"])
        );
        // riabuild verified a pinned digest for the binary on disk. A harness
        // that replaces itself makes that digest describe bytes that are gone.
        assert!(args.iter().any(|a| a == "--no-auto-update"));
        assert!(args.windows(2).any(|w| w == ["-p", "hello"]));

        let resumed = argv(Some("s1"), "again");
        assert!(resumed.windows(2).any(|w| w == ["--resume", "s1"]));
    }

    #[test]
    fn an_unknown_line_loses_detail_and_nothing_else() {
        assert_eq!(decode(r#"{"type":"auto_compact_started"}"#), Vec::new());
        assert_eq!(decode(r#"{"sessionUpdate":"something_new"}"#), Vec::new());
        assert_eq!(decode("not json"), Vec::new());
    }
}
