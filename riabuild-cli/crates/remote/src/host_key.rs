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
//! keep compiling and the split would buy nothing.

use super::Remote;
use super::identity::set_private_dir;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, Ui};
use std::path::Path;
use std::sync::Arc;

/// `SHA256:…` out of one line of `ssh-keygen -lf` output.
pub fn fingerprint_of(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .find(|word| word.starts_with("SHA256:"))
        .map(str::to_string)
}

/// The first field of this server's `known_hosts` line — bare for port 22,
/// `[host]:port` otherwise. Shared with `authorise`, which has to name the
/// exact line to remove when `ssh` refuses a stale pin.
///
/// Case-folded, like [`Remote::hash`] and like `ssh` itself: OpenSSH matches
/// `known_hosts` host patterns case-insensitively (verified against OpenSSH
/// 9.6 — `ssh-keygen -F Build-01.Fly.Dev` finds a `build-01.fly.dev` line and
/// vice versa), so one server typed two ways is one pinned line to `ssh` and
/// has to be one here too. `store::choose` lets the newest spelling win, so
/// typing `Build-01.Fly.Dev` once rewrites `record.host` permanently; a
/// case-sensitive first field then missed the line already in the file on
/// every later run — a re-scan, a fresh trust prompt, and another duplicate
/// entry appended each time.
pub fn entry_host(remote: &Remote) -> String {
    // ASCII-only, the same choice `Remote::hash` documents: hostnames are
    // ASCII or punycode, so there is no Unicode case-folding to get wrong.
    let host = remote.host.to_ascii_lowercase();
    if remote.port == 22 {
        host
    } else {
        format!("[{host}]:{}", remote.port)
    }
}

const UNREADABLE: &str = "an unreadable fingerprint";

/// What `ssh-keyscan -t` is asked for, and in [`PREFERRED`] order what riabuild
/// will pin out of the answer.
///
/// Not `ed25519` alone, which is what this was. A single-type scan cannot see a
/// server that offers only some *other* type, and riabuild has exactly one way
/// of reporting an empty scan: "reaching <host> on port <port>", which sends
/// the developer off to check their hostname, their port, and whether the box
/// is running SSH — for a server that answered on the first connection. SSHPiper,
/// which fronts several hosted SSH gateways, offers an RSA host key and nothing
/// else, so *every* riabuild remote behind one hit that dead end.
pub const KEY_TYPES: &str = "ed25519,ecdsa,rsa";

/// Best first. `ssh-keyscan` returns a line per type it was answered with, and
/// only one of them may be pinned — see [`preferred_key`].
const PREFERRED: [&str; 3] = ["ssh-ed25519", "ecdsa-sha2-", "ssh-rsa"];

/// The one line out of a scan that riabuild will show and pin.
///
/// Exactly one, and this is load-bearing rather than tidy. The developer is
/// shown a single fingerprint — the first key's — so pinning every line beside
/// it would trust keys nobody looked at: approve the RSA fingerprint you were
/// shown, and an unseen ed25519 key is pinned along with it. That risk is what
/// the single-type scan this replaces was avoiding; choosing here keeps the
/// property while letting the scan ask for every type, which is what an
/// RSA-only server needs.
///
/// Pinning one type is enough for `ssh` itself: OpenSSH reorders the host key
/// algorithms it offers to prefer what `known_hosts` already holds for that
/// host, so a server offering both keys will be asked for the pinned one.
///
/// A type not in [`PREFERRED`] still counts — `-t` cannot return one today, but
/// a future OpenSSH naming is a key riabuild saw and fingerprinted, not a
/// reason to declare a reachable server unreachable.
pub fn preferred_key(scan: &str) -> Option<&str> {
    let keys: Vec<&str> = scan
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    PREFERRED
        .iter()
        .find_map(|wanted| {
            keys.iter()
                .find(|line| {
                    line.split_whitespace()
                        .nth(1)
                        .is_some_and(|kind| kind.starts_with(wanted))
                })
                .copied()
        })
        .or_else(|| keys.first().copied())
}

/// Every fingerprint `ssh-keygen -lf -` reports for `keys`, which may be a
/// freshly scanned key or the lines already in `known_hosts`.
async fn fingerprints(runner: &dyn CommandRunner, keys: &str) -> Result<Vec<String>> {
    let shown = runner
        .run(
            "ssh-keygen",
            &["-lf", "-"],
            &RunOptions {
                stdin: Some(keys.as_bytes().to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(shown.stdout.lines().filter_map(fingerprint_of).collect())
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
        let found = fingerprints(runner.as_ref(), &pinned.join("\n")).await?;
        if !found.iter().any(|seen| seen == expected) {
            let shown = found.join(", ");
            return Err(mismatch(
                remote,
                paths,
                format!(
                    "expected {expected}, but {host_field} is already pinned as {}",
                    if shown.is_empty() { UNREADABLE } else { &shown }
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

    let found = fingerprints(runner.as_ref(), &keys).await?;
    let fingerprint = found
        .first()
        .cloned()
        .unwrap_or_else(|| UNREADABLE.to_string());

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

    ui.note(&format!("fingerprint {fingerprint}"));
    if !ui.confirm_required("is that the server you expected?")? {
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
/// blocking-pool task to actually write, the same gap `keychain/file.rs`'s
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

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
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

    #[test]
    fn the_best_key_of_several_is_the_one_chosen() {
        let scan = "host ssh-rsa AAAArsa\n\
                    host ecdsa-sha2-nistp256 AAAAecdsa\n\
                    host ssh-ed25519 AAAAed25519\n";
        assert_eq!(preferred_key(scan), Some("host ssh-ed25519 AAAAed25519"));
        assert_eq!(
            preferred_key("host ssh-rsa AAAArsa\nhost ecdsa-sha2-nistp256 AAAAecdsa\n"),
            Some("host ecdsa-sha2-nistp256 AAAAecdsa")
        );
        // The case this whole change exists for: nothing but RSA on offer.
        assert_eq!(
            preferred_key("# host:22 SSH-2.0-SSHPiper\nhost ssh-rsa AAAArsa\n"),
            Some("host ssh-rsa AAAArsa")
        );
        // Comments and blank lines are not keys, and a scan of nothing else
        // has no key to offer.
        assert_eq!(preferred_key("# host:22 SSH-2.0-OpenSSH_9.6\n\n"), None);
        assert_eq!(preferred_key(""), None);
        // A type riabuild has no opinion about is still a key it saw, and the
        // fingerprint shown is that line's own.
        assert_eq!(
            preferred_key("host ssh-newthing AAAAnew"),
            Some("host ssh-newthing AAAAnew")
        );
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
        ) -> Result<riabuild_runner::CommandOutput> {
            if program == "ssh-keyscan" {
                tokio::fs::write(&self.known_hosts, self.injected)
                    .await
                    .expect("inject a concurrent pin");
            }
            self.inner.run(program, args, options).await
        }

        // Delegated like the rest: this double exists only to inject a
        // concurrent `known_hosts` write in the middle of `ssh-keyscan`, so
        // every other entry point should behave exactly as the wrapped runner
        // does rather than acquire a second, divergent set of answers.
        async fn run_bytes(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<riabuild_runner::BytesOutput> {
            self.inner.run_bytes(program, args, options).await
        }

        async fn run_forking(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<i32> {
            self.inner.run_forking(program, args, options).await
        }

        async fn spawn(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<Box<dyn riabuild_runner::ChildHandle>> {
            self.inner.spawn(program, args, options).await
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
}
