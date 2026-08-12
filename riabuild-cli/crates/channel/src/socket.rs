//! Where the channel socket is, and who is allowed to own it.
//!
//! Two questions, deliberately kept apart, because only one of them can be
//! answered usefully. The shim and `riabuild channel open` **read** a path the
//! other end already created: they run on every Ctrl+V, they cannot repair
//! anything they dislike, and a stat-and-verify chain there would cost every
//! paste and change no outcome — a socket they refuse to talk to and a socket
//! that is not there are the same degraded channel. The agent (and, once it
//! lands, the supervisor) **creates and binds**, and that side is the only one
//! whose refusal means anything, because it is the side about to hand a
//! stranger the developer's clipboard.
//!
//! So: `socket_path` stays cheap and infallible, `socket_path_for_create` is
//! async and can fail. Collapsing the two would either make Ctrl+V pay for
//! checks it cannot act on, or quietly drop the checks from the side that can.

use anyhow::{Context, Result};
use riabuild_gh_session::choose_runtime_dir;
use riabuild_ui::Failure;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The environment variable the shim reads to find the channel.
///
/// Set by remote mode in the environment shell. Its absence is how a local
/// session — where the clipboard is already the developer's own — leaves the
/// real tools alone. Set explicitly it wins over the runtime directory on both
/// sides: it is how remote mode hands the server the path its forward lands on.
pub const SOCKET_ENV: &str = "RIABUILD_CHANNEL_SOCKET";

/// Everything riabuild puts in the runtime directory goes under one name, so a
/// mode and an owner can be asserted about a single directory rather than about
/// every file the channel might come to need.
const RUNTIME_SUBDIR: &str = "riabuild";
const SOCKET_FILE: &str = "channel.sock";

/// Where the shim should look for the channel.
///
/// The read side: no `stat`, no ownership check, no failure mode. See the
/// module doc for why the checks live only on the creating side.
pub fn socket_path() -> PathBuf {
    socket_path_from(
        std::env::var(SOCKET_ENV).ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
}

/// The read-side answer with the environment supplied rather than read, so a
/// test can drive it without mutating a process-wide variable.
fn socket_path_from(explicit: Option<&str>, xdg: Option<&str>, tmpdir: Option<&str>) -> PathBuf {
    if let Some(explicit) = non_empty(explicit) {
        return PathBuf::from(explicit);
    }
    // Deliberately *not* `choose_runtime_dir`: that one asks the filesystem
    // which candidate exists and is writable, and the reader has nothing to do
    // with either answer. It needs the same arithmetic the creator did, which
    // is the first non-empty name — if the creator's chosen directory did not
    // exist the connect fails anyway, and it fails the same way it does when
    // the laptop has simply closed its lid.
    let runtime = non_empty(xdg)
        .or_else(|| non_empty(tmpdir))
        .unwrap_or("/tmp");
    PathBuf::from(runtime)
        .join(RUNTIME_SUBDIR)
        .join(SOCKET_FILE)
}

/// Where the agent should create the channel, with the path checked.
///
/// `explicit` is `--socket` on the command line; it and `RIABUILD_CHANNEL_SOCKET`
/// both win over the runtime directory, in that order, exactly as they did when
/// this was one infallible function.
pub async fn socket_path_for_create(explicit: Option<&str>) -> Result<PathBuf> {
    socket_path_for_create_from(
        explicit,
        std::env::var(SOCKET_ENV).ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
    .await
}

/// The create-side answer with the environment supplied rather than read — the
/// same wrapper-and-parameter split `paths::default_project_dir_on` uses, and
/// for the same reason: an answer a test cannot reach is an answer nobody has
/// checked.
async fn socket_path_for_create_from(
    explicit: Option<&str>,
    env: Option<&str>,
    xdg: Option<&str>,
    tmpdir: Option<&str>,
) -> Result<PathBuf> {
    let socket = match non_empty(explicit).or_else(|| non_empty(env)) {
        Some(chosen) => PathBuf::from(chosen),
        None => {
            // The runtime directory riabuild's GitHub sessions already use,
            // rather than a second copy of the same fallback list. What that
            // helper guarantees is that the directory *exists* and that this
            // account can write it; the 0700 and the ownership below are this
            // module's, because the thing it enforces them with is not
            // reachable from here. Its own failure is worded for the caller it
            // was written for — a developer with no writable /tmp is told about
            // a GitHub sign-in — which is only reachable on a machine where the
            // channel is the least of the problems.
            let runtime = choose_runtime_dir(xdg, tmpdir).await?;
            let dir = runtime.join(RUNTIME_SUBDIR);
            ensure_owned_dir(&dir).await?;
            dir.join(SOCKET_FILE)
        }
    };

    ensure_ours(&socket, "opening the clipboard channel").await?;
    Ok(socket)
}

/// The directory the socket goes in, private from the instant it exists and
/// refused if it is somebody else's.
///
/// `/tmp` is the documented floor and is shared by every account on a server,
/// so a directory created at the default 0755 leaves the socket inside it
/// reachable by anyone logged in — the clipboard of a laptop that is not
/// theirs. `mode` on the builder means there is no window at 0755 before the
/// socket appears; the repair below covers a directory an earlier riabuild
/// created before this check existed.
async fn ensure_owned_dir(dir: &Path) -> Result<()> {
    match tokio::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(false)
        .create(dir)
        .await
    {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("creating {}", dir.display()));
        }
    }

    ensure_ours(dir, "opening the clipboard channel's runtime directory").await?;

    // Only reached for a directory the check above just said is ours, which is
    // what makes a path-based `chmod` acceptable here: `/tmp` is sticky, so no
    // other account can swap this name for one of theirs between the two calls.
    // `gh_session::private_dir` does the same repair through an `O_NOFOLLOW`
    // descriptor and needs no such argument — it is `pub(super)`, so this
    // module cannot borrow it.
    let mode = tokio::fs::metadata(dir)
        .await
        .with_context(|| format!("checking the mode of {}", dir.display()))?
        .mode();
    if mode & 0o777 != 0o700 {
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .await
            .with_context(|| format!("repairing {} to mode 0700", dir.display()))?;
    }
    Ok(())
}

/// What a `stat` that does not follow symlinks found at a path.
#[derive(Clone, Copy, Debug)]
struct Found {
    uid: u32,
    symlink: bool,
}

/// Refuses a path that exists and is not ours, so the caller may create, unlink
/// or bind it.
async fn ensure_ours(path: &Path, attempting: &str) -> Result<()> {
    // `symlink_metadata`, not `metadata`: a symlink is the one thing another
    // account can plant here whose owner is the interesting fact, and following
    // it would report the uid of whatever it aims at instead.
    let found = match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => Some(Found {
            uid: meta.uid(),
            symlink: meta.file_type().is_symlink(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("checking who owns {}", path.display()));
        }
    };
    refuse_unless_ours(path, found, current_uid(), attempting)
}

/// The decision, with the owning uid handed in.
///
/// Separated from the `stat` above because a test cannot `chown` a file without
/// privileges, and the branch worth pinning is not "can this process read
/// `st_uid`" but "what does riabuild do when the answer is somebody else".
fn refuse_unless_ours(
    path: &Path,
    found: Option<Found>,
    ours: u32,
    attempting: &str,
) -> Result<()> {
    let Some(found) = found else {
        return Ok(());
    };
    if !found.symlink && found.uid == ours {
        // Ours, and stale: the agent that left it is gone, or it is about to be
        // replaced by this one. Unlinking our own socket is how a channel comes
        // back after a killed session, so this is the ordinary case rather than
        // an error.
        return Ok(());
    }

    // Neither unlink nor bind. On a shared server this name is predictable and
    // may already be a colleague's live channel: binding over it takes their
    // clipboard traffic, and unlinking it takes their session's paste away
    // without either of them being told why. A symlink is refused on the same
    // grounds without consulting its target — riabuild did not put it there.
    let detail = if found.symlink {
        "it is a symlink, not a socket riabuild created".to_string()
    } else {
        format!(
            "it is owned by uid {} and this process is uid {ours}",
            found.uid
        )
    };
    Err(Failure::new(
        attempting,
        format!(
            "Remove {} if it is yours to remove, or set RIABUILD_CHANNEL_SOCKET to a path in a \
             directory you own, then start the session again.",
            path.display()
        ),
    )
    .detail(detail)
    .into())
}

/// The running process's uid. `libc::getuid` takes no arguments and cannot fail.
///
/// A twin of this lives in `gh_session::private_dir`, private to that module.
fn current_uid() -> u32 {
    // SAFETY: POSIX `getuid` takes no arguments, has no preconditions, and
    // cannot fail.
    unsafe { libc::getuid() }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The uid this process is not.
    fn a_stranger(ours: u32) -> u32 {
        ours.wrapping_add(1)
    }

    #[test]
    fn an_explicit_socket_beats_the_runtime_directory_on_the_read_side() {
        assert_eq!(
            socket_path_from(Some("/run/chosen.sock"), Some("/run/user/1000"), None),
            PathBuf::from("/run/chosen.sock")
        );
        assert_eq!(
            socket_path_from(Some(""), Some("/run/user/1000"), None),
            PathBuf::from("/run/user/1000/riabuild/channel.sock")
        );
        assert_eq!(
            socket_path_from(None, None, Some("/var/folders/xyz")),
            PathBuf::from("/var/folders/xyz/riabuild/channel.sock")
        );
        assert_eq!(
            socket_path_from(None, None, None),
            PathBuf::from("/tmp/riabuild/channel.sock")
        );
    }

    #[tokio::test]
    async fn an_explicit_socket_beats_the_runtime_directory_on_the_create_side() {
        // This is how remote mode hands the server the path its reverse forward
        // lands on, so the runtime directory must not be consulted at all —
        // including its create-time mkdir, which would otherwise leave a
        // `riabuild/` directory nothing ever binds in.
        let base = tempfile::TempDir::new().expect("tempdir");
        let chosen = base.path().join("chosen.sock");
        let runtime = base.path().join("runtime");
        tokio::fs::create_dir_all(&runtime).await.expect("mkdir");

        let path = socket_path_for_create_from(
            None,
            Some(&chosen.to_string_lossy()),
            Some(&runtime.to_string_lossy()),
            None,
        )
        .await
        .expect("path");

        assert_eq!(path, chosen);
        assert!(
            !runtime.join(RUNTIME_SUBDIR).exists(),
            "an explicit socket must not make riabuild touch the runtime directory"
        );
    }

    #[tokio::test]
    async fn the_command_line_socket_beats_the_environment() {
        let base = tempfile::TempDir::new().expect("tempdir");
        let flag = base.path().join("flag.sock");
        let env = base.path().join("env.sock");

        let path = socket_path_for_create_from(
            Some(&flag.to_string_lossy()),
            Some(&env.to_string_lossy()),
            None,
            None,
        )
        .await
        .expect("path");

        assert_eq!(path, flag);
    }

    #[tokio::test]
    async fn the_runtime_directory_is_created_private_and_the_socket_sits_in_it() {
        let base = tempfile::TempDir::new().expect("tempdir");
        let runtime = base.path().join("runtime");
        tokio::fs::create_dir_all(&runtime).await.expect("mkdir");

        let path = socket_path_for_create_from(None, None, Some(&runtime.to_string_lossy()), None)
            .await
            .expect("path");

        assert_eq!(path, runtime.join("riabuild").join("channel.sock"));
        let mode = tokio::fs::metadata(runtime.join("riabuild"))
            .await
            .expect("stat")
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "/tmp is shared, and a 0755 directory hands the laptop's clipboard to everyone on the box"
        );
    }

    #[tokio::test]
    async fn a_loose_runtime_directory_left_by_an_earlier_riabuild_is_repaired() {
        // The agent used to `create_dir_all` this directory, which takes the
        // umask — so upgrading onto a machine that already ran the channel must
        // fix the mode rather than refuse to start.
        let base = tempfile::TempDir::new().expect("tempdir");
        let runtime = base.path().join("runtime");
        let dir = runtime.join(RUNTIME_SUBDIR);
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("chmod 0755");

        socket_path_for_create_from(None, None, Some(&runtime.to_string_lossy()), None)
            .await
            .expect("path");

        let mode = tokio::fs::metadata(&dir).await.expect("stat").mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn a_socket_owned_by_another_account_is_refused_rather_than_unlinked() {
        // The failure this exists for: on a shared server the path is
        // predictable, so `/tmp/riabuild/channel.sock` may be a colleague's live
        // channel. Binding over it sends their clipboard traffic to this
        // laptop; unlinking it takes their paste away silently. A real file in a
        // TempDir stands in for the socket — `chown` needs privileges no test
        // has, so the owning uid is the parameter and this pins the decision.
        let base = tempfile::TempDir::new().expect("tempdir");
        let socket = base.path().join("channel.sock");
        tokio::fs::write(&socket, "").await.expect("write");
        let ours = current_uid();
        let found = Some(Found {
            uid: a_stranger(ours),
            symlink: false,
        });

        let error = refuse_unless_ours(&socket, found, ours, "opening the clipboard channel")
            .expect_err("another account owns it");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(
            failure.action.contains(&socket.display().to_string()),
            "the developer cannot act on a path they are not told: {}",
            failure.action
        );
        assert!(
            failure.action.contains("Remove") && failure.action.contains(SOCKET_ENV),
            "{}",
            failure.action
        );
        assert!(
            socket.exists(),
            "refusing must leave the other account's socket exactly where it was"
        );
    }

    #[tokio::test]
    async fn a_stale_socket_of_our_own_is_reusable_rather_than_fatal() {
        // A killed agent leaves its socket behind, and refusing that one would
        // mean the channel never comes back without a manual `rm`.
        let base = tempfile::TempDir::new().expect("tempdir");
        let socket = base.path().join("channel.sock");
        tokio::fs::write(&socket, "").await.expect("write");
        let ours = current_uid();

        refuse_unless_ours(
            &socket,
            Some(Found {
                uid: ours,
                symlink: false,
            }),
            ours,
            "opening the clipboard channel",
        )
        .expect("our own stale socket is ours to replace");

        // And through the real `stat`, since the file above genuinely belongs
        // to this process.
        socket_path_for_create_from(None, Some(&socket.to_string_lossy()), None, None)
            .await
            .expect("our own stale socket is ours to replace");
    }

    #[test]
    fn a_symlink_standing_in_for_the_socket_is_refused_without_following_it() {
        // A symlink our own uid owns still gets refused: riabuild binds sockets,
        // it does not plant links to them, so following one would be following
        // somebody's redirection to a target this check never saw.
        let ours = current_uid();
        let error = refuse_unless_ours(
            Path::new("/tmp/riabuild/channel.sock"),
            Some(Found {
                uid: ours,
                symlink: true,
            }),
            ours,
            "opening the clipboard channel",
        )
        .expect_err("riabuild did not put a symlink there");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.detail.contains("symlink"), "{}", failure.detail);
    }

    #[tokio::test]
    async fn nothing_at_the_path_is_not_a_refusal() {
        let base = tempfile::TempDir::new().expect("tempdir");
        ensure_ours(&base.path().join("absent.sock"), "opening the channel")
            .await
            .expect("an absent socket is the ordinary first run");
    }

    #[test]
    fn the_read_side_answers_for_a_path_it_cannot_stat() {
        // The shim runs on every Ctrl+V and can repair nothing, so this side
        // must stay free of the checks above: it returns a path for a runtime
        // directory that does not exist, and creates nothing on the way.
        let base = tempfile::TempDir::new().expect("tempdir");
        let missing = base.path().join("gone");

        let path = socket_path_from(None, Some(&missing.to_string_lossy()), None);

        assert_eq!(path, missing.join("riabuild").join("channel.sock"));
        assert!(!missing.exists(), "resolving a path must not create one");
    }
}
