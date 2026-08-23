//! The tools riabuild downloads whole — one shape, one row per tool.
//!
//! `gh`, `infisical`, `ngrok` and Grok Build are all the same four steps:
//! download the pinned release, verify it against a digest, land it under
//! `~/.riabuild/<tool>/<version>/`, and put something in `~/.riabuild/bin` that
//! the developer's shell finds first. They were four copies of that, and copies
//! drift: only `ngrok` checked its own shim, so a deleted `bin/gh` reported a
//! satisfied machine while the shell went on finding whatever `gh` the laptop
//! already had; and only `infisical` and `ngrok` read the version banner off
//! stderr, so a build that banners on the other stream read as a corrupted
//! download.
//!
//! One row per tool is what makes those structural rather than optional. The
//! root `CLAUDE.md` rule — riabuild owns every tool it installs and verifies it
//! against a digest — is a property of `Release` and of this file, and a new
//! owned tool gets it by being a row rather than by remembering to.
//!
//! **The rows are not all the same, and the differences are data.** ngrok's
//! entry in `bin/` is not an `exec` line: it fetches the team's authtoken from
//! riabuild-web on every invocation and hands it over in the environment, which
//! is the whole reason that token lands on no filesystem. Grok Build's is nine
//! numbered launchers its own task writes, so its row asks for none. And two
//! tools have work around the download that is genuinely theirs: `github_cli`
//! signs the developer in and asks GitHub about their membership, `grok_cli`
//! makes nine profile directories. Those two keep their own `Task` and *use* a
//! row; `infisical` and `ngrok` are nothing but a row, so the row is their task.
//!
//! What is deliberately **not** here: the Codex CLI. It is an npm package
//! installed with the Node riabuild owns — no release, no asset, no digest of
//! its own, and a `toolchain` edge — so a row describing it would have to be
//! empty in every field this table exists for.

use crate::shims;
use crate::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::tools::{self, Release};
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;
use std::path::Path;

/// What `~/.riabuild/bin/<name>` has to contain, for a tool that has one.
#[derive(Clone, Copy)]
pub(crate) struct Shim {
    /// The file in `bin/`, which is also the name the developer types.
    pub name: &'static str,
    /// Renders it, given riabuild's own binary and the tool's versioned path.
    ///
    /// A function rather than a flag because the one interesting shim —
    /// ngrok's — is a credential-carrying script and not an `exec` line, and
    /// flattening that into "an exec shim, plus a special case" is how the
    /// authtoken ends up somewhere it can be read twice.
    pub render: fn(riabuild: &Path, binary: &Path) -> String,
    /// What a developer loses while it is missing, in their words.
    pub without_it: &'static str,
}

/// One owned tool.
///
/// `Copy`, because each row is a `static` that `registry()` hands out by value
/// as a `Box<dyn Task>` and that `table()` hands out by reference.
#[derive(Clone, Copy)]
pub(crate) struct OwnedTool {
    pub id: TaskId,
    /// What the task ladder calls it.
    pub title: &'static str,
    /// What a reason calls the tool itself — the command name where there is
    /// one (`gh`), the product's name where the command is not the point
    /// (`Grok Build`).
    pub label: &'static str,
    pub version: u32,
    /// Low enough that only a truncated or half-written download fails it. A
    /// *bump* of the pin is caught by the path instead: the binary lives under
    /// its version, so a new pin is a file that is not there yet.
    pub min_version: &'static str,
    pub pinned_version: &'static str,
    pub release: fn() -> Result<Release>,
    /// Where riabuild's own calls find the binary — `Ctx::gh` and friends,
    /// never the bare name.
    pub binary: fn(&Ctx) -> String,
    /// The environment the `--version` probe runs in.
    pub probe: fn(&Ctx) -> RunOptions,
    pub shim: Option<Shim>,
    /// What `Failure::attempting` says when the download fails.
    pub installing: &'static str,
    /// Something to say after a successful `apply()`, if there is anything.
    pub note: fn(&Ctx) -> Option<String>,
}

/// The default for a tool whose probe needs nothing but the binary.
pub(crate) fn plain_probe(_ctx: &Ctx) -> RunOptions {
    RunOptions::default()
}

/// The default for a tool with nothing to say after it installs.
pub(crate) fn no_note(_ctx: &Ctx) -> Option<String> {
    None
}

/// The ordinary shim: hand straight over to the versioned binary.
pub(crate) fn exec_shim(_riabuild: &Path, binary: &Path) -> String {
    shims::exec_shim(binary)
}

/// What the copy riabuild owns has to say for itself.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Installed {
    /// There and runnable.
    Usable,
    /// Not there at all.
    Missing,
    /// There, and reporting this — which is not a version we can use.
    Unusable(String),
}

impl OwnedTool {
    /// Asks the installed copy what it is.
    ///
    /// Asked by `check()` and again by `apply()`, which is the point. Asking
    /// about the *version* rather than the file's existence is what keeps
    /// `apply()`'s skip-the-download shortcut honest: a truncated download
    /// would otherwise be left in place for a `check()` that could then never
    /// go green — a check its own repair cannot satisfy.
    pub(crate) async fn installed(&self, ctx: &Ctx) -> Result<Installed> {
        let binary = (self.binary)(ctx);
        // Existence before invocation: `RealRunner::run` returns `Err` when the
        // program is not there — a spawn failure, not an exit code — so asking
        // `--version` first would propagate an `anyhow` chain instead of
        // reaching the install.
        if !tokio::fs::try_exists(&binary).await.unwrap_or(false) {
            return Ok(Installed::Missing);
        }
        let output = ctx
            .runner
            .run(&binary, &["--version"], &(self.probe)(ctx))
            .await?;
        // Both streams, for every tool. Infisical and ngrok banner on stderr in
        // some builds, and a reader that only watched stdout called a perfectly
        // good install corrupted — which is a thing that can be true of any of
        // them, so none of them gets to be the one that only reads half.
        let reported = format!("{}{}", output.stdout, output.stderr);
        if version::at_least(&reported, self.min_version) {
            Ok(Installed::Usable)
        } else {
            Ok(Installed::Unusable(reported.trim().to_string()))
        }
    }

    /// Why this tool is not in the shape riabuild wants, or `None` when it is.
    ///
    /// The whole of what a row can observe: the binary, its version, and the
    /// shim. A task with more to check — a sign-in, a set of profile
    /// directories — asks this first and then asks its own questions.
    pub(crate) async fn drift(&self, ctx: &Ctx) -> Result<Option<String>> {
        match self.installed(ctx).await? {
            Installed::Missing => {
                return Ok(Some(format!(
                    "riabuild has not installed {} {} yet",
                    self.label, self.pinned_version
                )));
            }
            // The owned copy is a known version, so a low one means a truncated
            // or half-written download rather than an old release.
            Installed::Unusable(reported) => {
                return Ok(Some(format!(
                    "the {} in ~/.riabuild reports `{reported}`, which is not usable",
                    self.label
                )));
            }
            Installed::Usable => {}
        }
        self.shim_drift(ctx).await
    }

    /// Why `~/.riabuild/bin/<name>` is not the file riabuild writes.
    ///
    /// Every tool with a shim is checked, and that is the fix rather than a
    /// tidy-up: `bin/` leads `PATH` inside the environment shell, so a shim
    /// that is missing does not degrade to "riabuild's copy is not first" — it
    /// degrades to "the machine's own copy answers, and nothing riabuild
    /// verified is in the picture". Comparing the *text* rather than the file's
    /// existence is what catches a shim written by an older riabuild, whose own
    /// path moved underneath it.
    async fn shim_drift(&self, ctx: &Ctx) -> Result<Option<String>> {
        let Some(shim) = &self.shim else {
            return Ok(None);
        };
        let path = ctx.paths.bin_dir().join(shim.name);
        let wanted = self.shim_script(ctx, shim)?;
        Ok(match tokio::fs::read_to_string(&path).await {
            Ok(found) if found == wanted => None,
            Ok(_) => Some(format!(
                "the {} launcher in ~/.riabuild/bin is not the one this riabuild writes",
                self.label
            )),
            Err(_) => Some(format!(
                "{} is installed but has no launcher in ~/.riabuild/bin, {}",
                self.label, shim.without_it
            )),
        })
    }

    /// What the shim should contain right now.
    ///
    /// riabuild's own path is handed to every render, though only ngrok's uses
    /// it. That costs nothing a run does not already pay: `running_binary` is
    /// `current_exe`, and `provision::write_launchers_with` asks it once per
    /// run regardless — a machine that cannot answer has no shims at all, which
    /// is exactly what it should fail as.
    fn shim_script(&self, ctx: &Ctx, shim: &Shim) -> Result<String> {
        let binary = (self.binary)(ctx);
        Ok((shim.render)(&shims::running_binary()?, Path::new(&binary)))
    }

    /// Writes `~/.riabuild/bin/<name>`, for a tool that has one.
    pub(crate) async fn write_shim(&self, ctx: &Ctx) -> Result<()> {
        let Some(shim) = &self.shim else {
            return Ok(());
        };
        let script = self.shim_script(ctx, shim)?;
        shims::write_script(&ctx.paths.bin_dir(), shim.name, &script).await
    }

    /// Downloads the pinned release, whatever is there already.
    pub(crate) async fn download(&self, ctx: &mut Ctx) -> Result<()> {
        let release = (self.release)()?;
        ctx.ui
            .note(&format!("Downloading {} {}…", self.label, release.version));
        let tool_dir = ctx.paths.tool_dir(release.tool, release.version);
        tools::install(&release, &tool_dir).await.map_err(|error| {
            Failure::new(
                self.installing,
                "Check your network connection and run `riabuild` again. If it keeps \
                 failing, send this to your team lead.",
            )
            .detail(format!("{error:#}"))
        })?;
        Ok(())
    }

    /// Downloads the pinned release and writes the shim.
    ///
    /// The unconditional half of `ensure`, for the one caller that has already
    /// decided: `internal::seed_github`, which installs `gh` on a server before
    /// any setup pass has run.
    pub(crate) async fn install(&self, ctx: &mut Ctx) -> Result<()> {
        self.download(ctx).await?;
        self.write_shim(ctx).await
    }

    /// Brings the machine to what this row describes.
    ///
    /// The download is **not** unconditional. An `apply()` runs for a drifted
    /// shim far more often than for a missing binary — the shim names
    /// riabuild's own versioned path, so every riabuild release rewrites it —
    /// and re-fetching between 12 and 167 MB to rewrite six lines of shell
    /// would make each release a download every laptop pays for.
    pub(crate) async fn ensure(&self, ctx: &mut Ctx) -> Result<()> {
        if self.installed(ctx).await? != Installed::Usable {
            self.download(ctx).await?;
        }
        self.write_shim(ctx).await
    }
}

/// Every row, for the things that are true of all of them.
pub(crate) fn table() -> Vec<&'static OwnedTool> {
    vec![
        &crate::github_cli::GH,
        &crate::infisical_cli::INFISICAL_CLI,
        &crate::ngrok::NGROK,
        &crate::grok_cli::GROK,
    ]
}

/// A row with nothing of its own to do is its own task.
///
/// `infisical` and `ngrok` are exactly the four steps in the module header and
/// nothing else, so there is no `impl Task` for either of them to carry. `gh`
/// and Grok Build have work around the download that is genuinely theirs and
/// keep their own, delegating this half to `drift` and `ensure`.
#[async_trait]
impl Task for OwnedTool {
    fn id(&self) -> TaskId {
        self.id
    }

    fn title(&self) -> &str {
        self.title
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        Ok(match self.drift(ctx).await? {
            Some(detail) => Status::needs(detail),
            None => Status::Satisfied,
        })
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        self.ensure(ctx).await?;
        if let Some(note) = (self.note)(ctx) {
            ctx.ui.note(&note);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, ctx_with_tools};
    use riabuild_fetch::tools::Checksum;
    use riabuild_runner::FakeRunner;

    /// Every owned tool, reporting a version well above its floor.
    fn reporting_current() -> FakeRunner {
        FakeRunner::new()
            .with("gh --version", 0, "gh version 2.97.0 (2026-07-02)", "")
            .with("infisical --version", 0, "Infisical CLI v0.43.120", "")
            .with("ngrok --version", 0, "ngrok version 3.39.11", "")
            .with("grok --version", 0, "grok 1.0.5 (5115b46bc9)", "")
    }

    #[test]
    fn every_row_verifies_what_it_downloads_against_a_digest() {
        // The root rule this table exists to make structural: riabuild owns
        // every tool it installs, and a tool is owned only if the bytes were
        // checked. A row is the only way to be in this list, so a new tool
        // cannot arrive without an answer here.
        for tool in table() {
            let release = (tool.release)().expect("a release for this platform");
            match release.checksum {
                Checksum::Published(ref urls) => assert!(!urls.is_empty(), "{}", tool.id),
                Checksum::Pinned(digest) => assert!(!digest.is_empty(), "{}", tool.id),
            }
        }
    }

    #[test]
    fn every_row_names_a_task_that_is_registered() {
        let ids: Vec<TaskId> = crate::registry().iter().map(|task| task.id()).collect();
        for tool in table() {
            assert!(ids.contains(&tool.id), "{} is in no registry", tool.id);
        }
    }

    #[tokio::test]
    async fn a_bare_machine_asks_for_every_tool_by_name() {
        let (ctx, _home) = ctx_with(reporting_current()).await;
        for tool in table() {
            let drift = tool
                .drift(&ctx)
                .await
                .unwrap()
                .expect("nothing is installed");
            assert!(drift.contains(tool.label), "{drift}");
            assert!(drift.contains(tool.pinned_version), "{drift}");
        }
    }

    #[tokio::test]
    async fn a_deleted_shim_is_drift_for_every_tool_that_has_one() {
        // The bug this table was folded to remove. `bin/` leads `PATH` in the
        // environment shell, so a missing `bin/gh` is not "riabuild's copy is
        // second" — it is the machine's own gh answering, unverified, while
        // riabuild reports a satisfied machine.
        let (ctx, _home) = ctx_with_tools(reporting_current()).await;
        for tool in table() {
            let Some(shim) = &tool.shim else { continue };
            assert_eq!(tool.drift(&ctx).await.unwrap(), None, "{}", tool.id);
            tokio::fs::remove_file(ctx.paths.bin_dir().join(shim.name))
                .await
                .unwrap();
            let drift = tool.drift(&ctx).await.unwrap().expect("a deleted shim");
            assert!(drift.contains("no launcher"), "{drift}");
            tool.write_shim(&ctx).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_shim_an_older_riabuild_wrote_is_replaced_rather_than_kept() {
        // Self-update moves riabuild's own path and the tool's versioned path,
        // and a shim records both. A shim left pointing at either is drift the
        // check has to see: the file is there, and it is wrong.
        let (ctx, _home) = ctx_with_tools(reporting_current()).await;
        for tool in table() {
            let Some(shim) = &tool.shim else { continue };
            let stale = (shim.render)(
                Path::new("/opt/homebrew/bin/riabuild"),
                Path::new("/usr/local/bin/whatever"),
            );
            crate::testing::write_file(&ctx.paths.bin_dir().join(shim.name), &stale).await;
            let drift = tool.drift(&ctx).await.unwrap().expect("a stale shim");
            assert!(
                drift.contains("not the one this riabuild writes"),
                "{drift}"
            );
        }
    }

    #[tokio::test]
    async fn a_version_banner_on_stderr_is_read_for_every_tool() {
        // Some builds print it there. A reader that watched stdout alone called
        // a perfectly good install corrupted, and that could always have been
        // true of any of them — it was only ever noticed for two.
        for tool in table() {
            let release = (tool.release)().expect("a release for this platform");
            let command = Path::new(release.member)
                .file_name()
                .expect("the binary has a name")
                .to_string_lossy()
                .into_owned();
            let runner =
                FakeRunner::new().with(&format!("{command} --version"), 0, "", "version 99.0.0");
            let (ctx, _home) = ctx_with_tools(runner).await;
            assert_eq!(
                tool.installed(&ctx).await.unwrap(),
                Installed::Usable,
                "{}",
                tool.id
            );
        }
    }
}
