//! Everything `riabuild remote` does once it knows who the developer is:
//! choosing the server, pinning its host key, authorising riabuild's own key,
//! installing the server's riabuild, minting its session, lending it a GitHub
//! sign-in, and handing over a shell.
//!
//! Split out of `flow.rs` — which keeps the entry point, the `list`/`forget`
//! dispatch, and the two tasks a laptop runs first — because this one
//! sequence is most of the command and pushed that file past the crate's
//! ~300-line production budget, the same reason `forget.rs` was split off
//! before it. It is deliberately not carved up any further: the order of the
//! steps below is load-bearing, and every guard among them sits where it does
//! because of the call on either side of it, so a cut through the middle
//! would separate a guard from the thing that makes it correct.

use crate::cli::Cli;
use crate::paths::Paths;
use crate::remote::{
    Remote, askpass, authorise, channel, env_command, env_prefix, host_key, identity, install,
    resolve_home, seed, session, shell, ssh_once, store,
};
use crate::runner::CommandRunner;
use crate::tasks::Ctx;
use crate::ui::Ui;
use anyhow::{Result, anyhow};
use std::sync::Arc;

/// Everything from "which server" onward: reachable once `ctx.member` and
/// `ctx.org` already hold their answers, which is what makes it testable
/// against a `FakeRunner` without a real riabuild-web to `connect` against —
/// `flow::run` is the only caller that goes through `connect` first.
pub(super) async fn connect_and_setup(
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
    // Before the first `ssh`, not lazily at the one that happens to prompt:
    // `SSH_ASKPASS` is handed to every connection below, and a path that
    // names nothing is one `ssh` answers a password prompt with silence.
    // Rewritten each run, so a riabuild that moved is still the one that
    // answers.
    askpass::ensure_helper(ctx.paths.as_ref()).await?;
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
    host_key::trust_host(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        accept_host_key,
    )
    .await?;
    // Everywhere else in riabuild `--check` means *touch nothing*
    // (`tasks::engine` skips every `apply`), and `authorise` is emphatically
    // not a read: on a server riabuild's key has never reached, it prompts
    // for that account's password and runs `ssh-copy-id`, which writes into
    // the server's own `~/.ssh/authorized_keys`. So under `--check` the
    // question is asked and never answered — "riabuild's key is not
    // authorised there yet" is a check *result*, not something `--check`
    // quietly fixes on its way past.
    //
    // Reported here rather than by moving the whole `--check` gate above
    // this call, because that would also give up `resolve_home` and
    // `install::ensure_riabuild` — and on an already-authorised server
    // (where `authorise` is a single `can_sign_in` probe and a no-op) those
    // are exactly what lets `--check` go on to run the real check on the
    // server, which is the case the flag exists for. `ensure_key` and
    // `trust_host` above stay put for the same reason: a key pair and a
    // `known_hosts` line are this laptop's own files, not the server's.
    if cli.check {
        if !authorise::can_sign_in(&remote, ctx.paths.as_ref(), ctx.runner.clone()).await? {
            ctx.ui.note(
                "--check: riabuild's key is not authorised on that server yet, so there is \
                 nothing here to check. Run `riabuild remote` without --check to install it.",
            );
            return Ok(0);
        }
    } else {
        // Persisted *before* `authorise`, not after it succeeds: `ssh-copy-id`
        // can append riabuild's key to the server's `authorized_keys` and the
        // sign-in probe after it still fail, which returns `Err`. With no
        // record on disk, `remote forget build-01` then refuses — "there is no
        // saved server named build-01" — and that key line, this laptop's
        // host-key pin, and its key pair are removable by hand only. A record
        // for a server that never authorised at all is the cheaper mistake.
        store::persist_one(ctx.paths.as_ref(), store, &remote.name).await?;
        authorise::authorise(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui).await?;
    }

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
    // The second checkpoint: `forget`'s server-side cleanup builds its paths
    // out of `record.home` and skips entirely when that is empty, and the very
    // next step (`install::ensure_riabuild`) currently fails on every Linux
    // server — no published musl asset — so without this the common failure
    // leaves a key line on the server that `forget` can no longer reach in to
    // remove. Conditional because this is the one step `--check` runs against
    // the server, and saving here unconditionally is what made a read-only
    // probe show up in `remote list` as a server the developer had set up.
    if !cli.check {
        store::persist_one(ctx.paths.as_ref(), store, &remote.name).await?;
    }
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
    // The clipboard channel comes up with the shell and goes down with it, and
    // nothing about it may cost the developer that shell — `channel::open_shell`
    // owns both halves of that. `--check` and `--no-shell` have already
    // returned above, so neither starts anything.
    channel::open_shell(channel::Plan {
        remote: &remote,
        paths: ctx.paths.as_ref(),
        runner: ctx.runner.clone(),
        ui: &ctx.ui,
        quiet: cli.quiet,
        remote_socket: channel::remote_socket(&session::namespace(&home, &member.member_id)),
        // The probe carries the same environment every other remote invocation
        // does, so it looks for the socket where the forward actually lands
        // rather than where the server would have guessed.
        probe: env_command(&prefix_refs, &binary, &["channel", "status"]),
        shell: env_command(&prefix_refs, &binary, &["shell"]),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Member;
    use crate::remote::flow::accept_host_key_of;
    use crate::remote::forget;
    use crate::runner::FakeRunner;
    use crate::ui::Failure;
    use clap::Parser;

    fn remote() -> Remote {
        Remote {
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

    /// The `.pub` file `identity::ensure_key` would have left behind. The
    /// `FakeRunner`'s `ssh-keygen` stub writes no file, and `authorise`
    /// refuses on a missing public key before it probes the server at all —
    /// so a test asserting `ssh-copy-id` did not run needs this, or it
    /// passes for the wrong reason.
    async fn write_public_key(paths: &dyn Paths) {
        tokio::fs::create_dir_all(paths.identity_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            paths
                .identity_dir()
                .join(remote().hash())
                .with_extension("pub"),
            "ssh-ed25519 AAAA riabuild",
        )
        .await
        .expect("write pub");
    }

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";
    const GOOD_FINGERPRINT_LINE: &str =
        "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";

    /// Everything short of a network-backed `riabuild-web`, prepared the way
    /// `connect` would leave it: `ctx.member`/`ctx.org` already populated, an
    /// existing home cached (so `resolve_home` needs no round trip either), and
    /// riabuild already believed installed, so the run reaches `trust_host` in
    /// a handful of scripted SSH calls.
    async fn ready_ctx(
        fake: FakeRunner,
    ) -> (Ctx, tempfile::TempDir, store::Store, Arc<FakeRunner>) {
        let (mut ctx, home, fake) = crate::testing::ctx_and_runner(fake).await;
        ctx.member = Some(member());
        let mut store = store::Store::default();
        let mut record = store::record_for(&remote());
        record.home = "/home/dev".to_string();
        store.remotes.push(record);
        (ctx, home, store, fake)
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
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            .with("ssh-keygen -t ed25519", 0, "", "");
        let (mut ctx, _home, mut store, _fake) = ready_ctx(fake).await;

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
            .expect("must be the actionable Failure host_key::trust_host raises");
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
                        "ssh-keyscan -t {} -p {} -T 5 {}",
                        host_key::KEY_TYPES,
                        remote().port,
                        remote().host
                    ),
                    0,
                    &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                    "",
                )
                .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, ""),
        );

        host_key::trust_host(
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
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
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

    /// I2: `--check` against a server riabuild has never been authorised on
    /// must not prompt for that account's password and must not write into
    /// its `~/.ssh/authorized_keys`.
    ///
    /// The `--check` gate used to sit *after* `authorise`, so `riabuild
    /// remote --check newbox` ran `ssh-copy-id` — while the note the
    /// developer was shown mentioned only the session and the GitHub
    /// sign-in. Restore that ordering and this fails: every stub the old
    /// path needs is scripted below, including the `.pub` file `ensure_key`
    /// would have produced (the `FakeRunner`'s `ssh-keygen` writes none, and
    /// without it `authorise` fails on a missing key before it ever reaches
    /// `ssh-copy-id`, which would make this test pass for the wrong reason).
    #[tokio::test]
    async fn check_against_a_new_server_never_runs_ssh_copy_id() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // riabuild's key does not work yet, and the account will take a
            // password — precisely the state in which the old ordering ran
            // `ssh-copy-id`.
            .with(
                "ssh -o BatchMode=yes",
                255,
                "",
                "Permission denied (publickey,password).",
            )
            .with(
                "ssh -o PreferredAuthentications=none",
                255,
                "",
                "Permission denied (publickey,password).",
            )
            .with("ssh-copy-id", 0, "", "")
            .with("ssh", 0, "", "")
            .containing("printf %s", 0, "/home/dev", "");
        let (mut ctx, _home, mut store, fake) = fresh_ctx(fake).await;
        write_public_key(ctx.paths.as_ref()).await;

        let cli = Cli::parse_from([
            "riabuild",
            "--check",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert!(cli.check);
        let result = connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("build-01".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await;

        // What was *run* is asserted before what was returned, so restoring
        // the old ordering fails on the `ssh-copy-id` line itself rather than
        // on the error `authorise` happens to end up raising afterwards.
        let calls = fake.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("ssh-copy-id")),
            "--check must never write into the server's authorized_keys: {calls:?}"
        );
        // …and it stopped before asking the server anything at all.
        assert!(
            !calls.iter().any(|call| call.contains("printf %s")),
            "nothing should have been run on the server: {calls:?}"
        );
        assert!(
            !ctx.paths.remotes_file().exists(),
            "--check must not persist a new server to remotes.json"
        );
        assert_eq!(
            result.expect("not being authorised yet is a check result, not an error"),
            0
        );
    }

    /// The other half of "`--check` writes nothing": the server riabuild's
    /// key *already* works on, which is the one `--check` path that runs past
    /// the `can_sign_in` probe and reaches `resolve_home`.
    ///
    /// `check_against_a_new_server_never_runs_ssh_copy_id` asserts an empty
    /// `remotes.json` too, but it gets there for free — that run stops at the
    /// probe, before any command reaches the server. This one does not stop:
    /// it asks the server for `$HOME`, and `resolve_home` used to follow that
    /// answer with a `store.save`, persisting a record — name, host, port,
    /// user — for a machine the developer had only asked riabuild to *look*
    /// at. It then showed up in `riabuild remote list` as a server they had
    /// set up, and `remote forget` was the only way to take it back out.
    #[tokio::test]
    async fn check_never_persists_a_server_it_only_probed() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // riabuild's key already works here, so `--check` runs on past
            // the probe instead of stopping at it.
            .with("ssh -o BatchMode=yes", 0, "", "")
            .with("ssh", 0, "", "")
            .containing("printf %s", 0, "/home/dev", "");
        let (mut ctx, _home, fake) = crate::testing::ctx_and_runner(fake).await;
        ctx.member = Some(member());
        // Empty, unlike every other fixture here: a server named as a spec
        // rather than a saved label is what `store::choose` adds a record
        // for, and an added record is what there is to wrongly persist.
        let mut store = store::Store::default();

        let cli = Cli::parse_from([
            "riabuild",
            "--check",
            "remote",
            "ada@build-01.fly.dev",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        // `uname -sm` answers nothing a Rust target can be derived from, so
        // this ends inside `install::ensure_riabuild` — well past
        // `resolve_home`, which is the step under test.
        let _ = connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("ada@build-01.fly.dev".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await;

        // The round trip really happened — otherwise this passes for the same
        // free reason the probe-stopped test does, and would keep passing if
        // the save came back.
        assert!(
            fake.calls().iter().any(|call| call.contains("printf %s")),
            "this test is only meaningful if the run got as far as asking the \
             server anything: {:?}",
            fake.calls()
        );
        assert!(
            !ctx.paths.remotes_file().exists(),
            "--check must leave no record of a server it only probed"
        );
    }

    /// The over-correction guard: on a server riabuild's key *does* already
    /// work, `authorise` is a no-op anyway, so `--check` must still go on to
    /// install and run the real check rather than stopping at the probe.
    #[tokio::test]
    async fn check_on_an_authorised_server_still_reaches_the_install_step() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            .with("ssh -o BatchMode=yes", 0, "", "")
            .with("ssh", 0, "", "");
        let (mut ctx, _home, mut store, fake) = ready_ctx(fake).await;

        let cli = Cli::parse_from([
            "riabuild",
            "--check",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        // `uname -sm` answers nothing a Rust target can be derived from, so
        // the run stops inside `install::ensure_riabuild` — which is past
        // the probe, and is what this asserts.
        connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("build-01".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect_err("the fake server reports no usable platform");
        // Asserted on what ran, not on the wording of the error: stopping at
        // the probe returns `Ok(0)` rather than an `Err`, so an assertion
        // about the *error* can only ever fire for some other reason.
        // `uname -sm` is `install::ensure_riabuild`'s first question, three
        // steps past the probe.
        let calls = fake.calls();
        assert!(
            calls.iter().any(|call| call.contains("uname -sm")),
            "an authorised server must run on into the install step rather than \
             stopping at the probe: {calls:?}"
        );
    }

    /// What `ssh` prints when the pinned key no longer matches — a rebuilt VM,
    /// or a box recreated after `remote forget`, which leaves the pin on
    /// purpose.
    const HOST_KEY_CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
         Host key verification failed.";

    /// The composition bug: `--check` routes past `authorise` — where the
    /// stale-pin diagnosis used to live — straight to the `can_sign_in` probe.
    ///
    /// With the pin already in `known_hosts` and no `--accept-host-key`,
    /// `trust_host` returns without looking at it, so `ssh` is the first thing
    /// to notice the key changed. A probe that reports only its exit status
    /// then answers "no", and `--check` prints "riabuild's key is not
    /// authorised on that server yet" and **exits 0** — the exact
    /// misdiagnosis, and a success code, for a box that may not be the
    /// developer's at all.
    #[tokio::test]
    async fn check_reports_a_changed_host_key_as_one_rather_than_as_an_unauthorised_key() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with("ssh -o BatchMode=yes", 255, "", HOST_KEY_CHANGED);
        let (mut ctx, _home, mut store, fake) = ready_ctx(fake).await;
        // Pinned by an earlier run, exactly as `trust_host` would have left
        // it. No `ssh-keyscan` stub: re-scanning is not this path's behaviour.
        tokio::fs::create_dir_all(ctx.paths.ssh_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            ctx.paths.known_hosts_file(),
            format!(
                "{} ssh-ed25519 OLDSTALEKEYDATA\n",
                host_key::entry_host(&remote())
            ),
        )
        .await
        .expect("write");

        let cli = Cli::parse_from(["riabuild", "--check", "remote", "build-01"]);
        assert!(cli.check);
        let error = connect_and_setup(&mut ctx, &cli, &mut store, Some("build-01".into()), None)
            .await
            .expect_err("a server answering with a different host key is not a clean check");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable host-key Failure");
        assert!(
            failure.attempting.contains("host key"),
            "the developer has to be told which key is the problem: {}",
            failure.attempting
        );
        assert!(
            failure
                .action
                .contains(&ctx.paths.known_hosts_file().display().to_string()),
            "and where the pin that caused it lives: {}",
            failure.action
        );
        assert!(
            !fake.calls().iter().any(|call| call.contains("uname")),
            "nothing may be run on a server whose identity did not check out: {:?}",
            fake.calls()
        );
    }

    /// The other half of the `--check` persistence fix: a *real* run that gets
    /// past `authorise` — which may already have written riabuild's key into
    /// the server's `authorized_keys` — and then dies at the install step must
    /// leave a record behind, or `remote forget` has nothing to act on.
    ///
    /// This is today's default outcome rather than a corner case:
    /// `install::ensure_riabuild` fails on every Linux server, because no
    /// `x86_64-unknown-linux-musl` asset is published yet.
    #[tokio::test]
    async fn a_run_that_touched_the_server_and_then_failed_can_still_be_forgotten() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // riabuild's key works, so `authorise` is a no-op — the run gets
            // as far as `install::ensure_riabuild`, where `uname -sm` answers
            // nothing a Rust target can be derived from.
            .with("ssh -o BatchMode=yes", 0, "", "")
            .with("ssh", 0, "", "")
            .containing("printf %s", 0, "/home/dev", "")
            // Whichever keychain CLI this platform uses, for the `forget` below.
            .with("security", 0, "", "")
            .with("secret-tool", 0, "", "");
        let (mut ctx, _home, fake) = crate::testing::ctx_and_runner(fake).await;
        ctx.member = Some(member());
        // Empty: a server named as a spec is one riabuild has never saved,
        // which is the state in which nothing was left behind to forget.
        let mut store = store::Store::default();

        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "ada@build-01.fly.dev",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert!(!cli.check);
        connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("ada@build-01.fly.dev".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect_err("the fake server reports no usable platform");

        // Read back from disk, not from the in-memory store: `forget` runs in
        // a later process and sees only what `remotes.json` holds.
        let mut saved = store::Store::load(ctx.paths.as_ref()).await;
        let record = saved
            .find("build-01")
            .expect("a run that authorised itself on the server must be forgettable");
        assert_eq!(
            record.home, "/home/dev",
            "forget's server-side cleanup builds its paths from `home` and skips \
             entirely when it is empty, so the resolved home has to be there too"
        );

        forget::forget_remote(
            ctx.paths.as_ref(),
            ctx.runner.clone(),
            &ctx.ui,
            &ctx.api,
            &member().member_id,
            &mut saved,
            "build-01",
        )
        .await
        .expect("`remote forget build-01` must find it");
        assert!(saved.find("build-01").is_none());
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("authorized_keys")),
            "the key line riabuild added to the server has to be reachable by \
             forget's cleanup: {:?}",
            fake.calls()
        );
    }

    /// Why the first save sits *before* `authorise` rather than after the run
    /// is safely past it.
    ///
    /// `ssh-copy-id` appends riabuild's key to the server's
    /// `authorized_keys`, and everything after that point can still fail:
    /// `resolve_home` (which is where this run stops, with no `printf %s`
    /// stubbed), the install step, the session write. Saving later would
    /// leave that key line on a server with no local record naming it, and
    /// `remote forget build-01` answering "there is no saved server named
    /// build-01".
    ///
    /// The failure used to be `authorise`'s own — a key copied but unable to
    /// sign in was fatal. It is a warning now, so the run continues and dies
    /// further along instead. That is a *different* failure at a *later*
    /// step, and the reason for the early save is unchanged by it: what
    /// matters is that `ssh-copy-id` ran and the run then ended badly, which
    /// both assertions below still pin.
    #[tokio::test]
    async fn a_key_copied_onto_a_server_that_then_failed_is_still_recorded() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // The same refusal before and after the copy: `ssh-copy-id`
            // succeeds, the recheck does not, so `authorise` fails *after*
            // having written to the server.
            .with(
                "ssh -o BatchMode=yes",
                255,
                "",
                "Permission denied (publickey,password).",
            )
            .with(
                "ssh -o PreferredAuthentications=none",
                255,
                "",
                "Permission denied (publickey,password).",
            )
            .with("ssh-copy-id", 0, "", "");
        let (mut ctx, _home, fake) = crate::testing::ctx_and_runner(fake).await;
        ctx.member = Some(member());
        write_public_key(ctx.paths.as_ref()).await;
        let mut store = store::Store::default();

        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "ada@build-01.fly.dev",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        connect_and_setup(
            &mut ctx,
            &cli,
            &mut store,
            Some("ada@build-01.fly.dev".to_string()),
            Some(GOOD_FINGERPRINT),
        )
        .await
        .expect_err("a key that still cannot sign in is not success");

        // Only meaningful if the server really was written to.
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id")),
            "{:?}",
            fake.calls()
        );
        assert!(
            store::Store::load(ctx.paths.as_ref())
                .await
                .find("build-01")
                .is_some(),
            "the server holds riabuild's key now; `remote forget` has to be able to \
             name it"
        );
    }

    /// The whole point of the saved password, asserted where it is actually
    /// at risk: not in `askpass`'s own unit tests, which prove `ssh_env`
    /// returns three pairs, but across the real sequence of calls, where a
    /// site that kept `RunOptions::default()` is invisible until a developer
    /// on a password-only server is asked for it again mid-run.
    ///
    /// Deliberately *not* a list of the sites: it asserts over every `ssh`
    /// the run made, so a new SSH call added later is covered the day it is
    /// written rather than the day someone remembers to extend a list.
    #[tokio::test]
    async fn every_ssh_in_a_run_can_answer_a_password_prompt_without_asking_again() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with(
                &format!(
                    "ssh-keyscan -t {} -p {} -T 5 {}",
                    host_key::KEY_TYPES,
                    remote().port,
                    remote().host
                ),
                0,
                &format!("{} ssh-ed25519 AAAAstubkeydata\n", remote().host),
                "",
            )
            .with("ssh-keygen -lf -", 0, GOOD_FINGERPRINT_LINE, "")
            // The key works, so the run goes straight past `authorise` and on
            // through `resolve_home` and into the install step — which is
            // where it stops, `uname -sm` naming no Rust target.
            .with("ssh -o BatchMode=yes", 0, "", "")
            .with("ssh", 0, "", "")
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

        let expected = askpass::ssh_env(&remote(), ctx.paths.as_ref());
        let mut checked = 0;
        for call in fake.calls() {
            // `ssh -o BatchMode=yes` is the one family that must *not* carry
            // it: it asks whether the key works, and a saved password that
            // could answer for it would report a working key on a server
            // where there is none. See `authorise::can_sign_in`.
            if !call.starts_with("ssh ") || call.starts_with("ssh -o BatchMode=yes") {
                continue;
            }
            let env = fake.env_of(&call);
            for (key, value) in &expected {
                assert!(
                    env.contains(&(key.clone(), value.clone())),
                    "`{call}` cannot answer a password prompt: {key} missing from {env:?}"
                );
            }
            checked += 1;
        }
        assert!(
            checked > 0,
            "this asserts nothing unless the run actually made an ssh call: {:?}",
            fake.calls()
        );
    }

    /// The helper has to exist before the first connection that might need
    /// it, not be written by whichever step first notices. `SSH_ASKPASS`
    /// naming a path with nothing at it is how `ssh` answers a password
    /// prompt with silence — the developer sees `Permission denied` and no
    /// prompt at all, which is worse than the ten prompts this replaces.
    #[tokio::test]
    async fn the_askpass_helper_is_written_before_the_first_connection() {
        let fake = FakeRunner::new()
            .with("ssh-keygen -t ed25519", 0, "", "")
            .with("ssh -o BatchMode=yes", 0, "", "")
            .with("ssh", 0, "", "");
        let (mut ctx, _home, mut store, fake) = ready_ctx(fake).await;

        let cli = Cli::parse_from(["riabuild", "--check", "remote", "build-01"]);
        let _ = connect_and_setup(&mut ctx, &cli, &mut store, Some("build-01".into()), None).await;

        assert!(
            ctx.paths.askpass_helper().exists(),
            "nothing would answer a password prompt on this run"
        );
        // …and even on `--check`, which writes nothing to the *server*. The
        // helper is this laptop's own file, like the key pair and the
        // known_hosts line above it.
        assert!(
            !fake.calls().is_empty(),
            "only meaningful if the run got as far as connecting: {:?}",
            fake.calls()
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
