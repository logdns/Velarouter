//! LAN HTTP + WebSocket server.
//!
//! One port serves everything a viewer needs:
//!
//! * `GET /` — the reference web viewer (also the integration test for the JS SDK)
//! * `GET /info` — device, displays, capabilities (never the pairing code)
//! * `GET /vr?sid=…` — the encrypted VRP session
//! * `GET /healthz` — liveness for supervisors
//!
//! Serving the viewer from the agent means a LAN session needs no internet
//! access at all: open `http://<agent-ip>:34789`, type the code shown on the
//! agent, done.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vr_proto::{
    Capabilities, ClipboardDirection, ClipboardMsg, Codec, Control, DisplayInfo, FileHeader,
    InputEvent, Stats, VideoConfig,
};
use vr_session::{Event, Session, SessionConfig, SessionSender};
use vr_transport::{ChannelMsg, MessageChannel, MessageReceiver, MessageSender};

use crate::capture::{EncodedFrame, FrameSource, ScreenCapturer};
use crate::input::{DesktopInjector, DisplayRect, Injector};
use crate::transfer::{FileOut, TransferManager};
use crate::{AgentState, StreamControl, AGENT_VERSION};

const INDEX_HTML: &str = include_str!("../../../webviewer/index.html");
const APP_JS: &str = include_str!("../../../webviewer/app.js");
const STYLE_CSS: &str = include_str!("../../../webviewer/style.css");
const NOBLE_JS: &str = include_str!("../../../webviewer/vendor/noble.js");

/// Starts the LAN server. Returns when the listener fails or the task is aborted.
pub async fn serve(state: Arc<AgentState>, listener: tokio::net::TcpListener) -> Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/app.js", get(|| async { js(APP_JS) }))
        .route("/style.css", get(|| async { css(STYLE_CSS) }))
        .route("/vendor/noble.js", get(|| async { js(NOBLE_JS) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/info", get(info))
        .route("/vr", get(vr_ws))
        .with_state(state.clone());

    info!(addr = %state.config.bind, "lan server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn js(body: &'static str) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        body,
    )
}

fn css(body: &'static str) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        body,
    )
}

async fn info(State(state): State<Arc<AgentState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "device": state.info,
        "displays": state.displays,
        "capabilities": state.caps,
        "version": AGENT_VERSION,
        "protocol": vr_proto::PROTOCOL_VERSION,
        "port": state.config.bind.port(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VrQuery {
    #[serde(default)]
    sid: Option<String>,
}

async fn vr_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AgentState>>,
    Query(q): Query<VrQuery>,
) -> impl IntoResponse {
    ws.max_message_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            let peer = "lan".to_string();
            // An empty id tells the session engine to adopt the viewer's id, so a
            // bare `ws://host:port/vr` works for LAN connections.
            let session_id = q.sid.unwrap_or_default();
            let secret = state.secret.clone();
            let cfg = crate::session_config(&state, &session_id, secret);
            let channel: Box<dyn MessageChannel> = Box::new(AxumChannel { socket, peer });
            match crate::accept_viewer(&state, channel, cfg).await {
                Ok(session) => {
                    info!(%session_id, "viewer authenticated over lan");
                    if let Err(e) = run_session(session, state).await {
                        debug!(%e, "session ended");
                    }
                }
                Err(e) => warn!(%e, "handshake rejected"),
            }
        })
}

// ---------------------------------------------------------------------------
// Session pump
// ---------------------------------------------------------------------------

struct Presence<'a>(&'a Option<mpsc::UnboundedSender<crate::AgentEvent>>);
impl<'a> Presence<'a> {
    fn joined(events: &'a Option<mpsc::UnboundedSender<crate::AgentEvent>>) -> Self {
        if let Some(tx) = events {
            let _ = tx.send(crate::AgentEvent::SessionJoined);
        }
        Self(events)
    }
}
impl Drop for Presence<'_> {
    fn drop(&mut self) {
        if let Some(tx) = self.0 {
            let _ = tx.send(crate::AgentEvent::SessionLeft);
        }
    }
}
/// Runs one authenticated session: capture out, input in.
pub async fn run_session(session: Session, state: Arc<AgentState>) -> Result<()> {
    let _presence = Presence::joined(&state.events);
    let peer = session.peer().clone();
    let (mut sender, mut receiver, _peer) = session.into_parts();
    let view_only = peer.app.starts_with(vr_proto::WALL_APP_PREFIX);
    if view_only {
        sender.send_control(Control::ViewOnly).await?;
    }

    if !peer.capabilities.codecs.is_empty() && !peer.capabilities.codecs.contains(&Codec::Jpeg) {
        sender
            .send_control(Control::Warning {
                code: "codec".into(),
                message: "viewer does not offer JPEG; falling back to JPEG anyway".into(),
            })
            .await?;
    }

    #[cfg(target_os = "linux")]
    let mut permission = state.portal.clone();
    #[cfg(target_os = "linux")]
    let authorized_display = permission
        .as_ref()
        .and_then(|p| p.borrow().as_ref().map(|g| g.display));
    #[cfg(not(target_os = "linux"))]
    let authorized_display: Option<u32> = None;
    sender
        .send_control(Control::DisplayList {
            displays: state
                .displays
                .iter()
                .filter(|d| authorized_display.is_none_or(|id| id == d.id))
                .cloned()
                .collect(),
        })
        .await?;

    // Clamp the viewer's wish against both the agent defaults and its own caps.
    let mut video = VideoConfig {
        quality: state.config.quality,
        fps: state.config.fps,
        max_width: state.config.max_width,
        max_height: state.config.max_height,
        display: if state.config.display == 0 {
            state
                .displays
                .iter()
                .find(|d| d.primary)
                .or_else(|| state.displays.first())
                .map_or(0, |d| d.id)
        } else {
            state.config.display
        },
        ..Default::default()
    };
    if let Some(id) = authorized_display {
        video.display = id;
    }
    if peer.capabilities.max_fps > 0 {
        video.fps = video.fps.min(peer.capabilities.max_fps.max(1));
    }
    if peer.capabilities.max_width > 0 {
        video.max_width = video.max_width.min(peer.capabilities.max_width);
    }
    if peer.capabilities.max_height > 0 {
        video.max_height = video.max_height.min(peer.capabilities.max_height);
    }

    if view_only {
        video.max_width = video.max_width.min(vr_proto::WALL_WIDTH);
        video.max_height = video.max_height.min(vr_proto::WALL_HEIGHT);
        video.fps = video.fps.min(vr_proto::WALL_FPS);
    }
    let control = Arc::new(StreamControl::new(video.quality, video.display));
    let (frame_tx, mut frame_rx) = mpsc::channel::<EncodedFrame>(2);
    let (file_tx, mut file_rx) = mpsc::channel::<FileOut>(4);
    let mut transfer = TransferManager::new(file_tx);
    let mut capture = spawn_capture(control.clone(), &video, frame_tx.clone())?;

    // Creating the injector is deferred to the first input event: a transient
    // failure (window server busy right after login, permission just granted)
    // must not turn the whole session view-only.
    let mut injector: Option<DesktopInjector> = None;
    let mut injector_tried = false;

    #[cfg(target_os = "linux")]
    let mut portal_held = crate::portal_input::Held::default();
    #[cfg(target_os = "linux")]
    if permission.as_ref().is_some_and(|p| p.borrow().is_none()) {
        sender.send_control(Control::Warning { code:"input".into(), message:"被控端尚未授权 Wayland 远程交互。请在系统弹窗中允许键鼠，或在设置 → 权限重新申请。".into() }).await?;
    }
    let mut input_blocked = false;
    let mut last_inbound = Instant::now();
    let mut last_pong = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(3));
    let mut stats_timer = tokio::time::interval(Duration::from_secs(1));
    let mut ping_timer = tokio::time::interval(Duration::from_secs(3));
    let mut clipboard_timer = tokio::time::interval(Duration::from_secs(1));
    let mut frames: u32 = 0;
    let mut bytes: u64 = 0;
    let mut last_capture_ms = 0f32;
    let mut last_encode_ms = 0f32;
    let mut last_rtt_ms = 0u32;
    let mut last_clipboard: Option<String> = None;

    #[cfg(feature = "linux-audio")]
    let mut audio = AudioStream::default();
    let mut shutdown = state.config.shutdown.clone();
    let result: Result<()> = loop {
        tokio::select! {
            _ = async { match shutdown.as_mut() { Some(rx) => { let _ = rx.wait_for(|v| *v).await; }, None => std::future::pending::<()>().await } } => break Ok(()),
            _ = async {
                #[cfg(target_os = "linux")]
                if let Some(p) = permission.as_mut() { if p.changed().await.is_ok() { return; } }
                std::future::pending::<()>().await
            } => {
                #[cfg(target_os = "linux")]
                {
                    let display = permission.as_mut().and_then(|p| p.borrow_and_update().as_ref().map(|g|g.display));
                    let allowed = display.is_some();
                    // The previous portal session is gone; never carry pressed
                    // keys into a new grant.
                    portal_held = crate::portal_input::Held::default();
                    if let Some(id) = display {
                        control.display.store(id, Ordering::Relaxed);
                        sender.send_control(Control::DisplayList { displays:state.displays.iter().filter(|d| d.id==id).cloned().collect() }).await?;
                    }
                    sender.send_control(Control::Warning {code: if allowed {"input_ready"} else {"input"}.into(), message: if allowed {"系统已允许远程交互"} else {"被控端已结束远程交互授权"}.into() }).await?;
                }
            }
            _audio_event = async {
                #[cfg(feature = "linux-audio")]
                return audio.next().await;
                #[cfg(not(feature = "linux-audio"))]
                std::future::pending::<()>().await
            } => {
                #[cfg(feature = "linux-audio")]
                match _audio_event {
                    AudioEvent::Packet(pcm) => {
                        if let Err(error) = sender.send_audio(&pcm).await { break Err(error.into()); }
                    }
                    AudioEvent::Status(status) => {
                        if let Err(error) = sender.send_control(status).await { break Err(error.into()); }
                    }
                }

            }
            frame = frame_rx.recv() => {
                match frame {
                    Some(frame) => {
                        if frame.generation != control.generation.load(Ordering::Relaxed) {
                            continue;
                        }
                        last_capture_ms = frame.capture.as_secs_f32() * 1000.0;
                        last_encode_ms = frame.encode.as_secs_f32() * 1000.0;
                        let mut header = vr_proto::VideoHeader::new(frame.codec, 0, frame.display, frame.width, frame.height);
                        if frame.keyframe { header.flags |= vr_proto::frame_flags::KEYFRAME; }
                        if let Err(e) = sender
                            .send_video_frame(header, &frame.payload)
                            .await
                        {
                            break Err(e.into());
                        }
                        frames += 1;
                        bytes += frame.payload.len() as u64;
                    }
                    None => break Ok(()),
                }
            }
            event = receiver.recv() => {
                last_inbound = Instant::now();
                match event {
                    Ok(Event::Control(Control::Input { event })) => {
                        if view_only { continue; }
                        #[cfg(target_os = "linux")]
                        if let Some(p) = &state.portal {
                            let grant = p.borrow().clone();
                            if let Some(g) = grant {
                                if let Err(e) = g.apply(&event,&mut portal_held).await {
                                    g.release(&mut portal_held).await;
                                    sender.send_control(Control::Warning { code:"input".into(),message:format!("{e}；请检查授权、屏幕选择并重新连接") }).await?;
                                }
                            }
                            continue; // No X11 fallback on a denied Wayland session.
                        }
                        if input_blocked { continue; }
                        // `RUST_LOG=vr_agent::server=trace` prints every injected
                        // event: the fastest way to prove a viewer's input
                        // reaches this machine at all.
                        if tracing::enabled!(tracing::Level::TRACE) {
                            tracing::trace!(?event, "input event");
                        }
                        if injector.is_none() && !injector_tried {
                            injector_tried = true;
                            match DesktopInjector::new(displays_as_rects(&state.displays)) {
                                Ok(sink) => injector = Some(sink),
                                Err(e) => {
                                    warn!(%e, "cannot create the input sink");
                                    let message = if crate::input::accessibility_granted() {
                                        format!("被控端无法创建键鼠注入器：{e}")
                                    } else {
                                        "被控端未授权键鼠控制：macOS 请在「系统设置 → 隐私与安全性 → 辅助功能」允许 Velarouter，然后完全退出并重新打开。".to_string()
                                    };
                                    let _ = sender
                                        .send_control(Control::Warning {
                                            code: "input".into(),
                                            message,
                                        })
                                        .await;
                                    input_blocked = true;
                                }
                            }
                        }
                        if let Some(sink) = injector.as_mut() {
                            if let Err(e) = sink.apply(&event) {
                                warn!(%e, "input injection failed");
                                let _ = sender.send_control(Control::Warning { code: "input".into(), message: "系统拒绝了键鼠输入，请检查被控端权限并重新连接。".into() }).await;
                                let _ = sink.release_all();
                                input_blocked = true;
                            }
                        }
                    }
                    Ok(Event::Control(Control::AudioRequest { enabled })) => {
                        // Do not send new protocol messages to old viewers.
                        if view_only || !peer.capabilities.audio { continue; }
                        let status = if !state.config.share_system_audio || !state.caps.audio {
                            Control::AudioStatus { active: false, message: "被控端未允许系统声音，或音频设备不可用。".into() }
                        } else {
                            #[cfg(feature = "linux-audio")]
                            {
                                if !enabled {
                                    audio.stop();
                                    Control::AudioStatus { active: false, message: String::new() }
                                } else if audio.capture.is_some() {
                                    Control::AudioStatus { active: true, message: String::new() }
                                } else {
                                    audio.start();
                                    continue; // A slow audio device must never stall video or input.
                                }
                            }
                            #[cfg(not(feature = "linux-audio"))]
                            {
                                let _ = enabled;
                                Control::AudioStatus { active: false, message: "此版本不支持系统音频。".into() }
                            }
                        };
                        if let Err(error) = sender.send_control(status).await { break Err(error.into()); }
                    }
                    Ok(Event::Control(Control::VideoConfig(cfg))) => {
                        control.quality.store(cfg.quality.clamp(20, 95), Ordering::Relaxed);
                        #[cfg(target_os="linux")]
                        let selected=state.portal.as_ref().and_then(|p| p.borrow().as_ref().map(|g|g.display)).unwrap_or(cfg.display);
                        #[cfg(not(target_os="linux"))]
                        let selected=cfg.display;
                        if state.displays.iter().any(|d| d.id == selected) {control.display.store(selected, Ordering::Relaxed); }
                        video.quality = cfg.quality.clamp(20, 95);
                        video.display = control.display();
                        let (codec, max_fps, width_limit, height_limit) = if view_only {
                            (Codec::Jpeg, vr_proto::WALL_FPS, vr_proto::WALL_WIDTH, vr_proto::WALL_HEIGHT)
                        } else {
                            let codec = vr_video::capture_codec(cfg.codec, &state.caps.codecs, &peer.capabilities.codecs);
                            (codec, if codec == Codec::H264 { 30 } else { 60 }, 3840, 2160)
                        };
                        let fps = cfg.fps.clamp(1, max_fps);
                        let max_width = cfg.max_width.clamp(320, width_limit);
                        let max_height = cfg.max_height.clamp(240, height_limit);
                        if codec != video.codec || fps != video.fps
                            || max_width != video.max_width
                            || max_height != video.max_height
                        {
                            // Geometry and frame rate are baked into the capture
                            // thread, so restart it instead of deferring to the
                            // next session: the viewer asked for 2K/4K now.
                            video.codec = codec;
                            video.fps = fps;
                            video.max_width = max_width;
                            video.max_height = max_height;
                            control.generation.fetch_add(1, Ordering::Relaxed);
                            if let Some(old_capture) = capture.take() {
                                // Do not overlap platform capture pipelines across a reconfigure.
                                let _ = tokio::task::spawn_blocking(move || old_capture.join()).await;
                            }
                            capture = spawn_capture(control.clone(), &video, frame_tx.clone())?;
                            info!(
                                fps = video.fps,
                                width = video.max_width,
                                height = video.max_height,
                                "capture restarted"
                            );
                        }
                    }
                    Ok(Event::Control(Control::Clipboard(ClipboardMsg { direction, text }))) => {
                        if !view_only && direction == ClipboardDirection::ViewerToAgent {
                            set_clipboard(text.clone()).await;
                            last_clipboard = Some(text);
                        }
                    }
                    Ok(Event::Control(Control::Ping { ts })) => {
                        sender.send_control(Control::Pong { ts }).await.ok();
                    }
                    Ok(Event::Pong { ts }) => {
                        last_pong = Instant::now();
                        last_rtt_ms = now_ms().saturating_sub(ts) as u32;
                    }
                    Ok(Event::Control(Control::Bye { .. })) | Ok(Event::Closed(_)) => break Ok(()),
                    Ok(Event::Control(Control::Stats(_))) => {}
                    Ok(Event::Control(control)) => {
                        if view_only { continue; }
                        let replies = transfer.handle_control(control, &state.files).await;
                        for reply in replies {
                            let _ = sender.send_control(reply).await;
                        }
                    }
                    Ok(Event::File { header, payload }) => {
                        if view_only { continue; }
                        let replies = transfer.handle_chunk(header, &payload).await;
                        for reply in replies {
                            let _ = sender.send_control(reply).await;
                        }
                    }
                    Ok(Event::Media { .. } | Event::Audio(_)) => {
                        // Viewers never send media; ignoring is safer than
                        // closing a session over a stray frame.
                    }
                    Err(vr_session::SessionError::Closed) => break Ok(()),
                    Err(e) => break Err(e.into()),
                }
            }
            _ = stats_timer.tick() => {
                let seconds = 1.0f64;
                let stats = Stats {
                    fps: (frames as f64 / seconds) as f32,
                    kbps: (bytes as f64 * 8.0 / 1000.0 / seconds) as u32,
                    rtt_ms: last_rtt_ms,
                    capture_ms: last_capture_ms,
                    encode_ms: last_encode_ms,
                    decode_ms: 0.0,
                    dropped: 0,
                    e2e_ms: 0,
                };
                frames = 0;
                bytes = 0;
                if sender.send_control(Control::Stats(stats)).await.is_err() {
                    break Ok(());
                }
            }
            _ = ping_timer.tick() => {
                if sender.ping().await.is_err() {
                    break Ok(());
                }
            }
            out = file_rx.recv() => {
                match out {
                    Some(FileOut::Chunk { transfer_id, offset, seq, last, data }) => {
                        let header = FileHeader::new(transfer_id, offset, seq, data.len() as u32, last);
                        if sender.send_file_chunk(header, &data).await.is_err() {
                            break Ok(());
                        }
                    }
                    Some(FileOut::Done { transfer_id, path, sha256, total }) => {
                        let _ = sender.send_control(Control::FileProgress {
                            transfer_id, offset: total, total,
                        }).await;
                        let _ = sender.send_control(Control::FileComplete {
                            transfer_id, path, sha256,
                        }).await;
                    }
                    Some(FileOut::Error { transfer_id, code, message }) => {
                        let _ = sender.send_control(Control::FileError {
                            transfer_id, code: code.to_string(), message,
                        }).await;
                    }
                    None => {}
                }
            }
            _ = watchdog.tick() => {
                // A frozen or sleeping viewer used to hold the only session slot
                // forever, which is why the second connection "did nothing".
                if last_pong.elapsed() > Duration::from_secs(7)
                    || last_inbound.elapsed() > Duration::from_secs(30)
                {
                    warn!(idle = ?last_inbound.elapsed(), "viewer silent; ending the session");
                    let _ = sender.send_control(Control::Bye { reason: "idle".into() }).await;
                    break Ok(());
                }
            }
            _ = clipboard_timer.tick() => {
                if !view_only && peer.capabilities.clipboard {
                    if let Some(text) = read_clipboard().await {
                        if !text.is_empty() && last_clipboard.as_deref() != Some(text.as_str()) {
                            last_clipboard = Some(text.clone());
                            let _ = sender.send_control(Control::Clipboard(ClipboardMsg {
                                direction: ClipboardDirection::AgentToViewer,
                                text,
                            })).await;
                        }
                    }
                }
            }
        }
    };

    #[cfg(feature = "linux-audio")]
    drop(audio);
    #[cfg(target_os = "linux")]
    if let Some(g) = state.portal.as_ref().and_then(|p| p.borrow().clone()) {
        g.release(&mut portal_held).await;
    }
    if let Some(injector) = injector.as_mut() {
        let _ = injector.release_all();
    }
    // Stop capture before waiting for the worker to finish.
    control.set_paused(true);
    control.stop.store(1, Ordering::Relaxed);
    drop(frame_rx);
    if let Some(handle) = capture {
        let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    }
    result
}

#[cfg(feature = "linux-audio")]
#[derive(Default)]
struct AudioStream {
    opening: Option<tokio::task::JoinHandle<Result<vr_audio::Capture>>>,
    capture: Option<vr_audio::Capture>,
}

#[cfg(feature = "linux-audio")]
enum AudioEvent {
    Packet(Vec<u8>),
    Status(Control),
}

#[cfg(feature = "linux-audio")]
impl AudioStream {
    fn start(&mut self) {
        if self.opening.is_none() {
            self.opening = Some(tokio::spawn(vr_audio::Capture::start()));
        }
    }

    fn stop(&mut self) {
        if let Some(task) = self.opening.take() {
            task.abort();
        }
        self.capture = None;
    }

    async fn next(&mut self) -> AudioEvent {
        if let Some(task) = &mut self.opening {
            let result = task.await;
            self.opening = None;
            if let Ok(Ok(capture)) = result {
                self.capture = Some(capture);
                return AudioEvent::Status(Control::AudioStatus {
                    active: true,
                    message: String::new(),
                });
            }
            return AudioEvent::Status(Control::AudioStatus {
                active: false,
                message: "系统输出采集不可用，画面仍可继续。".into(),
            });
        }
        if let Some(capture) = &mut self.capture {
            if let Some(packet) = capture.recv().await {
                return AudioEvent::Packet(packet);
            }
            self.capture = None;
            return AudioEvent::Status(Control::AudioStatus {
                active: false,
                message: "系统音频采集已停止。".into(),
            });
        }
        std::future::pending().await
    }
}

#[cfg(feature = "linux-audio")]
impl Drop for AudioStream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_capture(
    control: Arc<StreamControl>,
    video: &VideoConfig,
    tx: mpsc::Sender<EncodedFrame>,
) -> Result<Option<std::thread::JoinHandle<()>>> {
    let generation = control.generation.load(Ordering::Relaxed);
    let mut capturer = match ScreenCapturer::new(
        video.max_width,
        video.max_height,
        video.quality,
        video.fps,
        video.display,
    ) {
        Ok(c) => c,
        Err(e) => {
            warn!(%e, "capture unavailable; the session will show nothing");
            return Ok(None);
        }
    };
    #[cfg(feature = "h264")]
    if video.codec == Codec::H264 {
        capturer.enable_h264();
    }
    let handle = std::thread::Builder::new()
        .name("vr-capture".into())
        .spawn(move || {
            // A cancelled P2P session drops the receiver before normal teardown.
            while !tx.is_closed()
                && control.stop.load(Ordering::Relaxed) == 0
                && control.generation.load(Ordering::Relaxed) == generation
            {
                if control.paused() {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                capturer.set_quality(control.quality());
                let display = control.display();
                capturer.select_display(display);
                match capturer.next_frame() {
                    Ok(Some(mut frame)) => {
                        frame.generation = generation;
                        if tx.try_send(frame).is_err() {
                            // A dropped delta invalidates future references.
                            // Also retry static JPEG frames lost to backpressure.
                            capturer.frame_dropped();
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        debug!(%e, "capture error");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })
        .context("cannot spawn capture thread")?;
    Ok(Some(handle))
}

fn displays_as_rects(displays: &[DisplayInfo]) -> Vec<DisplayRect> {
    let Ok(monitors) = xcap::Monitor::all() else {
        return displays
            .iter()
            .enumerate()
            .map(|(i, d)| DisplayRect {
                id: d.id,
                x: (i as i32) * d.width as i32,
                y: 0,
                width: d.width,
                height: d.height,
                scale: d.scale,
            })
            .collect();
    };
    let mut rects: Vec<DisplayRect> = monitors
        .iter()
        .filter_map(|m| {
            Some(DisplayRect {
                id: m.id().ok()?,
                x: m.x().ok()?,
                y: m.y().ok()?,
                width: m.width().ok()?,
                height: m.height().ok()?,
                scale: m.scale_factor().ok().unwrap_or(1.0),
            })
        })
        .collect();
    // The primary display must come first: an unknown display id falls back to
    // it, and the wrong fallback would silently move the pointer to another
    // monitor (which looks exactly like "input does not work").
    if let Some(index) = monitors
        .iter()
        .position(|m| m.is_primary().unwrap_or(false))
    {
        let primary = rects.remove(index);
        rects.insert(0, primary);
    }
    rects
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn set_clipboard(text: String) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text);
        }
    })
    .await;
}

async fn read_clipboard() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        arboard::Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_text().ok())
    })
    .await
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// Axum WebSocket adapter
// ---------------------------------------------------------------------------

pub struct AxumChannel {
    socket: WebSocket,
    peer: String,
}

#[async_trait::async_trait]
impl MessageChannel for AxumChannel {
    async fn send_text(&mut self, text: &str) -> Result<(), vr_transport::TransportError> {
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| vr_transport::TransportError::Ws(e.to_string()))
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), vr_transport::TransportError> {
        self.socket
            .send(Message::Binary(data.to_vec().into()))
            .await
            .map_err(|e| vr_transport::TransportError::Ws(e.to_string()))
    }
    async fn recv(&mut self) -> Result<ChannelMsg, vr_transport::TransportError> {
        loop {
            match self.socket.recv().await {
                Some(Ok(Message::Text(t))) => return Ok(ChannelMsg::Text(t.to_string())),
                Some(Ok(Message::Binary(b))) => return Ok(ChannelMsg::Binary(b.to_vec())),
                Some(Ok(Message::Close(_))) | None => return Ok(ChannelMsg::Closed),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(vr_transport::TransportError::Ws(e.to_string())),
            }
        }
    }
    fn peer(&self) -> String {
        self.peer.clone()
    }
    fn split(self: Box<Self>) -> (Box<dyn MessageSender>, Box<dyn MessageReceiver>) {
        let (sink, stream) = self.socket.split();
        (
            Box::new(AxumSender {
                sink,
                peer: self.peer,
            }),
            Box::new(AxumReceiver { stream }),
        )
    }
}

struct AxumSender {
    sink: futures_util::stream::SplitSink<WebSocket, Message>,
    peer: String,
}

#[async_trait::async_trait]
impl MessageSender for AxumSender {
    async fn send_text(&mut self, text: &str) -> Result<(), vr_transport::TransportError> {
        self.sink
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| vr_transport::TransportError::Ws(e.to_string()))
    }
    async fn send_binary(&mut self, data: &[u8]) -> Result<(), vr_transport::TransportError> {
        self.sink
            .send(Message::Binary(data.to_vec().into()))
            .await
            .map_err(|e| vr_transport::TransportError::Ws(e.to_string()))
    }
    fn peer(&self) -> String {
        self.peer.clone()
    }
}

struct AxumReceiver {
    stream: futures_util::stream::SplitStream<WebSocket>,
}

#[async_trait::async_trait]
impl MessageReceiver for AxumReceiver {
    async fn recv(&mut self) -> Result<ChannelMsg, vr_transport::TransportError> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(t))) => return Ok(ChannelMsg::Text(t.to_string())),
                Some(Ok(Message::Binary(b))) => return Ok(ChannelMsg::Binary(b.to_vec())),
                Some(Ok(Message::Close(_))) | None => return Ok(ChannelMsg::Closed),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(vr_transport::TransportError::Ws(e.to_string())),
            }
        }
    }
}

/// Helper used by the CLI to feed a pre-recorded frame into a session (tests).
pub async fn send_test_frame(sender: &mut SessionSender, jpeg: &[u8]) {
    let _ = sender.send_media(Codec::Jpeg, 0, 640, 480, jpeg).await;
}

#[allow(dead_code)]
fn _unused(_: AtomicU8, _: InputEvent, _: Capabilities, _: SessionConfig, _: Instant) {}

#[cfg(test)]
mod direct_tests {
    use super::*;
    use vr_session::WsChannel;

    // Exercise desktop-compatible framing over a real TCP listener; do not move
    // the test runner's cursor, type into its windows or request TCC access.
    #[tokio::test]
    async fn authenticated_mouse_and_keyboard_arrive_over_tcp() {
        let cfg = SessionConfig {
            session_id: "session".into(),
            secret: "temporary-test-secret".into(),
            app: "test".into(),
            device: None,
            capabilities: Capabilities::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = [
            InputEvent::MouseMove {
                x: 501,
                y: 600,
                display: 2,
            },
            InputEvent::Text {
                text: "远程 A".into(),
            },
        ];
        let (tx, mut rx) = mpsc::channel(2);
        let agent = cfg.clone();
        let app = Router::new().route(
            "/vr",
            get(move |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(move |socket| async move {
                    let mut session = Session::accept(
                        Box::new(AxumChannel {
                            socket,
                            peer: "test".into(),
                        }),
                        agent,
                    )
                    .await
                    .unwrap();
                    for _ in 0..2 {
                        if let Event::Control(Control::Input { event }) =
                            session.recv().await.unwrap()
                        {
                            tx.send(event).await.unwrap();
                        }
                    }
                })
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut viewer = Session::connect(
            Box::new(
                WsChannel::connect(&format!("ws://{addr}/vr"))
                    .await
                    .unwrap(),
            ),
            cfg,
        )
        .await
        .unwrap();
        for event in expected {
            viewer
                .send_control(Control::Input {
                    event: event.clone(),
                })
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .unwrap(),
                Some(event)
            );
        }
        task.abort();
    }
}
