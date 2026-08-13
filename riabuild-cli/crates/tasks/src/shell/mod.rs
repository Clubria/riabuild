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

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_theme::{Role, Theme};

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
/// `Ui`, is what prints them — so the palette has to be threaded across that
/// boundary rather than re-derived inside the shell.
///
/// `server` is the name of the box this riabuild is managing, from
/// `Ctx::server` (see `scope.rs`) — `None` on a developer's own laptop. The
/// uncoloured text is `scope::Scope`'s own construction, read straight
/// through rather than re-formatted a second time, so there is exactly one
/// sentence for "the environment is active" and one for "it is active on
/// this named server".
pub fn banner(theme: Theme, server: Option<&str>) -> String {
    let plain = crate::scope::Scope::read(server).banner();
    if !theme.enabled() {
        return plain;
    }
    let bullet = theme.paint(Role::Ok, BULLET);
    match server {
        Some(name) => format!(
            "{bullet} Clubria environment active on {name} {}",
            theme.paint(Role::Muted, REMOTE_HINT)
        ),
        None => format!("{bullet} {HEADLINE} {}", theme.paint(Role::Muted, HINT)),
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

/// riabuild's own directories, in the order they have to lead `PATH`.
pub fn riabuild_path_dirs(ctx: &Ctx) -> Vec<String> {
    let mut dirs = vec![ctx.paths.bin_dir()];
    if let Some(node_version) = &ctx.config.node_version {
        dirs.push(ctx.paths.node_dir(node_version).join("bin"));
    }
    dirs.iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// `PATH` with riabuild's own directories in front, so `node`, `pnpm` and
/// `claude` resolve to the versions riabuild installed.
pub fn path_with_riabuild(ctx: &Ctx, current_path: &str) -> String {
    format!("{}:{current_path}", riabuild_path_dirs(ctx).join(":"))
}

/// The POSIX snippet a generated rcfile runs *after* sourcing the developer's
/// own configuration — shared by bash and zsh, which both accept it verbatim.
///
/// The parent process exports riabuild's environment into the shell, and for a
/// long time that was assumed to settle it. It does not. `.bashrc` and `.zshrc`
/// run afterwards, and prepending to `PATH` there is the single most common
/// line in a developer's dotfiles — Ubuntu ships it for `~/.local/bin`, and
/// nvm, pyenv, mise, asdf and conda each write their own. Any one of them
/// demotes `~/.riabuild/bin` from the front, and everything that depends on it
/// leading silently stops working: the `claude` launcher, the clipboard shims,
/// and the `xdg-open` that carries links to the laptop. The symptom is not an
/// error — it is a developer's own `claude` starting instead of riabuild's.
///
/// This is the same shape the prompt already uses: riabuild goes last so it
/// gets the last word over whatever the developer configured.
///
/// `PATH` is **moved to the front rather than overwritten.** Restating the
/// parent's literal value would throw away everything the developer's rcfile
/// legitimately added, which is the opposite of the "riabuild only adds on
/// top" promise in each generated file's own header. Every other variable is
/// riabuild's outright and is simply re-exported.
///
/// The strip is a `tr`/`grep`/`paste` pipeline rather than a shell loop because
/// this one string has to run under both shells: zsh does not word-split an
/// unquoted `$PATH`, so the obvious `for entry in $PATH` reads as a single
/// element there and silently collapses the whole variable to one directory.
pub fn environment_command(env: &[(String, String)], dirs: &[String]) -> String {
    let strip: String = dirs
        .iter()
        .map(|dir| format!(" -e {}", shell_quote(dir)))
        .collect();
    let mut script = format!(
        r#"# riabuild's own environment, applied on top of the configuration above.
# The developer's rcfile has already run, and prepending to PATH is the most
# common line in one — so riabuild's directories are moved back to the front
# here rather than left wherever that put them. What they added is kept.
_riabuild_rest=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF{strip} | paste -sd: -)
PATH={lead}${{_riabuild_rest:+:$_riabuild_rest}}
export PATH
unset _riabuild_rest"#,
        lead = shell_quote(&dirs.join(":")),
    );
    for (name, value) in env {
        // Rebuilt above from the live value; restating the parent's would drop
        // whatever the developer's own rcfile added to it.
        if name == "PATH" {
            continue;
        }
        script.push_str(&format!("\nexport {name}={}", shell_quote(value)));
    }
    script
}

pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn environment(ctx: &Ctx) -> Vec<(String, String)> {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let mut env = vec![
        ("PATH".to_string(), path_with_riabuild(ctx, &current_path)),
        ("RIABUILD_SHELL".to_string(), "1".to_string()),
    ];
    env.extend(ctx.env.iter().cloned());
    // The inherited value is the one that matters in practice. On a server the
    // shell is started as `env 'RIABUILD_CHANNEL_SOCKET=…' '/abs/riabuild' shell`,
    // so the socket arrives in this process's *own* environment and never
    // through `ctx.env`, which nothing in production writes it to. Reading only
    // `ctx.env` left `BROWSER` unset on every real session while both the
    // clipboard shims and every test went on working — the channel was up, the
    // socket was right, and links still opened in a terminal browser on the
    // server.
    let inherited = std::env::var(riabuild_channel::SOCKET_ENV).ok();
    if let Some(browser) = browser_for(ctx, &env, inherited.as_deref()) {
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
///
/// `inherited` is this process's own `RIABUILD_CHANNEL_SOCKET`, taken as a
/// parameter rather than read here so a test can drive both sources without
/// mutating the environment of a suite that runs its tests in one process.
fn browser_for(ctx: &Ctx, env: &[(String, String)], inherited: Option<&str>) -> Option<String> {
    let configured = env
        .iter()
        .any(|(name, value)| name == riabuild_channel::SOCKET_ENV && !value.is_empty())
        || inherited.is_some_and(|value| !value.is_empty());
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
    use super::*;
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;
    use riabuild_theme::Depth;

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
            riabuild_channel::SOCKET_ENV.to_string(),
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
        assert_eq!(banner(Theme::plain(), None), BANNER);
    }

    #[test]
    fn colour_wraps_the_bullet_and_dims_the_advice() {
        let coloured = banner(Theme::with_depth(Depth::Ansi16), None);
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
        assert!(coloured.contains("\x1b[2m— type `exit`"), "{coloured:?}");
        assert!(coloured.ends_with("\x1b[0m"), "{coloured:?}");
        // The words survive the escapes.
        assert!(coloured.contains(HEADLINE));
    }

    #[test]
    fn a_capable_terminal_gets_the_brand_green_not_the_ansi_one() {
        // The shell banner is baked into a generated rcfile, so it is the one
        // place the palette could silently stay on the old sixteen colours
        // while everything printed by `Ui` moved to the brand.
        let coloured = banner(Theme::with_depth(Depth::TrueColor), None);
        assert!(
            coloured.starts_with("\x1b[38;2;61;220;132m●\x1b[0m "),
            "{coloured:?}"
        );
    }

    #[test]
    fn a_laptop_banner_is_unchanged_byte_for_byte() {
        // The whole reason `server` is a parameter and not a rewrite: a
        // laptop's banner — the case every existing developer sees — must be
        // exactly what it was before remote mode existed.
        assert_eq!(banner(Theme::plain(), None), BANNER);
    }

    #[test]
    fn a_servers_banner_names_it_in_both_variants() {
        let plain = banner(Theme::plain(), Some("build-01"));
        let coloured = banner(Theme::with_depth(Depth::Ansi16), Some("build-01"));
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
        let plain = banner(Theme::plain(), Some("build-01"));
        let coloured = banner(Theme::with_depth(Depth::Ansi16), Some("build-01"));
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
