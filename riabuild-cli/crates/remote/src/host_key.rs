//! Which key proves who the *server* is to us, and where riabuild pins it.
//!
//! Split out of `identity.rs`, which now holds only the other half — the key
//! that proves who *we* are to the server. The two were never to be confused,
//! and a single file naming both concerns in one module doc is exactly how
//! they get confused: getting this one wrong is how a developer gets phished
//! by a box that isn't theirs, and that failure has nothing to do with the
//! key pair `ensure_key` generates.
//!
//! Nothing here is re-exported from `identity`. `identity::trust_host` would
//! keep compiling and the split would buy nothing.//!
//! [`scan`] reads what `ssh-keyscan` and `ssh-keygen` say; [`pin`] writes the
//! one line that records the answer. This file is the decision between them.

use super::Remote;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, Ui};
use std::sync::Arc;
mod pin;
mod scan;

use pin::pin;
use scan::fingerprints;
pub use scan::{KEY_TYPES, entry_host, fingerprint_of, preferred_key};

/// riabuild has a host key and cannot say what it is.
///
/// A stop rather than a warning: every caller of [`fingerprints`] is about to
/// either pin that key or compare it against one the developer named, and both
/// are decisions about a value riabuild does not have. The remedy is
/// `ssh-keygen`, because on every machine this has been seen on it was
/// `ssh-keygen` that was missing or broken — riabuild owns the tools it
/// installs, and OpenSSH is deliberately not one of them.
pub(super) fn unreadable(stderr: String) -> anyhow::Error {
    Failure::new(
        "reading that server's host key fingerprint",
        "riabuild will not pin a host key it cannot show you. Check that `ssh-keygen` \
         is installed and on your PATH, then run `riabuild remote` again.",
    )
    .command("ssh-keygen -lf -")
    .detail(stderr)
    .into()
}

/// The alarming case: a fingerprint named in advance did not match. That is
/// what a man-in-the-middle looks like, not a typo — R13's `SHA256:` prefix
/// check at the CLI layer already ruled a mistyped paste out.
///
/// The action names riabuild's own `known_hosts`, which is otherwise
/// invisible to the developer (`-F /dev/null`, a file under `~/.riabuild` no
/// command prints). Without it a server genuinely rebuilt with a new key is a
/// dead end nothing riabuild offers can clear.
fn mismatch(remote: &Remote, paths: &dyn Paths, detail: String) -> anyhow::Error {
    Failure::new(
        format!("verifying {}'s host key", remote.host),
        format!(
            "That does not match the fingerprint riabuild was given. This can mean the \
             server was rebuilt — or that something else is answering at that address. \
             Confirm the new fingerprint with whoever runs the server, and only once they \
             have, remove the {} line from {} and run `riabuild remote` again.",
            entry_host(remote),
            paths.known_hosts_file().display()
        ),
    )
    .detail(detail)
    .into()
}

/// Shows the server's host key and pins it.
///
/// `accept` is the value of `--accept-host-key`, already checked at the CLI
/// layer to start with `SHA256:` (a typo can never masquerade as a match).
/// When `Some`, the scanned key must match it exactly or this fails and pins
/// nothing. When `None`, the key riabuild scanned is pinned on sight: the
/// fingerprint is printed and the run carries on, rather than stopping on a
/// `[y/N]` that an unattended run cannot answer and an attended one answers
/// without checking.
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
    let host_field = entry_host(remote);
    // Whole first field, not a prefix: `starts_with` would treat two
    // genuinely different servers as already trusted and skip the prompt.
    // Compared case-insensitively, as `ssh` compares it — the line already in
    // the file carries the host spelled the way it was typed on the run that
    // pinned it, because that is what `ssh-keyscan` echoes back, so folding
    // only one side would still miss a match in the other direction.
    let pinned: Vec<&str> = existing
        .lines()
        .filter(|line| {
            line.split_whitespace().next().is_some_and(|field| {
                field
                    .split(',')
                    .any(|name| name.eq_ignore_ascii_case(&host_field))
            })
        })
        .collect();
    if !pinned.is_empty() {
        // An already-trusted host is not re-scanned on an ordinary run. But a
        // fingerprint named on the command line has to be compared against
        // *something*, and returning here regardless is how a stale pin — a
        // VM rebuilt with a new host key, or a box recreated after `remote
        // forget`, which deliberately leaves the pin behind — silently
        // disabled `--accept-host-key`: the flag was never read, `ssh` failed
        // at the host-key step three steps later, and nothing connected the
        // two.
        let Some(expected) = accept else {
            return Ok(());
        };
        // `?`, and the empty case is gone with it: `fingerprints` now refuses
        // rather than answering with nothing, so there is no branch here that
        // could report a pin as "an unreadable fingerprint" and still let the
        // comparison stand.
        let found = fingerprints(runner.as_ref(), &pinned.join("\n")).await?;
        if !found.iter().any(|seen| seen == expected) {
            return Err(mismatch(
                remote,
                paths,
                format!(
                    "expected {expected}, but {host_field} is already pinned as {}",
                    found.join(", ")
                ),
            ));
        }
        return Ok(());
    }

    let scan = runner
        .run(
            "ssh-keyscan",
            // Every type riabuild can pin, not one: a server offering only
            // some other type answers a single-type scan with nothing, and
            // "nothing" is indistinguishable here from a server that is not
            // there at all. Which of the answers gets pinned is decided by
            // `preferred_key`, and it is still exactly one.
            &[
                "-t",
                KEY_TYPES,
                "-p",
                &remote.port.to_string(),
                "-T",
                "5",
                &remote.host,
            ],
            &RunOptions::default(),
        )
        .await?;
    let keys: String = preferred_key(&scan.stdout).unwrap_or_default().to_string();
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

    // No fallback string here either, and that is the point of the `?`: what
    // this value is about to do is be printed to the developer as the thing
    // riabuild is trusting, or be compared against a fingerprint they named.
    // A placeholder standing in for a fingerprint riabuild could not read
    // makes both of those a lie, and the developer's only signal — the line
    // they were shown — reads as though the check happened.
    let found = fingerprints(runner.as_ref(), &keys).await?;
    let fingerprint = found
        .first()
        .cloned()
        .ok_or_else(|| unreadable(String::new()))?;

    // A supplied fingerprint answers the prompt without weakening it: it has to
    // match exactly, or this fails rather than prompting on a terminal that
    // may not exist (CI, a container test). There is no "accept anything" flag.
    if let Some(expected) = accept {
        if expected != fingerprint {
            return Err(mismatch(
                remote,
                paths,
                format!("expected {expected}, the server offered {fingerprint}"),
            ));
        }
        pin(paths, &known_hosts, &keys).await?;
        return Ok(());
    }

    // Trust on first sight, with no question asked — the shape of
    // `StrictHostKeyChecking=accept-new`. The fingerprint is still printed, so
    // it is in the transcript for a developer who has one to compare against,
    // but riabuild does not stop the run to collect an answer nobody was going
    // to check. What this gives up is only the *first* connection: the key is
    // pinned here, `identity::ssh_options` passes `StrictHostKeyChecking=yes`
    // against riabuild's own `known_hosts`, and every later run — including the
    // one where a server is impersonated — still fails hard on a key that
    // disagrees with the pin. `--accept-host-key` is unchanged above and is
    // still compared exactly.
    ui.note(&format!("fingerprint {fingerprint} — trusting it"));
    pin(paths, &known_hosts, &keys).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::{Decoration, Delegating, FakeRunner};
    use riabuild_ui::Ui;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    fn scan_stub(remote: &Remote, fingerprint_line: &str) -> FakeRunner {
        FakeRunner::new()
            .with(
                &format!(
                    "ssh-keyscan -t {KEY_TYPES} -p {} -T 5 {}",
                    remote.port, remote.host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote.host),
                "",
            )
            .with("ssh-keygen -lf -", 0, fingerprint_line, "")
    }

    /// What `ssh-keyscan` prints for a gateway that offers an RSA host key and
    /// nothing else — SSHPiper, and the hosted SSH front doors built on it.
    /// The banner comments are part of the shape: they are the only thing a
    /// single-type scan of such a server comes back with.
    fn rsa_only_scan(remote: &Remote) -> String {
        format!(
            "# {}:{} SSH-2.0-SSHPiper\n\
             {} ssh-rsa AAAArsakeydata\n\
             # {}:{} SSH-2.0-SSHPiper\n",
            remote.host, remote.port, remote.host, remote.host, remote.port
        )
    }
    #[tokio::test]
    async fn the_scan_asks_for_every_key_type_riabuild_can_pin() {
        // The bug, at its source. Scanning `-t ed25519` alone reports a server
        // that offers only an RSA host key as *unreachable* — the one failure
        // wording that sends a developer to check their hostname, their port,
        // and whether the server is running SSH at all, when the scan in fact
        // connected and was answered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        let fake = Arc::new(
            FakeRunner::new()
                .containing("ssh-keyscan", 0, &rsa_only_scan(&remote), "")
                .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""),
        );

        trust_host(
            &remote,
            &paths,
            fake.clone(),
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("a reachable server must be scanned, not called unreachable");

        let scan = fake
            .calls()
            .into_iter()
            .find(|call| call.starts_with("ssh-keyscan"))
            .expect("the host key must be scanned");
        for key_type in ["ed25519", "ecdsa", "rsa"] {
            assert!(
                scan.contains(key_type),
                "a scan that leaves out {key_type} cannot see a server offering only \
                 that type, and reports it as unreachable: {scan}"
            );
        }
    }
    #[tokio::test]
    async fn a_server_offering_only_an_rsa_host_key_is_pinned_rather_than_called_unreachable() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        let fake = Arc::new(
            FakeRunner::new()
                .containing("ssh-keyscan", 0, &rsa_only_scan(&remote), "")
                .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""),
        );

        trust_host(
            &remote,
            &paths,
            fake,
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("an RSA-only server is a server riabuild can pin");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("known_hosts written");
        assert!(contents.contains("ssh-rsa AAAArsakeydata"), "{contents}");
        assert!(
            !contents.contains('#'),
            "a banner comment is not a host key: {contents}"
        );
    }
    #[tokio::test]
    async fn only_the_one_key_the_developer_was_shown_is_pinned() {
        // Why the scan cannot simply pin everything it is answered with: the
        // fingerprint on screen is the *first* key's, so pinning the rest
        // beside it trusts keys nobody looked at. Scanning three types and
        // pinning one is what keeps "you approved exactly what got pinned"
        // true now that the scan is no longer restricted to a single type.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        let scan = format!(
            "{host} ssh-rsa AAAArsakeydata\n\
             {host} ecdsa-sha2-nistp256 AAAAecdsakeydata\n\
             {host} ssh-ed25519 AAAAed25519keydata\n",
            host = remote.host
        );
        let fake = Arc::new(
            FakeRunner::new()
                .containing("ssh-keyscan", 0, &scan, "")
                .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""),
        );

        trust_host(
            &remote,
            &paths,
            fake.clone(),
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("pins");

        let shown = fake
            .stdin_text_of("ssh-keygen -lf -")
            .expect("the key has to be fingerprinted before it is shown");
        assert_eq!(
            shown.lines().count(),
            1,
            "the developer is shown one fingerprint, so one key is what may be \
             fingerprinted: {shown}"
        );
        assert!(shown.contains("ssh-ed25519"), "{shown}");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("known_hosts written");
        assert_eq!(
            contents.lines().filter(|line| !line.is_empty()).count(),
            1,
            "exactly the approved key, and nothing else: {contents}"
        );
        assert!(
            contents.contains("ssh-ed25519 AAAAed25519keydata"),
            "{contents}"
        );
    }
    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";
    const GOOD_FINGERPRINT_LINE: &str =
        "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";

    /// Puts `remote` in riabuild's `known_hosts` the way an earlier run would
    /// have left it — including after `remote forget`, which deliberately
    /// leaves the pin behind.
    async fn pin_existing(paths: &RealPaths, remote: &Remote, key: &str) {
        tokio::fs::create_dir_all(paths.ssh_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            paths.known_hosts_file(),
            format!("{} ssh-ed25519 {key}\n", entry_host(remote)),
        )
        .await
        .expect("write");
    }
    #[test]
    fn the_known_hosts_field_is_case_folded_the_way_ssh_matches_it() {
        let mixed = Remote {
            host: "Build-01.Fly.Dev".into(),
            ..remote()
        };
        assert_eq!(entry_host(&mixed), "[build-01.fly.dev]:2222");
        assert_eq!(
            entry_host(&Remote {
                port: 22,
                ..mixed.clone()
            }),
            "build-01.fly.dev"
        );
        // Unchanged for a host already typed in lower case, which is what
        // every other test here and `authorise`'s remedy message rely on.
        assert_eq!(entry_host(&remote()), "[build-01.fly.dev]:2222");
    }
    #[tokio::test]
    async fn a_host_pinned_in_one_spelling_is_not_re_pinned_when_typed_in_another() {
        // DNS is case-insensitive and so is `ssh`'s own `known_hosts` lookup,
        // but `store::choose` lets the newest spelling win — so one capitalised
        // hostname used to fork the pin: re-scanned, re-prompted, and appended
        // beside the line that was already there, on every later run.
        //
        // Both directions, because folding only `entry_host` fixes just one:
        // the file may hold either spelling, since `ssh-keyscan` echoes back
        // whatever host it was given.
        for (pinned_as, typed_as) in [
            ("build-01.fly.dev", "Build-01.Fly.Dev"),
            ("Build-01.Fly.Dev", "build-01.fly.dev"),
        ] {
            let home = tempfile::TempDir::new().expect("tempdir");
            let paths = RealPaths::rooted_at(home.path());
            let remote = Remote {
                host: typed_as.into(),
                port: 22,
                ..remote()
            };
            tokio::fs::create_dir_all(paths.ssh_dir())
                .await
                .expect("mkdir");
            tokio::fs::write(
                paths.known_hosts_file(),
                format!("{pinned_as} ssh-ed25519 AAAAstubkeydata\n"),
            )
            .await
            .expect("write");

            // No stubs at all: reaching for `ssh-keyscan` — or for the prompt
            // that follows it, on a test process with no TTY — fails outright.
            let fake = Arc::new(FakeRunner::new());
            trust_host(&remote, &paths, fake.clone(), &Ui::new(true), None)
                .await
                .unwrap_or_else(|error| {
                    panic!("{typed_as} is already pinned as {pinned_as}: {error}")
                });

            let contents = tokio::fs::read_to_string(paths.known_hosts_file())
                .await
                .expect("read");
            assert_eq!(
                contents.lines().count(),
                1,
                "one server must not accumulate a pin per spelling: {contents}"
            );
        }
    }
    #[tokio::test]
    async fn an_already_trusted_host_is_not_scanned_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        pin_existing(&paths, &remote, "AAAAstubkeydata").await;

        // No stubs at all: any call to ssh-keyscan or ssh-keygen would fail
        // with "no stub for", proving neither ran. With no `--accept-host-key`
        // there is nothing to compare the pin against, so the short-circuit is
        // the whole behaviour — an ordinary run must stay offline.
        let fake = Arc::new(FakeRunner::new());
        trust_host(&remote, &paths, fake, &Ui::new(true), None)
            .await
            .expect("already trusted");
    }
    #[tokio::test]
    async fn an_already_pinned_host_matching_the_accepted_fingerprint_is_not_rescanned() {
        // The pin has to be *consulted*, not merely counted. The old
        // short-circuit returned before `accept` was read at all, so this
        // fails against it on the `ssh-keygen -lf -` assertion — and it is
        // also what stops the fix over-correcting into a re-scan of every
        // already-trusted host.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        pin_existing(&paths, &remote, "AAAAstubkeydata").await;

        // No `ssh-keyscan` stub: reaching for the network would fail with "no
        // stub for".
        let fake =
            Arc::new(FakeRunner::new().with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""));
        trust_host(
            &remote,
            &paths,
            fake.clone(),
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("the pinned key is the one named on the command line");

        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh-keygen -lf -")),
            "the pinned entry must actually be fingerprinted: {:?}",
            fake.calls()
        );
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("ssh-keyscan")),
            "an already-trusted host must not be re-scanned: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn a_stale_pin_disagreeing_with_the_accepted_fingerprint_is_refused() {
        // The C3 regression. `trust_host` used to return `Ok(())` for any
        // pinned host before reading `accept`, so the real new fingerprint of
        // a rebuilt server was compared against nothing at all; the run then
        // died in `authorise` looking like an authentication problem, and no
        // riabuild command clears the pin that caused it.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        pin_existing(&paths, &remote, "OLDSTALEKEYDATA").await;
        let fake =
            Arc::new(FakeRunner::new().with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""));

        let err = trust_host(
            &remote,
            &paths,
            fake,
            &Ui::new(true),
            Some("SHA256:0000000000000000000000000000000000000000"),
        )
        .await
        .expect_err("a fingerprint that disagrees with the pin must not report success");

        let failure = err.downcast_ref::<Failure>().expect("a Failure");
        assert!(
            failure.attempting.contains("verifying"),
            "the mismatch wording, not some other failure: {}",
            failure.attempting
        );
        assert!(
            failure.detail.contains("expected") && failure.detail.contains(GOOD_FINGERPRINT),
            "both fingerprints have to be shown: {}",
            failure.detail
        );
        assert!(
            failure
                .action
                .contains(&paths.known_hosts_file().display().to_string()),
            "the file holding the stale pin is invisible unless named: {}",
            failure.action
        );
        assert!(
            failure.action.contains(&entry_host(&remote)),
            "and so is the line inside it: {}",
            failure.action
        );

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("read");
        assert_eq!(
            contents.lines().count(),
            1,
            "a mismatch must neither pin nor rewrite: {contents}"
        );
    }
    #[tokio::test]
    async fn an_accepted_fingerprint_that_matches_pins_the_key_it_names() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
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
    async fn a_host_nobody_has_pinned_yet_is_trusted_without_being_asked_about() {
        // The `[y/N]` is gone: a scanned key is pinned on sight. `Ui::new(true)`
        // has no terminal under `cargo test`, so `confirm_required` would refuse
        // outright — reaching one is what this test fails on, and `accept` is
        // `None` so nothing else could be answering the question either.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        let fake = Arc::new(scan_stub(&remote, GOOD_FINGERPRINT_LINE));

        trust_host(&remote, &paths, fake, &Ui::new(true), None)
            .await
            .expect("a new host is trusted on sight, with no question to answer");

        let contents = tokio::fs::read_to_string(paths.known_hosts_file())
            .await
            .expect("known_hosts written");
        assert!(
            contents.contains("ssh-ed25519 AAAAstubkeydata"),
            "{contents}"
        );
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
    ///
    /// A [`Decoration`] rather than a hand-written `impl CommandRunner`: the
    /// injection is the whole of what this double does, and everything else is
    /// forwarding that `Delegating` owns. Written out by hand it was six
    /// bodies, one of which — `spawn_piped` — was missing, so that entry point
    /// fell through to the trait's refusing default rather than to the runner
    /// it wrapped, while the comment beside it said "delegated like the rest".
    struct InjectAConcurrentPin {
        known_hosts: PathBuf,
        injected: &'static str,
    }

    #[async_trait::async_trait]
    impl Decoration for InjectAConcurrentPin {
        async fn before(&self, program: &str, _: &[&str], _: &RunOptions) -> Result<()> {
            if program == "ssh-keyscan" {
                tokio::fs::write(&self.known_hosts, self.injected)
                    .await
                    .expect("inject a concurrent pin");
            }
            Ok(())
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

        let runner = Delegating::around(
            Arc::new(scan_stub(&remote, GOOD_FINGERPRINT_LINE)),
            InjectAConcurrentPin {
                known_hosts: paths.known_hosts_file(),
                injected: "otherhost ssh-ed25519 OTHERHOSTKEY\n",
            },
        );

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
                "ssh-keyscan -t {KEY_TYPES} -p {} -T 5 {}",
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
    /// `ssh-keyscan` answered and `ssh-keygen -lf -` did not — the two ways
    /// that can happen, as one table.
    ///
    /// A broken `ssh-keygen` (missing, refusing the key type) exits non-zero
    /// with a diagnostic; one handed something it parses but has no
    /// fingerprint for exits 0 with nothing useful on stdout. Both used to
    /// come back as an empty `Vec` that `trust_host` turned into the literal
    /// string `an unreadable fingerprint`.
    fn keygen_broken(remote: &Remote, code: i32, stdout: &str, stderr: &str) -> FakeRunner {
        FakeRunner::new()
            .with(
                &format!(
                    "ssh-keyscan -t {KEY_TYPES} -p {} -T 5 {}",
                    remote.port, remote.host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote.host),
                "",
            )
            .with("ssh-keygen -lf -", code, stdout, stderr)
    }
    #[tokio::test]
    async fn a_host_key_whose_fingerprint_cannot_be_read_is_refused_rather_than_pinned() {
        // The whole point of showing a fingerprint is that somebody can
        // compare it. riabuild used to substitute the words "an unreadable
        // fingerprint" for one it could not read, print `fingerprint an
        // unreadable fingerprint — trusting it`, and pin the key anyway — so
        // the developer's only signal that the check happened was a line that
        // said the check had not.
        for (code, stdout, stderr) in [
            (1, "", "ssh-keygen: not found"),
            // Exit 0 and nothing to read: the quieter half of the same bug.
            (0, "", ""),
            (0, "no SHA256 anywhere on this line\n", ""),
        ] {
            let home = tempfile::TempDir::new().expect("tempdir");
            let paths = RealPaths::rooted_at(home.path());
            let remote = remote();
            let fake = Arc::new(keygen_broken(&remote, code, stdout, stderr));

            let err = trust_host(&remote, &paths, fake, &Ui::new(true), None)
                .await
                .expect_err("a fingerprint riabuild cannot read is not one to trust");

            let failure = err
                .downcast_ref::<Failure>()
                .unwrap_or_else(|| panic!("an actionable Failure, not a bare error: {err}"));
            assert!(
                failure.attempting.contains("fingerprint"),
                "it has to say which check could not be made: {}",
                failure.attempting
            );
            assert!(
                failure.action.contains("ssh-keygen"),
                "and what to do about it: {}",
                failure.action
            );
            // The placeholder is gone, not merely unlikely.
            assert!(
                !format!("{err}").contains("unreadable fingerprint"),
                "{err}"
            );
            // And nothing was pinned: the file must not exist at all, so a
            // second run re-scans rather than trusting a key nobody saw.
            assert!(
                tokio::fs::metadata(paths.known_hosts_file()).await.is_err(),
                "an unreadable fingerprint must pin nothing"
            );
        }
    }
    #[tokio::test]
    async fn an_accepted_fingerprint_cannot_be_checked_against_a_pin_that_will_not_read() {
        // The other caller of `fingerprints`. A `--accept-host-key` compared
        // against an empty list can never match, so this already failed — but
        // it failed as a *mismatch*, telling the developer their server had
        // been rebuilt or replaced and sending them to confirm a new
        // fingerprint with whoever runs it. The machine is fine; `ssh-keygen`
        // is not.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let remote = remote();
        pin_existing(&paths, &remote, "STUBKEYDATA").await;
        let fake =
            Arc::new(FakeRunner::new().with("ssh-keygen -lf -", 1, "", "ssh-keygen: broken"));

        let err = trust_host(
            &remote,
            &paths,
            fake,
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect_err("a pin that cannot be read cannot be compared");

        let failure = err
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("an actionable Failure: {err}"));
        assert!(
            failure.attempting.contains("fingerprint"),
            "{}",
            failure.attempting
        );
        assert!(
            !failure.action.contains("rebuilt"),
            "this is not the man-in-the-middle wording: {}",
            failure.action
        );
    }
}
