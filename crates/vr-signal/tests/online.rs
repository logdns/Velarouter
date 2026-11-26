//! Local network integration: no display capture, input injection or internet service required.
use futures_util::StreamExt;
use std::time::Duration;
use tokio::{
    net::{TcpListener, UdpSocket},
    time::timeout,
};
use tokio_tungstenite::connect_async;
use vr_proto::Capabilities;
use vr_session::{Session, SessionConfig};
use vr_signal::{serve, SignalConfig};

#[tokio::test]
async fn ephemeral_code_finds_peer_and_authenticates_without_relay() {
    timeout(Duration::from_secs(15), async {
        let bind = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let server = tokio::spawn(serve(SignalConfig {
            bind,
            reflex_port: 0,
            rendezvous_port: udp.port(),
            ..Default::default()
        }));
        let code = "123456789012";
        let url = format!("ws://{bind}/v1/online/{code}");
        let mut host = loop {
            if let Ok((ws, _)) = connect_async(&url).await {
                break ws;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let ready = host.next().await.unwrap().unwrap();
        assert!(ready.to_text().unwrap().contains("ready"));
        let (mut other, _) = connect_async(&url).await.unwrap();
        assert!(other
            .next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap()
            .contains("conflict"));
        let agent = tokio::spawn(async move {
            let token = loop {
                let m = host.next().await.unwrap().unwrap();
                if m.is_text() {
                    let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
                    break v["token"].as_str().unwrap().to_owned();
                }
            };
            let p = vr_p2p::prepare_agent(udp, &token, Duration::from_secs(5))
                .await
                .unwrap();
            let ch = vr_p2p::accept(&p.endpoint).await.unwrap();
            let mut s = Session::accept(
                Box::new(ch),
                SessionConfig {
                    session_id: token,
                    secret: "host-only-test-secret".into(),
                    app: "test".into(),
                    device: None,
                    capabilities: Capabilities::default(),
                },
            )
            .await
            .unwrap();
            let _ = s.recv().await; // hold endpoint until viewer finishes handshake
        });
        let mut v = vr_client::connect_p2p(
            &format!("http://{bind}"),
            code,
            "host-only-test-secret",
            "test",
            vr_client::default_capabilities(),
        )
        .await
        .unwrap();
        assert_eq!(v.transport, "p2p");
        v.sender
            .send_control(vr_proto::Control::Bye {
                reason: "test finished".into(),
            })
            .await
            .unwrap();
        agent.await.unwrap();
        server.abort();
    })
    .await
    .expect("presence and P2P handshake timed out");
}
