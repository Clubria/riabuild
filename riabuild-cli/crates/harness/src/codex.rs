//! The Codex CLI's `exec --json`, which is a thread of one-shot turns.
//!
//! `codex exec` takes a prompt, answers it and exits; `codex exec resume
//! <SESSION_ID> <PROMPT>` opens the same thread again. So a Codex session here
//! is a *thread id* plus one child per turn, and `thread.started` is the frame
//! that hands over the id everything after it depends on.
//!
//! `codex app-server` would be the better transport — one process, JSON-RPC 2.0,
//! real steering and interrupts — and is deliberately not used yet: `codex
//! --help` marks it `[experimental]`, its schema is generated per release
//! (`codex app-server generate-json-schema`), and OpenAI's documentation says it
//! changes without notice. Adopting it means pinning a version and generating
//! types against it, which is worth doing and is not this change.
//!
//! Read out of codex-cli 0.148.0. The **envelope** below — `thread.started`,
//! `turn.started`, `turn.failed`, `item.completed`, and top-level `error` — is
//! captured from that binary; see `tests::FAILURE`, which is a real transcript.
//! The bodies of the *successful* item types are not: this machine has no
//! OpenAI sign-in, so no successful turn could be recorded, and those arms are
//! written from OpenAI's documentation and marked `INFERRED`. They are the
//! first thing to re-read when the pin moves.

use serde_json::Value;

use super::{Decode, Event, Kind};

/// One turn.
///
/// `codex exec` was always one turn per process, so nothing here changed when
/// the other two joined it: continuity is `exec resume <SESSION_ID> <PROMPT>`,
/// with the id as the first positional and the prompt as the second.
pub(super) fn argv(thread: Option<&str>, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = vec!["exec".into()];
    // `resume` is a subcommand of `exec`. Options are given before both
    // positionals so that a prompt beginning with `-` cannot be read as one.
    if thread.is_some() {
        args.push("resume".into());
    }
    args.push("--json".into());
    args.extend(Kind::Codex.bypass().iter().map(|flag| (*flag).to_string()));
    // Hooks are configured in the checkout riabuild provisioned, and Codex
    // otherwise refuses to run them until somebody has trusted them
    // interactively — which in a headless session is nobody, for ever.
    args.push("--dangerously-bypass-hook-trust".into());
    if let Some(thread) = thread {
        args.push(thread.to_string());
    }
    args.push(prompt.to_string());
    args
}

/// Stateless: `thread.started` is the only frame carrying the id, and it goes
/// straight out as [`Event::Ready`].
pub(super) struct Reader;

impl Decode for Reader {
    fn read(&mut self, line: &str) -> Vec<Event> {
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            // Codex prints `Reading additional input from stdin...` before its
            // first frame, and writes `tracing` diagnostics that are not JSON.
            // Neither is an error and neither may end a session.
            return Vec::new();
        };
        match frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            // VERIFIED against 0.148.0.
            "thread.started" => {
                vec![Event::Ready {
                    thread: frame
                        .get("thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    // Codex names no model in this frame. Left `None` rather
                    // than guessed: a pane showing the wrong model is worse than
                    // one showing none.
                    model: None,
                }]
            }
            // VERIFIED. Carries nothing; the fleet already knows a turn began,
            // because it started the child.
            "turn.started" => Vec::new(),
            // INFERRED: documented to carry a `usage` object. The field names
            // are read defensively so that a rename costs the token count and
            // not the `Idle` that unblocks the session.
            "turn.completed" => {
                let mut events = Vec::new();
                if let Some(usage) = frame.get("usage") {
                    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
                    events.push(Event::Usage {
                        input: field("input_tokens") + field("cached_input_tokens"),
                        output: field("output_tokens"),
                    });
                }
                events.push(Event::Idle);
                events
            }
            // VERIFIED against 0.148.0 — this is the 401 path.
            "turn.failed" => vec![
                Event::Trouble(
                    frame
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("the turn failed")
                        .to_string(),
                ),
                Event::Idle,
            ],
            // VERIFIED. Codex reports retries this way — five per transport
            // before it gives up — so these arrive in bursts and are notices
            // rather than failures.
            "error" => vec![Event::Trouble(message(&frame))],
            "item.started" | "item.completed" | "item.updated" => {
                item(frame.get("item"), frame["type"] == "item.completed")
            }
            _ => Vec::new(),
        }
    }
}

fn message(frame: &Value) -> String {
    frame
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("codex reported an error")
        .to_string()
}

/// One item from the thread.
///
/// `done` distinguishes `item.completed` from `item.started`, which is what
/// decides whether a command execution is starting or finishing.
fn item(item: Option<&Value>, done: bool) -> Vec<Event> {
    let Some(item) = item else {
        return Vec::new();
    };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        // VERIFIED: this is the arm the captured 401 transcript exercises.
        "error" => vec![Event::Trouble(message(item))],
        // INFERRED below this line.
        "agent_message" => match item.get("text").and_then(Value::as_str) {
            Some(text) if done && !text.trim().is_empty() => vec![Event::Said(text.to_string())],
            _ => Vec::new(),
        },
        "reasoning" => match item.get("text").and_then(Value::as_str) {
            Some(text) if done => vec![Event::Thought(text.to_string())],
            _ => Vec::new(),
        },
        "command_execution" => {
            if done {
                // A command with no exit code recorded is treated as having
                // worked: reporting a red failure for a field that simply was
                // not sent would make every successful turn look broken.
                let ok = item
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|code| code == 0)
                    .unwrap_or(true);
                vec![Event::ToolFinished { id, ok }]
            } else {
                vec![Event::ToolStarted {
                    id,
                    name: "shell".into(),
                    detail: item
                        .get("command")
                        .and_then(Value::as_str)
                        .map(super::claude::one_line),
                }]
            }
        }
        "file_change" => {
            if done {
                vec![Event::ToolFinished { id, ok: true }]
            } else {
                vec![Event::ToolStarted {
                    id,
                    name: "edit".into(),
                    detail: item.get("path").and_then(Value::as_str).map(str::to_string),
                }]
            }
        }
        "mcp_tool_call" => {
            let name = item
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("mcp")
                .to_string();
            if done {
                vec![Event::ToolFinished { id, ok: true }]
            } else {
                vec![Event::ToolStarted {
                    id,
                    name,
                    detail: item
                        .get("server")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }]
            }
        }
        "web_search" => {
            if done {
                vec![Event::ToolFinished { id, ok: true }]
            } else {
                vec![Event::ToolStarted {
                    id,
                    name: "search".into(),
                    detail: item
                        .get("query")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }]
            }
        }
        // `todo_list` and whatever 0.149 adds. Dropped deliberately.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from codex-cli 0.148.0 by running
    /// `codex exec --json --dangerously-bypass-approvals-and-sandbox "say only
    /// the word ok"` with no OpenAI sign-in present. Every line is stdout
    /// verbatim; the interleaved `tracing` lines this binary writes went to
    /// stderr and are correctly absent.
    const FAILURE: &str = r#"Reading additional input from stdin...
{"type":"thread.started","thread_id":"01a03328-0c86-7f93-8c73-817d64e595c6"}
{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 2/5 (unexpected status 401 Unauthorized)"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Falling back from WebSockets to HTTPS transport."}}
{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}}"#;

    fn decode(transcript: &str) -> Vec<Event> {
        let mut reader = Reader;
        transcript
            .lines()
            .flat_map(|line| reader.read(line))
            .collect()
    }

    #[test]
    fn a_real_failed_transcript_decodes_to_a_thread_that_ends_idle() {
        // The important property is the last event: a turn that failed must
        // still unblock the session, or the pane shows an agent busy for ever.
        let events = decode(FAILURE);
        assert_eq!(
            events.first(),
            Some(&Event::Ready {
                thread: Some("01a03328-0c86-7f93-8c73-817d64e595c6".into()),
                model: None,
            })
        );
        assert_eq!(events.last(), Some(&Event::Idle));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Trouble(_)))
                .count(),
            3
        );
    }

    #[test]
    fn the_chatter_codex_prints_before_its_first_frame_is_not_an_error() {
        // `Reading additional input from stdin...` is a plain line on stdout.
        // A decoder that treated a non-JSON line as fatal would kill every
        // Codex session at the moment it started.
        assert_eq!(decode("Reading additional input from stdin..."), Vec::new());
    }

    #[test]
    fn a_resumed_turn_names_the_thread_and_keeps_every_bypass() {
        let first = argv(None, "hello");
        assert_eq!(first[0], "exec");
        assert!(!first.iter().any(|a| a == "resume"));
        assert_eq!(first.last().unwrap(), "hello");

        let next = argv(Some("thread-1"), "again");
        assert_eq!(&next[..2], ["exec", "resume"]);
        // The id is the first positional and the prompt the second; swapping
        // them sends the prompt as a session id and resumes nothing.
        let positionals: Vec<&String> = next
            .iter()
            .filter(|a| !a.starts_with("--"))
            .skip(2)
            .collect();
        assert_eq!(positionals, vec!["thread-1", "again"]);

        for args in [&first, &next] {
            assert!(args.iter().any(|a| a == "--json"));
            assert!(
                args.iter()
                    .any(|a| a == "--dangerously-bypass-approvals-and-sandbox")
            );
            assert!(args.iter().any(|a| a == "--dangerously-bypass-hook-trust"));
        }
    }

    #[test]
    fn a_command_that_exited_non_zero_is_reported_as_having_failed() {
        let started = r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"ls  -la"}}"#;
        let failed = r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","exit_code":2}}"#;
        assert_eq!(
            decode(&format!("{started}\n{failed}")),
            vec![
                Event::ToolStarted {
                    id: "i1".into(),
                    name: "shell".into(),
                    detail: Some("ls -la".into()),
                },
                Event::ToolFinished {
                    id: "i1".into(),
                    ok: false
                },
            ]
        );
    }

    #[test]
    fn a_command_with_no_exit_code_recorded_is_not_reported_as_broken() {
        // These arms are inferred rather than captured, so the failure mode that
        // matters is a field that is simply absent — which must not paint every
        // successful turn red.
        let done = r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution"}}"#;
        assert_eq!(
            decode(done),
            vec![Event::ToolFinished {
                id: "i1".into(),
                ok: true
            }]
        );
    }

    #[test]
    fn a_turn_that_completed_reports_its_tokens_and_then_goes_idle() {
        let done = r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":90,"output_tokens":5}}"#;
        assert_eq!(
            decode(done),
            vec![
                Event::Usage {
                    input: 100,
                    output: 5
                },
                Event::Idle
            ]
        );
    }

    #[test]
    fn a_turn_that_completed_with_no_usage_still_goes_idle() {
        // The `Idle` is what unblocks the session, so it can never be
        // conditional on a field this decoder only believes it will find.
        assert_eq!(decode(r#"{"type":"turn.completed"}"#), vec![Event::Idle]);
    }

    #[test]
    fn an_unknown_item_type_loses_detail_and_nothing_else() {
        assert_eq!(
            decode(r#"{"type":"item.completed","item":{"id":"i","type":"todo_list"}}"#),
            Vec::new()
        );
        assert_eq!(decode(r#"{"type":"thread.compacted"}"#), Vec::new());
    }
}
