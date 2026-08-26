//! Opening the environment on a server: mosh when it can, ssh when it cannot.
//!
//! Both handoffs are spaced by `Ui::blank`, and the spacing is this side's job
//! rather than the server's. `ssh` prints `Connection to … closed.` the instant
//! the remote command ends and `mosh` prints `[mosh is exiting.]` when it lets
//! the terminal go, both without a blank line of their own — so a laptop that
//! printed nothing wedges those between its own lines, and a server that
//! printed one of its own puts it at the top of a fresh mosh screen where
//! there is nothing above it to separate from.

use super::{NO_TMUX, Remote, askpass, mosh, ssh::Ssh};
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::sync::Arc;

/// Seconds between keepalives on a connection that has gone quiet.
const ALIVE_INTERVAL: u32 = 20;

/// Unanswered keepalives before ssh gives up, for the developer's shell.
///
/// Three minutes. Not a round number picked for tidiness: without mosh there is
/// nothing to reconnect, so ssh giving up *is* the session ending, and whatever
/// was running in it goes with it. A train tunnel outlasts the sixty seconds the
/// default would allow, and the cost of tolerating it is a terminal that stops
/// answering for as long as the network is gone — which the developer can see,
/// and which ends by itself.
const SESSION_TOLERANCE: u32 = 9;

/// The same, for a setup run, and deliberately the shorter of the two.
///
/// Setup's exit code is what decides whether a shell opens at all, so a
/// connection that will not come back has to be *reported* rather than waited
/// on: this repository's rule is that a hang presents as a failure and never as
/// a slow success. There is also nothing here for a developer to lose by giving
/// up — `apply()` is safe to run twice, so the answer to a dropped setup is to
/// run it again.
const SETUP_TOLERANCE: u32 = 3;

/// How much network trouble a *live* connection survives, which is the only
/// thing that genuinely differs between these two call sites.
///
/// `TCPKeepAlive=no` is the one that looks like it is switching resilience
/// *off*. It is not: it is on by default, it is answered by the kernel below
/// anything riabuild can tune, and leaving it there makes the tolerance above an
/// upper bound rather than the answer. One mechanism decides when a connection
/// is dead, and it is the one whose numbers are written down here.
///
/// The `ConnectTimeout`/`ConnectionAttempts` pair used to be in this list and
/// is now in [`identity::ssh_options`], where every connection gets it. The
/// argument for keeping it here was that a probe should not wait three
/// minutes — true of the tolerance above, and backwards for the dial, which
/// with no bound at all falls back to the kernel's SYN retry. What is left
/// here is what the tolerance argument actually varies.
///
/// [`identity::ssh_options`]: super::identity::ssh_options
fn resilience_options(tolerance: u32) -> Vec<String> {
    vec![
        format!("ServerAliveInterval={ALIVE_INTERVAL}"),
        format!("ServerAliveCountMax={tolerance}"),
        "TCPKeepAlive=no".to_string(),
    ]
}

/// Provisioning: always `ssh -t`, never mosh.
///
/// mosh does not propagate the remote command's exit status, so a failed setup
/// would look like a success and the flow would open a shell on a broken box.
/// mosh earns its place for the interactive shell, which is the only part that
/// benefits from surviving sleep and roaming.
///
/// Nothing is printed *before* this one: the caller's `Checking <server>`
/// heading already opens with a blank line, and the run on the far side opens
/// with a banner of its own. The blank line after is the one there is nobody
/// else to print — `ssh` ends the session with `Connection to … closed.` and
/// riabuild's next line would otherwise sit directly under it.
pub async fn run_setup(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    command: &str,
    carry: Option<&crate::issued::Working>,
) -> Result<i32> {
    let code = Ssh::to(remote, paths, runner)
        .carry(carry)
        .tty()
        .options(resilience_options(SETUP_TOLERANCE))
        .interactive(command)
        .await;
    ui.blank();
    code
}

/// The developer's shell, by the best route this network allows.
///
/// Three of them, and which one is taken is [`mosh::ask`]'s answer rather than
/// this function's guess. The order below is the order of preference and also
/// the order of how much has to be true for each to work: plain mosh needs UDP
/// to reach the server, the tunnel needs only the ssh connection riabuild was
/// making anyway, and `ssh -t` needs nothing at all.
///
/// `binary` is the server's own riabuild with its environment prefix already on
/// it. Both halves of the tunnel are that binary — see `mosh` — so this is the
/// one argument the mosh path gained over the one it had.
pub async fn open(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    command: &str,
    binary: &str,
    carry: Option<&crate::issued::Working>,
) -> Result<i32> {
    let local_mosh = runner.which("mosh").is_some();
    let route = match local_mosh {
        true => mosh::ask(remote, paths, &runner, binary, carry).await,
        // Nothing on this laptop can speak mosh, so asking the server about it
        // would be one connection spent on an answer riabuild cannot use.
        false => mosh::Route::NoServer,
    };

    // The tunnel, which is the whole of what this module gained: a network that
    // drops UDP is a conference guest network or a corporate egress filter, not
    // a mistake anybody made, and it used to cost the developer nineteen
    // seconds of silence and then a plain `ssh` with no explanation.
    if route == mosh::Route::OverTcp {
        if let Some(code) =
            mosh::open_over_tcp(remote, paths, runner.clone(), ui, command, binary, carry).await
        {
            return Ok(code);
        }
        ui.warn("mosh could not be tunnelled over TCP — falling back to ssh.");
    } else if local_mosh && route == mosh::Route::Direct {
        let ssh = format!(
            "ssh {}",
            Ssh::to(remote, paths, runner.clone())
                .carry(carry)
                .options_only()
                .join(" ")
        );
        let args = [
            format!("--ssh={ssh}"),
            remote.target(),
            "--".to_string(),
            // mosh `execvp`s this with no shell, so it is handed a complete
            // argv-shaped command rather than something needing parsing.
            //
            // `env` wraps the login shell rather than riding inside `command`,
            // and that is the whole point of this line: `-l` makes `/bin/sh`
            // read the account's profile, which on a cloudcli box is where the
            // tmux `exec` lives. `command` already carries `CLOUDCLI_NO_TMUX`
            // from `env_prefix`, but that `env` does not run until the profile
            // has already had its say — so this session would open inside tmux
            // and the copy further in would arrive too late to stop it.
            "env".to_string(),
            format!("{}={}", NO_TMUX.0, NO_TMUX.1),
            "/bin/sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        // mosh execs `ssh` to bootstrap the session, and that `ssh` inherits
        // this environment — so a server reached by password is reached by the
        // saved one here too, rather than prompting where mosh has already
        // taken over the terminal.
        //
        // The blank line is the last thing riabuild prints before the session
        // and the only thing left between the run and `[mosh is exiting.]`
        // once mosh gives the terminal back. The session itself opens with
        // none of its own, so this is the whole gap in both directions.
        ui.blank();
        let code = runner
            .run_interactive("mosh", &refs, &askpass::run_options(remote, paths))
            .await?;
        // Any non-zero code is mosh's own failure, and none of them is the
        // developer's. mosh does not propagate the remote command's exit
        // status — a session whose shell exits 7 still returns 0 — so there is
        // no status here to mistake for one. Nor is there a setup run to repeat:
        // `run_setup` is always plain `ssh`, and `open` has one caller.
        //
        // This was keyed to 5 and never fired. Nothing in mosh exits 5:
        // `mosh-client` gives up after roughly nineteen seconds of silence and
        // returns 1, the same code it uses for a bind failure and every other
        // network exception, and the perl wrapper's own `die` paths exit 10 or
        // 255. A server that drops the UDP session therefore cost the developer
        // the countdown and then handed back a laptop prompt.
        if code == 0 {
            return Ok(code);
        }
        ui.warn("mosh could not connect — falling back to ssh.");
    } else if !local_mosh {
        ui.note(
            "Install mosh for a connection that survives sleep and roaming: `brew install mosh`",
        );
    } else {
        ui.note(&format!(
            "{} has no mosh-server; using ssh. Install mosh there for a connection that survives sleep.",
            remote.name
        ));
    }

    // No `env` wrapper here, and no `SetEnv` either. `ssh host <command>` runs
    // the account's shell non-interactively and non-login, so the profile that
    // starts tmux is never read on this path at all; the copy `env_prefix` puts
    // inside `command` is in place well before riabuild spawns the developer's
    // bash. `-o SetEnv=` would be the only way to get in front of a `.bashrc`
    // that starts tmux with no interactivity guard, and it does nothing without
    // a matching `AcceptEnv` on the server while failing outright on an ssh
    // older than 7.8 — a certain cost against a hypothetical gain.
    // The same gap mosh gets above, and here it separates the session from the
    // note or warning immediately in front of it as well.
    ui.blank();
    Ssh::to(remote, paths, runner)
        .carry(carry)
        .tty()
        .options(resilience_options(SESSION_TOLERANCE))
        .interactive(command)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// The server's own riabuild, env-prefixed, as `connect` builds it.
    ///
    /// Only the mosh probe and the two halves of the tunnel append to it, and
    /// no assertion below depends on its contents — what matters is that the
    /// value reaching `open` is the one `env_command` produced rather than a
    /// bare `riabuild` the server's `PATH` would have to resolve.
    const BINARY: &str = "env 'RIABUILD_ROOT=/home/ada/.riabuild-remote/abc' \
                          '/home/ada/.riabuild/riabuild/2026.08.26/riabuild'";

    /// A fake whose mosh probe answers "could not tell".
    ///
    /// The probe is a held child, so scripting it is `spawning` rather than
    /// `with`: this one starts, exits 0, and prints no port line — which
    /// `mosh::ask` reads as [`mosh::Route::Direct`], the answer it gives to
    /// every question it could not settle. That is the branch every test below
    /// that expects plain mosh is about.
    fn cannot_tell() -> FakeRunner {
        FakeRunner::new().spawning("ssh", 0, "")
    }

    fn ssh_call(fake: &Arc<FakeRunner>) -> String {
        fake.calls()
            .into_iter()
            .find(|call| call.starts_with("ssh "))
            .expect("an ssh call")
    }

    /// The `ssh` a fallback `open` runs, with no mosh anywhere to take the path
    /// in front of it.
    async fn fallback_call() -> String {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));
        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("falls back");
        ssh_call(&fake)
    }

    /// The `ssh` a setup run performs.
    async fn setup_call() -> String {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));
        run_setup(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild --no-shell",
            None,
        )
        .await
        .expect("runs");
        ssh_call(&fake)
    }

    /// Setup is the longest-lived connection riabuild makes — the whole task DAG
    /// runs inside it — and it carried no keepalive at all, so a network that
    /// went silent under it left riabuild waiting on a socket nothing would ever
    /// close. The fallback shell had one; this is the path that did not.
    #[tokio::test]
    async fn the_setup_run_notices_a_network_that_has_gone_silent() {
        let call = setup_call().await;
        assert!(call.contains("ServerAliveInterval="), "{call}");
    }

    /// The asymmetry is the point, so both are asserted in one place: collapsing
    /// them to a single number is the tidying this test exists to fail.
    ///
    /// Losing an interactive session costs a developer whatever was running in
    /// it, and freezing for three minutes costs three minutes — so the shell
    /// rides out an outage the setup run does not. Setup is the opposite trade:
    /// its exit code decides whether a shell opens at all, and this repository's
    /// rule for it is that a hang must present as a failure rather than as a
    /// slow success.
    #[tokio::test]
    async fn the_session_rides_out_an_outage_the_setup_run_gives_up_on() {
        let session = fallback_call().await;
        let setup = setup_call().await;

        assert!(session.contains("ServerAliveCountMax=9"), "{session}");
        assert!(setup.contains("ServerAliveCountMax=3"), "{setup}");
    }

    /// One mechanism decides when a connection is dead, not the minimum of two.
    ///
    /// `TCPKeepAlive` is on by default and is answered by the kernel, below
    /// anything riabuild can tune — so with it left alone the tolerance set
    /// above is an upper bound rather than the answer. Off, the `ServerAlive`
    /// pair is the whole of the decision, which is the only way the three
    /// minutes above means three minutes.
    #[tokio::test]
    async fn nothing_below_ssh_decides_when_a_connection_is_dead() {
        assert!(fallback_call().await.contains("TCPKeepAlive=no"));
        assert!(setup_call().await.contains("TCPKeepAlive=no"));
    }

    /// A lost packet at dial time is not a failed run, and an unreachable server
    /// is not a two-minute wait: ssh's default is the kernel's connect timeout,
    /// which is neither bounded by riabuild nor short enough to report.
    #[tokio::test]
    async fn a_dial_is_bounded_and_retried_rather_than_failing_on_one_packet() {
        for call in [fallback_call().await, setup_call().await] {
            assert!(call.contains("ConnectTimeout="), "{call}");
            assert!(call.contains("ConnectionAttempts=2"), "{call}");
        }
    }

    #[tokio::test]
    async fn mosh_is_used_when_the_server_has_it() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(cannot_tell().with("ssh", 0, "", "").with("mosh", 0, "", ""));
        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("opens");

        assert!(
            fake.calls().iter().any(|call| call.starts_with("mosh ")),
            "{:?}",
            fake.calls()
        );
    }

    /// The one place `env_prefix`'s copy arrives too late.
    ///
    /// mosh runs `/bin/sh -lc <command>`, and `-l` reads the account's profile
    /// — which on a cloudcli box is where the tmux `exec` lives. By the time
    /// the `env` inside `command` runs, the session is already in a pane. So
    /// the assertion is about *order*, not presence: the variable has to be set
    /// on the outside of `/bin/sh`, before that shell reads anything.
    #[tokio::test]
    async fn the_login_shell_mosh_starts_is_told_not_to_start_tmux() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(cannot_tell().with("ssh", 0, "", "").with("mosh", 0, "", ""));

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "env 'RIABUILD_ROOT=/home/dev/.riabuild-remote/abc' riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("opens");

        let mosh = fake
            .calls()
            .into_iter()
            .find(|call| call.starts_with("mosh "))
            .expect("mosh ran");
        assert!(
            mosh.contains("env CLOUDCLI_NO_TMUX=1 /bin/sh -lc"),
            "the login shell must start with it already set: {mosh}"
        );
    }

    #[tokio::test]
    async fn a_server_without_mosh_falls_back_to_ssh_rather_than_stopping() {
        // A blocked UDP port is a cloud-firewall default, not a developer error.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        // `which` only knows stubbed programs, so mosh must be stubbed for the
        // laptop-has-mosh branch to be the one under test; the server-side probe
        // is what fails here.
        //
        // Exit 3 is `mosh::NO_MOSH_SERVER` — the one status the probe script
        // returns for itself, chosen to be distinguishable from ssh's own 255
        // and from a shell's 126/127, so "this server has no mosh" is never
        // read off a connection that failed.
        let fake = Arc::new(
            FakeRunner::new()
                .with("mosh", 0, "", "")
                .with("ssh", 0, "", "")
                .spawning("ssh", 3, "mosh-server: not found"),
        );

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("falls back");

        assert!(!fake.calls().iter().any(|call| call.starts_with("mosh ")));
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("-t") && call.contains("riabuild shell")),
            "{:?}",
            fake.calls()
        );
    }

    /// mosh's real "could not connect" exit is 1, and nothing exits 5.
    ///
    /// `mosh-client` gives up after roughly nineteen seconds of silence and
    /// returns 1 — the same code it uses for a bind failure and for every other
    /// network exception. A fallback keyed to 5 therefore never fires: the
    /// developer waits out the countdown and is handed back their laptop prompt
    /// with no shell and no explanation.
    #[tokio::test]
    async fn mosh_that_cannot_connect_falls_back_to_ssh() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(cannot_tell().with("ssh", 0, "", "").with("mosh", 1, "", ""));

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("falls back");

        assert!(
            fake.calls().iter().any(|call| call.starts_with("mosh ")),
            "mosh is still tried first: {:?}",
            fake.calls()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh -t ") && call.contains("riabuild shell")),
            "a mosh that could not connect must be followed by an ssh session: {:?}",
            fake.calls()
        );
    }

    /// The other side of the line the test above moved.
    ///
    /// Widening the fallback to every non-zero code is only safe while zero
    /// still means "the session happened". A second `ssh` after a mosh the
    /// developer simply exited would reopen a shell they had just closed.
    #[tokio::test]
    async fn a_mosh_session_that_ended_normally_does_not_reopen_over_ssh() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(cannot_tell().with("ssh", 0, "", "").with("mosh", 0, "", ""));

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("opens");

        assert!(
            !fake.calls().iter().any(|call| call.starts_with("ssh -t ")),
            "a session that ended normally must not be reopened: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn no_mosh_on_the_laptop_falls_back_to_ssh_and_says_so() {
        // Distinct from the server-side gap above: here the laptop itself has no
        // mosh binary at all, so the server is never asked about mosh — and
        // that is now a connection saved rather than a tidiness, because the
        // probe is a held ssh that binds a UDP port on the far side.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new());

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("falls back");

        assert!(
            fake.spawns().is_empty(),
            "the laptop has no mosh, so the server was never asked: {:?}",
            fake.spawns()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh") && call.contains("-t")),
            "{:?}",
            fake.calls()
        );
    }

    /// A network that drops UDP takes the tunnel, and never the direct mosh
    /// that is about to fail.
    ///
    /// The probe is driven by hand here because that is the only way to reach
    /// this branch: the server announces a port, nothing answers on it, and
    /// `mosh::ask` returns [`mosh::Route::OverTcp`]. This laptop has no
    /// `mosh-client`, so the tunnel then gives up — which is the *second* thing
    /// asserted, because a tunnel that cannot start must still leave the
    /// developer with a shell.
    #[tokio::test]
    async fn a_network_that_blocks_udp_never_runs_the_mosh_that_would_fail() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("mosh", 0, "", "")
                .with("ssh", 0, "", ""),
        );

        // A port that was free a moment ago: from the laptop it is
        // indistinguishable from a firewall dropping the datagram, which is the
        // point — both mean this session will not work over UDP.
        let silent = tokio::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a socket");
        let port = silent.local_addr().expect("an address").port();
        drop(silent);

        // `join!` rather than `spawn`, so the test can keep lending `open` the
        // borrowed remote, paths and `Ui` it takes. All three have to outlive
        // the call for exactly that reason.
        let (server, ui) = (remote(), Ui::new(true));
        let opening = open(
            &server,
            &paths,
            fake.clone(),
            &ui,
            "riabuild shell",
            BINARY,
            None,
        );
        let answering = async {
            for _ in 0..100_000 {
                if let Some(mut pipes) = fake.pipes(0) {
                    use tokio::io::AsyncWriteExt;
                    pipes
                        .to_riabuild
                        .write_all(format!("RIABUILD-UDP-ECHO {port}\n").as_bytes())
                        .await
                        .expect("announces its port");
                    // Held open: an echo responder does not exit after printing
                    // its line, and one that closed the pipe would look like a
                    // server that never had riabuild on it.
                    std::future::pending::<()>().await;
                }
                tokio::task::yield_now().await;
            }
        };
        // `select!`, not `join!`: the responder is deliberately a future that
        // never finishes — a real one stays up for the whole probe — so joining
        // the two would wait for a server that has nothing left to say.
        tokio::select! {
            code = opening => { code.expect("falls back to ssh"); }
            () = answering => unreachable!("the responder outlives the probe"),
        }

        assert!(
            !fake.calls().iter().any(|call| call.starts_with("mosh ")),
            "the direct mosh is the one thing this network cannot carry: {:?}",
            fake.calls()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh -t ") && call.contains("riabuild shell")),
            "a tunnel that could not start must still leave a shell: {:?}",
            fake.calls()
        );
    }

    /// One blank line in front of the session, whichever way in it takes.
    ///
    /// Both are asserted together because the ssh fallback is the branch that
    /// grows things in front of it — a warning that mosh could not connect, a
    /// note that the server has no `mosh-server` — and it is the branch where
    /// a gap printed once at the top of `open` would end up on the wrong side
    /// of them.
    #[tokio::test]
    async fn one_blank_line_separates_the_run_from_the_session() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());

        let with_mosh = Arc::new(cannot_tell().with("ssh", 0, "", "").with("mosh", 0, "", ""));
        let ui = Ui::new(false);
        open(
            &remote(),
            &paths,
            with_mosh,
            &ui,
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("opens");
        assert_eq!(ui.blanks(), 1, "mosh");

        // No mosh on the laptop: a note is printed first, and the gap belongs
        // under it rather than over it.
        let without_mosh = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));
        let ui = Ui::new(false);
        open(
            &remote(),
            &paths,
            without_mosh,
            &ui,
            "riabuild shell",
            BINARY,
            None,
        )
        .await
        .expect("falls back");
        assert_eq!(ui.blanks(), 1, "ssh");
    }

    /// `ssh` prints `Connection to … closed.` the instant the remote command
    /// ends, with no line of its own on either side. The line above it is the
    /// server's — `provision` prints one at the end of a `--no-shell` run —
    /// and this is the one below, which nothing else is in a position to
    /// print: riabuild's next line lands directly under it otherwise.
    #[tokio::test]
    async fn the_setup_run_leaves_a_line_under_ssh_s_closing_message() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));
        let ui = Ui::new(false);

        run_setup(&remote(), &paths, fake, &ui, "riabuild --no-shell", None)
            .await
            .expect("runs");

        assert_eq!(ui.blanks(), 1);
    }

    #[tokio::test]
    async fn setup_always_uses_plain_ssh_never_mosh() {
        // mosh does not propagate the remote command's exit status, so a failed
        // setup would look like a success and the flow would open a shell on a
        // broken box.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));

        let code = run_setup(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild --no-shell",
            None,
        )
        .await
        .expect("runs");
        assert_eq!(code, 0);
        assert!(fake.calls().iter().all(|call| !call.starts_with("mosh")));
        assert!(
            fake.calls().iter().any(|call| call.starts_with("ssh -t ")),
            "{:?}",
            fake.calls()
        );
    }
}
