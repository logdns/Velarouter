//! The Velarouter session engine.
//!
//! One implementation serves both roles: the agent accepts, the viewer
//! initiates, and after the handshake the two objects are identical. That is
//! what keeps the protocol small enough to reimplement in Swift, Kotlin, C# or
//! TypeScript without divergence.
//!
//! ## Framing after the handshake
//!
//! Every frame is a binary WebSocket message (or a length-prefixed stream
//! message) with this plaintext layout, sealed with AES-256-GCM:
//!
//! ```text
//! envelope = kind:u8 || body
//!   kind 1 -> body is a JSON `Control`
//!   kind 2 -> body is `VideoHeader || payload`
//!
//! wire = counter:u64 || AES-GCM(envelope)
//! ```
//!
//! The counter is implicit per direction and starts at zero. A gap is a protocol
//! error: on an ordered channel it means a message was lost or forged.
//!
//! ## Why the session splits
//!
//! A viewer sends input while receiving video; an agent captures while receiving
//! input. Both halves therefore own independent state and are exposed as
//! [`SessionSender`] and [`SessionReceiver`], which can be moved into different
//! tasks. [`Session`] keeps them together for simple cases and tests.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_tungstenite::tungstenite::Message;
use vr_crypto::{
    derive_session_key, proof, verify_proof, EphemeralKey, Sealer, DIR_AGENT_TO_VIEWER,
    DIR_VIEWER_TO_AGENT,
};
use vr_proto::{
    Capabilities, Control, DeviceInfo, FileHeader, Role, VideoHeader, MAX_CONTROL_BYTES,
    PROTOCOL_VERSION,
};
use vr_transport::{ChannelMsg, MessageChannel, MessageReceiver, MessageSender, TransportError};

pub const ENVELOPE_CONTROL: u8 = 1;
pub const ENVELOPE_MEDIA: u8 = 2;
pub const ENVELOPE_FILE: u8 = 3;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("crypto: {0}")]
    Crypto(#[from] vr_crypto::CryptoError),
    #[error("protocol: {0}")]
    Proto(#[from] vr_proto::ProtoError),
    #[error("peer closed the connection")]
    Closed,
    #[error("handshake timeout")]
    Timeout,
    #[error("unexpected message: {0}")]
    Unexpected(&'static str),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("peer rejected the session: {0}")]
    Rejected(String),
}

/// What the handshake learned about the peer.
#[derive(Debug, Clone)]
pub struct Peer {
    pub role: Role,
    pub app: String,
    pub device: Option<DeviceInfo>,
    pub capabilities: Capabilities,
}

/// Configuration for one side of a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub session_id: String,
    /// Pairing code or pinned secret; mixed into the key derivation.
    pub secret: String,
    pub app: String,
    pub device: Option<DeviceInfo>,
    pub capabilities: Capabilities,
}

/// One received item.
#[derive(Debug)]
pub enum Event {
    Control(Control),
    Media {
        header: VideoHeader,
        payload: Vec<u8>,
    },
    /// One file-transfer chunk. The receiver writes `payload` at `offset`.
    File {
        header: FileHeader,
        payload: Vec<u8>,
    },
    /// A pong arrived. Callers compute RTT from the timestamp they sent.
    Pong {
        ts: u64,
    },
    /// The peer said goodbye or the channel ended.
    Closed(Option<String>),
}

/// Sending half: owns the outbound sealer and the frame counter.
pub struct SessionSender {
    tx: Sealer,
    channel: Box<dyn MessageSender>,
    frame_seq: Arc<AtomicU32>,
    session_id: String,
}

impl SessionSender {
    pub async fn send_control(&mut self, control: Control) -> Result<(), SessionError> {
        let json = control.to_json();
        if json.len() > MAX_CONTROL_BYTES {
            return Err(SessionError::Unexpected("control message too large"));
        }
        let mut envelope = Vec::with_capacity(json.len() + 1);
        envelope.push(ENVELOPE_CONTROL);
        envelope.extend_from_slice(json.as_bytes());
        let sealed = self.tx.seal(&envelope)?;
        self.channel.send_binary(&sealed).await?;
        Ok(())
    }

    /// Sends one encoded video frame.
    pub async fn send_media(
        &mut self,
        codec: vr_proto::Codec,
        display: u32,
        width: u16,
        height: u16,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        let frame_id = self.frame_seq.fetch_add(1, Ordering::Relaxed);
        let header = VideoHeader::new(codec, frame_id, display, width, height);
        let frame = header.frame(payload);
        let mut envelope = Vec::with_capacity(frame.len() + 1);
        envelope.push(ENVELOPE_MEDIA);
        envelope.extend_from_slice(&frame);
        let sealed = self.tx.seal(&envelope)?;
        self.channel.send_binary(&sealed).await?;
        Ok(())
    }

    pub async fn ping(&mut self) -> Result<u64, SessionError> {
        let ts = now_ms();
        self.send_control(Control::Ping { ts }).await?;
        Ok(ts)
    }

    /// Sends one file chunk. The caller owns `seq`/`offset` bookkeeping; this
    /// keeps the transport independent of the transfer state machine.
    pub async fn send_file_chunk(
        &mut self,
        header: FileHeader,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        let frame = header.frame(payload);
        let mut envelope = Vec::with_capacity(frame.len() + 1);
        envelope.push(ENVELOPE_FILE);
        envelope.extend_from_slice(&frame);
        let sealed = self.tx.seal(&envelope)?;
        self.channel.send_binary(&sealed).await?;
        Ok(())
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn peer(&self) -> String {
        self.channel.peer()
    }
}

/// Receiving half: owns the inbound sealer.
pub struct SessionReceiver {
    rx: Sealer,
    channel: Box<dyn MessageReceiver>,
}

impl SessionReceiver {
    /// Next event. Pings are answered by the caller (see [`Event::Control`]) or,
    /// for convenience, by [`SessionReceiver::recv`] callers that use
    /// [`SessionReceiver::recv_auto_pong`].
    pub async fn recv(&mut self) -> Result<Event, SessionError> {
        let msg = self.channel.recv().await?;
        let sealed = match msg {
            ChannelMsg::Binary(b) => b,
            ChannelMsg::Text(_) => {
                return Err(SessionError::Unexpected("text frame after handshake"))
            }
            ChannelMsg::Closed => return Ok(Event::Closed(None)),
        };
        let plain = self.rx.open(&sealed)?;
        let (kind, body) = plain
            .split_first()
            .ok_or(SessionError::Unexpected("empty envelope"))?;
        match *kind {
            ENVELOPE_CONTROL => {
                let control = Control::from_json(
                    std::str::from_utf8(body)
                        .map_err(|_| SessionError::Unexpected("non-utf8 control"))?,
                )?;
                match control {
                    Control::Pong { ts } => Ok(Event::Pong { ts }),
                    Control::Bye { reason } => Ok(Event::Closed(Some(reason))),
                    other => Ok(Event::Control(other)),
                }
            }
            ENVELOPE_MEDIA => {
                let (header, payload) = VideoHeader::parse(body)?;
                Ok(Event::Media {
                    header,
                    payload: payload.to_vec(),
                })
            }
            ENVELOPE_FILE => {
                let (header, payload) = FileHeader::parse(body)?;
                Ok(Event::File {
                    header,
                    payload: payload.to_vec(),
                })
            }
            _ => Err(SessionError::Unexpected("unknown envelope kind")),
        }
    }

    /// Convenience wrapper that answers pings itself; requires the sender so the
    /// pong can travel back. Prefer this in single-task clients and mobile SDKs.
    pub async fn recv_auto_pong(
        &mut self,
        sender: &mut SessionSender,
    ) -> Result<Event, SessionError> {
        loop {
            match self.recv().await? {
                Event::Control(Control::Ping { ts }) => {
                    sender.send_control(Control::Pong { ts }).await?;
                }
                other => return Ok(other),
            }
        }
    }
}

/// Both halves plus peer metadata.
pub struct Session {
    pub sender: SessionSender,
    pub receiver: SessionReceiver,
    peer: Peer,
}

impl Session {
    pub fn into_parts(self) -> (SessionSender, SessionReceiver, Peer) {
        (self.sender, self.receiver, self.peer)
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    pub async fn send_control(&mut self, control: Control) -> Result<(), SessionError> {
        self.sender.send_control(control).await
    }

    pub async fn send_media(
        &mut self,
        codec: vr_proto::Codec,
        display: u32,
        width: u16,
        height: u16,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        self.sender
            .send_media(codec, display, width, height, payload)
            .await
    }

    pub async fn ping(&mut self) -> Result<u64, SessionError> {
        self.sender.ping().await
    }

    pub async fn send_file_chunk(
        &mut self,
        header: FileHeader,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        self.sender.send_file_chunk(header, payload).await
    }

    pub async fn recv(&mut self) -> Result<Event, SessionError> {
        self.receiver.recv_auto_pong(&mut self.sender).await
    }

    pub async fn close(mut self, reason: &str) {
        let _ = self
            .sender
            .send_control(Control::Bye {
                reason: reason.to_string(),
            })
            .await;
    }

    /// Viewer side: sends `Hello`, performs the ECDH handshake, then exchanges
    /// capabilities.
    pub async fn connect(
        mut channel: Box<dyn MessageChannel>,
        cfg: SessionConfig,
    ) -> Result<Self, SessionError> {
        let key_pair = EphemeralKey::generate();
        send_json(
            &mut *channel,
            &Control::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Viewer,
                device: cfg.device.clone(),
                session_id: cfg.session_id.clone(),
                app: cfg.app.clone(),
            },
        )
        .await?;
        send_json(
            &mut *channel,
            &Control::AuthInit {
                viewer_pub: key_pair.public_b64(),
                nonce: random_b64(16),
            },
        )
        .await?;

        let finish = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_control(&mut *channel))
            .await
            .map_err(|_| SessionError::Timeout)??;
        let (agent_pub, agent_proof) = match finish {
            Control::AuthFinish { agent_pub, proof } => (agent_pub, proof),
            Control::AuthFail { reason } => return Err(SessionError::Rejected(reason)),
            _ => return Err(SessionError::Unexpected("expected auth_finish")),
        };
        let shared = key_pair.shared_secret(&agent_pub)?;
        let key = derive_session_key(&shared, &cfg.session_id, &cfg.secret)?;
        if !verify_proof(&key, "agent", &cfg.session_id, &agent_proof) {
            let _ = send_json(
                &mut *channel,
                &Control::AuthFail {
                    reason: "agent proof mismatch".into(),
                },
            )
            .await;
            return Err(SessionError::Auth("agent proof mismatch".into()));
        }
        send_json(
            &mut *channel,
            &Control::AuthProof {
                proof: proof(&key, "viewer", &cfg.session_id),
            },
        )
        .await?;
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_control(&mut *channel))
            .await
            .map_err(|_| SessionError::Timeout)??
        {
            Control::AuthOk => {}
            Control::AuthFail { reason } => return Err(SessionError::Rejected(reason)),
            _ => return Err(SessionError::Unexpected("expected auth_ok")),
        }

        finish_session(
            channel,
            key,
            &cfg,
            DIR_VIEWER_TO_AGENT,
            DIR_AGENT_TO_VIEWER,
            Role::Agent,
            String::new(),
            None,
        )
        .await
    }

    /// Agent side: waits for `Hello`, derives the key, proves itself, then
    /// exchanges capabilities.
    pub async fn accept(
        mut channel: Box<dyn MessageChannel>,
        cfg: SessionConfig,
    ) -> Result<Self, SessionError> {
        let hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_control(&mut *channel))
            .await
            .map_err(|_| SessionError::Timeout)??;
        let (peer_app, peer_device, peer_session) = match hello {
            Control::Hello {
                version,
                role,
                device,
                session_id,
                app,
            } => {
                if version != PROTOCOL_VERSION {
                    let _ = send_json(
                        &mut *channel,
                        &Control::AuthFail {
                            reason: format!("unsupported version {version}"),
                        },
                    )
                    .await;
                    return Err(SessionError::Auth(format!("unsupported version {version}")));
                }
                if role != Role::Viewer {
                    return Err(SessionError::Unexpected("expected viewer role"));
                }
                (app, device, session_id)
            }
            _ => return Err(SessionError::Unexpected("expected hello")),
        };
        // An empty configured id means "adopt whatever the viewer proposed".
        // That is what makes LAN direct connections work with a bare URL; relay
        // sessions always carry an explicit id bound to the session token.
        let effective_id = if cfg.session_id.is_empty() || peer_session == cfg.session_id {
            peer_session
        } else {
            let _ = send_json(
                &mut *channel,
                &Control::AuthFail {
                    reason: "session id mismatch".into(),
                },
            )
            .await;
            return Err(SessionError::Auth("session id mismatch".into()));
        };
        let mut cfg = cfg;
        cfg.session_id = effective_id.clone();

        let init = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_control(&mut *channel))
            .await
            .map_err(|_| SessionError::Timeout)??;
        let viewer_pub = match init {
            Control::AuthInit { viewer_pub, .. } => viewer_pub,
            _ => return Err(SessionError::Unexpected("expected auth_init")),
        };

        let key_pair = EphemeralKey::generate();
        let shared = key_pair.shared_secret(&viewer_pub)?;
        let key = derive_session_key(&shared, &effective_id, &cfg.secret)?;
        send_json(
            &mut *channel,
            &Control::AuthFinish {
                agent_pub: key_pair.public_b64(),
                proof: proof(&key, "agent", &cfg.session_id),
            },
        )
        .await?;

        let proof_msg = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv_control(&mut *channel))
            .await
            .map_err(|_| SessionError::Timeout)??;
        let viewer_proof = match proof_msg {
            Control::AuthProof { proof } => proof,
            Control::AuthFail { reason } => return Err(SessionError::Rejected(reason)),
            _ => return Err(SessionError::Unexpected("expected auth_proof")),
        };
        if !verify_proof(&key, "viewer", &cfg.session_id, &viewer_proof) {
            let _ = send_json(
                &mut *channel,
                &Control::AuthFail {
                    reason: "viewer proof mismatch".into(),
                },
            )
            .await;
            return Err(SessionError::Auth("viewer proof mismatch".into()));
        }
        send_json(&mut *channel, &Control::AuthOk).await?;

        finish_session(
            channel,
            key,
            &cfg,
            DIR_AGENT_TO_VIEWER,
            DIR_VIEWER_TO_AGENT,
            Role::Viewer,
            peer_app,
            peer_device,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn finish_session(
    channel: Box<dyn MessageChannel>,
    key: [u8; vr_crypto::KEY_LEN],
    cfg: &SessionConfig,
    tx_dir: u8,
    rx_dir: u8,
    peer_role: Role,
    peer_app: String,
    peer_device: Option<DeviceInfo>,
) -> Result<Session, SessionError> {
    let (raw_sender, raw_receiver) = channel.split();
    let mut sender = SessionSender {
        tx: Sealer::new(&key, tx_dir),
        channel: raw_sender,
        frame_seq: Arc::new(AtomicU32::new(0)),
        session_id: cfg.session_id.clone(),
    };
    let mut receiver = SessionReceiver {
        rx: Sealer::new(&key, rx_dir),
        channel: raw_receiver,
    };
    sender
        .send_control(Control::Caps {
            capabilities: cfg.capabilities.clone(),
            device: cfg.device.clone(),
        })
        .await?;

    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let (peer_caps, announced_device) = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(SessionError::Timeout);
        }
        match tokio::time::timeout(remaining, receiver.recv_auto_pong(&mut sender)).await {
            Ok(Ok(Event::Control(Control::Caps {
                capabilities,
                device,
            }))) => break (capabilities, device),
            Ok(Ok(Event::Control(_))) | Ok(Ok(Event::Pong { .. })) | Ok(Ok(Event::File { .. })) => {
                continue
            }
            Ok(Ok(Event::Media { .. })) => {
                return Err(SessionError::Unexpected("media before capabilities"))
            }
            Ok(Ok(Event::Closed(_))) | Ok(Err(_)) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        }
    };

    Ok(Session {
        sender,
        receiver,
        peer: Peer {
            role: peer_role,
            app: peer_app,
            device: announced_device.or(peer_device),
            capabilities: peer_caps,
        },
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_b64(len: usize) -> String {
    use base64::Engine as _;
    use rand::Rng;
    let mut buf = vec![0u8; len];
    rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut buf);
    base64::engine::general_purpose::STANDARD.encode(&buf)
}

async fn send_json(channel: &mut dyn MessageChannel, c: &Control) -> Result<(), SessionError> {
    channel.send_text(&c.to_json()).await?;
    Ok(())
}

async fn recv_control(channel: &mut dyn MessageChannel) -> Result<Control, SessionError> {
    match channel.recv().await? {
        ChannelMsg::Text(t) => Ok(Control::from_json(&t)?),
        ChannelMsg::Binary(_) => Err(SessionError::Unexpected("binary during handshake")),
        ChannelMsg::Closed => Err(SessionError::Closed),
    }
}

/// WebSocket client channel, used by viewers (native and CLI).
///
/// Two modes:
/// * direct (`ws://agent:34789/vr`) — VRP frames map 1:1 to WebSocket frames.
/// * relay (`wss://signal/v1/session/ws`) — VRP frames are carried inside binary
///   frames with a one-byte kind prefix, because the relay also needs a channel
///   for its own control messages.
pub struct WsChannel {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    peer: String,
    relay: bool,
}

/// Relay prefix for a VRP text frame (the handshake).
pub const RELAY_TEXT: u8 = 1;
/// Relay prefix for a VRP binary frame (everything after the handshake).
pub const RELAY_BINARY: u8 = 2;

impl WsChannel {
    pub async fn connect(url: &str) -> Result<Self, TransportError> {
        // Avoid reformatting a raw IPv6 host as "host:port" in tungstenite's TLS-enabled resolver.
        let parsed = url::Url::parse(url).map_err(|e| TransportError::Ws(e.to_string()))?;
        let result = match (parsed.scheme(), parsed.host()) {
            ("ws", Some(url::Host::Ipv6(ip))) => {
                let socket = tokio::net::TcpStream::connect((
                    ip,
                    parsed.port_or_known_default().unwrap_or(80),
                ))
                .await?;
                socket.set_nodelay(true)?;
                tokio_tungstenite::client_async(
                    url,
                    tokio_tungstenite::MaybeTlsStream::Plain(socket),
                )
                .await
            }
            _ => tokio_tungstenite::connect_async_with_config(url, None, true).await,
        };
        let (stream, _resp) = result.map_err(|e| TransportError::Ws(e.to_string()))?;
        Ok(Self {
            stream,
            peer: url.to_string(),
            relay: false,
        })
    }

    /// Connects to the signal relay. For the viewer (`wait_ready = true`) this
    /// blocks until the agent has accepted and the relay is ready to pipe.
    pub async fn connect_relay(url: &str, wait_ready: bool) -> Result<Self, TransportError> {
        use futures_util::StreamExt;
        let (mut stream, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))?;
        if wait_ready {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(TransportError::Ws("relay ready timeout".into()));
                }
                match tokio::time::timeout(remaining, stream.next()).await {
                    Ok(Some(Ok(Message::Text(t)))) => {
                        let v: serde_json::Value = serde_json::from_str(&t)
                            .map_err(|e| TransportError::Ws(e.to_string()))?;
                        match v.get("t").and_then(|t| t.as_str()) {
                            Some("ready") => break,
                            Some("peer_left") => {
                                return Err(TransportError::Ws("peer left the relay".into()))
                            }
                            Some("error") => {
                                return Err(TransportError::Ws(
                                    v.get("message")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("relay error")
                                        .to_string(),
                                ))
                            }
                            _ => continue,
                        }
                    }
                    Ok(Some(Ok(_))) => continue,
                    Ok(Some(Err(e))) => return Err(TransportError::Ws(e.to_string())),
                    Ok(None) => return Err(TransportError::Closed),
                    Err(_) => return Err(TransportError::Ws("relay ready timeout".into())),
                }
            }
        }
        Ok(Self {
            stream,
            peer: url.to_string(),
            relay: true,
        })
    }
}

fn relay_encode(relay: bool, is_text: bool, data: &[u8]) -> Vec<u8> {
    if !relay {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + 1);
    out.push(if is_text { RELAY_TEXT } else { RELAY_BINARY });
    out.extend_from_slice(data);
    out
}

fn relay_decode(relay: bool, data: Vec<u8>) -> Result<ChannelMsg, TransportError> {
    if !relay {
        return Ok(ChannelMsg::Binary(data));
    }
    let mut it = data.into_iter();
    match it.next() {
        Some(RELAY_TEXT) => String::from_utf8(it.collect())
            .map(ChannelMsg::Text)
            .map_err(|_| TransportError::BadFrame("relay text is not utf-8")),
        Some(RELAY_BINARY) => Ok(ChannelMsg::Binary(it.collect())),
        _ => Err(TransportError::BadFrame("relay frame without prefix")),
    }
}

#[async_trait::async_trait]
impl MessageChannel for WsChannel {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        use futures_util::SinkExt;
        let msg = if self.relay {
            Message::Binary(relay_encode(true, true, text.as_bytes()).into())
        } else {
            Message::Text(text.into())
        };
        self.stream
            .send(msg)
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))
    }

    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        use futures_util::SinkExt;
        self.stream
            .send(Message::Binary(
                relay_encode(self.relay, false, data).into(),
            ))
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))
    }

    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        use futures_util::StreamExt;
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    return Ok(ChannelMsg::Text(t.to_string()));
                }
                Some(Ok(Message::Binary(b))) => {
                    return relay_decode(self.relay, b.to_vec());
                }
                Some(Ok(Message::Close(_))) | None => return Ok(ChannelMsg::Closed),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(TransportError::Ws(e.to_string())),
            }
        }
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }

    fn split(self: Box<Self>) -> (Box<dyn MessageSender>, Box<dyn MessageReceiver>) {
        use futures_util::StreamExt;
        let (sink, stream) = self.stream.split();
        (
            Box::new(WsSender {
                sink,
                peer: self.peer,
                relay: self.relay,
            }),
            Box::new(WsReceiver {
                stream,
                relay: self.relay,
            }),
        )
    }
}

struct WsSender {
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    peer: String,
    relay: bool,
}

#[async_trait::async_trait]
impl MessageSender for WsSender {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        use futures_util::SinkExt;
        let msg = if self.relay {
            Message::Binary(relay_encode(true, true, text.as_bytes()).into())
        } else {
            Message::Text(text.into())
        };
        self.sink
            .send(msg)
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        use futures_util::SinkExt;
        self.sink
            .send(Message::Binary(
                relay_encode(self.relay, false, data).into(),
            ))
            .await
            .map_err(|e| TransportError::Ws(e.to_string()))
    }
    fn peer(&self) -> String {
        self.peer.clone()
    }
}

struct WsReceiver {
    stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    relay: bool,
}

#[async_trait::async_trait]
impl MessageReceiver for WsReceiver {
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        use futures_util::StreamExt;
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(t))) => return Ok(ChannelMsg::Text(t.to_string())),
                Some(Ok(Message::Binary(b))) => return relay_decode(self.relay, b.to_vec()),
                Some(Ok(Message::Close(_))) | None => return Ok(ChannelMsg::Closed),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(TransportError::Ws(e.to_string())),
            }
        }
    }
}

/// In-memory channel used by tests and by the loopback demo.
pub struct MemoryChannel {
    tx: tokio::sync::mpsc::UnboundedSender<ChannelMsg>,
    rx: tokio::sync::mpsc::UnboundedReceiver<ChannelMsg>,
    name: String,
}

pub fn memory_channel_pair() -> (MemoryChannel, MemoryChannel) {
    let (a_tx, a_rx) = tokio::sync::mpsc::unbounded_channel();
    let (b_tx, b_rx) = tokio::sync::mpsc::unbounded_channel();
    (
        MemoryChannel {
            tx: b_tx,
            rx: a_rx,
            name: "a".into(),
        },
        MemoryChannel {
            tx: a_tx,
            rx: b_rx,
            name: "b".into(),
        },
    )
}

#[async_trait::async_trait]
impl MessageChannel for MemoryChannel {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        self.tx
            .send(ChannelMsg::Text(text.to_string()))
            .map_err(|_| TransportError::Closed)
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.tx
            .send(ChannelMsg::Binary(data.to_vec()))
            .map_err(|_| TransportError::Closed)
    }
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        self.rx.recv().await.ok_or(TransportError::Closed)
    }
    fn peer(&self) -> String {
        self.name.clone()
    }
    fn split(self: Box<Self>) -> (Box<dyn MessageSender>, Box<dyn MessageReceiver>) {
        (
            Box::new(MemorySender { tx: self.tx }),
            Box::new(MemoryReceiver { rx: self.rx }),
        )
    }
}

struct MemorySender {
    tx: tokio::sync::mpsc::UnboundedSender<ChannelMsg>,
}

#[async_trait::async_trait]
impl MessageSender for MemorySender {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        self.tx
            .send(ChannelMsg::Text(text.to_string()))
            .map_err(|_| TransportError::Closed)
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.tx
            .send(ChannelMsg::Binary(data.to_vec()))
            .map_err(|_| TransportError::Closed)
    }
    fn peer(&self) -> String {
        "memory".into()
    }
}

struct MemoryReceiver {
    rx: tokio::sync::mpsc::UnboundedReceiver<ChannelMsg>,
}

#[async_trait::async_trait]
impl MessageReceiver for MemoryReceiver {
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError> {
        self.rx.recv().await.ok_or(TransportError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vr_proto::{InputEvent, MouseButton};

    fn cfg(id: &str, secret: &str, app: &str) -> SessionConfig {
        SessionConfig {
            session_id: id.into(),
            secret: secret.into(),
            app: app.into(),
            device: None,
            capabilities: Capabilities::default(),
        }
    }

    #[tokio::test]
    async fn handshake_and_control_roundtrip() {
        let (viewer_ch, agent_ch) = memory_channel_pair();
        let agent = tokio::spawn(async move {
            Session::accept(Box::new(agent_ch), cfg("s1", "123456", "vragent")).await
        });
        let mut viewer = Session::connect(Box::new(viewer_ch), cfg("s1", "123456", "viewer"))
            .await
            .expect("viewer handshake");
        let mut agent = agent.await.unwrap().expect("agent handshake");

        assert_eq!(agent.peer().role, Role::Viewer);
        assert_eq!(agent.peer().app, "viewer");

        viewer
            .send_control(Control::Input {
                event: InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: true,
                    x: 100,
                    y: 200,
                    display: 0,
                },
            })
            .await
            .unwrap();

        match agent.recv().await.unwrap() {
            Event::Control(Control::Input { event }) => match event {
                InputEvent::MouseButton { down, x, y, .. } => {
                    assert!(down);
                    assert_eq!((x, y), (100, 200));
                }
                other => panic!("unexpected event {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_pairing_code_is_rejected() {
        let (viewer_ch, agent_ch) = memory_channel_pair();
        let agent = tokio::spawn(async move {
            Session::accept(Box::new(agent_ch), cfg("s1", "123456", "vragent")).await
        });
        let viewer = Session::connect(Box::new(viewer_ch), cfg("s1", "999999", "viewer")).await;
        let agent = agent.await.unwrap();
        assert!(viewer.is_err(), "viewer must reject the agent proof");
        assert!(agent.is_err(), "agent must reject the viewer proof");
    }

    #[tokio::test]
    async fn ping_pong_measures_rtt_on_the_sender() {
        let (viewer_ch, agent_ch) = memory_channel_pair();
        let agent_task = tokio::spawn(async move {
            Session::accept(Box::new(agent_ch), cfg("s1", "code", "vragent")).await
        });
        let mut viewer = Session::connect(Box::new(viewer_ch), cfg("s1", "code", "viewer"))
            .await
            .unwrap();
        let mut agent = agent_task.await.unwrap().unwrap();

        let viewer_task = tokio::spawn(async move {
            // Answer pings forever; aborted by the test once it has its pong.
            loop {
                let _ = viewer.recv().await;
            }
        });
        agent.ping().await.unwrap();
        match tokio::time::timeout(Duration::from_secs(2), agent.recv())
            .await
            .expect("pong must arrive")
            .unwrap()
        {
            Event::Pong { ts } => assert!(ts > 0),
            other => panic!("expected pong, got {other:?}"),
        }
        viewer_task.abort();
    }

    #[tokio::test]
    async fn media_frames_keep_order_and_metadata() {
        let (viewer_ch, agent_ch) = memory_channel_pair();
        let viewer_task = tokio::spawn(async move {
            Session::connect(Box::new(viewer_ch), cfg("s1", "code", "viewer")).await
        });
        let mut agent = Session::accept(Box::new(agent_ch), cfg("s1", "code", "vragent"))
            .await
            .unwrap();
        let mut viewer = viewer_task.await.unwrap().unwrap();

        agent
            .send_media(vr_proto::Codec::Jpeg, 0, 640, 480, b"jpegdata")
            .await
            .unwrap();
        match viewer.recv().await.unwrap() {
            Event::Media { header, payload } => {
                assert_eq!(header.width, 640);
                assert_eq!(header.height, 480);
                assert_eq!(payload, b"jpegdata");
                assert_eq!(header.frame_id, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn split_halves_work_in_independent_tasks() {
        let (viewer_ch, agent_ch) = memory_channel_pair();
        let agent_task = tokio::spawn(async move {
            Session::accept(Box::new(agent_ch), cfg("s1", "code", "vragent")).await
        });
        let viewer = Session::connect(Box::new(viewer_ch), cfg("s1", "code", "viewer"))
            .await
            .unwrap();
        let agent = agent_task.await.unwrap().unwrap();

        let (mut v_send, mut v_recv, _peer) = viewer.into_parts();
        let (mut a_send, mut a_recv, _peer2) = agent.into_parts();

        // viewer -> agent media, on its own half
        for i in 0..3u32 {
            v_send
                .send_media(vr_proto::Codec::Jpeg, 0, 100, 100, &i.to_be_bytes())
                .await
                .unwrap();
        }
        for expected in 0..3u32 {
            match a_recv.recv().await.unwrap() {
                Event::Media { payload, .. } => {
                    assert_eq!(u32::from_be_bytes(payload.try_into().unwrap()), expected);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }

        // agent -> viewer pings, answered on the receiver's task
        let viewer_task = tokio::spawn(async move {
            let mut answered = 0;
            while answered < 2 {
                match v_recv.recv().await.unwrap() {
                    Event::Control(Control::Ping { ts }) => {
                        v_send.send_control(Control::Pong { ts }).await.unwrap();
                        answered += 1;
                    }
                    Event::Pong { .. } => {}
                    other => panic!("unexpected: {other:?}"),
                }
            }
            v_send
        });
        for _ in 0..2 {
            a_send.ping().await.unwrap();
            match a_recv.recv().await.unwrap() {
                Event::Pong { .. } => {}
                other => panic!("expected pong, got {other:?}"),
            }
        }
        let _ = viewer_task.await.unwrap();
    }
}
