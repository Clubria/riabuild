//! Test scaffolding: a `Ctx` pointed at a tempdir, with every external process
//! and secret store faked.
//!
//! This is what makes each `check()` a pure unit test instead of something that
//! needs a real machine in a real state.

#![cfg(any(test, feature = "testing"))]
// This file is a fixture builder, but it is compiled into the *lib* target of
// every crate that turns `testing` on, so `cfg_attr(test, …)` in the crate root
// does not reach it and the panic lints apply as they would to production. They
// should not: a tempdir that cannot be made is a broken machine running the
// suite, and the only useful thing to do about it is stop with the message.
//
// Scoped to this file on purpose. The crate root used to carry the same
// exemption, which put it around the tasks as well — the register calls that
// out as I066 — and a module-sized hole in a file that ships in no binary is
// the version of it worth keeping.
#![allow(clippy::expect_used)]

use crate::Ctx;
use riabuild_api::ApiClient;
use riabuild_api::org::OrgConfig;
use riabuild_keychain::{Keychain, MemoryKeychain};
use riabuild_paths::config::{State, UserConfig};
use riabuild_paths::{Paths, RealPaths};
use riabuild_runner::{CommandRunner, FakeRunner, RunOptions};
use riabuild_ui::Ui;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

pub fn org_config() -> OrgConfig {
    OrgConfig {
        repo_slug: "Clubria/ai-builders-hub".into(),
        min_cli_version: "0.1.0".into(),
        latest_cli_version: "0.1.0".into(),
        secrets_updated_at: 0,
        // A developer who may see staging, which is the case with two files to
        // get right. Tests for the narrower case set this to `["dev"]`.
        secret_environments: vec!["dev".into(), "staging".into()],
        ngrok_authtoken_updated_at: 0,
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

/// The binaries **and** the shims in `~/.riabuild/bin` that name them.
///
/// Both halves, because both are what an owned tool's `check()` asks about: the
/// shim is what leads `PATH` in the environment shell, so a machine with the
/// binary and no shim is one where the developer's own copy answers. A test
/// that wants that machine deletes the shim afterwards and says so.
pub async fn install_owned_tools(ctx: &Ctx) {
    for binary in [ctx.gh(), ctx.infisical(), ctx.ngrok(), ctx.grok()] {
        write_file(std::path::Path::new(&binary), "#!/bin/sh\n").await;
    }
    for tool in crate::owned_tool::table() {
        tool.write_shim(ctx).await.expect("write the shim");
    }
}

/// A `Ctx` shaped like a **managed server**: `root()` is a per-developer
/// namespace under `~/.riabuild-remote/<member-id>`, and `tools_root()` stays at
/// `~/.riabuild` where every developer on the box shares one toolchain.
///
/// The laptop shape every other helper here builds collapses the two into one
/// directory, so a path that must be the *shared* one and a path that must be
/// the *namespaced* one are indistinguishable in those tests. Anything that has
/// to be right on a server needs this instead — `claude_statusline` is the case
/// that proved it, having shipped for months writing its script somewhere the
/// org settings never named.
pub async fn ctx_on_a_server(runner: FakeRunner) -> (Ctx, TempDir) {
    let (mut ctx, home, _) = build(runner, MemoryKeychain::with_token("rb_test_token")).await;
    let root =
        riabuild_paths::remote_namespace(home.path(), "550e8400-e29b-41d4-a716-446655440000");
    tokio::fs::create_dir_all(&root)
        .await
        .expect("create the namespace");
    ctx.paths = Arc::new(RealPaths::with_root(home.path(), &root));
    ctx.server = Some("build-01".to_string());
    (ctx, home)
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
        repo: None,
        member: None,
        server: None,
        cli_version: "0.1.0".into(),
        env: Vec::new(),
        notes: Vec::new(),
        dry_run: false,
    };
    (ctx, home, fake)
}

/// The bound every command was given, recorded through the same `Delegating`
/// base the production wrappers are built on.
///
/// `FakeRunner` records argv, the environment and stdin; it does not record
/// `RunOptions.timeout`. Without this a call site's patience is invisible to
/// the suite, which is how a genuinely long call — a clone, a package install —
/// ends up held to a default ceiling nobody meant to apply to it, and fails on
/// a slow link for a developer whose machine is fine.
///
/// A `Decoration` rather than a second fake, so what is recorded is what the
/// wrapped runner is actually asked for.
#[derive(Clone, Default)]
pub struct Bounds(Arc<std::sync::Mutex<Vec<Bounded>>>);

/// One call, and the bound it was given. `None` is a call deliberately left
/// unbounded.
type Bounded = (String, Option<Duration>);

impl Bounds {
    /// The runner to give a `Ctx`: this recorder in front of the fake it
    /// already holds.
    pub fn watching(&self, inner: Arc<dyn CommandRunner>) -> Arc<dyn CommandRunner> {
        Arc::new(riabuild_runner::Delegating::around(inner, self.clone()))
    }

    /// The bound the first call whose invocation contains `fragment` was given.
    ///
    /// `None` is a call deliberately left unbounded, which is a different
    /// answer from a call that never happened — that one panics, because a test
    /// asserting the patience of a command nothing ran would otherwise pass by
    /// reading nothing at all.
    pub fn of(&self, fragment: &str) -> Option<Duration> {
        self.0
            .lock()
            .expect("bounds")
            .iter()
            .find(|(call, _)| call.contains(fragment))
            .map(|(_, bound)| *bound)
            .expect("nothing ran that matched the fragment")
    }
}

#[async_trait::async_trait]
impl riabuild_runner::Decoration for Bounds {
    async fn before(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> anyhow::Result<()> {
        self.0
            .lock()
            .expect("bounds")
            .push((format!("{program} {}", args.join(" ")), options.timeout));
        Ok(())
    }
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
