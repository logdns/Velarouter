//! Small STUN client: learn the address a peer on the internet sees for us.
//!
//! Only `Binding Request` / `XOR-MAPPED-ADDRESS` are implemented. The socket is
//! supplied by the caller so the mapping that is discovered is exactly the one
//! that will later carry QUIC.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

/// Public STUN servers, tried in parallel. A reply from any of them is enough:
/// the address is only a *hint*, reachability is decided by the handshake.
pub const SERVERS: &[&str] = &[
    "stun.cloudflare.com:3478",
    "stun.l.google.com:19302",
    "stun.chat.bilibili.com:3478",
];

const MAGIC: u32 = 0x2112_A442;

fn request(id: [u8; 12]) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[..2].copy_from_slice(&0x0001u16.to_be_bytes());
    buf[2..4].copy_from_slice(&0u16.to_be_bytes());
    buf[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    buf[8..20].copy_from_slice(&id);
    buf
}

/// Parses a binding response and returns the mapped address.
fn parse(buf: &[u8], id: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 || buf[8..20] != *id {
        return None;
    }
    let kind = u16::from_be_bytes([buf[0], buf[1]]);
    if kind != 0x0101 {
        return None;
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let end = (20 + len).min(buf.len());
    let mut off = 20;
    while off + 4 <= end {
        let attr = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let alen = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
        let value = buf.get(off + 4..off + 4 + alen)?;
        match (attr, alen) {
            (0x0020, 8..) if value[1] == 0x01 => {
                let port = u16::from_be_bytes([value[2], value[3]]) ^ 0x2112;
                let raw = [
                    value[4] ^ 0x21,
                    value[5] ^ 0x12,
                    value[6] ^ 0xA4,
                    value[7] ^ 0x42,
                ];
                return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(raw)), port));
            }
            (0x0020, 20..) if value[1] == 0x02 => {
                let port = u16::from_be_bytes([value[2], value[3]]) ^ 0x2112;
                let mut raw = [0u8; 16];
                for (i, byte) in raw.iter_mut().enumerate() {
                    *byte = value[4 + i] ^ buf[4 + i];
                }
                return Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(raw)), port));
            }
            (0x0001, 8..) => {
                let port = u16::from_be_bytes([value[2], value[3]]);
                let raw = [value[4], value[5], value[6], value[7]];
                return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(raw)), port));
            }
            _ => {}
        }
        off += 4 + alen + ((4 - alen % 4) % 4);
    }
    None
}

async fn probe(socket: &UdpSocket, server: &str) -> Option<SocketAddr> {
    let addr = tokio::net::lookup_host(server).await.ok()?.next()?;
    let mut id = [0u8; 12];
    rand::Rng::fill_bytes(&mut rand::rand_core::UnwrapErr(rand::rngs::SysRng), &mut id);
    let packet = request(id);
    let mut buf = [0u8; 512];
    for _ in 0..3 {
        socket.send_to(&packet, addr).await.ok()?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
        while tokio::time::Instant::now() < deadline {
            let left = deadline - tokio::time::Instant::now();
            let Ok(Ok((n, from))) = tokio::time::timeout(left, socket.recv_from(&mut buf)).await
            else {
                break;
            };
            if from.ip() != addr.ip() || from.port() != addr.port() {
                continue;
            }
            if let Some(mapped) = parse(&buf[..n], &id) {
                return Some(mapped);
            }
        }
    }
    None
}

/// Returns this host's reflexive address as seen by the first STUN server that
/// answers, or `None` when UDP egress is blocked. The probes share the caller's
/// socket, so the discovered mapping is the one the peer will punch into.
pub async fn reflexive(socket: &UdpSocket, servers: &[&str]) -> Vec<SocketAddr> {
    // Symmetric NAT hands out a different port per destination, so every answer
    // is a separate candidate worth trying: one of them may be the port the peer
    // can reach.
    let mut found = Vec::new();
    for server in servers {
        if let Some(mapped) = probe(socket, server).await {
            if !found.contains(&mapped) {
                found.push(mapped);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address() {
        let id = [7u8; 12];
        let mut packet = Vec::from(request(id));
        packet[0..2].copy_from_slice(&0x0101u16.to_be_bytes()); // binding success
        packet[2..4].copy_from_slice(&12u16.to_be_bytes());
        packet.extend_from_slice(&0x0020u16.to_be_bytes());
        packet.extend_from_slice(&8u16.to_be_bytes());
        packet.extend_from_slice(&[0x00, 0x01]); // reserved, family = IPv4
        let port: u16 = 40_000;
        packet.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        let ip = [8u8, 8, 4, 4];
        packet.extend_from_slice(&[ip[0] ^ 0x21, ip[1] ^ 0x12, ip[2] ^ 0xA4, ip[3] ^ 0x42]);
        assert_eq!(
            parse(&packet, &id),
            Some(SocketAddr::from((Ipv4Addr::new(8, 8, 4, 4), port)))
        );
        let mut wrong = id;
        wrong[0] ^= 0xff;
        assert!(parse(&packet, &wrong).is_none());
    }
}
