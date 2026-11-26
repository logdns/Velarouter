//! Signal server client: registration, pairing refresh, invites and relay.
//!
//! The agent keeps one long-lived control WebSocket to the signal server. That
//! socket carries *no media*: it only receives invites and replies with accept
//! decisions. Once a session is accepted, both peers attach to a relay socket
//! and the encrypted VRP session runs there unchanged.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use vr_session::{Session, WsChannel};
use vr_transport::MessageChannel;

use crate::Identity;
use crate::{session_config, AgentState, AGENT_VERSION, PAIRING_REFRESH};

#[derive(Debug, Deserialize)]
pub struct RegisterResp {
    pub device_id: String,
    pub token: String,
    #[serde(default)]
    pub device_code: String,
}

#[derive(Debug, Deserialize)]
struct Invite {
    session_id: String,
    #[serde(default)]
    viewer: String,
}

pub struct SignalClient {
    base: String,
    http: reqwest::Client,
}

impl SignalClient {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn ws_url(&self, path: &str) -> String {
        let base = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.base.clone()
        };
        format!("{base}{path}")
    }

    pub async fn register(&self, state: &AgentState) -> Result<RegisterResp> {
        let resp = self
            .http
            .post(self.url("/v1/devices"))
            .json(&json!({
                "name": state.info.name,
                "platform": state.info.platform,
                "version": AGENT_VERSION,
                "tags": state.info.tags,
            }))
            .send()
            .await
            .context("signal server unreachable")?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn put_pairing(&self, device_id: &str, token: &str, code: &str) -> Result<()> {
        self.http
            .post(self.url(&format!("/v1/devices/{device_id}/pairing")))
            .bearer_auth(token)
            .json(&json!({ "code": code }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn accept(
        &self,
        session_id: &str,
        token: &str,
        lan_hints: &[String],
        udp_port: Option<u16>,
    ) -> Result<()> {
        self.http
            .post(self.url(&format!("/v1/sessions/{session_id}/accept")))
            .bearer_auth(token)
            .json(&json!({ "lan_hints": lan_hints, "udp_port": udp_port }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    fn relay_url(&self, session_id: &str, token: &str) -> String {
        self.ws_url(&format!(
            "/v1/session/ws?session_id={session_id}&token={token}&role=agent"
        ))
    }

    async fn connect_control(
        &self,
        token: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let mut req = self
            .ws_url("/v1/agent/ws")
            .into_client_request()
            .context("invalid signal url")?;
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .context("bad token header")?,
        );
        let (stream, _resp) = tokio_tungstenite::connect_async(req)
            .await
            .context("cannot open agent control channel")?;
        Ok(stream)
    }
}

/// Keeps the agent registered, refreshes its pairing code and handles invites.
pub async fn run_signal_loop(
    state: Arc<AgentState>,
    base: String,
    dir: PathBuf,
    identity: &mut Identity,
    events: Option<tokio::sync::mpsc::UnboundedSender<crate::AgentEvent>>,
) -> Result<()> {
    let client = SignalClient::new(&base);
    let mut backoff = Duration::from_secs(1);

    loop {
        // (Re)register when we have no token or the server forgot us.
        let mut registered = false;
        if !identity.token.is_empty() {
            match client
                .put_pairing(&identity.device_id, &identity.token, &state.secret)
                .await
            {
                Ok(()) => registered = true,
                Err(e) => {
                    warn!(%e, "stored device token rejected; registering again");
                }
            }
        }
        if !registered {
            let resp = client.register(&state).await?;
            identity.device_id = resp.device_id;
            identity.token = resp.token;
            identity.device_code = resp.device_code;
            crate::save_identity(&dir, identity)?;
            client
                .put_pairing(&identity.device_id, &identity.token, &state.secret)
                .await?;
        }
        info!(device_id = %identity.device_id, "registered with signal server");
        if let Some(tx) = &events {
            let _ = tx.send(crate::AgentEvent::Registered {
                device_code: identity.device_code.clone(),
                device_id: identity.device_id.clone(),
            });
        }
        println!();
        println!("  ── 互联网直连（无中继 / P2P）────────────────────────────");
        println!("  联机编码 / Connection ID : {}", identity.device_code);
        println!("  联机密码 / Password      : {}", state.secret);
        println!("  连接方式                  : TCP/UDP 打洞 + QUIC（失败即报错）");
        println!("  ─────────────────────────────────────────────────────────");
        println!();

        // Keep the code alive on the server side; it rotates only on restart.
        let refresh = {
            let client = SignalClient::new(&base);
            let device_id = identity.device_id.clone();
            let token = identity.token.clone();
            let code = state.secret.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(PAIRING_REFRESH).await;
                    if let Err(e) = client.put_pairing(&device_id, &token, &code).await {
                        debug!(%e, "pairing refresh failed");
                    }
                }
            })
        };

        match client.connect_control(&identity.token).await {
            Ok(mut stream) => {
                backoff = Duration::from_secs(1);
                info!("agent control channel connected");
                loop {
                    match stream.next().await {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                                continue;
                            };
                            match value.get("t").and_then(|t| t.as_str()) {
                                Some("ping") => {
                                    let _ = stream
                                        .send(Message::Text(json!({"t":"pong"}).to_string().into()))
                                        .await;
                                }
                                Some("rendezvous") => {
                                    if let Some(token) = value.get("token").and_then(|t| t.as_str())
                                    {
                                        let state = state.clone();
                                        let base = base.clone();
                                        let token = token.to_string();
                                        tokio::spawn(async move {
                                            if let Err(e) = accept_p2p(state, base, token).await {
                                                warn!(%e, "p2p rendezvous failed");
                                            }
                                        });
                                    }
                                }
                                Some("invite") => {
                                    if let Ok(invite) = serde_json::from_value::<Invite>(value) {
                                        let state = state.clone();
                                        let base = base.clone();
                                        let token = identity.token.clone();
                                        tokio::spawn(async move {
                                            if let Err(e) =
                                                handle_invite(state, base, token, invite).await
                                            {
                                                warn!(%e, "invite failed");
                                            }
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            warn!(%e, "control channel error");
                            break;
                        }
                    }
                }
            }
            Err(e) => warn!(%e, "cannot connect control channel"),
        }

        refresh.abort();
        warn!(?backoff, "reconnecting to signal server");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn handle_invite(
    state: Arc<AgentState>,
    base: String,
    token: String,
    invite: Invite,
) -> Result<()> {
    let client = SignalClient::new(&base);
    let hints: Vec<String> = vr_net::lan_hints(state.config.bind.port())
        .into_iter()
        .map(|h| h.addr)
        .collect();
    client
        .accept(&invite.session_id, &token, &hints, None)
        .await
        .context("accept rejected by signal server")?;
    info!(session = %invite.session_id, viewer = %invite.viewer, "session accepted");

    let url = client.relay_url(&invite.session_id, &token);
    let channel = WsChannel::connect_relay(&url, false).await?;
    let cfg = session_config(&state, &invite.session_id, state.secret.clone());
    let session: Session = Session::accept(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("relay handshake failed")?;
    info!(session = %invite.session_id, "relay session established");
    crate::server::run_session(session, state).await
}

/// Direct P2P accept: rendezvous → punch → QUIC → VRP handshake (no relay).
async fn accept_p2p(state: Arc<AgentState>, base: String, token: String) -> Result<()> {
    let signal_udp = udp_addr(&base, state.config.rendezvous_port).await?;
    accept_at(state, signal_udp, token).await
}

pub(super) async fn accept_at(
    state: Arc<AgentState>,
    signal_udp: std::net::SocketAddr,
    token: String,
) -> Result<()> {
    info!(%signal_udp, "p2p rendezvous start");
    let p2p = vr_p2p::prepare_agent(signal_udp, &token, Duration::from_secs(15)).await?;
    info!(peer = %p2p.peer, local_port = p2p.local_port, "punched; waiting for quic");
    let channel = tokio::time::timeout(Duration::from_secs(10), vr_p2p::accept(&p2p.endpoint))
        .await
        .context("quic accept timeout")??;
    // The rendezvous token doubles as the session id so both sides derive the
    // same HKDF salt.
    let cfg = session_config(&state, &token, state.secret.clone());
    let session = Session::accept(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("p2p VRP handshake failed (wrong password?)")?;
    info!(peer = %p2p.peer, "p2p session established (no relay)");
    crate::server::run_session(session, state).await
}

pub(super) async fn udp_addr(base: &str, port: u16) -> Result<std::net::SocketAddr> {
    let url = url::Url::parse(base).context("invalid signal url")?;
    let host = url.host_str().context("signal url has no host")?;
    let mut addrs = tokio::net::lookup_host((host, port)).await?;
    addrs
        .find(|a| a.is_ipv4())
        .context("no IPv4 rendezvous address")
}
