//! Uses the production desktop preview worker, WebSocket listener and X11 capture.
use super::*;
use std::sync::Arc;
use vr_agent::{
    capture::{FrameSource, ScreenCapturer},
    files::FileService,
    AgentConfig, AgentEvent, AgentState,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run scripts/test-desktop-wall.sh to supply an isolated Xvfb display"]
async fn wall_xvfb_four_previews() {
    assert_eq!(
        std::env::var("VR_WALL_XVFB_TEST").as_deref(),
        Ok("1"),
        "use the isolated test runner"
    );
    let tmp = tempfile::tempdir().unwrap();
    let identity = vr_crypto::DeviceSigningKey::generate();
    let public = vr_crypto::DevicePublicKey::parse(&identity.public_key()).unwrap();
    let (events, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let caps = vr_agent::agent_capabilities();
    let displays = ScreenCapturer::new(WIDTH, HEIGHT, 40, FPS, 0)
        .unwrap()
        .displays();
    assert_eq!(
        displays.len(),
        1,
        "the test should see only the virtual screen"
    );
    assert_eq!((displays[0].width, displays[0].height), (800, 450));
    let mut hosts = tokio::task::JoinSet::new();
    let ctx = egui::Context::default();
    let mut wall = Wall::default();
    for index in 0..MAX_TILES {
        let name = format!("Virtual desktop {}", index + 1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let host = Arc::new(AgentState {
            config: AgentConfig {
                bind,
                discoverable: false,
                trusted_viewers: Some(vec![public.clone()]),
                ..Default::default()
            },
            info: vr_proto::DeviceInfo {
                device_id: format!("xvfb-{index}"),
                name: name.clone(),
                platform: "linux".into(),
                version: "test".into(),
                tags: vec![],
            },
            secret: "ephemeral-test-password".into(),
            displays: displays.clone(),
            caps: caps.clone(),
            files: FileService::new(&tmp.path().join(index.to_string())).unwrap(),
            events: Some(events.clone()),
            portal: None,
        });
        hosts.spawn(vr_agent::server::serve(host, listener));
        // Public code generation intentionally excludes loopback. Feed only the
        // parsed destination here; parser acceptance is tested in vr-net.
        wall.start(
            name,
            ConnectReq {
                target: Target::Direct(bind),
                entered_code: String::new(),
                password: "ephemeral-test-password".into(),
                viewer_identity: Some(identity.clone()),
                brokers: vec![],
                initial_size: (WIDTH, HEIGHT),
            },
            &tokio::runtime::Handle::current(),
            &ctx,
        );
    }
    let wait = Duration::from_secs(20);
    tokio::time::timeout(wait, async {
        while !wall.tiles.iter().all(|tile| tile.texture.is_some()) {
            wall.update(&ctx);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "missing preview: {:?}",
            wall.tiles
                .iter()
                .map(|tile| &tile.status)
                .collect::<Vec<_>>()
        )
    });
    for tile in &wall.tiles {
        assert_eq!(tile.name.as_deref(), Some(tile.code.as_str()));
        assert_eq!(
            tile.texture.as_ref().unwrap().size(),
            [usize::from(WIDTH), usize::from(HEIGHT)]
        );
        assert!(!tile.task.is_finished());
    }
    for _ in 0..MAX_TILES {
        assert!(matches!(
            notices.try_recv().unwrap(),
            AgentEvent::SessionJoined
        ));
    }
    assert!(notices.try_recv().is_err());

    // Changing only the synthetic root window should update all four previews.
    // The initial frame test alone wouldn't catch a frozen capture pipeline.
    assert!(std::process::Command::new("xsetroot")
        .args(["-solid", "#D2A029"])
        .status()
        .unwrap()
        .success());
    let mut updated = [false; MAX_TILES];
    tokio::time::timeout(wait, async {
        while !updated.iter().all(|&changed| changed) {
            for (index, tile) in wall.tiles.iter().enumerate() {
                if let Some(frame) = tile.frame.lock().unwrap().as_ref() {
                    let middle = 4
                        * (frame.width as usize * (frame.height as usize / 2)
                            + frame.width as usize / 2);
                    let [r, g, b] = frame.rgba[middle..middle + 3] else {
                        unreachable!()
                    };
                    // Loose tolerances allow JPEG color rounding.
                    updated[index] |= r > 175 && g > 120 && b < 85;
                }
            }
            // Don't drain the frame slots while waiting: static JPEG may be
            // emitted only once, including between two adjacent polls.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing changed Xvfb frame: {updated:?}"));
    wall.update(&ctx);

    // Require prompt teardown rather than falling back to the 7s idle watchdog.
    let teardown = Duration::from_secs(5);
    wall.tiles.remove(0);
    expect_left(&mut notices, teardown).await;
    assert!(wall.tiles.iter().all(|tile| !tile.task.is_finished()));
    wall.clear();
    assert!(wall.tiles.is_empty());
    tokio::time::timeout(teardown, async {
        for _ in 1..MAX_TILES {
            expect_left(&mut notices, teardown).await;
        }
    })
    .await
    .expect("all remaining previews should disconnect promptly");
    assert!(notices.try_recv().is_err());
    hosts.shutdown().await;
}

async fn expect_left(
    notices: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    wait: Duration,
) {
    assert!(matches!(
        tokio::time::timeout(wait, notices.recv()).await.unwrap(),
        Some(AgentEvent::SessionLeft)
    ));
}
