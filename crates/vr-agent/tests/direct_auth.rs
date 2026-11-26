//! Exercises the actual LAN upgrade route without capturing a screen or injecting input.
use std::sync::Arc;
use vr_agent::{agent_capabilities, files::FileService, AgentConfig, AgentState};
use vr_proto::DeviceInfo;
use vr_session::{Session, SessionConfig, WsChannel};

#[tokio::test]
async fn direct_listener_requires_host_password() {
    check_listener("127.0.0.1:0").await;
}
#[tokio::test]
async fn ipv6_direct_upgrade_and_auth() {
    check_listener("[::1]:0").await;
}
async fn check_listener(bind: &str) {
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let (events, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(AgentState {
        events: Some(events),
        #[cfg(target_os = "linux")]
        portal: None,
        info: DeviceInfo {
            device_id: "host".into(),
            name: "test".into(),
            platform: "test".into(),
            version: "test".into(),
            tags: vec![],
        },
        secret: "test-host-secret".into(),
        config: AgentConfig {
            bind: addr,
            ..Default::default()
        },
        displays: vec![],
        caps: agent_capabilities(),
        files: FileService::new(&root).unwrap(),
    });
    let task = tokio::spawn(vr_agent::server::serve(host, listener));
    let url = format!("ws://{addr}/vr");
    assert!(WsChannel::connect(&format!("{url}?code=unused"))
        .await
        .is_err());
    let channel = WsChannel::connect(&url).await.unwrap();
    let r = Session::connect(
        Box::new(channel),
        SessionConfig {
            session_id: "test".into(),
            secret: "incorrect".into(),
            app: "test".into(),
            device: None,
            capabilities: Default::default(),
        },
    )
    .await;
    assert!(matches!(r, Err(vr_session::SessionError::Auth(_))));
    assert!(
        notices.try_recv().is_err(),
        "a rejected password is not a remote session"
    );
    let channel = WsChannel::connect(&url).await.unwrap();
    let session = Session::connect(
        Box::new(channel),
        SessionConfig {
            session_id: "accepted".into(),
            secret: "test-host-secret".into(),
            app: "test".into(),
            device: None,
            capabilities: Default::default(),
        },
    )
    .await
    .unwrap();
    let wait = std::time::Duration::from_secs(5);
    assert!(matches!(
        tokio::time::timeout(wait, notices.recv()).await.unwrap(),
        Some(vr_agent::AgentEvent::SessionJoined)
    ));
    drop(session);
    assert!(matches!(
        tokio::time::timeout(wait, notices.recv()).await.unwrap(),
        Some(vr_agent::AgentEvent::SessionLeft)
    ));
    task.abort();
    let _ = task.await;
    std::fs::remove_dir_all(root).unwrap();
}
