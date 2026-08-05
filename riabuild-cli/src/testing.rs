//! Test scaffolding: a `Ctx` pointed at a tempdir, with every external process
//! and secret store faked.
//!
//! This is what makes each `check()` a pure unit test instead of something that
//! needs a real machine in a real state.

#![cfg(test)]

use crate::api::ApiClient;
use crate::api::org::OrgConfig;
use crate::config::{State, UserConfig};
use crate::keychain::{Keychain, MemoryKeychain};
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, FakeRunner};
use crate::tasks::Ctx;
use crate::ui::Ui;
use std::sync::Arc;
use tempfile::TempDir;

pub fn org_config() -> OrgConfig {
    OrgConfig {
        repo_slug: "Clubria/ai-builders-hub".into(),
        default_project_path: "~/code/ai-builders-hub".into(),
        min_cli_version: "0.1.0".into(),
        latest_cli_version: "0.1.0".into(),
        secrets_updated_at: 0,
    }
}

pub fn test_ctx() -> (Ctx, TempDir) {
    let (ctx, home, _) = build(
        FakeRunner::new(),
        MemoryKeychain::with_token("rb_test_token"),
    );
    (ctx, home)
}

pub fn ctx_with(runner: FakeRunner) -> (Ctx, TempDir) {
    let (ctx, home, _) = ctx_and_runner(runner);
    (ctx, home)
}

/// Like `ctx_with`, but hands back the runner as well.
///
/// Some things a task must get right are only visible in *what it ran*: that a
/// developer whose machine is already fine is not sent through a browser
/// sign-in, for instance, cannot be seen in the returned `Status` at all.
pub fn ctx_and_runner(runner: FakeRunner) -> (Ctx, TempDir, Arc<FakeRunner>) {
    build(runner, MemoryKeychain::with_token("rb_test_token"))
}

fn build(runner: FakeRunner, keychain: MemoryKeychain) -> (Ctx, TempDir, Arc<FakeRunner>) {
    let home = TempDir::new().expect("tempdir");
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
    std::fs::create_dir_all(paths.root()).expect("create ~/.riabuild");

    let fake = Arc::new(runner);
    let runner: Arc<dyn CommandRunner> = fake.clone();
    let keychain: Arc<dyn Keychain> = Arc::new(keychain);

    let ctx = Ctx {
        paths,
        runner,
        keychain,
        api: ApiClient::new("0.1.0"),
        ui: Ui::new(true),
        config: UserConfig::default(),
        state: State::default(),
        org: Some(org_config()),
        member: None,
        cli_version: "0.1.0".into(),
        web_url: "https://riabuild.clubria.com".into(),
        env: Vec::new(),
        notes: Vec::new(),
        dry_run: false,
    };
    (ctx, home, fake)
}

/// Writes a file and every directory above it.
pub fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write file");
}
