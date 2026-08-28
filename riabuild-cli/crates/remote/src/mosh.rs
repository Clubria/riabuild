//! Whether this laptop can reach the server over UDP, and what to do when it
//! cannot.
//!
//! mosh is UDP, and a network that lets no UDP out is an ordinary thing to be
//! sitting on: a conference guest network, a corporate egress filter, a hotel,
//! a captive portal that opened 80 and 443 and nothing else. On one of those,
//! `mosh` used to cost the developer nineteen seconds of `mosh-client` silence
//! and then hand back a plain `ssh` with no explanation of what had happened.
//!
//! So riabuild asks first, and when the answer is no it tunnels the session
//! over TCP with Mullvad's `udp-over-tcp` — each datagram framed with a 16-bit
//! big-endian length — rather than giving mosh up. Both halves of that tunnel
//! are riabuild itself: the crate is compiled in, so there is no second tool to
//! install on either end and the server's copy is already the same version as
//! this laptop's. See
//! `docs/superpowers/specs/2026-08-25-mosh-over-tcp-design.md`.
//!
//! **The tunnel rides the ssh command's own stdio, never a port forward.** That
//! is the same decision the clipboard channel made and for the same reason: a
//! hardened server with `AllowTcpForwarding no` refuses `-L` outright, and a
//! transport that needs one works on exactly the servers that need it least.
//! `ssh.rs`'s own test forbids a forward reaching the shared option list, and
//! nothing here adds one.

mod probe;
mod serve;
mod tunnel;

pub use serve::{tcp2udp, udp_echo};
pub(crate) use tunnel::open as open_over_tcp;

use super::{Remote, shell_command, ssh::Ssh};
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, PipedChildHandle};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The UDP ports mosh binds in, and therefore the ports worth asking about.
///
/// The probe deliberately does not test UDP to somewhere else on the internet.
/// What decides whether a mosh session works is whether *these* datagrams reach
/// *this* server, and the two questions come apart in both directions: a
/// network that allows DNS and QUIC still blocks 60001, and a cloud firewall
/// that has never opened an inbound UDP port fails a session on a laptop whose
/// own UDP is wide open. Asking the real path answers both at once.
const MOSH_PORTS: std::ops::RangeInclusive<u16> = 60000..=61000;

/// The `internal` subcommand that answers the UDP probe on the server.
///
/// Named here rather than written out at each end for the reason [`TCP2UDP`]
/// records.
pub const UDP_ECHO: &str = "udp-echo";

/// The `internal` subcommand that is the server's end of the tunnel.
///
/// A constant, and used both by the laptop building the remote command and by
/// `cli.rs` naming the clap variant, because the two spellings drifting apart
/// is a failure no test in this repository could see: clap's own kebab-casing
/// of `MoshTcp2Udp` is `mosh-tcp2-udp`, the laptop asked for `mosh-tcp2udp`,
/// and the whole feature answered every session with "unrecognized subcommand"
/// on stderr, a closed stdout, and a silent fall back to `ssh`. Nothing on
/// either machine said so, because falling back quietly is exactly what
/// `open_over_tcp` returning `None` is *supposed* to do when a server cannot
/// run the far end.
pub const TCP2UDP: &str = "mosh-tcp2udp";

/// What `internal udp-echo` prints once it has a socket, followed by the port.
const ECHO_PORT_LINE: &str = "RIABUILD-UDP-ECHO";

/// What `internal mosh-tcp2udp` prints once its end of the tunnel is up.
///
/// The one line either helper is allowed to write before the stream becomes
/// framed datagrams, which is why every other word those two commands have to
/// say goes to stderr.
const TUNNEL_READY_LINE: &str = "RIABUILD-TCP2UDP-READY";

/// The exit status the probe script uses to mean "no `mosh-server` here".
///
/// Distinct from anything `ssh` returns for itself (255) and from what a shell
/// returns for a command it could not run (126, 127), so a server without mosh
/// is never confused with a connection that failed or a riabuild that is not
/// where riabuild thought it was.
const NO_MOSH_SERVER: i32 = 3;

/// How long riabuild waits for a helper's opening line before giving up on it.
///
/// Generous, because the ssh underneath it has to authenticate first, and the
/// cost of being wrong is a mosh session silently not happening. It is bounded
/// at all because an old riabuild on the far side answers an unknown
/// subcommand by printing to *stderr* and exiting, which looks from here
/// exactly like a server thinking about it.
const HANDSHAKE: Duration = Duration::from_secs(20);

/// The bound on the whole of [`ask`] — the line, the probe and the exit status
/// together.
///
/// One bound over all three rather than one each, because what riabuild is
/// buying here is the guarantee that asking about mosh can never be why a
/// session did not open. A server that answers nothing at all leaves both the
/// read *and* the `wait` after it with nothing to resolve, so bounding only the
/// read would move the hang one line down.
const DECISION: Duration = Duration::from_secs(25);

/// A line can only be so long before it is not the line riabuild is waiting
/// for. Nothing legitimate here exceeds forty bytes.
const LINE_LIMIT: usize = 256;

/// What riabuild learned by asking the server one question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// No `mosh-server` on the far side. Nothing to tunnel, so `ssh`.
    NoServer,
    /// UDP reaches the server — or riabuild could not tell, which is the same
    /// decision. The mosh riabuild has always run.
    Direct,
    /// Nothing came back over UDP. mosh, over a TCP stream.
    OverTcp,
}

/// Asks the server, in one connection, both things riabuild needs to know
/// about mosh: whether `mosh-server` is installed, and whether a datagram sent
/// from this laptop reaches it.
///
/// One `ssh` rather than two on purpose. A `riabuild remote` already makes
/// about ten connections, each one a full handshake to a machine that may be a
/// continent away, and "does this server have mosh" and "can UDP reach it" are
/// one question about one subsystem. The shell script is what joins them: it
/// answers the first itself, and `exec`s riabuild's echo responder to answer
/// the second.
///
/// Never fails. Every way this can go wrong — a connection that drops, a
/// server whose riabuild predates the responder, a line that arrives
/// malformed — is [`Route::Direct`], which is precisely the behaviour riabuild
/// had before any of this existed.
pub(crate) async fn ask(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
    binary: &str,
    carry: Option<&crate::issued::Working>,
) -> Route {
    let script = format!(
        "command -v mosh-server >/dev/null 2>&1 || exit {NO_MOSH_SERVER}; \
         exec {binary} internal {UDP_ECHO}"
    );
    let child = match Ssh::to(remote, paths, runner.clone())
        .carry(carry)
        .spawn_piped(&shell_command(&script))
        .await
    {
        Ok(child) => child,
        Err(_) => return Route::Direct,
    };

    let route = tokio::time::timeout(DECISION, decide(child.as_ref(), &remote.host))
        .await
        .unwrap_or(Route::Direct);
    let _ = child.kill().await;
    route
}

/// The decision itself, with the connection already open.
///
/// Split from [`ask`] so that one `timeout` covers every step of it — see
/// [`DECISION`] — and so the branch structure is readable without the ssh
/// around it.
async fn decide(child: &dyn PipedChildHandle, host: &str) -> Route {
    match echo_port(child).await {
        Some(port) => match probe::reaches(host, port).await {
            true => Route::Direct,
            false => Route::OverTcp,
        },
        // No port line: either the server has no `mosh-server` and the script
        // exited before reaching riabuild at all, or something else went wrong
        // that riabuild has no better answer to than the mosh it always ran.
        None => match child.wait().await {
            Ok(output) if output.code == Some(NO_MOSH_SERVER) => Route::NoServer,
            _ => Route::Direct,
        },
    }
}

/// The port `internal udp-echo` bound, from the one line it prints.
async fn echo_port(child: &dyn PipedChildHandle) -> Option<u16> {
    let mut stdout = child.take_stdout()?;
    let line = tokio::time::timeout(HANDSHAKE, read_line(&mut stdout))
        .await
        .ok()?
        .ok()?;
    line.trim()
        .strip_prefix(ECHO_PORT_LINE)?
        .trim()
        .parse()
        .ok()
}

/// Reads one `\n`-terminated line, a byte at a time and without a `BufReader`.
///
/// The buffering is the point of doing it the slow way. Both helpers follow
/// their opening line with framed datagrams on the same stream, and a
/// `BufReader` that read ahead past the newline would swallow the first frames
/// of the session into a buffer the pump never looks at. The lines are under
/// forty bytes, once per session.
pub(super) async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < LINE_LIMIT {
        match reader.read(&mut byte).await? {
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ => line.push(byte[0]),
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Copies one direction of the tunnel, flushing every read straight on.
///
/// `tokio::io::copy` would be shorter and is wrong here in one specific way:
/// it flushes only when the copy *ends*, and one end of this pump is
/// `std::io::Stdout`, which is a `LineWriter`. Framed datagrams contain no
/// newline to trigger a line flush, so a keystroke would sit in a buffer until
/// a later one pushed it past a kilobyte — a session that types in bursts of
/// nothing and then everything.
pub(super) async fn pump<R, W>(mut from: R, mut to: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // One datagram's worth. `udp-over-tcp` frames with a 16-bit length, so
    // nothing on this stream is larger than that plus two bytes.
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = from.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        to.write_all(&buffer[..read]).await?;
        to.flush().await?;
    }
}

/// The loopback address a tunnel leg binds or dials, on either machine.
///
/// Every socket either end of this opens is loopback-only, which is the
/// property that makes the tunnel need nothing from anybody's firewall: the
/// server's `mosh-server` binds `127.0.0.1`, `tcp2udp` listens on `127.0.0.1`,
/// and the only thing that crosses the network is the ssh connection riabuild
/// was making anyway.
fn loopback(port: u16) -> std::net::SocketAddr {
    (std::net::Ipv4Addr::LOCALHOST, port).into()
}

/// `udp-over-tcp`'s socket options, as both ends set them.
///
/// `nodelay` is the only one that is not a default, and it is the difference
/// between a usable session and an unusable one: Nagle's algorithm holds a
/// small write back waiting for company, and every frame here is one
/// keystroke.
fn tcp_options() -> udp_over_tcp::TcpOptions {
    let mut options = udp_over_tcp::TcpOptions::default();
    options.nodelay = true;
    options
}

/// Binds the first free UDP port in mosh's own range.
///
/// In the range rather than anywhere free, because a probe that asked about a
/// port mosh would never use would answer a question nobody asked: firewalls
/// are written per port, and the whole value of this probe is that it tests
/// the path a session is about to take.
async fn bind_in_mosh_range() -> Result<tokio::net::UdpSocket> {
    for port in MOSH_PORTS {
        if let Ok(socket) =
            tokio::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await
        {
            return Ok(socket);
        }
    }
    Err(anyhow::anyhow!(
        "no free UDP port between {} and {}",
        MOSH_PORTS.start(),
        MOSH_PORTS.end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::{FakeRunner, RunOptions};
    use tokio::io::AsyncWriteExt;

    /// A probe connection and the far end of its stdio, so a test can play the
    /// server: announce a port, or say nothing and go away.
    async fn probe(fake: &Arc<FakeRunner>) -> Box<dyn PipedChildHandle> {
        fake.spawn_piped("ssh", &["build-01"], &RunOptions::default())
            .await
            .expect("a probe")
    }

    /// A UDP port that was free a moment ago and has nothing on it — what a
    /// blocked network is indistinguishable from, and deliberately so: riabuild
    /// tunnels either way, because both mean this session will not work.
    async fn a_silent_port() -> u16 {
        let taken = tokio::net::UdpSocket::bind(loopback(0))
            .await
            .expect("a socket");
        let port = taken.local_addr().expect("an address").port();
        drop(taken);
        port
    }

    /// The whole point of the module, at the level the decision is made.
    #[tokio::test]
    async fn a_datagram_that_does_not_come_back_is_a_session_over_tcp() {
        let fake = Arc::new(FakeRunner::new());
        let child = probe(&fake).await;
        let mut pipes = fake.pipes(0).expect("the far end");
        let port = a_silent_port().await;
        pipes
            .to_riabuild
            .write_all(format!("{ECHO_PORT_LINE} {port}\n").as_bytes())
            .await
            .expect("announces its port");

        assert_eq!(decide(child.as_ref(), "127.0.0.1").await, Route::OverTcp);
    }

    /// …and one that does come back is the mosh riabuild has always run. The
    /// tunnel costs roaming, so it is taken only where the direct path is
    /// *proven* not to work.
    #[tokio::test]
    async fn a_datagram_that_comes_back_is_the_mosh_riabuild_always_ran() {
        let fake = Arc::new(FakeRunner::new());
        let child = probe(&fake).await;
        let mut pipes = fake.pipes(0).expect("the far end");

        let echo = tokio::net::UdpSocket::bind(loopback(0))
            .await
            .expect("a socket");
        let port = echo.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            while let Ok((read, from)) = echo.recv_from(&mut buffer).await {
                let _ = echo.send_to(&buffer[..read], from).await;
            }
        });
        pipes
            .to_riabuild
            .write_all(format!("{ECHO_PORT_LINE} {port}\n").as_bytes())
            .await
            .expect("announces its port");

        assert_eq!(decide(child.as_ref(), "127.0.0.1").await, Route::Direct);
    }

    /// The script's own exit status, which is the only thing that may mean
    /// "this server has no mosh". A connection that failed exits 255 and a
    /// command a shell could not run exits 126 or 127, and none of those is
    /// this.
    #[tokio::test]
    async fn a_server_without_mosh_server_says_so_in_its_exit_status() {
        let fake = Arc::new(FakeRunner::new().spawning("ssh", NO_MOSH_SERVER, ""));
        let child = probe(&fake).await;
        assert_eq!(decide(child.as_ref(), "127.0.0.1").await, Route::NoServer);
    }

    /// Every other way of ending is "could not tell", and riabuild answers that
    /// with exactly the behaviour it had before any of this existed. A probe
    /// that failed must never be why a session did not open.
    #[tokio::test]
    async fn a_probe_that_failed_is_not_an_answer_about_udp() {
        for code in [255, 127, 1, 0] {
            let fake = Arc::new(FakeRunner::new().spawning("ssh", code, ""));
            let child = probe(&fake).await;
            assert_eq!(
                decide(child.as_ref(), "127.0.0.1").await,
                Route::Direct,
                "exit {code}"
            );
        }
    }

    #[tokio::test]
    async fn a_line_is_read_without_swallowing_what_follows_it() {
        // The whole reason `read_line` does it a byte at a time: the bytes
        // after the newline are the session, and a reader that buffered them
        // would eat the first frames.
        let mut stream = std::io::Cursor::new(b"RIABUILD-TCP2UDP-READY\n\x00\x05frame".to_vec());
        let line = read_line(&mut stream).await.expect("a line");
        assert_eq!(line, TUNNEL_READY_LINE);

        let mut rest = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut rest)
            .await
            .expect("the rest");
        assert_eq!(rest, b"\x00\x05frame");
    }

    #[tokio::test]
    async fn a_line_that_never_ends_is_not_read_for_ever() {
        let mut stream = std::io::Cursor::new(vec![b'x'; LINE_LIMIT * 4]);
        let line = read_line(&mut stream).await.expect("a line");
        assert_eq!(line.len(), LINE_LIMIT);
    }

    #[tokio::test]
    async fn a_pump_flushes_each_read_rather_than_only_the_last() {
        let from = std::io::Cursor::new(b"one".to_vec());
        let mut to = Vec::new();
        pump(from, &mut to).await.expect("pumps");
        assert_eq!(to, b"one");
    }

    /// The test the whole feature was missing: a datagram that goes in one end
    /// of the tunnel and comes back out of it, with **both real halves** wired
    /// to each other over a pipe that stands in for ssh's stdio.
    ///
    /// Every other test in this module tests one side against a hand-written
    /// stand-in for the other, and all of them passed while the feature was
    /// dead: the laptop asked for `internal mosh-tcp2udp` and clap had named
    /// the subcommand `mosh-tcp2-udp`, so no server ever got past the usage
    /// error. That mismatch is now impossible — [`TCP2UDP`] is the one spelling
    /// both ends are built from — and this is what would catch the next
    /// wiring bug regardless of where it is.
    #[tokio::test]
    async fn a_datagram_crosses_both_halves_of_the_tunnel_and_comes_back() {
        // Standing in for `mosh-server`, which the tunnel only ever reaches on
        // loopback and which this test needs nothing else about.
        let mosh = tokio::net::UdpSocket::bind(loopback(0))
            .await
            .expect("a socket");
        let mosh_port = mosh.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            while let Ok((read, from)) = mosh.recv_from(&mut buffer).await {
                let mut back = b"PONG:".to_vec();
                back.extend_from_slice(&buffer[..read]);
                let _ = mosh.send_to(&back, from).await;
            }
        });

        // ssh's stdio: one duplex stream, split at each end exactly as the two
        // halves see it.
        let (laptop, server) = tokio::io::duplex(64 * 1024);
        let (laptop_reads, laptop_writes) = tokio::io::split(laptop);
        let (server_reads, server_writes) = tokio::io::split(server);
        tokio::spawn(async move { serve::serve(mosh_port, server_reads, server_writes).await });

        let joined = tokio::time::timeout(
            Duration::from_secs(10),
            tunnel::join(laptop_reads, laptop_writes),
        )
        .await
        .expect("the far end announces itself in time")
        .expect("a joined tunnel");

        let client = tokio::net::UdpSocket::bind(loopback(0))
            .await
            .expect("a socket");
        client
            .connect(loopback(joined.port))
            .await
            .expect("connects");
        client.send(b"HELLO").await.expect("sends");

        let mut back = [0u8; 64];
        let read = tokio::time::timeout(Duration::from_secs(10), client.recv(&mut back))
            .await
            .expect("an answer in time")
            .expect("an answer");
        assert_eq!(&back[..read], b"PONG:HELLO");
        joined.stop();
    }

    /// The two spellings that used to be able to drift apart, now the same
    /// constant — and the shape the laptop actually sends, so a `binary` prefix
    /// or an argument moving still has to keep the subcommand where clap can
    /// see it.
    #[test]
    fn both_ends_name_the_same_subcommand() {
        assert_eq!(TCP2UDP, "mosh-tcp2udp");
        assert_eq!(UDP_ECHO, "udp-echo");
    }

    /// The probe asks about the ports mosh actually uses. A range that had
    /// drifted off mosh's own would make every answer here meaningless while
    /// still passing every other test in this file.
    #[test]
    fn the_probe_asks_about_the_ports_mosh_binds() {
        assert_eq!(*MOSH_PORTS.start(), 60000);
        assert_eq!(*MOSH_PORTS.end(), 61000);
    }

    #[test]
    fn every_socket_either_end_opens_is_loopback() {
        assert!(loopback(60001).ip().is_loopback());
    }

    #[test]
    fn the_tunnel_does_not_wait_for_company_before_sending_a_keystroke() {
        assert!(tcp_options().nodelay);
    }
}
