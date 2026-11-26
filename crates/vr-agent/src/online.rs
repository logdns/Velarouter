//! Desktop presence uses only a public ID. The control password never goes to the directory.
use crate::{AgentEvent, AgentState};
use anyhow::{ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rand::RngExt;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc::UnboundedSender, task::JoinSet, time::timeout};
use tokio_tungstenite::tungstenite::Message;

pub fn fresh_code() -> String {
    rand::rand_core::UnwrapErr(rand::rngs::SysRng)
        .random_range(100_000_000_000u64..1_000_000_000_000)
        .to_string()
}
/// No public service or third-party fallback is assumed. Plain HTTP is for loopback tests only.
pub fn directory(raw: &str) -> Result<String> {
    let u = url::Url::parse(raw.trim())?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1"));
    ensure!(
        u.host().is_some()
            && (u.scheme() == "https" || (local && u.scheme() == "http"))
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "会合服务必须使用 HTTPS"
    );
    Ok(u.as_str().trim_end_matches('/').to_string())
}
pub(super) async fn run(
    state: Arc<AgentState>,
    base: String,
    mut code: String,
    events: UnboundedSender<AgentEvent>,
) {
    let mut delay = 1;
    let mut sessions = JoinSet::new();
    loop {
        let r = presence(&state, &base, &code, &events, &mut sessions, &mut delay).await;
        if matches!(r, Ok(true)) {
            code = fresh_code();
            continue;
        }
        let _ = events.send(AgentEvent::SignalUnavailable(
            "会合服务离线，正在自动重试".into(),
        ));
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}
async fn presence(
    state: &Arc<AgentState>,
    base: &str,
    code: &str,
    events: &UnboundedSender<AgentEvent>,
    sessions: &mut JoinSet<()>,
    delay: &mut u64,
) -> Result<bool> {
    let endpoint = directory(base)?.replacen("http", "ws", 1);
    let (mut ws, _) = timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(format!("{endpoint}/v1/online/{code}")),
    )
    .await??;
    let mut udp = None;
    loop {
        let msg = timeout(
            Duration::from_secs(if udp.is_some() { 45 } else { 10 }),
            ws.next(),
        )
        .await?
        .context("control channel closed")??;
        match msg {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)?;
                match v["t"].as_str() {
                    Some("conflict") if udp.is_none() => return Ok(true),
                    Some("ready") if udp.is_none() => {
                        let port = v["udp_port"]
                            .as_u64()
                            .and_then(|p| u16::try_from(p).ok())
                            .filter(|p| *p != 0)
                            .context("invalid UDP port")?;
                        udp = Some(crate::signal::udp_addr(base, port).await?);
                        *delay = 1;
                        events.send(AgentEvent::Registered {
                            device_code: code.into(),
                            device_id: state.info.device_id.clone(),
                        })?;
                    }
                    Some("rendezvous") => {
                        while sessions.try_join_next().is_some() {}
                        if sessions.len() < 2 {
                            if let (Some(addr), Some(token)) = (udp, v["token"].as_str()) {
                                let state = state.clone();
                                let token = token.to_owned();
                                sessions.spawn(async move {
                                    if let Err(e) =
                                        crate::signal::accept_at(state, addr, token).await
                                    {
                                        tracing::debug!(%e,"direct session ended");
                                    }
                                });
                            }
                        }
                    }
                    _ => anyhow::bail!("unexpected directory message"),
                }
            }
            Message::Ping(_) => ws.flush().await?,
            Message::Close(_) => return Ok(false),
            _ => {}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codes_and_directory() {
        assert!(vr_proto::is_internet_code(&fresh_code()));
        assert!(!vr_proto::is_internet_code("123456789"));
        assert!(directory("https://desk.example/").is_ok());
        assert!(directory("http://127.0.0.1:34790").is_ok());
        assert!(directory("http://desk.example").is_err());
        assert!(directory("https://user:pass@desk.example").is_err());
    }
}
