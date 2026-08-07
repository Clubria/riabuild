//! Spawning the Clubria environment shell.
//!
//! Per-shell handling is explicit work, not an implementation detail.
//! `bash --rcfile` *replaces* the user's `.bashrc` rather than adding to it, and
//! zsh has no `--rcfile` at all. Getting this wrong means every developer
//! silently loses their prompt, aliases and history configuration, which reads
//! as *riabuild broke my shell*.

pub mod bash;
pub mod fish;
pub mod zsh;

use crate::runner::RunOptions;
use crate::tasks::Ctx;
use anyhow::Result;

/// Arguments to pass the shell, plus environment entries only it needs.
pub type ShellLaunch = (Vec<String>, Vec<(String, String)>);

/// Printed once when the environment shell starts. Tells the developer they are
/// somewhere different and how to leave — and that launching an editor from
/// inside this shell is what makes the editor inherit the environment.
///
/// "Once" is the load-bearing word. This used to be printed by the parent
/// process *and* by the generated rcfile, so every developer saw it twice. The
/// rcfile is the one that keeps it: it runs after the developer's own config,
/// so the banner is the last thing on screen before the first prompt rather
/// than something their `.zshrc` output scrolls away.
pub const BANNER: &str =
    "● Clubria environment active — type `exit` to leave, `code .` to open your editor here";

const BULLET: &str = "●";
const HEADLINE: &str = "Clubria environment active";
const HINT: &str = "— type `exit` to leave, `code .` to open your editor here";

/// The banner with colour, matching what `Ui` does elsewhere: a green bullet
/// for a good state, and the trailing advice dimmed so the headline reads first.
///
/// The escapes are baked into the generated rcfile because that file, not
/// `Ui`, is what prints them — so `colour` has to be threaded across that
/// boundary rather than re-derived inside the shell.
pub fn banner(colour: bool) -> String {
    if !colour {
        return BANNER.to_string();
    }
    format!("\x1b[32m{BULLET}\x1b[0m {HEADLINE} \x1b[2m{HINT}\x1b[0m")
}

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

/// `PATH` with riabuild's own directories in front, so `node`, `pnpm` and
/// `claude` resolve to the versions riabuild installed.
pub fn path_with_riabuild(ctx: &Ctx, current_path: &str) -> String {
    let mut prefix = vec![ctx.paths.bin_dir()];
    if let Some(node_version) = &ctx.config.node_version {
        prefix.push(ctx.paths.node_dir(node_version).join("bin"));
    }
    let prefix: Vec<String> = prefix
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    format!("{}:{current_path}", prefix.join(":"))
}

pub fn environment(ctx: &Ctx) -> Vec<(String, String)> {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let mut env = vec![
        ("PATH".to_string(), path_with_riabuild(ctx, &current_path)),
        ("RIABUILD_SHELL".to_string(), "1".to_string()),
    ];
    env.extend(ctx.env.iter().cloned());
    if let Some(browser) = browser_for(ctx, &env) {
        env.push(("BROWSER".to_string(), browser));
    }
    env
}

/// `$BROWSER`, but only for a session that has a laptop to open links on.
///
/// Claude Code checks `BROWSER` before it checks anything else, and on a
/// headless server that check is the only thing standing between a login URL
/// and a terminal browser rendering over the session. Pointing it at the shim
/// is what makes remote links open on the laptop.
///
/// Conditional because the shim has nowhere to send a link without a channel.
/// A local session opens browsers perfectly well on its own, and exporting this
/// there would turn a working sign-in into an exit 1 — so the variable appears
/// exactly where the channel it depends on does.
fn browser_for(ctx: &Ctx, env: &[(String, String)]) -> Option<String> {
    let configured = env
        .iter()
        .any(|(name, value)| name == crate::channel::SOCKET_ENV && !value.is_empty());
    if !configured {
        return None;
    }
    Some(
        ctx.paths
            .bin_dir()
            .join(crate::shims::BROWSER_TOOL)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Everything printed when the environment shell starts: the account box, then
/// the banner.
///
/// One string so that each shell's existing `banner_command` — and the
/// `[[ -t 1 ]]` guard inside it that keeps this out of captured output — covers
/// both without any of them learning what an account is.
pub fn prelude(accounts: &[crate::accounts::status::Account], colour: bool) -> String {
    format!(
        "{}\n\n{}",
        crate::accounts::render::accounts_box(accounts, colour),
        banner(colour)
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
    let prelude = prelude(&accounts, ctx.ui.colour());

    let (args, extra_env) = match &shell {
        Shell::Zsh => zsh::prepare(ctx, &prelude).await?,
        Shell::Bash => bash::prepare(ctx, &prelude).await?,
        Shell::Fish => fish::prepare(ctx, &prelude).await?,
        // riabuild generates no startup file for a shell it does not know, so
        // there is nothing inside it to print this. The parent says it instead
        // — and only here, so it is still said once.
        Shell::Other(_) => {
            ctx.ui.info(&prelude);
            (Vec::new(), Vec::new())
        }
    };

    let mut options = RunOptions {
        cwd: ctx.project_dir(),
        env,
        stdin: None,
    };
    options.env.extend(extra_env);

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.runner
        .run_interactive(&shell.program(), &arg_refs, &options)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;

    /// A local session opens browsers on its own. Exporting BROWSER there would
    /// point Claude Code at a shim with nowhere to send the link, turning a
    /// working sign-in into an exit 1.
    #[tokio::test]
    async fn a_session_with_no_channel_gets_no_browser_variable() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let env = environment(&ctx);
        assert!(!env.iter().any(|(name, _)| name == "BROWSER"), "{env:?}");
    }

    #[tokio::test]
    async fn a_session_with_a_channel_points_browser_at_the_shim() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.env.push((
            crate::channel::SOCKET_ENV.to_string(),
            "/run/user/1000/riabuild/channel.sock".to_string(),
        ));

        let env = environment(&ctx);
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

    /// An empty socket variable is not a channel. Treating it as one would set
    /// BROWSER on a session that cannot open anything.
    #[tokio::test]
    async fn an_empty_socket_variable_does_not_count_as_a_channel() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.env
            .push((crate::channel::SOCKET_ENV.to_string(), String::new()));

        let env = environment(&ctx);
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

    #[test]
    fn the_coloured_banner_says_the_same_thing_as_the_plain_one() {
        // Two spellings of one sentence drift apart. This is what stops the
        // NO_COLOR path and the coloured path from disagreeing.
        assert_eq!(BANNER, format!("{BULLET} {HEADLINE} {HINT}"));
        assert_eq!(banner(false), BANNER);
    }

    #[test]
    fn colour_wraps_the_bullet_and_dims_the_advice() {
        let coloured = banner(true);
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
        assert!(coloured.contains("\x1b[2m— type `exit`"), "{coloured:?}");
        assert!(coloured.ends_with("\x1b[0m"), "{coloured:?}");
        // The words survive the escapes.
        assert!(coloured.contains(HEADLINE));
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

    #[tokio::test]
    async fn the_environment_marks_the_session_but_pins_no_account() {
        // The launchers each set CLAUDE_CONFIG_DIR themselves. Exporting it too
        // would go stale the moment `riabuild claude primary` reorders the
        // list, and would send any claude started outside a launcher to a
        // Clubria account with no org settings layered.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec!["11111111-2222-4333-8444-555555555555".into()];
        let env = environment(&ctx);

        assert!(
            env.iter()
                .any(|(key, value)| key == "RIABUILD_SHELL" && value == "1")
        );
        assert!(
            !env.iter().any(|(key, _)| key == "CLAUDE_CONFIG_DIR"),
            "{env:?}"
        );
    }

    #[test]
    fn the_prelude_is_the_box_then_the_banner() {
        use crate::accounts::status::{Account, Identity};
        let accounts = vec![Account {
            number: 1,
            id: "id-1".into(),
            identity: Identity::LoggedIn("clubria@proton.me".into()),
        }];
        let text = prelude(&accounts, false);

        let box_line = text.find("Your Claude Code accounts:").unwrap();
        let banner_line = text.find("Clubria environment active").unwrap();
        // The banner says how to leave, so it reads last, closest to the prompt.
        assert!(box_line < banner_line, "{text}");
    }
}
