//! The one place an `ssh` invocation for remote mode is assembled.
//!
//! Nine call sites used to write out `identity::ssh_options` → target →
//! command → `collect` → `run` by hand, differing only in whether they piped
//! stdin, asked for a `-t`, added keepalives, or deliberately left
//! `askpass::run_options` off. A new one that forgot the askpass environment,
//! or forgot to thread `carry` through, looked exactly like the eight that did
//! not — both of those shipped.
//!
//! What makes this a fix rather than tidying is where the timeout lives.
//! `ConnectTimeout` and `ConnectionAttempts` were written beside the two
//! `shell.rs` call sites, so the other nine got the kernel's SYN retry
//! instead: an unreachable server took minutes per connection, and one
//! `riabuild remote` opens about ten. They are in
//! [`identity::ssh_options`] now — under this builder, which every site goes
//! through — so a call site cannot forget an option it never writes.
//!
//! Differences that are real stay expressible, and none of them is flattened:
//! `-t` ([`Ssh::tty`]), piped stdin ([`Ssh::stdin`]), the keepalive pair whose
//! tolerance genuinely differs between a setup run and a session
//! ([`Ssh::option`]), the two probes that must **not** be able to answer a
//! password prompt ([`Ssh::without_askpass`]), and the one step that offers
//! the developer's own keys as well as riabuild's ([`Ssh::every_identity`]).

use super::{Remote, askpass, identity, issued};
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandOutput, CommandRunner, RunOptions};
use std::sync::Arc;
use std::time::Duration;

/// One `ssh` to one server, composed rather than concatenated.
pub(crate) struct Ssh<'a> {
    remote: &'a Remote,
    paths: &'a dyn Paths,
    runner: Arc<dyn CommandRunner>,
    identities_only: bool,
    offer: Option<identity::Offered<'a>>,
    options: Vec<String>,
    tty: bool,
    stdin: Option<Vec<u8>>,
    askpass: bool,
    patience: Option<Duration>,
}

impl<'a> Ssh<'a> {
    /// The defaults every connection wants: riabuild's own key and nothing
    /// else offered, the askpass environment attached, no pty, no stdin.
    pub(crate) fn to(
        remote: &'a Remote,
        paths: &'a dyn Paths,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            remote,
            paths,
            runner,
            identities_only: true,
            offer: None,
            options: Vec::new(),
            tty: false,
            stdin: None,
            askpass: true,
            patience: None,
        }
    }

    /// How long this call may take before riabuild kills the `ssh`, for the one
    /// site whose work is not a round trip.
    ///
    /// Every other site leaves it alone and gets `RunOptions`' default ceiling,
    /// which is right for them: they run a command on a server and read a line
    /// back, so ten minutes means the connection has hung. The binary push does
    /// not fit that — see `install::PUSH_PATIENCE`, which is the only caller.
    ///
    /// Note this bounds *riabuild's* wait, and is unrelated to
    /// `ConnectTimeout`, which bounds the dial. A site wanting both says both.
    pub(crate) fn patience(mut self, patience: Duration) -> Self {
        self.patience = Some(patience);
        self
    }

    /// An issued identity this laptop is carrying because its own key cannot
    /// sign in to this server — see `identity::ssh_options`.
    pub(crate) fn carry(mut self, carry: Option<&'a issued::Working>) -> Self {
        self.offer = carry.map(identity::Offered::from);
        self
    }

    /// One identity in an agent riabuild owns, named directly rather than
    /// through a [`issued::Working`]. `issued::agent`'s own probe is asking
    /// whether a key *becomes* a `Working`, so it has no `Working` yet.
    pub(crate) fn offering(mut self, offered: identity::Offered<'a>) -> Self {
        self.offer = Some(offered);
        self
    }

    /// Drops `IdentitiesOnly=yes`, so whatever the developer's own agent and
    /// `~/.ssh` hold is offered too. Exactly two steps want this, and both are
    /// about getting *in* to a server riabuild's key cannot reach yet.
    pub(crate) fn every_identity(mut self) -> Self {
        self.identities_only = false;
        self
    }

    /// One `-o` option of this call site's own.
    ///
    /// These lead the base list rather than trailing it, and that is load
    /// bearing in one direction: `ssh` takes the **first** value it is given
    /// for a repeated option, so a call site can override a base default and
    /// never the other way round. It is also the order every site already
    /// wrote by hand, which is what lets this be a refactor.
    pub(crate) fn option(mut self, option: impl Into<String>) -> Self {
        self.options.push("-o".to_string());
        self.options.push(option.into());
        self
    }

    /// [`Ssh::option`] for a whole list, in order.
    pub(crate) fn options(mut self, options: impl IntoIterator<Item = String>) -> Self {
        for option in options {
            self = self.option(option);
        }
        self
    }

    /// `-t`: a pty on the far side, for the two handoffs that carry a
    /// developer's terminal rather than capturing output.
    pub(crate) fn tty(mut self) -> Self {
        self.tty = true;
        self
    }

    /// Bytes for the remote command's stdin — a secret, or the server's own
    /// binary. Never argv: `ps` is readable by every other developer on the
    /// box.
    pub(crate) fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Runs with `RunOptions::default()` rather than the askpass environment.
    ///
    /// The whole of the exception, and it is deliberate: these are the probes
    /// that ask "can *this key* sign in", and an askpass able to answer a
    /// password prompt would let a saved password make the answer yes on a
    /// server where the key does not work at all — which is the state the
    /// warning path exists to report. `BatchMode=yes` at those sites already
    /// forbids every prompt; this is the belt beside those braces, and it is a
    /// method on the builder so that the exception is visible at the call site
    /// instead of being the absence of a line.
    pub(crate) fn without_askpass(mut self) -> Self {
        self.askpass = false;
        self
    }

    /// Just the options — no target, no command. For the two places that hand
    /// them to something else that builds its own argv: mosh's `--ssh=` and
    /// the channel supervisor's `Tunnel`.
    pub(crate) fn options_only(&self) -> Vec<String> {
        let mut args = self.options.clone();
        args.extend(identity::ssh_options(
            self.remote,
            self.paths,
            self.identities_only,
            self.offer,
        ));
        args
    }

    /// The complete argv `ssh` is given, command included.
    pub(crate) fn argv(&self, command: &str) -> Vec<String> {
        let mut args = Vec::new();
        if self.tty {
            args.push("-t".to_string());
        }
        args.extend(self.options_only());
        args.push(self.remote.target());
        args.push(command.to_string());
        args
    }

    fn run_options(&mut self) -> RunOptions {
        let base = if self.askpass {
            askpass::run_options(self.remote, self.paths)
        } else {
            RunOptions::default()
        };
        RunOptions {
            stdin: self.stdin.take(),
            timeout: self.patience.or(base.timeout),
            ..base
        }
    }

    /// Captured: riabuild reads the output.
    pub(crate) async fn run(mut self, command: &str) -> Result<CommandOutput> {
        let args = self.argv(command);
        let options = self.run_options();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner.run("ssh", &refs, &options).await
    }

    /// A handoff: the child gets riabuild's terminal.
    pub(crate) async fn interactive(mut self, command: &str) -> Result<i32> {
        let args = self.argv(command);
        let options = self.run_options();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner.run_interactive("ssh", &refs, &options).await
    }
}

#[cfg(test)]
mod tests {
    //! The argv every `ssh` call site produces, pinned one site at a time.
    //!
    //! This is the test that makes collapsing nine hand-rolled assemblies into
    //! one builder a refactor rather than a rewrite: each case below is the
    //! exact invocation its call site made before the builder existed, plus
    //! the `ConnectTimeout`/`ConnectionAttempts` pair that was the point of
    //! doing it.
    //!
    //! Two positions moved and both are inert, because `ssh` takes the
    //! **first** value for a repeated option and neither of these is repeated:
    //! the dial bound now sits inside the base list rather than after the
    //! keepalives, and `IdentitiesOnly=yes` now trails a carried identity
    //! rather than sitting between its two halves. Where order is **not**
    //! inert — the two `-i` at a site carrying an issued identity, which is
    //! the order the keys are *offered* in — it is unchanged, and the cases
    //! below are what say so.

    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
    use riabuild_ui::Ui;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    fn carried() -> issued::Working {
        issued::Working {
            label: "prod-bastion".into(),
            socket: "/run/riabuild/sock".into(),
            public_key_path: "/run/riabuild/k1.pub".into(),
        }
    }

    /// A real riabuild key line, for the one site that parses what it installs.
    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP riabuild 9544e195 ada@build-01:22";

    struct Fixture {
        _home: tempfile::TempDir,
        paths: RealPaths,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::TempDir::new().expect("tempdir");
            let paths = RealPaths::rooted_at(home.path());
            Self { _home: home, paths }
        }

        /// The base list every case below opens with, spelled out once so a
        /// case reads as "and what this site adds".
        fn base(&self, identities_only: bool) -> String {
            let known = self.paths.known_hosts_file();
            let key = identity::key_path(&remote(), &self.paths);
            let mut base = format!(
                "-p 2222 -F /dev/null -o UserKnownHostsFile={} -o StrictHostKeyChecking=yes \
                 -o ConnectTimeout=15 -o ConnectionAttempts=2 -i {}",
                known.display(),
                key.display()
            );
            if identities_only {
                base.push_str(" -o IdentitiesOnly=yes");
            }
            base
        }

        fn carrying(&self) -> String {
            let known = self.paths.known_hosts_file();
            let key = identity::key_path(&remote(), &self.paths);
            format!(
                "-p 2222 -F /dev/null -o UserKnownHostsFile={} -o StrictHostKeyChecking=yes \
                 -o ConnectTimeout=15 -o ConnectionAttempts=2 -i {} \
                 -o IdentityAgent=/run/riabuild/sock -i /run/riabuild/k1.pub -o IdentitiesOnly=yes",
                known.display(),
                key.display()
            )
        }
    }

    /// The `ssh` a call recorded, whole, so a case pins the argv rather than a
    /// substring of it.
    fn ssh_call(fake: &Arc<FakeRunner>) -> String {
        fake.calls()
            .into_iter()
            .find(|call| call.starts_with("ssh "))
            .expect("an ssh call")
    }

    fn fake() -> Arc<FakeRunner> {
        Arc::new(FakeRunner::new().with("ssh", 0, "", ""))
    }

    // ---- the sites that run one command and read the answer ---------------

    #[tokio::test]
    async fn ssh_once_is_the_base_list_and_nothing_else() {
        let f = Fixture::new();
        let runner = fake();
        crate::ssh_once(&remote(), &f.paths, runner.clone(), "true", None)
            .await
            .expect("runs");
        assert_eq!(
            ssh_call(&runner),
            format!("ssh {} ada@build-01.fly.dev true", f.base(true))
        );
    }

    #[tokio::test]
    async fn ssh_once_carrying_an_issued_identity_offers_riabuilds_key_first() {
        let f = Fixture::new();
        let runner = fake();
        crate::ssh_once(
            &remote(),
            &f.paths,
            runner.clone(),
            "true",
            Some(&carried()),
        )
        .await
        .expect("runs");
        assert_eq!(
            ssh_call(&runner),
            format!("ssh {} ada@build-01.fly.dev true", f.carrying())
        );
    }

    #[tokio::test]
    async fn the_mosh_probe_asks_the_server_for_mosh_server() {
        let f = Fixture::new();
        let runner = Arc::new(
            FakeRunner::new()
                .with("ssh", 1, "", "")
                .with("mosh", 0, "", ""),
        );
        crate::shell::open(
            &remote(),
            &f.paths,
            runner.clone(),
            &Ui::new(true),
            "riabuild shell",
            None,
        )
        .await
        .expect("falls back");
        assert_eq!(
            runner
                .calls()
                .into_iter()
                .find(|call| call.contains("mosh-server"))
                .expect("a probe"),
            format!(
                "ssh {} ada@build-01.fly.dev command -v mosh-server",
                f.base(true)
            )
        );
    }

    #[tokio::test]
    async fn seeding_a_github_sign_in_pipes_the_token_over_the_base_list() {
        let f = Fixture::new();
        let gh = f
            .paths
            .tool_dir("gh", riabuild_fetch::tools::GH_VERSION)
            .join(riabuild_fetch::tools::GH_MEMBER)
            .to_string_lossy()
            .into_owned();
        let runner = Arc::new(
            FakeRunner::new()
                .with(&format!("{gh} auth token"), 0, "gho_x\n", "")
                .with("ssh", 0, "", ""),
        );
        crate::seed::seed_github(
            &remote(),
            &f.paths,
            runner.clone(),
            &Ui::new(true),
            "/r/riabuild",
            None,
        )
        .await
        .expect("seeds");
        assert_eq!(
            ssh_call(&runner),
            format!(
                "ssh {} ada@build-01.fly.dev /r/riabuild internal seed-github",
                f.base(true)
            )
        );
        assert_eq!(runner.stdin_text_of("ssh").as_deref(), Some("gho_x"));
    }

    // ---- the two handoffs, which add a pty and their own keepalives -------

    #[tokio::test]
    async fn a_setup_run_adds_a_pty_and_the_shorter_tolerance() {
        let f = Fixture::new();
        let runner = fake();
        crate::shell::run_setup(
            &remote(),
            &f.paths,
            runner.clone(),
            &Ui::new(true),
            "riabuild --no-shell",
            None,
        )
        .await
        .expect("runs");
        assert_eq!(
            ssh_call(&runner),
            format!(
                "ssh -t -o ServerAliveInterval=20 -o ServerAliveCountMax=3 -o TCPKeepAlive=no {} \
                 ada@build-01.fly.dev riabuild --no-shell",
                f.base(true)
            )
        );
    }

    #[tokio::test]
    async fn a_session_adds_a_pty_and_the_longer_tolerance() {
        let f = Fixture::new();
        let runner = fake();
        crate::shell::open(
            &remote(),
            &f.paths,
            runner.clone(),
            &Ui::new(true),
            "riabuild shell",
            None,
        )
        .await
        .expect("opens");
        assert_eq!(
            runner
                .calls()
                .into_iter()
                .find(|call| call.starts_with("ssh -t"))
                .expect("the session"),
            format!(
                "ssh -t -o ServerAliveInterval=20 -o ServerAliveCountMax=9 -o TCPKeepAlive=no {} \
                 ada@build-01.fly.dev riabuild shell",
                f.base(true)
            )
        );
    }

    /// mosh gets the option list as one `--ssh=` string and builds its own
    /// argv from there, so what is pinned here is that list.
    #[tokio::test]
    async fn mosh_bootstraps_over_the_same_options() {
        let f = Fixture::new();
        let runner = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "/usr/bin/mosh-server\n", "")
                .with("mosh", 0, "", ""),
        );
        crate::shell::open(
            &remote(),
            &f.paths,
            runner.clone(),
            &Ui::new(true),
            "riabuild shell",
            None,
        )
        .await
        .expect("opens");
        let mosh = runner
            .calls()
            .into_iter()
            .find(|call| call.starts_with("mosh "))
            .expect("a mosh call");
        assert_eq!(
            mosh,
            format!(
                "mosh --ssh=ssh {} ada@build-01.fly.dev -- env CLOUDCLI_NO_TMUX=1 /bin/sh -lc \
                 riabuild shell",
                f.base(true)
            )
        );
    }

    // ---- the probes, which must not be able to answer a password ---------

    #[tokio::test]
    async fn the_key_probe_batches_and_carries_no_askpass() {
        let f = Fixture::new();
        let runner = fake();
        crate::authorise::can_sign_in(&remote(), &f.paths, runner.clone())
            .await
            .expect("probes");
        assert_eq!(
            ssh_call(&runner),
            format!(
                "ssh -o BatchMode=yes {} ada@build-01.fly.dev true",
                f.base(true)
            )
        );
        assert!(
            runner.env_of("ssh").is_empty(),
            "a probe that could answer a password prompt is not a probe"
        );
    }

    // ---- installing the key, the one step that offers every identity ------

    #[tokio::test]
    async fn installing_the_key_offers_the_developers_own_identities_too() {
        let f = Fixture::new();
        let runner = fake();
        crate::authorise::copy::install_key(&remote(), &f.paths, runner.clone(), KEY, None)
            .await
            .expect("installs");
        let call = ssh_call(&runner);
        assert!(
            call.starts_with(&format!("ssh {} ada@build-01.fly.dev ", f.base(false))),
            "{call}"
        );
    }

    #[tokio::test]
    async fn installing_the_key_over_an_issued_identity_pins_it_to_that_one() {
        let f = Fixture::new();
        let runner = fake();
        crate::authorise::copy::install_key(
            &remote(),
            &f.paths,
            runner.clone(),
            KEY,
            Some(&carried()),
        )
        .await
        .expect("installs");
        let call = ssh_call(&runner);
        assert!(
            call.starts_with(&format!("ssh {} ada@build-01.fly.dev ", f.carrying())),
            "{call}"
        );
    }

    // ---- the sites reached only through a real server, pinned as built ----

    /// `install`'s binary stream, `session`'s namespace write, `authorise`'s
    /// method probe and `issued::agent`'s per-key probe each need a live
    /// server, an API client or a running `ssh-agent` to reach. What they
    /// choose is still pinned: these are the builder expressions those call
    /// sites use, and the argv they produce.
    #[test]
    fn every_remaining_call_site_composes_the_same_way() {
        let f = Fixture::new();
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());

        // install::SshCtx::ssh_with_stdin — the base list, plus bytes.
        assert_eq!(
            Ssh::to(&remote(), &f.paths, runner.clone())
                .stdin(b"binary".to_vec())
                .argv("cat > riabuild")
                .join(" "),
            format!("{} ada@build-01.fly.dev cat > riabuild", f.base(true))
        );

        // session::write_into_namespace — the same, for a secret.
        assert_eq!(
            Ssh::to(&remote(), &f.paths, runner.clone())
                .carry(None)
                .stdin(b"token".to_vec())
                .argv("/bin/sh -c 'cat > t'")
                .join(" "),
            format!("{} ada@build-01.fly.dev /bin/sh -c 'cat > t'", f.base(true))
        );

        // authorise's method probe — every identity, and no askpass.
        assert_eq!(
            Ssh::to(&remote(), &f.paths, runner.clone())
                .every_identity()
                .option("PreferredAuthentications=none")
                .option("BatchMode=yes")
                .without_askpass()
                .argv("true")
                .join(" "),
            format!(
                "-o PreferredAuthentications=none -o BatchMode=yes {} ada@build-01.fly.dev true",
                f.base(false)
            )
        );

        // issued::agent::Agent::probe — one agent key, named directly.
        assert_eq!(
            Ssh::to(&remote(), &f.paths, runner.clone())
                .offering(identity::Offered {
                    socket: std::path::Path::new("/run/riabuild/sock"),
                    public_key_path: std::path::Path::new("/run/riabuild/k1.pub"),
                })
                .option("BatchMode=yes")
                .without_askpass()
                .argv("true")
                .join(" "),
            format!(
                "-o BatchMode=yes {} ada@build-01.fly.dev true",
                f.carrying()
            )
        );

        // channel::open_shell's Tunnel — options, with no target or command.
        assert_eq!(
            Ssh::to(&remote(), &f.paths, runner)
                .carry(None)
                .options_only()
                .join(" "),
            f.base(true)
        );
    }

    /// The whole point of the builder, asserted about the builder rather than
    /// about any one site: there is no way to compose an `ssh` here that waits
    /// on the kernel's SYN retry instead of a bound riabuild chose.
    #[test]
    fn no_call_site_can_leave_the_dial_unbounded() {
        let f = Fixture::new();
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        for argv in [
            Ssh::to(&remote(), &f.paths, runner.clone()).argv("true"),
            Ssh::to(&remote(), &f.paths, runner.clone())
                .every_identity()
                .tty()
                .without_askpass()
                .argv("true"),
            Ssh::to(&remote(), &f.paths, runner.clone())
                .carry(Some(&carried()))
                .argv("true"),
            Ssh::to(&remote(), &f.paths, runner).options_only(),
        ] {
            let argv = argv.join(" ");
            assert!(argv.contains("-o ConnectTimeout=15"), "{argv}");
            assert!(argv.contains("-o ConnectionAttempts=2"), "{argv}");
        }
    }

    /// The other side of collapsing every `ssh` into one place: what that one
    /// place must never grow.
    ///
    /// The clipboard channel is `ssh -T <host> riabuild channel pump` and asks
    /// an SSH server for command execution and nothing else — no `-R`, no
    /// `ExitOnForwardFailure`, no `StreamLocalBindUnlink`. Its option list is
    /// this builder's, so a forward added here for some other call site would
    /// reach the transport too, and the supervisor's own guard
    /// (`riabuild_channel::supervisor`) only covers the argv *it* composes. An
    /// option list that reintroduced a reverse forward would restore a
    /// dependency hardened servers refuse outright and put the socket's
    /// lifecycle back in `sshd`'s hands — see the exec-transport design.
    #[test]
    fn nothing_the_builder_composes_asks_for_a_forward() {
        let f = Fixture::new();
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let argv = Ssh::to(&remote(), &f.paths, runner)
            .carry(Some(&carried()))
            .tty()
            .options(["ServerAliveInterval=20".to_string()])
            .argv("riabuild channel pump")
            .join(" ");
        for forward in [
            " -R",
            " -L",
            "ExitOnForwardFailure",
            "StreamLocalBindUnlink",
            "AllowStreamLocalForwarding",
        ] {
            assert!(!argv.contains(forward), "{forward} in `{argv}`");
        }
    }
}
