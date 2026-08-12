//! Which socket each end of the channel uses, and whether an address can hold
//! it.
//!
//! Two paths and one limit, kept together because the limit is the reason both
//! paths are chosen the way they are: `sockaddr_un.sun_path` is small, and every
//! way of running out of it ends with two developers sharing one socket.

use crate::Remote;
use anyhow::Result;
use riabuild_channel::socket_path_for_create;
use riabuild_ui::Failure;
use std::path::{Path, PathBuf};

/// The bytes a unix socket address has room for.
///
/// `sockaddr_un.sun_path` is 108 on Linux and 104 on macOS, and this is the
/// smaller of the two rather than a per-platform answer: the laptop and the
/// server can be different systems, the path has to fit on both, and a
/// `cfg!(target_os)` here would be a platform decision in a file that is not
/// allowed one. Over the limit `bind()` fails with `ENAMETOOLONG` — or, worse,
/// silently truncates, which puts two developers back on one socket by a route
/// nothing would ever diagnose.
pub(super) const SUN_PATH_MAX: usize = 104;

/// What the shim on the server connects to: one socket inside this developer's
/// own namespace, never the shared runtime directory.
///
/// Several developers share one Unix account on a server, so they share one uid
/// and therefore one `$XDG_RUNTIME_DIR`. Left to resolve its own path, every one
/// of them would land on the same `…/riabuild/channel.sock`, and Ada's `xclip`
/// would read Ben's laptop.
pub fn remote_socket(namespace: &str) -> String {
    format!("{namespace}/channel.sock")
}

/// One socket per server, in the directory `riabuild_channel` would have put a
/// single one in.
///
/// Not the shared `channel.sock`: a `riabuild remote` to a *second* server binds
/// that same name, and whichever of the two sessions ends first takes the socket
/// with it — leaving the other one's forward pointing at a path nothing answers
/// on, with no error printed anywhere. [`Remote::hash`] is the key the SSH
/// identity and the session markers already use.
///
/// Both calls go through `socket_path_for_create`, which is what creates the
/// parent at 0700 and refuses — never unlinks — a path that is a symlink or
/// belongs to another account.
pub(super) async fn local_socket(remote: &Remote) -> Result<PathBuf> {
    let shared = socket_path_for_create(None).await?;
    let ours = shared.with_file_name(format!("channel-{}.sock", remote.hash()));
    socket_path_for_create(Some(&ours.to_string_lossy())).await
}

/// Refuses a path a unix socket address cannot hold, with the arithmetic in the
/// message.
///
/// Legibility is the point. `bind()` answers this with `ENAMETOOLONG` at best
/// and a silent truncation at worst, and a truncated path is two developers on
/// one socket again — so the developer is told the length, the limit and the
/// path, rather than being left with a channel that never worked.
pub(super) fn fits(path: &Path, remote: &Remote) -> Result<()> {
    let bytes = path.as_os_str().len();
    if bytes <= SUN_PATH_MAX {
        return Ok(());
    }
    Err(Failure::new(
        format!(
            "the clipboard channel's socket path is {bytes} bytes, and a unix socket address \
             holds {SUN_PATH_MAX}"
        ),
        format!(
            "Nothing else about {} is affected. Paste needs a shorter home directory on that \
             server, or a shorter RIABUILD_CHANNEL_SOCKET.",
            remote.name
        ),
    )
    .detail(path.display().to_string())
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// The length that has to hold in production, pinned against the limit
    /// `bind()` enforces silently.
    ///
    /// The headroom is smaller than it looks, which is the finding worth
    /// keeping: a long-but-ordinary macOS home and a Convex member id spend 89
    /// of the 104 bytes. That is a fit, not a comfortable one — a server whose
    /// accounts live somewhere deeper than `/Users/<name>` would run out, which
    /// is exactly why `fits` refuses in bytes instead of leaving `bind()` to
    /// truncate the path onto a colleague's socket.
    #[test]
    fn a_namespaced_socket_path_fits_a_unix_socket_address() {
        const HEADROOM: usize = 8;
        let namespace = session::namespace(
            "/Users/alexandra.pemberton",
            "jh7dq3k2vv8n9x3m4k5j6h7g8f9d0s1a",
        );
        let socket = remote_socket(&namespace);

        assert!(socket.ends_with("/channel.sock"), "{socket}");
        assert!(
            socket.len() + HEADROOM <= SUN_PATH_MAX,
            "a realistic namespaced socket has to fit with room to spare: {} bytes of {SUN_PATH_MAX} — {socket}",
            socket.len()
        );
        fits(Path::new(&socket), &remote()).expect("fits");
    }

    #[test]
    fn a_path_over_the_limit_is_refused_in_bytes_rather_than_bound_and_truncated() {
        let long = format!("/home/{}/channel.sock", "d".repeat(SUN_PATH_MAX));
        let error = fits(Path::new(&long), &remote()).expect_err("over the limit");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(
            failure.attempting.contains(&SUN_PATH_MAX.to_string()),
            "the developer has to be told the limit: {}",
            failure.attempting
        );
        assert!(
            failure.action.contains("Nothing else"),
            "and that only paste is affected: {}",
            failure.action
        );
        assert!(
            failure.detail.contains("channel.sock"),
            "{}",
            failure.detail
        );
    }
}
