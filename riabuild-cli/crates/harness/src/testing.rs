//! Canned transcripts, and the real decoders run over them.
//!
//! `riabuild-agents` draws from [`Event`]s and must be testable without three
//! signed-in vendor accounts and a network. Handing it a hand-written `Vec<Event>`
//! would test the renderer against a fiction, so this runs the *production*
//! decoder over bytes a real binary produced — which means a decoder change that
//! breaks the TUI fails the TUI's own tests.
//!
//! Behind the `testing` feature rather than `#[cfg(test)]` because a dependency
//! crate is never compiled with `cfg(test)`, so a downstream test could not
//! otherwise reach any of this.

use super::{Decoder, Event, Kind, claude, codex, grok};

/// Runs the harness's own decoder over a transcript, as the pump would.
pub fn decode(kind: Kind, transcript: &str) -> Vec<Event> {
    let mut decoder: Box<dyn Decoder> = match kind {
        Kind::Claude => Box::new(claude::Reader),
        Kind::Codex => Box::new(codex::Reader),
        Kind::Grok => Box::new(grok::Reader::default()),
    };
    transcript
        .lines()
        .flat_map(|line| decoder.read(line))
        .collect()
}

/// A short Claude Code session: start, one tool call, an answer, idle.
///
/// The frame shapes are those of Claude Code 2.1.235, taken from a transcript
/// captured from the real binary; the tool call is spliced in from a second run
/// so that one fixture exercises the whole of a turn.
pub const CLAUDE: &str = r#"{"type":"system","subtype":"init","session_id":"28eac785-edf9-4ac2-8920-81bd1870b094","model":"claude-opus-5[1m]","permissionMode":"bypassPermissions"}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test --workspace"}}],"usage":{"input_tokens":2,"output_tokens":4}},"parent_tool_use_id":null,"session_id":"28eac785"}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false}]},"session_id":"28eac785"}
{"type":"assistant","message":{"content":[{"type":"text","text":"All tests pass."}]},"parent_tool_use_id":null,"session_id":"28eac785"}
{"is_error":false,"session_id":"28eac785","usage":{"input_tokens":2,"cache_creation_input_tokens":12529,"cache_read_input_tokens":15900,"output_tokens":4},"subtype":"success","result":"All tests pass.","type":"result"}"#;

/// A Codex thread that failed to authenticate. Captured from codex-cli 0.148.0.
pub const CODEX: &str = r#"Reading additional input from stdin...
{"type":"thread.started","thread_id":"01a03328-0c86-7f93-8c73-817d64e595c6"}
{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 2/5 (unexpected status 401 Unauthorized)"}
{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}}"#;

/// A Grok session that is not signed in. Captured from Grok Build 1.0.5.
pub const GROK: &str = r#"{"type":"error","message":"Not signed in. To authenticate without a browser, run:\n  grok login --device-code"}"#;

/// One transcript per harness, for a test that must cover all three.
pub fn every_harness() -> [(Kind, &'static str); 3] {
    [
        (Kind::Claude, CLAUDE),
        (Kind::Codex, CODEX),
        (Kind::Grok, GROK),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canned_transcript_decodes_to_something() {
        // A fixture that silently stopped parsing would make the TUI's tests
        // pass against an empty screen.
        for (kind, transcript) in every_harness() {
            assert!(!decode(kind, transcript).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn the_claude_fixture_covers_a_whole_turn() {
        let events = decode(Kind::Claude, CLAUDE);
        assert!(matches!(events.first(), Some(Event::Ready { .. })));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ToolStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ToolFinished { .. }))
        );
        assert!(events.iter().any(|e| matches!(e, Event::Said(_))));
        assert_eq!(events.last(), Some(&Event::Idle));
    }

    #[test]
    fn every_harness_ends_a_failed_turn_idle_or_not_at_all() {
        // The property the TUI depends on: a session is never left busy by a
        // turn that went wrong, because nothing else would ever unblock it.
        for (kind, transcript) in every_harness() {
            let events = decode(kind, transcript);
            let busy_forever = events.iter().any(|e| matches!(e, Event::Trouble(_)))
                && !events.iter().any(|e| matches!(e, Event::Idle))
                && kind != Kind::Grok;
            assert!(!busy_forever, "{kind:?} leaves a failed turn busy");
        }
    }
}
