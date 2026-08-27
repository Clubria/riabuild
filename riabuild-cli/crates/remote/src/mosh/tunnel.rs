//! The laptop's half: a mosh session whose datagrams travel over the ssh
//! connection riabuild was making anyway.
//!
//! This is `mosh` the perl wrapper, done by hand, because the wrapper has no
//! way to be told where to send its datagrams — it runs `mosh-client` against
//! the address it just ssh'd to, and that address is exactly the one the
//! network is dropping. So riabuild does what the wrapper does (bootstrap
//! `mosh-server` over ssh, read `MOSH CONNECT <port> <key>`, hand both to
//! `mosh-client`) and changes the one thing that has to change: `mosh-server`
//! binds `127.0.0.1` on the server, `mosh-client` is pointed at `127.0.0.1` on
//! the laptop, and the two loopbacks are joined by `udp-over-tcp` over a
//! second ssh's stdio.
//!
//! ```text
//!   mosh-client ─udp→ udp2tcp ─tcp→ ┐
//!                                   │  ssh stdio  (the only thing on the wire)
//!   mosh-server ←udp─ tcp2udp ←tcp─ ┘
//! ```
//!
//! Every failure here returns `None` rather than an error, and `None` means
//! "open an ordinary ssh session instead". A developer on a network that blocks
//! UDP has already lost mosh; losing the shell as well because the workaround
//! did not come up would be riabuild making that worse.

use super::{TUNNEL_READY_LINE, loopback, pump, read_line, tcp_options};
use crate::{NO_TMUX, Remote, env_command, shell_command, ssh::Ssh};
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, PipedChildHandle, RunOptions};
use riabuild_ui::Ui;
use std::sync::Arc;
use tokio::net::TcpListener;

/// The locale variables mosh carries from the laptop to the server.
///
/// The same three the perl wrapper sends, and for its reason: `mosh-server`
/// refuses to start unless the session's character set is UTF-8, and what
/// decides that is the account's environment on the *server*, which is not the
/// developer's laptop. Sending the laptop's answer is what makes a session on a
/// minimal box render anything but mojibake.
const LOCALE_VARS: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

/// The locale riabuild names when the laptop's own does not say UTF-8.
///
/// `mosh-server` exits rather than starting under a non-UTF-8 locale, and a
/// laptop with `LANG` unset — a cron, a CI runner, a stripped login — would
/// otherwise send nothing and be refused. `C.UTF-8` is present on every distro
/// riabuild provisions and asserts only the character set, which is the single
/// thing `mosh-server` is asking about.
const FALLBACK_LOCALE: &str = "C.UTF-8";

/// What `mosh-server` prints when it is up, followed by the port and the key.
const CONNECT_LINE: &str = "MOSH CONNECT";

/// What riabuild tells `mosh-client` to do about local echo.
///
/// `adaptive` is `mosh`'s own default, and it is the whole reason a developer
/// notices mosh at all: it echoes a keystroke immediately once it has measured
/// the round trip. Named rather than left unset because `mosh-client` reads it
/// from the environment and the wrapper riabuild is standing in for is what
/// normally sets it.
const PREDICTION: &str = "adaptive";

/// Opens the session, tunnelled. `None` means it could not be, and the caller
/// should fall back to `ssh`.
///
/// `binary` is the server's own riabuild with its environment prefix already
/// on it — the same string every other remote invocation uses — because the
/// far end of the tunnel is riabuild rather than a second tool to install.
pub(crate) async fn open(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    command: &str,
    binary: &str,
    carry: Option<&crate::issued::Working>,
) -> Option<i32> {
    // `mosh` the wrapper is a perl script riabuild is deliberately not running;
    // what it needs is `mosh-client`, which a mosh install always carries.
    runner.which("mosh-client")?;

    let session = bootstrap(remote, paths, &runner, command, carry).await?;

    // Held for the length of the session: this ssh *is* the tunnel.
    let child = Ssh::to(remote, paths, runner.clone())
        .carry(carry)
        .spawn_piped(&format!(
            "{binary} internal {} {}",
            super::TCP2UDP,
            session.port
        ))
        .await
        .ok()?;
    let stdio = match tunnel_stdio(child.as_ref()) {
        Some(halves) => halves,
        None => {
            let _ = child.kill().await;
            return None;
        }
    };
    let joined = match join(stdio.0, stdio.1).await {
        Some(joined) => joined,
        None => {
            let _ = child.kill().await;
            return None;
        }
    };
    let local = joined.port;

    warn(ui, remote);
    ui.blank();
    let code = runner
        .run_interactive(
            "mosh-client",
            &["127.0.0.1", &local.to_string()],
            &RunOptions {
                env: vec![
                    ("MOSH_KEY".to_string(), session.key),
                    (
                        "MOSH_PREDICTION_DISPLAY".to_string(),
                        PREDICTION.to_string(),
                    ),
                ],
                ..RunOptions::default()
            },
        )
        .await;

    // The order matters only in that both happen: the ssh child holds the
    // server's `tcp2udp`, and a task left running holds a half of the stdio it
    // would otherwise have closed.
    joined.stop();
    let _ = child.kill().await;

    // Any non-zero code is the tunnel's failure rather than the developer's,
    // for the reason `shell::open` gives about the direct path: mosh does not
    // propagate the remote command's exit status, so there is no status here to
    // mistake for one.
    match code {
        Ok(0) => Some(0),
        _ => None,
    }
}

/// The laptop's end of a tunnel whose far end has said it is ready.
pub(super) struct Joined {
    /// The loopback UDP port `mosh-client` is pointed at.
    pub(super) port: u16,
    /// Both run for the length of the session: one carries the ssh stdio to
    /// and from the local `TcpStream`, the other is `Udp2Tcp` itself.
    tasks: [tokio::task::JoinHandle<()>; 2],
}

impl Joined {
    /// Ends both directions. A task left running holds a half of the stdio it
    /// would otherwise have closed.
    pub(super) fn stop(self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Joins a far end's stdio to a local UDP socket `mosh-client` can be pointed
/// at, once that far end has announced itself.
///
/// Takes the stream rather than the ssh child so that the whole of the laptop's
/// side can be driven by a test against the real server side in
/// `serve::serve` — see the end-to-end test in `mosh.rs`. Everything above this
/// needs a server, an account and a `mosh-server`; none of what is below does,
/// and this is the part that was wrong.
pub(super) async fn join<R, W>(mut incoming: R, mut outgoing: W) -> Option<Joined>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    if tokio::time::timeout(super::HANDSHAKE, read_line(&mut incoming))
        .await
        .ok()?
        .ok()?
        .trim()
        != TUNNEL_READY_LINE
    {
        return None;
    }

    // Bound before `Udp2Tcp` is built, because `Udp2Tcp` connects to this
    // address rather than being handed a socket, and a listener that is not up
    // yet is a connection refused rather than a retry.
    let listener = TcpListener::bind(loopback(0)).await.ok()?;
    let joining = listener.local_addr().ok()?;
    let udp2tcp = udp_over_tcp::Udp2Tcp::new(loopback(0), joining, tcp_options())
        .await
        .ok()?;
    let port = udp2tcp.local_udp_addr().ok()?.port();

    // `Udp2Tcp` does not connect until the first datagram arrives, so the
    // accept below waits until `mosh-client` speaks — which is after the
    // handoff.
    let pumping = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (from_server, to_server) = stream.into_split();
        tokio::select! {
            _ = pump(&mut incoming, to_server) => {}
            _ = pump(from_server, &mut outgoing) => {}
        }
    });
    let forwarding = tokio::spawn(async move {
        let _ = udp2tcp.run().await;
    });

    Some(Joined {
        port,
        tasks: [pumping, forwarding],
    })
}

/// What `mosh-server` answered with.
struct Session {
    port: u16,
    key: String,
}

/// Starts `mosh-server` on the far side and reads the one line it prints.
///
/// `-i 127.0.0.1` is the whole difference from what `mosh` does for itself.
/// The server's UDP socket is then reachable only from the server, which is
/// what makes this need nothing from a cloud firewall — and is also why it is
/// useless without the tunnel that follows.
///
/// A capture rather than a held child: `mosh-server new` daemonises, closes its
/// stdout and exits 0, so the connection is over by the time this returns and
/// the session it left behind belongs to nobody. `mosh-server` gives up on a
/// client that never arrives, so a tunnel that fails after this point leaves
/// nothing on the server for longer than a minute.
async fn bootstrap(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
    command: &str,
    carry: Option<&crate::issued::Working>,
) -> Option<Session> {
    // `env … /bin/sh -lc <command>`, exactly as the direct mosh path builds it
    // and for the same reason: `-l` reads the account's profile, which on a
    // cloudcli box is where the tmux `exec` lives, so the variable that stops
    // it has to be set on the outside of that shell.
    let inner = env_command(&[NO_TMUX], "/bin/sh", &["-lc", command]);
    let mut script = String::from("exec mosh-server new -i 127.0.0.1");
    if let Some(colours) = colours(runner).await {
        script.push_str(&format!(" -c {colours}"));
    }
    for locale in locales() {
        script.push_str(&format!(" -l {locale}"));
    }
    script.push_str(&format!(" -- {inner}"));

    let output = Ssh::to(remote, paths, runner.clone())
        .carry(carry)
        .run(&shell_command(&script))
        .await
        .ok()?;
    connect_line(&output.stdout)
}

/// The port and key out of `MOSH CONNECT <port> <key>`.
///
/// Split out from the ssh around it so the parse is testable without a server,
/// and written to ignore everything else on stdout: a login banner, an MOTD and
/// a `stty` warning all arrive on the same stream.
fn connect_line(stdout: &str) -> Option<Session> {
    let line = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(CONNECT_LINE))?;
    let mut parts = line.split_whitespace();
    let port = parts.next()?.parse().ok()?;
    let key = parts.next()?.to_string();
    Some(Session { port, key })
}

/// The locale assignments passed to `mosh-server` as `-l VAR=VALUE`.
///
/// Returns the laptop's own answers where it has them, and adds `LANG` where
/// nothing it is sending names UTF-8 — `mosh-server` exits rather than starting
/// otherwise, and the message it prints is about the *server's* locale, which
/// sends a developer to change the wrong machine.
fn locales() -> Vec<String> {
    locales_from(|key| std::env::var(key).ok())
}

/// [`locales`] with the environment as a parameter, so the fallback is
/// testable without changing the process's own.
fn locales_from(read: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut sending: Vec<String> = Vec::new();
    let mut utf8 = false;
    for key in LOCALE_VARS {
        let Some(value) = read(key) else { continue };
        if value.to_ascii_lowercase().replace('-', "").contains("utf8") {
            utf8 = true;
        }
        sending.push(format!("{key}={value}"));
    }
    if !utf8 {
        sending.push(format!("LANG={FALLBACK_LOCALE}"));
    }
    sending
}

/// How many colours this terminal has, as `mosh-client` reports it.
///
/// Asked of `mosh-client` rather than of `TERM` here, because `mosh-client -c`
/// is the same question the perl wrapper asks and it answers from the terminfo
/// database this machine actually has. `None` where it cannot be asked, which
/// leaves `mosh-server` on its own default rather than on a guess.
async fn colours(runner: &Arc<dyn CommandRunner>) -> Option<u32> {
    let output = runner
        .run("mosh-client", &["-c"], &RunOptions::default())
        .await
        .ok()?;
    output.stdout.trim().parse().ok()
}

/// Both halves of the ssh child's stdio, or nothing.
///
/// Taken together because one without the other is a tunnel that carries
/// keystrokes in exactly one direction, which is a session that paints once and
/// then hangs — worse to debug than one that never opened.
fn tunnel_stdio(
    child: &dyn PipedChildHandle,
) -> Option<(riabuild_runner::ChildReader, riabuild_runner::ChildWriter)> {
    Some((child.take_stdout()?, child.take_stdin()?))
}

/// Says what riabuild is doing before it does it.
///
/// The developer asked for a shell and is about to get one that behaves
/// differently from the mosh they know, so the difference is named rather than
/// left to be discovered: a tunnelled session is still instant-echo and still
/// survives a dropped packet, and it does **not** survive changing network,
/// because the TCP connection underneath it does not.
fn warn(ui: &Ui, remote: &Remote) {
    ui.warn(&format!(
        "This network blocks UDP, so mosh to {} is being tunnelled over TCP.",
        remote.name
    ));
    ui.note(
        "Local echo and a dropped-packet-proof session still work. Roaming does not — \
         changing network or sleeping ends this one, where plain mosh would have survived it.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connect_line_is_found_under_a_login_banner() {
        let stdout = "Welcome to Ubuntu 24.04\n\
                      Last login: Tue\n\
                      MOSH CONNECT 60001 4NeCCgvZFe0RnbY5AwCfmw\n";
        let session = connect_line(stdout).expect("a session");
        assert_eq!(session.port, 60001);
        assert_eq!(session.key, "4NeCCgvZFe0RnbY5AwCfmw");
    }

    #[test]
    fn a_server_that_said_something_else_is_not_a_session() {
        assert!(connect_line("mosh-server: command not found\n").is_none());
        assert!(connect_line("MOSH CONNECT\n").is_none());
        assert!(connect_line("MOSH CONNECT sixty 4NeCC\n").is_none());
    }

    /// The laptop's own locale is what goes on the wire, unchanged.
    #[test]
    fn the_laptops_locale_is_what_the_server_is_told() {
        let sending = locales_from(|key| match key {
            "LANG" => Some("en_GB.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(sending, vec!["LANG=en_GB.UTF-8"]);
    }

    /// `mosh-server` refuses to start under a non-UTF-8 locale, and the message
    /// it prints is about the server's — so a laptop with `LANG` unset would
    /// send a developer to fix the wrong machine.
    #[test]
    fn a_laptop_that_names_no_utf8_locale_still_gets_a_session() {
        for env in [
            Vec::new(),
            vec![("LANG", "C")],
            vec![("LC_ALL", "en_GB.ISO-8859-1")],
        ] {
            let sending = locales_from(|key| {
                env.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.to_string())
            });
            assert!(
                sending.iter().any(|line| line == "LANG=C.UTF-8"),
                "{sending:?}"
            );
        }
    }

    /// …and a laptop that does name one is left alone, however it spells it.
    #[test]
    fn a_utf8_locale_is_not_overridden_however_it_is_spelled() {
        for spelling in ["en_GB.UTF-8", "en_US.utf8", "C.UTF8"] {
            let sending = locales_from(|key| (key == "LC_ALL").then(|| spelling.to_string()));
            assert_eq!(sending, vec![format!("LC_ALL={spelling}")], "{spelling}");
        }
    }
}
