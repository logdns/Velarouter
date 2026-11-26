//! Minimal MQTT 3.1.1 publish/subscribe over TLS WebSocket.
//!
//! Only `CONNECT`, `SUBSCRIBE`, `PUBLISH` (QoS 0) and `PINGREQ` are implemented:
//! the rendezvous channel needs a tiny, well-audited message bus, not a full
//! MQTT client. Any broker that accepts MQTT-over-WebSocket works, including a
//! self-hosted one — the public defaults are only a convenience so that no
//! server has to be deployed before two machines can meet.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info};

/// Public brokers, tried in order until one accepts the connection.
pub const BROKERS: &[&str] = &[
    "wss://broker.emqx.io:8084/mqtt",
    "wss://broker.hivemq.com:8884/mqtt",
    "wss://test.mosquitto.org:8081/mqtt",
];

pub struct Payload {
    pub body: Vec<u8>,
}

pub struct Link {
    pub out: mpsc::Sender<Vec<u8>>,
    pub incoming: mpsc::Receiver<Payload>,
    pub online: watch::Receiver<bool>,
}

/// Opens a keep-alive subscription. The task owns reconnection and broker
/// failover, so callers only ever see "published" / "received".
pub fn open(topic: String, brokers: Vec<String>) -> Link {
    let (out_tx, out_rx) = mpsc::channel(64);
    let (in_tx, in_rx) = mpsc::channel(64);
    let (online_tx, online_rx) = watch::channel(false);
    tokio::spawn(run(topic, brokers, out_rx, in_tx, online_tx));
    Link {
        out: out_tx,
        incoming: in_rx,
        online: online_rx,
    }
}

async fn run(
    topic: String,
    brokers: Vec<String>,
    mut out: mpsc::Receiver<Vec<u8>>,
    incoming: mpsc::Sender<Payload>,
    online: watch::Sender<bool>,
) {
    let mut index = 0usize;
    let mut backoff = 1u64;
    loop {
        if brokers.is_empty() {
            return;
        }
        let url = brokers[index % brokers.len()].clone();
        match session(&url, &topic, &mut out, &incoming, &online).await {
            Ok(true) => return, // our side closed
            Ok(false) => {}
            Err(e) => debug!(%e, broker = %url, "broker session ended"),
        }
        online.send_replace(false);
        index += 1;
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(15);
    }
}

fn encode_len(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            return;
        }
    }
}

fn packet(header: u8, body: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(body.len() + 5);
    pkt.push(header);
    encode_len(body.len(), &mut pkt);
    pkt.extend_from_slice(body);
    pkt
}

/// Incremental decoder: one WebSocket frame may hold a partial packet or several.
#[derive(Default)]
struct Parser {
    buf: Vec<u8>,
}

impl Parser {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }
    fn next(&mut self) -> Option<(u8, Vec<u8>)> {
        if self.buf.len() < 2 {
            return None;
        }
        let header = self.buf[0];
        let mut len = 0usize;
        let mut mult = 1usize;
        let mut i = 1usize;
        loop {
            let byte = *self.buf.get(i)?;
            i += 1;
            len += (byte & 0x7f) as usize * mult;
            mult *= 128;
            if byte & 0x80 == 0 {
                break;
            }
            if i > 4 {
                return None;
            }
        }
        if self.buf.len() < i + len {
            return None;
        }
        let body = self.buf[i..i + len].to_vec();
        self.buf.drain(..i + len);
        Some((header, body))
    }
}

async fn session(
    url: &str,
    topic: &str,
    out: &mut mpsc::Receiver<Vec<u8>>,
    incoming: &mpsc::Sender<Payload>,
    online: &watch::Sender<bool>,
) -> Result<bool> {
    let mut request = url.into_client_request().context("invalid broker url")?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "mqtt".parse().context("bad subprotocol header")?,
    );
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(8),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .context("broker connect timeout")?
    .context("broker refused the connection")?;

    let mut connect = vec![0x00, 0x04];
    connect.extend_from_slice(b"MQTT");
    connect.push(4); // protocol level 3.1.1
    connect.push(0x02); // clean session
    connect.extend_from_slice(&45u16.to_be_bytes()); // keep-alive seconds
    let mut id = [0u8; 8];
    rand::Rng::fill_bytes(&mut rand::rand_core::UnwrapErr(rand::rngs::SysRng), &mut id);
    let client = format!("vr{}", hex(&id));
    connect.extend_from_slice(&(client.len() as u16).to_be_bytes());
    connect.extend_from_slice(client.as_bytes());
    ws.send(Message::Binary(packet(0x10, &connect).into()))
        .await?;

    let mut parser = Parser::default();
    // CONNACK
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("broker did not acknowledge the connection");
        }
        let frame = tokio::time::timeout(Duration::from_secs(8), ws.next()).await?;
        let Some(Ok(frame)) = frame else {
            bail!("broker closed during handshake");
        };
        if let Message::Binary(data) = frame {
            parser.push(&data);
            if let Some((header, body)) = parser.next() {
                if header >> 4 == 2 {
                    if body.len() >= 2 && body[1] != 0 {
                        bail!("broker rejected the connection (code {})", body[1]);
                    }
                    break;
                }
            }
        }
    }

    // SUBSCRIBE
    let mut sub = vec![0x00, 0x01];
    sub.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    sub.extend_from_slice(topic.as_bytes());
    sub.push(0);
    ws.send(Message::Binary(packet(0x82, &sub).into())).await?;
    online.send_replace(true);
    info!(broker = %url, "rendezvous channel online");

    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            body = out.recv() => {
                let Some(body) = body else { return Ok(true) };
                let mut publish = Vec::with_capacity(body.len() + topic.len() + 2);
                publish.extend_from_slice(&(topic.len() as u16).to_be_bytes());
                publish.extend_from_slice(topic.as_bytes());
                publish.extend_from_slice(&body);
                ws.send(Message::Binary(packet(0x30, &publish).into())).await?;
            }
            frame = ws.next() => {
                let Some(frame) = frame else { return Ok(false) };
                match frame? {
                    Message::Binary(data) => {
                        parser.push(&data);
                        while let Some((header, body)) = parser.next() {
                            let (kind, _) = (header >> 4, header & 0x0f);
                            if kind != 3 || body.len() < 2 {
                                continue;
                            }
                            let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
                            if body.len() < 2 + topic_len {
                                continue;
                            }
                            let (_, rest) = body.split_at(2 + topic_len);
                            if incoming.send(Payload { body: rest.to_vec() }).await.is_err() {
                                return Ok(true);
                            }
                        }
                    }
                    Message::Ping(_) => {
                        // Tungstenite answers automatically on flush.
                        ws.flush().await?;
                    }
                    Message::Close(_) => return Ok(false),
                    _ => {}
                }
            }
            _ = ping.tick() => {
                ws.send(Message::Binary(packet(0xc0, &[]).into())).await?;
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragmented_and_batched_packets() {
        let mut parser = Parser::default();
        let mut publish = Vec::new();
        publish.extend_from_slice(&2u16.to_be_bytes());
        publish.extend_from_slice(b"ab");
        publish.extend_from_slice(b"payload");
        let pkt = packet(0x30, &publish);
        parser.push(&pkt[..3]);
        assert!(parser.next().is_none());
        parser.push(&pkt[3..]);
        assert_eq!(parser.next(), Some((0x30, publish.clone())));
        assert!(parser.next().is_none());
    }
}
