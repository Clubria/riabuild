//! The laptop's half of the UDP question: send a nonce, wait for it back.
//!
//! Deliberately not a STUN request to a public server, which would have needed
//! no cooperation from the far side and would have answered a different
//! question. "Can this laptop send UDP to the internet" and "will a mosh
//! session to *this* server work" are not the same, and they come apart in both
//! directions — a network that passes DNS and QUIC still drops 60001, and a
//! cloud firewall that has never opened an inbound UDP port fails the session
//! from a laptop whose own UDP is fine. Asking the real path answers the
//! question riabuild is about to act on, and tells no third party that this
//! developer is connecting to anything.

use std::time::Duration;
use tokio::net::UdpSocket;

/// Prefixes the nonce so a stray datagram from something else on that port
/// cannot be mistaken for an answer.
const MAGIC: &[u8] = b"riabuild-udp-probe ";

/// Random bytes after the magic. Sixteen, because the only job is to be
/// unguessable by an on-path device that echoes whatever it is sent.
const NONCE_BYTES: usize = 16;

/// How many datagrams are sent before riabuild concludes none is coming back.
///
/// More than one because UDP is allowed to lose a packet and a single loss is
/// not a blocked network — concluding "blocked" from one dropped datagram
/// would tunnel a session that had no need to be tunnelled, at the cost of
/// mosh's roaming.
const TRIES: usize = 4;

/// How long each try waits before the next one goes out.
///
/// The whole probe is therefore bounded at two seconds, and on a working
/// network it finishes in one round trip.
const PATIENCE: Duration = Duration::from_millis(500);

/// Whether a datagram sent from this laptop to `host:port` comes back.
///
/// `false` is the interesting answer and it is deliberately the *cautious*
/// one: every failure that is not a proven round trip — DNS that will not
/// resolve, a socket that will not bind, a reply that does not match — reads
/// as "not proven", and the caller tunnels. Tunnelling a session that did not
/// need it costs roaming; not tunnelling one that did costs the session.
pub(super) async fn reaches(host: &str, port: u16) -> bool {
    let Ok(mut addresses) = tokio::net::lookup_host((host, port)).await else {
        return false;
    };
    let Some(server) = addresses.next() else {
        return false;
    };

    // Bound to match the family the name resolved to. A v4 socket cannot send
    // to a v6 address at all, and the failure is silent enough to look like a
    // blocked network.
    let local = if server.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
    };
    let Ok(socket) = UdpSocket::bind(local).await else {
        return false;
    };
    // Connected, so the kernel drops anything that did not come from the
    // server — the probe is then only ever answered by the machine it asked.
    if socket.connect(server).await.is_err() {
        return false;
    }

    let mut nonce = MAGIC.to_vec();
    let mut random = [0u8; NONCE_BYTES];
    if getrandom::fill(&mut random).is_err() {
        return false;
    }
    nonce.extend_from_slice(&random);

    let mut buffer = vec![0u8; nonce.len() + 1];
    for _ in 0..TRIES {
        if socket.send(&nonce).await.is_err() {
            return false;
        }
        if let Ok(Ok(read)) = tokio::time::timeout(PATIENCE, socket.recv(&mut buffer)).await
            && buffer[..read] == nonce[..]
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The round trip, against a socket standing in for the server's echo.
    /// Loopback, so this asserts the wire format and the matching rather than
    /// anything about a network.
    #[tokio::test]
    async fn a_datagram_that_comes_back_is_a_reachable_server() {
        let echo = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("an echo socket");
        let port = echo.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 1024];
            while let Ok((read, from)) = echo.recv_from(&mut buffer).await {
                let _ = echo.send_to(&buffer[..read], from).await;
            }
        });

        assert!(reaches("127.0.0.1", port).await);
    }

    /// Nothing is listening, so nothing comes back — which is exactly what a
    /// blocked network looks like from here, and is why the two are one answer.
    #[tokio::test]
    async fn a_port_nothing_answers_on_is_not_reachable() {
        let taken = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a socket");
        let port = taken.local_addr().expect("an address").port();
        drop(taken);

        assert!(!reaches("127.0.0.1", port).await);
    }

    /// A middlebox that answers every datagram with something of its own must
    /// not be able to make riabuild believe the server replied.
    #[tokio::test]
    async fn a_reply_that_is_not_the_nonce_does_not_count() {
        let liar = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a socket");
        let port = liar.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 1024];
            while let Ok((_, from)) = liar.recv_from(&mut buffer).await {
                let _ = liar.send_to(b"something else entirely", from).await;
            }
        });

        assert!(!reaches("127.0.0.1", port).await);
    }

    #[tokio::test]
    async fn a_name_that_does_not_resolve_is_not_reachable() {
        assert!(!reaches("this-host-does-not-exist.invalid", 60001).await);
    }
}
