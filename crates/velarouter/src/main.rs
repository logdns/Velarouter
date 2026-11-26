//! `velarouter` — one binary for every server-side role.
//!
//! ```text
//! velarouter signal  --bind 0.0.0.0:34790
//! velarouter agent   --code 123456 --port 34789
//! velarouter probe   --url ws://127.0.0.1:34789/vr --code 123456 --save shot.jpg
//! velarouter discover
//! velarouter doctor
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;
use vr_proto::{Capabilities, Codec, Control, FileEntry, FileHeader, VideoConfig, FILE_CHUNK};
use vr_session::{Event, Session, SessionConfig, WsChannel};
use vr_transport::MessageChannel;

#[derive(Parser)]
#[command(
    name = "velarouter",
    version,
    about = "Velarouter — 局域网优先的远程桌面",
    long_about = "Velarouter 是一个以局域网直连为第一优先的远程桌面框架。\n\
                  signal 只负责设备发现与会话撮合，画面与输入始终端到端加密。"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// 日志级别：error, warn, info, debug, trace
    #[arg(long, global = true, default_value = "info", env = "VELAROUTER_LOG")]
    log: String,
}

#[derive(Subcommand)]
enum Command {
    /// 启动信令服务器（设备注册、配对、会话撮合、中继）
    Signal(SignalArgs),
    /// 启动被控端（采集屏幕、注入输入、局域网监听、可选中继）
    Agent(AgentArgs),
    /// 无头客户端：连接被控端，验证握手并抓取一帧
    Probe(ProbeArgs),
    /// 互联网直连（P2P，无中继）：联机编码 + 密码
    Connect(ConnectArgs),
    /// 列出被控端共享目录
    Ls(LsArgs),
    /// 从被控端下载文件
    Get(GetArgs),
    /// 上传文件到被控端
    Put(PutArgs),
    /// 发送 Wake-on-LAN 魔术包
    Wol(WolArgs),
    /// 在局域网内广播发现被控端
    Discover(DiscoverArgs),
    /// 环境自检：屏幕录制、输入注入、端口、网络
    Doctor,
}

#[derive(Parser)]
struct SignalArgs {
    #[arg(long, default_value = "0.0.0.0:34790")]
    bind: SocketAddr,
    /// 客户端访问该服务使用的公网地址，例如 https://signal.example.com
    #[arg(long)]
    public_url: Option<String>,
    /// mini-STUN 反射服务 UDP 端口，0 表示关闭
    #[arg(long, default_value_t = vr_proto::DEFAULT_REFLEX_PORT)]
    reflex_port: u16,
    /// P2P 会合服务 UDP 端口，0 表示关闭
    #[arg(long, default_value_t = vr_proto::DEFAULT_RENDEZVOUS_PORT)]
    rendezvous_port: u16,
    /// 保护设备列表的管理令牌
    #[arg(long, env = "VELAROUTER_ADMIN_TOKEN")]
    admin_token: Option<String>,
}

#[derive(Parser)]
struct AgentArgs {
    /// 固定配对码 / 联机密码；不传则每次启动随机生成 6 位
    #[arg(long)]
    code: Option<String>,
    /// 局域网监听端口
    #[arg(long, default_value_t = vr_proto::DEFAULT_AGENT_PORT)]
    port: u16,
    /// 监听地址，默认所有网卡
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// 信令服务器地址；不传则纯局域网模式
    #[arg(long, env = "VELAROUTER_SIGNAL")]
    signal: Option<String>,
    /// 设备显示名称
    #[arg(long)]
    name: Option<String>,
    /// 默认画质 20..95
    #[arg(long, default_value_t = 70)]
    quality: u8,
    /// 默认帧率
    #[arg(long, default_value_t = 30)]
    fps: u8,
    #[arg(long, default_value_t = 1920)]
    max_width: u16,
    #[arg(long, default_value_t = 1080)]
    max_height: u16,
    /// 显示器 id（见 doctor 输出）
    #[arg(long, default_value_t = 0)]
    display: u32,
    /// 关闭局域网广播发现
    #[arg(long)]
    no_discovery: bool,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// 允许主控端浏览/传输的目录（默认 ~/Velarouter）
    #[arg(long)]
    files: Option<PathBuf>,
    /// 信令服务器 UDP 会合端口（默认 34792）
    #[arg(long, default_value_t = vr_proto::DEFAULT_RENDEZVOUS_PORT)]
    rendezvous_port: u16,
    /// 联机码：不传则每次启动随机生成 12 位，打印后即可分享
    #[arg(long)]
    internet_code: Option<String>,
    /// 自建 MQTT 会合服务；留空使用内置公共服务。传 `off` 关闭互联网联机
    #[arg(long)]
    broker: Vec<String>,
}

#[derive(Parser)]
struct ProbeArgs {
    /// 被控端 WebSocket 地址，例如 ws://192.168.1.10:34789/vr
    #[arg(long)]
    url: String,
    /// 配对码
    #[arg(long)]
    code: String,
    /// 抓取多少帧后退出
    #[arg(long, default_value_t = 3)]
    frames: usize,
    /// 超时秒数
    #[arg(long, default_value_t = 15)]
    timeout: u64,
    /// 把最后一帧写成文件
    #[arg(long)]
    save: Option<PathBuf>,
    /// 请求的画质
    #[arg(long, default_value_t = 70)]
    quality: u8,
}

#[derive(Parser)]
struct ConnectOpts {
    #[arg(long)]
    url: String,
    #[arg(long)]
    code: String,
    /// 覆盖默认的共享目录（被控端 --files 决定根目录）
    #[arg(long, default_value = "")]
    path: String,
}

#[derive(Parser)]
struct LsArgs {
    #[command(flatten)]
    connect: ConnectOpts,
}

#[derive(Parser)]
struct GetArgs {
    #[command(flatten)]
    connect: ConnectOpts,
    /// 本地保存路径（默认取远端文件名）
    #[arg(long, short)]
    out: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Parser)]
struct PutArgs {
    /// 本地文件
    file: PathBuf,
    #[command(flatten)]
    connect: ConnectOpts,
    /// 远端文件名（默认与本地相同）
    #[arg(long)]
    remote: Option<String>,
    #[arg(long)]
    force: bool,
}

#[derive(Parser)]
struct WolArgs {
    /// 目标网卡 MAC，支持 AA:BB:CC:DD:EE:FF、AA-BB-… 或 AABBCCDDEEFF
    mac: String,
    /// 广播地址，默认 255.255.255.255
    #[arg(long, default_value = "255.255.255.255")]
    broadcast: String,
    /// 端口，默认 9（同时发 7）
    #[arg(long, default_value_t = 9)]
    port: u16,
}

#[derive(Parser)]
struct ConnectArgs {
    /// 自建信令服务器地址；留空则使用内置会合（无需部署）
    #[arg(long, default_value = "")]
    signal: String,
    /// 被控端显示的 12 位联机码（局域网码以 VR- 开头）
    #[arg(long)]
    code: String,
    /// 自建 MQTT 会合服务；留空使用内置公共服务
    #[arg(long)]
    broker: Vec<String>,
    /// 被控端显示的联机密码
    #[arg(long)]
    password: String,
    #[arg(long, default_value_t = 3)]
    frames: usize,
    #[arg(long)]
    save: Option<PathBuf>,
    #[arg(long, default_value_t = 25)]
    timeout: u64,
}

#[derive(Parser)]
struct DiscoverArgs {
    #[arg(long, default_value_t = 3)]
    seconds: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log)),
        )
        .with_target(false)
        .init();

    match cli.command {
        Command::Signal(args) => run_signal(args).await,
        Command::Agent(args) => run_agent(args).await,
        Command::Probe(args) => run_probe(args).await,
        Command::Connect(args) => run_connect(args).await,
        Command::Ls(args) => run_ls(args).await,
        Command::Get(args) => run_get(args).await,
        Command::Put(args) => run_put(args).await,
        Command::Wol(args) => run_wol(args).await,
        Command::Discover(args) => run_discover(args).await,
        Command::Doctor => run_doctor(),
    }
}

async fn run_signal(args: SignalArgs) -> Result<()> {
    let public_url = args
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.bind));
    let cfg = vr_signal::SignalConfig {
        bind: args.bind,
        public_url: public_url.clone(),
        reflex_port: args.reflex_port,
        rendezvous_port: args.rendezvous_port,
        admin_token: args.admin_token,
        ..Default::default()
    };
    println!("Velarouter signal server");
    println!("  bind      : {}", args.bind);
    println!("  public    : {public_url}");
    println!("  health    : {public_url}/v1/health");
    vr_signal::serve(cfg).await
}

async fn run_agent(args: AgentArgs) -> Result<()> {
    let bind: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .context("invalid host/port")?;
    let code = args
        .internet_code
        .clone()
        .unwrap_or_else(vr_agent::online::fresh_code);
    let meet = if args.broker.len() == 1 && args.broker[0] == "off" {
        None
    } else {
        Some((code.clone(), args.broker.clone()))
    };
    let config = vr_agent::AgentConfig {
        name: args.name.unwrap_or_else(vr_agent::default_device_name),
        bind,
        signal_url: args.signal,
        online: None,
        fixed_code: args.code.clone(),
        trusted_viewers: None,
        share_system_audio: false,
        quality: args.quality,
        fps: args.fps,
        max_width: args.max_width,
        max_height: args.max_height,
        display: args.display,
        state_dir: args.state_dir,
        discoverable: !args.no_discovery,
        files_root: args.files.unwrap_or_else(vr_agent::default_files_root),
        rendezvous_port: args.rendezvous_port,
        meet,
        shutdown: None,
    };
    println!();
    println!("  互联网联机码 / internet code : {code}");
    println!(
        "  联机密码     / password      : {}",
        args.code.as_deref().unwrap_or("(随机生成，见上方)")
    );
    vr_agent::run(config).await
}

async fn run_probe(args: ProbeArgs) -> Result<()> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let cfg = SessionConfig {
        session_id: session_id.clone(),
        secret: args.code.clone(),
        app: "velarouter-probe/0.1".into(),
        device: None,
        capabilities: Capabilities {
            codecs: vec![Codec::Jpeg],
            max_width: 2560,
            max_height: 1600,
            max_fps: 60,
            input: true,
            clipboard: true,
            multi_display: true,
            audio: false,
        },
    };
    let channel = WsChannel::connect(&args.url)
        .await
        .with_context(|| format!("cannot connect to {}", args.url))?;
    println!("connected to {}", args.url);
    let session = Session::connect(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("handshake failed")?;
    let peer = session.peer().clone();
    println!(
        "handshake ok: peer={:?} app={} device={:?}",
        peer.role,
        peer.app,
        peer.device.as_ref().map(|d| d.name.clone())
    );
    let (mut sender, mut receiver, _peer) = session.into_parts();
    sender
        .send_control(Control::VideoConfig(VideoConfig {
            codec: Codec::Jpeg,
            quality: args.quality,
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            display: 0,
        }))
        .await?;

    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let mut got = 0usize;
    let mut last: Option<Vec<u8>> = None;
    let mut last_dims = (0u16, 0u16);
    let mut total_bytes = 0usize;
    let started = Instant::now();

    while got < args.frames {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout: received {got}/{} frames", args.frames);
        }
        match tokio::time::timeout(remaining, receiver.recv()).await {
            Ok(Ok(Event::Media { header, payload })) => {
                got += 1;
                total_bytes += payload.len();
                last_dims = (header.width, header.height);
                if got == 1 {
                    println!(
                        "first frame after {:?}: {}x{} {} bytes",
                        started.elapsed(),
                        header.width,
                        header.height,
                        payload.len()
                    );
                }
                last = Some(payload);
            }
            Ok(Ok(Event::Control(Control::DisplayList { displays }))) => {
                println!("displays: {displays:?}");
            }
            Ok(Ok(Event::Control(Control::Warning { code, message }))) => {
                warn!(%code, %message, "agent warning");
            }
            Ok(Ok(Event::Control(Control::Ping { ts }))) => {
                sender.send_control(Control::Pong { ts }).await.ok();
            }
            Ok(Ok(Event::Control(Control::Stats(s)))) => {
                println!(
                    "  stats: {:.1} fps, {} kbps, rtt {} ms, capture {:.1} ms, encode {:.1} ms",
                    s.fps, s.kbps, s.rtt_ms, s.capture_ms, s.encode_ms
                );
            }
            Ok(Ok(Event::Control(_))) => {}
            Ok(Ok(Event::Pong { .. } | Event::File { .. } | Event::Audio(_))) => {}
            Ok(Ok(Event::Closed(reason))) => {
                anyhow::bail!("session closed: {reason:?}");
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout while waiting for frames"),
        }
    }

    let elapsed = started.elapsed();
    let fps = got as f64 / elapsed.as_secs_f64();
    println!(
        "received {got} frames, {}x{}, {} KiB total, {:.1} fps",
        last_dims.0,
        last_dims.1,
        total_bytes / 1024,
        fps
    );
    if let (Some(path), Some(jpeg)) = (args.save, last) {
        std::fs::write(&path, &jpeg)?;
        println!("saved {}", path.display());
    }
    sender
        .send_control(Control::Bye {
            reason: "probe done".into(),
        })
        .await
        .ok();
    Ok(())
}

async fn run_discover(args: DiscoverArgs) -> Result<()> {
    let discovery = vr_net::Discovery::new().await?;
    let found = discovery
        .scan(
            vr_proto::DEFAULT_AGENT_PORT,
            Duration::from_secs(args.seconds),
        )
        .await?;
    if found.is_empty() {
        println!("未发现设备（广播可能被网络策略阻止，可改用 --url 直连）");
    }
    for d in found {
        println!("{}  {}  {}  {}", d.name, d.device_id, d.platform, d.version);
    }
    Ok(())
}

fn run_doctor() -> Result<()> {
    println!("Velarouter doctor\n");
    println!("platform      : {}", vr_agent::platform_name());
    println!("version       : {}", vr_agent::AGENT_VERSION);
    println!("protocol      : v{}", vr_proto::PROTOCOL_VERSION);
    println!("state dir     : {}", vr_agent::state_dir().display());

    println!("\n[1] displays");
    match xcap_monitors() {
        Ok(list) if !list.is_empty() => {
            for d in list {
                println!(
                    "  id={} {}x{} scale={} primary={} {}",
                    d.id, d.width, d.height, d.scale, d.primary, d.name
                );
            }
        }
        _ => println!("  ✗ 未检测到显示器，或被系统权限阻止（macOS：屏幕录制权限）"),
    }

    println!("\n[2] capture");
    match probe_capture() {
        Ok((w, h, bytes)) => println!("  ✓ 采集成功 {w}x{h}，JPEG {} KiB", bytes / 1024),
        Err(e) => println!("  ✗ 采集失败：{e}"),
    }

    println!("\n[3] input injection");
    match probe_input() {
        Ok(()) => println!("  ✓ 输入注入可用"),
        Err(e) => println!("  ✗ 输入注入不可用：{e}"),
    }

    println!("\n[4] listening ports");
    for port in [vr_proto::DEFAULT_AGENT_PORT, vr_proto::DEFAULT_SIGNAL_PORT] {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        match std::net::TcpListener::bind(addr) {
            Ok(_) => println!("  ✓ TCP {port} 空闲"),
            Err(e) => println!("  ✗ TCP {port} 被占用：{e}"),
        }
    }
    match std::net::UdpSocket::bind(("0.0.0.0", vr_proto::DEFAULT_REFLEX_PORT)) {
        Ok(_) => println!("  ✓ UDP {} 空闲", vr_proto::DEFAULT_REFLEX_PORT),
        Err(e) => println!("  ✗ UDP {} 被占用：{e}", vr_proto::DEFAULT_REFLEX_PORT),
    }

    println!("\n[5] LAN addresses");
    for ip in vr_net::ranked_addresses() {
        println!("  {ip}");
    }

    println!("\n平台提示");
    println!("  macOS  : 系统设置 › 隐私与安全性 › 屏幕录制 / 辅助功能 中允许本程序");
    println!("  Windows: 首次采集若黑屏，检查“硬件加速 GPU 计划”与显卡驱动");
    println!("  Linux  : Wayland 需 xdg-desktop-portal + PipeWire；X11 直接可用");
    Ok(())
}

fn xcap_monitors() -> Result<Vec<vr_proto::DisplayInfo>> {
    let monitors = xcap::Monitor::all()?;
    Ok(monitors
        .iter()
        .filter_map(|m| {
            Some(vr_proto::DisplayInfo {
                id: m.id().ok()?,
                name: m.name().ok().unwrap_or_else(|| "Display".into()),
                width: m.width().ok()?,
                height: m.height().ok()?,
                scale: m.scale_factor().ok().unwrap_or(1.0),
                primary: m.is_primary().ok().unwrap_or(false),
            })
        })
        .collect())
}

fn probe_capture() -> Result<(u32, u32, usize)> {
    use vr_agent::capture::{FrameSource, ScreenCapturer};
    let mut cap = ScreenCapturer::new(1280, 720, 60, 30, 0)?;
    let frame = cap
        .next_frame()?
        .context("first capture returned no frame")?;
    Ok((frame.width as u32, frame.height as u32, frame.payload.len()))
}

fn probe_input() -> Result<()> {
    use vr_agent::input::{DesktopInjector, Injector};
    use vr_proto::InputEvent;
    let mut injector = DesktopInjector::new(vec![vr_agent::input::DisplayRect {
        id: 0,
        x: 0,
        y: 0,
        width: 100,
        height: 100,
        scale: 1.0,
    }])?;
    // A no-op event: query the cursor position without moving anything visible.
    injector.apply(&InputEvent::MouseMove {
        x: 0,
        y: 0,
        display: 0,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// File transfer commands
// ---------------------------------------------------------------------------

async fn open_viewer(
    url: &str,
    code: &str,
    app: &str,
) -> Result<(
    vr_session::SessionSender,
    vr_session::SessionReceiver,
    vr_session::Peer,
)> {
    let cfg = SessionConfig {
        session_id: uuid::Uuid::new_v4().to_string(),
        secret: code.to_string(),
        app: app.to_string(),
        device: None,
        capabilities: Capabilities {
            codecs: vec![Codec::Jpeg],
            max_width: 1920,
            max_height: 1080,
            max_fps: 30,
            input: true,
            clipboard: true,
            multi_display: true,
            audio: false,
        },
    };
    let channel = WsChannel::connect(url)
        .await
        .with_context(|| format!("cannot connect to {url}"))?;
    let session = Session::connect(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("handshake failed")?;
    Ok(session.into_parts())
}

async fn run_ls(args: LsArgs) -> Result<()> {
    let (mut tx, mut rx, _peer) =
        open_viewer(&args.connect.url, &args.connect.code, "velarouter-ls/0.1").await?;
    tx.send_control(Control::FileListRequest {
        request_id: 1,
        path: args.connect.path.clone(),
    })
    .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout waiting for directory listing");
        }
        match tokio::time::timeout(remaining, rx.recv_auto_pong(&mut tx)).await {
            Ok(Ok(Event::Control(Control::FileList { entries, .. }))) => {
                print_entries(&entries);
                break;
            }
            Ok(Ok(Event::Control(Control::FileListError { message, .. }))) => {
                anyhow::bail!("list failed: {message}");
            }
            Ok(Ok(Event::Control(_))) | Ok(Ok(Event::Pong { .. })) => continue,
            Ok(Ok(Event::File { .. } | Event::Media { .. } | Event::Audio(_))) => continue,
            Ok(Ok(Event::Closed(reason))) => anyhow::bail!("closed: {reason:?}"),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout waiting for directory listing"),
        }
    }
    let _ = tx
        .send_control(Control::Bye {
            reason: "ls done".into(),
        })
        .await;
    Ok(())
}

fn print_entries(entries: &[FileEntry]) {
    println!("{:<6} {:>12}  PATH", "TYPE", "SIZE");
    for e in entries {
        let kind = if e.is_dir { "dir" } else { "file" };
        println!("{:<6} {:>12}  {}", kind, e.size, e.path);
    }
    println!("{} entries", entries.len());
}

async fn run_get(args: GetArgs) -> Result<()> {
    let (mut tx, mut rx, _peer) =
        open_viewer(&args.connect.url, &args.connect.code, "velarouter-get/0.1").await?;
    let remote = args.connect.path.clone();
    if remote.is_empty() {
        anyhow::bail!("remote path is required (--path)");
    }
    let name = remote.rsplit('/').next().unwrap_or("download").to_string();
    let out = args.out.unwrap_or_else(|| PathBuf::from(&name));
    if out.exists() && !args.force {
        anyhow::bail!("{} exists; pass --force", out.display());
    }
    let part = out.with_extension(format!(
        "{}.part",
        out.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));

    let transfer_id = 1u32;
    tx.send_control(Control::FilePull {
        transfer_id,
        path: remote.clone(),
        offset: 0,
    })
    .await?;

    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut last_report = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("transfer timed out after {written} bytes");
        }
        match tokio::time::timeout(remaining, rx.recv_auto_pong(&mut tx)).await {
            Ok(Ok(Event::File { header, payload })) => {
                if header.transfer_id != transfer_id {
                    continue;
                }
                if header.offset != written {
                    anyhow::bail!("chunk offset {} != {}", header.offset, written);
                }
                file.write_all(&payload).await?;
                hasher.update(&payload);
                written += payload.len() as u64;
                if last_report.elapsed() > Duration::from_millis(500) {
                    println!("  {:.1} MiB", written as f64 / 1048576.0);
                    last_report = Instant::now();
                }
            }
            Ok(Ok(Event::Control(Control::FileComplete { sha256, .. }))) => {
                file.flush().await?;
                file.sync_all().await?;
                drop(file);
                tokio::fs::rename(&part, &out).await?;
                let local = hex(&hasher.finalize());
                println!(
                    "downloaded {} ({} bytes)\n  sha256 local  {}\n  sha256 remote {}  {}",
                    out.display(),
                    written,
                    local,
                    sha256,
                    if local == sha256 {
                        "✓"
                    } else {
                        "✗ mismatch"
                    }
                );
                break;
            }
            Ok(Ok(Event::Control(Control::FileError { message, .. }))) => {
                let _ = tokio::fs::remove_file(&part).await;
                anyhow::bail!("transfer failed: {message}");
            }
            Ok(Ok(Event::Control(Control::FileProgress { offset, total, .. }))) => {
                println!("  progress {offset}/{total}");
            }
            Ok(Ok(Event::Control(_)))
            | Ok(Ok(Event::Pong { .. }))
            | Ok(Ok(Event::Media { .. } | Event::Audio(_))) => continue,
            Ok(Ok(Event::Closed(reason))) => anyhow::bail!("closed: {reason:?}"),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout"),
        }
    }
    let _ = tx
        .send_control(Control::Bye {
            reason: "get done".into(),
        })
        .await;
    Ok(())
}

async fn run_put(args: PutArgs) -> Result<()> {
    let (mut tx, mut rx, _peer) =
        open_viewer(&args.connect.url, &args.connect.code, "velarouter-put/0.1").await?;
    let meta = tokio::fs::metadata(&args.file).await?;
    if meta.is_dir() {
        anyhow::bail!("directories are not supported yet; tar it first");
    }
    let name = args
        .remote
        .clone()
        .or_else(|| {
            args.file
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .context("remote name required")?;
    let transfer_id = 2u32;

    tx.send_control(Control::FilePush {
        transfer_id,
        name: name.clone(),
        size: meta.len(),
        offset: 0,
        overwrite: args.force,
    })
    .await?;

    let mut file = tokio::fs::File::open(&args.file).await?;
    let mut buf = vec![0u8; FILE_CHUNK];
    let mut offset = 0u64;
    let mut seq = 0u32;
    let mut hasher = Sha256::new();
    loop {
        let n = file.read(&mut buf).await?;
        let last = n == 0 || offset + n as u64 == meta.len();
        if n > 0 {
            hasher.update(&buf[..n]);
        }
        let header = FileHeader::new(transfer_id, offset, seq, n as u32, last);
        tx.send_file_chunk(header, &buf[..n]).await?;
        offset += n as u64;
        seq = seq.wrapping_add(1);
        if last {
            break;
        }
        if n == 0 {
            break;
        }
    }

    let expected = hex(&hasher.finalize());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout waiting for confirmation");
        }
        match tokio::time::timeout(remaining, rx.recv_auto_pong(&mut tx)).await {
            Ok(Ok(Event::Control(Control::FileComplete { sha256, path, .. }))) => {
                println!(
                    "uploaded {} -> {} ({} bytes)\n  sha256 local  {}\n  sha256 remote {}  {}",
                    args.file.display(),
                    path,
                    offset,
                    expected,
                    sha256,
                    if expected == sha256 {
                        "✓"
                    } else {
                        "✗ mismatch"
                    }
                );
                break;
            }
            Ok(Ok(Event::Control(Control::FileError { message, .. }))) => {
                anyhow::bail!("upload rejected: {message}");
            }
            Ok(Ok(Event::Control(Control::FileProgress {
                offset: o, total, ..
            }))) => {
                if o == total {
                    println!("  received {o}/{total}");
                }
            }
            Ok(Ok(Event::Control(_))) | Ok(Ok(Event::Pong { .. })) | Ok(Ok(Event::File { .. })) => {
                continue
            }
            Ok(Ok(Event::Media { .. } | Event::Audio(_))) => continue,
            Ok(Ok(Event::Closed(reason))) => anyhow::bail!("closed: {reason:?}"),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout"),
        }
    }
    let _ = tx
        .send_control(Control::Bye {
            reason: "put done".into(),
        })
        .await;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Wake-on-LAN
// ---------------------------------------------------------------------------

async fn run_wol(args: WolArgs) -> Result<()> {
    let mac = parse_mac(&args.mac)?;
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    let ip: std::net::IpAddr = args
        .broadcast
        .parse()
        .with_context(|| format!("invalid broadcast address {}", args.broadcast))?;
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;

    // Limited broadcast may be rejected on macOS when no interface owns it, so
    // also try every subnet broadcast address. Port 9 is standard, 7 historical.
    let mut targets = vec![std::net::SocketAddr::new(ip, args.port)];
    for b in vr_net::broadcast_addresses() {
        if !targets
            .iter()
            .any(|t| t.ip() == std::net::IpAddr::V4(*b.ip()))
        {
            targets.push(std::net::SocketAddr::new(
                std::net::IpAddr::V4(*b.ip()),
                args.port,
            ));
        }
    }
    let mut sent = 0usize;
    let mut last_err = None;
    for target in &targets {
        for port in [args.port, 7] {
            match socket
                .send_to(&packet, std::net::SocketAddr::new(target.ip(), port))
                .await
            {
                Ok(_) => sent += 1,
                Err(e) => last_err = Some(e),
            }
        }
    }
    if sent == 0 {
        if let Some(e) = last_err {
            anyhow::bail!("cannot send magic packet: {e}");
        }
        anyhow::bail!("no broadcast target available");
    }
    println!(
        "sent {sent} magic packets for {} ({})",
        format_mac(&mac),
        targets
            .iter()
            .map(|t| t.ip().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

fn parse_mac(input: &str) -> Result<[u8; 6]> {
    let clean: String = input
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if clean.len() != 12 {
        anyhow::bail!("MAC must contain 12 hex digits, got {input}");
    }
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

// ---------------------------------------------------------------------------
// Internet P2P (no relay)
// ---------------------------------------------------------------------------

async fn run_connect(args: ConnectArgs) -> Result<()> {
    if args.signal.trim().is_empty() {
        // No server configured: meet on the broker and dial directly.
        let (sender, receiver, peer) = connect_meet(&args).await?;
        return stream_frames(&args, sender, receiver, peer).await;
    }
    run_connect_signal(args).await
}

async fn run_connect_signal(args: ConnectArgs) -> Result<()> {
    let base = args.signal.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp: serde_json::Value = http
        .post(format!("{base}/v1/rendezvous"))
        .json(&serde_json::json!({ "device_code": args.code }))
        .send()
        .await
        .context("signal server unreachable")?
        .error_for_status()
        .context("device code rejected")?
        .json()
        .await?;
    let token = resp["token"].as_str().context("missing token")?.to_string();
    let udp_port = resp["udp_port"].as_u64().unwrap_or(0) as u16;
    if udp_port == 0 {
        anyhow::bail!("server has no rendezvous UDP port configured");
    }
    let url = url::Url::parse(&base)?;
    let host = url.host_str().context("signal url has no host")?;
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, udp_port))
        .await
        .context("cannot resolve signal host")?
        .collect();
    let signal_udp = addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .context("signal host resolved to no address")?;
    println!("rendezvous token acquired; punching UDP to {signal_udp}");

    let timeout = Duration::from_secs(args.timeout);
    let p2p = vr_p2p::prepare_viewer(signal_udp, &token, timeout)
        .await
        .context("P2P setup failed (no relay fallback by design)")?;
    println!(
        "punched: local port {} → peer {} (no relay)",
        p2p.local_port, p2p.peer
    );
    let channel = tokio::time::timeout(
        Duration::from_secs(10),
        vr_p2p::connect(&p2p.endpoint, p2p.peer),
    )
    .await
    .context("QUIC connect timeout")??;

    let cfg = SessionConfig {
        session_id: token,
        secret: args.password.clone(),
        app: "velarouter-connect/0.1".into(),
        device: None,
        capabilities: Capabilities {
            codecs: vec![Codec::Jpeg],
            max_width: 2560,
            max_height: 1600,
            max_fps: 60,
            input: true,
            clipboard: true,
            multi_display: true,
            audio: false,
        },
    };
    let session = Session::connect(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("P2P handshake failed (wrong password?)")?;
    let peer = session.peer().clone();
    println!(
        "P2P session established: device={:?}",
        peer.device.as_ref().map(|d| d.name.clone())
    );

    let (mut tx, mut rx, _) = session.into_parts();
    tx.send_control(Control::VideoConfig(VideoConfig {
        codec: Codec::Jpeg,
        quality: 60,
        fps: 30,
        max_width: 1600,
        max_height: 900,
        display: 0,
    }))
    .await?;

    let started = Instant::now();
    let mut got = 0usize;
    let mut last: Option<Vec<u8>> = None;
    let mut last_dims = (0u16, 0u16);
    let deadline = tokio::time::Instant::now() + timeout;
    while got < args.frames {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout: {got}/{} frames", args.frames);
        }
        match tokio::time::timeout(remaining, rx.recv_auto_pong(&mut tx)).await {
            Ok(Ok(Event::Media { header, payload })) => {
                got += 1;
                last_dims = (header.width, header.height);
                if got == 1 {
                    println!(
                        "first frame after {:?}: {}x{} {} bytes",
                        started.elapsed(),
                        header.width,
                        header.height,
                        payload.len()
                    );
                }
                last = Some(payload);
            }
            Ok(Ok(Event::Control(Control::DisplayList { displays }))) => {
                println!("displays: {displays:?}");
            }
            Ok(Ok(Event::Control(Control::Pong { .. }) | Event::Pong { .. } | Event::Audio(_))) => {
            }
            Ok(Ok(Event::Control(_))) => {}
            Ok(Ok(Event::File { .. })) => {}
            Ok(Ok(Event::Closed(reason))) => anyhow::bail!("closed: {reason:?}"),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout"),
        }
    }
    println!(
        "received {got} frames, {}x{} in {:?} (P2P/QUIC, no relay)",
        last_dims.0,
        last_dims.1,
        started.elapsed()
    );
    if let (Some(path), Some(jpeg)) = (args.save, last) {
        std::fs::write(&path, &jpeg)?;
        println!("saved {}", path.display());
    }
    let _ = tx
        .send_control(Control::Bye {
            reason: "connect done".into(),
        })
        .await;
    Ok(())
}

/// Internet rendezvous entry point for the CLI: no signal server involved.
async fn connect_meet(
    args: &ConnectArgs,
) -> Result<(
    vr_session::SessionSender,
    vr_session::SessionReceiver,
    Option<vr_proto::DeviceInfo>,
)> {
    let socket = std::sync::Arc::new(tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await?);
    let mut announce = vr_meet::Announce::open(
        socket,
        &args.code,
        vr_meet::Role::Viewer,
        &args.broker,
        None,
    )?;
    println!("会合中：正在通过公共会合通道查找 {} …", args.code);
    let peer = announce
        .wait_peer(Duration::from_secs(args.timeout))
        .await?;
    println!("已找到对方（{} 个候选地址），开始直连", peer.addrs.len());
    let socket = announce.into_socket().await?;
    let endpoint = vr_p2p::client_endpoint(socket.into_std()?)?;
    let (channel, addr) =
        vr_p2p::connect_any(&endpoint, &peer.addrs, Duration::from_secs(args.timeout)).await?;
    println!("UDP 直连成功：{addr}（无中继）");
    let cfg = SessionConfig {
        session_id: peer.session,
        secret: args.password.clone(),
        app: "velarouter-connect/0.2".into(),
        device: None,
        capabilities: Capabilities {
            codecs: vec![Codec::Jpeg],
            max_width: 2560,
            max_height: 1600,
            max_fps: 60,
            input: true,
            clipboard: true,
            multi_display: true,
            audio: false,
        },
    };
    let session = Session::connect(Box::new(channel) as Box<dyn MessageChannel>, cfg)
        .await
        .context("密码校验失败：请核对联机码与密码")?;
    let device = session.peer().device.clone();
    println!("会话已建立：{:?}", device.as_ref().map(|d| d.name.clone()));
    let (sender, receiver, _) = session.into_parts();
    Ok((sender, receiver, device))
}

/// Requests a stream and saves/prints the frames. Shared by both transports.
async fn stream_frames(
    args: &ConnectArgs,
    mut tx: vr_session::SessionSender,
    mut receiver: vr_session::SessionReceiver,
    _peer: Option<vr_proto::DeviceInfo>,
) -> Result<()> {
    let timeout = Duration::from_secs(args.timeout);
    tx.send_control(Control::VideoConfig(VideoConfig {
        codec: Codec::Jpeg,
        quality: 60,
        fps: 30,
        max_width: 1600,
        max_height: 900,
        display: 0,
    }))
    .await?;
    let started = Instant::now();
    let mut got = 0usize;
    let mut last: Option<Vec<u8>> = None;
    let mut last_dims = (0u16, 0u16);
    let deadline = tokio::time::Instant::now() + timeout;
    while got < args.frames {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timeout: {got}/{} frames", args.frames);
        }
        match tokio::time::timeout(remaining, receiver.recv_auto_pong(&mut tx)).await {
            Ok(Ok(Event::Media { header, payload })) => {
                got += 1;
                last_dims = (header.width, header.height);
                if got == 1 {
                    println!(
                        "first frame after {:.1} ms: {}x{} {} bytes",
                        started.elapsed().as_secs_f64() * 1000.0,
                        header.width,
                        header.height,
                        payload.len()
                    );
                }
                last = Some(payload);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("timeout: {got}/{} frames", args.frames),
        }
    }
    println!(
        "received {} frames, {}x{} in {:.1} ms (直接连接)",
        got,
        last_dims.0,
        last_dims.1,
        started.elapsed().as_secs_f64() * 1000.0
    );
    if let (Some(path), Some(bytes)) = (args.save.as_ref(), last) {
        std::fs::write(path, &bytes).context("cannot write frame")?;
        println!("saved {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

#[cfg(test)]
mod wol_tests {
    use super::*;

    #[test]
    fn mac_parsing_accepts_common_formats() {
        assert_eq!(
            parse_mac("AA:BB:CC:DD:EE:FF").unwrap(),
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
        assert_eq!(
            parse_mac("aa-bb-cc-dd-ee-ff").unwrap(),
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
        assert_eq!(
            parse_mac("aabbccddeeff").unwrap(),
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
        assert!(parse_mac("aa:bb:cc").is_err());
        assert!(parse_mac("zz:bb:cc:dd:ee:ff").is_err());
    }

    #[test]
    fn magic_packet_shape() {
        let mac = [1u8, 2, 3, 4, 5, 6];
        let mut packet = vec![0xFFu8; 6];
        for _ in 0..16 {
            packet.extend_from_slice(&mac);
        }
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xFF; 6]);
        assert_eq!(&packet[6..12], &mac);
    }
}
