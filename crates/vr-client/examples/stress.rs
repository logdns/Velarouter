//! Reproduces the desktop viewer's loop: `select!` between receiving frames and
//! sending input. Before the channel made writes atomic and reads cancel-safe,
//! this desynchronised the stream within seconds and surfaced as
//! `message too large: <garbage>`.
//!
//! ```sh
//! cargo run -p velarouter -- agent --code 445566 --quality 95 --fps 30 --max-width 3840 --max-height 2160
//! cargo run -p vr-client --example stress -- ws://127.0.0.1:34789/vr 445566 20
//! ```

use std::time::{Duration, Instant};

use vr_client::Viewer;
use vr_proto::{Codec, Control, InputEvent, VideoConfig};
use vr_session::Event;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .unwrap_or_else(|| "ws://127.0.0.1:34789/vr".into());
    let code = args.next().unwrap_or_else(|| "445566".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);

    let mut viewer: Viewer = vr_client::connect_lan(
        &url,
        &code,
        "velarouter-stress/0.1",
        vr_client::default_capabilities(),
    )
    .await?;
    viewer
        .sender
        .send_control(Control::VideoConfig(VideoConfig {
            codec: Codec::Jpeg,
            quality: 95,
            fps: 30,
            max_width: 3840,
            max_height: 2160,
            display: 0,
        }))
        .await?;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0u64;
    let mut bytes = 0u64;
    let mut sent = 0u64;
    let mut x = 0u16;
    while Instant::now() < deadline {
        tokio::select! {
            ev = viewer.receiver.recv_auto_pong(&mut viewer.sender) => {
                match ev? {
                    Event::Media { header, payload } => {
                        frames += 1;
                        bytes += payload.len() as u64;
                        assert!(header.width > 0);
                    }
                    Event::Closed(reason) => anyhow::bail!("closed early: {reason:?} after {frames} frames"),
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(8)) => {
                // Same cadence as a mouse being moved by a person.
                x = x.wrapping_add(700);
                viewer.sender.send_control(Control::Input {
                    event: InputEvent::MouseMove { x, y: 32768, display: 0 },
                }).await?;
                sent += 1;
            }
        }
    }
    println!("ok: {frames} frames, {bytes} bytes, {sent} input events, session stayed in sync");
    Ok(())
}
