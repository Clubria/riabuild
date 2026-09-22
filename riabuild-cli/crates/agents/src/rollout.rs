//! The model and reasoning effort of a Codex turn, which its stream never says.
//!
//! `codex exec --json` names neither: `thread.started` carries the thread id and
//! nothing else, and no later frame adds them — checked against codex-cli
//! 0.149.0 with `-c model_reasoning_effort=high`. Codex does write both down,
//! in the thread's own rollout, `$CODEX_HOME/sessions/YYYY/MM/DD/
//! rollout-<time>-<thread id>.jsonl`: every turn opens with a `turn_context`
//! line naming the model it resolved and, where one was set, the effort.
//!
//! That file is Codex's and not riabuild's, so this is read-only, bounded, and
//! allowed to find nothing. A pane that cannot learn the model says
//! `thinking… · codex` and no more; it never falls back to a default it
//! believes Codex has.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// How much of the end of a rollout is read. A turn's `turn_context` is written
/// as it begins, and this is read as it begins, so it is near the end; the
/// whole file is every tool output of the thread and can be tens of megabytes.
const TAIL: u64 = 1 << 20;

/// What a turn was started with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Context {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The rollout for `thread` under this Codex home, newest day first.
pub async fn find(home: &Path, thread: &str) -> Option<PathBuf> {
    let suffix = format!("-{thread}.jsonl");
    let mut level = vec![home.join("sessions")];
    // Year, month, day — each walked newest first, so a thread from this week
    // is found without listing last year.
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in &level {
            next.extend(children(dir, true).await);
        }
        level = next;
    }
    for day in level {
        if let Some(file) = children(&day, false)
            .await
            .into_iter()
            .find(|path| path.to_string_lossy().ends_with(&suffix))
        {
            return Some(file);
        }
    }
    None
}

async fn children(dir: &Path, dirs: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return found;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
        if is_dir == dirs {
            found.push(entry.path());
        }
    }
    found.sort();
    found.reverse();
    found
}

/// The newest `turn_context` in the last [`TAIL`] bytes of a rollout.
pub async fn read(path: &Path) -> Option<Context> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let length = file.metadata().await.ok()?.len();
    file.seek(std::io::SeekFrom::Start(length.saturating_sub(TAIL)))
        .await
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await.ok()?;
    last_turn_context(&String::from_utf8_lossy(&bytes))
}

/// The newest `turn_context` line in some rollout text.
///
/// `effort` is where 0.149.0 puts it; `collaboration_mode.settings.
/// reasoning_effort` carries the same value and is the fallback. A turn run
/// with no effort configured has neither, and that is reported as `None`.
pub fn last_turn_context(text: &str) -> Option<Context> {
    text.lines().rev().find_map(|line| {
        // Cheap test first: most lines are tool output, and parsing each one
        // to find out it is not this would be most of the work.
        if !line.contains("\"turn_context\"") {
            return None;
        }
        let frame: Value = serde_json::from_str(line).ok()?;
        if frame.get("type").and_then(Value::as_str) != Some("turn_context") {
            return None;
        }
        let payload = frame.get("payload")?;
        let text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        Some(Context {
            model: text(payload.get("model")),
            effort: text(payload.get("effort"))
                .or_else(|| text(payload.pointer("/collaboration_mode/settings/reasoning_effort"))),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `turn_context` written by codex-cli 0.149.0 for
    /// `codex exec --json -c model_reasoning_effort=high`, trimmed to the
    /// fields near the ones read here. The field names are as written.
    const TURN: &str = r#"{"timestamp":"2026-09-22T20:45:53.912Z","type":"turn_context","payload":{"turn_id":"01a0cade-2fdb-79a0-90a1-01dde427669f","cwd":"/tmp/wd","approval_policy":"never","model":"gpt-5.6-sol","personality":"pragmatic","collaboration_mode":{"mode":"default","settings":{"model":"gpt-5.6-sol","reasoning_effort":"high"}},"effort":"high","summary":"auto"}}"#;

    /// The same line from a turn with no effort configured, as an interactive
    /// session on 0.149.0 wrote it: the model and nothing else.
    const NO_EFFORT: &str = r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol","collaboration_mode":{"mode":"default","settings":{"model":"gpt-5.6-sol","reasoning_effort":null}}}}"#;

    #[test]
    fn a_turn_context_names_the_model_and_the_effort() {
        let text = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"model_provider\":\"openai\"}}}}\n{TURN}\n{{\"type\":\"response_item\",\"payload\":{{}}}}"
        );
        assert_eq!(
            last_turn_context(&text),
            Some(Context {
                model: Some("gpt-5.6-sol".into()),
                effort: Some("high".into()),
            })
        );
    }

    #[test]
    fn an_effort_nobody_set_is_left_out_rather_than_guessed() {
        assert_eq!(
            last_turn_context(NO_EFFORT),
            Some(Context {
                model: Some("gpt-5.6-sol".into()),
                effort: None,
            })
        );
    }

    #[test]
    fn the_newest_turn_wins() {
        let text = format!("{TURN}\n{NO_EFFORT}");
        assert_eq!(last_turn_context(&text).unwrap().effort, None);
    }

    #[test]
    fn a_rollout_with_no_turn_yet_says_nothing() {
        assert_eq!(last_turn_context("{\"type\":\"session_meta\"}"), None);
        assert_eq!(last_turn_context(""), None);
    }

    #[tokio::test]
    async fn the_rollout_is_found_by_its_thread_id_under_its_day() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/09/22");
        tokio::fs::create_dir_all(&day).await.unwrap();
        tokio::fs::create_dir_all(home.path().join("sessions/2026/09/21"))
            .await
            .unwrap();
        let file = day.join("rollout-2026-09-22T20-45-53-01a0cade-2f60.jsonl");
        tokio::fs::write(&file, format!("{TURN}\n")).await.unwrap();
        tokio::fs::write(day.join("rollout-2026-09-22T20-40-00-other.jsonl"), "")
            .await
            .unwrap();

        let found = find(home.path(), "01a0cade-2f60").await;
        assert_eq!(found.as_deref(), Some(file.as_path()));
        assert_eq!(
            read(&file).await.unwrap().model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(find(home.path(), "nope").await, None);
    }
}
