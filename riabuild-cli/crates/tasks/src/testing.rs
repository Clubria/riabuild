//! Test scaffolding: a `Ctx` pointed at a tempdir, with every external process
//! and secret store faked.
//!
//! This is what makes each `check()` a pure unit test instead of something that
//! needs a real machine in a real state.

#![cfg(any(test, feature = "testing"))]

use crate::Ctx;
use riabuild_api::ApiClient;
use riabuild_api::org::OrgConfig;
use riabuild_keychain::{Keychain, MemoryKeychain};
use riabuild_paths::config::{State, UserConfig};
use riabuild_paths::{Paths, RealPaths};
use riabuild_runner::{CommandRunner, FakeRunner};
use riabuild_ui::Ui;
use std::sync::Arc;
use tempfile::TempDir;

pub fn org_config() -> OrgConfig {
    OrgConfig {
        repo_slug: "Clubria/ai-builders-hub".into(),
        min_cli_version: "0.1.0".into(),
        latest_cli_version: "0.1.0".into(),
        secrets_updated_at: 0,
    }
}

pub async fn test_ctx() -> (Ctx, TempDir) {
    let (ctx, home, _) = build(
        FakeRunner::new(),
        MemoryKeychain::with_token("rb_test_token"),
    )
    .await;
    (ctx, home)
}

pub async fn ctx_with(runner: FakeRunner) -> (Ctx, TempDir) {
    let (ctx, home, _) = ctx_and_runner(runner).await;
    (ctx, home)
}

/// A `Ctx` on a machine where riabuild has already installed the tools it owns.
///
/// The file contents are irrelevant — every invocation goes through
/// `FakeRunner` — but the files must exist, because their existence is exactly
/// what `check()` uses to tell a provisioned machine from a bare one. Tests
/// that want the bare case use `ctx_with` and assert the task asks to install.
pub async fn ctx_with_tools(runner: FakeRunner) -> (Ctx, TempDir) {
    let (ctx, home) = ctx_with(runner).await;
    install_owned_tools(&ctx).await;
    (ctx, home)
}

pub async fn install_owned_tools(ctx: &Ctx) {
    for binary in [ctx.gh(), ctx.infisical()] {
        write_file(std::path::Path::new(&binary), "#!/bin/sh\n").await;
    }
}

/// Like `ctx_with`, but hands back the runner as well.
///
/// Some things a task must get right are only visible in *what it ran*: that a
/// developer whose machine is already fine is not sent through a browser
/// sign-in, for instance, cannot be seen in the returned `Status` at all.
pub async fn ctx_and_runner(runner: FakeRunner) -> (Ctx, TempDir, Arc<FakeRunner>) {
    build(runner, MemoryKeychain::with_token("rb_test_token")).await
}

async fn build(runner: FakeRunner, keychain: MemoryKeychain) -> (Ctx, TempDir, Arc<FakeRunner>) {
    let home = TempDir::new().expect("tempdir");
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create ~/.riabuild");

    let fake = Arc::new(runner);
    let runner: Arc<dyn CommandRunner> = fake.clone();
    let keychain: Arc<dyn Keychain> = Arc::new(keychain);

    let ctx = Ctx {
        paths,
        runner,
        keychain,
        api: ApiClient::new("0.1.0"),
        // Deliberately *not* interactive. `cargo test` gives the process no
        // tty, and this models that honestly — which is also what a CI job
        // has. An earlier version of this file called
        // `.assume_prompts_work(true)` here so that tests of the interactive
        // path would exercise it; the cost was that every other test silently
        // became interactive too, including the ones asserting that riabuild
        // refuses rather than assumes when there is nobody to ask.
        //
        // So the default is the unattended machine, and a test that wants a
        // developer at the keyboard says so: `ctx.ui =
        // Ui::new(true).assume_prompts_work(true);`, or `Ui::scripted([...])`
        // to supply the answers as well.
        ui: Ui::new(true),
        config: UserConfig::default(),
        state: State::default(),
        org: Some(org_config()),
        member: None,
        server: None,
        cli_version: "0.1.0".into(),
        env: Vec::new(),
        notes: Vec::new(),
        dry_run: false,
    };
    (ctx, home, fake)
}

/// Writes a file and every directory above it.
pub async fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .expect("create parent");
    }
    tokio::fs::write(path, contents).await.expect("write file");
}
