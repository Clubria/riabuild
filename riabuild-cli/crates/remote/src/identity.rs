//! The key pair that proves who *we* are to one server, and the `ssh`
//! options every connection to it carries.
//!
//! The other trust decision — which key proves who the *server* is to us —
//! lives in `host_key.rs` and is deliberately not re-exported from here. The
//! two were once one file, and one module doc naming both is how they get
//! confused; nothing in this file has an opinion about whether the box
//! answering is the developer's.

use super::Remote;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, Ui};
use std::path::PathBuf;
use std::sync::Arc;

/// The `ssh` options every connection to this server uses.
///
/// `identities_only` is false for exactly one step — authorising the new key
/// (Task 16) — where an existing key or the agent is what proves who we are.
///
/// `carry` is an issued key that has proved it can sign in **and** that this
/// laptop's own key demonstrably cannot replace, because the server accepted the
/// line into `authorized_keys` and still refuses it. A managed SSH gateway does
/// exactly that. Where `carry` is `Some`, the connection offers both identities
/// and lets the server pick; where it is `None` — which is every run against an
/// ordinary server — nothing changes.
///
/// Note that a carried identity is added *beside* riabuild's own `-i`, never
/// instead of it. `IdentitiesOnly=yes` restricts the offer to the identities
/// named here, so both have to be named for either to be tried, and dropping
/// riabuild's own would silently give up the key that works everywhere else.
pub fn ssh_options(
    remote: &Remote,
    paths: &dyn Paths,
    identities_only: bool,
    carry: Option<&crate::issued::Working>,
) -> Vec<String> {
    let mut options = vec![
        "-p".to_string(),
        remote.port.to_string(),
        // The developer's own ~/.ssh/config is read by `ssh` regardless, and a
        // `Host` block there could redirect where this connects. "riabuild
        // never touches ~/.ssh" is only true with this flag.
        "-F".to_string(),
        "/dev/null".to_string(),
        "-o".to_string(),
        format!(
            "UserKnownHostsFile={}",
            paths.known_hosts_file().to_string_lossy()
        ),
        "-o".to_string(),
        "StrictHostKeyChecking=yes".to_string(),
        "-i".to_string(),
        key_path(remote, paths).to_string_lossy().into_owned(),
    ];
    if let Some(carry) = carry {
        options.push("-o".to_string());
        options.push(format!("IdentityAgent={}", carry.socket.to_string_lossy()));
        options.push("-i".to_string());
        options.push(carry.public_key_path.to_string_lossy().into_owned());
    }
    if identities_only {
        options.push("-o".to_string());
        options.push("IdentitiesOnly=yes".to_string());
    }
    options
}

/// Where this server's private key lives, keyed by [`Remote::hash`] so a
/// renamed saved server still finds the key it already has.
pub fn key_path(remote: &Remote, paths: &dyn Paths) -> PathBuf {
    paths.identity_dir().join(remote.hash())
}

/// The `-C` comment `ensure_key` puts on a freshly generated key.
///
/// `member_id` comes first and is what `remote::forget::forget_remote`'s
/// server-side cleanup greps `authorized_keys` for via
/// [`key_comment_marker`] — see `ensure_key`'s doc comment for why the member
/// id, not the login target, has to be the unique part.
pub fn key_comment(remote: &Remote, member_id: &str) -> String {
    format!("riabuild {member_id} {}:{}", remote.target(), remote.port)
}

/// The substring `forget_remote` greps `authorized_keys` for — a prefix of
/// [`key_comment`], shared so the two can never drift out of sync with each
/// other.
pub fn key_comment_marker(member_id: &str) -> String {
    format!("riabuild {member_id}")
}

/// Generates the key pair if this server does not have one yet.
///
/// Idempotent: a second call against the same `remote` finds the file
/// `ssh-keygen` left behind and returns immediately, without shelling out
/// again — `apply()` has to be safe to run twice, and this is the same rule.
///
/// `member_id` goes into the key's `-C` comment alongside the login target,
/// because `riabuild remote forget`'s server-side cleanup greps
/// `authorized_keys` for it. On a shared account every developer's comment
/// would otherwise carry the identical `user@host:port` (Task 15's original
/// shape), so forgetting one developer's key would delete everyone's line —
/// the member id is the one part of the comment that is unique per developer
/// rather than per server.
pub async fn ensure_key(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    member_id: &str,
) -> Result<PathBuf> {
    let path = key_path(remote, paths);
    // Repaired unconditionally, before the existence check — same order as
    // `keychain/file.rs`'s `ensure_private_dir`, so a world-readable directory
    // doesn't stay that way just because riabuild finds it already there.
    tokio::fs::create_dir_all(paths.identity_dir()).await?;
    set_private_dir(&paths.identity_dir()).await?;

    if tokio::fs::metadata(&path).await.is_ok() {
        // Found on a later run, not just written below — repair its mode
        // too, for the same reason.
        set_private_file(&path).await?;
        return Ok(path);
    }

    ui.working("SSH key", "generating one for this server");

    let output = runner
        .run(
            "ssh-keygen",
            &[
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                &key_comment(remote, member_id),
                "-f",
                &path.to_string_lossy(),
            ],
            &RunOptions::default(),
        )
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            format!("making an SSH key for {}", remote.name),
            "Check that ssh-keygen works on this machine, then run `riabuild remote` again.",
        )
        .command("ssh-keygen -t ed25519")
        .detail(output.stderr)
        .into());
    }
    // ssh-keygen itself chmods a freshly-written key to 0600 (verified
    // directly against a real binary under umask 022), but that guarantee
    // lives in another program, not this crate — repair it explicitly, same
    // as the branch above. `NotFound` is tolerated only here: this file's own
    // tests script a successful `ssh-keygen` via `FakeRunner`, which writes
    // no real file, so that is the one expected reason this call can fail —
    // anything else, a real chmod failure above all, still surfaces.
    match set_private_file(&path).await {
        Ok(()) => {}
        Err(error) if is_not_found(&error) => {}
        Err(error) => return Err(error),
    }
    ui.applied("SSH key");
    Ok(path)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
}

/// Locks a directory riabuild owns under `~/.riabuild` down to `0700`.
///
/// `pub(super)` rather than private because `host_key::pin` needs the same
/// treatment for `~/.riabuild/ssh` — a second copy over there would be two
/// definitions of one rule, and the mode that matters would be whichever
/// copy the reader happened to find. This is a filesystem-permissions
/// helper, not a trust decision, so sharing it does not put the two trust
/// concerns back into one place.
#[cfg(unix)]
pub(super) async fn set_private_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[allow(dead_code)]
#[cfg(not(unix))]
pub(super) async fn set_private_dir(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Pins a private key file at `0600` regardless of its prior mode — the same
/// "set explicitly, don't trust creation-time permissions" rule
/// `keychain/file.rs`'s `write_private_token` documents.
#[cfg(unix)]
async fn set_private_file(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(())
}

#[allow(dead_code)]
#[cfg(not(unix))]
async fn set_private_file(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
    use riabuild_ui::Ui;
    use std::sync::Arc;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    #[test]
    fn ssh_options_pin_riabuilds_own_known_hosts() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, true, None).join(" ");

        assert!(options.contains("-p 2222"), "{options}");
        assert!(options.contains("StrictHostKeyChecking=yes"), "{options}");
        assert!(options.contains("UserKnownHostsFile="), "{options}");
        assert!(options.contains(".riabuild/ssh/known_hosts"), "{options}");
        assert!(options.contains("IdentitiesOnly=yes"), "{options}");
        // riabuild ignores the developer's own ssh config outright.
        assert!(options.contains("-F /dev/null"), "{options}");
    }

    #[test]
    fn a_carried_identity_is_offered_beside_riabuilds_own_never_instead_of_it() {
        // On a server that will not honour `authorized_keys`, riabuild's own
        // key never works and the issued one always does. Naming only the
        // issued key would still connect — and would quietly give up the key
        // that works on every other server, including this one once whoever
        // runs it fixes the file.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let carried = crate::issued::Working {
            label: "prod-bastion".into(),
            socket: "/run/riabuild/sock".into(),
            public_key_path: "/run/riabuild/k1.pub".into(),
        };

        let options = ssh_options(&remote(), &paths, true, Some(&carried)).join(" ");

        assert!(
            options.contains("IdentityAgent=/run/riabuild/sock"),
            "{options}"
        );
        assert!(options.contains("-i /run/riabuild/k1.pub"), "{options}");
        // riabuild's own identity is still named, and `IdentitiesOnly=yes`
        // means only the identities named here are offered at all.
        assert!(
            options.contains(&key_path(&remote(), &paths).to_string_lossy().to_string()),
            "{options}"
        );
        assert!(options.contains("IdentitiesOnly=yes"), "{options}");
    }

    #[test]
    fn an_ordinary_server_carries_nothing_and_looks_exactly_as_it_did() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, true, None).join(" ");
        assert!(!options.contains("IdentityAgent"), "{options}");
    }

    #[test]
    fn the_authorising_step_does_not_pin_identities_only() {
        // The common cloud-VM case is a box that already trusts the developer's
        // existing key and has password auth disabled. That key is what
        // authorises the new one, so it must still be offered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, false, None).join(" ");
        assert!(!options.contains("IdentitiesOnly"), "{options}");
    }

    const MEMBER_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn the_key_comment_leads_with_the_member_id_not_just_the_login_target() {
        // On a shared account every developer's login target (`ada@box:22`)
        // is identical; the member id is what `forget_remote` can grep for
        // without also deleting a co-tenant's line.
        let comment = key_comment(&remote(), MEMBER_ID);
        assert!(
            comment.starts_with(&format!("riabuild {MEMBER_ID} ")),
            "{comment}"
        );
        assert!(comment.contains(&remote().target()), "{comment}");
        assert!(
            comment.contains(&key_comment_marker(MEMBER_ID)),
            "{comment}"
        );
    }

    #[tokio::test]
    async fn a_key_is_generated_once_and_reused() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        let ui = Ui::new(true);

        // First call generates. The fake does not write files, so simulate what
        // ssh-keygen would leave behind before the second call.
        let path = ensure_key(&remote(), &paths, fake.clone(), &ui, MEMBER_ID)
            .await
            .expect("generate");
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.contains("ssh-keygen -t ed25519")),
            "{:?}",
            fake.calls()
        );
        assert!(
            fake.calls().iter().any(|c| c.contains("-N ")),
            "the key must have no passphrase"
        );
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.contains(&format!("riabuild {MEMBER_ID}"))),
            "the key comment must carry the member id, for forget_remote to grep on: {:?}",
            fake.calls()
        );

        tokio::fs::write(&path, "PRIVATE").await.expect("write");
        let again = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        ensure_key(&remote(), &paths, again.clone(), &ui, MEMBER_ID)
            .await
            .expect("reuse");
        assert!(
            again.calls().is_empty(),
            "an existing key must not be regenerated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_key_directory_and_an_existing_key_are_locked_down() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let ui = Ui::new(true);

        // Simulate a key from a stale riabuild version, sitting at a looser
        // mode than this one would ever create.
        let path = key_path(&remote(), &paths);
        tokio::fs::create_dir_all(paths.identity_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(&path, "PRIVATE").await.expect("write");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .await
            .expect("loosen");

        let fake = Arc::new(FakeRunner::new());
        ensure_key(&remote(), &paths, fake, &ui, MEMBER_ID)
            .await
            .expect("reuse");

        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "an existing key must be repaired to 0600"
        );

        let dir_mode = tokio::fs::metadata(paths.identity_dir())
            .await
            .expect("stat dir")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }
}
