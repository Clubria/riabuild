//! The one question this task asks GitHub — is this developer in the org? —
//! and the answers it can come back with.
//!
//! One variant per remedy, because the whole point of asking is to tell a
//! developer something they can act on. Reading `gh`'s stderr for the HTTP
//! status lives here too: it is what tells "your token expired" apart from
//! "your token may not read membership" apart from "the network is down", and
//! those three have nothing in common but the endpoint.

use super::ORG;
use crate::runner::RunOptions;
use crate::tasks::Ctx;
use anyhow::Result;

/// What GitHub says when asked whether this developer is in the org.
///
/// One variant per remedy: anything that collapses two of these together ends
/// up telling a developer to do something that cannot help.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Membership {
    Active,
    /// Invited, but the invite has not been accepted.
    Pending,
    /// GitHub answered, and the answer is no.
    NotAMember,
    /// The token is gone — expired, revoked, or signed out from under us.
    SignedOut,
    /// The token is valid but may not read organisation membership.
    Forbidden,
    /// Rate limit, outage, captive portal, corporate proxy.
    Unreadable(String),
}

impl Membership {
    pub(super) fn describe(&self) -> String {
        match self {
            Membership::Active => format!("you are an active member of {ORG}"),
            Membership::Pending => format!("your {ORG} invite has not been accepted yet"),
            Membership::NotAMember => {
                format!("GitHub does not report you as a member of {ORG}")
            }
            Membership::SignedOut => "your GitHub sign-in is no longer valid".into(),
            Membership::Forbidden => {
                format!("your GitHub token may not read {ORG} membership")
            }
            Membership::Unreadable(why) => {
                format!("could not check your {ORG} membership: {why}")
            }
        }
    }
}

/// Asks GitHub the only question this task actually cares about.
///
/// This replaced a test for the literal string `read:org` in `gh auth status`,
/// which asked a different question and got it wrong in both directions.
/// GitHub accepts `admin:org`, `read:org`, `repo`, `user`, or `write:org` here,
/// and folds `read:org` into `admin:org` when both are granted — so a developer
/// holding `admin:org` was told they lacked permission, sent through a browser
/// sign-in that could not add a scope they already had, and told to try again.
/// Forever: no run of `gh auth refresh` can make that string appear.
pub(super) async fn membership(ctx: &Ctx) -> Result<Membership> {
    let output = ctx
        .runner
        .run(
            "gh",
            &["api", &format!("/user/memberships/orgs/{ORG}")],
            &RunOptions::default(),
        )
        .await?;

    if output.ok() {
        // Tolerant of pretty-printed bodies: `gh api` emits compact JSON today,
        // and a formatting change should not read as "not a member".
        let body: String = output
            .stdout
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if body.contains(r#""state":"active""#) {
            return Ok(Membership::Active);
        }
        if body.contains(r#""state":"pending""#) {
            return Ok(Membership::Pending);
        }
        return Ok(Membership::Unreadable(
            "GitHub replied without a membership state".into(),
        ));
    }

    Ok(match http_status(&output.stderr) {
        Some(401) => Membership::SignedOut,
        Some(403) => Membership::Forbidden,
        // GitHub returns 404 rather than 403 when there is simply no
        // membership to report. The `NotAMember` remedy names the scope case
        // too, because this endpoint is the only evidence available here.
        Some(404) => Membership::NotAMember,
        _ => Membership::Unreadable(first_line(&output.stderr)),
    })
}

/// `gh` reports a failed API call as `gh: Not Found (HTTP 404)` on stderr.
fn http_status(stderr: &str) -> Option<u16> {
    stderr
        .split("(HTTP ")
        .nth(1)?
        .split(')')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh gave no explanation")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_http_status_is_read_out_of_ghs_message() {
        assert_eq!(http_status("gh: Not Found (HTTP 404)"), Some(404));
        assert_eq!(http_status("gh: Forbidden (HTTP 403)\n"), Some(403));
        assert_eq!(http_status("dial tcp: no such host"), None);
    }
}
