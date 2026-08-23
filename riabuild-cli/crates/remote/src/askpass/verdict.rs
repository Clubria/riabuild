//! What one `ssh` says about the credential it used.
//!
//! Read out of the exit status and stderr rather than tracked: `ssh` passes
//! the remote command's status back, so any status but its own means sshd
//! authenticated us, whatever the command then did.

/// The exit status `ssh` uses for its own failures, as opposed to passing back
/// the remote command's. Any *other* status is the server's script answering,
/// which means the credential riabuild used was accepted.
const SSH_ITSELF_FAILED: i32 = 255;

/// What one `ssh` says about the credential it used.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    /// sshd authenticated us — the remote command ran, whatever it exited
    /// with. A password given during this connection is riabuild's to keep.
    Accepted,
    /// sshd refused a password. Anything stored for this server is stale, and
    /// replaying it is how an account gets locked out.
    Rejected,
    /// `ssh` never got as far as an answer either way — a timeout, a closed
    /// port, a host key that did not match. Nothing is learned, so nothing is
    /// changed.
    Unanswered,
}

/// Reads that verdict out of one `ssh`'s exit status and stderr.
///
/// Pure, so the decision that governs whether a secret is kept is testable
/// without a server, a keychain or a terminal.
pub fn verdict(code: Option<i32>, stderr: &str) -> Verdict {
    if refused_a_password(stderr) {
        return Verdict::Rejected;
    }
    match code {
        Some(code) if code != SSH_ITSELF_FAILED => Verdict::Accepted,
        _ => Verdict::Unanswered,
    }
}

/// Did sshd refuse **a password**, as opposed to refusing a key?
///
/// Narrow on purpose, and the narrowness is the point. `Permission denied
/// (publickey)` on a keys-only server is riabuild's own key or an issued one
/// being turned away, and no password was offered at all — clearing a saved
/// password there would cost the developer a prompt for a secret that is
/// perfectly good. Only a refusal that *names* password or keyboard-interactive
/// among the methods sshd offered is a refusal of the thing this module holds.
///
/// [`super::authorise::offered_methods`] rather than a second reading of the
/// same stderr: two parsers of one sentence is how they come to disagree.
pub fn refused_a_password(stderr: &str) -> bool {
    crate::authorise::offered_methods(stderr)
        .iter()
        .any(|method| method == "password" || method == "keyboard-interactive")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_one_ssh_says_about_the_credential_it_used() {
        // The whole decision, as a table: this is what governs whether a
        // secret is kept, and it must be readable without a server.
        let denied = "ada@build-01.fly.dev: Permission denied (publickey,password).";
        assert_eq!(verdict(Some(255), denied), Verdict::Rejected);
        assert_eq!(
            verdict(Some(255), "Permission denied (keyboard-interactive)."),
            Verdict::Rejected
        );

        // Any exit status that is not ssh's own is the remote script's, which
        // means sshd let us in — including the script failing.
        assert_eq!(verdict(Some(0), ""), Verdict::Accepted);
        assert_eq!(verdict(Some(3), ""), Verdict::Accepted);
        assert_eq!(
            verdict(Some(1), "mkdir: Read-only file system"),
            Verdict::Accepted
        );

        // ssh's own failures teach nothing about the password either way.
        assert_eq!(
            verdict(Some(255), "Connection timed out"),
            Verdict::Unanswered
        );
        assert_eq!(verdict(None, ""), Verdict::Unanswered);

        // A key being turned away is not a password being turned away. An
        // issued key failing on a keys-only server must not cost the
        // developer a perfectly good saved password.
        assert_eq!(
            verdict(Some(255), "Permission denied (publickey)."),
            Verdict::Unanswered
        );
        assert!(!refused_a_password("Permission denied (publickey)."));
        assert!(!refused_a_password("Host key verification failed."));
        assert!(refused_a_password(denied));
    }
}
