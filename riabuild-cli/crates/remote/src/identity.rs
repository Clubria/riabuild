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
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Seconds to wait for a connection to be established, per attempt.
///
/// Here rather than beside a call site, and that placement is the fix rather
/// than a tidy: this pair lived next to the two `shell.rs` handoffs, so the
/// other nine `ssh` calls a run makes had no bound at all and fell back to the
/// kernel's SYN retry — minutes each, on a server that is simply switched off.
/// The comment that put them there argued a probe should not wait three
/// minutes, which is right about the *keepalive* tolerance and backwards about
/// this: omitting a `ConnectTimeout` is what made those probes wait longest.
const CONNECT_TIMEOUT: u32 = 15;

/// A lost SYN is a retry, not a failed run. `ssh` sleeps a second between
/// attempts, so the bound above is what keeps this from doubling an
/// unreachable server's wait rather than merely surviving a dropped packet.
const CONNECTION_ATTEMPTS: u32 = 2;

/// One identity in an agent riabuild owns, offered *beside* riabuild's own key.
///
/// The triple this expands to — `IdentityAgent`, the public half as `-i`, and
/// the `IdentitiesOnly=yes` that makes naming both of them mean something —
/// was hand-assembled at three call sites, in two different orders. It is one
/// shape with one meaning: "try this key too, and nothing that was not named".
#[derive(Clone, Copy)]
pub(crate) struct Offered<'a> {
    pub(crate) socket: &'a Path,
    pub(crate) public_key_path: &'a Path,
}

impl<'a> From<&'a crate::issued::Working> for Offered<'a> {
    fn from(working: &'a crate::issued::Working) -> Self {
        Self {
            socket: &working.socket,
            public_key_path: &working.public_key_path,
        }
    }
}

/// The `ssh` options every connection to this server uses.
///
/// `pub(crate)`, and reached only through [`crate::ssh::Ssh`]: an option list
/// is half an invocation, and the half that used to be pasted together nine
/// different ways. Nothing outside this crate composes an `ssh` — the channel
/// supervisor is handed the finished list rather than building one.
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
pub(crate) fn ssh_options(
    remote: &Remote,
    paths: &dyn Paths,
    identities_only: bool,
    carry: Option<Offered<'_>>,
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
        // Every connection, not the two that remembered to ask for it.
        "-o".to_string(),
        format!("ConnectTimeout={CONNECT_TIMEOUT}"),
        "-o".to_string(),
        format!("ConnectionAttempts={CONNECTION_ATTEMPTS}"),
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
    // Created at 0700 and repaired unconditionally, before the existence
    // check: a directory this run makes is never briefly world-readable, and
    // one an older riabuild left open does not stay that way just because it
    // is already there.
    ensure_private_dir(&paths.identity_dir()).await?;

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

/// Creates a directory riabuild owns under `~/.riabuild`, private from the
/// first instant, and repairs the mode of one that is already there.
///
/// The three sites this replaces — here, `host_key::pin` and
/// `issued::agent::start` — each wrote `create_dir_all` and then
/// `set_private_dir`, which is right about the second run and wrong about the
/// first: `create_dir_all` applies the process umask, so a directory holding a
/// private key or a `known_hosts` existed world-readable for the width of two
/// syscalls before the chmod landed. The root `CLAUDE.md` states the rule for
/// the channel socket's parent — created **at** 0700 rather than created and
/// then chmod'd — and it is the same rule here.
///
/// Repairing afterwards is still needed and is not the same thing.
/// `create_dir_all` does not re-apply `mode` to a directory that already
/// exists, so one left open by an older riabuild, an admin script or a wide
/// umask would otherwise keep that mode for ever. `keychain/file.rs`'s
/// `ensure_private_dir` argues both halves at length.
///
/// What this deliberately does **not** do is `keychain/file.rs`'s third step:
/// opening the result with `O_NOFOLLOW | O_DIRECTORY` and checking the owner.
/// That is there because a server namespace is a predictable path under a home
/// directory every developer on the box can write to. These three live under
/// `~/.riabuild` on the laptop, whose root the same run created.
#[cfg(unix)]
pub(super) async fn ensure_private_dir(path: &std::path::Path) -> Result<()> {
    match tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .await
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    set_private_dir(path).await
}

#[cfg(not(unix))]
pub(super) async fn ensure_private_dir(path: &std::path::Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

/// Locks a directory riabuild owns under `~/.riabuild` down to `0700`.
///
/// Separate from [`ensure_private_dir`] above, which calls it: creating at the
/// right mode and repairing a mode already on disk are two different
/// guarantees, and only the second one applies to a directory somebody else
/// made.
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
        // The bound on the dial belongs to *every* connection — see the two
        // constants above, and `ssh.rs` for why it moved here.
        assert!(options.contains("ConnectTimeout=15"), "{options}");
        assert!(options.contains("ConnectionAttempts=2"), "{options}");
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

        let options = ssh_options(&remote(), &paths, true, Some((&carried).into())).join(" ");

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
