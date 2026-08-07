//! Getting the right riabuild onto a server.
//!
//! Downloaded and verified on the laptop, then streamed over SSH. That keeps
//! digest verification in the one place that already does it properly, and
//! needs nothing installed on the server but a shell.

use super::{Remote, identity, shell_command, shell_quote, ssh_once};
use crate::download;
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandOutput, CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

/// The fixed `(remote, paths, runner, ui)` quad every step below needs —
/// bundled so threading it through a private call chain does not turn into a
/// wall of repeated parameters (and the `#[allow(clippy::too_many_arguments)]`
/// that would otherwise be needed at every stage).
struct SshCtx<'a> {
    remote: &'a Remote,
    paths: &'a dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &'a Ui,
}

impl SshCtx<'_> {
    async fn ssh(&self, command: &str) -> Result<CommandOutput> {
        ssh_once(self.remote, self.paths, self.runner.clone(), command).await
    }

    /// Same as [`Self::ssh`], but with `stdin` piped to the command — for
    /// streaming the binary itself, which `ssh_once` has no room for.
    async fn ssh_with_stdin(&self, command: String, stdin: Vec<u8>) -> Result<CommandOutput> {
        let mut args = identity::ssh_options(self.remote, self.paths, true);
        args.push(self.remote.target());
        args.push(command);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner
            .run(
                "ssh",
                &refs,
                &RunOptions {
                    stdin: Some(stdin),
                    ..Default::default()
                },
            )
            .await
    }
}

/// The network calls made before anything is trusted — a seam so the
/// *composed* install path (`ensure_riabuild` through `write_binary`) is
/// testable end to end without a real GitHub release to fetch, mirroring how
/// `CommandRunner` seams out `ssh`. `target` and `expected` are adjacent,
/// same-typed `&str` parameters handed into [`ensure_matching_binary`]; with
/// each stage previously only testable in isolation, transposing them would
/// have compiled and passed every existing test silently. `RealDownloads` is
/// what production uses; tests substitute a fixed pair of responses.
#[async_trait]
trait Downloads: Send + Sync {
    async fn checksums(&self, version: &str) -> Result<String>;
    async fn tarball(&self, version: &str, target: &str) -> Result<Vec<u8>>;
}

struct RealDownloads;

#[async_trait]
impl Downloads for RealDownloads {
    async fn checksums(&self, version: &str) -> Result<String> {
        download::fetch_text(&download::riabuild_checksums_url(version))
            .await
            .map_err(|error| {
                Failure::new(
                    format!("verifying the riabuild {version} download"),
                    "Check this laptop's network connection, then run `riabuild remote` again.",
                )
                .detail(error.to_string())
                .into()
            })
    }

    async fn tarball(&self, version: &str, target: &str) -> Result<Vec<u8>> {
        download::fetch_bytes(&download::riabuild_asset_url(version, target))
            .await
            .map_err(|error| {
                Failure::new(
                    format!("downloading riabuild {version} for this server"),
                    "Check this laptop's network connection, then run `riabuild remote` again.",
                )
                .detail(error.to_string())
                .into()
            })
    }
}

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
pub fn remote_binary_path(home: &str, version: &str) -> String {
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

/// Installs riabuild `version` on `remote`, if it is not already there, and
/// returns its absolute path.
///
/// `home` is the server's own home directory, from `remote::resolve_home` —
/// never a `~` (R1). Platform detection and checksum verification happen
/// here, before anything is compared against the expected digest, so that
/// digest always exists before its first use (R8(b) in `decisions.md`).
pub async fn ensure_riabuild(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    home: &str,
    version: &str,
) -> Result<String> {
    let ctx = SshCtx {
        remote,
        paths,
        runner,
        ui,
    };
    ensure_riabuild_with(&ctx, home, version, &RealDownloads).await
}

/// The body of [`ensure_riabuild`], taking [`Downloads`] as a seam so the
/// whole composed path is testable without a real release to fetch.
async fn ensure_riabuild_with(
    ctx: &SshCtx<'_>,
    home: &str,
    version: &str,
    downloads: &dyn Downloads,
) -> Result<String> {
    let platform = ctx.ssh("uname -sm").await?;
    if !platform.ok() {
        return Err(Failure::new(
            format!("asking {} what it is", ctx.remote.host),
            "Check that you can `ssh` to that server yourself, then run `riabuild remote` again.",
        )
        .command("uname -sm")
        .detail(platform.stderr)
        .into());
    }
    let mut parts = platform.trimmed().split_whitespace();
    let system = parts.next().unwrap_or_default();
    let machine = parts.next().unwrap_or_default();
    let target = download::rust_target(system, machine).map_err(|error| {
        Failure::new(
            format!("installing riabuild on {}", ctx.remote.host),
            "Use a server riabuild publishes a build for: Linux or macOS, on x86_64 or arm64.",
        )
        .detail(error.to_string())
    })?;

    // Computed before it is compared against anything or trusted to skip an
    // install — see R8(b).
    let checksums = downloads.checksums(version).await?;
    let asset = download::riabuild_asset(version, &target);
    let expected = download::digest_for(&checksums, &asset).ok_or_else(|| {
        Failure::new(
            format!("verifying the riabuild {version} download"),
            "Tell your team lead — the release is missing a checksum for this platform.",
        )
        // Names the asset it looked for, so "this platform" is answerable
        // from the message alone. `e2e/remote/run.sh` also keys its
        // known-gap check on the target appearing here: without it, a
        // release that is simply missing the *right* asset name reads
        // identically to the tracked Linux/musl gap.
        .detail(format!(
            "no checksum for {asset} in the release's checksums file"
        ))
    })?;

    ensure_matching_binary(ctx, home, version, &target, &expected, downloads).await
}

/// The part of [`ensure_riabuild_with`] that needs no network beyond the
/// tarball download itself.
async fn ensure_matching_binary(
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
    let binary = download::extract_single_file(&tarball, "riabuild")?;

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
    use crate::runner::FakeRunner;

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
    async fn an_unpublished_architecture_stops_before_anything_is_written() {
        // Scripted with `containing`, not `with`: every remote invocation shares
        // the `ssh -p … -o …` prefix, and the part that distinguishes the digest
        // probe from `uname -sm` is the last argument. A prefix stub on "ssh"
        // answers both and the test passes while exercising nothing.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(
            FakeRunner::new()
                .containing("sha256sum", 1, "", "No such file")
                .containing("uname -sm", 0, "Linux i686\n", ""),
        );
        let error = ensure_riabuild(
            &remote(),
            &paths,
            fake,
            &Ui::new(true),
            "/home/dev",
            "2026.08.06",
        )
        .await
        .expect_err("unsupported");
        // `rust_target`'s error names the architecture; `Failure`'s `Display`
        // only renders `attempting — action` (see `ui.rs`), so the detail is
        // where it lands.
        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(failure.detail.contains("i686"), "{}", failure.detail);
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

    fn make_tarball(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, name, payload)
            .expect("append");
        let tar_bytes = archive.into_inner().expect("finish");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzip");
        encoder.finish().expect("gzip")
    }

    /// Returns the exact tarball for whichever `target` `ensure_riabuild_with`
    /// asks for, and errors on any other — so a bug that hands this function
    /// the wrong argument (say, `target` and `expected` transposed at the
    /// call into `ensure_matching_binary`) is caught immediately rather than
    /// producing a merely-wrong digest later.
    struct FixedDownloads {
        checksums: String,
        tarball: Vec<u8>,
        target: &'static str,
    }

    #[async_trait]
    impl Downloads for FixedDownloads {
        async fn checksums(&self, _version: &str) -> Result<String> {
            Ok(self.checksums.clone())
        }
        async fn tarball(&self, _version: &str, target: &str) -> Result<Vec<u8>> {
            if target != self.target {
                anyhow::bail!(
                    "wrong target requested: {target:?}, expected {:?}",
                    self.target
                );
            }
            Ok(self.tarball.clone())
        }
    }

    #[tokio::test]
    async fn the_composed_install_path_downloads_verifies_and_writes() {
        // Drives `ensure_riabuild_with` end to end — platform detection,
        // checksum lookup, download, digest verification, and the write —
        // against a fixed `Downloads` and a `FakeRunner` sequenced by call
        // order. This is what would fail if `target` and `expected` were
        // ever transposed at the handoff into `ensure_matching_binary`: they
        // are adjacent, same-typed `&str` parameters, and no per-stage test
        // can see that handoff at all.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let remote = remote();
        let version = "2026.08.06";
        let target = "x86_64-unknown-linux-musl";

        let tarball = make_tarball("riabuild", b"a real riabuild binary, or close enough");
        let digest = download::sha256_hex(&tarball);
        let asset = download::riabuild_asset(version, target);
        let downloads = FixedDownloads {
            checksums: format!("{digest}  {asset}\n"),
            tarball,
            target,
        };

        let prefix = ssh_prefix(&remote, &paths);
        let fake = Arc::new(
            FakeRunner::new()
                .then(&prefix, 0, "Linux x86_64\n", "") // uname -sm
                .then(&prefix, 1, "", "No such file") // installed check: nothing there yet
                .then(&prefix, 0, "", "") // the write
                .then(&prefix, 0, &format!("{digest}\n"), ""), // post-write reverify
        );
        let ctx = SshCtx {
            remote: &remote,
            paths: &paths,
            runner: fake,
            ui: &Ui::new(true),
        };

        let path = ensure_riabuild_with(&ctx, "/home/dev", version, &downloads)
            .await
            .expect("installs end to end");
        assert_eq!(path, remote_binary_path("/home/dev", version));
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
