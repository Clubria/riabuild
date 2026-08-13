//! Task 13 — the plugins the checkout declares, installed before the first session.
//!
//! Claude Code already installs these on its own: once a checkout is trusted,
//! the `extraKnownMarketplaces` and `enabledPlugins` in its
//! `.claude/settings.json` are reconciled by a background pass at session
//! start. That is the whole problem. The pass lands *during* the first session,
//! and a plugin installed mid-session is only loaded by the next one — so the
//! developer riabuild exists to serve meets our codebase for the first time
//! without the tooling our codebase asks for, and nothing on screen suggests
//! that launching again would fix it.
//!
//! So riabuild performs the same installation up front, once per account,
//! through the same CLI Claude Code exposes for it. This is not a second
//! mechanism racing the first: the pass finds the work already done and does
//! nothing.
//!
//! **Nothing here decides what to install.** The list is read from the
//! checkout's own settings file, which arrived through a pull request and is
//! the same file Claude Code would read. riabuild-web is not asked and could
//! not be: an org setting naming a marketplace would be the server-driven task
//! manifest `../../../CLAUDE.md` forbids, moved one repository along. The
//! trust that makes those settings load at all is `claude_trust`'s job, and is
//! the only gate Claude Code puts in front of a plugin — there is no separate
//! plugin dialog to accept.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::{CommandOutput, RunOptions};
use riabuild_ui::Failure;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub struct ClaudePlugins;

/// What one checkout asks Claude Code to load.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Declared {
    /// Marketplace name → the source argument `plugin marketplace add` takes.
    /// Ordered so two runs issue the same commands in the same order.
    marketplaces: BTreeMap<String, String>,
    /// The `<plugin>@<marketplace>` ids the settings switch on.
    plugins: Vec<String>,
}

impl Declared {
    fn is_empty(&self) -> bool {
        self.marketplaces.is_empty() && self.plugins.is_empty()
    }
}

/// The argument `claude plugin marketplace add` takes for one declaration.
///
/// `None` for a shape riabuild cannot name on a command line — a source kind
/// added after this was written, or an entry missing the field its kind needs.
/// Skipping is deliberate and is not a silent failure mode: a marketplace this
/// cannot *install* must not become one `check()` demands, or the task fails
/// forever on a machine whose `apply()` could never satisfy it. Claude Code
/// still installs it in the background exactly as it did before.
fn source_argument(entry: &Value) -> Option<String> {
    let source = entry.get("source")?;
    // The CLI accepts the same shorthand a human types, so a bare string needs
    // no interpreting.
    if let Value::String(text) = source {
        return Some(text.clone());
    }
    let named = |key: &str| source.get(key)?.as_str().map(str::to_string);
    match source.get("source")?.as_str()? {
        "github" => named("repo"),
        "url" => named("url"),
        "directory" | "file" => named("path"),
        _ => None,
    }
}

/// What the checkout declares, or nothing at all.
///
/// Every failure here — no file, unreadable, not JSON — reports nothing
/// declared rather than an error, because that is what Claude Code itself does
/// with the same file. A settings file the developer is midway through editing
/// is not a reason to refuse to provision their machine, and treating it as one
/// would make riabuild stricter about a checked-in file than the tool the file
/// is for.
pub(crate) async fn declared_in(dir: &Path) -> Declared {
    let file = dir.join(".claude").join("settings.json");
    let Ok(text) = tokio::fs::read_to_string(&file).await else {
        return Declared::default();
    };
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&text) else {
        return Declared::default();
    };

    let marketplaces = root
        .get("extraKnownMarketplaces")
        .and_then(Value::as_object)
        .map(|declared| {
            declared
                .iter()
                .filter_map(|(name, entry)| Some((name.clone(), source_argument(entry)?)))
                .collect()
        })
        .unwrap_or_default();

    // `false` is how a developer turns one off, and is as meaningful as its
    // absence.
    let plugins = root
        .get("enabledPlugins")
        .and_then(Value::as_object)
        .map(|enabled| {
            enabled
                .iter()
                .filter(|(_, on)| on.as_bool() == Some(true))
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();

    Declared {
        marketplaces,
        plugins,
    }
}

/// Runs one `claude` against one account's config directory.
///
/// No `cwd`, so the answer depends on argv and `CLAUDE_CONFIG_DIR` and nothing
/// else. Claude Code reads `.claude/` out of its working directory, which is
/// the class of bug `../../CLAUDE.md` describes for pnpm and infisical: pointed
/// at the checkout, `plugin list` would answer partly for the *repository*
/// rather than for the account, and a `check()` built on that reports drift the
/// `apply()` after it cannot repair.
async fn ask(ctx: &Ctx, id: &str, args: &[&str]) -> Result<CommandOutput> {
    let dir = ctx.paths.claude_profile_dir(id);
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    ctx.runner.run(&ctx.claude(), args, &options).await
}

/// The `key` of every entry in a `--json` listing, or nothing if the listing is
/// not one riabuild understands.
fn names(json: &str, key: &str) -> Vec<String> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| entry.get(key)?.as_str().map(str::to_string))
        .collect()
}

#[async_trait]
impl Task for ClaudePlugins {
    fn id(&self) -> TaskId {
        "claude_plugins"
    }

    fn title(&self) -> &str {
        "Checkout plugins"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // `claude_accounts` supplies the config directories to install into and
        // the Claude Code that does the installing; `project` supplies the
        // checkout whose settings say what to install. A checkout moved by
        // `project` is re-read at its new path, which is what that edge buys.
        &["claude_accounts", "project"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        let declared = declared_in(&dir).await;
        // The common case for a repository with no plugins, and worth an early
        // return: it costs zero subprocesses on every run thereafter.
        if declared.is_empty() {
            return Ok(Status::Satisfied);
        }
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let number = index + 1;

            if !declared.marketplaces.is_empty() {
                let listed = ask(ctx, id, &["plugin", "marketplace", "list", "--json"]).await?;
                if !listed.ok() {
                    return Ok(Status::needs(format!(
                        "account {number} cannot list its plugin marketplaces"
                    )));
                }
                let present = names(&listed.stdout, "name");
                if let Some(missing) = declared
                    .marketplaces
                    .keys()
                    .find(|name| !present.contains(name))
                {
                    return Ok(Status::needs(format!(
                        "account {number} does not have the {missing} marketplace yet"
                    )));
                }
            }

            if !declared.plugins.is_empty() {
                let listed = ask(ctx, id, &["plugin", "list", "--json"]).await?;
                if !listed.ok() {
                    return Ok(Status::needs(format!(
                        "account {number} cannot list its plugins"
                    )));
                }
                let present = names(&listed.stdout, "id");
                if let Some(missing) = declared.plugins.iter().find(|id| !present.contains(id)) {
                    return Ok(Status::needs(format!(
                        "{missing} is not installed for account {number} yet"
                    )));
                }
            }
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(dir) = ctx.project_dir() else {
            return Err(Failure::new(
                "installing the plugins the checkout asks for",
                "Run `riabuild` again — the checkout has to exist first.",
            )
            .into());
        };
        let declared = declared_in(&dir).await;
        if declared.is_empty() {
            return Ok(());
        }
        if ctx.config.claude_accounts.is_empty() {
            return Err(Failure::new(
                "installing the plugins the checkout asks for",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }

        for id in ctx.config.claude_accounts.clone() {
            // Marketplaces first: a plugin cannot be installed out of one that
            // is not registered yet. Both commands are idempotent — the second
            // run reports "already on disk" and "already installed" and exits
            // zero — which is what makes re-running this whole task safe.
            for source in declared.marketplaces.values() {
                install(ctx, &id, &["plugin", "marketplace", "add", source]).await?;
            }
            for plugin in &declared.plugins {
                install(ctx, &id, &["plugin", "install", plugin]).await?;
            }
        }
        Ok(())
    }
}

/// One installation step, or a failure that names what to run by hand.
///
/// Deliberately without `--yes`. That flag exists for a plugin a marketplace
/// installs by *running a command it declares*, and riabuild accepting that on
/// a developer's behalf, unattended, is the boundary the architecture rules
/// draw — riabuild may install what the checkout names, never run what a
/// marketplace hands it. Such a plugin fails here instead, with the command to
/// review and run.
async fn install(ctx: &Ctx, id: &str, args: &[&str]) -> Result<()> {
    let ran = ask(ctx, id, args).await?;
    if ran.ok() {
        return Ok(());
    }
    let spelled = format!("claude {}", args.join(" "));
    Err(Failure::new(
        "installing the plugins the checkout asks for",
        format!("Run `{spelled}` yourself to see what it says — it is safe to re-run, and so is riabuild."),
    )
    .command(spelled)
    .detail(if ran.stderr.trim().is_empty() {
        format!("that command exited with status {:?}", ran.code)
    } else {
        ran.stderr.trim().to_string()
    })
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::new_id;
    use crate::testing::{ctx_and_runner, write_file};
    use riabuild_runner::FakeRunner;
    use serde_json::json;
    use std::sync::Arc;

    /// What `ai-builders-hub` actually declares, reduced to one of each.
    const SETTINGS: &str = r#"{
      "permissions": { "allow": ["Bash(echo:*)"] },
      "enabledPlugins": { "typescript-lsp@claude-plugins-official": true },
      "extraKnownMarketplaces": {
        "claude-plugins-official": {
          "source": { "source": "github", "repo": "anthropics/claude-plugins-official" }
        }
      }
    }"#;

    const MARKETPLACES: &str = r#"[{"name":"claude-plugins-official"}]"#;
    const PLUGINS: &str = r#"[{"id":"typescript-lsp@claude-plugins-official"}]"#;

    /// Everything installed, for both accounts, and both writes scripted.
    fn provisioned() -> FakeRunner {
        FakeRunner::new()
            .with("claude plugin marketplace list --json", 0, MARKETPLACES, "")
            .with("claude plugin list --json", 0, PLUGINS, "")
            .with("claude plugin marketplace add", 0, "", "")
            .with("claude plugin install", 0, "", "")
    }

    /// A ctx with two accounts and a checkout carrying `settings`.
    async fn ready(
        runner: FakeRunner,
        settings: &str,
    ) -> (Ctx, tempfile::TempDir, Arc<FakeRunner>) {
        let (mut ctx, home, fake) = ctx_and_runner(runner).await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id = new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
                .await
                .expect("account dir");
            ids.push(id);
        }
        let dir = home.path().join("code/hub");
        tokio::fs::create_dir_all(dir.join(".claude"))
            .await
            .expect("checkout");
        if !settings.is_empty() {
            write_file(&dir.join(".claude").join("settings.json"), settings).await;
        }
        ctx.config.claude_accounts = ids;
        ctx.config.project_path = Some(dir.to_string_lossy().into_owned());
        (ctx, home, fake)
    }

    #[tokio::test]
    async fn a_checkout_declaring_nothing_costs_no_subprocesses() {
        // The common case for every repository without plugins. Asking Claude
        // Code twice per account on every run to be told there is nothing to do
        // is half a second each of a developer's life, forever.
        let (ctx, _home, fake) = ready(provisioned(), r#"{"permissions":{}}"#).await;
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[tokio::test]
    async fn a_checkout_with_no_settings_file_at_all_is_satisfied() {
        let (ctx, _home, _fake) = ready(provisioned(), "").await;
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn settings_that_do_not_parse_declare_nothing() {
        // Claude Code ignores this file too. riabuild refusing to provision the
        // machine over a half-typed settings file would be stricter about it
        // than the tool it is for.
        let (ctx, _home, _fake) = ready(provisioned(), "{ not json").await;
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_fully_installed_machine_is_satisfied() {
        let (ctx, _home, _fake) = ready(provisioned(), SETTINGS).await;
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_missing_marketplace_is_detected() {
        let runner = provisioned().with("claude plugin marketplace list --json", 0, "[]", "");
        let (ctx, _home, _fake) = ready(runner, SETTINGS).await;

        let status = ClaudePlugins.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("claude-plugins-official marketplace"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_plugin_is_detected() {
        let runner = provisioned().with("claude plugin list --json", 0, "[]", "");
        let (ctx, _home, _fake) = ready(runner, SETTINGS).await;

        let status = ClaudePlugins.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("typescript-lsp@claude-plugins-official"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn an_uninstallable_plugin_listing_is_drift_rather_than_a_thrown_error() {
        // `--check` on a machine whose Claude Code cannot answer must still
        // report, not take the whole run down.
        let runner = provisioned().with("claude plugin list --json", 1, "", "boom");
        let (ctx, _home, _fake) = ready(runner, SETTINGS).await;

        let status = ClaudePlugins.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("cannot list"), "{status:?}");
    }

    /// The `CLAUDE_CONFIG_DIR` each matching invocation carried, in order.
    ///
    /// Which account a command was aimed at lives only in its environment, so
    /// `calls()` cannot show it: two accounts and one account run the identical
    /// command string.
    fn config_dirs(fake: &FakeRunner, prefix: &str) -> Vec<String> {
        fake.recorded
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.invocation.starts_with(prefix))
            .filter_map(|call| {
                call.env
                    .iter()
                    .find(|(key, _)| key == "CLAUDE_CONFIG_DIR")
                    .map(|(_, dir)| dir.clone())
            })
            .collect()
    }

    fn account_dirs(ctx: &Ctx) -> Vec<String> {
        ctx.config
            .claude_accounts
            .iter()
            .map(|id| {
                ctx.paths
                    .claude_profile_dir(id)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[tokio::test]
    async fn a_satisfied_first_account_does_not_end_the_check() {
        // claude-2 would otherwise open its first session without the
        // repository's tooling — the gap this task closes, one account over.
        let (ctx, _home, fake) = ready(provisioned(), SETTINGS).await;
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);

        assert_eq!(
            config_dirs(&fake, "claude plugin list --json"),
            account_dirs(&ctx)
        );
    }

    #[tokio::test]
    async fn applying_installs_for_every_account_marketplace_first() {
        let (mut ctx, _home, fake) = ready(provisioned(), SETTINGS).await;
        ClaudePlugins.apply(&mut ctx).await.unwrap();

        let calls = fake.calls();
        assert_eq!(
            calls,
            vec![
                "claude plugin marketplace add anthropics/claude-plugins-official",
                "claude plugin install typescript-lsp@claude-plugins-official",
                "claude plugin marketplace add anthropics/claude-plugins-official",
                "claude plugin install typescript-lsp@claude-plugins-official",
            ],
            "two accounts, marketplace before the plugin that comes out of it"
        );
        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn each_account_is_installed_into_its_own_config_directory() {
        // The whole reason a plugin is per-account rather than per-machine. An
        // install that leaked `CLAUDE_CONFIG_DIR` would put every plugin in one
        // account and pass every assertion about *what* ran.
        let (mut ctx, _home, fake) = ready(provisioned(), SETTINGS).await;
        ClaudePlugins.apply(&mut ctx).await.unwrap();

        let wanted = account_dirs(&ctx);
        assert_eq!(config_dirs(&fake, "claude plugin marketplace add"), wanted);
        assert_eq!(config_dirs(&fake, "claude plugin install"), wanted);
    }

    #[tokio::test]
    async fn applying_twice_issues_the_same_commands() {
        // Both CLI commands are idempotent, so re-running is the whole repair
        // strategy — there is no "already done" branch to take.
        let (mut ctx, _home, fake) = ready(provisioned(), SETTINGS).await;
        ClaudePlugins.apply(&mut ctx).await.unwrap();
        let first = fake.calls();
        ClaudePlugins.apply(&mut ctx).await.unwrap();
        let both = fake.calls();

        assert_eq!(both.len(), first.len() * 2);
        assert_eq!(both[first.len()..], first[..]);
    }

    #[tokio::test]
    async fn a_failed_install_names_the_command_to_run_by_hand() {
        let runner = provisioned().with("claude plugin install", 1, "", "needs a confirmation");
        let (mut ctx, _home, _fake) = ready(runner, SETTINGS).await;

        let failure = ClaudePlugins.apply(&mut ctx).await.unwrap_err();
        let failure = failure
            .downcast_ref::<Failure>()
            .expect("a Failure, not a bare error");

        assert_eq!(
            failure.command.as_deref(),
            Some("claude plugin install typescript-lsp@claude-plugins-official")
        );
        // What the CLI said, rather than riabuild's paraphrase of it.
        assert_eq!(failure.detail, "needs a confirmation");
    }

    #[tokio::test]
    async fn a_disabled_plugin_is_left_alone() {
        let settings = json!({ "enabledPlugins": { "noisy@somewhere": false } }).to_string();
        let (ctx, _home, fake) = ready(provisioned(), &settings).await;

        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[tokio::test]
    async fn a_marketplace_riabuild_cannot_name_is_skipped_rather_than_demanded() {
        // The trap the skill warns about: a `check()` that requires something
        // the `apply()` after it could never do turns a working machine into a
        // hard error on every run, forever. A source kind added to Claude Code
        // after this was written must therefore be invisible here — Claude Code
        // still installs it in the background exactly as before.
        let settings = json!({
            "extraKnownMarketplaces": { "future": { "source": { "source": "quantum" } } }
        })
        .to_string();
        let (ctx, _home, fake) = ready(provisioned(), &settings).await;

        assert_eq!(ClaudePlugins.check(&ctx).await.unwrap(), Status::Satisfied);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn every_source_kind_the_cli_accepts_becomes_its_argument() {
        let cases = [
            (
                json!({"source":{"source":"github","repo":"a/b"}}),
                Some("a/b"),
            ),
            // A pinned ref is not expressible as an `add` argument; the
            // repository still is, and is better than nothing registered.
            (
                json!({"source":{"source":"github","repo":"a/b","ref":"v1"}}),
                Some("a/b"),
            ),
            (
                json!({"source":{"source":"url","url":"https://x/y.json"}}),
                Some("https://x/y.json"),
            ),
            (
                json!({"source":{"source":"directory","path":"/opt/m"}}),
                Some("/opt/m"),
            ),
            (json!({"source":"a/b"}), Some("a/b")),
            (json!({"source":{"source":"quantum"}}), None),
            (json!({"source":{"source":"github"}}), None),
            (json!({"nothing":1}), None),
        ];
        for (entry, want) in cases {
            assert_eq!(source_argument(&entry).as_deref(), want, "{entry}");
        }
    }

    #[test]
    fn a_listing_claude_code_did_not_produce_names_nothing() {
        assert!(names("not json", "id").is_empty());
        assert!(names(r#"{"id":"x"}"#, "id").is_empty());
        assert_eq!(names(r#"[{"id":"x"},{"nope":1}]"#, "id"), vec!["x"]);
    }
}
