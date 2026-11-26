//! Read-only terminal preview. No local GUI, keyboard forwarding or clipboard.
mod preview;

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::{cursor, execute, queue, terminal};
use vr_client::Viewer;
use vr_net::connection_code::{self, Destination};
use vr_proto::{Capabilities, Codec, Control};
use vr_session::{Event, SessionError};

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const CAPTURE_WIDTH: u16 = 960;
const CAPTURE_HEIGHT: u16 = 540;
const CAPTURE_FPS: u8 = 5;

#[derive(Parser)]
#[command(version, about = "使用联机码在终端只读预览 Velarouter 远程桌面")]
struct Args {
    /// 对方的联机码：VR-…、12 位互联网码或自建信令的 9 位码
    code: String,
    /// 9 位联机码使用的自建信令 HTTPS 地址
    #[arg(long, env = "VELAROUTER_SIGNAL")]
    signal: Option<String>,
    /// 12 位联机码使用的自建会合服务（可重复）
    #[arg(long)]
    broker: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if std::env::var("TERM").as_deref() == Ok("dumb") || !io::stdout().is_terminal() {
        bail!("请在支持 ANSI 的交互式终端中运行；不向文件或管道输出画面。");
    }
    let destination = connection_code::parse(&args.code).map_err(anyhow::Error::msg)?;
    if matches!(destination, Destination::Signal(_)) && args.signal.is_none() {
        bail!("9 位联机码需要通过 --signal 指定两端使用的自建信令服务器。");
    }
    let password = rpassword::prompt_password("连接密码（不会回显）：")?;
    if password.is_empty() {
        bail!("连接密码不能为空。");
    }
    eprintln!("正在连接 · 只读预览 · Ctrl+C 退出");
    let mut interrupt = std::pin::pin!(tokio::signal::ctrl_c());
    let mut viewer = tokio::select! {
        received = &mut interrupt => return received.context("无法监听终端退出信号"),
        connected = dial(destination, &args, &password) => connected?,
    };
    drop(password);
    let mut terminal = Screen::enter(io::stdout())?;
    let result = tokio::select! {
        received = &mut interrupt => received.context("无法监听终端退出信号"),
        result = show(&mut viewer, &mut terminal) => result,
    };
    // Restore even on decode/network errors, then make a best effort to close.
    drop(terminal);
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        viewer.sender.send_control(Control::Bye {
            reason: "terminal preview closed".into(),
        }),
    )
    .await;
    result
}

fn capabilities() -> Capabilities {
    Capabilities {
        codecs: vec![Codec::Jpeg],
        max_width: CAPTURE_WIDTH,
        max_height: CAPTURE_HEIGHT,
        max_fps: CAPTURE_FPS,
        input: false,
        clipboard: false,
        multi_display: false,
        audio: false,
    }
}

async fn dial(destination: Destination, args: &Args, password: &str) -> Result<Viewer> {
    let app = concat!("velarouter-view/", env!("CARGO_PKG_VERSION"));
    let caps = capabilities();
    let connect_timeout = match destination {
        Destination::Direct(_) => Duration::from_secs(8),
        _ => Duration::from_secs(55),
    };
    let result = tokio::time::timeout(connect_timeout, async {
        match destination {
            Destination::Direct(addr) => {
                vr_client::connect_lan(&format!("ws://{addr}/vr"), password, app, caps).await
            }
            Destination::Meet(code) => {
                vr_client::connect_meet(&code, password, &args.broker, app, caps).await
            }
            Destination::Signal(code) => {
                let signal = args.signal.as_deref().context("缺少 --signal")?;
                vr_client::connect_p2p(signal, &code, password, app, caps).await
            }
        }
    })
    .await
    .context("连接超时")?;
    result.map_err(|error| match error.downcast_ref::<SessionError>() {
        Some(SessionError::Rejected(reason)) if reason == vr_session::UNTRUSTED_VIEWER_REASON => {
            anyhow::anyhow!("对方仅允许可信设备；请使用已配对的桌面客户端。终端预览不支持此限制。")
        }
        // Peer-supplied reason strings are never printed to the terminal.
        Some(SessionError::Rejected(_) | SessionError::Auth(_)) => {
            anyhow::anyhow!("连接未通过认证，请核对密码及被控端的连接权限。")
        }
        _ => anyhow::anyhow!("未能建立连接，请检查联机码、共享状态和连接网络。"),
    })
}

enum Update<'a> {
    Waiting,
    Frame(&'a image::GrayImage),
    CheckSize(Option<&'a image::GrayImage>),
}

async fn show(viewer: &mut Viewer, screen: &mut Screen<io::Stdout>) -> Result<()> {
    receive_preview(viewer, |update| match update {
        Update::Waiting => Ok(screen.draw("Waiting for video...  Ctrl+C", None)?),
        Update::Frame(frame) => Ok(screen.draw("READ ONLY  |  Ctrl+C to exit", Some(frame))?),
        Update::CheckSize(frame) => screen.redraw_if_resized(frame),
    })
    .await
}

async fn receive_preview(
    viewer: &mut Viewer,
    mut update: impl FnMut(Update<'_>) -> Result<()>,
) -> Result<()> {
    if !viewer.peer.capabilities.codecs.contains(&Codec::Jpeg) {
        bail!("对方未提供 JPEG 画面，终端预览暂不支持此设备。");
    }
    vr_client::request_video(viewer, 45, CAPTURE_FPS, 0, CAPTURE_WIDTH, CAPTURE_HEIGHT).await?;
    update(Update::Waiting)?;
    let mut last_frame = None;
    let started = tokio::time::Instant::now();
    loop {
        // A stationary desktop may not send frames. Its other control traffic
        // still keeps the preview open; lack of the first frame is an error.
        let idle = if last_frame.is_none() {
            STREAM_IDLE_TIMEOUT.saturating_sub(started.elapsed())
        } else {
            STREAM_IDLE_TIMEOUT
        };
        let event = tokio::time::timeout(idle, viewer.receiver.recv_auto_pong(&mut viewer.sender))
            .await
            .context("没有收到远端画面或会话已超时")?;
        match event {
            Ok(Event::Media { header, payload }) => {
                let frame = preview::decode(header, &payload)?;
                update(Update::Frame(&frame))?;
                last_frame = Some(frame);
            }
            Ok(Event::Control(Control::Stats(_))) => {
                update(Update::CheckSize(last_frame.as_ref()))?
            }
            Ok(Event::Closed(_)) | Err(SessionError::Closed) => return Ok(()),
            Ok(_) => {}
            Err(_) => bail!("远端会话异常中断。"),
        }
    }
}

struct Screen<W: Write> {
    out: W,
    size: Option<(u16, u16)>,
}

impl<W: Write> Screen<W> {
    fn enter(out: W) -> io::Result<Self> {
        let mut screen = Self { out, size: None };
        execute!(screen.out, terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(screen)
    }

    fn draw(&mut self, title: &str, frame: Option<&image::GrayImage>) -> io::Result<()> {
        let size = terminal::size()?;
        self.size = Some(size);
        queue!(
            self.out,
            terminal::Clear(terminal::ClearType::All),
            cursor::MoveTo(0, 0)
        )?;
        let status = if usize::from(size.0) <= title.len() {
            "Ctrl+C"
        } else {
            title
        };
        if size.0 > 6 {
            write!(self.out, "{status}")?;
        }
        if let Some(frame) = frame.filter(|_| size.1 > 2) {
            queue!(self.out, cursor::MoveTo(0, 1))?;
            self.out
                .write_all(preview::render(frame, size).as_bytes())?;
        }
        self.out.flush()
    }

    fn redraw_if_resized(&mut self, frame: Option<&image::GrayImage>) -> Result<()> {
        let size = terminal::size()?;
        if self.size != Some(size) {
            let title = if frame.is_some() {
                "READ ONLY  |  Ctrl+C to exit"
            } else {
                "Waiting for video...  Ctrl+C"
            };
            self.draw(title, frame)?;
        }
        Ok(())
    }
}

impl<W: Write> Drop for Screen<W> {
    fn drop(&mut self) {
        let _ = execute!(self.out, cursor::Show, terminal::LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preview_authenticates_requests_jpeg_and_only_replies_to_ping() {
        let (viewer_channel, host_channel) = vr_session::memory_channel_pair();
        let config = |capabilities| vr_session::SessionConfig {
            session_id: "preview".into(),
            secret: "test-secret".into(),
            app: "test".into(),
            device: None,
            capabilities,
        };
        let (viewer, host) = tokio::join!(
            vr_session::Session::connect(Box::new(viewer_channel), config(capabilities())),
            vr_session::Session::accept(
                Box::new(host_channel),
                config(vr_client::default_capabilities())
            ),
        );
        let (sender, receiver, peer) = viewer.unwrap().into_parts();
        let mut viewer = Viewer {
            sender,
            receiver,
            peer,
            transport: "test",
            session_id: "preview".into(),
        };
        let mut host = host.unwrap();
        let mut received = false;
        let exercise_host = async {
            assert!(
                matches!(host.recv().await.unwrap(), Event::Control(Control::VideoConfig(config)) if config.codec == Codec::Jpeg && config.fps == CAPTURE_FPS)
            );
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut bytes)
                .encode(&[255, 0], 2, 1, image::ExtendedColorType::L8)
                .unwrap();
            host.send_media(Codec::Jpeg, 0, 2, 1, &bytes).await.unwrap();
            let ts = host.ping().await.unwrap();
            assert!(matches!(host.recv().await.unwrap(), Event::Pong { ts: got } if ts == got));
            host.close("test done").await;
        };
        let exercise_viewer = receive_preview(&mut viewer, |update| {
            if let Update::Frame(frame) = update {
                assert_eq!(frame.dimensions(), (2, 1));
                received = true;
            }
            Ok(())
        });
        let (_, preview) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(exercise_host, exercise_viewer)
        })
        .await
        .unwrap();
        preview.unwrap();
        assert!(received);
    }

    #[test]
    fn preview_never_requests_side_channels() {
        let caps = capabilities();
        assert_eq!(caps.codecs, vec![Codec::Jpeg]);
        assert!(!caps.input && !caps.clipboard && !caps.audio && !caps.multi_display);
        Args::try_parse_from(["velarouter-view", "123456789012"]).unwrap();
        assert!(
            Args::try_parse_from(["velarouter-view", "123456789012", "--password", "hidden"])
                .is_err()
        );
    }
}
