//! Getting the right riabuild onto a server.
//!
//! Downloaded and verified on the laptop, then streamed over SSH. That keeps
//! digest verification in the one place that already does it properly, and
//! needs nothing installed on the server but a shell.

use super::{Remote, identity, shell_command, shell_quote, ssh_once};
use crate::download;
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

/// Where riabuild's own layout (`paths::riabuild_dir`) lands when evaluated
/// against a *remote* home rather than this laptop's. `RealPaths::with_root`
/// exists precisely so this is derived rather than formatted a second time —
/// see R10 in `decisions.md`.
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

/// `sha256sum` on Linux, `shasum -a 256` on macOS; whichever exists prints the
/// digest as its first word.
fn digest_command(path: &str) -> String {
    shell_command(&format!(
        "if command -v sha256sum >/dev/null 2>&1; then sha256sum {q}; \
         else shasum -a 256 {q}; fi | cut -d' ' -f1",
        q = shell_quote(path)
    ))
}

/// Installs riabuild `version` on `remote`, if it is not already there, and
/// returns its absolute path.
///
/// `home` is the server's own home directory, from `remote::resolve_home` —
/// never a `~` (R1). Platform detection and checksum verification happen
/// here, before anything is compared against the expected digest, so that
/// digest always exists before its first use (R8(b) in `decisions.md`).
#[allow(dead_code)] // consumed by Task 18, via session::ensure
pub async fn ensure_riabuild(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    home: &str,
    version: &str,
) -> Result<String> {
    let platform = ssh_once(remote, paths, runner.clone(), "uname -sm").await?;
    if !platform.ok() {
        return Err(Failure::new(
            format!("asking {} what it is", remote.host),
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
            format!("installing riabuild on {}", remote.host),
            "Use a server riabuild publishes a build for: Linux or macOS, on x86_64 or arm64.",
        )
        .detail(error.to_string())
    })?;

    // Computed before it is compared against anything or trusted to skip an
    // install — see R8(b).
    let checksums = download::fetch_text(&download::riabuild_checksums_url(version)).await?;
    let asset = download::riabuild_asset(version, &target);
    let expected = download::digest_for(&checksums, &asset).ok_or_else(|| {
        Failure::new(
            format!("verifying the riabuild {version} download"),
            "Tell your team lead — the release is missing a checksum for this platform.",
        )
    })?;

    ensure_matching_binary(remote, paths, runner, ui, home, version, &target, &expected).await
}

/// The part of [`ensure_riabuild`] that needs no network beyond the tarball
/// download itself. Split out so "already correct, nothing to do" is
/// testable against a `FakeRunner` alone, with `expected` supplied already
/// resolved rather than requiring a real fetch of
/// `riabuild-*-checksums.txt` — which does not exist for a test's made-up
/// version — in every test that exercises it.
#[allow(clippy::too_many_arguments)]
async fn ensure_matching_binary(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    home: &str,
    version: &str,
    target: &str,
    expected: &str,
) -> Result<String> {
    let path = remote_binary_path(home, version);

    // Trusted by digest, never by the version it claims. A co-tenant can put
    // a script at this path that prints any version string it likes, and
    // every other developer on a shared account would then execute it with
    // their session token in the environment. `sha256sum`/`shasum` is asked
    // for the digest of what is actually there.
    let installed = ssh_once(remote, paths, runner.clone(), &digest_command(&path)).await?;
    if installed.ok() && installed.trimmed() == expected {
        return Ok(path);
    }

    ui.working("riabuild", &format!("installing {version} on the server"));

    // Verified against `expected` before a single byte is extracted or sent
    // anywhere near the server.
    let tarball = download::fetch_bytes(&download::riabuild_asset_url(version, target)).await?;
    if download::sha256_hex(&tarball) != expected {
        return Err(Failure::new(
            format!("verifying the riabuild {version} download"),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail("the download did not match its published digest")
        .into());
    }
    let binary = download::extract_single_file(&tarball, "riabuild")?;

    write_binary(remote, paths, runner, ui, home, version, expected, binary).await
}

/// Streams already-verified bytes onto the server and confirms what landed —
/// split out from [`ensure_matching_binary`] so the write, and its failure
/// and corruption paths, are testable against a `FakeRunner` without ever
/// downloading anything.
#[allow(clippy::too_many_arguments)]
async fn write_binary(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
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
    let final_path = format!("{dir}/riabuild");
    let quoted_dir = shell_quote(&dir);
    let quoted_part = shell_quote(&part);
    let quoted_final = shell_quote(&final_path);
    let install = shell_command(&format!(
        "umask 077 && mkdir -p {quoted_dir} && cat > {quoted_part} && \
         chmod 755 {quoted_part} && mv {quoted_part} {quoted_final}"
    ));
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(install);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let written = runner
        .run(
            "ssh",
            &refs,
            &RunOptions {
                stdin: Some(binary),
                ..Default::default()
            },
        )
        .await?;
    if !written.ok() {
        return Err(Failure::new(
            format!("installing riabuild on {}", remote.host),
            "Check there is space in your home directory on that server, then run `riabuild remote` again.",
        )
        .detail(written.stderr)
        .into());
    }

    // Re-verified after writing: a truncated or corrupted transfer must not
    // be reported as a successful install.
    let confirmed = ssh_once(remote, paths, runner.clone(), &digest_command(&path)).await?;
    if !confirmed.ok() || confirmed.trimmed() != expected {
        return Err(Failure::new(
            format!("installing riabuild on {}", remote.host),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail(format!(
            "the server reports {:?} after installing {version}",
            confirmed.trimmed()
        ))
        .into());
    }
    ui.applied("riabuild");
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
        // Exercises `ensure_matching_binary` directly, with `expected` already
        // resolved: `ensure_riabuild` itself always fetches
        // `riabuild-*-checksums.txt` first (R8(b) — that digest must exist
        // before anything is compared against it), which is a real network call
        // this test has no business making, and no real release exists for a
        // made-up test version anyway.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        // The digest of what is on disk, not the version it claims. A co-tenant
        // can put a script at that path that prints any version string.
        let fake =
            Arc::new(FakeRunner::new().containing("sha256sum", 0, &format!("{EXPECTED}\n"), ""));

        let path = ensure_matching_binary(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "/home/dev",
            "2026.08.06",
            "aarch64-apple-darwin",
            EXPECTED,
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

        let path = write_binary(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
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

        let error = write_binary(
            &remote(),
            &paths,
            fake,
            &Ui::new(true),
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
    async fn a_corrupted_transfer_is_caught_after_writing() {
        // The write itself reported success, but what the server can now read
        // back does not match — a truncated or interleaved transfer, say. This
        // must not be reported as an install that worked.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(
            FakeRunner::new()
                .containing("mkdir -p", 0, "", "")
                .containing("sha256sum", 0, "deadbeef\n", ""),
        );

        let error = write_binary(
            &remote(),
            &paths,
            fake,
            &Ui::new(true),
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
    }
}
