//! The Velarouter signal server.
//!
//! Responsibilities, deliberately narrow:
//!
//! * **Device registry** — who exists, how to reach them, no screen data.
//! * **Pairing** — a device publishes a short-lived 6-digit code; a viewer must
//!   present it to open a session. Unattended devices may pin a long-term secret
//!   instead.
//! * **Session brokering** — invites travel over the agent's control WebSocket,
//!   the viewer polls/streams the same session by id.
//! * **Encrypted relay** — when a direct path is impossible, both peers connect
//!   to the same relay socket and the server copies opaque frames between them.
//!
//! The server never holds a session key. It cannot read a frame, a file name or
//! an input event, which is what lets us run a relay for other people's traffic.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tower_http::cors::CorsLayer;
use tracing::{debug, info, warn};
use vr_proto::DeviceInfo;

/// A VRP payload tunnelled through the relay: `0x01` text, `0x02` binary.
pub const RELAY_TEXT: u8 = 0x01;
pub const RELAY_BINARY: u8 = 0x02;

#[derive(Debug, Clone)]
pub struct SignalConfig {
    pub bind: SocketAddr,
    /// URL clients should use for this server, e.g. `wss://signal.example.com`.
    pub public_url: String,
    /// Port of the UDP reflex service; `0` disables it.
    pub reflex_port: u16,
    /// UDP port of the rendezvous service; `0` disables it.
    pub rendezvous_port: u16,
    /// Optional bearer token required for `GET /v1/devices` and `DELETE`.
    pub admin_token: Option<String>,
    /// How long an unused pairing code lives.
    pub pairing_ttl: Duration,
    /// How long an unaccepted session lives.
    pub session_ttl: Duration,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], vr_proto::DEFAULT_SIGNAL_PORT)),
            public_url: format!("http://127.0.0.1:{}", vr_proto::DEFAULT_SIGNAL_PORT),
            reflex_port: vr_proto::DEFAULT_REFLEX_PORT,
            rendezvous_port: vr_proto::DEFAULT_RENDEZVOUS_PORT,
            admin_token: None,
            pairing_ttl: Duration::from_secs(300),
            session_ttl: Duration::from_secs(120),
        }
    }
}

#[derive(Debug)]
struct Device {
    info: DeviceInfo,
    /// Human-typed connection code, unique, 9 digits.
    device_code: String,
    token: String,
    pairing: Option<(String, Instant)>,
    ws: Option<mpsc::UnboundedSender<String>>,
    last_seen: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Pending,
    Accepted,
    Active,
    Closed,
}

#[derive(Debug)]
struct Session {
    id: String,
    device_id: String,
    token: String,
    viewer_name: String,
    state: SessionState,
    created: Instant,
    lan_hints: Vec<String>,
    udp_port: Option<u16>,
    viewer_tx: Option<mpsc::UnboundedSender<RelayMsg>>,
    agent_tx: Option<mpsc::UnboundedSender<RelayMsg>>,
}

#[derive(Debug, Clone)]
enum RelayMsg {
    Binary(Vec<u8>),
    Control(String),
    Close,
}

#[derive(Debug, Default)]
struct RendezvousEntry {
    agent: Option<SocketAddr>,
    viewer: Option<SocketAddr>,
    created: Option<Instant>,
}

struct AppState {
    cfg: SignalConfig,
    devices: RwLock<HashMap<String, Device>>,
    // Ephemeral desktop codes live only as long as the owning control socket.
    online: RwLock<HashMap<String, mpsc::Sender<String>>>,
    sessions: RwLock<HashMap<String, Session>>,
    rendezvous: RwLock<HashMap<String, RendezvousEntry>>,
    started: Instant,
}

/// Starts the HTTP/WebSocket server and the UDP reflex responder.
pub async fn serve(cfg: SignalConfig) -> anyhow::Result<()> {
    if cfg.reflex_port != 0 {
        let port = vr_net::serve_reflex(cfg.reflex_port).await?;
        info!(port, "udp reflex listening");
    }
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        devices: RwLock::new(HashMap::new()),
        online: Default::default(),
        sessions: RwLock::new(HashMap::new()),
        rendezvous: RwLock::new(HashMap::new()),
        started: Instant::now(),
    });

    // Expire stale pairing codes and sessions.
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let now = Instant::now();
                let pairing_ttl = state.cfg.pairing_ttl;
                let session_ttl = state.cfg.session_ttl;
                {
                    let mut devices = state.devices.write().await;
                    for d in devices.values_mut() {
                        if let Some((_, exp)) = &d.pairing {
                            if now.duration_since(*exp) > pairing_ttl {
                                d.pairing = None;
                            }
                        }
                    }
                }
                state.rendezvous.write().await.retain(|_, v| {
                    v.created
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
                });
                let mut sessions = state.sessions.write().await;
                sessions.retain(|_, s| {
                    s.state == SessionState::Active || now.duration_since(s.created) < session_ttl
                });
            }
        });
    }

    if cfg.rendezvous_port != 0 {
        let st = state.clone();
        let port = cfg.rendezvous_port;
        let udp = tokio::net::UdpSocket::bind(("0.0.0.0", port)).await?;
        tokio::spawn(async move {
            if let Err(e) = serve_rendezvous(udp, st).await {
                warn!(%e, "rendezvous service stopped");
            }
        });
    }

    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/devices", post(register_device).get(list_devices))
        .route("/v1/devices/{id}/pairing", post(put_pairing))
        .route("/v1/devices/{id}", delete(delete_device))
        .route("/v1/rendezvous", post(create_rendezvous))
        .route("/v1/online/{code}", get(online_ws))
        .route("/v1/sessions", post(create_session))
        .route(
            "/v1/sessions/{id}",
            get(session_status).delete(close_session),
        )
        .route("/v1/sessions/{id}/accept", post(accept_session))
        .route("/v1/agent/ws", get(agent_ws))
        .route("/v1/session/ws", get(session_ws))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    info!(addr = %cfg.bind, public = %cfg.public_url, "signal server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
}

fn random_token() -> String {
    use rand::Rng;
    let mut buf = [0u8; 24];
    rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs(instant: Instant) -> u64 {
    instant.elapsed().as_secs()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let devices = state.devices.read().await.len();
    let sessions = state.sessions.read().await.len();
    Json(serde_json::json!({
        "ok": true,
        "version": vr_proto::PROTOCOL_VERSION,
        "uptime_secs": now_secs(state.started),
        "devices": devices,
        "sessions": sessions,
    }))
}

#[derive(Debug, Deserialize)]
struct RegisterReq {
    name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RegisterResp {
    device_id: String,
    token: String,
    /// 9-digit connection code the user types on the viewer.
    device_code: String,
}

async fn register_device(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterReq>,
) -> Result<Json<RegisterResp>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("device name is required"));
    }
    let device_id = uuid::Uuid::new_v4().to_string();
    let token = random_token();
    let device_code = loop {
        let code = format!("{:09}", rand::random::<u32>() % 1_000_000_000);
        if !state
            .devices
            .read()
            .await
            .values()
            .any(|d| d.device_code == code)
        {
            break code;
        }
    };
    let info = DeviceInfo {
        device_id: device_id.clone(),
        name: req.name.trim().to_string(),
        platform: req.platform,
        version: req.version,
        tags: req.tags,
    };
    state.devices.write().await.insert(
        device_id.clone(),
        Device {
            info,
            device_code: device_code.clone(),
            token: token.clone(),
            pairing: None,
            ws: None,
            last_seen: Instant::now(),
        },
    );
    info!(%device_id, "device registered");
    Ok(Json(RegisterResp {
        device_id,
        token,
        device_code,
    }))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceInfo>>, ApiError> {
    let devices = state.devices.read().await;
    let is_admin = match &state.cfg.admin_token {
        Some(t) => bearer(&headers).as_deref() == Some(t.as_str()),
        None => true,
    };
    if !is_admin {
        // A non-admin caller may still be a registered device.
        let authed = bearer(&headers)
            .map(|t| devices.values().any(|d| d.token == t))
            .unwrap_or(false);
        if !authed {
            return Err(ApiError::unauthorized("admin token required"));
        }
    }
    Ok(Json(devices.values().map(|d| d.info.clone()).collect()))
}

#[derive(Debug, Deserialize)]
struct PairingReq {
    code: String,
}

async fn put_pairing(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PairingReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.code.len() < 4 || req.code.len() > 128 {
        return Err(ApiError::bad_request("pairing code must be 4..128 chars"));
    }
    let mut devices = state.devices.write().await;
    let device = devices
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("unknown device"))?;
    if bearer(&headers).as_deref() != Some(device.token.as_str()) {
        return Err(ApiError::unauthorized("device token required"));
    }
    device.pairing = Some((req.code.clone(), Instant::now()));
    device.last_seen = Instant::now();
    Ok(Json(serde_json::json!({
        "ok": true,
        "ttl_secs": state.cfg.pairing_ttl.as_secs(),
    })))
}

async fn delete_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut devices = state.devices.write().await;
    let device = devices
        .get(&id)
        .ok_or_else(|| ApiError::not_found("unknown device"))?;
    let allowed = bearer(&headers).as_deref() == Some(device.token.as_str())
        || state
            .cfg
            .admin_token
            .as_deref()
            .map(|t| bearer(&headers).as_deref() == Some(t))
            .unwrap_or(false);
    if !allowed {
        return Err(ApiError::unauthorized("device or admin token required"));
    }
    devices.remove(&id);
    Ok(Json(serde_json::json!({"ok": true})))
}

#[derive(Debug, Deserialize)]
struct CreateSessionReq {
    device_id: String,
    #[serde(default)]
    pairing_code: String,
    #[serde(default)]
    viewer_name: String,
}

#[derive(Debug, Serialize)]
struct CreateSessionResp {
    session_id: String,
    token: String,
    /// WebSocket path the viewer should connect to (relative to the signal URL).
    ws_path: String,
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSessionReq>,
) -> Result<Json<CreateSessionResp>, ApiError> {
    let devices = state.devices.read().await;
    let device = devices
        .get(&req.device_id)
        .ok_or_else(|| ApiError::not_found("unknown device"))?;

    let pinned_ok = match &device.pairing {
        Some((code, exp)) => {
            let fresh = Instant::now().duration_since(*exp) <= state.cfg.pairing_ttl;
            fresh && constant_time_eq(code, &req.pairing_code)
        }
        None => false,
    };
    if !pinned_ok {
        return Err(ApiError::forbidden("pairing code is missing or expired"));
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let token = random_token();
    let mut sessions = state.sessions.write().await;
    sessions.insert(
        session_id.clone(),
        Session {
            id: session_id.clone(),
            device_id: req.device_id.clone(),
            token: token.clone(),
            viewer_name: req.viewer_name.clone(),
            state: SessionState::Pending,
            created: Instant::now(),
            lan_hints: Vec::new(),
            udp_port: None,
            viewer_tx: None,
            agent_tx: None,
        },
    );
    if let Some(tx) = &device.ws {
        let invite = serde_json::json!({
            "t": "invite",
            "session_id": session_id,
            "viewer": req.viewer_name,
        });
        let _ = tx.send(invite.to_string());
    }
    info!(%session_id, device = %req.device_id, "session created");
    Ok(Json(CreateSessionResp {
        session_id: session_id.clone(),
        token,
        ws_path: format!("/v1/session/ws?session_id={session_id}&token=...&role=viewer"),
    }))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[derive(Debug, Deserialize)]
struct AcceptReq {
    #[serde(default)]
    lan_hints: Vec<String>,
    #[serde(default)]
    udp_port: Option<u16>,
}

async fn accept_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<AcceptReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut sessions = state.sessions.write().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("unknown session"))?;
    let device_token = {
        let devices = state.devices.read().await;
        devices
            .get(&session.device_id)
            .map(|d| d.token.clone())
            .ok_or_else(|| ApiError::not_found("device vanished"))?
    };
    if bearer(&headers).as_deref() != Some(device_token.as_str()) {
        return Err(ApiError::unauthorized("device token required"));
    }
    session.state = SessionState::Accepted;
    session.lan_hints = req.lan_hints;
    session.udp_port = req.udp_port;
    if let Some(tx) = &session.viewer_tx {
        let ready = serde_json::json!({
            "t": "ready",
            "lan_hints": session.lan_hints,
            "udp_port": session.udp_port,
        });
        let _ = tx.send(RelayMsg::Control(ready.to_string()));
    }
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn session_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| ApiError::not_found("unknown session"))?;
    if bearer(&headers).as_deref() != Some(session.token.as_str()) {
        return Err(ApiError::unauthorized("session token required"));
    }
    Ok(Json(serde_json::json!({
        "session_id": session.id,
        "state": format!("{:?}", session.state).to_lowercase(),
        "viewer": session.viewer_name,
        "lan_hints": session.lan_hints,
        "udp_port": session.udp_port,
        "age_secs": session.created.elapsed().as_secs(),
    })))
}

async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut sessions = state.sessions.write().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("unknown session"))?;
    if bearer(&headers).as_deref() != Some(session.token.as_str()) {
        return Err(ApiError::unauthorized("session token required"));
    }
    session.state = SessionState::Closed;
    if let Some(tx) = session.viewer_tx.take() {
        let _ = tx.send(RelayMsg::Close);
    }
    if let Some(tx) = session.agent_tx.take() {
        let _ = tx.send(RelayMsg::Close);
    }
    sessions.remove(&id);
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn agent_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = bearer(&headers).ok_or_else(|| ApiError::unauthorized("device token required"))?;
    let device_id = {
        let devices = state.devices.read().await;
        devices
            .iter()
            .find(|(_, d)| d.token == token)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| ApiError::unauthorized("unknown device token"))?
    };
    Ok(ws.on_upgrade(move |socket| agent_loop(socket, state, device_id)))
}

async fn agent_loop(socket: WebSocket, state: Arc<AppState>, device_id: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    {
        let mut devices = state.devices.write().await;
        if let Some(d) = devices.get_mut(&device_id) {
            d.ws = Some(tx);
            d.last_seen = Instant::now();
        }
    }
    info!(%device_id, "agent control channel open");

    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                if sink.send(Message::Text(msg.into())).await.is_err() { break; }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        debug!(%device_id, %t, "agent control message");
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                            if v.get("t").and_then(|t| t.as_str()) == Some("ping") {
                                let _ = sink.send(Message::Text(
                                    serde_json::json!({"t":"pong"}).to_string().into()
                                )).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { warn!(%device_id, %e, "agent ws error"); break; }
                }
            }
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let mut devices = state.devices.write().await;
    if let Some(d) = devices.get_mut(&device_id) {
        d.ws = None;
    }
    info!(%device_id, "agent control channel closed");
}

#[derive(Debug, Deserialize)]
struct RelayQuery {
    session_id: String,
    token: String,
    role: String,
}

async fn session_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(q): Query<RelayQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if q.role != "viewer" && q.role != "agent" {
        return Err(ApiError::bad_request("role must be viewer or agent"));
    }
    {
        let sessions = state.sessions.read().await;
        let session = sessions
            .get(&q.session_id)
            .ok_or_else(|| ApiError::not_found("unknown session"))?;
        let authorized = if q.role == "agent" {
            // The agent authenticates with its long-lived device token and must
            // own the session it tries to attach to.
            let devices = state.devices.read().await;
            devices
                .get(&session.device_id)
                .map(|d| d.token == q.token)
                .unwrap_or(false)
        } else {
            // The viewer authenticates with the single-use session token.
            session.token == q.token
        };
        if !authorized {
            return Err(ApiError::unauthorized("bad token for role"));
        }
    }
    Ok(ws.on_upgrade(move |socket| relay_loop(socket, state, q)))
}

/// Copies opaque frames between the two peers of a session.
async fn relay_loop(socket: WebSocket, state: Arc<AppState>, q: RelayQuery) {
    let role = q.role.clone();
    let session_id = q.session_id.clone();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<RelayMsg>();

    // Register this peer and tell it whether the other side is already here.
    let peer_ready = {
        let mut sessions = state.sessions.write().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            return;
        };
        if role == "viewer" {
            session.viewer_tx = Some(tx.clone());
            if session.state == SessionState::Accepted {
                let ready = serde_json::json!({
                    "t": "ready",
                    "lan_hints": session.lan_hints,
                    "udp_port": session.udp_port,
                });
                let _ = tx.send(RelayMsg::Control(ready.to_string()));
            } else {
                let _ = tx.send(RelayMsg::Control(
                    serde_json::json!({"t":"waiting"}).to_string(),
                ));
            }
        } else {
            session.agent_tx = Some(tx.clone());
        }
        if session.viewer_tx.is_some() && session.agent_tx.is_some() {
            session.state = SessionState::Active;
            true
        } else {
            false
        }
    };

    if peer_ready {
        notify(&state, &session_id, r#"{"t":"peer_joined"}"#).await;
    }
    info!(%session_id, %role, "relay attached");

    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                let sent = match msg {
                    RelayMsg::Binary(b) => sink.send(Message::Binary(b.into())).await,
                    RelayMsg::Control(c) => sink.send(Message::Text(c.into())).await,
                    RelayMsg::Close => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                };
                if sent.is_err() { break; }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(_))) => {
                        // Text frames are relay control only. VRP frames always
                        // travel prefixed inside binary frames, so a text frame
                        // can never be mistaken for protocol payload.
                    }
                    Some(Ok(Message::Binary(b))) => {
                        // Opaque: the server cannot tell video from input.
                        forward(&state, &session_id, &role, RelayMsg::Binary(b.to_vec())).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { debug!(%e, "relay socket error"); break; }
                }
            }
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }

    let mut sessions = state.sessions.write().await;
    if let Some(session) = sessions.get_mut(&session_id) {
        if role == "viewer" {
            session.viewer_tx = None;
        } else {
            session.agent_tx = None;
        }
        if session.state == SessionState::Active {
            session.state = SessionState::Closed;
        }
    }
    drop(sessions);
    notify(&state, &session_id, r#"{"t":"peer_left"}"#).await;
    info!(%session_id, %role, "relay detached");
}

async fn forward(state: &Arc<AppState>, session_id: &str, from: &str, msg: RelayMsg) {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(session_id) {
        let target = if from == "viewer" {
            session.agent_tx.as_ref()
        } else {
            session.viewer_tx.as_ref()
        };
        if let Some(tx) = target {
            let _ = tx.send(msg);
        }
    }
}

async fn notify(state: &Arc<AppState>, session_id: &str, control: &str) {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(session_id) {
        for tx in [session.viewer_tx.as_ref(), session.agent_tx.as_ref()]
            .into_iter()
            .flatten()
        {
            let _ = tx.send(RelayMsg::Control(control.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: msg.into(),
        }
    }
    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: msg.into(),
        }
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: msg.into(),
        }
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        }));
        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_unique() {
        let a = random_token();
        let b = random_token();
        assert!(a.len() >= 40);
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_compare_matches_semantics() {
        assert!(constant_time_eq("123456", "123456"));
        assert!(!constant_time_eq("123456", "123457"));
        assert!(!constant_time_eq("123456", "12345"));
    }

    #[test]
    fn relay_prefixes_are_stable() {
        // Documented contract for SDK implementers.
        assert_eq!(RELAY_TEXT, 1);
        assert_eq!(RELAY_BINARY, 2);
    }
}

// ---------------------------------------------------------------------------
// Rendezvous (P2P, no relay)
// ---------------------------------------------------------------------------

/// A desktop claims its freshly generated code atomically after upgrade. No password is sent.
async fn online_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if !vr_proto::is_internet_code(&code) || state.cfg.rendezvous_port == 0 {
        return Err(ApiError::bad_request("invalid code or rendezvous disabled"));
    }
    Ok(ws.max_message_size(2048).on_upgrade(move |mut socket| async move {
        let (tx,mut rx)=mpsc::channel(4);
        let claimed={let mut codes=state.online.write().await; if let std::collections::hash_map::Entry::Vacant(slot)=codes.entry(code.clone()){slot.insert(tx);true}else{false}};
        if !claimed {let _=socket.send(Message::Text(r#"{"t":"conflict"}"#.into())).await;return;}
        let _=async {
            socket.send(Message::Text(serde_json::json!({"t":"ready","udp_port":state.cfg.rendezvous_port}).to_string().into())).await?;
            let mut tick=tokio::time::interval(Duration::from_secs(15)); let mut seen=Instant::now();
            loop {tokio::select! {
                Some(token)=rx.recv()=>socket.send(Message::Text(serde_json::json!({"t":"rendezvous","token":token}).to_string().into())).await?,
                msg=socket.recv()=>match msg {Some(Ok(Message::Pong(_)))=>seen=Instant::now(),Some(Ok(Message::Ping(p)))=>socket.send(Message::Pong(p)).await?,_=>break},
                _=tick.tick()=>{if seen.elapsed()>Duration::from_secs(45){break;}socket.send(Message::Ping(Vec::new().into())).await?;}
            }} Ok::<(),axum::Error>(())
        }.await;
        state.online.write().await.remove(&code);
    }))
}

#[derive(Debug, Deserialize)]
struct RendezvousReq {
    device_code: String,
}

/// Creates a short-lived rendezvous token for a viewer.
///
/// The password is **not** part of this request: the server only introduces the
/// two UDP endpoints. Authentication happens inside the encrypted VRP handshake,
/// so a compromised signal server still cannot read or impersonate a session.
async fn create_rendezvous(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RendezvousReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if vr_proto::is_internet_code(&req.device_code) {
        let target = state
            .online
            .read()
            .await
            .get(&req.device_code)
            .cloned()
            .ok_or_else(|| ApiError::not_found("device offline"))?;
        let token = random_token();
        let mut pending = state.rendezvous.write().await;
        target
            .try_send(token.clone())
            .map_err(|_| ApiError::bad_request("device offline or busy"))?;
        pending.insert(
            token.clone(),
            RendezvousEntry {
                created: Some(Instant::now()),
                ..Default::default()
            },
        );
        return Ok(Json(
            serde_json::json!({"token":token,"udp_port":state.cfg.rendezvous_port,"ttl_secs":60}),
        ));
    }
    let (device_id, ws) = {
        let devices = state.devices.read().await;
        let device = devices
            .values()
            .find(|d| d.device_code == req.device_code)
            .ok_or_else(|| ApiError::not_found("unknown connection code"))?;
        (device.info.device_id.clone(), device.ws.clone())
    };
    let token = random_token();
    state.rendezvous.write().await.insert(
        token.clone(),
        RendezvousEntry {
            created: Some(Instant::now()),
            ..Default::default()
        },
    );
    if let Some(tx) = ws {
        let _ = tx.send(
            serde_json::json!({"t":"rendezvous","token": token, "device_id": device_id})
                .to_string(),
        );
    } else {
        state.rendezvous.write().await.remove(&token);
        return Err(ApiError::bad_request("device is offline"));
    }
    Ok(Json(serde_json::json!({
        "token": token,
        "udp_port": state.cfg.rendezvous_port,
        "ttl_secs": 60,
    })))
}

/// UDP side of the rendezvous: observes each peer's public address and returns
/// the other peer's address so both can punch at the same time.
async fn serve_rendezvous(
    socket: tokio::net::UdpSocket,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    info!(addr=%socket.local_addr()?, "udp rendezvous listening");
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, from)) = socket.recv_from(&mut buf).await else {
            break;
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        let mut parts = text.split_whitespace();
        if parts.next() != Some("VRRG1") {
            continue;
        }
        let Some(role) = parts.next() else { continue };
        let Some(token) = parts.next() else { continue };
        let is_agent = role == "A";
        let reply = {
            let mut map = state.rendezvous.write().await;
            let now = Instant::now();
            map.retain(|_, e| {
                e.created
                    .map(|t| now.duration_since(t) < Duration::from_secs(120))
                    .unwrap_or(false)
            });
            let Some(entry) = map.get_mut(token) else {
                continue;
            };
            if is_agent {
                entry.agent = Some(from);
            } else {
                entry.viewer = Some(from);
            }
            match (entry.agent, entry.viewer) {
                // Each side must receive the *other* side's observed address.
                (Some(agent), Some(viewer)) => {
                    let peer = if is_agent { viewer } else { agent };
                    format!("VRRG1P {peer}")
                }
                _ => "VRRG1W".to_string(),
            }
        };
        let _ = socket.send_to(reply.as_bytes(), from).await;
    }
    Ok(())
}
