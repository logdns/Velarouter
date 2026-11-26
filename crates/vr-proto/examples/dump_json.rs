//! Prints every control message as JSON, so documentation and SDK tests can be
//! generated from the single source of truth instead of copied by hand.
//!
//! `cargo run -p vr-proto --example dump_json`

use vr_proto::*;

fn main() {
    let messages = vec![
        Control::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
            device: None,
            session_id: "7f3c9a2b".into(),
            app: "webviewer/0.1".into(),
        },
        Control::AuthInit {
            viewer_pub: "<base64 SEC1 uncompressed>".into(),
            nonce: "<base64 16 bytes>".into(),
        },
        Control::AuthFinish {
            agent_pub: "<base64 SEC1 uncompressed>".into(),
            proof: "<base64 HMAC-SHA256>".into(),
        },
        Control::AuthProof {
            proof: "<base64 HMAC-SHA256>".into(),
        },
        Control::AuthOk,
        Control::AuthFail {
            reason: "pairing code mismatch".into(),
        },
        Control::Caps {
            capabilities: Capabilities::default(),
            device: Some(DeviceInfo {
                device_id: "f2b1".into(),
                name: "Studio".into(),
                platform: "macos-aarch64".into(),
                version: "0.1.0".into(),
                tags: vec!["agent".into()],
            }),
        },
        Control::Transport {
            udp: Some("203.0.113.7:51820".into()),
            token: "t".into(),
        },
        Control::VideoConfig(VideoConfig::default()),
        Control::DisplayList {
            displays: vec![DisplayInfo {
                id: 1,
                name: "Built-in".into(),
                width: 2560,
                height: 1440,
                scale: 2.0,
                primary: true,
            }],
        },
        Control::FrameMeta {
            frame_id: 42,
            display: 1,
            width: 1600,
            height: 900,
            codec: Codec::Jpeg,
            keyframe: true,
            tiles: 1,
        },
        Control::Input {
            event: InputEvent::MouseMove {
                x: 32768,
                y: 16384,
                display: 1,
            },
        },
        Control::Input {
            event: InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 32768,
                y: 16384,
                display: 1,
            },
        },
        Control::Input {
            event: InputEvent::Wheel {
                dx: 0,
                dy: -3,
                x: 32768,
                y: 16384,
                display: 1,
            },
        },
        Control::Input {
            event: InputEvent::Key {
                code: "KeyA".into(),
                down: true,
                modifiers: modifiers::SHIFT,
            },
        },
        Control::Input {
            event: InputEvent::Text {
                text: "你好".into(),
            },
        },
        Control::Input {
            event: InputEvent::Touch {
                phase: TouchPhase::Move,
                points: vec![TouchPoint {
                    id: 0,
                    x: 100,
                    y: 200,
                }],
                display: 0,
            },
        },
        Control::Input {
            event: InputEvent::ReleaseAll,
        },
        Control::Clipboard(ClipboardMsg {
            direction: ClipboardDirection::ViewerToAgent,
            text: "hello".into(),
        }),
        Control::Cursor {
            visible: true,
            shape: "default".into(),
            x: 100,
            y: 200,
        },
        Control::Stats(Stats::default()),
        Control::Ping { ts: 1730000000000 },
        Control::Pong { ts: 1730000000000 },
        Control::Warning {
            code: "capture".into(),
            message: "screen recording permission missing".into(),
        },
        Control::Bye {
            reason: "viewer closed".into(),
        },
        Control::Error {
            code: "protocol".into(),
            message: "unexpected frame".into(),
        },
    ];

    for msg in messages {
        println!("{}", msg.to_json());
    }
}
