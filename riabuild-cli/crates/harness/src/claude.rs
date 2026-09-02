//! Claude Code's stream-json, which is the only one of the three that is a
//! conversation rather than a series of one-shot runs.
//!
//! `--input-format stream-json` leaves stdin open and reads one JSON user
//! message per turn, so a session is one process for as long as the developer
//! keeps talking to it. That is what [`Restart::Persistent`] means, and it is
//! why this is the only encoder with a [`Encoder::stdin_prompt`].
//!
//! Read out of Claude Code 2.1.235. Every shape below is pinned against a
//! transcript captured from that binary — see `tests::TRANSCRIPT`, which is
//! literally what `claude -p --output-format stream-json --verbose` wrote.
//!
//! [`Restart::Persistent`]: super::Restart::Persistent

use serde_json::Value;

use super::{Decode, Event, Kind};

/// One turn.
///
/// `-p` is a flag and the prompt is a positional, which is what lets a turn be
/// one process: `--input-format stream-json` would make this a conversation that
/// reads stdin for ever, and a detached child has nobody left holding the write
/// end of that pipe.
///
/// Verified against Claude Code 2.1.235: this exact argv, with `--resume`,
/// answers inside the session the id names. `--settings` is a root option
/// taking exactly one value, verified against 2.1.252.
pub(super) fn argv(thread: Option<&str>, prompt: &str, org_settings: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        // Not optional under `-p`: without it the stream is the final result and
        // nothing else, so every tool call, every subagent and the whole of the
        // reasoning never arrive.
        "--verbose".into(),
    ];
    args.extend(Kind::Claude.bypass().iter().map(|flag| (*flag).to_string()));
    // The team's Claude Code settings. An interactive session gets them because
    // every account launcher passes `--settings`; `riabuild agents` runs the
    // binary itself, so until this was passed org policy stopped at the edge of
    // the window — the model the org chose and a lead's `permissions.deny`
    // applied to `claude` and to nothing `riabuild agents` started.
    //
    // What this names is the *vetted* cache `org_settings` writes, never the
    // server's payload, so it has already been through `vetting.rs` and carries
    // no program. `bypass()` above still decides the permission mode: it is
    // argv and this is a file, and a turn that stopped to ask permission would
    // have nobody to ask.
    if let Some(settings) = org_settings {
        args.push("--settings".into());
        args.push(settings.to_string());
    }
    if let Some(thread) = thread {
        args.push("--resume".into());
        args.push(thread.to_string());
    }
    // Last, and a positional. `--resume` takes an optional value, so the prompt
    // has to be separated from it by nothing that could be read as one.
    args.push(prompt.to_string());
    args
}

/// Stateless: every frame Claude Code writes carries its own `session_id`, so
/// nothing has to be remembered between lines.
pub(super) struct Reader;

impl Decode for Reader {
    fn read(&mut self, line: &str) -> Vec<Event> {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let kind = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Work a subagent did carries the tool call that started it. Claude Code
        // is the only one of the three that attributes anything this way, and it
        // is what lets a pane show a delegated tool call under the agent that
        // asked for it rather than as the parent's own.
        let parent = frame
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let attribute = |events: Vec<Event>| -> Vec<Event> {
            match &parent {
                Some(parent) => events
                    .into_iter()
                    .map(|event| Event::Delegated {
                        parent: parent.clone(),
                        inner: Box::new(event),
                    })
                    .collect(),
                None => events,
            }
        };

        match kind {
            "system" => self.system(&frame),
            "assistant" => attribute(self.assistant(&frame)),
            "user" => attribute(tool_results(&frame)),
            "result" => result(&frame),
            // Partial deltas, only present under `--include-partial-messages`,
            // which this crate does not pass: a TUI that redrew per token would
            // spend its whole frame budget on text it is about to receive whole.
            "stream_event" => Vec::new(),
            // Claude Code reports its own rate-limit posture unprompted. Only a
            // refusal is worth a line; `allowed` is the normal case and saying
            // so on every turn would bury everything else.
            "rate_limit_event" => rate_limit(&frame),
            _ => Vec::new(),
        }
    }
}

impl Reader {
    fn system(&mut self, frame: &Value) -> Vec<Event> {
        match frame.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                vec![Event::Ready {
                    thread: frame
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    model: frame
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }]
            }
            // A hook that failed is worth surfacing; one that ran is not. This
            // is the only place a `SessionStart` hook can report at all, and a
            // hook that exits non-zero blocks the session it was meant to set up.
            Some("hook_response") => {
                let code = frame.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
                if code == 0 {
                    return Vec::new();
                }
                let name = frame
                    .get("hook_name")
                    .and_then(Value::as_str)
                    .unwrap_or("a hook");
                let stderr = frame
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                vec![Event::Trouble(
                    format!("{name} exited {code}: {stderr}").trim().to_string(),
                )]
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, frame: &Value) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(content) = frame
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return events;
        };
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Whitespace-only text blocks are real and frequent — a turn
                    // that only called a tool still carries one — and a blank
                    // line in the transcript for each is noise.
                    if let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        events.push(Event::Said(text.to_string()));
                    }
                }
                Some("thinking") => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        events.push(Event::Thought(text.to_string()));
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    events.push(Event::ToolStarted {
                        detail: summarise(block.get("input")),
                        id,
                        name,
                    });
                }
                _ => {}
            }
        }
        events
    }
}

/// A tool's result arrives as a *user* message, because that is how the
/// Messages API models it.
fn tool_results(frame: &Value) -> Vec<Event> {
    let mut events = Vec::new();
    let Some(content) = frame
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return events;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        events.push(Event::ToolFinished {
            id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            ok: !block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    events
}

fn result(frame: &Value) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(usage) = frame.get("usage") {
        let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
        events.push(Event::Usage {
            // Cache reads and writes are input tokens that were billed
            // differently, not tokens that did not exist. A count that ignored
            // them reads as an agent doing almost nothing on a long session.
            input: field("input_tokens")
                + field("cache_read_input_tokens")
                + field("cache_creation_input_tokens"),
            output: field("output_tokens"),
        });
    }
    if frame.get("is_error").and_then(Value::as_bool) == Some(true) {
        let text = frame
            .get("result")
            .and_then(Value::as_str)
            .or_else(|| frame.get("error").and_then(Value::as_str))
            .unwrap_or("the turn failed");
        events.push(Event::Trouble(text.to_string()));
    }
    events.push(Event::Idle);
    events
}

fn rate_limit(frame: &Value) -> Vec<Event> {
    let info = frame.get("rate_limit_info");
    let status = info
        .and_then(|info| info.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("allowed");
    if status == "allowed" {
        return Vec::new();
    }
    vec![Event::Trouble(format!("rate limited: {status}"))]
}

/// One line describing a tool call's input.
///
/// A tool call's input is unbounded — a `Write` carries the whole file — so this
/// picks the field that identifies the call and gives up rather than truncating
/// something structural. The keys are Claude Code's own tool schema, and an
/// unknown tool simply gets no detail, which is correct: a wrong summary is
/// worse than none.
fn summarise(input: Option<&Value>) -> Option<String> {
    let input = input?;
    for key in ["command", "file_path", "pattern", "path", "url", "query"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return Some(one_line(value));
        }
    }
    // `Task` — a subagent — is the one call whose point is its prompt.
    if let Some(description) = input.get("description").and_then(Value::as_str) {
        return Some(one_line(description));
    }
    None
}

/// Flattens to a single line and bounds the length.
///
/// A pane renders one row per tool call, and a `command` is routinely a
/// multi-line shell script; left whole it would push everything after it off
/// the screen.
pub(super) fn one_line(value: &str) -> String {
    const MAX: usize = 120;
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from Claude Code 2.1.235 by running
    /// `claude -p "say only the word ok" --output-format stream-json --verbose`
    /// and taking stdout verbatim. Trimmed only by removing fields irrelevant to
    /// this decoder from the `init` frame's very long lists, and by shortening
    /// the ids; every field this file reads is exactly as the binary wrote it.
    const TRANSCRIPT: &str = r#"{"type":"system","subtype":"hook_started","hook_id":"0c4d2a35","hook_name":"SessionStart:startup","hook_event":"SessionStart","session_id":"28eac785-edf9-4ac2-8920-81bd1870b094"}
{"type":"system","subtype":"hook_response","hook_id":"0c4d2a35","hook_name":"SessionStart:startup","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","session_id":"28eac785-edf9-4ac2-8920-81bd1870b094"}
{"type":"system","subtype":"init","cwd":"/home/user","session_id":"28eac785-edf9-4ac2-8920-81bd1870b094","tools":["Task","Bash"],"mcp_servers":[],"model":"claude-opus-5[1m]","permissionMode":"bypassPermissions","apiKeySource":"none","claude_code_version":"2.1.235","capabilities":["interrupt_receipt_v1"]}
{"type":"assistant","message":{"model":"claude-opus-5","id":"msg_011","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":null,"usage":{"input_tokens":2,"cache_creation_input_tokens":12529,"cache_read_input_tokens":15900,"output_tokens":4}},"parent_tool_use_id":null,"session_id":"28eac785-edf9-4ac2-8920-81bd1870b094"}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1787578200,"rateLimitType":"five_hour"},"session_id":"28eac785-edf9-4ac2-8920-81bd1870b094"}
{"is_error":false,"duration_api_ms":1667,"num_turns":1,"stop_reason":"end_turn","session_id":"28eac785-edf9-4ac2-8920-81bd1870b094","total_cost_usd":0.13335,"usage":{"input_tokens":2,"cache_creation_input_tokens":12529,"cache_read_input_tokens":15900,"output_tokens":4},"subtype":"success","result":"ok","type":"result","duration_ms":1736}"#;

    fn decode(transcript: &str) -> Vec<Event> {
        let mut reader = Reader;
        transcript
            .lines()
            .flat_map(|line| reader.read(line))
            .collect()
    }

    #[test]
    fn a_real_transcript_decodes_to_the_session_the_developer_had() {
        let events = decode(TRANSCRIPT);
        assert_eq!(
            events,
            vec![
                Event::Ready {
                    thread: Some("28eac785-edf9-4ac2-8920-81bd1870b094".into()),
                    model: Some("claude-opus-5[1m]".into()),
                },
                Event::Said("ok".into()),
                Event::Usage {
                    input: 2 + 15900 + 12529,
                    output: 4
                },
                Event::Idle,
            ]
        );
    }

    #[test]
    fn a_hook_that_succeeded_is_not_news_and_one_that_failed_is() {
        // The `SessionStart` hook above exits 0 and produces nothing. A hook
        // that fails is the only signal that a session was set up wrong, and it
        // appears nowhere else.
        assert!(
            !decode(TRANSCRIPT)
                .iter()
                .any(|e| matches!(e, Event::Trouble(_)))
        );

        let failed = r#"{"type":"system","subtype":"hook_response","hook_name":"SessionStart:startup","exit_code":2,"stderr":"no such file"}"#;
        assert_eq!(
            decode(failed),
            vec![Event::Trouble(
                "SessionStart:startup exited 2: no such file".into()
            )]
        );
    }

    #[test]
    fn cached_input_tokens_are_still_input_tokens() {
        // Counting only `input_tokens` reports 2 for a turn that read nearly
        // 30,000 — an agent that appears to be doing nothing.
        let Some(Event::Usage { input, .. }) = decode(TRANSCRIPT)
            .into_iter()
            .find(|event| matches!(event, Event::Usage { .. }))
        else {
            panic!("no usage in the transcript");
        };
        assert_eq!(input, 28431);
    }

    #[test]
    fn a_tool_call_and_its_result_are_matched_by_id() {
        let transcript = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la\n  /tmp"}}]},"session_id":"s"}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false}]},"session_id":"s"}"#;
        assert_eq!(
            decode(transcript),
            vec![
                Event::ToolStarted {
                    id: "toolu_1".into(),
                    name: "Bash".into(),
                    // Folded to one line: a command is routinely a whole script.
                    detail: Some("ls -la /tmp".into()),
                },
                Event::ToolFinished {
                    id: "toolu_1".into(),
                    ok: true
                },
            ]
        );
    }

    #[test]
    fn a_failed_tool_result_is_not_a_successful_one() {
        let transcript = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true}]}}"#;
        assert_eq!(
            decode(transcript),
            vec![Event::ToolFinished {
                id: "t".into(),
                ok: false
            }]
        );
    }

    #[test]
    fn a_subagents_work_is_attributed_to_the_call_that_spawned_it() {
        // `parent_tool_use_id` is the only cross-provider-relevant thing any of
        // the three emits: it is how a delegated turn is told from the parent's
        // own, and it is why `Event::Delegated` exists.
        let transcript = r#"{"type":"assistant","parent_tool_use_id":"toolu_parent","message":{"content":[{"type":"text","text":"inner"}]}}"#;
        assert_eq!(
            decode(transcript),
            vec![Event::Delegated {
                parent: "toolu_parent".into(),
                inner: Box::new(Event::Said("inner".into())),
            }]
        );
    }

    #[test]
    fn a_line_that_is_not_json_is_dropped_rather_than_fatal() {
        // Nothing in Claude Code writes one today. The rule is the point: a
        // decoder that failed here would take the session down over a stray
        // line of someone else's output.
        assert_eq!(decode("not json at all\n\n"), Vec::new());
    }

    #[test]
    fn an_unknown_frame_type_loses_detail_and_nothing_else() {
        // The stream gains frame types between patch releases. Each one must
        // cost a line of transcript, never a session.
        assert_eq!(
            decode(r#"{"type":"something_new_in_2_2","payload":{}}"#),
            Vec::new()
        );
    }

    #[test]
    fn an_allowed_rate_limit_is_not_worth_saying_and_a_refusal_is() {
        assert_eq!(
            decode(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#),
            vec![Event::Trouble("rate limited: rejected".into())]
        );
    }

    #[test]
    fn a_turn_asks_for_the_whole_stream_and_carries_the_bypass() {
        let args = argv(None, "hello", None);
        // `--verbose`, without which the stream is the final result and nothing
        // else — no tool calls, no subagents, no reasoning.
        assert!(
            args.windows(2)
                .any(|w| w == ["--output-format", "stream-json"])
        );
        assert!(args.iter().any(|a| a == "--verbose"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "bypassPermissions"])
        );
        assert!(!args.iter().any(|a| a == "--resume"));
        // Never `--input-format stream-json`: that makes the process a
        // conversation reading stdin for ever, and a detached child has nobody
        // holding the write end of that pipe.
        assert!(!args.iter().any(|a| a == "--input-format"));
        // A machine with no cached team settings names no file, rather than
        // naming one that is not there. The account launchers drop `--settings`
        // under exactly the same condition.
        assert!(!args.iter().any(|a| a == "--settings"));
    }

    #[test]
    fn the_prompt_is_the_last_argument_so_resume_cannot_swallow_it() {
        // `--resume` takes an *optional* value. With the id immediately before
        // the prompt, the prompt is the next positional and the id is the
        // option's value; any other order risks the prompt being read as the id.
        let args = argv(Some("abc"), "hello", None);
        assert_eq!(args.last().unwrap(), "hello");
        assert!(args.windows(2).any(|w| w == ["--resume", "abc"]));
        let resume_at = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[resume_at + 1], "abc");
        assert_eq!(args[resume_at + 2], "hello");
    }

    /// The team's settings reach a headless turn, which is the only way the
    /// model the org chose applies to `riabuild agents` at all: nothing else
    /// here passes `--settings`, because nothing else has an account launcher
    /// in front of it.
    #[test]
    fn the_team_settings_are_carried_and_still_leave_the_prompt_last() {
        let args = argv(Some("abc"), "hello", Some("/home/dev/.riabuild/org.json"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--settings", "/home/dev/.riabuild/org.json"])
        );
        // Ahead of `--resume`, and both ahead of the prompt. `--settings` takes
        // exactly one value, so the danger is not that it swallows the id — it
        // is an order in which the *prompt* becomes somebody's option value.
        let settings_at = args.iter().position(|a| a == "--settings").unwrap();
        let resume_at = args.iter().position(|a| a == "--resume").unwrap();
        assert!(settings_at < resume_at);
        assert_eq!(args.last().unwrap(), "hello");
        assert_eq!(args.iter().filter(|a| *a == "hello").count(), 1);
    }
}
