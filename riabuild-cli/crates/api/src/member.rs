//! Who riabuild-web says this machine belongs to.
//!
//! The member id is the one field everything on a server is namespaced by, so
//! it is validated as a UUID on the way in rather than trusted: it reaches a
//! directory name.

use anyhow::Result;
use riabuild_ui::Failure;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    #[serde(rename = "githubLogin")]
    pub github_login: String,
    /// Immutable and ours. Names this developer's directory on a server.
    /// Deliberately not `#[serde(default)]`: an identifier that half the
    /// deployments might not send is not an identifier.
    #[serde(rename = "memberId", deserialize_with = "uuid_only")]
    pub member_id: String,
    #[serde(rename = "firstName")]
    pub first_name: String,
    #[serde(rename = "lastName")]
    pub last_name: String,
    pub email: String,
    pub role: String,
    pub status: String,
}

/// Refuses anything that is not a lowercase, hyphenated UUID.
///
/// An empty or malformed `member_id` is worse than a missing one: it reaches a
/// remote command line, and an empty one collapses `~/.riabuild-remote/<member-id>`
/// to `~/.riabuild-remote`, which puts every developer in one namespace and
/// makes `forget`'s cleanup delete all of them.
fn uuid_only<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = String::deserialize(deserializer)?;
    let shaped = value.len() == 36
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit() && !character.is_ascii_uppercase(),
            });
    if shaped {
        Ok(value)
    } else {
        Err(D::Error::custom(format!("{value:?} is not a member id")))
    }
}

impl Member {
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.first_name, self.last_name);
        let name = name.trim();
        if name.is_empty() {
            format!("@{}", self.github_login)
        } else {
            name.to_string()
        }
    }
}

/// Pulls `Member` out of the `{ "member": { ... } }` envelope `/api/v1/me`
/// returns, reporting a decode failure as "the dashboard is stale" rather
/// than the raw serde error `main.rs` would otherwise print as an unnamed
/// bug. Kept as a standalone function — rather than inlined into `me()` —
/// specifically so a test can call the exact code `me()` runs instead of a
/// hand-copied stand-in that would silently stop matching it.
pub(crate) fn decode_member(envelope: serde_json::Value) -> Result<Member> {
    let member = envelope
        .get("member")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value::<Member>(member).map_err(|error| {
        Failure::new(
            "reading your riabuild profile",
            "Ask your team lead to deploy the dashboard — this riabuild is newer than it.",
        )
        .detail(error.to_string())
        .into()
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_without_a_name_still_has_something_to_greet() {
        let member = Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: String::new(),
            last_name: String::new(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: "active".into(),
        };
        assert_eq!(member.display_name(), "@ada");
    }

    #[test]
    fn a_member_payload_carries_the_member_id() {
        let member: Member = serde_json::from_str(
            r#"{"githubLogin":"ada","githubId":"1234","memberId":"550e8400-e29b-41d4-a716-446655440000",
                "firstName":"Ada","lastName":"Lovelace","email":"ada@clubria.dev",
                "role":"developer","status":"active"}"#,
        )
        .expect("payload should parse");
        assert_eq!(member.member_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn a_payload_without_a_member_id_is_refused() {
        // A deployment older than this binary. Failing here is correct: the
        // alternative is a namespace directory named after an empty string,
        // silently shared by every developer on a server.
        let parsed = serde_json::from_str::<Member>(
            r#"{"githubLogin":"ada","githubId":"1234","firstName":"Ada","lastName":"Lovelace",
                "email":"ada@clubria.dev","role":"developer","status":"active"}"#,
        );
        assert!(parsed.is_err(), "a missing memberId must not default");
    }

    #[test]
    fn a_member_id_that_is_not_a_uuid_is_refused() {
        for bad in [
            "",
            "../../etc",
            "not-a-uuid",
            "550E8400-E29B-41D4-A716-446655440000",
            " 550e8400-e29b-41d4-a716-446655440000",
            "550e8400-e29b-41d4-a716-446655440000 ",
        ] {
            let json = format!(
                r#"{{"githubLogin":"ada","githubId":"1","memberId":"{bad}","firstName":"A",
                    "lastName":"B","email":"a@b.c","role":"developer","status":"active"}}"#
            );
            assert!(
                serde_json::from_str::<Member>(&json).is_err(),
                "{bad:?} must not be accepted as a member id"
            );
        }
    }

    #[test]
    fn decode_member_reports_a_missing_member_id_as_a_stale_dashboard_not_a_bug() {
        // Calls the exact function `me()` calls — not a hand-copied
        // stand-in — so this test tracks `me()`'s real behavior. If `me()`
        // regressed to wrapping its whole body (including the propagated
        // `ApiError` from `interpret`) in one `map_err`, that regression
        // would not touch this function's signature or this test's call
        // site, which is exactly why the split into `decode_member` matters.
        // `loopback` drives the same failure through a real `me()` over HTTP;
        // this one stays because it is plain data in, `Result` out, and pins
        // the decode without a socket.
        let envelope = serde_json::json!({
            "member": {
                "githubLogin": "ada",
                "firstName": "Ada",
                "lastName": "Lovelace",
                "email": "ada@clubria.dev",
                "role": "developer",
                "status": "active",
            }
        });
        let error =
            decode_member(envelope).expect_err("a payload with no memberId must not decode");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("a decode failure must surface as a Failure, not an opaque error");
        assert!(failure.action.contains("deploy the dashboard"));
    }

    #[test]
    fn decode_member_reads_a_well_formed_envelope() {
        let envelope = serde_json::json!({
            "member": {
                "githubLogin": "ada",
                "memberId": "550e8400-e29b-41d4-a716-446655440000",
                "firstName": "Ada",
                "lastName": "Lovelace",
                "email": "ada@clubria.dev",
                "role": "developer",
                "status": "active",
            }
        });
        let member = decode_member(envelope).expect("a well-formed envelope should decode");
        assert_eq!(member.member_id, "550e8400-e29b-41d4-a716-446655440000");
    }
}
