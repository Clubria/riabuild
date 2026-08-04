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
    build(
        FakeRunner::new(),
        MemoryKeychain::with_token("rb_test_token"),
    )
}

pub fn ctx_with(runner: FakeRunner) -> (Ctx, TempDir) {
    build(runner, MemoryKeychain::with_token("rb_test_token"))
}

fn build(runner: FakeRunner, keychain: MemoryKeychain) -> (Ctx, TempDir) {
    let home = TempDir::new().expect("tempdir");
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
    std::fs::create_dir_all(paths.root()).expect("create ~/.riabuild");

    let runner: Arc<dyn CommandRunner> = Arc::new(runner);
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
    (ctx, home)
}

/// Writes a file and every directory above it.
pub fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write file");
}
