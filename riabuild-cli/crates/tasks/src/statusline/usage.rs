//! The usage sample this render appends, and the clock that decides when one is
//! sent.
//!
//! **Collection is automatic for every account riabuild manages.** It was opt-in
//! per account until 2026-09-05 — `riabuild claude track <n>`, a list of uuids
//! in `config.json`, and a launcher that handed the status line no spool path
//! for an account nobody had marked. What that actually produced was a fleet
//! reporting nothing, because the developer who never reads the release note is
//! also the developer who never runs the command. The gate that remains is the
//! one that was doing the real work all along, and it is a *fact* rather than a
//! setting: only an account under `<root>/claude/<uuid>` — one riabuild created,
//! numbered and launches — is ever spooled. A `CLAUDE_CONFIG_DIR` pointing
//! anywhere else, which is what a developer's own `~/.claude` install looks
//! like, writes nothing at all.
//!
//! What is measured is **volume, never content**: a cost, some durations, a line
//! count and two rate-limit percentages. This module has the repository, the
//! model's prompt and the transcript path within reach when it writes a sample,
//! and sends none of them.
//!
//! Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.

use riabuild_api::usage::Sample;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often starting a flush is worth it.
///
/// This runs about once per assistant message, which is far more often than a
/// usage dashboard needs, so the spool is *sent* at most once a minute and every
/// other render only appends.
const FLUSH_EVERY: Duration = Duration::from_secs(60);

/// The directory name that makes a `CLAUDE_CONFIG_DIR` one of riabuild's.
///
/// `Paths::claude_profile_dir` is `<root>/claude/<uuid>` and nothing else
/// produces that shape. Checking it is what keeps a developer's own
/// `CLAUDE_CONFIG_DIR=~/.claude` from having a spool invented for it two
/// directories above their home.
const ACCOUNTS_DIR: &str = "claude";

/// Where this account's samples are spooled, and what the account is called.
///
/// `<root>/claude/<uuid>` in, `<root>/usage/<uuid>.ndjson` and `<uuid>` out.
/// The file name *is* the account id, which is what lets a sample name its
/// account without anything having to pass it twice.
///
/// `None` for a `CLAUDE_CONFIG_DIR` that is not one of riabuild's — see
/// [`ACCOUNTS_DIR`] — and `None` when there is no variable at all.
pub fn spool_target(config_dir: &Path) -> Option<(PathBuf, String)> {
    let account = config_dir.file_name()?.to_str()?.to_string();
    let accounts = config_dir.parent()?;
    if accounts.file_name()?.to_str()? != ACCOUNTS_DIR {
        return None;
    }
    let root = accounts.parent()?;
    Some((
        root.join("usage").join(format!("{account}.ndjson")),
        account,
    ))
}

/// One usage sample, in the shape `POST /api/v1/usage` takes.
///
/// Only fields Claude Code documents as **cumulative for the session** are
/// carried, because the server merges samples by taking the larger of what it
/// holds and what arrives. A per-call figure merged that way would report the
/// largest single request rather than the session — a number that means nothing
/// and looks like one that does.
///
/// **No token count is collected, and that is deliberate.**
/// `context_window.total_input_tokens` reads like a session total and is
/// documented as the tokens *currently in the context window*: `0` before the
/// first response, and smaller again after every `/compact`. Merged by maximum
/// it would report the largest the context ever grew, under a column heading
/// that said "tokens". `cost.total_cost_usd` is the only cumulative measure of
/// volume the status line offers, so it is the one taken.
///
/// `None` for a payload naming no session: a sample that cannot say what it
/// measured is not a measurement.
pub fn sample_from_payload(payload: &Value, account_id: &str) -> Option<Sample> {
    let session_id = payload.get("session_id").and_then(Value::as_str)?;
    if session_id.is_empty() {
        return None;
    }

    let number =
        |parent: &str, key: &str| -> Option<f64> { payload.get(parent)?.get(key)?.as_f64() };
    let limit = |window: &str, key: &str| -> Option<f64> {
        payload.get("rate_limits")?.get(window)?.get(key)?.as_f64()
    };

    Some(Sample {
        harness: "claude".to_string(),
        account_id: account_id.to_string(),
        session_id: session_id.to_string(),
        model: payload
            .get("model")
            .and_then(|model| model.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        cost_usd: number("cost", "total_cost_usd"),
        duration_ms: number("cost", "total_duration_ms").map(|ms| ms as u64),
        api_duration_ms: number("cost", "total_api_duration_ms").map(|ms| ms as u64),
        lines_added: number("cost", "total_lines_added").map(|lines| lines as u64),
        lines_removed: number("cost", "total_lines_removed").map(|lines| lines as u64),
        five_hour_pct: limit("five_hour", "used_percentage"),
        five_hour_resets_at: limit("five_hour", "resets_at").map(|at| at as u64),
        seven_day_pct: limit("seven_day", "used_percentage"),
        seven_day_resets_at: limit("seven_day", "resets_at").map(|at| at as u64),
    })
}

/// Appends this render's sample, and says whether a flush is due.
///
/// Every failure is a quiet `false`. This is the render path of an interactive
/// session that did not ask riabuild for anything, and a provisioner that breaks
/// a developer's status line because a dashboard is unreachable has turned a
/// usage tracker into an outage.
pub(super) async fn collect(payload: &Value, config_dir: Option<&Path>) -> bool {
    let Some((spool, account)) = config_dir.and_then(spool_target) else {
        return false;
    };
    let Some(sample) = sample_from_payload(payload, &account) else {
        return false;
    };
    let Ok(line) = serde_json::to_string(&sample) else {
        return false;
    };

    if append(&spool, &line).await.is_err() {
        return false;
    }
    // Read here and *written by the flush itself*, so the clock paces attempts
    // rather than successes: a laptop that cannot reach riabuild-web retries
    // once a minute, where a marker moved only on success would spawn a process
    // on every render for as long as the outage lasted.
    flush_is_due(&spool.with_file_name("flushed")).await
}

/// One line, appended. Never a read-modify-write: several windows of one
/// developer render at once, and `O_APPEND` is what makes their lines interleave
/// rather than overwrite each other.
async fn append(spool: &Path, line: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = spool.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spool)
        .await?;
    file.write_all(format!("{line}\n").as_bytes()).await
}

async fn flush_is_due(marker: &Path) -> bool {
    let Ok(metadata) = tokio::fs::metadata(marker).await else {
        return true; // no marker yet: this machine has never flushed.
    };
    let Ok(modified) = metadata.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|since| since >= FLUSH_EVERY)
        // A marker in the future — a clock that has been put back, a file
        // copied off another machine — must not park the flush until it catches
        // up. Sending twice costs nothing; the server merges by maximum.
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::testing::namespace;
    use serde_json::json;

    /// A payload of the shape Claude Code documents, with the fields this
    /// collects and several it must ignore.
    fn payload() -> Value {
        json!({
            "session_id": "sess-1",
            "model": { "id": "claude-opus-5", "display_name": "Opus" },
            "workspace": { "current_dir": "/tmp", "project_dir": "/tmp" },
            "cost": {
                "total_cost_usd": 0.5,
                "total_duration_ms": 45_000,
                "total_api_duration_ms": 2_300,
                "total_lines_added": 156,
                "total_lines_removed": 23
            },
            "context_window": {
                "remaining_percentage": 92,
                "total_input_tokens": 15_500,
                "total_output_tokens": 1_200
            },
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 1_738_425_600u64 },
                "seven_day": { "used_percentage": 41.2, "resets_at": 1_738_857_600u64 }
            }
        })
    }

    /// The spool is derived from the account directory and nothing else, which
    /// is what lets one shared binary serve two developers on one server.
    #[test]
    fn the_spool_is_named_by_the_account_under_that_developers_own_root() {
        let (spool, account) =
            spool_target(Path::new("/home/dev/.riabuild-remote/m1/claude/abc")).unwrap();

        assert_eq!(account, "abc");
        assert_eq!(
            spool,
            Path::new("/home/dev/.riabuild-remote/m1/usage/abc.ndjson")
        );
    }

    /// The whole of the privacy answer, and the only gate left: an account
    /// riabuild did not create is not one riabuild collects from. A developer's
    /// own `CLAUDE_CONFIG_DIR=~/.claude` must not have a spool invented for it
    /// two directories above their home.
    #[test]
    fn a_claude_config_dir_that_is_not_riabuilds_has_no_spool() {
        assert_eq!(spool_target(Path::new("/home/dev/.claude")), None);
        assert_eq!(spool_target(Path::new("/home/dev/elsewhere/abc")), None);
    }

    #[tokio::test]
    async fn an_account_spools_one_line_per_render() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("acc-uuid", None)]);

        collect(&payload(), Some(&dirs[0])).await;
        collect(&payload(), Some(&dirs[0])).await;

        let spool = home.path().join("ns/usage/acc-uuid.ndjson");
        let written = std::fs::read_to_string(&spool).unwrap();
        let lines: Vec<_> = written.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 2, "one line per render: {written:?}");

        let sample: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(sample["harness"], "claude");
        assert_eq!(sample["sessionId"], "sess-1");
        // The file name *is* the account, so nothing has to be passed twice.
        assert_eq!(sample["accountId"], "acc-uuid");
        assert_eq!(sample["costUsd"], 0.5);
        assert_eq!(sample["fiveHourPct"], 23.5);
        assert_eq!(sample["sevenDayPct"], 41.2);
    }

    /// The fields that read like session totals and are not.
    ///
    /// `context_window.total_input_tokens` is documented as the tokens
    /// *currently in the window* — zero before the first response, smaller again
    /// after every `/compact` — so merged by maximum it would report peak
    /// context size under a heading that said "tokens". This is what stops it
    /// being added back because the payload obviously has it.
    #[test]
    fn no_token_count_is_ever_spooled() {
        let sample = sample_from_payload(&payload(), "acc").unwrap();
        let json = serde_json::to_string(&sample).unwrap();

        for forbidden in ["Token", "token", "15500", "1200"] {
            assert!(!json.contains(forbidden), "{forbidden} in {json}");
        }
    }

    /// Volume, never content. The repository, the directory and the model's
    /// display name are all in the payload and none of them are a measurement of
    /// how much was used.
    #[test]
    fn nothing_about_the_work_itself_is_spooled() {
        let sample = sample_from_payload(&payload(), "acc").unwrap();
        let json = serde_json::to_string(&sample).unwrap();

        for forbidden in ["workspace", "current_dir", "/tmp", "project_dir"] {
            assert!(!json.contains(forbidden), "{forbidden} in {json}");
        }
    }

    /// An absent cost means *unreported*, never free. Claude Code omits `cost`
    /// before the first API response and omits `rate_limits` entirely for a
    /// login that is not a Pro or Max subscription, and a `0` in either place
    /// would be a measurement riabuild invented.
    #[test]
    fn an_unmeasured_field_is_absent_rather_than_zero() {
        let sample = sample_from_payload(&json!({ "session_id": "s1" }), "acc").unwrap();
        let json = serde_json::to_string(&sample).unwrap();

        assert!(!json.contains("costUsd"), "{json}");
        assert!(!json.contains("fiveHourPct"), "{json}");
        assert!(!json.contains("null"), "{json}");
    }

    #[test]
    fn a_payload_with_no_session_is_not_a_sample() {
        assert!(sample_from_payload(&json!({}), "acc").is_none());
        assert!(sample_from_payload(&json!({ "session_id": "" }), "acc").is_none());
    }

    /// A `claude` the launchers did not start has no account directory, so it
    /// has nothing to spool — and must still not be a failure.
    #[tokio::test]
    async fn a_session_with_no_account_directory_spools_nothing() {
        assert!(!collect(&payload(), None).await);
    }

    /// The clock paces attempts. A flush that has just run must not be started
    /// again by the next render, and one that has never run must not wait.
    #[tokio::test]
    async fn a_flush_is_due_once_a_minute_and_not_once_a_render() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("acc-uuid", None)]);

        assert!(
            collect(&payload(), Some(&dirs[0])).await,
            "a machine that has never flushed is due"
        );

        // What `internal usage-flush` writes when it starts. Until it does, the
        // next render is still due — an attempt that never happened must not be
        // recorded by the renderer that asked for it.
        std::fs::write(home.path().join("ns/usage/flushed"), "0").unwrap();

        assert!(
            !collect(&payload(), Some(&dirs[0])).await,
            "a flush a moment old is not due again"
        );
    }
}
