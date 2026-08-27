//! What riabuild puts into the environment shell, and how it keeps the last
//! word over the developer's own config.
//!
//! `PATH` is the one that has to be *moved to the front* rather than
//! overwritten: the developer's dotfiles run after riabuild set the
//! environment, and prepending to `PATH` is the most common line in one.

use crate::Ctx;
use riabuild_harness::Kind;

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

/// Single-quotes a value for a POSIX shell, so it survives intact as one
/// argument no matter what characters it contains.
///
/// Single quotes admit no escape sequences at all — the only character that
/// needs special handling is the single quote itself, which cannot appear
/// inside a single-quoted string. The standard trick closes the quote, emits
/// an escaped literal quote outside it, then reopens: `it's` becomes
/// `'it'\''s'` — `'it'` + `\'` + `'s'`, concatenated by the shell back into
/// `it's`.
///
/// **The one POSIX copy in the workspace.** `bash`, `zsh` and
/// `shell::environment_command` beside it, `riabuild-remote` (which re-exports
/// this) and `riabuild env` in the binary each had a byte-identical private
/// copy — five definitions of one rule, which is four opportunities for the
/// next person to fix a quoting bug in the wrong one. `fish::shell_quote` is
/// **not** one of them and must not be folded in here: fish reads backslash as
/// an escape inside single quotes and POSIX does not, so the two disagree
/// about what a correct answer looks like.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn environment(ctx: &Ctx) -> Vec<(String, String)> {
    // The inherited value is the one that matters in practice. On a server the
    // shell is started as `env 'RIABUILD_CHANNEL_SOCKET=…' '/abs/riabuild' shell`,
    // so the socket arrives in this process's *own* environment and never
    // through `ctx.env`, which nothing in production writes it to. Reading only
    // `ctx.env` left `BROWSER` unset on every real session while both the
    // clipboard shims and every test went on working — the channel was up, the
    // socket was right, and links still opened in a terminal browser on the
    // server.
    let inherited = std::env::var(riabuild_channel::SOCKET_ENV).ok();
    environment_with(ctx, inherited.as_deref())
}

/// `environment`, with this process's own socket handed in.
///
/// Split for the reason `browser_for` takes the same parameter: a developer's
/// terminal is not a controlled input, and the two tests that assert *no*
/// `BROWSER` failed on any machine that happened to have
/// `RIABUILD_CHANNEL_SOCKET` set — which is every machine inside a riabuild
/// remote session, including the one this is most likely to be edited on. A
/// test that only passes in a clean environment is a test that will be
/// disbelieved.
pub(super) fn environment_with(ctx: &Ctx, inherited: Option<&str>) -> Vec<(String, String)> {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let mut env = vec![
        ("PATH".to_string(), path_with_riabuild(ctx, &current_path)),
        ("RIABUILD_SHELL".to_string(), "1".to_string()),
    ];
    // Ahead of `ctx.env`, so anything a caller set by name keeps the last word
    // over the default derived here — `riabuild remote` starting a shell for a
    // particular profile, and every test that names one.
    env.extend(harness_homes(ctx));
    env.extend(ctx.env.iter().cloned());
    if let Some(browser) = browser_for(ctx, &env, inherited) {
        env.push(("BROWSER".to_string(), browser));
    }
    env
}

/// The config directory each harness reads, pointed at the profile riabuild
/// owns: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME`.
///
/// These were withheld for most of riabuild's life, on the reasoning that the
/// launchers in `bin/` already set them and one exported value would quietly
/// make all nine profiles share a directory. The first half is still true and
/// the second never was: each launcher `export`s its own profile's value over
/// whatever it inherited, so `claude-2`, `codex-3` and `grok-9` are unaffected
/// by anything here. What withholding them actually bought was an *unset*
/// variable everywhere a harness is reached by any route other than the
/// launcher — an absolute path out of `~/.riabuild/<tool>/<version>/`, an
/// editor extension that found the binary itself, a hook or MCP server that
/// reads the variable to find the config it is meant to edit, a script a
/// developer wrote. Unset does not mean *no opinion*; it means each of those
/// silently uses `~/.claude`, `~/.codex` or `~/.grok` — the three directories
/// riabuild does not manage, holding the developer's own sign-ins and none of
/// the org's settings. So the default is now stated rather than left to a
/// fallback nobody chose.
///
/// The primary profile in each case, which is the one the unnumbered launcher
/// runs: `claude` is account 1, `codex` and `grok` are profile 1. A shell holds
/// the value it opened with, so `riabuild claude primary` reordering the list
/// leaves an already-open shell naming the account that *was* primary — the
/// launchers there are rewritten and stay right, and the next shell agrees with
/// them again.
///
/// Claude's is conditional because its accounts are created by riabuild's own
/// sign-in flow and the list can be empty; there is no `claude` launcher on
/// such a machine either, so there is nothing here to be consistent with.
/// Codex's and Grok's are unconditional because their nine profiles are a fixed
/// set both tasks create on every run.
fn harness_homes(ctx: &Ctx) -> Vec<(String, String)> {
    Kind::ALL
        .into_iter()
        .filter_map(|kind| Some((kind.home_env().to_string(), profile_home(ctx, kind)?)))
        .collect()
}

/// The directory `harness_homes` names for one harness, or `None` where this
/// machine has no profile to name.
///
/// A `match` over [`Kind`] rather than three pushes, so a fourth harness cannot
/// be added to `riabuild-harness` and silently miss the environment shell: it
/// stops compiling here until somebody says where its profile lives.
fn profile_home(ctx: &Ctx, kind: Kind) -> Option<String> {
    let dir = match kind {
        Kind::Claude => ctx
            .paths
            .claude_profile_dir(ctx.config.claude_accounts.first()?),
        Kind::Codex => ctx.paths.codex_profile_dir(1),
        Kind::Grok => ctx.paths.grok_profile_dir(1),
    };
    Some(dir.to_string_lossy().into_owned())
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
pub(super) fn browser_for(
    ctx: &Ctx,
    env: &[(String, String)],
    inherited: Option<&str>,
) -> Option<String> {
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
