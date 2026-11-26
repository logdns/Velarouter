//! Internet rendezvous: the "share this machine" path that needs no server.
//!
//! The agent publishes its UDP candidates under the connection code, opens its
//! NAT mapping toward the viewer, and then accepts a QUIC connection on the
//! very same socket. Nothing is relayed: the media path is direct.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{info, warn};
use vr_meet::{Announce, Role};

use crate::{AgentEvent, AgentState};

const BROKER_WAIT: Duration = Duration::from_secs(8);
const QUIC_WAIT: Duration = Duration::from_secs(8);

/// Binds the socket QUIC will run on. A stable port makes the same candidate
/// list reusable, and keeps router state warm between sessions.
async fn bind_socket(preferred: u16) -> Result<UdpSocket> {
    if preferred != 0 {
        if let Ok(socket) = UdpSocket::bind(("0.0.0.0", preferred)).await {
            return Ok(socket);
        }
    }
    if let Ok(socket) = UdpSocket::bind(("0.0.0.0", vr_proto::DEFAULT_MEET_PORT)).await {
        return Ok(socket);
    }
    Ok(UdpSocket::bind(("0.0.0.0", 0)).await?)
}

pub(super) fn spawn(
    state: Arc<AgentState>,
    code: String,
    brokers: Vec<String>,
    events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    shutdown: Option<watch::Receiver<bool>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // A session that ends (or a viewer that fails the handshake) must not
        // leave the machine unreachable: restart announcing immediately, and
        // only back off when the failure repeats without a session in between.
        let mut failures = 0u32;
        // Reusing the port keeps the published candidates valid across rounds.
        let mut last_port = 0u16;
        loop {
            match once(
                &state,
                &code,
                &brokers,
                &events,
                shutdown.clone(),
                &mut last_port,
            )
            .await
            {
                Ok(()) => failures = 0,
                Err(e) => {
                    if e.downcast_ref::<Stopped>().is_some() {
                        return;
                    }
                    failures += 1;
                    warn!(%e, failures, "internet rendezvous ended");
                    if failures <= 2 {
                        let _ = events.send(AgentEvent::InternetFailed(format!(
                            "会合中断，正在重连：{e}"
                        )));
                    }
                    let wait = match failures {
                        0..=2 => Duration::from_millis(400),
                        3..=5 => Duration::from_secs(2),
                        _ => Duration::from_secs(10),
                    };
                    tokio::time::sleep(wait).await;
                }
            }
        }
    })
}

/// Marker for "the embedder asked us to stop", so the retry loop exits quietly.
#[derive(Debug)]
struct Stopped;
impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped")
    }
}
impl std::error::Error for Stopped {}

async fn once(
    state: &Arc<AgentState>,
    code: &str,
    brokers: &[String],
    events: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    mut shutdown: Option<watch::Receiver<bool>>,
    last_port: &mut u16,
) -> Result<()> {
    let socket = bind_socket(*last_port)
        .await
        .context("无法绑定会合 UDP 端口")?;
    let local = socket.local_addr()?;
    *last_port = local.port();
    // Ask the router for a UDP mapping in the background; if it succeeds the
    // candidate is reachable even from a symmetric NAT.
    let (mapped_tx, mapped_rx) = tokio::sync::mpsc::channel(1);
    crate::nat::spawn(local.port(), mapped_tx);
    let mut announce = Announce::open_with_external(
        Arc::new(socket),
        code,
        Role::Host,
        brokers,
        None,
        Some(mapped_rx),
    )?;
    let mut online = announce.online();

    // Tell the UI as soon as the broker accepted us: from that moment the code
    // on screen is genuinely reachable by a viewer who has internet access.
    let waiting = async {
        let _ = online.wait_for(|value| *value).await;
        online.borrow().to_owned()
    };
    let online_now = tokio::time::timeout(BROKER_WAIT, waiting)
        .await
        .unwrap_or(false);
    if online_now {
        info!(%local, code, "internet rendezvous online");
        let _ = events.send(AgentEvent::InternetReady {
            code: code.to_string(),
        });
    } else {
        let _ = events.send(AgentEvent::InternetFailed(
            "会合通道连接失败，正在重试其他服务".into(),
        ));
    }

    let peer = tokio::select! {
        peer = announce.wait_peer(Duration::from_secs(3600)) => peer?,
        _ = async {
            match shutdown.as_mut() {
                Some(rx) => { let _ = rx.changed().await; }
                None => std::future::pending::<()>().await,
            }
        } => return Err(anyhow::Error::new(Stopped)),
    };
    info!(peer = ?peer.addrs, "viewer found, opening NAT mapping");

    let socket = announce.into_socket().await?;
    let endpoint = vr_p2p::server_endpoint(socket.into_std()?)?;
    let channel = tokio::time::timeout(QUIC_WAIT, vr_p2p::accept(&endpoint))
        .await
        .context("对方没有完成直连（双方都可能位于对称 NAT 之后）")??;

    let configuration = crate::session_config(state, &peer.session, state.secret.clone());
    let session = crate::accept_viewer(state, Box::new(channel), configuration)
        .await
        .context("密码校验失败或对方取消了连接")?;
    info!("internet session established");
    crate::server::run_session(session, state.clone()).await
}
