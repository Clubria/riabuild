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

/// The remote counterpart to [`HINT`], named in `scope::Scope::banner` too —
/// `a_servers_banner_matches_between_colour_and_plain` guards the two from
/// drifting apart.
const REMOTE_HINT: &str = "— type `exit` to leave, `claude` to start working";

/// The banner with colour, matching what `Ui` does elsewhere: a green bullet
/// for a good state, and the trailing advice dimmed so the headline reads first.
///
/// The escapes are baked into the generated rcfile because that file, not
/// `Ui`, is what prints them — so `colour` has to be threaded across that
/// boundary rather than re-derived inside the shell.
///
/// `server` is the name of the box this riabuild is managing, from
/// `Ctx::server` (see `scope.rs`) — `None` on a developer's own laptop. The
/// uncoloured text is `scope::Scope`'s own construction, read straight
/// through rather than re-formatted a second time, so there is exactly one
/// sentence for "the environment is active" and one for "it is active on
/// this named server".
pub fn banner(colour: bool, server: Option<&str>) -> String {
    let plain = crate::scope::Scope::read(server).banner();
    if !colour {
        return plain;
    }
    match server {
        Some(name) => format!(
            "\x1b[32m{BULLET}\x1b[0m Clubria environment active on {name} \x1b[2m{REMOTE_HINT}\x1b[0m"
        ),
        None => format!("\x1b[32m{BULLET}\x1b[0m {HEADLINE} \x1b[2m{HINT}\x1b[0m"),
    }
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

/// `PATH` with riabuild's own directories in front, so `node`, `pnpm` and `c`
/// resolve to the versions riabuild installed.
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
    if let Some(profile) = &ctx.config.claude_profile {
        env.push((
            "CLAUDE_CONFIG_DIR".to_string(),
            ctx.paths
                .claude_dir()
                .join(profile)
                .to_string_lossy()
                .into_owned(),
        ));
    }
    env.extend(ctx.env.iter().cloned());
    env
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

    let (args, extra_env) = match &shell {
        Shell::Zsh => zsh::prepare(ctx).await?,
        Shell::Bash => bash::prepare(ctx).await?,
        Shell::Fish => fish::prepare(ctx).await?,
        // riabuild generates no startup file for a shell it does not know, so
        // there is nothing inside it to print the banner or touch the prompt.
        // The parent says it instead — and only here, so it is still said once.
        Shell::Other(_) => {
            ctx.ui.info(&banner(ctx.ui.colour(), ctx.server.as_deref()));
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
        assert_eq!(banner(false, None), BANNER);
    }

    #[test]
    fn colour_wraps_the_bullet_and_dims_the_advice() {
        let coloured = banner(true, None);
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
        assert!(coloured.contains("\x1b[2m— type `exit`"), "{coloured:?}");
        assert!(coloured.ends_with("\x1b[0m"), "{coloured:?}");
        // The words survive the escapes.
        assert!(coloured.contains(HEADLINE));
    }

    #[test]
    fn a_laptop_banner_is_unchanged_byte_for_byte() {
        // The whole reason `server` is a parameter and not a rewrite: a
        // laptop's banner — the case every existing developer sees — must be
        // exactly what it was before remote mode existed.
        assert_eq!(banner(false, None), BANNER);
    }

    #[test]
    fn a_servers_banner_names_it_in_both_variants() {
        let plain = banner(false, Some("build-01"));
        let coloured = banner(true, Some("build-01"));
        assert!(plain.contains("build-01"), "{plain}");
        assert!(coloured.contains("build-01"), "{coloured}");
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
    }

    #[test]
    fn a_servers_banner_matches_between_colour_and_plain() {
        // REMOTE_HINT (used by the coloured path) and scope::Scope::banner
        // (used by the plain path) are two spellings of one sentence — this
        // is what stops them drifting apart the way BANNER and HINT are
        // guarded above.
        let plain = banner(false, Some("build-01"));
        let coloured = banner(true, Some("build-01"));
        assert!(plain.contains("`exit` to leave, `claude` to start working"));
        assert!(coloured.contains("`exit` to leave, `claude` to start working"));
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
    async fn the_environment_marks_the_session_and_points_claude_at_the_profile() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_profile = Some("11111111-2222-4333-8444-555555555555".into());
        let env = environment(&ctx);

        let lookup = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(lookup("RIABUILD_SHELL"), "1");
        assert!(lookup("CLAUDE_CONFIG_DIR").ends_with("11111111-2222-4333-8444-555555555555"));
    }
}
