//! The file-backed store, and the filesystem helpers that keep it private.
//!
//! A server has no keyring, so its own session token is the one secret riabuild
//! writes down — the exception argued for on [`FileKeychain`] below. Everything
//! else here exists to make that file unreadable by a co-tenant, and to make
//! sure it is *ours*: the directory's mode is repaired on every write rather
//! than only set at creation, because `DirBuilder::mode` describes a path that
//! does not exist yet and says nothing about one that already does, and the
//! directory is refused outright when it is a symlink or belongs to another
//! account.
//!
//! The write itself is `riabuild_paths::config::write_atomic` rather than a
//! truncate and a write, and that matters more here than at any other call
//! site: a concurrent run reading between those two syscalls sees an empty
//! file, [`Keychain::get`] answers `None`, and riabuild mints a fresh
//! ninety-day session for a machine that already had one.

use super::Keychain;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// A secret in a 0600 file, for a machine with nowhere better to put one.
///
/// Two machines reach it, and [`describe`](Keychain::describe) must say which,
/// because `provision.rs` prints that sentence to the developer and it is the
/// only thing on screen that says where their token went. A store that names
/// the wrong place is the shape of an earlier bug here — `scope.rs` still
/// carries the note about a laptop whose keychain reported itself as "this
/// server's riabuild namespace" — so the description is a constructor
/// argument rather than one string that covers both.
///
/// - [`server_namespace`](Self::server_namespace): a managed server's own
///   session. The original exception to "no secrets in ~/.riabuild", argued in
///   the remote mode design: a server has no keyring, the token is minted for
///   that server alone, it is labelled and listed in the dashboard, and
///   `riabuild remote forget` revokes it.
/// - [`keyringless_machine`](Self::keyringless_machine): a Linux machine with
///   no Secret Service answering. The same exception, widened the same way the
///   remote-password design already widened it for an SSH password, and for
///   the identical reason: the alternative on such a machine is not "no token
///   on disk", it is that riabuild cannot run there at all.
///
/// What the invariant exists to protect — the Infisical org credential — is
/// untouched by either: it is still brokered per use and still never written
/// down.
pub struct FileKeychain {
    path: PathBuf,
    description: &'static str,
}

impl FileKeychain {
    /// A managed server's own session token, in the developer's namespace.
    pub fn server_namespace(path: PathBuf) -> Self {
        Self {
            path,
            description: "this server's riabuild namespace",
        }
    }

    /// A machine whose keyring does not answer. Named for the *reason* rather
    /// than the path, because that reason is what the developer needs in order
    /// to know this is not where riabuild would normally have put it.
    pub fn keyringless_machine(path: PathBuf) -> Self {
        Self {
            path,
            description: "a private file, because this machine has no keyring",
        }
    }
}

#[async_trait]
impl Keychain for FileKeychain {
    async fn get(&self) -> Result<Option<String>> {
        match read_token(&self.path).await? {
            Some(text) => {
                let token = text.trim().to_string();
                Ok((!token.is_empty()).then_some(token))
            }
            None => Ok(None),
        }
    }

    async fn set(&self, token: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            ensure_private_dir(parent).await?;
        }
        // A 0600 temporary beside the target, renamed over it. Three of the
        // properties that helper documents are the reason this is not a write
        // of its own: no reader ever sees the empty file a truncate leaves; the
        // mode is right from the instant the bytes exist, rather than repaired
        // afterwards on a file that already holds the token; and the rename
        // *replaces* a symlink at this path instead of writing the session
        // token through it.
        riabuild_paths::config::write_atomic(&self.path, token.as_bytes())
            .await
            .with_context(|| {
                format!(
                    "could not save the session token to {}",
                    self.path.display()
                )
            })
    }

    async fn delete(&self) -> Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn describe(&self) -> &'static str {
        self.description
    }
}

/// Reads the token file, refusing a symlink standing in for it.
///
/// `O_NOFOLLOW` rather than a plain read: a symlink here is not a file riabuild
/// wrote, and following one means answering `get()` with whatever a co-tenant
/// pointed it at — a session token of *their* choosing, used by this machine
/// for every request it makes afterwards. It is reported rather than treated as
/// absence for the same reason: "there is no token" starts a device-code flow
/// and quietly writes over the planted link, saying nothing about the machine
/// having been tampered with.
#[cfg(unix)]
async fn read_token(path: &Path) -> Result<Option<String>> {
    use tokio::io::AsyncReadExt;

    let mut file = match tokio::fs::OpenOptions::new()
        .read(true)
        // tokio's own, behind `cfg(unix)`; `OpenOptionsExt` would be an unused
        // import on this type and so a build failure under `-D warnings`.
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not read {} — if that path is a symlink, riabuild will not read a \
                     session token through it",
                    path.display()
                )
            });
        }
    };

    let mut text = String::new();
    file.read_to_string(&mut text)
        .await
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(Some(text))
}

#[cfg(not(unix))]
async fn read_token(path: &Path) -> Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The namespace directory the session file lives in, `0700` and ours.
///
/// `create_dir_all` (or `DirBuilder` with `recursive(true)`) does not re-apply
/// `mode` to a directory that already exists, so a namespace directory left
/// world-readable by something else — an earlier riabuild version, an admin
/// script, an `umask` — would otherwise keep that mode forever. This is the
/// directory the one file the "no secrets in ~/.riabuild" invariant was amended
/// for lives in, so its mode is repaired unconditionally, not just set at
/// creation.
///
/// Repairing is not enough on its own, because the mode of a directory somebody
/// else owns is not a thing riabuild can fix by chmod-ing it — and on a server
/// `~/.riabuild-remote/<member-id>` is a predictable name under a home
/// directory every developer on the box can write to, so getting there first is
/// available to any of them. So the directory is also *verified*: a symlink or
/// a foreign uid is refused rather than written into.
///
/// `gh_session::private_dir` argues the same case at length for `/tmp` and
/// differs deliberately in one respect — its create is non-recursive, because
/// there the parent's absence is information. Here the parent chain is
/// riabuild's own root, which a first run legitimately has to create.
#[cfg(unix)]
async fn ensure_private_dir(dir: &Path) -> Result<()> {
    match tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .await
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("creating {}", dir.display())),
    }

    let owned = dir.to_path_buf();
    // Blocking, not async: opening by descriptor and `fstat`/`fchmod`-ing it
    // are POSIX calls tokio has no async wrapper for. `spawn_blocking` runs
    // them on the blocking pool, so no future on the reactor thread stalls.
    tokio::task::spawn_blocking(move || verify_private_dir(&owned)).await?
}

#[cfg(not(unix))]
async fn ensure_private_dir(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    Ok(())
}

/// Checks — by descriptor, not by name — that the directory is a real directory
/// this account owns, and repairs its mode if it is.
///
/// Statting the path and then chmod-ing the same path in a second call leaves a
/// window in which the name can be repointed at something else. One descriptor
/// opened with `O_NOFOLLOW | O_DIRECTORY` closes it: the fd names one fixed
/// inode from open to close, a symlink at the path fails the open outright, and
/// there is no second name lookup for anyone to win.
#[cfg(unix)]
fn verify_private_dir(dir: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(dir.as_os_str().as_bytes())
        .with_context(|| format!("{} contains a NUL byte", dir.display()))?;

    // SAFETY: `c_path` is a valid, NUL-terminated C string that outlives the
    // call. `O_NOFOLLOW` refuses a symlink at the final component rather than
    // following it and `O_DIRECTORY` refuses anything that is not a directory;
    // either shows up as -1, handled below.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "opening {} to check it is a private directory of yours",
                dir.display()
            )
        });
    }

    let result = check_owner_and_mode(fd, dir);

    // SAFETY: `fd` came from the `open` above and is not used again after this.
    unsafe {
        libc::close(fd);
    }
    result
}

/// The `fstat`, the ownership check and the `fchmod`, against a descriptor
/// already known to name a real directory. Split out so `verify_private_dir`'s
/// `close` runs on every return path from here, including an early `?`.
#[cfg(unix)]
fn check_owner_and_mode(fd: i32, dir: &Path) -> Result<()> {
    // SAFETY: zero-initialised before `fstat` fills every field `libc::stat`
    // defines; nothing below reads a field `fstat` did not write.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and valid for this call, and `&mut stat` is a valid
    // writable buffer of the layout `fstat` expects.
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("checking who owns {}", dir.display()));
    }

    // SAFETY: POSIX `getuid` takes no arguments, has no preconditions, and
    // cannot fail.
    if stat.st_uid != unsafe { libc::getuid() } {
        return Err(riabuild_ui::Failure::new(
            "opening a private directory for this machine's riabuild session",
            format!(
                "Remove {}, or ask whoever owns it to, then run riabuild again.",
                dir.display()
            ),
        )
        .detail("it exists and belongs to another account, so riabuild will not write a session token into it")
        .into());
    }

    if stat.st_mode & 0o777 != 0o700 {
        // SAFETY: `fd` is open, valid, and known by the `fstat` above to name a
        // directory this process owns.
        if unsafe { libc::fchmod(fd, 0o700) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("repairing {} to mode 0700", dir.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn a_server_keeps_its_session_in_a_file_it_owns() {
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        let keychain = FileKeychain::server_namespace(path.clone());

        assert_eq!(keychain.get().await.expect("read"), None);
        keychain.set("rb_live_token").await.expect("write");
        assert_eq!(
            keychain.get().await.expect("read"),
            Some("rb_live_token".to_string())
        );

        keychain.delete().await.expect("delete");
        assert_eq!(keychain.get().await.expect("read"), None);
        // Deleting what is already gone is not an error: `apply()` runs twice.
        keychain.delete().await.expect("delete again");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_session_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        FileKeychain::server_namespace(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "session.token must be 0600");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_readable_session_file_is_repaired_on_write() {
        // Creating with mode 0600 does not fix a file that is already 0644 — the
        // token still works, it is just readable by every co-tenant on the
        // shared account. `set()` must repair the mode of a file that already
        // exists, not only of one it creates, and it must still write the new
        // token correctly while doing so.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        std::fs::write(&path, "stale").expect("pre-create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");

        let keychain = FileKeychain::server_namespace(path.clone());
        keychain.set("rb_live_token").await.expect("write");

        assert_eq!(
            keychain.get().await.expect("read"),
            Some("rb_live_token".to_string()),
            "the repair path must still write the new token"
        );
        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "an existing 0644 file must be repaired"
        );
    }

    // NOTE ON WHAT THE TEST ABOVE DOES AND DOES NOT PROVE, for a reviewer
    // looking for a test that pins the *ordering* rather than the end state:
    //
    // It proves content and mode are both correct once `set()` returns. It
    // cannot prove there was never a moment in between where the new token sat
    // behind the file's old, looser mode — a black-box test can only observe
    // before-and-after, not the sequence of syscalls a single sequential async
    // task made in between. Making that window observable would need a second
    // task genuinely running in parallel with the write, and this crate
    // deliberately does not enable tokio's `rt-multi-thread` feature (see the
    // comment on the `tokio` dependency in Cargo.toml: "so a stray
    // `Runtime::new()` cannot quietly spawn a worker pool") — pulling that in
    // for one test, even a test-only one, would itself be inconsistent with
    // that constraint, and a race against real disk I/O timing would be
    // nondeterministic regardless: it could pass on a fast disk and fail on a
    // slow one, which is a worse property than an honest gap.
    //
    // So the ordering claim is not directly black-box testable here, and is
    // instead fixed by construction, and the construction is now somebody
    // else's: `set` writes through `riabuild_paths::config::write_atomic`,
    // which creates a *new* file at 0600, writes it, and renames it over the
    // target. The window this note was written about cannot exist there,
    // because the bytes and the mode arrive on an inode nothing else has a name
    // for until the rename publishes it — the pre-existing 0644 file the test
    // above sets up is not modified at all, it is replaced. `paths`' own
    // `the_temporary_is_never_at_the_umask` is where that 0600 is pinned.
    //
    // Two earlier shapes are what a reviewer should keep checking this file has
    // not drifted back into: writing the content and chmod-ing the path
    // afterwards, and `create(true).truncate(true)` on the target itself — the
    // second of which additionally let a concurrent reader see an empty file
    // and mint a fresh ninety-day session for a machine that had one.

    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_readable_namespace_directory_is_repaired_on_write() {
        // The containing directory is part of the same security property: a
        // 0755 namespace directory lets anyone on the box list it and read the
        // session file inside, regardless of the file's own mode.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let namespace = home.path().join("ns");
        std::fs::create_dir(&namespace).expect("pre-create namespace dir");
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o777))
            .expect("loosen permissions");

        FileKeychain::server_namespace(namespace.join("session.token"))
            .set("rb_live_token")
            .await
            .expect("write");

        let mode = tokio::fs::metadata(&namespace)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "an existing 0777 namespace dir must be repaired"
        );
    }

    #[tokio::test]
    async fn each_file_store_says_which_machine_it_is() {
        // `provision.rs` prints `describe()`, and it is the only line telling a
        // developer where their token went. One string covering both callers
        // would put "this server's riabuild namespace" on a laptop — which is
        // the exact dishonesty `scope.rs` still carries a note about.
        let path = PathBuf::from("/home/ada/.riabuild/session.token");
        assert_eq!(
            FileKeychain::server_namespace(path.clone()).describe(),
            "this server's riabuild namespace"
        );
        assert_eq!(
            FileKeychain::keyringless_machine(path).describe(),
            "a private file, because this machine has no keyring"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_keyringless_machines_token_is_as_private_as_a_servers() {
        // The fallback is a widening of "no secrets in ~/.riabuild", so it has
        // to carry the same protection the original exception was granted on:
        // 0600, in a directory at 0700. Same code path as the server store, and
        // this asserts it rather than assuming the shared constructor implies
        // it.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let dir = home.path().join("riabuild");
        let path = dir.join("session.token");
        FileKeychain::keyringless_machine(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

        let file = tokio::fs::metadata(&path).await.expect("stat file");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let parent = tokio::fs::metadata(&dir).await.expect("stat dir");
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    }

    #[tokio::test]
    async fn writing_the_token_twice_replaces_it() {
        let home = TempDir::new().expect("tempdir");
        let keychain = FileKeychain::server_namespace(home.path().join("session.token"));
        keychain.set("first").await.expect("write");
        keychain.set("second").await.expect("write");
        assert_eq!(
            keychain.get().await.expect("read"),
            Some("second".to_string())
        );
    }

    #[tokio::test]
    async fn a_write_leaves_nothing_beside_the_token() {
        // The rename is what makes a concurrent reader see the old token or the
        // new one and never an empty file — so a temporary left behind is not
        // untidiness, it is a write that did not land the way it claims to.
        let home = TempDir::new().expect("tempdir");
        FileKeychain::server_namespace(home.path().join("session.token"))
            .set("rb_live_token")
            .await
            .expect("write");

        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(home.path()).await.expect("read_dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec!["session.token".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_where_the_token_belongs_is_not_read_through() {
        // Following one would answer `get()` with a token a co-tenant chose,
        // and this machine would then make every request with it. Refusing is
        // deliberately not the same as answering `None`: absence starts a
        // device-code flow and says nothing about the machine.
        let home = TempDir::new().expect("tempdir");
        let planted = home.path().join("planted.token");
        std::fs::write(&planted, "rb_live_theirs").expect("plant");
        let path = home.path().join("session.token");
        tokio::fs::symlink(&planted, &path).await.expect("symlink");

        let error = FileKeychain::server_namespace(path)
            .get()
            .await
            .expect_err("a symlink must not be followed");
        assert!(
            !format!("{error:#}").contains("rb_live_theirs"),
            "and the planted token must not be echoed into the error either"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_where_the_token_belongs_is_replaced_rather_than_written_through() {
        // The other half: the rename lands on the link itself, so the file the
        // link pointed at never sees this machine's session token.
        let home = TempDir::new().expect("tempdir");
        let elsewhere = home.path().join("elsewhere.token");
        std::fs::write(&elsewhere, "not ours").expect("plant");
        let path = home.path().join("session.token");
        tokio::fs::symlink(&elsewhere, &path)
            .await
            .expect("symlink");

        FileKeychain::server_namespace(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

        assert_eq!(
            std::fs::read_to_string(&elsewhere).expect("read"),
            "not ours",
            "the token must not have travelled through the symlink"
        );
        assert!(
            !std::fs::symlink_metadata(&path)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link must have been replaced by a real file"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_namespace_directory_is_refused() {
        // `~/.riabuild-remote/<member-id>` is a predictable name under a home
        // directory every developer on a shared server can write to, so getting
        // there first with a symlink is available to any of them. `O_NOFOLLOW |
        // O_DIRECTORY` is what refuses it, rather than a `symlink_metadata`
        // check a second syscall could race.
        //
        // The uid half of the same check has no test: creating a directory
        // owned by another account needs a second account, which no unit test
        // has. It is `libc::getuid` against `fstat`'s `st_uid` in
        // `check_owner_and_mode`, read there rather than asserted here.
        let home = TempDir::new().expect("tempdir");
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("mkdir");
        let namespace = home.path().join("ns");
        tokio::fs::symlink(&elsewhere, &namespace)
            .await
            .expect("symlink");

        FileKeychain::server_namespace(namespace.join("session.token"))
            .set("rb_live_token")
            .await
            .expect_err("a symlinked namespace directory must be refused");

        assert!(
            !elsewhere.join("session.token").exists(),
            "nothing may be written through it"
        );
    }
}
