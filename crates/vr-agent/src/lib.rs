//! Velarouter agent ("被控端").
//!
//! Responsibilities:
//!
//! * capture a display and encode it,
//! * inject the viewer's mouse/keyboard,
//! * listen on the LAN so a viewer can connect without any server,
//! * optionally stay reachable through the signal relay for WAN sessions,
//! * show a pairing code that the viewer must type — the code never leaves the
//!   device over the network.
//!
//! Everything a viewer can see or do is gated by [`vr_session`]; this crate only
//! implements the two platform-specific ends: pixels out, events in.

pub mod capture;
pub mod files;
pub mod input;
pub mod meet;
pub mod nat;
pub mod online;
pub mod server;
pub mod signal;
pub mod transfer;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use vr_proto::{Capabilities, Codec, DeviceInfo, DisplayInfo};

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runtime notifications for embedders (the desktop app uses these to show the
/// connection code and LAN status without parsing stdout).
#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionJoined,
    SessionLeft,
    #[cfg(target_os = "linux")]
    InteractionStatus(String),
    Registered {
        device_code: String,
        device_id: String,
    },
    LanListening {
        port: u16,
        ipv6: bool,
    },
    SignalUnavailable(String),
    /// The public rendezvous channel accepted us: the code can now be shared.
    InternetReady {
        code: String,
    },
    /// The rendezvous channel is down; the code on screen is not reachable yet.
    InternetFailed(String),
    HostingFailed(String),
}

fn dual_listener(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?;
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), port).into())?;
    socket.listen(128)?;
    tokio::net::TcpListener::from_std(socket.into())
}

/// Runtime configuration of the agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub bind: SocketAddr,
    pub signal_url: Option<String>,
    /// Desktop presence: operator-owned directory and ephemeral local connection ID.
    pub online: Option<(String, String)>,
    /// `None` generates a new 6-digit code on every start.
    pub fixed_code: Option<String>,
    pub quality: u8,
    pub fps: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub display: u32,
    pub state_dir: Option<PathBuf>,
    pub discoverable: bool,
    /// Directory the viewer may browse and transfer. Never chosen by the peer.
    pub files_root: PathBuf,
    /// UDP port of the signal server's rendezvous service.
    pub rendezvous_port: u16,
    /// Internet rendezvous over a broker: `(connection code, broker urls)`.
    /// Empty broker list means the public defaults.
    pub meet: Option<(String, Vec<String>)>,
    /// Optional shutdown signal for embedders (the desktop app toggles hosting).
    pub shutdown: Option<tokio::sync::watch::Receiver<bool>>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: default_device_name(),
            bind: SocketAddr::from(([0, 0, 0, 0], vr_proto::DEFAULT_AGENT_PORT)),
            signal_url: None,
            online: None,
            fixed_code: None,
            quality: 70,
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            display: 0,
            state_dir: None,
            discoverable: true,
            files_root: default_files_root(),
            rendezvous_port: vr_proto::DEFAULT_RENDEZVOUS_PORT,
            meet: None,
            shutdown: None,
        }
    }
}

/// Live settings shared with the capture thread. Atomics keep the frame loop
/// lock-free: a viewer changing quality must never block a frame.
#[derive(Debug)]
pub struct StreamControl {
    pub quality: AtomicU8,
    pub display: AtomicU32,
    pub paused: AtomicU8,
    pub stop: AtomicU8,
    /// Bumped when the viewer changes resolution or frame rate: the capture
    /// thread for the previous geometry must exit instead of fighting for frames.
    pub generation: AtomicU32,
}

impl StreamControl {
    pub fn new(quality: u8, display: u32) -> Self {
        Self {
            quality: AtomicU8::new(quality),
            display: AtomicU32::new(display),
            paused: AtomicU8::new(0),
            stop: AtomicU8::new(0),
            generation: AtomicU32::new(0),
        }
    }
    pub fn quality(&self) -> u8 {
        self.quality.load(Ordering::Relaxed)
    }
    pub fn display(&self) -> u32 {
        self.display.load(Ordering::Relaxed)
    }
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed) != 0
    }
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(u8::from(paused), Ordering::Relaxed);
    }
}

/// State shared by the LAN server, the relay client and every session.
#[cfg(target_os = "linux")]
pub mod portal_input;

pub struct AgentState {
    pub events: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    #[cfg(target_os = "linux")]
    pub portal: Option<portal_input::Permission>,
    pub info: DeviceInfo,
    pub secret: String,
    pub config: AgentConfig,
    pub displays: Vec<DisplayInfo>,
    pub caps: Capabilities,
    pub files: files::FileService,
}

/// Persistent device identity, stored next to the config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub device_id: String,
    pub token: String,
    /// 9-digit code the user types on the viewer. Assigned by the signal server.
    #[serde(default)]
    pub device_code: String,
}

/// Platform-appropriate config directory.
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VELAROUTER_STATE_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Velarouter");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("Velarouter");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("velarouter");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("velarouter");
    }
    PathBuf::from(".velarouter")
}

/// Uploads and downloads are confined to this directory.
pub fn default_files_root() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Velarouter");
    }
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile).join("Velarouter");
    }
    std::env::temp_dir().join("velarouter-files")
}

pub fn default_device_name() -> String {
    hostname().unwrap_or_else(|| "Velarouter device".to_string())
}

fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").ok()
    }
}

pub fn platform_name() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn load_or_create_identity(dir: &PathBuf) -> Result<Identity> {
    std::fs::create_dir_all(dir).ok();
    let path = dir.join("identity.json");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(id) = serde_json::from_slice::<Identity>(&bytes) {
            return Ok(id);
        }
    }
    let identity = Identity {
        device_id: uuid::Uuid::new_v4().to_string(),
        token: String::new(),
        device_code: String::new(),
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&identity)?)?;
    Ok(identity)
}

pub(crate) fn save_identity(dir: &PathBuf, identity: &Identity) -> Result<()> {
    std::fs::create_dir_all(dir).ok();
    std::fs::write(
        dir.join("identity.json"),
        serde_json::to_vec_pretty(identity)?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            dir.join("identity.json"),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    Ok(())
}

pub fn agent_capabilities() -> Capabilities {
    Capabilities {
        codecs: vec![Codec::Jpeg],
        max_width: 3840,
        max_height: 2160,
        max_fps: 60,
        input: true,
        clipboard: true,
        multi_display: true,
        audio: false,
    }
}

fn list_displays() -> Vec<DisplayInfo> {
    match xcap::Monitor::all() {
        Ok(monitors) => monitors
            .iter()
            .filter_map(|m| {
                Some(DisplayInfo {
                    id: m.id().ok()?,
                    name: m.name().ok().unwrap_or_else(|| "Display".into()),
                    width: m.width().ok()?,
                    height: m.height().ok()?,
                    scale: m.scale_factor().ok().unwrap_or(1.0),
                    primary: m.is_primary().ok().unwrap_or(false),
                })
            })
            .collect(),
        Err(e) => {
            warn!(%e, "cannot enumerate displays");
            Vec::new()
        }
    }
}

/// Runs the agent until the process is stopped.
pub async fn run(config: AgentConfig) -> Result<()> {
    run_with_events(config, None).await
}

/// Same as [`run`], but reports registration/LAN events to an optional channel.
pub async fn run_with_events(
    config: AgentConfig,
    events: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
) -> Result<()> {
    let dir = config.state_dir.clone().unwrap_or_else(state_dir);
    let identity = load_or_create_identity(&dir)?;
    let secret = config
        .fixed_code
        .clone()
        .unwrap_or_else(vr_crypto::generate_pairing_code);

    let displays = list_displays();
    if displays.is_empty() {
        warn!("no displays detected; capture will fail until a display is available");
    }

    let info = DeviceInfo {
        device_id: identity.device_id.clone(),
        name: config.name.clone(),
        platform: platform_name(),
        version: AGENT_VERSION.to_string(),
        tags: vec!["agent".into()],
    };
    let files = files::FileService::new(&config.files_root)?;
    let state = Arc::new(AgentState {
        events: events.clone(),
        #[cfg(target_os = "linux")]
        portal: portal_input::is_wayland()
            .then(|| portal_input::request(events.clone(), config.shutdown.clone())),
        info: info.clone(),
        secret: secret.clone(),
        config: config.clone(),
        displays: displays.clone(),
        caps: agent_capabilities(),
        files,
    });

    // LAN server first: it makes the agent usable without any cloud component.
    // Desktop wildcard hosting: prefer one dual-stack listener (same password and
    // shutdown path for both families). Never broaden an explicit CLI bind.
    let dual = if events.is_some() && config.bind.ip() == std::net::Ipv4Addr::UNSPECIFIED {
        dual_listener(config.bind.port()).ok()
    } else {
        None
    };
    let ipv6 = dual.is_some() || config.bind.is_ipv6();
    let listener = match dual {
        Some(listener) => listener,
        None => tokio::net::TcpListener::bind(config.bind).await?,
    };
    let port = listener.local_addr()?.port();
    let mut server = tokio::spawn(server::serve(state.clone(), listener));
    if let Some(tx) = &events {
        let _ = tx.send(AgentEvent::LanListening { port, ipv6 });
    }

    let discovery_task = if config.discoverable {
        let discovery_info = info.clone();
        Some(tokio::spawn(async move {
            if let Err(e) =
                vr_net::serve_discovery(vr_proto::DEFAULT_AGENT_PORT, discovery_info).await
            {
                tracing::debug!(%e, "discovery responder stopped");
            }
        }))
    } else {
        None
    };
    let embedded = events.is_some();
    let online_task = config
        .online
        .clone()
        .zip(events.clone())
        .map(|((base, code), events)| tokio::spawn(online::run(state.clone(), base, code, events)));

    // Internet rendezvous without a server of our own.
    let meet_task = config.meet.clone().map(|(code, brokers)| {
        meet::spawn(
            state.clone(),
            code,
            brokers,
            events.clone().unwrap_or_else(|| {
                let (tx, _) = tokio::sync::mpsc::unbounded_channel();
                tx
            }),
            config.shutdown.clone(),
        )
    });

    // Optional WAN reachability.
    let signal_task = if let Some(base) = config.signal_url.clone() {
        let state = state.clone();
        let dir = dir.clone();
        let mut signal_identity = identity.clone();
        Some(tokio::spawn(async move {
            if let Err(e) =
                signal::run_signal_loop(state, base, dir, &mut signal_identity, events.clone())
                    .await
            {
                warn!(%e, "signal loop stopped");
                if let Some(tx) = &events {
                    let _ = tx.send(AgentEvent::SignalUnavailable(e.to_string()));
                }
            }
        }))
    } else {
        None
    };

    println!();
    println!("  Velarouter agent «{}» is running", info.name);
    if !embedded {
        println!("  pairing code : {}   (type this on the viewer)", secret);
    }
    println!("  lan          : ws://<this-ip>:{}", config.bind.port());
    if let Some((code, _)) = &config.meet {
        println!("  internet code: {code}   (share this with the viewer)");
    } else {
        println!("  internet code: disabled (LAN only)");
    }
    if let Some(url) = &config.signal_url {
        println!("  signal       : {url}");
        println!("  device id    : {}", info.device_id);
    }
    println!("  files        : {}", state.files.root().display());
    println!("  stop         : Ctrl+C");
    println!();

    let mut shutdown = config.shutdown.clone();
    let server_finished = tokio::select! {
        r = &mut server => {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!(%e, "lan server exited"),
                Err(e) => warn!(%e, "lan server task failed"),
            }
            true
        }
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");false
        }
        _ = async {
            match shutdown.as_mut() {
                Some(rx) => { let _ = rx.changed().await; }
                None => std::future::pending::<()>().await,
            }
        } => {
            info!("shutdown requested by embedder");false
        }
    };
    if !server_finished {
        server.abort();
        let _ = server.await;
    }
    if let Some(t) = discovery_task {
        t.abort();
    }
    if let Some(t) = online_task {
        t.abort();
        let _ = t.await;
    }
    if let Some(t) = meet_task {
        t.abort();
    }
    if let Some(t) = signal_task {
        t.abort();
    }
    // The signal loop persists its own identity as soon as it registers.
    let _ = &identity;
    Ok(())
}

/// Small helper shared by the LAN server and the relay client: build the agent
/// side session configuration for a given viewer.
pub fn session_config(
    state: &AgentState,
    session_id: &str,
    secret: String,
) -> vr_session::SessionConfig {
    vr_session::SessionConfig {
        session_id: session_id.to_string(),
        secret,
        app: format!("vragent/{AGENT_VERSION}"),
        device: Some(state.info.clone()),
        capabilities: state.caps.clone(),
    }
}

/// How often the agent refreshes its pairing code on the signal server.
pub const PAIRING_REFRESH: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_is_absolute() {
        let dir = state_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn capabilities_advertise_jpeg() {
        let caps = agent_capabilities();
        assert!(caps.codecs.contains(&Codec::Jpeg));
        assert!(caps.input);
    }

    #[test]
    fn identity_roundtrip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("vr-agent-test-{}", uuid::Uuid::new_v4()));
        let id = load_or_create_identity(&dir).unwrap();
        let again = load_or_create_identity(&dir).unwrap();
        assert_eq!(id.device_id, again.device_id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
