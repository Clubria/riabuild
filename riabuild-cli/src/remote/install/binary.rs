//! The riabuild binary at its path on a server: where that path is, whether
//! what is already sitting there is the right thing, putting it there when it
//! is not, and proving afterwards that what landed is what was sent.
//!
//! Split out of `install.rs`, which keeps the decisions taken before any of
//! this — what the server is, which Rust target that makes it, and the
//! expected digest from the release's checksums — because the two halves
//! together ran past the crate's ~300-line production budget. Nothing here
//! decides *which* binary is the right one: every step below is handed that
//! answer as a digest and only ever asks whether the server holds it.

use super::{Downloads, SshCtx};
use crate::archive;
use crate::download;
use crate::paths::{Paths, RealPaths};
use crate::remote::{shell_command, shell_quote};
use crate::ui::Failure;
use anyhow::Result;
use std::path::PathBuf;

/// Where riabuild's own layout (`paths::riabuild_dir`) lands when evaluated
/// against a *remote* home rather than this laptop's. The second argument to
/// `with_root` is inert here on purpose: `RealPaths::tools_root()` always
/// resolves against `home` and ignores `root` (Task 6's tested invariant —
/// state is per-developer, toolchains are shared), so there is no other root
/// to hand it; `home` is simply repeated. See R10 in `decisions.md`.
fn remote_riabuild_dir(home: &str, version: &str) -> PathBuf {
    RealPaths::with_root(home, home).riabuild_dir(version)
}

/// Absolute, because nothing riabuild sends is parsed by a shell that would
/// expand a `~` — `mosh` and `fish`/`csh` do not expand it in the positions
/// remote mode uses, and an unexpanded `~` reaching `paths::root_for` is
/// refused outright rather than defaulting (R1 in `decisions.md`).
pub(super) fn remote_binary_path(home: &str, version: &str) -> String {
    remote_riabuild_dir(home, version)
        .join("riabuild")
        .to_string_lossy()
        .into_owned()
}

/// `sha256sum` on Linux, `shasum -a 256` on macOS; whichever exists prints
/// the digest as its first word. A shell fragment, not a full command —
/// callers wrap it with `shell_command`.
fn digest_probe(path: &str) -> String {
    let quoted = shell_quote(path);
    format!(
        "if command -v sha256sum >/dev/null 2>&1; then sha256sum {quoted}; \
         else shasum -a 256 {quoted}; fi | cut -d' ' -f1"
    )
}

fn digest_command(path: &str) -> String {
    shell_command(&digest_probe(path))
}

/// What `verify_or_remove_command` prints, in place of a digest, when `rm -f`
/// was issued but the file is still there afterwards — permission denied, a
/// read-only home, a quota, an immutable bit. Distinctive on purpose: it must
/// never be mistaken for a real sha256 digest (64 lowercase hex characters)
/// by whatever compares against `expected`.
const REMOVE_FAILED_MARKER: &str = "RIABUILD_REMOVE_FAILED";

/// Computes the on-disk digest of `path` and, if it does not match
/// `expected`, removes `path` — in the *same* SSH round trip as the check,
/// and checks the file's actual absence afterwards rather than trusting
/// `rm -f`'s exit status. `rm -f` succeeding is not the same as the file
/// being gone (it swallows its own failures, and a corrupted transfer plus a
/// permission problem is exactly the kind of unlucky day this exists for):
/// without the `[ -e … ]` check, `rm -f` could fail silently, `write_binary`
/// would report "removed" while the corrupted, `chmod 755`'d binary is still
/// sitting at a well-known, shared, executable path — a false reassurance
/// that is worse than no message at all, because it stops anyone looking.
fn verify_or_remove_command(path: &str, expected: &str) -> String {
    shell_command(&format!(
        "digest=$({probe}); \
         if [ \"$digest\" != {expected} ]; then \
         rm -f {path}; \
         if [ -e {path} ]; then printf {marker}; exit 1; fi; \
         fi; \
         printf %s \"$digest\"",
        probe = digest_probe(path),
        expected = shell_quote(expected),
        path = shell_quote(path),
        marker = shell_quote(REMOVE_FAILED_MARKER),
    ))
}

/// The part of [`super::ensure_riabuild_with`] that needs no network beyond
/// the tarball download itself.
pub(super) async fn ensure_matching_binary(
    ctx: &SshCtx<'_>,
    home: &str,
    version: &str,
    target: &str,
    expected: &str,
    downloads: &dyn Downloads,
) -> Result<String> {
    let path = remote_binary_path(home, version);

    // Trusted by digest, never by the version it claims. A co-tenant can put
    // a script at this path that prints any version string it likes, and
    // every other developer on a shared account would then execute it with
    // their session token in the environment. `sha256sum`/`shasum` is asked
    // for the digest of what is actually there.
    let installed = ctx.ssh(&digest_command(&path)).await?;
    if installed.ok() && installed.trimmed() == expected {
        return Ok(path);
    }

    ctx.ui
        .working("riabuild", &format!("installing {version} on the server"));

    // Verified against `expected` before a single byte is extracted or sent
    // anywhere near the server.
    let tarball = downloads.tarball(version, target).await?;
    if download::sha256_hex(&tarball) != expected {
        return Err(Failure::new(
            format!("verifying the riabuild {version} download"),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail("the download did not match its published digest")
        .into());
    }
    let binary = archive::extract_single_file(&tarball, "riabuild")?;

    write_binary(ctx, home, version, expected, binary).await
}

/// Streams already-verified bytes onto the server and confirms what landed —
/// split out from [`ensure_matching_binary`] so the write, and its failure
/// and corruption paths, are testable against a `FakeRunner` without ever
/// downloading anything.
async fn write_binary(
    ctx: &SshCtx<'_>,
    home: &str,
    version: &str,
    expected: &str,
    binary: Vec<u8>,
) -> Result<String> {
    let path = remote_binary_path(home, version);

    // Written to a temporary name and moved into place, so a concurrent
    // reader sees a complete binary or none. The name carries this process's
    // pid: a fixed `.part` would have two developers installing the same
    // version at the same moment write into one file and rename the
    // interleaved result into place, and that race is the ordinary case on a
    // shared box, not the exotic one.
    let dir = remote_riabuild_dir(home, version)
        .to_string_lossy()
        .into_owned();
    let part = format!("{dir}/.riabuild.{}.part", std::process::id());
    let quoted_dir = shell_quote(&dir);
    let quoted_part = shell_quote(&part);
    let quoted_path = shell_quote(&path);
    let install = shell_command(&format!(
        "umask 077 && mkdir -p {quoted_dir} && cat > {quoted_part} && \
         chmod 755 {quoted_part} && mv {quoted_part} {quoted_path}"
    ));
    let written = ctx.ssh_with_stdin(install, binary).await?;
    if !written.ok() {
        return Err(Failure::new(
            format!("installing riabuild on {}", ctx.remote.host),
            "Check there is space in your home directory on that server, then run `riabuild remote` again.",
        )
        .detail(written.stderr)
        .into());
    }

    // Re-verified after writing, and cleaned up in the same round trip on a
    // mismatch (see `verify_or_remove_command`): a truncated or corrupted
    // transfer must never be left behind at this well-known, shared path,
    // and must never be reported as a successful install.
    let confirmed = ctx.ssh(&verify_or_remove_command(&path, expected)).await?;

    // Checked before the generic `!confirmed.ok()` branch below: a failed
    // removal also exits non-zero, and this is the one case that must never
    // be mistaken for an ordinary "try again" failure — the corrupted binary
    // is still sitting at a well-known, shared, executable path, and nobody
    // must be told otherwise.
    if confirmed.trimmed() == REMOVE_FAILED_MARKER {
        return Err(Failure::new(
            format!("removing a corrupted riabuild from {}", ctx.remote.host),
            format!(
                "riabuild could not remove it. SSH to that server yourself and delete {path} \
                 by hand before running `riabuild remote` again — do not run anything at that \
                 path in the meantime."
            ),
        )
        .detail("rm -f was issued but the file is still there afterwards")
        .into());
    }
    if !confirmed.ok() {
        return Err(Failure::new(
            format!("verifying riabuild on {}", ctx.remote.host),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail(confirmed.stderr)
        .into());
    }
    if confirmed.trimmed() != expected {
        return Err(Failure::new(
            format!("installing riabuild on {}", ctx.remote.host),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail(format!(
            "the server reports {:?} after installing {version} — the file was removed rather than left behind",
            confirmed.trimmed()
        ))
        .into());
    }
    ctx.ui.applied("riabuild");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{Remote, identity};
    use crate::runner::FakeRunner;
    use crate::ui::Ui;
    use async_trait::async_trait;
    use std::sync::Arc;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    const EXPECTED: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// The exact prefix `identity::ssh_options` plus the login target
    /// produces — shared by every command sent to `remote`, so it is what
    /// lets `FakeRunner::then` sequence responses to *successive* remote
    /// calls in order, regardless of which trailing command each one sends.
    fn ssh_prefix(remote: &Remote, paths: &dyn Paths) -> String {
        let options = identity::ssh_options(remote, paths, true).join(" ");
        format!("ssh {options} {}", remote.target())
    }

    /// A `Downloads` that panics if called — for tests proving a path never
    /// touches the network at all.
    struct UnreachableDownloads;

    #[async_trait]
    impl Downloads for UnreachableDownloads {
        async fn checksums(&self, _version: &str) -> Result<String> {
            panic!("must not fetch checksums on this path");
        }
        async fn tarball(&self, _version: &str, _target: &str) -> Result<Vec<u8>> {
            panic!("must not download a tarball on this path");
        }
    }

    #[test]
    fn the_remote_path_is_versioned_and_shared() {
        // Shared, so five developers on one account get one toolchain; versioned,
        // so two developers on two riabuild versions do not fight over a file.
        // Absolute (R1): a `~` is only a home directory to a shell willing to
        // expand it, and mosh and fish/csh do not in the positions this is used.
        assert_eq!(
            remote_binary_path("/home/dev", "2026.08.06"),
            "/home/dev/.riabuild/riabuild/2026.08.06/riabuild"
        );
    }

    #[tokio::test]
    async fn a_server_already_holding_the_right_binary_is_left_alone() {
        // `UnreachableDownloads` makes the property absolute: this path must
        // not touch the network at all, not merely "happens not to" in this
        // particular stub.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        // The digest of what is on disk, not the version it claims. A co-tenant
        // can put a script at that path that prints any version string.
        let fake =
            Arc::new(FakeRunner::new().containing("sha256sum", 0, &format!("{EXPECTED}\n"), ""));
        let remote = remote();
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake.clone(),
            ui: &Ui::new(true),
        };

        let path = ensure_matching_binary(
            &ctx,
            "/home/dev",
            "2026.08.06",
            "aarch64-apple-darwin",
            EXPECTED,
            &UnreachableDownloads,
        )
        .await
        .expect("already installed");

        assert_eq!(path, remote_binary_path("/home/dev", "2026.08.06"));
        assert!(
            !fake.calls().iter().any(|call| call.contains("mkdir")),
            "nothing should be installed: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn a_missing_binary_is_written_verified_and_confirmed() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(
            FakeRunner::new()
                .containing("mkdir -p", 0, "", "")
                .containing("sha256sum", 0, &format!("{EXPECTED}\n"), ""),
        );
        let remote = remote();
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake.clone(),
            ui: &Ui::new(true),
        };

        let path = write_binary(
            &ctx,
            "/home/dev",
            "2026.08.06",
            EXPECTED,
            b"fake riabuild binary".to_vec(),
        )
        .await
        .expect("writes");

        assert_eq!(path, remote_binary_path("/home/dev", "2026.08.06"));
        assert!(
            fake.calls().iter().any(|call| call.contains("chmod 755")),
            "{:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn a_failed_write_is_reported_with_an_actionable_next_step() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake =
            Arc::new(FakeRunner::new().containing("mkdir -p", 1, "", "No space left on device"));
        let remote = remote();
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake,
            ui: &Ui::new(true),
        };

        let error = write_binary(
            &ctx,
            "/home/dev",
            "2026.08.06",
            EXPECTED,
            b"fake riabuild binary".to_vec(),
        )
        .await
        .expect_err("no space");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(failure.action.contains("space"), "{}", failure.action);
    }

    #[tokio::test]
    async fn a_corrupted_transfer_is_caught_and_the_bad_file_is_removed() {
        // The write itself reported success, but what the server can now read
        // back does not match — a truncated or interleaved transfer, say. This
        // must not be reported as an install that worked, and must not leave
        // an executable file behind at a well-known, shared path.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let remote = remote();
        let path = remote_binary_path("/home/dev", "2026.08.06");

        let prefix = ssh_prefix(&remote, &paths);
        let fake = Arc::new(
            FakeRunner::new()
                .then(&prefix, 0, "", "") // the write
                // Post-write reverify: mismatch, `rm -f` ran and the file's
                // `[ -e … ]` check afterwards came back false, so the script
                // falls through to `printf %s "$digest"` with the *old*
                // digest — the shape a successful cleanup actually produces.
                .then(&prefix, 0, "deadbeef\n", ""),
        );
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake.clone(),
            ui: &Ui::new(true),
        };

        let error = write_binary(
            &ctx,
            "/home/dev",
            "2026.08.06",
            EXPECTED,
            b"fake riabuild binary".to_vec(),
        )
        .await
        .expect_err("digest mismatch after writing");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(failure.detail.contains("deadbeef"), "{}", failure.detail);

        // The removal, and a check that it actually worked, must both be in
        // the very same round trip as the mismatch check — not a second
        // command a retry might never reach, and not trusting `rm -f`'s exit
        // status, which is discarded here on purpose (see
        // `verify_or_remove_command`'s doc comment): it can fail silently,
        // and a "removed" message while the file is still there is a false
        // reassurance that stops anyone from looking.
        let issued = fake.calls();
        let reverify = issued
            .iter()
            .rev()
            .find(|call| call.contains("sha256sum") || call.contains("shasum"))
            .expect("a reverify call was made");
        assert!(reverify.contains("rm -f"), "{reverify}");
        assert!(reverify.contains(&path), "{reverify}");
        assert!(
            reverify.contains("-e ") && reverify.contains(REMOVE_FAILED_MARKER),
            "must check the file's absence after rm -f, not just issue it: {reverify}"
        );
    }

    #[tokio::test]
    async fn a_removal_that_fails_is_reported_as_a_dangerous_leftover_not_a_generic_retry() {
        // `rm -f` was issued but the file is still there afterwards —
        // permission denied, a read-only home, a quota. This must never be
        // confused with an ordinary "try again" failure or, worse, reported
        // as a successful cleanup: a corrupted, `chmod 755`'d binary is still
        // sitting at a well-known, shared, executable path.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let remote = remote();
        let path = remote_binary_path("/home/dev", "2026.08.06");

        let prefix = ssh_prefix(&remote, &paths);
        let fake = Arc::new(
            FakeRunner::new()
                .then(&prefix, 0, "", "") // the write
                .then(&prefix, 1, REMOVE_FAILED_MARKER, ""), // rm -f ran, file survived
        );
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake,
            ui: &Ui::new(true),
        };

        let error = write_binary(
            &ctx,
            "/home/dev",
            "2026.08.06",
            EXPECTED,
            b"fake riabuild binary".to_vec(),
        )
        .await
        .expect_err("a surviving corrupted file must be reported");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        // Distinct from the ordinary "try again" wording elsewhere in this
        // file: this is the one case that must read as dangerous, not routine.
        assert!(
            failure.action.contains("by hand"),
            "must not read as an ordinary retry: {}",
            failure.action
        );
        assert!(failure.action.contains(&path), "{}", failure.action);
        assert!(
            !failure.detail.contains("removed"),
            "must not claim a removal that did not happen: {}",
            failure.detail
        );
    }
}
