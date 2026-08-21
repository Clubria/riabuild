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
//!
//! Reading that list out of the checkout is `declared`; the three `claude`
//! invocations riabuild makes to compare it against an account and repair the
//! difference are `cli`.

mod cli;
mod declared;

use cli::{ask, install, names};
use declared::declared_in;

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_ui::Failure;

pub struct ClaudePlugins;

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
            //
            // `--` before every value that came out of the checkout, so the
            // option parser stops there and what follows is read as the source
            // or the id it is. `nameable` has already refused a leading `-`;
            // this is the half that does not depend on riabuild having thought
            // of the spelling. Verified against Claude Code 2.1.235: both
            // subcommands take their positional after `--`.
            for source in declared.marketplaces.values() {
                install(ctx, &id, &["plugin", "marketplace", "add", "--", source]).await?;
            }
            for plugin in &declared.plugins {
                install(ctx, &id, &["plugin", "install", "--", plugin]).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::declared::source_argument;
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

    /// A `.claude/settings.json` arrives through a pull request, and until this
    /// was checked its strings went into an argv unread. Against Claude Code
    /// 2.1.235 `claude plugin marketplace add --version` answers `error:
    /// unknown option '--version'` — the source was parsed as an option — so a
    /// declaration is refused before it can become one.
    #[test]
    fn a_source_that_would_be_read_as_an_option_is_skipped() {
        for hostile in ["--version", "-v", "--json"] {
            assert_eq!(
                source_argument(&json!({ "source": hostile })),
                None,
                "{hostile} reached argv"
            );
            assert_eq!(
                source_argument(&json!({
                    "source": { "source": "github", "repo": hostile }
                })),
                None,
                "{hostile} reached argv as a github repo"
            );
            assert_eq!(
                source_argument(&json!({
                    "source": { "source": "directory", "path": hostile }
                })),
                None,
                "{hostile} reached argv as a path"
            );
        }
        assert_eq!(source_argument(&json!({ "source": "" })), None);
    }

    #[test]
    fn an_ordinary_source_still_reaches_argv() {
        assert_eq!(
            source_argument(&json!({
                "source": { "source": "github", "repo": "anthropics/claude-plugins-official" }
            })),
            Some("anthropics/claude-plugins-official".to_string())
        );
        assert_eq!(
            source_argument(&json!({ "source": "./local-marketplace" })),
            Some("./local-marketplace".to_string())
        );
    }

    /// `check()` may only demand what `apply()` is willing to ask for. A plugin
    /// id skipped in one and required in the other is a task that fails forever
    /// on a machine nothing can repair.
    #[tokio::test]
    async fn a_plugin_id_that_would_be_read_as_an_option_is_skipped() {
        let (_ctx, home, _fake) = ready(provisioned(), SETTINGS).await;
        let dir = home.path().join("code/hub");
        write_file(
            &dir.join(".claude").join("settings.json"),
            r#"{"enabledPlugins": {"--yolo": true, "typescript-lsp@claude-plugins-official": true}}"#,
        )
        .await;

        let declared = declared_in(&dir).await;
        assert_eq!(
            declared.plugins,
            vec!["typescript-lsp@claude-plugins-official".to_string()]
        );
    }

    /// The half that does not depend on riabuild having thought of the
    /// spelling. Verified against Claude Code 2.1.235: with `--` in front, the
    /// same `--version` reaches the marketplace resolver instead of the option
    /// parser.
    #[tokio::test]
    async fn every_value_out_of_the_checkout_is_passed_after_a_separator() {
        let (mut ctx, _home, fake) = ready(provisioned(), SETTINGS).await;
        ClaudePlugins.apply(&mut ctx).await.expect("apply");

        let adds: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|call| call.contains("marketplace add") || call.contains("plugin install"))
            .collect();
        assert!(!adds.is_empty(), "nothing was installed");
        for call in adds {
            assert!(
                call.contains(" -- "),
                "a checkout string reached argv unseparated: {call}"
            );
        }
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
                "claude plugin marketplace add -- anthropics/claude-plugins-official",
                "claude plugin install -- typescript-lsp@claude-plugins-official",
                "claude plugin marketplace add -- anthropics/claude-plugins-official",
                "claude plugin install -- typescript-lsp@claude-plugins-official",
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

        // With the `--` in it, because that is the command riabuild ran and
        // the one a developer can paste. Claude Code takes the positional after
        // it — verified against 2.1.235.
        assert_eq!(
            failure.command.as_deref(),
            Some("claude plugin install -- typescript-lsp@claude-plugins-official")
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
