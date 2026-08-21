//! Where the command line meets the library.
//!
//! Every function here is one `match` from a clap enum onto calls that know
//! nothing about clap. That is the whole job, and keeping it in one place is
//! what lets `channel/`, `accounts/` and `remote/` be libraries rather than
//! command handlers: a module that matches on `ChannelAction` has to be
//! compiled with the parser, and a module compiled with the parser can read
//! any flag it likes rather than the ones its caller chose to pass.
//!
//! Each of these started life inside the module it dispatches into, which is
//! the direction this drifts if nobody is watching — a handler grows one
//! `RealPaths::new()` at a time until the library it lives in cannot be built
//! without the binary.

use crate::cli::{ChannelAction, ClaudeAction, Cli, RemoteAction};
use anyhow::Result;
use riabuild_channel as channel;
use riabuild_paths::{Paths, RealPaths};
use riabuild_remote as remote;
use riabuild_runner::{CommandRunner, RealRunner};
use riabuild_tasks::Ctx;
use riabuild_tasks::{accounts, shims};
use riabuild_ui::Ui;
use std::sync::Arc;

/// Whether this shell was opened by `riabuild remote`.
///
/// The presence of the variable, never whether the socket behind it answers —
/// those are the two different questions `channel status` exists to tell apart.
/// A remote session with a dead channel has something to reconnect; a local
/// shell has nothing to reconnect to, and telling its developer to "run
/// `riabuild remote` again" would send them somewhere there is no problem.
fn in_a_remote_session() -> bool {
    remote_session_from(std::env::var(channel::SOCKET_ENV).ok().as_deref())
}

/// The answer with the environment supplied rather than read, so a test can
/// drive both without mutating a variable the whole suite shares — the same
/// wrapper-and-parameter split `socket_path_from` and `browser_for` use.
///
/// An empty value is not a session. It is the same rule `browser_for` applies
/// to the same variable, and the two must agree: a session that exported an
/// empty socket would otherwise be told to reconnect a channel that was never
/// configured.
fn remote_session_from(socket: Option<&str>) -> bool {
    socket.is_some_and(|socket| !socket.is_empty())
}

/// `riabuild channel …`
///
/// Handled before the setup flow: the shim runs on every Ctrl+V and must not
/// check the machine or talk to the API.
pub async fn channel(action: &ChannelAction, quiet: bool) -> Result<i32> {
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);

    match action {
        ChannelAction::Shim { tool, args } => {
            let Some(tool) = shims::clipboard::Tool::from_name(tool) else {
                // Not a tool riabuild shadows. The shell's own code for it,
                // rather than a silent success.
                return Ok(127);
            };
            let bin = RealPaths::new()?.bin_dir();
            Ok(
                shims::clipboard::run(tool, args, Some(channel::socket_path()), &bin, &runner)
                    .await,
            )
        }

        ChannelAction::Agent { socket } => {
            // The creating side, so the checked resolver: this is the one call
            // that can still refuse a socket belonging to somebody else, before
            // `serve` unlinks whatever is in the way and binds.
            let socket = channel::socket_path_for_create(socket.as_deref()).await?;
            let bin = RealPaths::new()?.bin_dir();
            channel::laptop_agent(runner, &bin)?.serve(&socket).await?;
            Ok(0)
        }

        ChannelAction::Pump { socket } => {
            // The creating side, so the checked resolver — the same one `Agent`
            // uses, and for the same reason: on a server several developers
            // share, this is what refuses a path that is a symlink or belongs
            // to another account instead of binding over it.
            let socket = channel::socket_path_for_create(socket.as_deref()).await?;
            channel::pump::run(&socket).await?;
            Ok(0)
        }

        ChannelAction::Open { args } => {
            Ok(shims::browser::run(args, Some(channel::socket_path())).await)
        }

        ChannelAction::Status => {
            let ui = Ui::new(quiet);
            let socket = channel::socket_path();
            // On the ping's own deadline rather than a transfer's: this command
            // answers one question and must answer it quickly. See
            // `client::PING_TIMEOUT`.
            match channel::client::request_within(
                &socket,
                &channel::protocol::Request::ChannelPing,
                channel::client::PING_TIMEOUT,
            )
            .await
            {
                Ok(_) => {
                    ui.info(&format!(
                        "Clipboard channel — connected ({})",
                        socket.display()
                    ));
                    Ok(0)
                }
                // A shell with no `RIABUILD_CHANNEL_SOCKET` was not opened by
                // `riabuild remote` at all, and `socket_path` answered with the
                // runtime-directory fallback rather than with a session's path.
                // Told apart from a channel that is down because the two have
                // nothing in common: there is no laptop to reconnect to here,
                // and the clipboard is already the developer's own.
                Err(_) if !in_a_remote_session() => {
                    ui.warn("Clipboard channel — not part of a remote session");
                    ui.note(
                        "This shell was not opened by `riabuild remote`, so there is no laptop \
                         on the other end of a channel. On your own machine the clipboard and \
                         the browser are already yours, and riabuild shadows neither.",
                    );
                    Ok(1)
                }
                Err(error) => {
                    // The diagnosis and the remedy rendered apart, the way
                    // `supervisor::report` does it: `warn` takes the line that
                    // says what is wrong, `note` folds the prose. `Ui::info`
                    // would not fold — it is a bare `println!` — and a paragraph
                    // through it wraps at column 0 wherever the terminal
                    // happens to end.
                    let failure = error.downcast_ref::<riabuild_ui::Failure>();
                    match failure {
                        Some(failure) => {
                            ui.warn(&format!("Clipboard channel — down: {}", failure.attempting));
                            for line in failure
                                .detail
                                .lines()
                                .filter(|line| !line.trim().is_empty())
                            {
                                ui.note(line);
                            }
                            ui.note(&failure.action);
                        }
                        None => ui.warn(&format!("Clipboard channel — down: {error}")),
                    }
                    // The sentence that would have saved this being reported as
                    // two unrelated bugs. Copying keeps working while everything
                    // else stops, because Claude Code's copy always *also*
                    // returns an OSC 52 escape and the terminal acts on that
                    // with no channel involved — so "copy works, paste does
                    // not" is one dead channel, not a half-broken riabuild.
                    ui.note(
                        "Paste, image paste and `xdg-open` all go through the channel and fail \
                         until it is back. Copying out of Claude Code may still appear to work: \
                         it emits an OSC 52 escape your terminal acts on by itself, which needs \
                         no channel and carries text only.",
                    );
                    Ok(1)
                }
            }
        }
    }
}

/// `riabuild claude …`, which defaults to `list`.
pub async fn claude(ctx: &mut Ctx, action: Option<ClaudeAction>) -> Result<i32> {
    match action.unwrap_or(ClaudeAction::List) {
        ClaudeAction::List => accounts::command::list(ctx).await,
        ClaudeAction::New => accounts::command::new(ctx).await,
        ClaudeAction::Delete { number, yes } => accounts::command::delete(ctx, number, yes).await,
        ClaudeAction::Primary { number } => accounts::command::primary(ctx, number).await,
    }
}

/// `riabuild remote …`
///
/// `list` and `forget` are whole commands rather than variations on the
/// default flow — neither connects to a server — so they are named calls here
/// rather than an `Option<RemoteAction>` threaded down into `flow`.
///
/// `forget` is the one that has to read `--check`. It revokes the server's
/// riabuild session at the API, clears this laptop's key out of the server's
/// `authorized_keys`, and deletes the saved password and cached session from
/// the keychain — three irreversible things under a flag documented as
/// changing nothing. The guard is here rather than in `remote/` because
/// `remote::Request` is what that crate is handed, and `forget` takes a name
/// rather than a request: `dispatch` is where the global flags of this
/// invocation and the library call meet, which is the whole reason this
/// module exists.
pub async fn remote(
    ctx: &mut Ctx,
    action: Option<RemoteAction>,
    request: remote::Request,
) -> Result<i32> {
    match action {
        Some(RemoteAction::List) => remote::list(ctx).await,
        Some(RemoteAction::Forget { name }) => {
            if ctx.dry_run {
                ctx.ui.info(&format!(
                    "--check: this would revoke {name}'s riabuild session, remove riabuild's key \
                     from it, and delete the session and password this machine saved for it. \
                     Nothing was changed."
                ));
                return Ok(0);
            }
            remote::forget_server(ctx, &name).await
        }
        None => remote::run(ctx, request).await,
    }
}

/// The `remote` request this invocation describes.
///
/// `--accept-host-key` is scoped to the `remote` subcommand rather than being a
/// global flag (R13 in `.superpowers/sdd/2026-08-06-remote-mode/decisions.md`),
/// so it arrives already destructured from `Command::Remote`. That is the whole
/// reason this can be a plain function: `remote/` used to reach back into
/// `cli.command` and match on it, because from down there the flag is not a
/// field of `Cli` and there was nowhere else to find it.
pub fn remote_request(
    cli: &Cli,
    target: Option<String>,
    accept_host_key: Option<String>,
) -> remote::Request {
    remote::Request {
        target,
        accept_host_key,
        check: cli.check,
        quiet: cli.quiet,
        no_shell: cli.no_shell,
        project: cli.project.clone(),
        repo: cli.repo.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;
    use clap::Parser;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::testing::{ctx_and_runner, write_file};

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";

    #[tokio::test]
    async fn check_forgets_no_server() {
        // `riabuild --check remote forget build-01` used to revoke the
        // session, strip the key and drop the saved password — everything the
        // real command does, under a flag that promises to do none of it.
        let (mut ctx, _home, runner) = ctx_and_runner(FakeRunner::new()).await;
        ctx.dry_run = true;
        let remotes = ctx.paths.remotes_file();
        write_file(&remotes, "{\"remotes\":[]}").await;
        let before = tokio::fs::read_to_string(&remotes).await.unwrap();

        let code = remote(
            &mut ctx,
            Some(RemoteAction::Forget {
                name: "build-01".into(),
            }),
            remote::Request {
                target: None,
                accept_host_key: None,
                check: true,
                quiet: true,
                no_shell: false,
                project: None,
                repo: None,
            },
        )
        .await
        .expect("a dry run succeeds");

        assert_eq!(code, 0);
        assert_eq!(
            tokio::fs::read_to_string(&remotes).await.unwrap(),
            before,
            "the record of the server must survive --check"
        );
        assert_eq!(
            ctx.keychain.get().await.unwrap().as_deref(),
            Some("rb_test_token"),
            "and so must this machine's own session"
        );
        assert!(
            runner.calls().is_empty(),
            "nothing may be run against the server either"
        );
    }

    /// `channel status` has to tell "there is no channel here" apart from "the
    /// channel is down", because only one of them has anything to reconnect.
    /// Both fail to connect, so the socket alone cannot separate them.
    #[test]
    fn only_a_shell_with_a_socket_is_a_remote_session() {
        assert!(remote_session_from(Some(
            "/home/dev/.riabuild-remote/abc/channel.sock"
        )));
        // A laptop: nothing exported it, and `socket_path` answered with the
        // runtime-directory fallback rather than with any session's path.
        assert!(!remote_session_from(None));
        // The same rule `browser_for` applies to this variable. Disagreeing
        // would tell a session that exported nothing to go and reconnect it.
        assert!(!remote_session_from(Some("")));
    }

    /// Builds the request the way `main` does, from a parsed command line.
    fn request_from(argv: &[&str]) -> remote::Request {
        let cli = Cli::parse_from(argv);
        let Some(Command::Remote {
            target,
            accept_host_key,
            ..
        }) = cli.command.clone()
        else {
            panic!("{argv:?} is not a `remote` invocation");
        };
        remote_request(&cli, target, accept_host_key)
    }

    #[test]
    fn accept_host_key_reaches_the_request_from_the_remote_subcommand() {
        // R13: the flag is scoped to `remote`, not global. This used to be
        // asserted against a helper inside `remote/flow.rs` that matched on
        // `cli.command` to find it — the coupling this function exists to end.
        let request = request_from(&[
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert_eq!(request.accept_host_key.as_deref(), Some(GOOD_FINGERPRINT));
        assert_eq!(request.target.as_deref(), Some("build-01"));

        let without = request_from(&["riabuild", "remote", "build-01"]);
        assert_eq!(without.accept_host_key, None);
    }

    #[test]
    fn the_global_flags_a_remote_run_honours_reach_the_request() {
        // `--check`, `--quiet`, `--no-shell`, `--project` and `--repo` are
        // global, and `flow/connect.rs` reads all five. Handing them over by
        // name is what stops it reading any other flag it likes.
        let request = request_from(&[
            "riabuild",
            "--check",
            "--quiet",
            "--no-shell",
            "--project",
            "/srv/checkout",
            "--repo",
            "Clubria/payments",
            "remote",
            "build-01",
        ]);
        assert_eq!(request.repo.as_deref(), Some("Clubria/payments"));
        assert!(request.check);
        assert!(request.quiet);
        assert!(request.no_shell);
        assert_eq!(request.project.as_deref(), Some("/srv/checkout"));
    }
}
