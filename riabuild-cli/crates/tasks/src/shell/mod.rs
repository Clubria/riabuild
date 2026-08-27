//! Spawning the Clubria environment shell.
//!
//! Per-shell handling is explicit work, not an implementation detail.
//! `bash --rcfile` *replaces* the user's `.bashrc` rather than adding to it, and
//! zsh has no `--rcfile` at all. Getting this wrong means every developer
//! silently loses their prompt, aliases and history configuration, which reads
//! as *riabuild broke my shell*.
//!
//! The text the shell opens with is `banner`; what riabuild puts into the
//! shell's environment, and how it keeps the last word over the developer's
//! own config, is `environment`. What is here is which shell is being
//! started and how it is handed over.

mod banner;
pub mod bash;
mod environment;
pub mod fish;
pub mod zsh;

pub use banner::{BANNER, banner};
pub use environment::{
    environment, environment_command, path_with_riabuild, riabuild_path_dirs, shell_quote,
};

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_theme::Theme;

/// Arguments to pass the shell, plus environment entries only it needs.
pub type ShellLaunch = (Vec<String>, Vec<(String, String)>);

/// Marks the prompt so a developer can tell at a glance which shell they are in.
///
/// Without it the only evidence of the environment is a banner that scrolls
/// away after the first few commands.
pub const PROMPT_LABEL: &str = "(riabuild)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    Other(String),
}

impl Shell {
    pub fn detect() -> Shell {
        let path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        Shell::from_path(&path)
    }

    pub fn from_path(path: &str) -> Shell {
        match path.rsplit('/').next().unwrap_or("") {
            "zsh" => Shell::Zsh,
            "bash" => Shell::Bash,
            "fish" => Shell::Fish,
            "" => Shell::Other("/bin/sh".into()),
            _ => Shell::Other(path.to_string()),
        }
    }

    pub fn program(&self) -> String {
        match self {
            Shell::Zsh => "zsh".into(),
            Shell::Bash => "bash".into(),
            Shell::Fish => "fish".into(),
            Shell::Other(path) => path.clone(),
        }
    }
}

/// Everything printed when the environment shell starts: the account box, then
/// the banner.
///
/// One string so that each shell's existing `banner_command` — and the
/// `[[ -t 1 ]]` guard inside it that keeps this out of captured output — covers
/// both without any of them learning what an account is.
///
/// `server` is threaded through rather than re-derived here for the same reason
/// the theme is: this text is baked into a generated rcfile, and the only thing
/// that knows which machine the shell is being started on is the `Ctx` back in
/// `spawn`. Dropping it would compile and read as a working banner — a
/// developer on `build-01` would simply be told they are on their laptop.
///
/// It ends with a newline, and that is the blank line between the banner and
/// the first prompt. Every printer of this text — each shell's
/// `banner_command`, and `Ui::info` for a shell riabuild generates nothing for
/// — adds exactly one newline of its own, so the gap has to come from the
/// string rather than from the four places that print it. Without it the
/// banner and the prompt are one paragraph, and the sentence telling the
/// developer how to leave reads as part of the line they are about to type on.
pub fn prelude(
    accounts: &[crate::accounts::status::Account],
    theme: Theme,
    server: Option<&str>,
) -> String {
    format!(
        "{}\n\n{}\n",
        crate::accounts::render::accounts_box(accounts, theme),
        banner(theme, server)
    )
}

/// True when riabuild is already inside its own shell.
///
/// Nesting would stack PATH entries and leave the developer two `exit`s from
/// their own terminal, so riabuild reports the existing session instead.
pub fn already_inside() -> bool {
    std::env::var("RIABUILD_SHELL").is_ok_and(|value| value == "1")
}

pub async fn spawn(ctx: &mut Ctx) -> Result<i32> {
    let shell = Shell::detect();
    let env = environment(ctx);

    let accounts = crate::accounts::status::read_all(ctx).await;
    let prelude = prelude(&accounts, ctx.ui.theme(), ctx.server.as_deref());

    let (args, extra_env) = match &shell {
        Shell::Zsh => zsh::prepare(ctx, &prelude, &env).await?,
        Shell::Bash => bash::prepare(ctx, &prelude, &env).await?,
        Shell::Fish => fish::prepare(ctx, &prelude, &env).await?,
        // riabuild generates no startup file for a shell it does not know, so
        // there is nothing inside it to print this. The parent says it instead
        // — and only here, so it is still said once.
        Shell::Other(_) => {
            ctx.ui.info(&prelude);
            (Vec::new(), Vec::new())
        }
    };

    // Not subdued. This is the developer's own shell, not riabuild's output —
    // dimming it would dim everything they went on to do in it.
    let mut options = RunOptions {
        cwd: ctx.project_dir(),
        env,
        ..Default::default()
    };
    options.env.extend(extra_env);

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.runner
        .run_interactive(&shell.program(), &arg_refs, &options)
        .await
}

#[cfg(test)]
mod tests {
    use super::environment::{browser_for, environment_with};
    use super::*;
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;

    /// A local session opens browsers on its own. Exporting BROWSER there would
    /// point Claude Code at a shim with nowhere to send the link, turning a
    /// working sign-in into an exit 1.
    #[tokio::test]
    async fn a_session_with_no_channel_gets_no_browser_variable() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let env = environment_with(&ctx, None);
        assert!(!env.iter().any(|(name, _)| name == "BROWSER"), "{env:?}");
    }

    #[tokio::test]
    async fn a_session_with_a_channel_points_browser_at_the_shim() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.env.push((
            riabuild_channel::SOCKET_ENV.to_string(),
            "/run/user/1000/riabuild/channel.sock".to_string(),
        ));

        let env = environment_with(&ctx, None);
        let browser = env
            .iter()
            .find(|(name, _)| name == "BROWSER")
            .map(|(_, value)| value.clone())
            .expect("BROWSER should be set when the channel is");

        assert_eq!(
            browser,
            ctx.paths
                .bin_dir()
                .join(crate::shims::BROWSER_TOOL)
                .to_string_lossy()
        );
    }

    /// The path every real remote session takes, and the one no test covered.
    /// A server's shell is started as `env 'RIABUILD_CHANNEL_SOCKET=…'
    /// '/abs/riabuild' shell`, so the socket is in the process's own
    /// environment and never in `ctx.env`. While `browser_for` read only
    /// `ctx.env`, `BROWSER` went unset on every server — with the tunnel up,
    /// the socket correct, and the clipboard shims working — so a login URL
    /// still rendered in a terminal browser over the session.
    #[tokio::test]
    async fn a_socket_inherited_from_the_remote_prefix_still_points_browser_at_the_shim() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let browser = browser_for(
            &ctx,
            &[],
            Some("/home/dev/.riabuild-remote/m1/channel.sock"),
        )
        .expect("BROWSER should be set when the socket is inherited");
        assert_eq!(
            browser,
            ctx.paths
                .bin_dir()
                .join(crate::shims::BROWSER_TOOL)
                .to_string_lossy()
        );
    }

    /// Neither source present, and an inherited empty string, are both "no
    /// channel" — the same rule `ctx.env` already followed.
    #[tokio::test]
    async fn an_empty_inherited_socket_is_not_a_channel() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(browser_for(&ctx, &[], Some("")).is_none());
        assert!(browser_for(&ctx, &[], None).is_none());
    }

    /// An empty socket variable is not a channel. Treating it as one would set
    /// BROWSER on a session that cannot open anything.
    #[tokio::test]
    async fn an_empty_socket_variable_does_not_count_as_a_channel() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.env
            .push((riabuild_channel::SOCKET_ENV.to_string(), String::new()));

        let env = environment_with(&ctx, None);
        assert!(!env.iter().any(|(name, _)| name == "BROWSER"), "{env:?}");
    }

    #[test]
    fn recognises_the_common_shells() {
        assert_eq!(Shell::from_path("/bin/zsh"), Shell::Zsh);
        assert_eq!(Shell::from_path("/opt/homebrew/bin/fish"), Shell::Fish);
        assert_eq!(Shell::from_path("/usr/bin/bash"), Shell::Bash);
        assert_eq!(
            Shell::from_path("/usr/bin/nu"),
            Shell::Other("/usr/bin/nu".into())
        );
    }

    #[tokio::test]
    async fn riabuild_paths_come_first() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some("22.23.1".into());
        let path = path_with_riabuild(&ctx, "/usr/bin:/bin");

        let bin = ctx.paths.bin_dir().to_string_lossy().into_owned();
        let node = ctx
            .paths
            .node_dir("22.23.1")
            .join("bin")
            .to_string_lossy()
            .into_owned();
        assert!(path.starts_with(&format!("{bin}:{node}:")), "{path}");
        assert!(path.ends_with("/usr/bin:/bin"));
    }

    fn dirs() -> Vec<String> {
        vec![
            "/home/ada/.riabuild/bin".to_string(),
            "/home/ada/.riabuild/node/24.19.0/bin".to_string(),
        ]
    }

    #[test]
    fn the_environment_command_puts_riabuild_directories_back_in_front() {
        // The developer's rcfile has already run by the time this does, and
        // theirs is the one that may have prepended ~/.local/bin.
        let script = environment_command(&[], &dirs());
        assert!(
            script.contains("PATH='/home/ada/.riabuild/bin:/home/ada/.riabuild/node/24.19.0/bin'"),
            "{script}"
        );
        assert!(script.contains("export PATH"), "{script}");
    }

    #[test]
    fn the_environment_command_keeps_what_the_developer_added_to_path() {
        // Restating the parent's literal PATH would discard nvm, cargo and
        // their own ~/bin. The tail is rebuilt from the live value instead.
        let script = environment_command(&[], &dirs());
        assert!(script.contains("\"$PATH\""), "{script}");
    }

    #[test]
    fn the_environment_command_strips_a_stale_copy_rather_than_stacking_one() {
        // A developer whose rcfile re-sources itself, or a nested shell, must
        // not grow a second copy of riabuild's directories.
        let script = environment_command(&[], &dirs());
        for dir in dirs() {
            assert!(script.contains(&format!("-e '{dir}'")), "{script}");
        }
    }

    #[test]
    fn the_environment_command_reexports_riabuilds_own_variables() {
        // BROWSER is the one that bites: `export BROWSER=firefox` in a .bashrc
        // silently defeats the shim that carries links to the laptop.
        let env = vec![
            ("PATH".to_string(), "/parent/path/only".to_string()),
            ("RIABUILD_SHELL".to_string(), "1".to_string()),
            (
                "BROWSER".to_string(),
                "/home/ada/.riabuild/bin/xdg-open".to_string(),
            ),
        ];
        let script = environment_command(&env, &dirs());
        assert!(script.contains("export RIABUILD_SHELL='1'"), "{script}");
        assert!(
            script.contains("export BROWSER='/home/ada/.riabuild/bin/xdg-open'"),
            "{script}"
        );
        assert!(!script.contains("/parent/path/only"), "{script}");
    }

    #[tokio::test]
    async fn the_environment_names_every_harness_config_directory() {
        // The launchers set these too, and still overrule what is here. What
        // this covers is every harness reached by another route — an absolute
        // path, an editor extension, a hook that reads the variable to find the
        // config it edits — which without it silently falls back to the one
        // directory riabuild does not manage.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let id = "11111111-2222-4333-8444-555555555555";
        ctx.config.claude_accounts = vec![id.into()];
        let env = environment_with(&ctx, None);

        assert!(
            env.iter()
                .any(|(key, value)| key == "RIABUILD_SHELL" && value == "1")
        );
        for (key, expected) in [
            ("CLAUDE_CONFIG_DIR", ctx.paths.claude_profile_dir(id)),
            ("CODEX_HOME", ctx.paths.codex_profile_dir(1)),
            ("GROK_HOME", ctx.paths.grok_profile_dir(1)),
        ] {
            let expected = expected.to_string_lossy().into_owned();
            assert!(
                env.iter()
                    .any(|(name, value)| name == key && value == &expected),
                "{key} is not {expected}: {env:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_environment_names_the_primary_claude_account() {
        // The account the unnumbered `claude` launcher runs, so a harness
        // started outside a launcher lands where the developer's own `claude`
        // would have.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let first = "11111111-2222-4333-8444-555555555555";
        let second = "22222222-3333-4444-8555-666666666666";
        ctx.config.claude_accounts = vec![first.into(), second.into()];

        let value = environment_with(&ctx, None)
            .into_iter()
            .find(|(key, _)| key == "CLAUDE_CONFIG_DIR")
            .map(|(_, value)| value)
            .unwrap();
        assert!(value.ends_with(first), "{value}");
    }

    #[tokio::test]
    async fn the_environment_names_no_claude_account_where_there_are_none() {
        // A machine that has never signed in has no `claude` launcher either,
        // so there is nothing here to be consistent with — and naming a
        // directory riabuild never made would point Claude Code at one nothing
        // has layered the org settings over.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts.clear();
        let env = environment_with(&ctx, None);

        assert!(
            !env.iter().any(|(key, _)| key == "CLAUDE_CONFIG_DIR"),
            "{env:?}"
        );
        // The other two are a fixed set of nine that both tasks create on every
        // run, so they are named whatever the sign-in state is.
        assert!(env.iter().any(|(key, _)| key == "CODEX_HOME"), "{env:?}");
        assert!(env.iter().any(|(key, _)| key == "GROK_HOME"), "{env:?}");
    }

    #[tokio::test]
    async fn a_named_harness_home_beats_the_default() {
        // `ctx.env` is how a caller says which profile a shell is for. Derived
        // values go in ahead of it so that stays true.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec!["11111111-2222-4333-8444-555555555555".into()];
        ctx.env
            .push(("CLAUDE_CONFIG_DIR".to_string(), "/elsewhere".to_string()));

        let env = environment_with(&ctx, None);
        let last = env
            .iter()
            .rfind(|(key, _)| key == "CLAUDE_CONFIG_DIR")
            .unwrap();
        assert_eq!(last.1, "/elsewhere", "{env:?}");
    }

    fn one_account() -> Vec<crate::accounts::status::Account> {
        use crate::accounts::status::{Account, Identity};
        vec![Account {
            number: 1,
            id: "id-1".into(),
            identity: Identity::LoggedIn("clubria@proton.me".into()),
        }]
    }

    #[test]
    fn the_prelude_is_the_box_then_the_banner() {
        let text = prelude(&one_account(), Theme::plain(), None);

        let box_line = text.find("Your Claude Code accounts:").unwrap();
        let banner_line = text.find("Clubria environment active").unwrap();
        // The banner says how to leave, so it reads last, closest to the prompt.
        assert!(box_line < banner_line, "{text}");
    }

    #[test]
    fn the_prelude_ends_with_the_blank_line_the_prompt_needs() {
        let text = prelude(&one_account(), Theme::plain(), None);

        // Every printer of this string adds exactly one newline of its own —
        // bash's `printf '%s\n'`, zsh's `print -r --`, fish's `echo`, and
        // `Ui::info` for a shell riabuild generates no rcfile for — so the gap
        // between the banner and the first prompt has to be here rather than in
        // the four places that print it. Without it the sentence telling the
        // developer how to leave reads as part of the line they type on.
        assert!(
            text.ends_with(&format!("{}\n", banner(Theme::plain(), None))),
            "the banner, then exactly one newline: {text:?}"
        );
        assert!(
            !text.ends_with("\n\n"),
            "one newline: the printer supplies the other, and two here is a \
             wasted line in every session: {text:?}"
        );
    }

    #[test]
    fn a_servers_prelude_names_the_server() {
        // The prelude is the only thing the generated rcfiles print, so a
        // `server` that stops at `prelude` is a banner that never reaches a
        // shell: everything still compiles, every other test still passes, and
        // a developer on `build-01` is told they are on their laptop with no
        // way to tell the difference.
        let text = prelude(&one_account(), Theme::plain(), Some("build-01"));
        assert!(text.contains("build-01"), "{text}");
        // And the laptop case is still the laptop case.
        assert!(!prelude(&one_account(), Theme::plain(), None).contains("build-01"));
    }
}
