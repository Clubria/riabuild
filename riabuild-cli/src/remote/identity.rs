//! The key pair for one server, and the host key riabuild agreed to trust.
//!
//! Two trust decisions live here, never to be confused: which key proves who
//! *we* are to the server (`ensure_key`, `ssh_options`), and which key
//! proves who the *server* is to us (`trust_host`) — getting that one wrong
//! is how a developer gets phished by a box that isn't theirs.

use super::Remote;
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The `ssh` options every connection to this server uses.
///
/// `identities_only` is false for exactly one step — authorising the new key
/// (Task 16) — where an existing key or the agent is what proves who we are.
pub fn ssh_options(remote: &Remote, paths: &dyn Paths, identities_only: bool) -> Vec<String> {
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
    // `keychain.rs`'s `ensure_private_dir`, so a world-readable directory
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

/// `SHA256:…` out of `ssh-keygen -lf` output.
pub fn fingerprint_of(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .find(|word| word.starts_with("SHA256:"))
        .map(str::to_string)
}

/// Shows the server's host key and pins it once the developer agrees.
///
/// `accept` is the value of `--accept-host-key`, already checked at the CLI
/// layer to start with `SHA256:` (a typo can never masquerade as a match).
/// When `Some`, it answers the trust question non-interactively: it must
/// match the scanned key exactly, or this fails rather than falling back to
/// a prompt with no terminal to show on. When `None`, a developer is asked.
pub async fn trust_host(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    accept: Option<&str>,
) -> Result<()> {
    let known_hosts = paths.known_hosts_file();
    let existing = tokio::fs::read_to_string(&known_hosts)
        .await
        .unwrap_or_default();
    let entry_host = if remote.port == 22 {
        remote.host.clone()
    } else {
        format!("[{}]:{}", remote.host, remote.port)
    };
    // Exact first field, not a prefix: `starts_with` would treat two
    // genuinely different servers as already trusted and skip the prompt.
    let already = existing.lines().any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|field| field.split(',').any(|name| name == entry_host))
    });
    if already {
        return Ok(());
    }

    let scan = runner
        .run(
            "ssh-keyscan",
            // One key type only: scanning all of them risks the developer
            // approving the RSA fingerprint while an unseen ed25519 key gets
            // pinned beside it — and a cloud console hands out the ed25519 one.
            &[
                "-t",
                "ed25519",
                "-p",
                &remote.port.to_string(),
                "-T",
                "5",
                &remote.host,
            ],
            &RunOptions::default(),
        )
        .await?;
    let keys: String = scan
        .stdout
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !scan.ok() || keys.is_empty() {
        // Unreachable: no fingerprint was ever shown, unlike the mismatch and
        // declined-prompt cases below.
        return Err(Failure::new(
            format!("reaching {} on port {}", remote.host, remote.port),
            "Check the hostname and port, and that the server is running SSH. \
             On a Mac, turn on System Settings → General → Sharing → Remote Login.",
        )
        .command(format!("ssh-keyscan -p {} {}", remote.port, remote.host))
        .detail(scan.stderr)
        .into());
    }

    let shown = runner
        .run(
            "ssh-keygen",
            &["-lf", "-"],
            &RunOptions {
                stdin: Some(keys.clone().into_bytes()),
                ..Default::default()
            },
        )
        .await?;
    let fingerprint =
        fingerprint_of(&shown.stdout).unwrap_or_else(|| "an unreadable fingerprint".to_string());

    // A supplied fingerprint answers the prompt without weakening it: it has to
    // match exactly, or this fails rather than prompting on a terminal that
    // may not exist (CI, a container test). There is no "accept anything" flag.
    if let Some(expected) = accept {
        if expected != fingerprint {
            // The alarming case: a fingerprint named in advance didn't match
            // what the server offered — what a man-in-the-middle looks like,
            // not a typo (the CLI's `SHA256:` prefix check ruled that out).
            return Err(Failure::new(
                format!("verifying {}'s host key", remote.host),
                "That does not match the fingerprint riabuild was given. This can mean the \
                 server was rebuilt — or that something else is answering at that address. \
                 Confirm the new fingerprint with whoever runs the server before trusting it \
                 again.",
            )
            .detail(format!(
                "expected {expected}, the server offered {fingerprint}"
            ))
            .into());
        }
        pin(paths, &known_hosts, &keys).await?;
        return Ok(());
    }

    ui.note(&format!("fingerprint {fingerprint}"));
    if !ui.confirm("is that the server you expected?")? {
        // Declined, not mismatched: no expected value was ever supplied to
        // compare against — worded as a next step, not an alarm.
        return Err(Failure::new(
            format!("trusting {}", remote.host),
            "Check the fingerprint with whoever runs that server, then run `riabuild remote` again.",
        )
        .into());
    }

    pin(paths, &known_hosts, &keys).await?;
    Ok(())
}

/// Appends a newly-trusted host key to riabuild's own `known_hosts`,
/// creating its directory (`0700`) if needed. Shared by the `accept` and
/// interactive paths so there is exactly one place that writes this file.
///
/// **Append-only, no read-modify-write.** A prior version re-read
/// `known_hosts` right before composing a full rewrite, which closed the
/// original stale-snapshot window but still let two genuinely concurrent
/// `pin` calls each read before either wrote (and its temp-file name,
/// process-id-only, collided between them regardless). `trust_host` only
/// ever calls this for a host with no existing entry — its `already` check
/// returns earlier otherwise — so there is nothing here to *replace*, only
/// to add, and `O_APPEND` has no read step to go stale: the kernel
/// atomically extends the file and places the write at the new end, so two
/// concurrent appenders on one local filesystem cannot overwrite each
/// other's bytes. (This assumes a local filesystem, true here under
/// `~/.riabuild`; `O_APPEND` is not atomic across clients on NFS.)
///
/// The one thing append can get wrong is gluing onto a line missing its
/// trailing `\n` (a hand-edited file) — guarded by leading with a newline
/// when the file already has bytes. A race on that check costs at most one
/// redundant blank line, which `ssh` ignores, never lost or corrupted data.
async fn pin(paths: &dyn Paths, known_hosts: &Path, keys: &str) -> Result<()> {
    tokio::fs::create_dir_all(paths.ssh_dir()).await?;
    set_private_dir(&paths.ssh_dir()).await?;
    let has_content = tokio::fs::metadata(known_hosts)
        .await
        .map(|meta| meta.len() > 0)
        .unwrap_or(false);
    let mut entry = String::new();
    if has_content {
        entry.push('\n');
    }
    entry.push_str(keys);
    entry.push('\n');
    append(known_hosts, entry.as_bytes()).await
}

/// Opens `path` for append (creating it if needed) and writes `bytes`,
/// flushed before returning — `write_all` alone only queues the bytes for a
/// blocking-pool task to actually write, the same gap `keychain.rs`'s
/// `write_private_token` was fixed for.
async fn append(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn set_private_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[allow(dead_code)]
#[cfg(not(unix))]
async fn set_private_dir(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Pins a private key file at `0600` regardless of its prior mode — the same
/// "set explicitly, don't trust creation-time permissions" rule
/// `keychain.rs`'s `write_private_token` documents.
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
    use crate::paths::RealPaths;
    use crate::runner::FakeRunner;
    use crate::ui::Ui;
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
        let options = ssh_options(&remote(), &paths, true).join(" ");

        assert!(options.contains("-p 2222"), "{options}");
        assert!(options.contains("StrictHostKeyChecking=yes"), "{options}");
        assert!(options.contains("UserKnownHostsFile="), "{options}");
        assert!(options.contains(".riabuild/ssh/known_hosts"), "{options}");
        assert!(options.contains("IdentitiesOnly=yes"), "{options}");
        // riabuild ignores the developer's own ssh config outright.
        assert!(options.contains("-F /dev/null"), "{options}");
    }

    #[test]
    fn the_authorising_step_does_not_pin_identities_only() {
        // The common cloud-VM case is a box that already trusts the developer's
        // existing key and has password auth disabled. That key is what
        // authorises the new one, so it must still be offered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, false).join(" ");
        assert!(!options.contains("IdentitiesOnly"), "{options}");
    }

    #[test]
    fn a_fingerprint_is_read_out_of_ssh_keygen_output() {
        let line = "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";
        assert_eq!(
            fingerprint_of(line).as_deref(),
            Some("SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y")
        );
        assert_eq!(fingerprint_of("").as_deref(), None);
        assert_eq!(fingerprint_of("nothing useful here").as_deref(), None);
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

    fn scan_stub(remote: &Remote, fingerprint_line: &str) -> FakeRunner {
        FakeRunner::new()
            .with(
                &format!(
                    "ssh-keyscan -t ed25519 -p {} -T 5 {}",
                    remote.port, remote.host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote.host),
                "",
            )
            .with("ssh-keygen -lf -", 0, fingerprint_line, "")
    }

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";
    const GOOD_FINGERPRINT_LINE: &str =
        "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";

    #[tokio::test]
    async fn an_already_trusted_host_is_not_scanned_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let ui = Ui::new(true);
        let remote = remote();

        tokio::fs::create_dir_all(paths.ssh_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            paths.known_hosts_file(),
            format!(
                "[{}]:{} ssh-ed25519 AAAAstubkeydata\n",
                remote.host, remote.port
            ),
        )
        .await
        .expect("write");

        // No stubs at all: any call to ssh-keyscan or ssh-keygen would fail
        // with "no stub for", proving neither ran.
        let fake = Arc::new(FakeRunner::new());
        trust_host(&remote, &paths, fake, &ui, None)
            .await
            .expect("already trusted");
    }

    #[tokio::test]
    async fn an_accepted_fingerprint_that_matches_pins_without_a_prompt() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        // A non-interactive Ui would refuse `confirm` outright — proving this
        // path never reaches it.
        let ui = Ui::new(true);
        let remote = remote();
        let fake = Arc::new(scan_stub(&remote, GOOD_FINGERPRINT_LINE));

        trust_host(&remote, &paths, fake, &ui, Some(GOOD_FINGERPRINT))
            .await
            .expect("matching fingerprint pins");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("known_hosts written");
        assert!(contents.contains("ssh-ed25519 AAAAstubkeydata"));

        let dir_mode = {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::metadata(paths.ssh_dir())
                .await
                .expect("stat")
                .permissions()
                .mode()
        };
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn an_accepted_fingerprint_that_does_not_match_is_refused_and_nothing_is_pinned() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let ui = Ui::new(true);
        let remote = remote();
        let fake = Arc::new(scan_stub(&remote, GOOD_FINGERPRINT_LINE));

        let err = trust_host(
            &remote,
            &paths,
            fake,
            &ui,
            Some("SHA256:0000000000000000000000000000000000000000"),
        )
        .await
        .expect_err("a mismatch must fail rather than fall back to prompting");

        let message = err.to_string();
        assert!(message.contains("verifying"), "{message}");
        let failure = err
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(
            failure.detail.contains("expected") && failure.detail.contains("offered"),
            "{}",
            failure.detail
        );
        assert!(
            tokio::fs::metadata(paths.known_hosts_file()).await.is_err(),
            "a mismatch must never write a key to known_hosts"
        );
    }

    /// Stands in for a second, concurrent `trust_host` call finishing its own
    /// `pin` while this one is still waiting on `ssh-keyscan` — deterministic,
    /// unlike relying on real task scheduling to land in the same window.
    struct InterleavedRunner {
        inner: FakeRunner,
        known_hosts: PathBuf,
        injected: &'static str,
    }

    #[async_trait::async_trait]
    impl CommandRunner for InterleavedRunner {
        async fn run(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<crate::runner::CommandOutput> {
            if program == "ssh-keyscan" {
                tokio::fs::write(&self.known_hosts, self.injected)
                    .await
                    .expect("inject a concurrent pin");
            }
            self.inner.run(program, args, options).await
        }

        async fn run_interactive(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<i32> {
            self.inner.run_interactive(program, args, options).await
        }

        fn which(&self, program: &str) -> Option<PathBuf> {
            self.inner.which(program)
        }
    }

    #[tokio::test]
    async fn a_host_pinned_while_another_scan_is_in_flight_is_not_lost() {
        // The lost-update race: `existing` was read once at `trust_host`
        // entry, then `ssh-keyscan` (up to 5s) and, on the interactive path,
        // an unbounded human prompt sit before `pin` composes a full rewrite
        // from that now-possibly-stale value. This simulates another
        // `trust_host` call, for a different host, completing its own `pin`
        // during our `ssh-keyscan` — its entry must still be there afterward.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let ui = Ui::new(true);
        let remote = remote();
        // The directory has to exist before the injected write can land in
        // it — in the real race this is guaranteed, because the "other"
        // `trust_host` call already got as far as pinning.
        tokio::fs::create_dir_all(paths.ssh_dir())
            .await
            .expect("mkdir");

        let runner = InterleavedRunner {
            inner: scan_stub(&remote, GOOD_FINGERPRINT_LINE),
            known_hosts: paths.known_hosts_file(),
            injected: "otherhost ssh-ed25519 OTHERHOSTKEY\n",
        };

        trust_host(
            &remote,
            &paths,
            Arc::new(runner),
            &ui,
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("trust");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("read");
        assert!(
            contents.contains("otherhost ssh-ed25519 OTHERHOSTKEY"),
            "a host pinned by another call while this one was scanning must survive: {contents}"
        );
        assert!(
            contents.contains("ssh-ed25519 AAAAstubkeydata"),
            "{contents}"
        );
    }

    #[tokio::test]
    async fn two_pins_for_different_hosts_running_concurrently_both_survive() {
        // Round-2 finding: a temp-file-plus-rename `pin` needs a name unique
        // per *call*, not merely per process — two concurrent `pin`s in one
        // process computed the identical temp path (keyed on
        // `std::process::id()` alone), so whichever `rename` landed second
        // silently discarded the first. `pin` was restructured to an
        // append-only write instead (see its doc comment) rather than
        // patched with a counter: there is no temp file, and no read, for a
        // second call to race against. Proven here with real concurrency —
        // `tokio::join!`, not a simulated interleave — because correctness no
        // longer depends on which one the scheduler happens to run first.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let known_hosts = paths.known_hosts_file();

        let (a, b) = tokio::join!(
            pin(&paths, &known_hosts, "hostA ssh-ed25519 AAAA"),
            pin(&paths, &known_hosts, "hostB ssh-ed25519 BBBB"),
        );
        a.expect("pin a");
        b.expect("pin b");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("read");
        assert!(contents.contains("hostA ssh-ed25519 AAAA"), "{contents}");
        assert!(contents.contains("hostB ssh-ed25519 BBBB"), "{contents}");
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_before_ever_asking_about_a_fingerprint() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let ui = Ui::new(true);
        let remote = remote();
        let fake = Arc::new(FakeRunner::new().with(
            &format!(
                "ssh-keyscan -t ed25519 -p {} -T 5 {}",
                remote.port, remote.host
            ),
            1,
            "",
            "ssh-keyscan: connect to host build-01.fly.dev port 2222: Connection timed out",
        ));

        let err = trust_host(&remote, &paths, fake, &ui, Some(GOOD_FINGERPRINT))
            .await
            .expect_err("an unreachable host must fail");

        let message = err.to_string();
        assert!(message.contains("reaching"), "{message}");
        // Distinct from the mismatch wording — no key was ever seen, so this
        // must not talk about what was "offered" or "expected".
        assert!(!message.contains("offered"), "{message}");
    }
}
