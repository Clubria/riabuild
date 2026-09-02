//! The TypeScript language server used by Claude Code's official TypeScript plugin.
//!
//! The plugin supplies the LSP integration, not the executable it starts. Keep the
//! server and TypeScript in riabuild's Node prefix so the environment shell finds
//! pinned, npm-integrity-verified copies without needing a system Node or npm.

use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;

const SERVER_PACKAGE: &str = "typescript-language-server";
const SERVER_VERSION: &str = "6.0.0";
const TYPESCRIPT_PACKAGE: &str = "typescript";
const TYPESCRIPT_VERSION: &str = "7.0.2";

pub struct TypescriptLanguageServer;

#[async_trait]
impl Task for TypescriptLanguageServer {
    fn id(&self) -> TaskId {
        "typescript_language_server"
    }

    fn title(&self) -> &str {
        "TypeScript language server"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["toolchain"]
    }

    // npm global installs read and rewrite the same Node prefix as the Claude
    // and Codex installs. The resource serialises those operations without
    // inventing a dependency between otherwise independent tools.
    fn writes(&self) -> &[Resource] {
        &["node_prefix"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(node_version) = &ctx.config.node_version else {
            return Ok(Status::needs("Node is not installed yet"));
        };
        let bin = ctx.paths.node_dir(node_version).join("bin");
        for (name, wanted, label) in [
            (
                "typescript-language-server",
                SERVER_VERSION,
                "TypeScript language server",
            ),
            ("tsc", TYPESCRIPT_VERSION, "TypeScript"),
        ] {
            let executable = bin.join(name);
            if !tokio::fs::try_exists(&executable).await.unwrap_or(false) {
                return Ok(Status::needs(format!("{label} is not installed yet")));
            }
            let output = ctx
                .runner
                .run(
                    &executable.to_string_lossy(),
                    &["--version"],
                    &probe_options(&bin),
                )
                .await?;
            if !output.ok() || !version::same(output.trimmed(), wanted) {
                return Ok(Status::needs(format!(
                    "{label} reports `{}`, and riabuild installs {wanted}",
                    output.trimmed()
                )));
            }
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(node_version) = &ctx.config.node_version else {
            return Err(Failure::new(
                "installing the TypeScript language server",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .into());
        };
        let node_dir = ctx.paths.node_dir(node_version);
        let bin = node_dir.join("bin");
        let npm = bin.join("npm");
        if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
            return Err(Failure::new(
                "installing the TypeScript language server",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .detail(format!("{} does not exist", npm.display()))
            .into());
        }

        ctx.ui.note("Installing the TypeScript language server…");
        let prefix = node_dir.to_string_lossy().into_owned();
        let server = format!("{SERVER_PACKAGE}@{SERVER_VERSION}");
        let typescript = format!("{TYPESCRIPT_PACKAGE}@{TYPESCRIPT_VERSION}");
        let output = ctx
            .runner
            .run(
                &npm.to_string_lossy(),
                &[
                    "install",
                    "-g",
                    "--ignore-scripts",
                    "--prefix",
                    &prefix,
                    &server,
                    &typescript,
                ],
                &install_options(&bin),
            )
            .await?;
        if !output.ok() {
            return Err(Failure::new(
                "installing the TypeScript language server",
                "Check your network connection and run `riabuild` again.",
            )
            .command(format!("npm install -g {server} {typescript}"))
            .detail(output.stderr)
            .into());
        }
        Ok(())
    }
}

fn probe_options(node_bin: &std::path::Path) -> RunOptions {
    RunOptions {
        env: vec![path_led_by(node_bin)],
        ..Default::default()
    }
}

fn install_options(node_bin: &std::path::Path) -> RunOptions {
    RunOptions {
        timeout: Some(std::time::Duration::from_secs(1800)),
        ..probe_options(node_bin)
    }
}

fn path_led_by(dir: &std::path::Path) -> (String, String) {
    let ambient = std::env::var("PATH").unwrap_or_default();
    ("PATH".to_string(), format!("{}:{ambient}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    const NODE: &str = "22.23.1";

    async fn installed_ctx(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with(runner).await;
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .expect("config");
        let bin = ctx.paths.node_dir(NODE).join("bin");
        write_file(&bin.join("typescript-language-server"), "#!/bin/sh\n").await;
        write_file(&bin.join("tsc"), "#!/bin/sh\n").await;
        (ctx, home)
    }

    #[tokio::test]
    async fn exact_server_and_typescript_versions_are_satisfied() {
        let runner = FakeRunner::new()
            .with(
                "typescript-language-server --version",
                0,
                SERVER_VERSION,
                "",
            )
            .with(
                "tsc --version",
                0,
                &format!("Version {TYPESCRIPT_VERSION}"),
                "",
            );
        let (ctx, _home) = installed_ctx(runner).await;
        assert_eq!(
            TypescriptLanguageServer.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn a_missing_server_is_detected_without_running_it() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = TypescriptLanguageServer.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("Node is not installed"));
    }

    #[test]
    fn packages_are_exactly_pinned() {
        assert_eq!(
            format!("{SERVER_PACKAGE}@{SERVER_VERSION}"),
            "typescript-language-server@6.0.0"
        );
        assert_eq!(
            format!("{TYPESCRIPT_PACKAGE}@{TYPESCRIPT_VERSION}"),
            "typescript@7.0.2"
        );
    }
}
