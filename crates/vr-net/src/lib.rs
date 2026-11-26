//! LAN discovery, address reflection and UDP hole punching.
//!
//! Velarouter prefers the shortest path between two peers:
//!
//! 1. **Same LAN** — the viewer probes the agent's advertised addresses and
//!    connects directly. Nothing leaves the LAN, latency is a single switch hop.
//! 2. **Different NATs** — both peers learn their public UDP address from the
//!    signal server, punch a hole with simultaneous probes, and only fall back
//!    to the relay when that fails.
//! 3. **Relay** — the encrypted session is tunnelled through the signal server.
//!
//! Every step is optional: a direct WebSocket on the LAN works even when the
//! signal server is unreachable, which is what makes offline/air-gapped use
//! possible.

pub mod direct;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UdpSocket;

pub const DISCOVERY_REQUEST: &[u8] = b"VRDISC1";
pub const DISCOVERY_RESPONSE: &[u8] = b"VRDISC1R";
pub const REFLEX_REQUEST: &[u8] = b"VRREF1";
pub const REFLEX_RESPONSE: &[u8] = b"VRREF1R";
pub const PUNCH_PROBE: &[u8] = b"VRPUNCH";
pub const PUNCH_ACK: &[u8] = b"VRPUNCHK";

#[derive(Debug, Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("malformed packet")]
    Malformed,
}

/// One address a viewer can try on the local network.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LanHint {
    pub addr: String,
    /// `ws` (agent WebSocket), `tcp` (framed stream) or `udp` (media only).
    pub transport: String,
}

/// Enumerates the host's non-loopback IPv4/IPv6 addresses.
///
/// Loopback is skipped because a viewer on another machine cannot use it, and
/// link-local IPv6 is skipped because it needs a scope id to be routable.
pub fn local_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            match iface.addr {
                if_addrs::IfAddr::V4(v4) => {
                    if !v4.ip.is_link_local() && !v4.ip.is_unspecified() {
                        out.push(IpAddr::V4(v4.ip));
                    }
                }
                if_addrs::IfAddr::V6(v6) => {
                    let ip = v6.ip;
                    // fe80::/10 is link-local and needs a scope id to be usable;
                    // checking the prefix keeps the MSRV at 1.82.
                    let link_local = (ip.segments()[0] & 0xffc0) == 0xfe80;
                    if !link_local && !ip.is_unspecified() {
                        out.push(IpAddr::V6(ip));
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Addresses sorted so that the ones most likely to be reachable come first:
/// private ranges (192.168/10./172.16) before anything else.
pub fn ranked_addresses() -> Vec<IpAddr> {
    fn rank(ip: &IpAddr) -> u8 {
        match ip {
            IpAddr::V4(v4) if v4.is_private() => 0,
            IpAddr::V4(v4) if v4.is_loopback() => 3,
            IpAddr::V4(_) => 1,
            IpAddr::V6(v6) if v6.is_loopback() => 3,
            IpAddr::V6(_) => 2,
        }
    }
    let mut addrs = local_addresses();
    addrs.sort_by_key(rank);
    addrs
}

/// IPv4 subnet broadcast addresses, used by Wake-on-LAN and LAN discovery.
///
/// `255.255.255.255` is not routable on every network (macOS returns
/// `EADDRNOTAVAIL` when no interface claims it), so callers should also try the
/// per-interface broadcast address.
pub fn broadcast_addresses() -> Vec<SocketAddrV4> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                if let Some(b) = v4.broadcast {
                    out.push(SocketAddrV4::new(b, 0));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Builds the hint list an agent publishes for direct connections.
pub fn lan_hints(port: u16) -> Vec<LanHint> {
    ranked_addresses()
        .into_iter()
        .take(8)
        .flat_map(|ip| {
            let host = match ip {
                IpAddr::V4(v4) => v4.to_string(),
                IpAddr::V6(v6) => format!("[{v6}]"),
            };
            [
                LanHint {
                    addr: format!("ws://{host}:{port}/vr"),
                    transport: "ws".into(),
                },
                LanHint {
                    addr: format!("{host}:{port}"),
                    transport: "tcp".into(),
                },
            ]
        })
        .collect()
}

/// A discovery client: broadcasts on the LAN and collects agent replies.
pub struct Discovery {
    socket: UdpSocket,
}

impl Discovery {
    /// Binds an ephemeral port and enables broadcast.
    pub async fn new() -> Result<Self, NetError> {
        let sock = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        sock.set_reuse_address(true)?;
        sock.set_broadcast(true)?;
        sock.set_nonblocking(true)?;
        let addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, 0).into();
        sock.bind(&addr.into())?;
        let socket = UdpSocket::from_std(sock.into())?;
        Ok(Self { socket })
    }

    /// Broadcasts a probe and gathers replies for `wait`.
    ///
    /// Replies are `VRDISC1R` followed by a UTF-8 JSON [`vr_proto::DeviceInfo`].
    pub async fn scan(
        &self,
        port: u16,
        wait: Duration,
    ) -> Result<Vec<vr_proto::DeviceInfo>, NetError> {
        let broadcast: SocketAddr = (Ipv4Addr::BROADCAST, port).into();
        // A single broadcast can be dropped; send a few probes.
        for _ in 0..3 {
            let _ = self.socket.send_to(DISCOVERY_REQUEST, broadcast).await;
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        let mut found = Vec::new();
        let deadline = tokio::time::Instant::now() + wait;
        let mut buf = [0u8; 2048];
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, _from))) => {
                    if !buf[..n].starts_with(DISCOVERY_RESPONSE) {
                        continue;
                    }
                    if let Ok(info) = serde_json::from_slice::<vr_proto::DeviceInfo>(
                        &buf[DISCOVERY_RESPONSE.len()..n],
                    ) {
                        if !found
                            .iter()
                            .any(|d: &vr_proto::DeviceInfo| d.device_id == info.device_id)
                        {
                            found.push(info);
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(found)
    }
}

/// Answers discovery probes on the LAN. Runs until the task is aborted.
pub async fn serve_discovery(port: u16, info: vr_proto::DeviceInfo) -> Result<(), NetError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    socket.set_broadcast(true)?;
    let payload = serde_json::to_vec(&info).expect("DeviceInfo serialises");
    let mut buf = [0u8; 512];
    loop {
        let (n, from) = socket.recv_from(&mut buf).await?;
        if &buf[..n] != DISCOVERY_REQUEST {
            continue;
        }
        let mut reply = Vec::with_capacity(DISCOVERY_RESPONSE.len() + payload.len());
        reply.extend_from_slice(DISCOVERY_RESPONSE);
        reply.extend_from_slice(&payload);
        let _ = socket.send_to(&reply, from).await;
    }
}

/// Asks a reflex server what our public UDP address looks like.
///
/// This is a four-line "mini STUN": request magic in, `magic + ip + port` out.
/// Implementing it ourselves keeps the dependency surface tiny, and a peer can
/// also ask another peer (the agent) instead of the signal server.
pub async fn reflect(server: SocketAddr, timeout: Duration) -> Result<SocketAddr, NetError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.connect(server).await?;
    socket.send(REFLEX_REQUEST).await?;
    let mut buf = [0u8; 64];
    let n = match tokio::time::timeout(timeout, socket.recv(&mut buf)).await {
        Ok(r) => r?,
        Err(_) => return Err(NetError::Timeout(timeout)),
    };
    parse_reflex(&buf[..n])
}

fn parse_reflex(buf: &[u8]) -> Result<SocketAddr, NetError> {
    if !buf.starts_with(REFLEX_RESPONSE) {
        return Err(NetError::Malformed);
    }
    let rest = &buf[REFLEX_RESPONSE.len()..];
    if rest.len() < 6 {
        return Err(NetError::Malformed);
    }
    let ip_len = rest[0] as usize;
    if rest.len() < 1 + ip_len + 2 {
        return Err(NetError::Malformed);
    }
    let ip = std::str::from_utf8(&rest[1..1 + ip_len])
        .ok()
        .and_then(|s| s.parse::<IpAddr>().ok())
        .ok_or(NetError::Malformed)?;
    let port = u16::from_be_bytes([rest[1 + ip_len], rest[2 + ip_len]]);
    Ok(SocketAddr::new(ip, port))
}

/// Replies to reflex requests. Returns the bound port.
pub async fn serve_reflex(port: u16) -> Result<u16, NetError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    let local = socket.local_addr()?.port();
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf).await else {
                break;
            };
            if &buf[..n] != REFLEX_REQUEST {
                continue;
            }
            let ip = from.ip().to_string();
            let mut reply = Vec::with_capacity(REFLEX_RESPONSE.len() + 1 + ip.len() + 2);
            reply.extend_from_slice(REFLEX_RESPONSE);
            reply.push(ip.len() as u8);
            reply.extend_from_slice(ip.as_bytes());
            reply.extend_from_slice(&from.port().to_be_bytes());
            let _ = socket.send_to(&reply, from).await;
        }
    });
    Ok(local)
}

/// Performs simultaneous UDP hole punching with a peer.
///
/// Both sides must call this at roughly the same time with each other's
/// reflexive address. Returns a socket that has exchanged at least one
/// datagram with the peer, which is what keeps the NAT mapping alive.
pub async fn punch(peer: SocketAddr, timeout: Duration) -> Result<UdpSocket, NetError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.connect(peer).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 64];
    let started = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        let _ = socket.send(PUNCH_PROBE).await;
        let wait = Duration::from_millis(120);
        if let Ok(Ok(n)) = tokio::time::timeout(wait, socket.recv(&mut buf)).await {
            if buf[..n].starts_with(PUNCH_PROBE) {
                let _ = socket.send(PUNCH_ACK).await;
                tracing::debug!(peer = %peer, elapsed = ?started.elapsed(), "hole punched");
                return Ok(socket);
            }
            if buf[..n].starts_with(PUNCH_ACK) {
                return Ok(socket);
            }
        }
    }
    Err(NetError::Timeout(timeout))
}

/// Answers punch probes from a peer that wants a direct path to us.
pub async fn answer_punch(socket: &UdpSocket, timeout: Duration) -> Result<SocketAddr, NetError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 64];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(NetError::Timeout(timeout));
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) if buf[..n].starts_with(PUNCH_PROBE) => {
                let _ = socket.send_to(PUNCH_ACK, from).await;
                return Ok(from);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(NetError::Io(e)),
            Err(_) => return Err(NetError::Timeout(timeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflex_packet_roundtrip() {
        let ip = "203.0.113.7";
        let mut reply = Vec::new();
        reply.extend_from_slice(REFLEX_RESPONSE);
        reply.push(ip.len() as u8);
        reply.extend_from_slice(ip.as_bytes());
        reply.extend_from_slice(&45678u16.to_be_bytes());
        assert_eq!(
            parse_reflex(&reply).unwrap(),
            "203.0.113.7:45678".parse().unwrap()
        );
    }

    #[test]
    fn reflex_rejects_garbage() {
        assert!(matches!(parse_reflex(b"nope"), Err(NetError::Malformed)));
        assert!(matches!(
            parse_reflex(&[REFLEX_RESPONSE, &[4u8, b'1', b'.', b'2']].concat()),
            Err(NetError::Malformed)
        ));
    }

    #[test]
    fn hints_are_private_addresses_first() {
        for hint in lan_hints(34789) {
            assert!(hint.addr.contains("34789"));
            assert!(hint.transport == "ws" || hint.transport == "tcp");
        }
    }

    #[tokio::test]
    async fn reflex_server_and_client_agree() {
        let port = serve_reflex(0).await.unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let observed = reflect(addr, Duration::from_secs(2)).await.unwrap();
        assert_eq!(observed.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(observed.port(), 0);
    }

    #[tokio::test]
    async fn punch_pair_connects() {
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let b_addr = b.local_addr().unwrap();

        let answer_task =
            tokio::spawn(async move { answer_punch(&b, Duration::from_secs(3)).await });
        let punched = punch(b_addr, Duration::from_secs(3))
            .await
            .expect("initiator should punch through");
        let observed = answer_task
            .await
            .unwrap()
            .expect("responder should see the probe");
        // The responder learns the initiator's ephemeral port, which is exactly
        // the mapping the initiator is sending from.
        assert_eq!(observed, punched.local_addr().unwrap());
        assert_ne!(observed.port(), 0);
    }
}
