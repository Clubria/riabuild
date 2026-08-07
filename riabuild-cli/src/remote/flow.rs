//! `riabuild remote` — the flow that ties every piece in this module together.
//!
//! Kept separate from `mod.rs`, which already carries `Remote` and the
//! shell-safety primitives every step here builds commands with — folding the
//! orchestration in as well would have pushed that file well past the
//! crate's ~300-line production budget. The teardown half lives one door
//! further along in `forget.rs`, split out of this file for the same reason
//! and by the same precedent.

use super::{Remote, forget, shell, ssh_once, store};
use super::{authorise, env_command, env_prefix, identity, install, resolve_home, seed, session};
use crate::cli::{Cli, Command, RemoteAction};
use crate::paths::Paths;
use crate::runner::CommandRunner;
use crate::tasks::{Ctx, Status, Task};
use crate::ui::{Failure, Ui};
use anyhow::{Result, anyhow};
use std::sync::Arc;

/// `riabuild remote` — the whole flow.
pub async fn run(
    ctx: &mut Ctx,
    cli: &Cli,
    target: Option<String>,
    action: Option<RemoteAction>,
) -> Result<i32> {
    let mut store = store::Store::load(ctx.paths.as_ref()).await;

    match action {
        Some(RemoteAction::List) => return store::list(ctx, &store),
        Some(RemoteAction::Forget { name }) => {
            // Needs `ctx.member`/`ctx.api`'s bearer token, both of which
            // `connect` populates: the API revoke below authenticates as
            // this laptop's own session, and the server-side cleanup needs
            // to know whose namespace it is clearing. The default flow below
            // also calls `connect`, but only after this match already
            // returned for `list`/`forget`.
            crate::connect(ctx).await?;
            let member = ctx
                .member
                .clone()
                .ok_or_else(|| anyhow!("riabuild does not know who you are yet"))?;
            forget::forget_remote(
                ctx.paths.as_ref(),
                ctx.runner.clone(),
                &ctx.ui,
                &ctx.api,
                &member.member_id,
                &mut store,
                &name,
            )
            .await?;
            return Ok(0);
        }
        None => {}
    }

    // `main::connect` is what populates `ctx.member` and `ctx.org`, and it only
    // runs from `provision` — which this command never reaches. Without it,
    // `ctx.org()?` below fails on every single `riabuild remote` with "riabuild
    // has not loaded the team configuration yet".
    crate::connect(ctx).await?;

    // The laptop then runs exactly two tasks: sign-in, because it mints the
    // server's session, and GitHub, because the server borrows this laptop's
    // sign-in. `github_cli`'s check also re-verifies org membership, so a
    // departed developer fails here rather than on somebody's server.
    ensure_local_prerequisites(ctx).await?;

    let accept_host_key = accept_host_key_of(cli);
    connect_and_setup(ctx, cli, &mut store, target, accept_host_key).await
}

/// The `--accept-host-key` value for this invocation, if any.
///
/// Scoped to `Command::Remote` (R13 in `decisions.md`), not a global `Cli`
/// field, so it is reached by matching `cli.command` rather than reading a
/// top-level field.
fn accept_host_key_of(cli: &Cli) -> Option<&str> {
    match &cli.command {
        Some(Command::Remote {
            accept_host_key, ..
        }) => accept_host_key.as_deref(),
        _ => None,
    }
}

/// Everything from "which server" onward: reachable once `ctx.member` and
/// `ctx.org` already hold their answers, which is what makes it testable
/// against a `FakeRunner` without a real riabuild-web to `connect` against —
/// `run` above is the only caller that goes through `connect` first.
async fn connect_and_setup(
    ctx: &mut Ctx,
    cli: &Cli,
    store: &mut store::Store,
    target: Option<String>,
    accept_host_key: Option<&str>,
) -> Result<i32> {
    let remote = store::choose(ctx, store, target).await?;
    let member = ctx
        .member
        .clone()
        .ok_or_else(|| anyhow!("riabuild does not know who you are yet"))?;
    let version = ctx.org()?.latest_cli_version.clone();

    ctx.ui
        .heading(&format!("Connecting to {}", remote.target()));
    identity::ensure_key(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &member.member_id,
    )
    .await?;
    // R12: the flag threaded here, not `None` — a `None` compiles, passes
    // every test that does not check for it, and silently reduces
    // `trust_host` to the interactive prompt, which errors out with no TTY
    // to show one on (an unattended CI or container run, in particular).
    identity::trust_host(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        accept_host_key,
    )
    .await?;
    authorise::authorise(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui).await?;

    // `resolve_home` runs its own command over `ssh_once`, which is refused
    // outright — before any authentication is even attempted — by a host key
    // `trust_host` has not pinned yet or a key `authorise` has not put in
    // `authorized_keys` yet (`StrictHostKeyChecking=yes` fails the whole
    // connection at the host-key step, before publickey auth is offered a
    // chance). It has to come after both, or the very first `riabuild
    // remote <new-server>` — the one case every unit test in this file
    // sidesteps by pre-seeding `record.home` in its fixture — fails
    // immediately with a confusing "asking … where your home directory is"
    // error instead of ever reaching the host-key prompt. Found by actually
    // running this flow against a fresh container in Task 22's e2e test,
    // which is the one place a truly new remote is ever exercised.
    let home = resolve_home(&remote, ctx.paths.as_ref(), ctx.runner.clone(), store).await?;
    let prefix = env_prefix(&home, &member.member_id, &remote.name);
    let prefix_refs: Vec<(&str, &str)> = prefix
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    let binary = install::ensure_riabuild(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &home,
        &version,
    )
    .await?;

    // `--check` stops here. Everywhere else in riabuild that flag means *touch
    // nothing*, and the steps below mint a bearer token onto a remote
    // filesystem and hand that server this developer's GitHub identity.
    if cli.check {
        ctx.ui
            .note("--check: not minting a session or lending a GitHub sign-in.");
        let command = env_command(&prefix_refs, &binary, &["--check", "--no-shell"]);
        let code =
            shell::run_setup(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &command).await?;
        return Ok(code);
    }

    session::ensure(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &ctx.api,
        &member,
        &ctx.web_url,
        &ctx.cli_version,
        store,
    )
    .await?;

    // `gh-sweep` and `seed-github` both exec the *server's own* riabuild —
    // which is what makes `scope::detect` see it as remote at all (it reads
    // `RIABUILD_REMOTE` from that process's own environment) and, through
    // that, what routes `internal seed-github`'s `gh auth login` into this
    // developer's own scoped `GH_CONFIG_DIR` rather than a shared default.
    // `env_command` with no trailing args produces exactly the
    // `env 'K=V'… '/abs/path/riabuild'` prefix these two internal
    // subcommands are appended onto as plain, unquoted words.
    let remote_binary = env_command(&prefix_refs, &binary, &[]);
    sweep_then_seed(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &remote_binary,
    )
    .await?;

    ctx.ui.heading(&format!("Checking {}", remote.name));
    let mut args: Vec<String> = vec!["--no-shell".to_string()];
    if cli.quiet {
        args.push("--quiet".to_string());
    }
    if let Some(project) = &cli.project {
        args.push("--project".to_string());
        args.push(project.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let setup = env_command(&prefix_refs, &binary, &arg_refs);
    // `ssh -t`, never mosh. mosh does not propagate the remote command's exit
    // status, so a failed setup would return 0 and the flow would open a
    // shell on a broken box. mosh is for the shell, which is the only thing
    // that benefits from surviving sleep and roaming.
    let code = shell::run_setup(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &setup).await?;
    if code != 0 {
        return Ok(code);
    }

    store::remember(ctx, store, &remote, &version).await?;
    if cli.no_shell {
        return Ok(0);
    }
    let shell_invocation = env_command(&prefix_refs, &binary, &["shell"]);
    shell::open(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &shell_invocation,
    )
    .await
}

/// Clears a dead session's leftovers, then lends this laptop's GitHub sign-in
/// to the server for a fresh one — in that order, as two separate SSH
/// processes.
///
/// The order is load-bearing, not cosmetic (Task 20's own finding): each SSH
/// invocation is a separate process, so sweeping *after* seeding would let
/// the seeding run's own exit see itself as the only live session and wipe
/// the credential it had just written, milliseconds before the setup run —
/// a third, later SSH hop — ever saw it. Sweeping first only ever clears a
/// session that already ended; it can never race against a write this call
/// is about to make.
///
/// `remote_binary` is the env-prefixed invocation `env_command(…, &[])`
/// produces (e.g. `env 'RIABUILD_ROOT=…' 'RIABUILD_REMOTE=…' '/abs/riabuild'`),
/// not a bare path: both subcommands below exec the server's own riabuild,
/// which is what makes `scope::detect` see that process as remote at all, and
/// through it what scopes `seed-github`'s `gh auth login` to this developer's
/// own `GH_CONFIG_DIR` rather than a shared default.
async fn sweep_then_seed(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    remote_binary: &str,
) -> Result<()> {
    ssh_once(
        remote,
        paths,
        runner.clone(),
        &format!("{remote_binary} internal gh-sweep"),
    )
    .await?;
    seed::seed_github(remote, paths, runner, ui, remote_binary).await
}

/// The two tasks a laptop runs before it touches a server.
async fn ensure_local_prerequisites(ctx: &mut Ctx) -> Result<()> {
    for task in [
        Box::new(crate::tasks::login::Login) as Box<dyn Task>,
        Box::new(crate::tasks::github_cli::GithubCli),
    ] {
        if let Status::Needs(reason) = task.check(ctx).await? {
            ctx.ui.working(task.title(), &reason.describe());
            task.apply(ctx).await?;
            // The invariant: apply is always followed by a re-run of check.
            if let Status::Needs(reason) = task.check(ctx).await? {
                return Err(Failure::new(
                    format!("getting {} ready on this laptop", task.title()),
                    "Run `riabuild` on this machine and see what it says.",
                )
                .detail(reason.describe())
                .into());
            }
            ctx.ui.applied(task.title());
        } else {
            ctx.ui.satisfied(task.title());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Member;
    use crate::runner::FakeRunner;
    use clap::Parser;

    fn remote() -> super::super::Remote {
        super::super::Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    fn member() -> Member {
        Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@clubria.dev".into(),
            role: "member".into(),
            status: "active".into(),
        }
    }

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";
    const GOOD_FINGERPRINT_LINE: &str =
        "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";

    /// Everything short of a network-backed `riabuild-web`, prepared the way
    /// `connect` would leave it: `ctx.member`/`ctx.org` already populated, an
    /// existing home cached (so `resolve_home` needs no round trip either), and
    /// riabuild already believed installed, so the run reaches `trust_host` in
    /// a handful of scripted SSH calls.
    async fn ready_ctx(fake: FakeRunner) -> (Ctx, tempfile::TempDir, store::Store) {
        let (mut ctx, home) = crate::testing::ctx_with(fake).await;
        ctx.member = Some(member());
        let mut store = store::Store::default();
        let mut record = store::record_for(&remote());
        record.home = "/home/dev".to_string();
        store.remotes.push(record);
        (ctx, home, store)
    }

    /// `ready_ctx`'s opposite: the state a genuinely *new* server is in, with
    /// no home cached, so `resolve_home` really does make its round trip
    /// instead of returning from `remotes.json`. Hands back the runner too,
    /// because what this fixture exists to expose is only visible in the
    /// order of what was run.
    async fn fresh_ctx(
        fake: FakeRunner,
    ) -> (Ctx, tempfile::TempDir, store::Store, Arc<FakeRunner>) {
        let (mut ctx, home, fake) = crate::testing::ctx_and_runner(fake).await;
        ctx.member = Some(member());
        let mut store = store::Store::default();
        // `record_for` leaves `home` empty — deliberately not filled in here.
        store.remotes.push(store::record_for(&remote()));
        (ctx, home, store, fake)
    }

    #[test]
    fn accept_host_key_is_read_out_of_command_remote_not_a_global_field() {
        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert_eq!(accept_host_key_of(&cli), Some(GOOD_FINGERPRINT));

        let no_flag = Cli::parse_from(["riabuild", "remote", "build-01"]);
        assert_eq!(accept_host_key_of(&no_flag), None);

        let other_command = Cli::parse_from(["riabuild", "status"]);
        assert_eq!(accept_host_key_of(&other_command), None);
    }

    /// R12, proven at the call site rather than by inspection: this is the
    /// scenario that a `None` passed to `trust_host` instead of the real flag
    /// would get *wrong*. A mismatch fails with `trust_host`'s "verifying …"
    /// wording, which only exists on the `Some(expected)` branch — with
    /// `None`, this same setup would instead hit `Ui::confirm` and fail with
    /// its own "asking you to confirm" wording (no TTY in a test process),
    /// never reaching a fingerprint comparison at all.
    #[tokio::test]
    async fn the_accept_host_key_flag_reaches_trust_host() {
        let fake = FakeRunner::new()
            .with(
                &format!(
                    "ssh-keyscan -t ed25519 -p {} -T 5 {}",
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            .with("ssh-keygen -t ed25519", 0, "", "");
        let (mut ctx, _home, mut store) = ready_ctx(fake).await;

        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            "SHA256:0000000000000000000000000000000000000000",
        ]);
        let accept_host_key = accept_host_key_of(&cli);
        assert_eq!(
            accept_host_key,
            Some("SHA256:0000000000000000000000000000000000000000")
        );

        let error = connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("build-01".to_string()),
            accept_host_key,
        )
        .await
        .expect_err("a fingerprint that does not match must fail, not silently prompt");

        let message = error.to_string();
        assert!(
            message.contains("verifying"),
            "a `None` reaching trust_host would fail on the confirm prompt instead, \
             with different wording entirely: {message}"
        );
        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure identity::trust_host raises");
        assert!(
            failure.detail.contains("expected") && failure.detail.contains("offered"),
            "{}",
            failure.detail
        );
    }

    /// The other half of R12's proof: a fingerprint that *does* match pins
    /// without ever reaching `Ui::confirm` (which would error out — no TTY in
    /// a test process). Stops at `trust_host` deliberately: everything past
    /// it (`authorise`, `install::ensure_riabuild`) either needs real
    /// checksums over the network or a real riabuild-web, neither of which
    /// this crate's test scaffolding stands up (see `session.rs`'s own note
    /// on `auth::login`).
    #[tokio::test]
    async fn a_matching_accept_host_key_pins_without_ever_touching_a_prompt() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    &format!(
                        "ssh-keyscan -t ed25519 -p {} -T 5 {}",
                        remote().port,
                        remote().host
                    ),
                    0,
                    &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                    "",
                )
                .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""),
        );

        identity::trust_host(
            &remote(),
            &paths,
            fake,
            &Ui::new(true),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect("a matching fingerprint must not hit a prompt");
    }

    /// The ordering inside `connect_and_setup` that every *other* test in
    /// this file is blind to, because they all pre-seed `record.home` and so
    /// reduce `resolve_home` to a lookup that runs no command at all.
    ///
    /// You cannot ask a server where its home directory is before agreeing to
    /// its host key: `StrictHostKeyChecking=yes` fails the connection at the
    /// host-key step, before publickey auth is even offered. So the scan has
    /// to come first. Asserted on the assembled call sequence rather than
    /// trusted to the comment above `resolve_home`'s call — with the old
    /// ordering restored, the home question lands at position 0 and this
    /// fails, which is the whole point of writing it this way.
    ///
    /// The run is expected to end in an error: with `uname -sm` answering
    /// nothing a Rust target can be derived from, `install::ensure_riabuild`
    /// stops before it would reach the network. That is well past the two
    /// steps being ordered here, and it is what keeps this a unit test.
    #[tokio::test]
    async fn a_new_server_is_asked_for_its_home_only_after_its_host_key_is_agreed() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t ed25519 -p {} -T 5 {}",
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // `authorise`'s "can I already sign in?" probe, and the `uname
            // -sm` the install step opens with.
            .with("ssh", 0, "", "")
            // …and `resolve_home`'s own question, which is the one call this
            // test is locating in the sequence.
            .containing("printf %s", 0, "/home/dev", "");
        let (mut ctx, _home, mut store, fake) = fresh_ctx(fake).await;

        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        let _ = connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("build-01".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await;

        let calls = fake.calls();
        let scanned = calls
            .iter()
            .position(|call| call.starts_with("ssh-keyscan"))
            .expect("the host key must be scanned and pinned");
        let asked_home = calls
            .iter()
            .position(|call| call.contains("printf %s"))
            .expect("an empty record.home must produce a real round trip to the server");
        assert!(
            scanned < asked_home,
            "the host key has to be agreed before the first command is run on the server, \
             or a brand-new server fails at `resolve_home` and never reaches the prompt: \
             {calls:?}"
        );
        // The home the server gave back is what got persisted, so a second
        // run does not ask again.
        assert_eq!(
            store.find("build-01").map(|record| record.home.as_str()),
            Some("/home/dev")
        );
    }

    /// Task 20's own finding, pinned at the call this task wires up:
    /// `internal gh-sweep` has to run — as its own SSH process — before
    /// `seed_github`'s. Reversing the order is exactly the bug that made an
    /// already-landed change silently wipe the credential it had just
    /// written (see `seed.rs`'s module doc); this asserts the order in the
    /// assembled call sequence rather than trusting a comment to hold.
    #[tokio::test]
    async fn gh_sweep_runs_before_seeding_not_after() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("gh auth token", 0, "gho_super_secret\n", "")
                .with("ssh", 0, "", ""),
        );

        // The shape `connect_and_setup` actually builds: both subcommands exec
        // the server's own riabuild, so both need `RIABUILD_ROOT`/
        // `RIABUILD_REMOTE` in their own environment for `scope::detect` to
        // see that process as remote at all.
        let remote_binary = env_command(
            &[
                ("RIABUILD_ROOT", "/home/dev/.riabuild-remote/abc"),
                ("RIABUILD_REMOTE", "build-01"),
            ],
            "/home/dev/.riabuild/riabuild/2026.08.06/riabuild",
            &[],
        );

        sweep_then_seed(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &remote_binary,
        )
        .await
        .expect("sweeps then seeds");

        let calls = fake.calls();
        let sweep_index = calls
            .iter()
            .position(|call| call.contains("internal gh-sweep"))
            .expect("gh-sweep ran");
        let seed_index = calls
            .iter()
            .position(|call| call.contains("internal seed-github"))
            .expect("seed-github ran");
        assert!(
            calls[sweep_index].contains("RIABUILD_REMOTE=build-01"),
            "gh-sweep must exec the server's riabuild WITH its environment, or \
             scope::detect never sees that process as remote: {calls:?}"
        );
        assert!(
            calls[seed_index].contains("RIABUILD_REMOTE=build-01"),
            "{calls:?}"
        );
        assert!(
            sweep_index < seed_index,
            "gh-sweep must run before seed-github, not after: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_laptop_with_no_gh_sign_in_still_sweeps_first() {
        // `seed_github` itself is never fatal when there is nothing to lend —
        // the sweep must still have happened before it gives up.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("gh auth token", 1, "", "not logged in")
                .with("ssh", 0, "", ""),
        );

        sweep_then_seed(&remote(), &paths, fake.clone(), &Ui::new(true), "riabuild")
            .await
            .expect("must not fail the run");

        assert!(
            fake.calls().iter().any(|call| call.contains("gh-sweep")),
            "{:?}",
            fake.calls()
        );
    }
}
