//! Direct P2P transport: rendezvous → UDP hole punching → QUIC.
//!
//! No relay is involved. A session only exists if a direct path was established;
//! if punching fails the connection fails loudly instead of silently degrading.
//!
//! ```text
//! agent                              signal (UDP + WS)                 viewer
//!   │  register device code                 │                             │
//!   │ ◄──── invite(token) ──────────────────│◄── POST /v1/rendezvous ─────│
//!   │  VRRG1 A <token> ────────────────────►│◄── VRRG1 V <token> ─────────│
//!   │ ◄──── VRRG1P <peer> ──────────────────│──── VRRG1P <peer> ─────────►│
//!   │  VRPUNCH ⇄ VRPUNCHK (same UDP socket) │                             │
//!   │  QUIC handshake over the punched socket                              │
//!   │ ◄══════════════ VRP session (one bidi stream) ═════════════════════►│
//! ```
//!
//! The password never reaches the signal server: it is the `secret` of the VRP
//! handshake that runs inside QUIC. QUIC provides transport encryption and
//! congestion control; VRP provides identity and end-to-end authentication.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::{Buf, BytesMut};
use quinn::rustls;
use quinn::{
    ClientConfig, Connection, Endpoint, EndpointConfig, RecvStream, SendStream, ServerConfig,
    TransportConfig,
};
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use vr_transport::{ChannelMsg, MessageChannel, MessageReceiver, MessageSender, TransportError};

const PUNCH_PROBE: &[u8] = b"VRPUNCH";
const PUNCH_ACK: &[u8] = b"VRPUNCHK";
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// One frame on the QUIC stream: `len u32 | kind u8 | payload`.
const KIND_TEXT: u8 = 0;
const KIND_BINARY: u8 = 1;

/// Waits for the peer's reflexive address, then punches on the same UDP socket
/// that will later carry QUIC.
pub async fn rendezvous(
    signal_udp: SocketAddr,
    token: &str,
    is_agent: bool,
    timeout: Duration,
) -> Result<(UdpSocket, SocketAddr)> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
    let role = if is_agent { "A" } else { "V" };
    let request = format!("VRRG1 {role} {token}");
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 256];
    let mut peer = None;

    while tokio::time::Instant::now() < deadline {
        socket.send_to(request.as_bytes(), signal_udp).await.ok();
        match tokio::time::timeout(Duration::from_millis(400), socket.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) if from == signal_udp => {
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(rest) = text.strip_prefix("VRRG1P ") {
                    peer = Some(rest.trim().parse::<SocketAddr>()?);
                    break;
                }
            }
            _ => continue,
        }
    }
    let peer =
        peer.ok_or_else(|| anyhow::anyhow!("rendezvous timed out (peer offline or UDP blocked)"))?;
    punch(&socket, peer, Duration::from_secs(4)).await?;
    Ok((socket, peer))
}

/// Simultaneous UDP punch on an existing socket, so the NAT mapping is the one
/// QUIC will reuse.
pub async fn punch(socket: &UdpSocket, peer: SocketAddr, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 64];
    while tokio::time::Instant::now() < deadline {
        socket.send_to(PUNCH_PROBE, peer).await.ok();
        match tokio::time::timeout(Duration::from_millis(150), socket.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) if from == peer => {
                let data = &buf[..n];
                if data == PUNCH_PROBE {
                    let _ = socket.send_to(PUNCH_ACK, from).await;
                    drain(socket).await;
                    return Ok(());
                }
                if data == PUNCH_ACK {
                    drain(socket).await;
                    return Ok(());
                }
            }
            _ => continue,
        }
    }
    bail!("udp hole punching failed (symmetric NAT or firewall); no relay is configured")
}

/// Removes punch packets still queued before QUIC takes over the socket.
async fn drain(socket: &UdpSocket) {
    let mut buf = [0u8; 256];
    for _ in 0..32 {
        match tokio::time::timeout(Duration::from_millis(5), socket.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) if n <= 16 => continue,
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    // A killed viewer process never sends CONNECTION_CLOSE; QUIC only notices at
    // the idle timeout, and until then the agent keeps its only session slot.
    // 12 s + 3 s keep-alive makes "reconnect after a crash" feel immediate.
    t.keep_alive_interval(Some(Duration::from_secs(3)));
    t.max_idle_timeout(Some(Duration::from_secs(10).try_into().expect("timeout")));
    Arc::new(t)
}

/// Server endpoint for the agent. A self-signed certificate is generated per
/// process; peers are authenticated by VRP, not by the certificate.
pub fn server_endpoint(socket: std::net::UdpSocket) -> Result<Endpoint> {
    let cert = rcgen::generate_simple_self_signed(vec!["velarouter".to_string()])?;
    let cert_der = quinn::rustls::pki_types::CertificateDer::from(cert.cert);
    let key = quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let mut cfg = ServerConfig::with_single_cert(vec![cert_der], key.into())?;
    cfg.transport_config(transport());
    Ok(Endpoint::new(
        EndpointConfig::default(),
        Some(cfg),
        socket,
        Arc::new(quinn::TokioRuntime),
    )?)
}

/// Client endpoint for the viewer. Certificate verification is deliberately
/// skipped: the certificate is ephemeral and the real identity proof is the VRP
/// password handshake inside the stream.
pub fn client_endpoint(socket: std::net::UdpSocket) -> Result<Endpoint> {
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify(Arc::new(provider))))
        .with_no_client_auth();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut cfg = ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(transport());
    let mut endpoint = Endpoint::new(
        EndpointConfig::default(),
        None,
        socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(cfg);
    Ok(endpoint)
}

#[derive(Debug)]
struct SkipVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Takes ownership of the punched socket and starts QUIC on it.
fn into_std(socket: UdpSocket) -> Result<std::net::UdpSocket> {
    let socket = socket.into_std()?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

pub async fn connect(endpoint: &Endpoint, peer: SocketAddr) -> Result<QuicChannel> {
    let conn = endpoint
        .connect(peer, "velarouter")?
        .await
        .context("quic handshake failed")?;
    let (send, recv) = conn.open_bi().await.context("open stream")?;
    Ok(QuicChannel::new(conn, send, recv, peer))
}

pub async fn accept(endpoint: &Endpoint) -> Result<QuicChannel> {
    let incoming = endpoint
        .accept()
        .await
        .ok_or_else(|| anyhow::anyhow!("endpoint closed"))?;
    let peer = incoming.remote_address();
    let conn = incoming.await.context("quic handshake failed")?;
    let (send, recv) = conn.accept_bi().await.context("accept stream")?;
    Ok(QuicChannel::new(conn, send, recv, peer))
}

pub fn bind_socket() -> Result<UdpSocket> {
    Ok(std::net::UdpSocket::bind(("0.0.0.0", 0))?.try_into()?)
}

/// A prepared P2P endpoint: punched socket + QUIC, ready for a VRP session.
pub struct P2p {
    pub endpoint: Endpoint,
    pub peer: SocketAddr,
    pub local_port: u16,
}

pub async fn prepare_agent(signal_udp: SocketAddr, token: &str, timeout: Duration) -> Result<P2p> {
    let (socket, peer) = rendezvous(signal_udp, token, true, timeout).await?;
    let local_port = socket.local_addr()?.port();
    let endpoint = server_endpoint(into_std(socket)?)?;
    Ok(P2p {
        endpoint,
        peer,
        local_port,
    })
}

pub async fn prepare_viewer(signal_udp: SocketAddr, token: &str, timeout: Duration) -> Result<P2p> {
    let (socket, peer) = rendezvous(signal_udp, token, false, timeout).await?;
    let local_port = socket.local_addr()?.port();
    let endpoint = client_endpoint(into_std(socket)?)?;
    Ok(P2p {
        endpoint,
        peer,
        local_port,
    })
}

/// Dials every candidate and keeps the first connection that completes. Used
/// by the broker rendezvous path, where the host may still be opening its NAT
/// mapping when the viewer starts dialling.
pub async fn connect_any(
    endpoint: &Endpoint,
    addrs: &[SocketAddr],
    timeout: Duration,
) -> Result<(QuicChannel, SocketAddr)> {
    use futures_util::stream::FuturesUnordered;
    use futures_util::StreamExt;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut attempts = FuturesUnordered::new();
    for addr in addrs {
        let endpoint = endpoint.clone();
        let addr = *addr;
        attempts.push(async move {
            let conn = endpoint.connect(addr, "velarouter")?.await?;
            Ok::<_, anyhow::Error>((conn, addr))
        });
    }
    let mut last = None;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() || attempts.is_empty() {
            return Err(last.unwrap_or_else(|| {
                anyhow::anyhow!("no direct path: the peer never answered (symmetric NAT?)")
            }));
        }
        match tokio::time::timeout(left, attempts.next()).await {
            Ok(Some(Ok((conn, addr)))) => {
                let (send, recv) = conn.open_bi().await.context("open stream")?;
                return Ok((QuicChannel::new(conn, send, recv, addr), addr));
            }
            Ok(Some(Err(e))) => last = Some(e),
            Ok(None) => {
                return Err(last.unwrap_or_else(|| {
                    anyhow::anyhow!("no direct path: every candidate was rejected")
                }))
            }
            Err(_) => return Err(anyhow::anyhow!("direct connection timed out")),
        }
    }
}

// ---------------------------------------------------------------------------
// MessageChannel over one QUIC bidirectional stream
// ---------------------------------------------------------------------------
//
// Two properties matter more than anything else here, because a remote-desktop
// session sends and receives from a `select!` loop:
//
// * writes are **atomic**: a complete frame goes through a queue to a writer
//   task, so a cancelled `send` can never leave half a frame on the wire;
// * reads are **cancel-safe**: bytes land in a buffer owned by the receiver and
//   are parsed from there, so a cancelled `recv` loses nothing.
//
// Getting either wrong desynchronises the length-prefixed stream and surfaces
// as `message too large: <garbage>` — which is exactly what users see when an
// input event races a 4K frame.

pub struct QuicChannel {
    conn: Arc<Connection>,
    send: SendStream,
    recv: RecvStream,
    peer: SocketAddr,
    buf: BytesMut,
}

impl QuicChannel {
    fn new(conn: Connection, send: SendStream, recv: RecvStream, peer: SocketAddr) -> Self {
        Self {
            conn: Arc::new(conn),
            send,
            recv,
            peer,
            buf: BytesMut::new(),
        }
    }

    async fn write_frame(&mut self, kind: u8, payload: &[u8]) -> Result<(), TransportError> {
        let frame = frame_bytes(kind, payload)?;
        self.send
            .write_all(&frame)
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))?;
        self.send
            .flush()
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<(u8, Vec<u8>), TransportError> {
        read_frame(&mut self.recv, &mut self.buf).await
    }
}

/// `[len u32][kind u8][payload]` as one contiguous buffer.
fn frame_bytes(kind: u8, payload: &[u8]) -> Result<Vec<u8>, TransportError> {
    if payload.len() > MAX_FRAME {
        return Err(TransportError::TooLarge(payload.len()));
    }
    let mut frame = Vec::with_capacity(payload.len() + 5);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.push(kind);
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Cancel-safe frame reader: only `read_chunk` may be interrupted, and it never
/// consumes bytes it does not return.
async fn read_frame(
    stream: &mut RecvStream,
    buf: &mut BytesMut,
) -> Result<(u8, Vec<u8>), TransportError> {
    loop {
        if buf.len() >= 5 {
            let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                return Err(TransportError::TooLarge(len));
            }
            if buf.len() >= 5 + len {
                let kind = buf[4];
                let mut payload = vec![0u8; len];
                payload.copy_from_slice(&buf[5..5 + len]);
                buf.advance(5 + len);
                return Ok((kind, payload));
            }
        }
        match stream.read_chunk(64 * 1024, true).await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk.bytes),
            Ok(None) => return Err(TransportError::Closed),
            Err(e) => return Err(TransportError::Ws(e.to_string())),
        }
    }
}

#[async_trait]
impl MessageChannel for QuicChannel {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        self.write_frame(KIND_TEXT, text.as_bytes()).await
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.write_frame(KIND_BINARY, data).await
    }
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        match self.read_frame().await {
            Ok((KIND_TEXT, payload)) => String::from_utf8(payload)
                .map(ChannelMsg::Text)
                .map_err(|_| TransportError::BadFrame("non-utf8 text frame")),
            Ok((KIND_BINARY, payload)) => Ok(ChannelMsg::Binary(payload)),
            Ok(_) => Err(TransportError::BadFrame("unknown frame kind")),
            Err(TransportError::Closed) => Ok(ChannelMsg::Closed),
            Err(e) => Err(e),
        }
    }
    fn peer(&self) -> String {
        self.peer.to_string()
    }
    fn split(self: Box<Self>) -> (Box<dyn MessageSender>, Box<dyn MessageReceiver>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        let mut send = self.send;
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if send.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
        });
        (
            Box::new(QuicSender {
                _conn: self.conn.clone(),
                tx,
                peer: self.peer,
            }),
            Box::new(QuicReceiver {
                _conn: self.conn,
                recv: self.recv,
                buf: self.buf,
            }),
        )
    }
}

pub struct QuicSender {
    _conn: Arc<Connection>,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    peer: SocketAddr,
}

#[async_trait]
impl MessageSender for QuicSender {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        self.write(KIND_TEXT, text.as_bytes()).await
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.write(KIND_BINARY, data).await
    }
    fn peer(&self) -> String {
        self.peer.to_string()
    }
}

impl QuicSender {
    async fn write(&mut self, kind: u8, payload: &[u8]) -> Result<(), TransportError> {
        let frame = frame_bytes(kind, payload)?;
        // Queueing is the only await: dropping this future cannot tear a frame.
        self.tx
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)
    }
}

pub struct QuicReceiver {
    _conn: Arc<Connection>,
    recv: RecvStream,
    buf: BytesMut,
}

#[async_trait]
impl MessageReceiver for QuicReceiver {
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        match read_frame(&mut self.recv, &mut self.buf).await {
            Ok((KIND_TEXT, payload)) => String::from_utf8(payload)
                .map(ChannelMsg::Text)
                .map_err(|_| TransportError::BadFrame("non-utf8 text frame")),
            Ok((KIND_BINARY, payload)) => Ok(ChannelMsg::Binary(payload)),
            Ok(_) => Err(TransportError::BadFrame("unknown frame kind")),
            Err(TransportError::Closed) => Ok(ChannelMsg::Closed),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn punch_connects_two_local_sockets() {
        let a = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let b = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let b_addr = b.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            for _ in 0..50 {
                let (n, from) = b.recv_from(&mut buf).await.unwrap();
                if buf[..n].starts_with(PUNCH_PROBE) {
                    let _ = b.send_to(PUNCH_ACK, from).await;
                    return;
                }
            }
        });
        punch(&a, b_addr, Duration::from_secs(3)).await.unwrap();
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn punch_fails_loudly_without_peer() {
        let a = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let dead = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        // Nothing answers; the error must mention that no relay is available.
        let err = punch(&a, dead, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no relay"));
    }
}

#[cfg(test)]
mod cancellation {
    use super::*;

    /// A `select!` loop drops the losing future. Neither direction may lose
    /// bytes when that happens, otherwise the length-prefixed stream desyncs and
    /// the session dies with "message too large".
    #[tokio::test]
    async fn cancelled_reads_and_writes_keep_framing() {
        let server_std = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = server_std.local_addr().unwrap().port();
        let server = server_endpoint(server_std).unwrap();
        let client_std = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = client_endpoint(client_std).unwrap();
        let peer: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        let total = 40usize;
        let payload = vec![0x5a_u8; 96 * 1024];
        let expected = payload.clone();

        let accept_task = tokio::spawn(async move {
            let channel = accept(&server).await.unwrap();
            let (_tx, mut rx) = Box::new(channel).split();
            let mut got = 0;
            while got < total {
                // Cancellation stress: a 1 ms timer usually wins the select.
                tokio::select! {
                    msg = rx.recv() => match msg.unwrap() {
                        ChannelMsg::Binary(data) => {
                            assert_eq!(data.len(), expected.len(), "frame {got} is truncated");
                            assert!(data.iter().all(|b| *b == 0x5a));
                            got += 1;
                        }
                        ChannelMsg::Closed => break,
                        _ => {}
                    },
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
            got
        });

        let channel = connect(&client, peer).await.unwrap();
        let (mut tx, _rx) = Box::new(channel).split();
        for index in 0..total {
            tokio::select! {
                r = tx.send_binary(&payload) => r.unwrap(),
                _ = tokio::time::sleep(Duration::from_millis(1)) => {
                    // Cancelled send: the frame is either queued whole or not at all.
                }
            }
            if index % 7 == 0 {
                tokio::task::yield_now().await;
            }
        }
        // Make sure every frame is flushed before asserting.
        drop(tx);
        let got = tokio::time::timeout(Duration::from_secs(20), accept_task)
            .await
            .expect("reader timed out")
            .unwrap();
        assert_eq!(got, total);
    }
}
