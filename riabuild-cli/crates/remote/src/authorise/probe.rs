//! What the server will accept, and what a refusal actually means.
//!
//! Everything here reads an `ssh` refusal rather than acting on one. Keeping
//! it apart from the flow is what makes "an empty method list is not the same
//! as publickey-only" a property of one small file: a host-key failure aborts
//! before any method is offered, so its stderr names none, and read as a
//! method list it becomes advice to paste a key that was never the problem.

use std::sync::Arc;

use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Failure;

use super::Remote;
use crate::host_key::entry_host;
use crate::ssh::Ssh;

/// What `ssh` really prints when the pinned key no longer matches.
#[cfg(test)]
pub(super) const HOST_KEY_CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
     @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
     IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
     Host key verification failed.";

/// The authentication methods sshd named in its refusal, e.g. `Permission
/// denied (publickey,password).` → `["publickey", "password"]`. Empty when
/// the failure was not an authentication refusal at all (a timeout, a closed
/// port) — there is no method list to read out of those.
pub fn offered_methods(stderr: &str) -> Vec<String> {
    let Some(start) = stderr.find("Permission denied (") else {
        return Vec::new();
    };
    let rest = &stderr[start + "Permission denied (".len()..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .map(|method| method.trim().to_string())
        .filter(|method| !method.is_empty())
        .collect()
}

/// Did `ssh` refuse over the *server's* identity rather than ours?
///
/// A host-key failure aborts the connection before any authentication method
/// is offered, so its stderr names none — and [`offered_methods`] reads an
/// empty list as "publickey only". Left to fall through, a stale pin (a VM
/// rebuilt with a new key, or a box recreated after `remote forget`, which
/// leaves the pin behind on purpose) is reported as a server that wants a key
/// pasted into `authorized_keys`. The developer pastes it, nothing changes,
/// and no riabuild command clears the pin that actually caused it.
///
/// Deliberately narrow: OpenSSH's two literals, and only when the stderr does
/// not also carry an authentication refusal — so a genuine `Permission denied
/// (publickey,password)` can never be swallowed by, say, a login banner that
/// quotes the phrase back at us.
pub fn host_key_failure(stderr: &str) -> bool {
    (stderr.contains("Host key verification failed")
        || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"))
        && !stderr.contains("Permission denied (")
}

/// The one wording for "`ssh` refused the *server's* identity, so this never
/// reached authentication at all". Shared rather than written at each site
/// that can observe it, so the remedy — which names riabuild's own
/// `known_hosts`, invisible to the developer under `-F /dev/null` — cannot
/// drift between them.
fn stale_pin(remote: &Remote, paths: &dyn Paths, stderr: String) -> anyhow::Error {
    Failure::new(
        format!("verifying {}'s host key", remote.host),
        format!(
            "ssh refused the host key riabuild has pinned for {}, so this never got as \
             far as authenticating — the key in `authorized_keys` is not the problem. If \
             that server was rebuilt or replaced, confirm its new fingerprint with \
             whoever runs it, then remove the {} line from {} and run `riabuild remote` \
             again.",
            remote.host,
            entry_host(remote),
            paths.known_hosts_file().display()
        ),
    )
    .detail(stderr)
    .into()
}

/// Can riabuild's own key sign in, without a password and without falling
/// back to the developer's own agent or default identities?
///
/// `Err` is narrower than "no". A host key that no longer matches the pin
/// aborts the connection at the host-key step, before any authentication
/// method is offered, so the honest answer is not "riabuild's key is not
/// authorised" but "that is not the server riabuild pinned" — and it is
/// diagnosed here, in the probe every path shares, rather than at one
/// caller. `riabuild remote --check` calls this *instead of* [`authorise`]
/// (nothing may write to the server on that path), so a diagnosis living
/// only inside `authorise` left `--check` against a rebuilt — or
/// impersonated — box printing "riabuild's key is not authorised there yet"
/// and exiting 0.
pub async fn can_sign_in(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<bool> {
    // No `carry`: the question is whether *riabuild's own* key signs in, and
    // an issued key answering it would report someone else's access as ours
    // and skip the install entirely.
    //
    // `without_askpass`, deliberately: this is one of the only two families of
    // ssh calls in remote mode that does not carry `askpass::run_options`. An
    // askpass that could answer a password prompt would let a saved password
    // make the answer yes on a server where the key does not work at all —
    // which is exactly the state the warning path exists to report.
    // `BatchMode=yes` already forbids every prompt, askpass included, so this
    // is belt and braces; it is a named method on the builder rather than an
    // absent line precisely because the belt used to be invisible, and someone
    // adding the askpass environment "for consistency" would break the probe
    // without breaking a single test.
    let probe = Ssh::to(remote, paths, runner)
        .option("BatchMode=yes")
        .without_askpass()
        .run("true")
        .await?;
    if host_key_failure(&probe.stderr) {
        return Err(stale_pin(remote, paths, probe.stderr));
    }
    Ok(probe.ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    #[test]
    fn the_methods_a_server_offers_are_read_from_its_refusal() {
        assert_eq!(
            offered_methods("ada@box: Permission denied (publickey,password)."),
            vec!["publickey".to_string(), "password".to_string()]
        );
        assert_eq!(
            offered_methods("Permission denied (publickey,keyboard-interactive)."),
            vec!["publickey".to_string(), "keyboard-interactive".to_string()]
        );
        assert_eq!(
            offered_methods("Permission denied (publickey)."),
            vec!["publickey".to_string()]
        );
        assert!(offered_methods("ssh: connect to host box port 22: Connection refused").is_empty());
    }

    #[test]
    fn a_host_key_failure_is_told_apart_from_an_authentication_refusal() {
        assert!(host_key_failure(HOST_KEY_CHANGED));
        // A first-connection refusal under StrictHostKeyChecking=yes prints
        // only the second literal.
        assert!(host_key_failure(
            "No ED25519 host key is known for box and you have requested strict checking.\n\
             Host key verification failed."
        ));
        assert!(!host_key_failure(
            "ada@box: Permission denied (publickey,password)."
        ));
        assert!(!host_key_failure(
            "ssh: connect to host box port 22: Connection refused"
        ));
        assert!(!host_key_failure(
            "Warning: Permanently added 'box' to the list of known hosts."
        ));
        // Narrow on purpose: a real refusal wins even where the phrase also
        // appears, so nothing chatty on stderr can mask a method list.
        assert!(!host_key_failure(
            "Host key verification failed.\nPermission denied (publickey,password)."
        ));
    }

    #[tokio::test]
    async fn a_changed_host_key_makes_the_probe_itself_fail_rather_than_answer_no() {
        // `can_sign_in` is not only `authorise`'s first step: `riabuild remote
        // --check` calls it *instead of* `authorise`, and reads `false` as
        // "riabuild's key is not authorised on that server yet" — a note, and
        // exit 0. So a probe that only reports `output.ok()` hands that
        // sentence, and a success exit code, to a developer whose server's
        // host key has changed under them.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake =
            Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 255, "", HOST_KEY_CHANGED));

        let error = can_sign_in(&remote(), &paths, fake)
            .await
            .expect_err("a refused host key is not an answer to `can this key sign in?`");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(
            failure.attempting.contains("host key"),
            "this has to read as a host-key problem: {}",
            failure.attempting
        );
        assert!(
            failure
                .action
                .contains(&paths.known_hosts_file().display().to_string()),
            "the remedy must name the file holding the stale pin: {}",
            failure.action
        );

        // The other direction, so this cannot pass by treating every failed
        // probe as a host-key problem: an ordinary refusal is still `false`.
        let denied = Arc::new(FakeRunner::new().with(
            "ssh -o BatchMode=yes",
            255,
            "",
            "Permission denied (publickey,password).",
        ));
        assert!(
            !can_sign_in(&remote(), &paths, denied)
                .await
                .expect("an authentication refusal is an answer, not an error")
        );
    }
}
