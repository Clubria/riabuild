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
pub const BANNER: &str =
    "● Clubria environment active — type `exit` to leave, `code .` to open your editor here";

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

pub fn spawn(ctx: &mut Ctx) -> Result<i32> {
    let shell = Shell::detect();
    let env = environment(ctx);

    let (args, extra_env) = match &shell {
        Shell::Zsh => zsh::prepare(ctx)?,
        Shell::Bash => bash::prepare(ctx)?,
        Shell::Fish => fish::prepare(ctx)?,
        Shell::Other(_) => (Vec::new(), Vec::new()),
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
    fn riabuild_paths_come_first() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new());
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

    #[test]
    fn the_environment_marks_the_session_and_points_claude_at_the_profile() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new());
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
