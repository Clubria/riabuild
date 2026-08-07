//! The key pair for one server, and the host key riabuild agreed to trust.
//!
//! Two trust decisions live here and must never be confused: which key
//! proves who *we* are to the server (`ensure_key`, `ssh_options`), and
//! which key proves who the *server* is to us (`trust_host`). Getting the
//! second wrong is how a developer gets phished by a box that isn't theirs,
//! so `trust_host`'s errors read differently depending on why trust failed.

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

/// Generates the key pair if this server does not have one yet.
///
/// Idempotent: a second call against the same `remote` finds the file
/// `ssh-keygen` left behind and returns immediately, without shelling out
/// again — `apply()` has to be safe to run twice, and this is the same rule.
#[allow(dead_code)] // consumed by Task 16
pub async fn ensure_key(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
) -> Result<PathBuf> {
    let path = key_path(remote, paths);
    // Repaired unconditionally, before the existence check — the same order
    // `keychain.rs`'s `ensure_private_dir` uses: a directory left
    // world-readable by something else must not stay that way just because
    // riabuild finds it already there.
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
                &format!("riabuild {}:{}", remote.target(), remote.port),
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
    // ssh-keygen itself chmods a freshly-written private key to 0600; the
    // mode is repaired explicitly only on the branch above, where the key's
    // history is unknown.
    ui.applied("SSH key");
    Ok(path)
}

/// `SHA256:…` out of `ssh-keygen -lf` output.
#[allow(dead_code)] // consumed by Task 21, via trust_host
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
#[allow(dead_code)] // consumed by Task 21
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
        // Unreachable: no fingerprint was ever shown, so this must not read
        // like the mismatch or declined-prompt cases below, both of which
        // imply a key *was* seen.
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
            // The alarming case, worded differently from both others on
            // purpose: a fingerprint was named in advance and the server
            // offered a different one — what a man-in-the-middle looks like,
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
        pin(paths, &known_hosts, existing, &keys).await?;
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

    pin(paths, &known_hosts, existing, &keys).await?;
    Ok(())
}

/// Appends a newly-trusted host key to riabuild's own `known_hosts`,
/// creating its directory (`0700`) if needed. Shared by the `accept` and
/// interactive paths so there is exactly one place that writes this file.
#[allow(dead_code)] // consumed by Task 21, via trust_host
async fn pin(paths: &dyn Paths, known_hosts: &Path, existing: String, keys: &str) -> Result<()> {
    tokio::fs::create_dir_all(paths.ssh_dir()).await?;
    set_private_dir(&paths.ssh_dir()).await?;
    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(keys);
    contents.push('\n');
    // `tokio::fs::write` runs one synchronous `std::fs::write` on the
    // blocking pool and only returns once that has, so — unlike a manual
    // `File` + `write_all` — no separate flush is needed here.
    tokio::fs::write(known_hosts, contents).await?;
    Ok(())
}

#[allow(dead_code)] // consumed by Task 16 (ensure_key) and Task 21 (trust_host, via pin)
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
#[allow(dead_code)] // consumed by Task 16, via ensure_key
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

    #[tokio::test]
    async fn a_key_is_generated_once_and_reused() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        let ui = Ui::new(true);

        // First call generates. The fake does not write files, so simulate what
        // ssh-keygen would leave behind before the second call.
        let path = ensure_key(&remote(), &paths, fake.clone(), &ui)
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

        tokio::fs::write(&path, "PRIVATE").await.expect("write");
        let again = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        ensure_key(&remote(), &paths, again.clone(), &ui)
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
        ensure_key(&remote(), &paths, fake, &ui)
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
