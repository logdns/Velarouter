//! Exercises the actual LAN upgrade route without capturing a screen or injecting input.
use std::sync::Arc;
use vr_agent::{agent_capabilities, files::FileService, AgentConfig, AgentState};
use vr_proto::{Control, DeviceInfo};
use vr_session::{Event, Session, SessionConfig, WsChannel};

#[tokio::test]
async fn direct_listener_requires_host_password() {
    check_listener("127.0.0.1:0", false, false).await;
}
#[tokio::test]
async fn ipv6_direct_upgrade_and_auth() {
    check_listener("[::1]:0", false, false).await;
}

#[tokio::test]
async fn trusted_listener_rejects_old_and_unpaired_clients_before_session() {
    check_listener("127.0.0.1:0", true, false).await;
}

#[tokio::test]
async fn wall_requires_auth_and_blocks_file_requests() {
    check_listener("127.0.0.1:0", true, true).await;
}

async fn check_listener(bind: &str, require_trust: bool, wall: bool) {
    let identity = vr_crypto::DeviceSigningKey::generate();
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let (events, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let (stop, shutdown) = tokio::sync::watch::channel(false);
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
            shutdown: Some(shutdown),
            trusted_viewers: require_trust
                .then(|| vec![vr_crypto::DevicePublicKey::parse(&identity.public_key()).unwrap()]),
            ..Default::default()
        },
        displays: vec![],
        caps: agent_capabilities(),
        files: FileService::new(&root).unwrap(),
    });
    let app = if wall {
        format!("{}test", vr_proto::WALL_APP_PREFIX)
    } else {
        "test".into()
    };
    let task = tokio::spawn(vr_agent::server::serve(host, listener));
    let url = format!("ws://{addr}/vr");
    assert!(WsChannel::connect(&format!("{url}?code=unused"))
        .await
        .is_err());
    let channel = WsChannel::connect(&url).await.unwrap();
    let r = Session::connect_with_identity(
        Box::new(channel),
        SessionConfig {
            session_id: "test".into(),
            secret: "incorrect".into(),
            app: app.clone(),
            device: None,
            capabilities: Default::default(),
        },
        require_trust.then_some(&identity),
    )
    .await;
    assert!(matches!(r, Err(vr_session::SessionError::Auth(_))));
    assert!(
        notices.try_recv().is_err(),
        "a rejected password is not a remote session"
    );
    if require_trust {
        let unpaired = vr_crypto::DeviceSigningKey::generate();
        for key in [None, Some(&unpaired)] {
            let r = Session::connect_with_identity(
                Box::new(WsChannel::connect(&url).await.unwrap()),
                SessionConfig {
                    session_id: "untrusted".into(),
                    secret: "test-host-secret".into(),
                    app: app.clone(),
                    device: None,
                    capabilities: Default::default(),
                },
                key,
            )
            .await;
            assert!(
                matches!(r, Err(vr_session::SessionError::Rejected(reason)) if reason == vr_session::UNTRUSTED_VIEWER_REASON)
            );
            assert!(
                notices.try_recv().is_err(),
                "no session event before device verification"
            );
        }
    }
    let channel = WsChannel::connect(&url).await.unwrap();
    let mut session = Session::connect_with_identity(
        Box::new(channel),
        SessionConfig {
            session_id: "accepted".into(),
            secret: "test-host-secret".into(),
            app,
            device: None,
            capabilities: Default::default(),
        },
        require_trust.then_some(&identity),
    )
    .await
    .unwrap();
    let wait = std::time::Duration::from_secs(5);
    assert!(matches!(
        tokio::time::timeout(wait, notices.recv()).await.unwrap(),
        Some(vr_agent::AgentEvent::SessionJoined)
    ));
    if wall {
        check_wall(&mut session, wait).await;
        drop(session);
        assert!(matches!(
            tokio::time::timeout(wait, notices.recv()).await.unwrap(),
            Some(vr_agent::AgentEvent::SessionLeft)
        ));
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    stop.send(true).unwrap();
    assert!(matches!(
        tokio::time::timeout(wait, notices.recv()).await.unwrap(),
        Some(vr_agent::AgentEvent::SessionLeft)
    ));
    drop(session);
    let r = Session::connect_with_identity(
        Box::new(WsChannel::connect(&url).await.unwrap()),
        SessionConfig {
            session_id: "stopped".into(),
            secret: "test-host-secret".into(),
            app: "test".into(),
            device: None,
            capabilities: Default::default(),
        },
        require_trust.then_some(&identity),
    )
    .await;
    assert!(
        r.is_err(),
        "the old listener cannot admit sessions after shutdown"
    );
    assert!(notices.try_recv().is_err());
    task.abort();
    let _ = task.await;
    std::fs::remove_dir_all(root).unwrap();
}

async fn check_wall(session: &mut Session, wait: std::time::Duration) {
    assert!(matches!(
        tokio::time::timeout(wait, session.recv())
            .await
            .unwrap()
            .unwrap(),
        Event::Control(Control::ViewOnly)
    ));
    // A wall session cannot enumerate shared files. A following Ping/Pong
    // proves the session processed the request and stayed connected.
    session
        .send_control(Control::FileListRequest {
            request_id: 1,
            path: String::new(),
        })
        .await
        .unwrap();
    session
        .send_control(Control::Ping { ts: 42 })
        .await
        .unwrap();
    tokio::time::timeout(wait, async {
        loop {
            match session.recv().await.unwrap() {
                Event::Pong { ts: 42 } => break,
                Event::Control(Control::FileList { .. } | Control::FileListError { .. }) => {
                    panic!("wall handled a file request")
                }
                Event::Closed(_) => panic!("wall closed before Pong"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
