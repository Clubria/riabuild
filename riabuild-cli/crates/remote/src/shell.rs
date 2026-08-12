//! Opening the environment on a server: mosh when it can, ssh when it cannot.

use super::{Remote, askpass, identity};
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
pub async fn run_setup(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    command: &str,
) -> Result<i32> {
    let mut args = vec!["-t".to_string()];
    args.extend(identity::ssh_options(remote, paths, true));
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner
        .run_interactive("ssh", &refs, &askpass::run_options(remote, paths))
        .await
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
            "/bin/sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        // mosh execs `ssh` to bootstrap the session, and that `ssh` inherits
        // this environment — so a server reached by password is reached by the
        // saved one here too, rather than prompting where mosh has already
        // taken over the terminal.
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

    let mut args = vec!["-t".to_string()];
    args.extend(identity::ssh_options(remote, paths, true));
    args.push("-o".to_string());
    args.push("ServerAliveInterval=20".to_string());
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
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

    #[tokio::test]
    async fn setup_always_uses_plain_ssh_never_mosh() {
        // mosh does not propagate the remote command's exit status, so a failed
        // setup would look like a success and the flow would open a shell on a
        // broken box.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", ""));

        let code = run_setup(&remote(), &paths, fake.clone(), "riabuild --no-shell")
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
