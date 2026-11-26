//! Rendezvous without a server of our own.
//!
//! ## Why a public broker
//!
//! Two machines behind ordinary home routers cannot find each other from a
//! short code alone: someone has to tell each side where the other is. Running
//! that directory ourselves means every self-hoster needs a domain and a TLS
//! certificate before the first connection.
//!
//! Instead, the connection code *is* the rendezvous secret. Both sides derive
//! the same topic and the same AES key from it and meet on a public MQTT broker
//! over TLS (§[`broker`]). The broker only forwards encrypted blobs — it never
//! sees the code, the password, the addresses in clear, or a single pixel.
//! Self-hosters can point `--broker` at their own broker; the public defaults
//! are a convenience, not a dependency.
//!
//! ```text
//!   host (agent)                     broker                     viewer
//!   │ topic = f(code)                  │                          │
//!   │ cands = local + STUN  ──────────►│◄────────  cands         │
//!   │◄──────────────────────────────── │  (AES-GCM, key = f(code))│
//!   │ punch the viewer's candidates    │                          │
//!   │ ═════════ QUIC handshake over the punched socket ═════════►│
//! ```
//!
//! The code is short, so an attacker who guesses it can read candidate
//! addresses and inject noise. It still cannot complete the VRP handshake,
//! which needs the rotating password — see `docs/SECURITY.md`.

pub mod broker;
pub mod stun;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// How often a host republishes its candidates while waiting.
const HOST_INTERVAL: Duration = Duration::from_millis(1500);
/// Viewers publish faster: their window is short and the host reacts to them.
const VIEWER_INTERVAL: Duration = Duration::from_millis(700);
/// Time the host spends opening its NAT mapping toward the viewer.
const PUNCH_WINDOW: Duration = Duration::from_millis(1300);
/// Grace period so the viewer dials after the host's mapping exists.
const VIEWER_DELAY: Duration = Duration::from_millis(700);

const PUNCH: &[u8] = b"VRMEET1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Host,
    Viewer,
}

impl Role {
    fn tag(self) -> &'static str {
        match self {
            Self::Host => "h",
            Self::Viewer => "v",
        }
    }
    fn interval(self) -> Duration {
        match self {
            Self::Host => HOST_INTERVAL,
            Self::Viewer => VIEWER_INTERVAL,
        }
    }
}

/// What the host learned about the viewer: where to punch, and the session id
/// that both sides must feed into the VRP handshake.
#[derive(Debug, Clone)]
pub struct Peer {
    pub addrs: Vec<SocketAddr>,
    pub session: String,
}

#[derive(Serialize, Deserialize)]
struct Message {
    v: u8,
    role: String,
    session: String,
    addrs: Vec<String>,
    /// Milliseconds since the Unix epoch. The socket port can change between
    /// announcement rounds, so a later list always replaces an earlier one.
    #[serde(default)]
    ts: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// One topic + key per connection code. The code itself never appears on the
/// wire.
pub fn channel(code: &str) -> (String, [u8; 32]) {
    let code = code.trim();
    let digest = Sha256::digest(format!("velarouter-meet-v1|{code}").as_bytes());
    let topic = format!("vr/meet/1/{}", b64(&digest[..15]));
    (
        topic,
        vr_crypto::derive_short_secret(code, "velarouter/meet/v1"),
    )
}

fn random_session() -> String {
    let mut bytes = [0u8; 12];
    rand::Rng::fill_bytes(
        &mut rand::rand_core::UnwrapErr(rand::rngs::SysRng),
        &mut bytes,
    );
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Local addresses worth publishing. Loopback and link-local never help a
/// remote peer, so they are filtered out here rather than at the receiver.
fn local_candidates(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for ip in vr_net::ranked_addresses() {
        match ip {
            IpAddr::V4(v4) if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() => {}
            IpAddr::V6(v6) if v6.is_loopback() || v6.is_unspecified() => {}
            // `is_unicast_link_local` is newer than our MSRV; fe80::/10 is enough.
            IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => {}
            _ => out.push(SocketAddr::new(ip, port)),
        }
    }
    out
}

pub struct Announce {
    socket: Arc<UdpSocket>,
    link: broker::Link,
    topic: String,
    key: [u8; 32],
    role: Role,
    session: String,
    candidates: Vec<SocketAddr>,
    stun_results: mpsc::Receiver<Vec<SocketAddr>>,
    /// Candidates discovered elsewhere (router mapping, manual entry).
    external: Option<mpsc::Receiver<SocketAddr>>,
    last_publish: Option<Instant>,
}

impl Announce {
    /// Publishes this machine's candidates and subscribes to the peer's.
    ///
    /// STUN runs in the background: local candidates are announced immediately
    /// so a LAN peer does not wait for a public round trip.
    pub fn open(
        socket: Arc<UdpSocket>,
        code: &str,
        role: Role,
        brokers: &[String],
        session: Option<String>,
    ) -> Result<Self> {
        Self::open_with_external(socket, code, role, brokers, session, None)
    }

    /// Same as [`Announce::open`], but also accepts candidates that arrive
    /// later from outside (a UPnP mapping, for example).
    pub fn open_with_external(
        socket: Arc<UdpSocket>,
        code: &str,
        role: Role,
        brokers: &[String],
        session: Option<String>,
        external: Option<mpsc::Receiver<SocketAddr>>,
    ) -> Result<Self> {
        let port = socket.local_addr()?.port();
        let (topic, key) = channel(code);
        let mut candidates = local_candidates(port);
        if candidates.is_empty() {
            candidates.push(SocketAddr::from(([0, 0, 0, 0], port)));
        }
        let listeners = if brokers.is_empty() {
            broker::BROKERS.iter().map(|s| s.to_string()).collect()
        } else {
            brokers.to_vec()
        };
        let (tx, stun_results) = mpsc::channel(1);
        let probe_socket = socket.clone();
        tokio::spawn(async move {
            let found = stun::reflexive(&probe_socket, stun::SERVERS).await;
            for addr in &found {
                info!(%addr, "reflexive address discovered");
            }
            let _ = tx.send(found).await;
        });
        Ok(Self {
            socket,
            link: broker::open(topic.clone(), listeners),
            topic,
            key,
            role,
            session: session.unwrap_or_else(random_session),
            candidates,
            stun_results,
            external,
            last_publish: None,
        })
    }

    /// Becomes `true` once the broker accepted the subscription. The UI uses it
    /// to distinguish "code generated" from "the other side can find you".
    pub fn online(&self) -> tokio::sync::watch::Receiver<bool> {
        self.link.online.clone()
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn candidates(&self) -> &[SocketAddr] {
        &self.candidates
    }

    fn add_candidate(&mut self, addr: SocketAddr) {
        if addr.port() == 0 || addr.ip().is_unspecified() {
            return;
        }
        if !self.candidates.contains(&addr) {
            self.candidates.push(addr);
            self.last_publish = None; // announce the improvement right away
        }
    }

    fn publish(&mut self) {
        let message = Message {
            v: 1,
            role: self.role.tag().into(),
            session: self.session.clone(),
            addrs: self.candidates.iter().map(|a| a.to_string()).collect(),
            ts: now_ms(),
        };
        let Ok(json) = serde_json::to_vec(&message) else {
            return;
        };
        let Ok(sealed) = vr_crypto::seal_random(&self.key, self.topic.as_bytes(), &json) else {
            return;
        };
        if self.link.out.try_send(sealed).is_ok() {
            self.last_publish = Some(Instant::now());
        }
    }

    fn decode(&self, body: &[u8]) -> Option<(Role, String, Vec<SocketAddr>, u64)> {
        let plain = vr_crypto::open_random(&self.key, self.topic.as_bytes(), body).ok()?;
        let message: Message = serde_json::from_slice(&plain).ok()?;
        if message.v != 1 {
            return None;
        }
        let role = match message.role.as_str() {
            "h" => Role::Host,
            "v" => Role::Viewer,
            _ => return None,
        };
        let addrs = message
            .addrs
            .iter()
            .filter_map(|a| a.parse::<SocketAddr>().ok())
            .collect();
        Some((role, message.session, addrs, message.ts))
    }

    /// Waits for the peer, then (as host) opens the local NAT mapping.
    pub async fn wait_peer(&mut self, timeout: Duration) -> Result<Peer> {
        let deadline = Instant::now() + timeout;
        let mut peer_addrs: Vec<SocketAddr> = Vec::new();
        let mut peer_session = String::new();
        let mut peer_ts = 0u64;

        loop {
            if Instant::now() >= deadline {
                bail!("对方没有上线：请确认联机码正确，且两端都能访问互联网");
            }
            if let Ok(addrs) = self.stun_results.try_recv() {
                for addr in addrs {
                    self.add_candidate(addr);
                }
            }
            let mut extra = Vec::new();
            if let Some(external) = self.external.as_mut() {
                while let Ok(addr) = external.try_recv() {
                    extra.push(addr);
                }
            }
            for addr in extra {
                self.add_candidate(addr);
            }
            if self
                .last_publish
                .is_none_or(|at| at.elapsed() >= self.role.interval())
            {
                self.publish();
            }
            while let Ok(payload) = self.link.incoming.try_recv() {
                let Some((role, session, addrs, ts)) = self.decode(&payload.body) else {
                    continue;
                };
                if role == self.role {
                    continue; // the broker echoes our own publishes back
                }
                if self.role == Role::Viewer {
                    // A newer list wins outright: the host rebinds its socket
                    // between sessions, so old ports are dead ends.
                    if ts >= peer_ts {
                        peer_ts = ts;
                        peer_addrs = addrs;
                        if !session.is_empty() {
                            peer_session = session;
                        }
                    }
                } else {
                    if !session.is_empty() && peer_session.is_empty() {
                        peer_session = session;
                    }
                    for addr in addrs {
                        if !peer_addrs.contains(&addr) {
                            peer_addrs.push(addr);
                        }
                    }
                }
            }
            if !peer_addrs.is_empty() {
                debug!(
                    role = ?self.role,
                    peer = ?peer_addrs,
                    "rendezvous peer found"
                );
                if self.role == Role::Host {
                    // The viewer subscribed before its first publish, so a
                    // publish issued *now* is guaranteed to be delivered. Doing
                    // it after the punch covers a lost broker packet too.
                    self.publish();
                    self.punch(&peer_addrs).await;
                    self.publish();
                } else {
                    tokio::time::sleep(VIEWER_DELAY).await;
                }
                if self.role == Role::Viewer && peer_session.is_empty() {
                    bail!("对方没有公布会话标识，请让对方重新开始共享");
                }
                return Ok(Peer {
                    addrs: peer_addrs,
                    session: if self.role == Role::Viewer {
                        peer_session
                    } else {
                        self.session.clone()
                    },
                });
            }
            tokio::select! {
                payload = self.link.incoming.recv() => {
                    let Some(payload) = payload else {
                        bail!("会合通道已断开");
                    };
                    if let Some((role, session, addrs, ts)) = self.decode(&payload.body) {
                        if role != self.role {
                            if self.role == Role::Viewer {
                                if ts >= peer_ts {
                                    peer_ts = ts;
                                    peer_addrs = addrs;
                                    if !session.is_empty() {
                                        peer_session = session;
                                    }
                                }
                            } else {
                                for addr in addrs {
                                    if !peer_addrs.contains(&addr) {
                                        peer_addrs.push(addr);
                                    }
                                }
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(120)) => {}
            }
        }
    }

    /// Hands the socket back for QUIC. Waits for the background STUN probe to
    /// drop its clone so the caller gets exclusive ownership.
    pub async fn into_socket(self) -> Result<UdpSocket> {
        let Self { socket, .. } = self;
        for _ in 0..60 {
            if Arc::strong_count(&socket) == 1 {
                return Arc::try_unwrap(socket)
                    .map_err(|_| anyhow::anyhow!("会合套接字仍在使用中"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("会合套接字未能释放")
    }

    /// Sends throwaway packets so the router keeps a mapping for the viewer's
    /// address; QUIC then takes over the same socket.
    async fn punch(&self, addrs: &[SocketAddr]) {
        let deadline = Instant::now() + PUNCH_WINDOW;
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            for addr in addrs {
                let _ = self.socket.send_to(PUNCH, addr).await;
            }
            // Drain anything that arrives: proton packets, our probes echoed,
            // or a stray STUN reply. The socket must look idle for QUIC.
            let _ =
                tokio::time::timeout(Duration::from_millis(40), self.socket.recv_from(&mut buf))
                    .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_maps_to_an_unpredictable_topic() {
        let (topic, key) = channel("123456789012");
        assert!(topic.starts_with("vr/meet/1/"));
        assert!(!topic.contains("123456789012"));
        assert_eq!(channel("123456789012").0, topic);
        assert_eq!(channel("123456789012").1, key);
        assert_ne!(channel("123456789013").0, topic);
        assert_ne!(channel("123456789013").1, key);
    }

    #[test]
    fn candidate_list_excludes_loopback() {
        for addr in local_candidates(4000) {
            assert!(!addr.ip().is_loopback());
            assert_eq!(addr.port(), 4000);
        }
    }

    #[test]
    fn messages_are_sealed_and_role_tagged() {
        let (topic, key) = channel("abcdef123456");
        let message = Message {
            v: 1,
            role: "v".into(),
            session: "s1".into(),
            addrs: vec!["8.8.4.4:4000".into()],
            ts: 1,
        };
        let sealed = vr_crypto::seal_random(
            &key,
            topic.as_bytes(),
            &serde_json::to_vec(&message).unwrap(),
        )
        .unwrap();
        let (_, other) = channel("abcdef123456");
        let plain = vr_crypto::open_random(&other, topic.as_bytes(), &sealed).unwrap();
        let parsed: Message = serde_json::from_slice(&plain).unwrap();
        assert_eq!(parsed.role, "v");
        assert_eq!(parsed.addrs, vec!["8.8.4.4:4000".to_string()]);
        let (_, wrong) = channel("abcdef123457");
        assert!(vr_crypto::open_random(&wrong, topic.as_bytes(), &sealed).is_err());
    }
}
