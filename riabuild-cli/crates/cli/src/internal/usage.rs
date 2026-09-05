//! `riabuild internal usage-flush` — sends what the status line spooled.
//!
//! Started **detached** by `internal statusline`, at most once a minute, and
//! never by a person. Nothing waits for it and nothing reads its output, which
//! shapes every decision in this file:
//!
//! - **It takes the lock non-blocking and gives up.** Three Claude Code windows
//!   on one laptop notice a stale spool in the same second; the one that wins is
//!   doing the work that makes the other two unnecessary. A blocking wait would
//!   park a blocking-pool thread per window to send what has already been sent.
//! - **Every failure is silent.** No riabuild session, no network, a 503, an
//!   expired token — warn nobody, leave the spool, try again in a minute. This
//!   runs beside an interactive Claude Code session that did not ask for it, and
//!   a provisioner that prints to a developer's terminal because a dashboard is
//!   down has turned a usage tracker into an outage.
//! - **The spool is only truncated once the server has taken it.** A flush that
//!   cannot reach riabuild-web must leave the samples where a later one will
//!   find them.
//!
//! Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use riabuild_api::usage::{MAX_SAMPLES, Sample};
use riabuild_paths::config;
use riabuild_paths::filelock::FileLock;
use riabuild_tasks::Ctx;

/// Exit code. Always zero: nobody reads it, and a non-zero exit from a detached
/// process is a line in a log nobody has either.
const QUIETLY: i32 = 0;

pub(crate) async fn flush(ctx: &mut Ctx) -> Result<i32> {
    let dir = ctx.paths.usage_dir();
    if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        return Ok(QUIETLY);
    }

    // Non-blocking, and a refusal is success: somebody else is already sending
    // these samples. `try_acquire` rather than `acquire` for the reason in the
    // module header.
    let Some(_lock) = FileLock::try_acquire(&ctx.paths.usage_lock_file()).await? else {
        return Ok(QUIETLY);
    };

    // Touched *before* the send and whatever the send does, so that the status
    // line's one-a-minute check paces attempts rather than successes. A laptop
    // that cannot reach riabuild-web retries once a minute; without this it
    // would spawn a process on every render for as long as the outage lasted.
    mark_attempted(&ctx.paths.usage_flushed_marker()).await;

    for spool in spools(&dir).await {
        flush_one(ctx, &spool).await;
    }
    Ok(QUIETLY)
}

/// Every `<account>.ndjson` in the usage directory.
///
/// Read from the directory rather than from the account list, so that a spool
/// belonging to an account `riabuild claude delete` has since removed is still
/// sent and its file cleaned up. Deciding from `config.json` instead would leave
/// that spool on disk for ever, which is the one outcome worse than sending it:
/// samples nobody collects any more, kept indefinitely.
///
/// This is also what carried the fleet across the end of opt-in collection: a
/// spool written for an account somebody had marked was flushed by the riabuild
/// that stopped asking, because it never consulted the mark.
async fn spools(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return found;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "ndjson") {
            found.push(path);
        }
    }
    // Stable, so a laptop with several accounts sends them in the same order
    // every time and a failure is reproducible.
    found.sort();
    found
}

async fn flush_one(ctx: &mut Ctx, spool: &Path) {
    let Ok(text) = tokio::fs::read_to_string(spool).await else {
        return;
    };
    let samples = compact(&text);
    if samples.is_empty() {
        // Nothing parseable in it, so there is nothing a later run could
        // recover either. Removing it is what stops a file of junk being
        // re-read on every flush for ever.
        let _ = tokio::fs::remove_file(spool).await;
        return;
    }

    // A session's own connect, not the caller's: this process was started by a
    // status line and has done none of the setup a normal run does. A laptop
    // with no session simply has nothing to send yet.
    // `connect` is soft — a laptop with no session in the keychain returns
    // `Ok` having done nothing — so the question is whether it produced a
    // member, not whether it errored.
    if ctx.connect().await.is_err() || ctx.member.is_none() {
        return;
    }

    for batch in samples.chunks(MAX_SAMPLES) {
        if riabuild_api::usage::send(&ctx.api, batch).await.is_err() {
            // Leave the whole spool. Re-sending a batch that did land is free —
            // the server merges by taking the larger value — and losing one that
            // did not is a measurement gone for good, so the flush errs towards
            // sending twice.
            return;
        }
    }

    // Only now. Truncated before the send, an unreachable dashboard would eat
    // every sample it was holding.
    let _ = tokio::fs::remove_file(spool).await;
}

/// One line per session, keeping the largest reading of each.
///
/// This is the same rule the server applies, applied early: `cost` and the
/// session counters are cumulative and reset on `/clear`, so the newest sample
/// for a session is the whole truth about it and the rest are prefixes.
/// Compacting here is what bounds the spool — a laptop offline for a week sends
/// as many lines as it had sessions, not as many as it had messages.
///
/// A line that does not parse is dropped rather than failing the file. The spool
/// is written by a `>>` from a script Claude Code may kill mid-render, so a
/// torn last line is an ordinary thing to meet, and one of them must not strand
/// every sample above it.
fn compact(text: &str) -> Vec<Sample> {
    let mut newest: HashMap<(String, String), Sample> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(sample) = serde_json::from_str::<Sample>(line) else {
            continue;
        };
        if sample.session_id.is_empty() || sample.account_id.is_empty() {
            continue;
        }
        let key = (sample.account_id.clone(), sample.session_id.clone());
        match newest.get_mut(&key) {
            Some(held) => merge(held, sample),
            None => {
                order.push(key.clone());
                newest.insert(key, sample);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|key| newest.remove(&key))
        .collect()
}

/// Keeps the larger of two readings of one session.
///
/// Larger rather than newer, for the cumulative metrics: two windows rendering
/// the same session can write samples out of order, and "newest wins" would let
/// a total walk backwards. The rate-limit percentages are the exception and
/// take the newer value — a five-hour window legitimately *falls* when it
/// resets, so a maximum there would pin a developer at their worst hour of the
/// day for ever.
fn merge(held: &mut Sample, incoming: Sample) {
    fn larger<T: PartialOrd>(held: &mut Option<T>, incoming: Option<T>) {
        match (&held, &incoming) {
            (Some(a), Some(b)) if b > a => *held = incoming,
            (None, Some(_)) => *held = incoming,
            _ => {}
        }
    }

    larger(&mut held.cost_usd, incoming.cost_usd);
    larger(&mut held.duration_ms, incoming.duration_ms);
    larger(&mut held.api_duration_ms, incoming.api_duration_ms);
    larger(&mut held.lines_added, incoming.lines_added);
    larger(&mut held.lines_removed, incoming.lines_removed);

    if incoming.model.is_some() {
        held.model = incoming.model;
    }
    if incoming.five_hour_pct.is_some() {
        held.five_hour_pct = incoming.five_hour_pct;
        held.five_hour_resets_at = incoming.five_hour_resets_at;
    }
    if incoming.seven_day_pct.is_some() {
        held.seven_day_pct = incoming.seven_day_pct;
        held.seven_day_resets_at = incoming.seven_day_resets_at;
    }
}

async fn mark_attempted(marker: &Path) {
    if let Some(parent) = marker.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    // The *contents* are never read — only the mtime — but something has to be
    // written for the mtime to move.
    let _ = config::write_atomic(marker, config::now_secs().to_string().as_bytes()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(session: &str, cost: f64) -> String {
        format!(
            r#"{{"harness":"claude","accountId":"acc","sessionId":"{session}","costUsd":{cost}}}"#
        )
    }

    /// The whole correctness of the tracker in one assertion.
    ///
    /// Six renders of one session are six cumulative readings of the same
    /// number, not six costs to add up. Summing them here would report $2.10
    /// for a session that cost $0.60, and it would do so more the busier the
    /// session was — so the error would be largest exactly where a lead was
    /// most likely to look.
    #[test]
    fn samples_for_one_session_collapse_to_the_largest_never_the_sum() {
        let spool = [line("s1", 0.10), line("s1", 0.25), line("s1", 0.60)].join("\n");

        let samples = compact(&spool);

        assert_eq!(samples.len(), 1, "one session is one row");
        assert_eq!(samples[0].cost_usd, Some(0.60));
    }

    /// Two windows can render the same session out of order.
    #[test]
    fn an_older_sample_arriving_late_does_not_walk_a_total_backwards() {
        let spool = [line("s1", 0.60), line("s1", 0.10)].join("\n");

        assert_eq!(compact(&spool)[0].cost_usd, Some(0.60));
    }

    /// A rate-limit window falls when it resets, so this one is *not* a maximum.
    #[test]
    fn a_rate_limit_percentage_follows_the_newest_reading_down() {
        let spool = [
            r#"{"harness":"claude","accountId":"a","sessionId":"s","fiveHourPct":91.0}"#,
            r#"{"harness":"claude","accountId":"a","sessionId":"s","fiveHourPct":3.5}"#,
        ]
        .join("\n");

        assert_eq!(compact(&spool)[0].five_hour_pct, Some(3.5));
    }

    /// Claude Code kills a status line script a newer render supersedes, so a
    /// half-written last line is ordinary rather than exotic.
    #[test]
    fn a_torn_line_is_dropped_without_stranding_the_samples_above_it() {
        let spool = format!(
            "{}\n{}\n{{\"harness\":\"cla",
            line("s1", 0.10),
            line("s2", 0.20)
        );

        let samples = compact(&spool);

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].session_id, "s1");
        assert_eq!(samples[1].session_id, "s2");
    }

    /// Two accounts on one laptop are two rows even for identical session ids.
    #[test]
    fn sessions_are_keyed_by_account_as_well_as_by_id() {
        let spool = [
            r#"{"harness":"claude","accountId":"work","sessionId":"s","costUsd":1.0}"#,
            r#"{"harness":"claude","accountId":"other","sessionId":"s","costUsd":2.0}"#,
        ]
        .join("\n");

        assert_eq!(compact(&spool).len(), 2);
    }

    /// A line naming no session is not a session.
    #[test]
    fn a_sample_with_no_session_id_is_dropped() {
        let spool = r#"{"harness":"claude","accountId":"a","sessionId":"","costUsd":1.0}"#;

        assert!(compact(spool).is_empty());
    }
}
