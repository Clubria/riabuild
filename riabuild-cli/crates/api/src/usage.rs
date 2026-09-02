//! Sending this laptop's Claude Code usage samples to riabuild-web.
//!
//! The one endpoint riabuild *posts* measurements to, and the only place in this
//! crate whose request body says anything about what a developer did. Read the
//! two limits on that plainly, because both are load-bearing:
//!
//! **The member is never on the wire.** The request carries an account uuid and
//! a session id and no identity at all — who this is comes from the bearer token
//! the server already authenticated. A `memberId` field would be a claim the
//! client makes about itself, which is strictly weaker than the one the session
//! already proves, and it would put a *personal* Claude account's identity in a
//! payload that did not need it.
//!
//! **What is measured is volume, never content.** A sample is a cost, some
//! durations, a line count and two rate-limit percentages. The status line has
//! the repository, the model's prompt and the transcript path in hand when it
//! writes one, and sends none of them.
//!
//! Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ApiClient;

/// How many samples one request may carry.
///
/// The server rejects more, so this is the laptop's half of one agreed number
/// rather than a second opinion about it. A spool longer than this is sent in
/// batches; it only gets long on a laptop that has been unable to reach
/// riabuild-web for a while, and compaction keeps even that bounded by the
/// number of sessions rather than the number of messages.
pub const MAX_SAMPLES: usize = 200;

/// One session's usage, as the status line last observed it.
///
/// Every metric is `Option`, and the distinction is not decoration: an absent
/// cost means *unreported*, never free. Claude Code omits `cost` before the
/// first API response and omits `rate_limits` entirely for a login that is not
/// a Pro or Max subscription, and a `0` in either place would be a measurement
/// riabuild invented.
///
/// `#[serde(default)]` on the way in so that a spool written by an older
/// riabuild still parses when a field is added — the file outlives the process
/// that wrote it, and a flush that refused the whole line over one missing key
/// would strand every sample already on disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sample {
    /// `claude` today. On the wire from the first version so that Codex and
    /// Grok Build are a new producer rather than a migration.
    pub harness: String,
    /// The Claude config directory's uuid — riabuild's own name for the
    /// account, and deliberately not the account's email address.
    pub account_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Cumulative for the session, at list price. On a personal subscription
    /// this is not money anyone spent, which is why every rendering of it says
    /// "list-price equivalent".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    /// How much of the rolling five-hour window this subscription has used.
    /// Present only for a Pro or Max login, which is the whole fleet this was
    /// written for and is nonetheless not something to assume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_resets_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Accepted {
    #[allow(dead_code)]
    accepted: u32,
}

/// Sends one batch. The caller is responsible for keeping it under
/// [`MAX_SAMPLES`].
pub async fn send(api: &ApiClient, samples: &[Sample]) -> Result<()> {
    let _: Accepted = api
        .post_json("/api/v1/usage", serde_json::json!({ "samples": samples }))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam nothing else can see: this struct is serialised by Rust and
    /// narrowed by `parseSamples` in `riabuild-web/convex/http.ts`, and no
    /// compiler reads both. A field renamed on one side builds perfectly, passes
    /// every test in its own language, and arrives at the server as a key it
    /// ignores — so the sample lands with that metric silently missing and the
    /// dashboard is quietly wrong rather than broken.
    ///
    /// This is `every_generated_launcher_parses_back_into_the_plan_that_wrote_it`
    /// for the other cross-language seam riabuild has. It cannot run the
    /// TypeScript, so it pins the exact key set instead: change it here and the
    /// list below has to change too, which is the moment to change `parseSamples`
    /// and `usageSessions` with it.
    #[test]
    fn the_wire_carries_exactly_the_keys_the_server_narrows() {
        // Every field populated, so nothing is skipped by `skip_serializing_if`.
        let sample = Sample {
            harness: "claude".to_string(),
            account_id: "acc".to_string(),
            session_id: "s1".to_string(),
            model: Some("claude-opus-5".to_string()),
            cost_usd: Some(1.0),
            duration_ms: Some(1),
            api_duration_ms: Some(1),
            lines_added: Some(1),
            lines_removed: Some(1),
            five_hour_pct: Some(1.0),
            five_hour_resets_at: Some(1),
            seven_day_pct: Some(1.0),
            seven_day_resets_at: Some(1),
        };

        let value = serde_json::to_value(&sample).expect("serialises");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();

        // Read straight off `parseSamples` in `convex/http.ts`. `memberId` is
        // deliberately absent: the member comes from the authenticated session,
        // and a client-supplied one would be a weaker claim than the bearer
        // token already proves.
        let mut expected = [
            "harness",
            "accountId",
            "sessionId",
            "model",
            "costUsd",
            "durationMs",
            "apiDurationMs",
            "linesAdded",
            "linesRemoved",
            "fiveHourPct",
            "fiveHourResetsAt",
            "sevenDayPct",
            "sevenDayResetsAt",
        ];
        expected.sort_unstable();

        assert_eq!(keys, expected);
    }

    /// The batch size is one agreed number, not two opinions.
    ///
    /// The server refuses an over-long batch rather than truncating it — a
    /// silently dropped tail is a total that is quietly wrong — so a laptop that
    /// chunked at a larger number would simply stop being able to flush.
    #[test]
    fn the_batch_size_matches_what_the_server_accepts() {
        // `MAX_SAMPLES_PER_REQUEST` in `riabuild-web/convex/usage.ts`.
        assert_eq!(MAX_SAMPLES, 200);
    }

    /// The wire is camelCase, like every other `/api/v1` body, and an absent
    /// metric is an absent *key* rather than a null.
    ///
    /// The second half is the one worth a test: `null` and "not measured" would
    /// both deserialise to `None` here, but the server tells them apart when it
    /// merges, and a null that overwrote a real reading would lose a
    /// measurement riabuild had already sent.
    #[test]
    fn an_unmeasured_field_is_absent_rather_than_null() {
        let sample = Sample {
            harness: "claude".to_string(),
            account_id: "bbbbbbbb-2222-4333-8444-555555555555".to_string(),
            session_id: "abc123".to_string(),
            cost_usd: Some(0.25),
            ..Sample::default()
        };

        let json = serde_json::to_string(&sample).expect("serialises");

        assert!(json.contains(r#""accountId":"bbbbbbbb-2222-4333-8444-555555555555""#));
        assert!(json.contains(r#""costUsd":0.25"#));
        assert!(
            !json.contains("fiveHourPct"),
            "an unmeasured rate limit must not appear at all: {json}"
        );
        assert!(
            !json.contains("null"),
            "no field may be sent as null: {json}"
        );
    }

    /// A spool line written by an older riabuild still parses.
    ///
    /// The spool is a file on disk that outlives the process that wrote it, so
    /// a flush meets lines from whichever riabuild was installed when Claude
    /// Code rendered them.
    #[test]
    fn a_sample_missing_every_optional_field_still_parses() {
        let sample: Sample =
            serde_json::from_str(r#"{"harness":"claude","accountId":"abc","sessionId":"s1"}"#)
                .expect("parses");

        assert_eq!(sample.session_id, "s1");
        assert_eq!(sample.cost_usd, None);
        assert_eq!(sample.five_hour_pct, None);
    }
}
