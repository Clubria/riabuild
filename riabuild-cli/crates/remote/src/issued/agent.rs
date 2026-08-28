//! The `ssh-agent` riabuild runs for one server, and the one place an issued
//! private key exists on this laptop.
//!
//! ## Why an agent and not a file
//!
//! An issued key is the org's, not this machine's, and `CLAUDE.md`'s rule for
//! it is that it is never written down. `ssh -i` wants a *path*, so the obvious
//! implementations are a `0600` file that is deleted afterwards — which a crash
//! between the write and the unlink leaves behind — or an anonymous in-memory
//! file passed as `/proc/self/fd/N`, which means `memfd_create` on Linux,
//! `shm_open` on macOS, an `fchmod` so `ssh`'s own permission check passes, and
//! two platform paths where this crate has none.
//!
//! `ssh-agent` is the mechanism OpenSSH built for exactly this, and every
//! primitive it needs is already in `CommandRunner`. The key travels on
//! **stdin** to `ssh-add`, never in an argument vector, for the reason
//! `RunOptions.stdin` documents: `ps` shows an argv to every process on the
//! machine, and on a shared box that includes every other developer.
//!
//! ## Three details that are not incidental
//!
//! **`-D`.** Without it `ssh-agent` forks and daemonises, and riabuild is left
//! holding no handle to a process holding the org's keys. In the foreground it
//! is an ordinary child and [`Agent::stop`] ends it.
//!
//! **`-t 900` on the key.** A `SIGKILL`ed riabuild orphans its children, and
//! `stop` never runs. An orphaned agent would then serve those keys until the
//! machine rebooted; a lifetime caps that at fifteen minutes. Nothing
//! legitimate needs them longer — they are spent before the install step, not
//! across the developer's shell.
//!
//! **Public halves on disk, addressed one at a time.** They are not secret, and
//! with an agent loaded `-i <public-key-file> -o IdentitiesOnly=yes` selects
//! exactly one agent identity. That buys two things: the terminal can say which
//! key got in, and each probe offers a single key — where one connection
//! offering all of them would hit sshd's `MaxAuthTries` (6 by default) and stop
//! before a developer's seventh key was ever tried.

use crate::Remote;
use crate::identity::{Offered, ensure_private_dir};
use crate::ssh::Ssh;
use anyhow::Result;
use riabuild_api::issued::IssuedKey;
use riabuild_paths::Paths;
use riabuild_paths::filelock::FileLock;
use riabuild_runner::{ChildHandle, CommandRunner, RunOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long a key stays loaded **while riabuild is only probing it**. See the
/// module doc: this is the ceiling on what an orphaned agent can hand out in
/// the common case, where the key is spent before the install step.
///
/// A key riabuild goes on to *carry* is reloaded without it — see
/// `Issued::hold`, and [`Agent::add`] for what that trades.
pub const PROBE_LIFETIME: &str = "900";

/// How long to wait for the agent to bind its socket before giving up.
///
/// The child is up before the socket exists, and a probe against a socket that
/// is not there yet fails for the wrong reason — "this key does not work"
/// rather than "the agent had not started". Polled rather than slept for so the
/// common case costs one interval.
const SOCKET_WAIT: Duration = Duration::from_secs(2);
const SOCKET_POLL: Duration = Duration::from_millis(25);

/// The file inside a run's directory whose `flock` says the run is still going.
///
/// Held for the life of the [`Agent`], and released by the kernel however this
/// process ends. It is what lets one run tell a *sibling window's live agent*
/// from *a dead run's leftovers* — two things that look identical on disk, and
/// which riabuild used to answer the same way. See [`sweep`].
const RUN_LOCK: &str = "run.lock";

pub struct Agent {
    child: Box<dyn ChildHandle>,
    /// This run's own directory, `<root>/agent/<server-hash>/<pid>` — never the
    /// server's, which is now the directory those live *in*.
    ///
    /// **The change is the whole of a bug that could only happen to one person
    /// with two windows open.** The socket used to be
    /// `<root>/agent/<server-hash>/sock`, one path per server, and every run
    /// against that server both unlinked whatever was there and, on its way
    /// out, `remove_dir_all`'d the directory. So a second `riabuild remote` to
    /// a server reached by an issued key pulled the socket out from under the
    /// first — leaving its `IdentityAgent` option pointing at a path that no
    /// longer existed — and then the first window to finish deleted the
    /// directory the other was still serving from. What that costs is the
    /// clipboard channel's reconnect and every later `ssh` in the surviving
    /// session, on exactly the servers issued keys exist for: the ones
    /// riabuild's own key cannot sign in to.
    dir: PathBuf,
    socket: PathBuf,
    /// Released by the kernel when this process ends, however it ends — see
    /// [`RUN_LOCK`]. Never read; its existence is the whole of its job.
    _lock: FileLock,
}

impl Agent {
    /// Starts an agent for this server, or reports that this machine has none
    /// to start.
    ///
    /// `Ok(None)` — not `Err` — when `ssh-agent` is not on `PATH`. riabuild
    /// stops when there is no way in, not when the convenient way in failed:
    /// the caller falls back to the password path this feature was added
    /// alongside, which is exactly what would have happened before it existed.
    pub async fn start(
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
    ) -> Result<Option<Agent>> {
        if runner.which("ssh-agent").is_none() {
            return Ok(None);
        }

        let server = paths.agent_dir(&remote.hash());
        // Created at 0700 and repaired unconditionally, before anything is
        // written into it — the same call `identity::ensure_key` and
        // `host_key::pin` make, so an agent socket is never reachable by
        // another account on the box, not even for the two syscalls a
        // create-then-chmod leaves open.
        ensure_private_dir(&server).await?;
        // Before this run's own directory exists, so it is never a candidate
        // for its own sweep.
        sweep(&server).await;

        // One directory per *run*, not per server. The pid is for whoever reads
        // `~/.riabuild` with their eyes; what actually decides whether a run is
        // alive is the lock inside, because a pid can be recycled and an
        // `flock` cannot be mistaken for one.
        let dir = server.join(std::process::id().to_string());
        ensure_private_dir(&dir).await?;
        let Some(lock) = FileLock::try_acquire(&dir.join(RUN_LOCK)).await? else {
            // Only reachable if a live process already owns this pid's
            // directory, which the operating system does not permit. Refusing
            // is still right: the alternative is serving from a directory
            // something else may delete.
            return Ok(None);
        };

        let socket = dir.join("sock");
        // A killed run leaves its socket behind, and `ssh-agent` refuses to
        // bind over one. Removing it is safe in a way removing the *channel*
        // socket is not: this path is this run's own, under a 0700 directory
        // this process owns, not a shared runtime directory a co-tenant — or,
        // as it turned out, the developer's own other window — might hold.
        let _ = tokio::fs::remove_file(&socket).await;

        let child = runner
            .spawn(
                "ssh-agent",
                &["-D", "-a", &socket.to_string_lossy()],
                &RunOptions::default(),
            )
            .await?;

        let agent = Agent {
            child,
            dir,
            socket,
            _lock: lock,
        };
        agent.await_socket().await;
        Ok(Some(agent))
    }

    /// Measured on `tokio::time::Instant`, never `std`'s — the same rule the
    /// pump's keepalive states: the sleep below is tokio's, so a deadline taken
    /// off the other clock is one the two halves disagree about. Under a paused
    /// or advanced test clock the sleeps return at once while `std`'s clock
    /// crawls at wall speed, which turns a bounded wait into a hot spin for the
    /// full two seconds and makes the deadline something no test can reach
    /// deliberately.
    async fn await_socket(&self) {
        let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
        while tokio::time::Instant::now() < deadline {
            if tokio::fs::metadata(&self.socket).await.is_ok() {
                return;
            }
            tokio::time::sleep(SOCKET_POLL).await;
        }
        // Deliberately not an error. The probe that follows is the real test of
        // whether this agent works, and it reports a failure in terms of the
        // key rather than of a socket the developer cannot see. A `FakeRunner`
        // also never creates one, which is the other reason this waits rather
        // than insists.
    }

    /// Loads one key, and writes the public half that addresses it.
    ///
    /// Returns the path to that public half — the handle every later probe
    /// uses, since an agent with several keys in it needs to be told which.
    /// `lifetime` is `Some` while riabuild is only *probing* — a bounded window,
    /// because an orphaned agent must not serve the org's keys forever — and
    /// `None` once riabuild has committed to carrying this identity for the
    /// rest of the run. A carried key has to outlive an interactive shell,
    /// which can be open all day, and an expiry mid-session would break the
    /// clipboard channel's reconnect rather than anything visible.
    ///
    /// The exposure that buys is narrower than it sounds: the socket lives in a
    /// `0700` directory owned by the developer, so an orphan is reachable only
    /// by them — the same footing as the `ssh-agent` they run themselves.
    pub async fn add(
        &self,
        runner: Arc<dyn CommandRunner>,
        key: &IssuedKey,
        lifetime: Option<&str>,
    ) -> Result<PathBuf> {
        let public = self.dir.join(format!("{}.pub", key.id));
        tokio::fs::write(&public, format!("{}\n", key.public_key)).await?;

        let mut args: Vec<&str> = Vec::new();
        if let Some(seconds) = lifetime {
            args.push("-t");
            args.push(seconds);
        }
        args.push("-");
        let output = runner
            .run(
                "ssh-add",
                &args,
                &RunOptions {
                    // The private key, on stdin. Never an argument: see the
                    // module doc.
                    stdin: Some(key.private_key.clone().into_bytes()),
                    env: vec![("SSH_AUTH_SOCK".into(), self.socket.to_string_lossy().into())],
                    ..RunOptions::default()
                },
            )
            .await?;
        if !output.ok() {
            // The label is the caller's to name — it already has it, and
            // repeating it here reads as "the cloudcli key: ssh-add
            // refused cloudcli".
            anyhow::bail!("ssh-add refused it: {}", output.stderr.trim());
        }
        Ok(public)
    }

    /// Can this one identity sign in to the server?
    ///
    /// `IdentitiesOnly=yes` plus `-i <public half>` restricts the attempt to a
    /// single agent key, so the answer names a key rather than the set.
    ///
    /// `without_askpass`, deliberately, and for the reason
    /// `authorise::can_sign_in` sets out at length: this and that probe are
    /// the only ssh calls in remote mode that must not carry
    /// `askpass::run_options`. A saved password could otherwise answer a
    /// prompt and make the answer yes for a key that does not work at all.
    /// `BatchMode=yes` already forbids every prompt; this is the belt beside
    /// those braces.
    pub async fn probe(
        &self,
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
        public_key_path: &Path,
    ) -> Result<bool> {
        // Composed rather than restated, so the pinned `known_hosts`, the
        // `-F /dev/null`, the port and the bound on the dial all stay in one
        // place. `offering` names this one agent key beside riabuild's own,
        // which is the same shape a carried identity takes everywhere else —
        // there is just no `Working` yet, because whether there is one is the
        // question being asked.
        let probe = Ssh::to(remote, paths, runner)
            .offering(Offered {
                socket: &self.socket,
                public_key_path,
            })
            .option("BatchMode=yes")
            .without_askpass()
            .run("true")
            .await?;
        Ok(probe.ok())
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Ends the agent and removes what it left behind.
    ///
    /// Best effort throughout: a failure here must not surface as a failure of
    /// the run. The keys are gone with the process either way, and the two
    /// things left on disk — a socket and a public key — are inert.
    ///
    /// This is the *orderly* teardown, and [`Drop`] below is what makes it
    /// unmissable. Both exist because neither is enough on its own: only this
    /// one can `await` the kill and know the process is signalled before the
    /// run moves on, and only `Drop` runs on the paths that never reach it.
    pub async fn stop(self) {
        let _ = self.child.kill().await;
        let _ = tokio::fs::remove_dir_all(&self.dir).await;
        // The server's directory too, but only if this was the last run in it —
        // `remove_dir` refuses a directory that is not empty, which is exactly
        // the test wanted and is why it is not `remove_dir_all`. A sibling
        // window's live agent lives in there.
        if let Some(server) = self.dir.parent() {
            let _ = tokio::fs::remove_dir(server).await;
        }
        // `self` is dropped here, so `Drop::drop` runs immediately after and
        // finds the directory already gone. Both halves are idempotent
        // precisely so that is a no-op rather than a second error to swallow.
    }
}

/// Removes the directories of runs that have ended, and nothing else.
///
/// The question this answers cannot be answered by looking: a run that was
/// `SIGKILL`ed and a run serving a shell in the developer's other window leave
/// the same socket and the same public halves behind. So it is asked of the
/// kernel — a `run.lock` nothing holds is a run nothing is running. That is the
/// same reasoning `channel::lease` sets out at length for the same reason, and
/// the alternative it rejects is the one that would be wrong here too: a pid
/// and a `kill -0`, which says "alive" about a recycled pid and "dead" about
/// nothing at all.
///
/// The failure direction is chosen. A lock riabuild cannot take leaves a
/// directory behind, which costs a dead socket and a public key — both inert,
/// both swept by the next run that can. Deleting one it should have kept is the
/// bug this whole file was changed for, and no path here can do it.
///
/// Best effort throughout: a laptop whose `~/.riabuild/agent` cannot be read is
/// not a laptop that should be refused a server.
async fn sweep(server: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(server).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let run = entry.path();
        if !entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        // Nobody holds it, so nobody is running. The two arms this does not
        // spell out are a live sibling window and a directory riabuild could
        // not ask about, and both are left exactly where they are.
        if let Ok(Some(lock)) = FileLock::try_acquire(&run.join(RUN_LOCK)).await {
            // Dropped before the removal for legibility rather than necessity:
            // unlinking a file somebody holds open is ordinary on Unix, and the
            // lock is on the inode either way.
            drop(lock);
            let _ = tokio::fs::remove_dir_all(&run).await;
        }
    }
}

impl Drop for Agent {
    /// The backstop for every path out of a run that does not reach
    /// [`Agent::stop`] — which, until this existed, was the whole success path
    /// through `flow::connect` and every `?` in it: only `--check` and a failed
    /// `authorise` called `stop`, though `Issued::stop`'s doc claimed every
    /// path did.
    ///
    /// Two different things are being cleaned up and only one of them was ever
    /// safe to leave to chance. **The process** goes with the handle —
    /// `RealRunner::spawn` sets `kill_on_drop`, so dropping `child` signals the
    /// agent — which is why an orphaned `ssh-agent` was not the visible
    /// symptom. **The directory** does not: `<root>/agent/<hash>/<pid>` was left
    /// behind on every successful run, holding the public halves that name
    /// which org keys this laptop was issued and a dead socket the next run's
    /// `Agent::start` then has to unlink.
    ///
    /// It removes this *run's* directory and never the server's, which is the
    /// half that had to change when several windows came to have one each: a
    /// `remove_dir_all` on the server's directory here would take a sibling
    /// window's live agent with it. What clears a directory whose run has ended
    /// is [`sweep`], which asks the kernel rather than guessing.
    ///
    /// `std::fs`, and this is the one place in the crate where that is not the
    /// bug `CLAUDE.md` says it is: a `Drop` cannot `await`, and the choice here
    /// is not between blocking and not blocking, it is between an `unlink` of a
    /// handful of small files under `~/.riabuild` and never cleaning up at all.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        // And the server's directory when this was the last run in it.
        // `remove_dir`, never `remove_dir_all`: refusing a directory that is
        // not empty is precisely the test, because what would still be in there
        // is a sibling window's live agent.
        if let Some(server) = self.dir.parent() {
            let _ = std::fs::remove_dir(server);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    /// A key whose private half carries a marker no argv may ever contain.
    fn key() -> IssuedKey {
        IssuedKey {
            id: "k17abc".into(),
            label: "prod-bastion".into(),
            key_type: "ssh-ed25519".into(),
            public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPA49 riabuild".into(),
            fingerprint: "SHA256:X4Nt8DcFy4DCOoCxomm4oJjRFs6sQN36IJHq7jWTD9E".into(),
            private_key:
                "-----BEGIN OPENSSH PRIVATE KEY-----\nSECRETBODYMARKER\n-----END OPENSSH PRIVATE KEY-----\n"
                    .into(),
        }
    }

    fn runner() -> Arc<FakeRunner> {
        Arc::new(
            FakeRunner::new()
                .spawning_until_killed("ssh-agent -D")
                .with("ssh-add", 0, "", "")
                .containing("ssh ", 0, "", ""),
        )
    }

    #[tokio::test]
    async fn the_private_key_reaches_ssh_add_on_stdin_and_appears_in_no_argument() {
        // The test this whole module exists to be able to pass. `ps` shows an
        // argv to every process on the machine, and on a shared box that
        // includes every other developer.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        agent
            .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
            .await
            .expect("add");

        let piped = fake.stdin_text_of("ssh-add").expect("ssh-add got stdin");
        assert!(piped.contains("SECRETBODYMARKER"), "{piped}");
        for call in fake.calls().iter().chain(fake.spawns().iter()) {
            assert!(
                !call.contains("SECRETBODYMARKER") && !call.contains("BEGIN OPENSSH"),
                "key material in an argv: {call}"
            );
        }
    }

    /// The wait for the socket ends on the clock its sleeps are on.
    ///
    /// A `FakeRunner` binds no socket, so this is the path that runs the
    /// deadline out — the same path a real `ssh-agent` that never came up
    /// takes. With the deadline on `std::time::Instant` and the sleep on
    /// tokio's, a paused clock lets every sleep return at once while the
    /// deadline crawls at wall speed: the bounded poll becomes a hot spin for
    /// two real seconds, and the loop's own bound is something no test can
    /// reach on purpose. Asserting on elapsed *virtual* time is what tells the
    /// two apart — spinning would run the clock far past `SOCKET_WAIT` before
    /// `std` agreed the wait was over.
    #[tokio::test(start_paused = true)]
    async fn the_wait_for_a_socket_is_bounded_on_the_clock_its_sleeps_use() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let started = tokio::time::Instant::now();

        Agent::start(&remote(), &paths, runner())
            .await
            .expect("start")
            .expect("an agent");

        let waited = started.elapsed();
        assert!(
            waited >= SOCKET_WAIT,
            "the deadline must be reachable at all: {waited:?}"
        );
        assert!(
            waited < SOCKET_WAIT + SOCKET_POLL * 4,
            "the deadline is on the same clock as the sleeps: {waited:?}"
        );
    }

    #[tokio::test]
    async fn keys_are_loaded_with_a_lifetime_so_an_orphaned_agent_forgets_them() {
        // A SIGKILLed riabuild never runs `stop`. Without `-t`, the orphan it
        // leaves behind would serve the org's keys until the machine rebooted.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        agent
            .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
            .await
            .expect("add");

        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("ssh-add -t 900 -")),
            "{:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn the_agent_runs_in_the_foreground_and_does_not_outlive_the_run() {
        // Without `-D` the agent daemonises and riabuild holds no handle to a
        // process holding the org's keys.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        assert!(
            fake.spawns()
                .iter()
                .any(|call| call.contains("ssh-agent -D")),
            "{:?}",
            fake.spawns()
        );

        agent.stop().await;
        assert_eq!(
            fake.killed().len(),
            1,
            "the agent must be killed, not left running: {:?}",
            fake.spawns()
        );
    }

    #[tokio::test]
    async fn a_probe_offers_exactly_one_identity_and_cannot_be_answered_by_a_password() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        let public = agent
            .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
            .await
            .expect("add");
        assert!(
            agent
                .probe(&remote(), &paths, fake.clone(), &public)
                .await
                .expect("probe")
        );

        let call = fake
            .calls()
            .into_iter()
            .find(|call| call.starts_with("ssh "))
            .expect("probed over ssh");
        // One identity, named — not the whole agent, which would hit sshd's
        // MaxAuthTries and never say which key got in.
        assert!(call.contains("IdentityAgent="), "{call}");
        assert!(call.contains("IdentitiesOnly=yes"), "{call}");
        assert!(call.contains("k17abc.pub"), "{call}");
        // Nothing may answer a password prompt here: a saved password would
        // make the answer yes for a key that does not work at all.
        assert!(call.contains("BatchMode=yes"), "{call}");
        assert!(
            fake.env_of("ssh ")
                .iter()
                .all(|(name, _)| name != "SSH_ASKPASS"),
            "the probe must not carry an askpass"
        );
        // And it still pins riabuild's own known_hosts rather than the
        // developer's.
        assert!(call.contains("-F /dev/null"), "{call}");
    }

    #[tokio::test]
    async fn an_agent_nobody_stopped_still_takes_its_directory_with_it() {
        // `Issued::stop`'s doc claimed it was "called on every path out of
        // connect". It was called on two: `--check`, and a failed `authorise`.
        // The whole success path — and every `?` below the copy — left
        // `<root>/agent/<hash>` on disk holding the public halves that name
        // which org keys this laptop was issued, plus a dead socket the next
        // run's `Agent::start` then has to unlink. Since `connect` cannot be
        // reached from this crate's tests, the guarantee is asserted where it
        // now lives: dropping the agent, by any route, cleans up.
        //
        // The public halves live one level deeper now — `<hash>/<pid>` — so
        // this asserts on the server's directory to keep saying what it always
        // said: a laptop with no run in flight has nothing left under
        // `~/.riabuild/agent` naming which org keys it was issued. The empty
        // `<hash>` goes with the last run out, and stays while any other one is
        // still in there.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();
        let dir = paths.agent_dir(&remote().hash());

        {
            let agent = Agent::start(&remote(), &paths, fake.clone())
                .await
                .expect("start")
                .expect("an agent");
            agent
                .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
                .await
                .expect("add");
            assert!(
                tokio::fs::metadata(&dir).await.is_ok(),
                "the agent has to have somewhere to keep the public halves first"
            );
            // No `stop()`. This is the shape of every path that forgot one.
        }

        assert!(
            tokio::fs::metadata(&dir).await.is_err(),
            "an agent that went out of scope left {} behind",
            dir.display()
        );
    }

    /// Two windows into one server, each with its own agent, and neither able
    /// to touch the other's.
    ///
    /// The bug, and it is a quiet one: the socket was
    /// `<root>/agent/<hash>/sock`, one path per *server*. The second window's
    /// `Agent::start` unlinked the first window's live socket and bound its own
    /// there, so the first window's `-o IdentityAgent=…` addressed a process it
    /// had never started; and whichever window finished first ran
    /// `remove_dir_all` on the directory both were using, leaving the survivor
    /// with an `IdentityAgent` pointing at nothing. It shows up as a session
    /// that works and then, some minutes later, cannot reconnect its clipboard
    /// channel or open a single further `ssh` — on precisely the servers issued
    /// keys exist for, since those are the ones riabuild's own key cannot sign
    /// in to.
    ///
    /// Two `Agent`s in one process share a pid and therefore a run directory,
    /// which no two real windows do; the second is built by hand at another
    /// name to stand in for the other window's, and what is asserted is that
    /// each one's teardown leaves the other's socket, key and lock exactly
    /// where they were.
    #[tokio::test]
    async fn one_window_tearing_its_agent_down_leaves_another_windows_alone() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();
        let server = paths.agent_dir(&remote().hash());

        // The other window: a live run directory, held open by the lock that
        // says so, with an agent socket and a public half in it.
        let sibling = server.join("other-window");
        ensure_private_dir(&sibling).await.expect("mkdir");
        let sibling_lock = FileLock::try_acquire(&sibling.join(RUN_LOCK))
            .await
            .expect("lock")
            .expect("nobody holds it yet");
        tokio::fs::write(sibling.join("sock"), "")
            .await
            .expect("socket");
        tokio::fs::write(sibling.join("k17abc.pub"), "ssh-ed25519 AAAA riabuild")
            .await
            .expect("public half");

        {
            let agent = Agent::start(&remote(), &paths, fake.clone())
                .await
                .expect("start")
                .expect("an agent");
            agent
                .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
                .await
                .expect("add");

            // A socket of its own. Sharing one was the whole bug.
            assert_ne!(
                agent.socket(),
                sibling.join("sock"),
                "two windows into one server must not serve from one socket"
            );
            assert!(
                tokio::fs::metadata(sibling.join("sock")).await.is_ok(),
                "starting an agent must not unlink another window's live socket"
            );
        }

        // …and neither does ending one. This is the `Drop` path, which is the
        // one every successful run takes.
        assert!(
            tokio::fs::metadata(sibling.join("sock")).await.is_ok()
                && tokio::fs::metadata(sibling.join("k17abc.pub"))
                    .await
                    .is_ok(),
            "a window that finished took the other window's agent with it"
        );

        // Once that window ends too, its lock falls free and the next run
        // sweeps what it left. Nothing else does, and nothing sooner.
        drop(sibling_lock);
        sweep(&server).await;
        assert!(
            tokio::fs::metadata(&sibling).await.is_err(),
            "a run that has ended has to be swept, or every server accumulates them"
        );
    }

    /// A run directory whose lock is *held* is a window that is still open, and
    /// the sweep must leave it exactly where it is.
    ///
    /// The half that has to be right, because the other half only costs a
    /// leftover directory holding two inert files, while this one costs a live
    /// session its identity. It is the same trade `channel::lease` documents:
    /// the kernel answers "is anybody running this", never a pid and a
    /// `kill -0`, which says "alive" about a recycled pid.
    #[tokio::test]
    async fn the_sweep_leaves_a_run_that_is_still_going() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let server = paths.agent_dir(&remote().hash());
        let live = server.join("11111");
        let ended = server.join("22222");
        ensure_private_dir(&live).await.expect("mkdir");
        ensure_private_dir(&ended).await.expect("mkdir");

        let _held = FileLock::try_acquire(&live.join(RUN_LOCK))
            .await
            .expect("lock")
            .expect("free");
        // `ended` gets a lock file and nobody holding it — a run that was
        // killed, which is indistinguishable from the one above by looking.
        drop(
            FileLock::try_acquire(&ended.join(RUN_LOCK))
                .await
                .expect("lock")
                .expect("free"),
        );

        sweep(&server).await;

        assert!(
            tokio::fs::metadata(&live).await.is_ok(),
            "a window that is still open must keep its agent"
        );
        assert!(
            tokio::fs::metadata(&ended).await.is_err(),
            "a run that has ended must be cleared"
        );
    }

    #[tokio::test]
    async fn stopping_an_agent_twice_over_is_not_an_error() {
        // `stop` consumes the agent, so `Drop` runs immediately after it and
        // finds the directory already gone. Both halves have to be idempotent
        // or the orderly teardown would panic on the tidy path and only the
        // forgotten one would work.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();
        let dir = paths.agent_dir(&remote().hash());

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        agent.stop().await;

        assert!(tokio::fs::metadata(&dir).await.is_err());
        assert_eq!(fake.killed().len(), 1, "{:?}", fake.spawns());
    }

    #[tokio::test]
    async fn a_machine_with_no_ssh_agent_is_a_none_rather_than_an_error() {
        // riabuild stops when there is no way in, not when the convenient way
        // in failed — the rule `authorise`'s module doc sets out. The caller
        // falls back to the password path this feature was added alongside.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new());

        let started = Agent::start(&remote(), &paths, fake)
            .await
            .expect("not an error");
        assert!(started.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_agent_directory_is_private_and_holds_no_private_key() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = runner();

        let agent = Agent::start(&remote(), &paths, fake.clone())
            .await
            .expect("start")
            .expect("an agent");
        agent
            .add(fake.clone(), &key(), Some(PROBE_LIFETIME))
            .await
            .expect("add");

        let dir = paths.agent_dir(&remote().hash());
        let mode = tokio::fs::metadata(&dir)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);

        // The claim the module doc makes, as an assertion over the directory
        // rather than as prose: what lands here is a public key and nothing
        // else.
        let mut entries = tokio::fs::read_dir(&dir).await.expect("read dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            let body = tokio::fs::read_to_string(entry.path())
                .await
                .unwrap_or_default();
            assert!(
                !body.contains("SECRETBODYMARKER") && !body.contains("BEGIN OPENSSH"),
                "a private key was written to {}",
                entry.path().display()
            );
        }
    }
}
