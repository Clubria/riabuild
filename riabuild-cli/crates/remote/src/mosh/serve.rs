//! The two halves that run **on the server**, each behind a hidden `internal`
//! subcommand and each driven by the laptop over one ssh connection.
//!
//! Both are reached by `exec`ing the server's own riabuild, which
//! `install::ensure_riabuild` has already put there at the same version this
//! laptop is running — so there is no second tool to install for the tunnel,
//! and no version of the protocol to negotiate. A server whose riabuild
//! predates these subcommands answers on stderr and exits, which the laptop
//! reads as "could not tell" and treats exactly as it treated every server
//! before this existed.
//!
//! **Nothing here may print to stdout except its own one protocol line.**
//! stdout is the transport: after `RIABUILD-TCP2UDP-READY` it carries framed
//! datagrams, and a stray warning wedged into that stream is a corrupted
//! session rather than a message anybody reads. This is why `main.rs`
//! dispatches both before the banner, the config and the API client exist, the
//! same reason `internal askpass` is dispatched there.

use super::{TUNNEL_READY_LINE, bind_in_mosh_range, loopback, pump, tcp_options};
use anyhow::Result;
use riabuild_ui::Failure;
use std::time::Duration;
use tokio::net::TcpStream;

/// How long the echo responder stays up if nobody probes it.
///
/// The probe takes at most two seconds and the laptop kills this the moment it
/// has its answer, so this bound is only ever reached by a laptop that died
/// mid-probe. It exists because the alternative is a process holding a port in
/// mosh's own range on somebody else's server for as long as `sshd` takes to
/// notice — which is the sort of leftover that is discovered a month later by
/// a session that could not get a port.
const ECHO_LIFETIME: Duration = Duration::from_secs(20);

/// How long the far end waits for `tcp2udp` to be listening before giving up.
const LISTEN_PATIENCE: Duration = Duration::from_secs(5);

/// `riabuild internal udp-echo` — bind a UDP port in mosh's range, say which,
/// and send back whatever arrives.
///
/// The whole of the server's part in answering "will a mosh session work from
/// this network". It echoes rather than merely receiving because a one-way
/// datagram proves nothing the laptop can see: the question is a round trip,
/// and both directions of it are firewalled separately.
///
/// Echoes every datagram rather than stopping after the first, because the
/// laptop sends several — one lost packet is not a blocked network — and a
/// responder that answered only the first would turn its own success into a
/// silence for every retry after it.
pub async fn udp_echo() -> Result<i32> {
    let socket = bind_in_mosh_range().await.map_err(|error| {
        Failure::new(
            "opening a UDP port to test this network with",
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail(error.to_string())
    })?;
    let port = socket.local_addr()?.port();
    announce_port(&format!("{} {port}", super::ECHO_PORT_LINE))?;

    let echoing = async {
        let mut buffer = vec![0u8; 2048];
        loop {
            let (read, from) = socket.recv_from(&mut buffer).await?;
            socket.send_to(&buffer[..read], from).await?;
        }
        // `loop` never breaks, and naming the error type is what lets `?`
        // above resolve at all.
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };

    tokio::select! {
        _ = tokio::time::sleep(ECHO_LIFETIME) => {}
        _ = echoing => {}
    }
    Ok(0)
}

/// `riabuild internal mosh-tcp2udp <mosh-port>` — the server end of the
/// tunnel: TCP frames in over ssh's stdio, datagrams out to `mosh-server`.
///
/// Three sockets, all of them loopback, which is what makes this need nothing
/// from the server's firewall: `mosh-server` is bound to `127.0.0.1` by the
/// laptop that started it, `tcp2udp` listens on `127.0.0.1`, and this process
/// dials `127.0.0.1` and pumps that connection to and from the stdio ssh gave
/// it. Nothing new listens on an address anyone else can reach.
///
/// The extra loopback hop is the price of using `udp-over-tcp` as published:
/// its `tcp2udp` speaks to a `TcpStream` and nothing else, so a stream that
/// arrived as a pair of pipes has to become a `TcpStream` before it can be
/// handed over. Reimplementing the framing here to save the hop would mean
/// riabuild owning a wire protocol it does not own, and the hop costs a memcpy
/// on traffic measured in keystrokes.
pub async fn tcp2udp(mosh_port: u16) -> Result<i32> {
    serve(mosh_port, tokio::io::stdin(), tokio::io::stdout()).await
}

/// [`tcp2udp`] with the stdio ssh gave it as parameters.
///
/// Split out for one reason: it is the half of the tunnel that cannot be
/// reached from a test otherwise, because in production its two ends are this
/// process's real stdin and stdout. With the stream as an argument, the laptop
/// half in `tunnel::join` can be wired straight to it over a pair of pipes and
/// a datagram pushed the whole way through — which is the only test that would
/// have caught either end being unable to speak to the other.
pub(super) async fn serve<R, W>(mosh_port: u16, mut incoming: R, mut outgoing: W) -> Result<i32>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let port = free_local_port().await?;
    let mut options =
        udp_over_tcp::tcp2udp::Options::new(vec![loopback(port)], loopback(mosh_port));
    // Bound to loopback for the same reason everything else here is: the
    // datagrams this sends have `mosh-server` on the other end of the same
    // machine, and the default would have bound `0.0.0.0`.
    options.udp_bind_ip = Some(std::net::Ipv4Addr::LOCALHOST.into());
    options.tcp_options = tcp_options();

    // `run` never returns on success — its `Ok` type is `Infallible` — so it
    // is a task rather than something to await, and whether it came up is
    // answered by connecting to it below rather than by asking it.
    let listening = tokio::spawn(async move { udp_over_tcp::tcp2udp::run(options).await });

    let stream = dial(port).await?;
    // Only now, and down the same writer the frames go down rather than a
    // second handle onto the same file descriptor. A ready line printed before
    // the connection succeeded would tell the laptop to start sending into a
    // listener that may never have bound, and the first thing it would send is
    // the session.
    announce(&mut outgoing).await?;

    let (from_mosh, to_mosh) = stream.into_split();
    tokio::select! {
        // The laptop closing stdin is how a session ends cleanly, and how a
        // laptop that went away ends one that did not.
        _ = pump(&mut incoming, to_mosh) => {}
        _ = pump(from_mosh, &mut outgoing) => {}
    }
    listening.abort();
    Ok(0)
}

/// Writes the tunnel's ready line and makes sure it has actually left.
///
/// The flush is not decoration. stdout here is a pipe rather than a terminal,
/// so it is block-buffered, and a ready line that never left would deadlock
/// both ends against each other: the laptop waiting for a line, this process
/// waiting for a session that the laptop will not start until it sees one.
async fn announce<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    writer
        .write_all(format!("{TUNNEL_READY_LINE}\n").as_bytes())
        .await?;
    writer.flush().await?;
    Ok(())
}

/// The same, for the echo responder, whose stdout carries one line and then
/// nothing at all — so it has no stream to be handed and writes its own.
fn announce_port(line: &str) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{line}")?;
    stdout.flush()?;
    Ok(())
}

/// A loopback TCP port that was free a moment ago.
///
/// Bound and released rather than held, because `tcp2udp` opens its own
/// listener and there is no way to hand it one. The window is microseconds
/// wide, on loopback, against the ephemeral range — and `tcp2udp` sets
/// `SO_REUSEADDR`, so the only thing that can lose the race is another process
/// binding this exact port in that window. If one does, `run` fails, `dial`
/// below times out, and the laptop falls back to `ssh` with a warning rather
/// than to anything silent.
async fn free_local_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind(loopback(0)).await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Connects to `tcp2udp`, retrying while it is still binding.
///
/// The retry is the readiness check: `run` was spawned a moment ago and binds
/// its listener inside the task, so the first dial may well arrive first. A
/// connection that succeeds is proof the listener is up, which is what the
/// ready line then promises the laptop.
async fn dial(port: u16) -> Result<TcpStream> {
    let deadline = tokio::time::Instant::now() + LISTEN_PATIENCE;
    loop {
        match TcpStream::connect(loopback(port)).await {
            Ok(stream) => return Ok(stream),
            Err(error) if tokio::time::Instant::now() >= deadline => {
                return Err(Failure::new(
                    "starting this server's end of the mosh tunnel",
                    "Run `riabuild remote` again; it will open an ssh session if this keeps failing.",
                )
                .detail(error.to_string())
                .into());
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn a_free_port_is_one_that_can_then_be_bound() {
        let port = free_local_port().await.expect("a port");
        assert!(port > 0);
        tokio::net::TcpListener::bind(loopback(port))
            .await
            .expect("the port really was free");
    }

    #[tokio::test]
    async fn dialling_waits_for_a_listener_that_is_not_up_yet() {
        let port = free_local_port().await.expect("a port");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let listener = tokio::net::TcpListener::bind(loopback(port))
                .await
                .expect("binds");
            let _ = listener.accept().await;
            // Held until the test ends, so the accepted connection stays up.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        dial(port).await.expect("connects once the listener is up");
    }

    #[tokio::test]
    async fn dialling_gives_up_rather_than_waiting_for_ever() {
        let port = free_local_port().await.expect("a port");
        let started = std::time::Instant::now();
        assert!(dial(port).await.is_err());
        assert!(
            started.elapsed() < LISTEN_PATIENCE * 3,
            "it waited {:?}",
            started.elapsed()
        );
    }

    /// The echo responder answers *every* datagram, not just the first: the
    /// laptop sends several because one lost packet is not a blocked network,
    /// and a responder that stopped after one would make its own success look
    /// like silence to every retry.
    #[tokio::test]
    async fn the_echo_answers_more_than_one_datagram() {
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

        let client = tokio::net::UdpSocket::bind(loopback(0))
            .await
            .expect("a socket");
        client.connect(loopback(port)).await.expect("connects");
        for attempt in 0..3u8 {
            client.send(&[attempt]).await.expect("sends");
            let mut back = [0u8; 4];
            let read = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut back))
                .await
                .expect("an answer in time")
                .expect("an answer");
            assert_eq!(&back[..read], &[attempt]);
        }
    }

    /// The ready line is a line, terminated, and nothing else — the laptop
    /// reads exactly one before it starts treating the stream as frames.
    #[tokio::test]
    async fn the_ready_line_is_one_terminated_line() {
        let mut written = Vec::new();
        written
            .write_all(format!("{TUNNEL_READY_LINE}\n").as_bytes())
            .await
            .expect("writes");
        let mut stream = std::io::Cursor::new(written);
        assert_eq!(
            super::super::read_line(&mut stream).await.expect("a line"),
            TUNNEL_READY_LINE
        );
    }
}
