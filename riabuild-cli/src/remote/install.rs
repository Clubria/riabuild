//! Getting the right riabuild onto a server.
//!
//! Downloaded and verified on the laptop, then streamed over SSH. That keeps
//! digest verification in the one place that already does it properly, and
//! needs nothing installed on the server but a shell.
//!
//! This file answers *which* riabuild a given server needs and what it must
//! hash to: it asks the server what it is, derives the Rust target from the
//! answer, and looks the expected digest out of the release's checksums.
//! What then happens to the file at its path on the server is
//! `install/binary.rs`, split off because the two halves together ran past
//! the crate's ~300-line production budget.

mod binary;

use super::{Remote, identity, ssh_once};
use crate::download;
use crate::paths::Paths;
use crate::runner::{CommandOutput, CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use async_trait::async_trait;
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
                    ..super::askpass::run_options(self.remote, self.paths)
                },
            )
            .await
    }
}

/// The network calls made before anything is trusted — a seam so the
/// *composed* install path (`ensure_riabuild` through `write_binary`) is
/// testable end to end without a real GitHub release to fetch, mirroring how
/// `CommandRunner` seams out `ssh`. `target` and `expected` are adjacent,
/// same-typed `&str` parameters handed into [`binary::ensure_matching_binary`];
/// with each stage previously only testable in isolation, transposing them
/// would have compiled and passed every existing test silently. `RealDownloads`
/// is what production uses; tests substitute a fixed pair of responses.
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
            "Use a server riabuild publishes a build for: macOS or Linux, on x86_64 or \
             arm64.",
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

    binary::ensure_matching_binary(ctx, home, version, &target, &expected, downloads).await
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

    /// The exact prefix `identity::ssh_options` plus the login target
    /// produces — shared by every command sent to `remote`, so it is what
    /// lets `FakeRunner::then` sequence responses to *successive* remote
    /// calls in order, regardless of which trailing command each one sends.
    fn ssh_prefix(remote: &Remote, paths: &dyn Paths) -> String {
        let options = identity::ssh_options(remote, paths, true).join(" ");
        format!("ssh {options} {}", remote.target())
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

        // Two digests, and keeping them apart is the subject of this test. The
        // release publishes the **archive's**; what lands on the server is the
        // **binary's**. Scripting the post-write check below with `digest`
        // rather than `binary_digest` is what let a version ship in which
        // `ensure_matching_binary` compared a binary against an archive — the
        // fixture answered with whatever the assertion was about to compare.
        let payload = b"a real riabuild binary, or close enough";
        let tarball = make_tarball("riabuild", payload);
        let digest = download::sha256_hex(&tarball);
        let binary_digest = download::sha256_hex(payload);
        assert_ne!(digest, binary_digest, "otherwise this proves nothing");
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
                .then(&prefix, 0, &format!("{binary_digest}\n"), ""), // post-write reverify
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
        assert_eq!(path, binary::remote_binary_path("/home/dev", version));
    }
}
