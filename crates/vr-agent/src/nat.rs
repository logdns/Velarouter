//! Best-effort UDP port mapping through the home router (UPnP IGD).
//!
//! Rendezvous and hole punching already work on most home networks, but a
//! *symmetric* NAT gives every destination a different external port, which
//! punching cannot predict. A router mapping fixes exactly that case: the port
//! is opened on the router itself, so the candidate is reachable from anywhere.
//!
//! Never changes firewall rules, never asks for a permanent lease, and fails
//! silently when there is no gateway.

use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use igd_next::aio::tokio::search_gateway;
use igd_next::PortMappingProtocol::UDP;

const LEASE_SECONDS: u32 = 300;

async fn acquire(port: u16) -> Option<SocketAddrV4> {
    let gateway = search_gateway(Default::default()).await.ok()?;
    let IpAddr::V4(public) = gateway.get_external_ip().await.ok()? else {
        return None;
    };
    if !vr_net::direct::public_ip(public) {
        return None;
    }
    // Map the address of the interface that actually routes to the gateway,
    // not the first address on the machine (which may be a VPN adapter).
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    probe.connect(gateway.addr).await.ok()?;
    let IpAddr::V4(local) = probe.local_addr().ok()?.ip() else {
        return None;
    };
    let local = SocketAddrV4::new(local, port);
    let assigned = gateway
        .add_any_port(UDP, local.into(), LEASE_SECONDS, "Velarouter")
        .await
        .ok()?;
    (assigned != 0).then_some(SocketAddrV4::new(public, assigned))
}

/// Looks for a mapping in the background; the caller publishes the result as an
/// extra candidate if it arrives in time.
pub fn spawn(port: u16, tx: tokio::sync::mpsc::Sender<SocketAddr>) {
    tokio::spawn(async move {
        if let Ok(Some(mapped)) = tokio::time::timeout(Duration::from_secs(6), acquire(port)).await
        {
            tracing::info!(%mapped, "router mapping opened for the meet socket");
            let _ = tx.send(SocketAddr::V4(mapped)).await;
        }
    });
}
