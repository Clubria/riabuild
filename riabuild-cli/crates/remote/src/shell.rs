//! Opening the environment on a server: mosh when it can, ssh when it cannot.
//!
//! Both handoffs are spaced by `Ui::blank`, and the spacing is this side's job
//! rather than the server's. `ssh` prints `Connection to … closed.` the instant
//! the remote command ends and `mosh` prints `[mosh is exiting.]` when it lets
//! the terminal go, both without a blank line of their own — so a laptop that
//! printed nothing wedges those between its own lines, and a server that
//! printed one of its own puts it at the top of a fresh mosh screen where
//! there is nothing above it to separate from.

use super::{NO_TMUX, Remote, askpass, identity};
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::sync::Arc;

async fn has_mosh_server(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
) -> bool {
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push("command -v mosh-server".to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner
        .run("ssh", &refs, &askpass::run_options(remote, paths))
        .await
        .map(|output| output.ok())
        .unwrap_or(false)
}

/// mosh exits 5 when it could not establish a session at all — a blocked UDP
/// port, typically. Any other code is the command's own.
const MOSH_NO_SESSION: i32 = 5;

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
) -> Result<i32> {
    let mut args = vec!["-t".to_string()];
    args.extend(identity::ssh_options(remote, paths, true));
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = runner
        .run_interactive("ssh", &refs, &askpass::run_options(remote, paths))
        .await;
    ui.blank();
    code
}

pub async fn open(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    command: &str,
) -> Result<i32> {
    let local_mosh = runner.which("mosh").is_some();
    if local_mosh && has_mosh_server(remote, paths, &runner).await {
        let ssh = format!(
            "ssh {}",
            identity::ssh_options(remote, paths, true).join(" ")
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
        // Only mosh's own "could not connect" exit falls back. Treating *any*
        // non-zero code as a connection failure would silently reconnect a
        // developer who exited their shell with a non-zero status — and, on the
        // setup path, would run the whole provisioning a second time.
        if code != MOSH_NO_SESSION {
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
    let mut args = vec!["-t".to_string()];
    args.extend(identity::ssh_options(remote, paths, true));
    args.push("-o".to_string());
    args.push("ServerAliveInterval=20".to_string());
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // The same gap mosh gets above, and here it separates the session from the
    // note or warning immediately in front of it as well.
    ui.blank();
    runner
        .run_interactive("ssh", &refs, &askpass::run_options(remote, paths))
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

    #[tokio::test]
    async fn mosh_is_used_when_the_server_has_it() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "/usr/bin/mosh-server\n", "")
                .with("mosh", 0, "", ""),
        );
        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
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
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "/usr/bin/mosh-server\n", "")
                .with("mosh", 0, "", ""),
        );

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "env 'RIABUILD_ROOT=/home/dev/.riabuild-remote/abc' riabuild shell",
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
        let fake = Arc::new(FakeRunner::new().with("mosh", 0, "", "").containing(
            "command -v mosh-server",
            1,
            "",
            "not found",
        ));

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
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

    #[tokio::test]
    async fn no_mosh_on_the_laptop_falls_back_to_ssh_and_says_so() {
        // Distinct from the server-side gap above: here the laptop itself has no
        // mosh binary at all, so `has_mosh_server` must never even be asked.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new());

        open(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild shell",
        )
        .await
        .expect("falls back");

        assert!(
            !fake.calls().iter().any(|call| call.contains("mosh-server")),
            "the laptop has no mosh, so the server was never asked: {:?}",
            fake.calls()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh") && call.contains("-t")),
            "{:?}",
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

        let with_mosh = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "/usr/bin/mosh-server\n", "")
                .with("mosh", 0, "", ""),
        );
        let ui = Ui::new(false);
        open(&remote(), &paths, with_mosh, &ui, "riabuild shell")
            .await
            .expect("opens");
        assert_eq!(ui.blanks(), 1, "mosh");

        // No mosh on the laptop: a note is printed first, and the gap belongs
        // under it rather than over it.
        let without_mosh = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));
        let ui = Ui::new(false);
        open(&remote(), &paths, without_mosh, &ui, "riabuild shell")
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

        run_setup(&remote(), &paths, fake, &ui, "riabuild --no-shell")
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
