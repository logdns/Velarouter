//! Transport plumbing shared by the agent and every client.
//!
//! Velarouter deliberately does **not** depend on WebRTC. The session is a
//! duplex message channel; what carries it is an implementation detail:
//!
//! * `ws://agent:34789/vr` — LAN direct, one WebSocket, lowest setup cost.
//! * `wss://signal/v1/session/ws` — relay through the signal server.
//! * `tcp://agent:34789` — plain framed TCP for embedded/headless viewers.
//! * `udp://…` — optional low-latency media path (see [`Fragmenter`]).
//!
//! The encrypted VRP session runs *inside* the channel unchanged, so switching
//! paths never renegotiates keys and the relay only ever sees ciphertext.

use async_trait::async_trait;
use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// Payload per UDP datagram. 1200 keeps the whole packet below the 1280-byte
/// IPv6 minimum MTU once VRP, UDP and IP headers are accounted for.
pub const UDP_CHUNK: usize = 1200;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("channel closed")]
    Closed,
    #[error("message too large: {0}")]
    TooLarge(usize),
    #[error("bad frame: {0}")]
    BadFrame(&'static str),
    #[error("websocket: {0}")]
    Ws(String),
}

/// One message received from a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMsg {
    Text(String),
    Binary(Vec<u8>),
    Closed,
}

/// Sending half of a channel. `&mut self` keeps back-pressure honest: a caller
/// that writes faster than the socket drains must await.
#[async_trait]
pub trait MessageSender: Send {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError>;
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError>;
    fn peer(&self) -> String;
}

/// Receiving half of a channel.
#[async_trait]
pub trait MessageReceiver: Send {
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError>;
}

/// A duplex, message-oriented channel.
///
/// Splitting is part of the contract because every real client needs to send
/// input while receiving video; a single `&mut self` object cannot be polled in
/// two directions at once.
#[async_trait]
pub trait MessageChannel: Send {
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError>;
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), TransportError>;
    async fn recv(&mut self) -> Result<ChannelMsg, TransportError>;
    fn peer(&self) -> String;
    fn split(self: Box<Self>) -> (Box<dyn MessageSender>, Box<dyn MessageReceiver>);
}

/// Length-prefixed framing for byte streams.
///
/// `[u32 big-endian length][payload]` — the same layout every SDK implements.
#[derive(Debug, Default)]
pub struct StreamCodec {
    buf: BytesMut,
}

impl StreamCodec {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(64 * 1024),
        }
    }

    /// Appends raw bytes read from the socket.
    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pops one complete message, if the buffer holds one.
    pub fn next_message(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > MAX_MESSAGE {
            return Err(TransportError::TooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        self.buf.advance(4);
        let msg = self.buf.split_to(len).to_vec();
        Ok(Some(msg))
    }

    pub fn encode(payload: &[u8]) -> Result<Vec<u8>, TransportError> {
        if payload.len() > MAX_MESSAGE {
            return Err(TransportError::TooLarge(payload.len()));
        }
        let mut out = Vec::with_capacity(4 + payload.len());
        out.put_u32(payload.len() as u32);
        out.extend_from_slice(payload);
        Ok(out)
    }
}

/// Header of one UDP fragment (16 bytes).
///
/// ```text
/// frame_id u32 | index u16 | count u16 | total_len u32 | nonce u32
/// ```
/// `nonce` makes every fragment independently decryptable and replay-safe
/// without relying on delivery order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    pub frame_id: u32,
    pub index: u16,
    pub count: u16,
    pub total_len: u32,
    pub nonce: u32,
}

pub const FRAGMENT_HEADER_LEN: usize = 16;

impl FragmentHeader {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.put_u32(self.frame_id);
        out.put_u16(self.index);
        out.put_u16(self.count);
        out.put_u32(self.total_len);
        out.put_u32(self.nonce);
    }

    pub fn parse(buf: &[u8]) -> Result<Self, TransportError> {
        if buf.len() < FRAGMENT_HEADER_LEN {
            return Err(TransportError::BadFrame("fragment header too short"));
        }
        Ok(Self {
            frame_id: u32::from_be_bytes(buf[0..4].try_into().unwrap()),
            index: u16::from_be_bytes(buf[4..6].try_into().unwrap()),
            count: u16::from_be_bytes(buf[6..8].try_into().unwrap()),
            total_len: u32::from_be_bytes(buf[8..12].try_into().unwrap()),
            nonce: u32::from_be_bytes(buf[12..16].try_into().unwrap()),
        })
    }
}

/// Splits a sealed media frame into UDP datagrams.
pub struct Fragmenter {
    frame_id: u32,
    nonce_seq: u32,
}

impl Fragmenter {
    pub fn new() -> Self {
        Self {
            frame_id: 0,
            nonce_seq: 1,
        }
    }

    /// Returns datagrams, each `FragmentHeader || chunk`.
    pub fn fragment(&mut self, sealed: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let frame_id = self.frame_id;
        self.frame_id = self.frame_id.wrapping_add(1);
        let count = sealed.len().div_ceil(chunk.max(1)) as u16;
        let mut out = Vec::with_capacity(count as usize);
        for (i, part) in sealed.chunks(chunk.max(1)).enumerate() {
            let nonce = self.nonce_seq;
            self.nonce_seq = self.nonce_seq.wrapping_add(1);
            let hdr = FragmentHeader {
                frame_id,
                index: i as u16,
                count,
                total_len: sealed.len() as u32,
                nonce,
            };
            let mut packet = Vec::with_capacity(FRAGMENT_HEADER_LEN + part.len());
            hdr.encode(&mut packet);
            packet.extend_from_slice(part);
            out.push(packet);
        }
        out
    }
}

impl Default for Fragmenter {
    fn default() -> Self {
        Self::new()
    }
}

/// Reassembles fragments out of order or lossy. A frame that is incomplete after
/// `timeout` is dropped and counted as a loss instead of stalling the stream.
#[derive(Debug)]
pub struct Reassembler {
    partial: std::collections::HashMap<u32, Partial>,
    max_age: std::time::Duration,
    dropped: u64,
}

#[derive(Debug)]
struct Partial {
    total_len: usize,
    count: u16,
    parts: Vec<Option<Vec<u8>>>,
    filled: usize,
    first_seen: std::time::Instant,
}

impl Reassembler {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            partial: Default::default(),
            max_age: timeout,
            dropped: 0,
        }
    }

    /// Feeds one datagram; returns a complete frame when the last fragment lands.
    pub fn push(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>, TransportError> {
        let hdr = FragmentHeader::parse(packet)?;
        if hdr.count == 0 || hdr.total_len as usize > MAX_MESSAGE {
            return Err(TransportError::BadFrame("invalid fragment"));
        }
        if hdr.index >= hdr.count {
            return Err(TransportError::BadFrame("fragment index out of range"));
        }
        self.evict_expired();

        let entry = self.partial.entry(hdr.frame_id).or_insert_with(|| Partial {
            total_len: hdr.total_len as usize,
            count: hdr.count,
            parts: vec![None; hdr.count as usize],
            filled: 0,
            first_seen: std::time::Instant::now(),
        });

        if entry.count != hdr.count || entry.total_len != hdr.total_len as usize {
            return Err(TransportError::BadFrame("fragment metadata mismatch"));
        }
        let slot = &mut entry.parts[hdr.index as usize];
        if slot.is_none() {
            *slot = Some(packet[FRAGMENT_HEADER_LEN..].to_vec());
            entry.filled += 1;
        }
        if entry.filled != entry.count as usize {
            return Ok(None);
        }

        let done = self.partial.remove(&hdr.frame_id).unwrap();
        let mut out = Vec::with_capacity(done.total_len);
        for part in done.parts.into_iter() {
            match part {
                Some(p) => out.extend_from_slice(&p),
                None => {
                    self.dropped += 1;
                    return Ok(None);
                }
            }
        }
        out.truncate(done.total_len);
        Ok(Some(out))
    }

    fn evict_expired(&mut self) {
        let now = std::time::Instant::now();
        let max_age = self.max_age;
        let before = self.partial.len();
        self.partial
            .retain(|_, p| now.duration_since(p.first_seen) < max_age);
        self.dropped += (before - self.partial.len()) as u64;
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn pending(&self) -> usize {
        self.partial.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_framing_handles_split_reads() {
        let mut codec = StreamCodec::new();
        let msg = StreamCodec::encode(b"hello world").unwrap();
        codec.feed(&msg[..3]);
        assert!(codec.next_message().unwrap().is_none());
        codec.feed(&msg[3..]);
        assert_eq!(codec.next_message().unwrap().unwrap(), b"hello world");
        assert!(codec.next_message().unwrap().is_none());
    }

    #[test]
    fn stream_framing_rejects_oversize_length() {
        let mut codec = StreamCodec::new();
        codec.feed(&((MAX_MESSAGE as u32) + 1).to_be_bytes());
        assert!(matches!(
            codec.next_message(),
            Err(TransportError::TooLarge(_))
        ));
    }

    #[test]
    fn fragmentation_roundtrip_out_of_order() {
        let payload: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut frag = Fragmenter::new();
        let packets = frag.fragment(&payload, UDP_CHUNK);
        assert!(packets.len() > 1);

        let mut re = Reassembler::new(std::time::Duration::from_secs(5));
        let mut got = None;
        for packet in packets.iter().rev() {
            if let Some(frame) = re.push(packet).unwrap() {
                got = Some(frame);
            }
        }
        assert_eq!(got.unwrap(), payload);
        assert_eq!(re.pending(), 0);
    }

    #[test]
    fn reassembler_drops_incomplete_frames() {
        let payload = vec![7u8; 4000];
        let mut frag = Fragmenter::new();
        let packets = frag.fragment(&payload, UDP_CHUNK);
        let mut re = Reassembler::new(std::time::Duration::from_millis(1));
        assert!(re.push(&packets[0]).unwrap().is_none());
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(re.push(&packets[1]).unwrap().is_none());
        assert!(re.dropped() >= 1);
    }

    #[test]
    fn fragment_header_roundtrip() {
        let hdr = FragmentHeader {
            frame_id: 9,
            index: 2,
            count: 5,
            total_len: 9000,
            nonce: 77,
        };
        let mut buf = Vec::new();
        hdr.encode(&mut buf);
        assert_eq!(FragmentHeader::parse(&buf).unwrap(), hdr);
    }
}
