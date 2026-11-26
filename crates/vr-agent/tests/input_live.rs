//! End-to-end input check: a real viewer session must move the OS cursor.
//!
//! Ignored by default: it needs Accessibility permission (macOS) or an
//! interactive desktop, and it moves the pointer. Run with
//! `cargo test -p vr-agent --test input_live -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::Duration;

use vr_agent::{agent_capabilities, files::FileService, AgentConfig, AgentState};
use vr_proto::{Control, DeviceInfo, InputEvent};
use vr_session::{Session, SessionConfig, WsChannel};

#[tokio::test]
#[ignore]
async fn mouse_move_reaches_the_operating_system() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let state = Arc::new(AgentState {
        events: None,
        #[cfg(target_os = "linux")]
        portal: None,
        info: DeviceInfo {
            device_id: "host".into(),
            name: "input-test".into(),
            platform: "test".into(),
            version: "test".into(),
            tags: vec![],
        },
        secret: "input-test-code".into(),
        config: AgentConfig::default(),
        displays: vec![],
        caps: agent_capabilities(),
        files: FileService::new(&root).unwrap(),
    });
    let server = tokio::spawn(vr_agent::server::serve(state, listener));

    let channel = WsChannel::connect(&format!("ws://{addr}/vr"))
        .await
        .unwrap();
    let session = Session::connect(
        Box::new(channel),
        SessionConfig {
            session_id: "input".into(),
            secret: "input-test-code".into(),
            app: "test".into(),
            device: None,
            capabilities: agent_capabilities(),
        },
    )
    .await
    .unwrap();
    let (mut sender, _receiver, _peer) = session.into_parts();

    let settings = enigo::Settings::default();
    let probe = enigo::Enigo::new(&settings).expect("input sink");
    use enigo::Mouse;
    let before = probe.location().unwrap();
    let target = (120u16, 140u16);
    // 65535 is the normalised width, so this lands on (120, 140) of display 0.
    sender
        .send_control(Control::Input {
            event: InputEvent::MouseMove {
                x: (target.0 as f64 / 1920.0 * 65535.0) as u16,
                y: (target.1 as f64 / 1080.0 * 65535.0) as u16,
                display: 0,
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (x, y) = probe.location().unwrap();
    println!("cursor {before:?} -> ({x},{y}) after remote move");
    assert!(
        (x - before.0).abs() > 100 || (y - before.1).abs() > 100,
        "the operating system did not move the cursor"
    );
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}
