//! Revoking the server's riabuild session, and the one failure that is not one.

use anyhow::Result;
use riabuild_api::ApiError;
use riabuild_ui::{Failure, Ui};

use super::api::Revokes;

/// Step 1 of [`forget_remote`]: revoke this server's session.
pub(super) async fn revoke_session(revokes: &dyn Revokes, ui: &Ui, session_id: &str) -> Result<()> {
    match revokes.revoke(session_id).await {
        Ok(()) => Ok(()),
        Err(error) if already_revoked(&error) => {
            // Not silent: see [`already_revoked`] for why this answer is
            // genuinely ambiguous and why the ambiguity cannot be resolved
            // from this side.
            ui.warn(&format!(
                "riabuild-web does not recognise this server's session ({session_id}), so it \
                 is being treated as already revoked. If this laptop has signed in as a \
                 different member since that session was minted, the token may still be \
                 live — check the sessions list on the dashboard."
            ));
            Ok(())
        }
        Err(error) => Err(Failure::new(
            "revoking this server's riabuild session",
            "Check your network connection, then run `riabuild remote forget` again — \
             until this succeeds, the token this laptop minted is still live on the server.",
        )
        .detail(error.to_string())
        .into()),
    }
}

/// Whether an error from [`revoke_session`]'s call means the session is
/// already gone rather than that the call itself failed.
///
/// Treating "already gone" as success is what stops a retry after a
/// half-finished `forget` getting stuck forever: the goal ("no live token")
/// holds whether this laptop revoked it or something else did — another
/// laptop's `forget`, an admin, natural expiry.
///
/// **The honest caveat, which this function cannot close.** riabuild-web
/// deliberately answers `session_unknown` for a session that exists but
/// belongs to a *different* member, so that session ids cannot be probed for
/// existence by whoever holds one. So `session_unknown` means "gone, or not
/// yours" — and the second reading is a live token. A hand-edited
/// `remotes.json`, or an account switch on this laptop, reaches it. Nothing
/// on the wire distinguishes the two, and trying to would defeat the
/// endpoint's design, so [`revoke_session`] warns instead: the developer,
/// unlike this process, can look at the dashboard's session list and tell.
fn already_revoked(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApiError>()
        .is_some_and(|api_error| api_error.code == "session_unknown")
}

/// An `ApiError` the way riabuild-web sends one, for the tests either side of
/// this module that feed one to code reading its `code`.
#[cfg(test)]
pub(crate) fn api_error(code: &str, status: u16) -> ApiError {
    ApiError {
        status,
        code: code.into(),
        message: "x".into(),
        action: "y".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn a_session_unknown_error_reads_as_already_revoked_not_a_failure() {
        // Someone else already forgot this server — another laptop, an admin,
        // natural expiry. The goal ("no live token") already holds, so this
        // must not block a retry that would otherwise never find anything to
        // revoke on the second attempt.
        let error: anyhow::Error = api_error("session_unknown", 404).into();
        assert!(already_revoked(&error));
    }

    #[test]
    fn any_other_failure_is_not_mistaken_for_already_revoked() {
        let upstream: anyhow::Error = api_error("upstream_error", 503).into();
        assert!(!already_revoked(&upstream));

        let transport = anyhow!("riabuild could not reach riabuild-web");
        assert!(!already_revoked(&transport));
    }
}
