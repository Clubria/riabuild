//! Reading a scan: which key types riabuild asks for, which of the answers it
//! prefers, and the fingerprint it shows a developer.
//!
//! All of it is about text `ssh-keyscan` and `ssh-keygen` produced. Nothing
//! here decides whether to trust anything or writes a line anywhere.

use anyhow::Result;
use riabuild_runner::{CommandRunner, RunOptions};

use super::unreadable;
use crate::Remote;

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
///
/// **An unreadable key is an error, never an empty list.** This used to drop
/// `ssh-keygen`'s exit status and its empty output on the floor, and the
/// consequence was the one thing this whole module exists to prevent: a
/// `ssh-keygen` that failed — not on `PATH`, refusing the key type, handed a
/// truncated scan — produced no fingerprint, the caller substituted the
/// literal string `an unreadable fingerprint`, and riabuild pinned the key
/// while telling the developer it had shown them what it was pinning. A
/// fingerprint riabuild could not read is a fingerprint nobody can compare, so
/// there is nothing here for a prompt or a `--accept-host-key` to be right
/// about.
pub(super) async fn fingerprints(runner: &dyn CommandRunner, keys: &str) -> Result<Vec<String>> {
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
    let found: Vec<String> = shown.stdout.lines().filter_map(fingerprint_of).collect();
    if !shown.ok() || found.is_empty() {
        return Err(unreadable(shown.stderr));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
