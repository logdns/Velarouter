//! Establish a viewer session over LAN or P2P, with no UI assumptions.
//!
//! Both the CLI and the desktop clients use this module, which keeps the
//! connection logic in one place and the GUI free of protocol details.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use vr_crypto::DeviceSigningKey;
use vr_proto::{Capabilities, Control, VideoConfig};
use vr_session::{Peer, Session, SessionReceiver, SessionSender, WsChannel};
use vr_transport::MessageChannel;

/// A live viewer session, ready to send input and receive frames.
pub struct Viewer {
    pub sender: SessionSender,
    pub receiver: SessionReceiver,
    pub peer: Peer,
    /// `"lan"`, `"p2p"` — shown in the UI.
    pub transport: &'static str,
    pub session_id: String,
}

pub fn default_capabilities() -> Capabilities {
    Capabilities {
        codecs: vec![vr_proto::Codec::Jpeg],
        max_width: 2560,
        max_height: 1600,
        max_fps: 60,
        input: true,
        clipboard: true,
        multi_display: true,
        audio: false,
    }
}

fn config(
    session_id: String,
    secret: String,
    app: &str,
    caps: Capabilities,
) -> vr_session::SessionConfig {
    vr_session::SessionConfig {
        session_id,
        secret,
        app: app.to_string(),
        device: None,
        capabilities: caps,
    }
}

/// Direct LAN connection: `ws://ip:34789/vr` + pairing code.
pub async fn connect_lan(
    url: &str,
    pairing_code: &str,
    app: &str,
    caps: Capabilities,
) -> Result<Viewer> {
    connect_lan_with_identity(url, pairing_code, app, caps, None).await
}

pub async fn connect_lan_with_identity(
    url: &str,
    pairing_code: &str,
    app: &str,
    caps: Capabilities,
    identity: Option<&DeviceSigningKey>,
) -> Result<Viewer> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let channel = WsChannel::connect(url)
        .await
        .with_context(|| format!("cannot connect to {url}"))?;
    let session = Session::connect_with_identity(
        Box::new(channel) as Box<dyn MessageChannel>,
        config(session_id.clone(), pairing_code.to_string(), app, caps),
        identity,
    )
    .await
    .context("LAN handshake failed")?;
    Ok(into_viewer(session, "lan", session_id))
}

#[derive(Debug, serde::Deserialize)]
struct RendezvousResp {
    token: String,
    udp_port: u16,
}

/// Internet P2P: rendezvous by 9-digit code, punch UDP, run QUIC. No relay.
pub async fn connect_p2p(
    signal_base: &str,
    device_code: &str,
    password: &str,
    app: &str,
    caps: Capabilities,
) -> Result<Viewer> {
    connect_p2p_with_identity(signal_base, device_code, password, app, caps, None).await
}

pub async fn connect_p2p_with_identity(
    signal_base: &str,
    device_code: &str,
    password: &str,
    app: &str,
    caps: Capabilities,
    identity: Option<&DeviceSigningKey>,
) -> Result<Viewer> {
    let base = signal_base.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp: RendezvousResp = http
        .post(format!("{base}/v1/rendezvous"))
        .json(&serde_json::json!({ "device_code": device_code }))
        .send()
        .await
        .context("signal server unreachable")?
        .error_for_status()
        .context("connection code rejected (device offline?)")?
        .json()
        .await?;
    let url = url::Url::parse(&base)?;
    let host = url.host_str().context("signal url has no host")?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, resp.udp_port))
        .await
        .context("cannot resolve signal host")?
        .collect();
    // The rendezvous UDP service binds 0.0.0.0; prefer IPv4 so `localhost`
    // does not resolve to ::1 and silently time out.
    let signal_udp = addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .context("signal host resolved to no address")?;

    let p2p = vr_p2p::prepare_viewer(signal_udp, &resp.token, Duration::from_secs(20))
        .await
        .context("P2P setup failed (no relay fallback by design)")?;
    let channel = tokio::time::timeout(
        Duration::from_secs(10),
        vr_p2p::connect(&p2p.endpoint, p2p.peer),
    )
    .await
    .context("QUIC connect timeout")??;

    let session = Session::connect_with_identity(
        Box::new(channel) as Box<dyn MessageChannel>,
        config(resp.token.clone(), password.to_string(), app, caps),
        identity,
    )
    .await
    .context("P2P handshake failed (wrong password?)")?;
    Ok(into_viewer(session, "p2p", resp.token))
}

/// Internet rendezvous: the code + password are the only inputs. The two peers
/// meet on a public broker, punch through their routers and speak QUIC directly.
/// Nothing is relayed, so no server has to be deployed before this works.
pub async fn connect_meet(
    code: &str,
    password: &str,
    brokers: &[String],
    app: &str,
    caps: Capabilities,
) -> Result<Viewer> {
    connect_meet_with_identity(code, password, brokers, app, caps, None).await
}

pub async fn connect_meet_with_identity(
    code: &str,
    password: &str,
    brokers: &[String],
    app: &str,
    caps: Capabilities,
    identity: Option<&DeviceSigningKey>,
) -> Result<Viewer> {
    // The host stops announcing while it serves a session, and a session whose
    // viewer died lingers until the QUIC idle timeout. So instead of a couple of
    // long attempts, keep short attempts coming until the budget is spent: a
    // machine that just finished a session is then picked up within a second of
    // becoming available, and a wrong code still fails in a few seconds.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut last: Option<anyhow::Error> = None;
    let mut attempt = 0u32;

    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        attempt += 1;
        if attempt > 1 {
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        let socket = Arc::new(tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await?);
        let mut announce =
            vr_meet::Announce::open(socket, code, vr_meet::Role::Viewer, brokers, None)?;
        let peer = match announce.wait_peer(Duration::from_secs(6)).await {
            Ok(peer) => peer,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let socket = match announce.into_socket().await {
            Ok(socket) => socket,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let endpoint = vr_p2p::client_endpoint(socket.into_std()?)?;
        // Short dial window: a stale candidate list must not eat the budget.
        let dial = vr_p2p::connect_any(&endpoint, &peer.addrs, Duration::from_secs(8)).await;
        let channel = match dial {
            Ok((channel, _addr)) => channel,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let session = Session::connect_with_identity(
            Box::new(channel) as Box<dyn MessageChannel>,
            config(peer.session.clone(), password.to_string(), app, caps),
            identity,
        )
        .await
        .context("P2P handshake failed (wrong password?)")?;
        return Ok(into_viewer(session, "p2p", peer.session));
    }

    Err(last
        .unwrap_or_else(|| anyhow::anyhow!("no peer answered"))
        .context("对方没有响应：可能未在线、正被其他会话占用，或网络无法访问会合服务"))
}

fn into_viewer(session: Session, transport: &'static str, session_id: String) -> Viewer {
    let (sender, receiver, peer) = session.into_parts();
    Viewer {
        sender,
        receiver,
        peer,
        transport,
        session_id,
    }
}

/// Convenience: request a JPEG stream and return the sender.
pub async fn request_video(
    viewer: &mut Viewer,
    quality: u8,
    fps: u8,
    display: u32,
    max_width: u16,
    max_height: u16,
) -> Result<()> {
    viewer
        .sender
        .send_control(Control::VideoConfig(VideoConfig {
            codec: vr_proto::Codec::Jpeg,
            quality,
            fps,
            max_width,
            max_height,
            display,
        }))
        .await?;
    Ok(())
}
