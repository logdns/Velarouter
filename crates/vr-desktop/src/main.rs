#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Velarouter desktop client.
//!
//! Opening the app hosts this machine immediately (like Sunflower/UU Remote):
//! a local connection code and password are generated and shown, and the agent
//! starts in the background. The same window is also the viewer.

mod browser;
mod display;
mod file_window;
mod files;
mod home;
mod ime;
mod network_status;
mod reachability;
mod startup;
mod theme;
use std::collections::HashSet;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use egui::{
    Align, Color32, ColorImage, Layout, Margin, Pos2, Rect, RichText, Sense, TextureHandle,
    TextureOptions, Vec2,
};
use theme::{accent, muted, OK, WARN};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use vr_agent::{AgentConfig, AgentEvent};
use vr_client::Viewer;
use vr_proto::{
    modifiers as mods, ClipboardMsg, Control, DisplayInfo, InputEvent, MouseButton, Stats,
    TouchPhase, TouchPoint,
};
use vr_session::Event;

const QUALITY_LEVELS: [u8; 5] = [40, 55, 70, 85, 95];
enum Target {
    Direct(SocketAddr),
    Meet {
        code: String,
    },
    /// Code assigned by an operator's signal server for UDP rendezvous.
    Signal {
        base: String,
        code: String,
    },
}
struct ConnectReq {
    target: Target,
    password: String,
    brokers: Vec<String>,
    initial_size: (u16, u16),
}

enum Cmd {
    Input(InputEvent),
    Video {
        quality: u8,
        fps: u8,
        display: u32,
        max_w: u16,
        max_h: u16,
    },
    ListFiles {
        path: String,
    },
    Download {
        path: String,
        save_dir: Option<std::path::PathBuf>,
    },
    Upload {
        local: std::path::PathBuf,
        /// Remote directory the file panel is showing ("" means the root).
        remote_dir: String,
    },
    Disconnect,
}

enum Ev {
    Connected {
        peer: String,
        transport: &'static str,
    },
    Displays(Vec<DisplayInfo>),
    Stats(Stats),
    InputWarning(String),
    ListError(String),
    Files {
        root: Option<String>,
        path: String,
        entries: Vec<vr_proto::FileEntry>,
    },
    Transfer(TransferView),
    Closed(String),
    Error(String),
}

/// Resolution presets. The agent clamps them against its own capabilities.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Deserialize, serde::Serialize)]
enum Resolution {
    #[default]
    Local,
    Adaptive,
    P1080,
    P1440,
    P2160,
}

impl Resolution {
    fn label(self) -> &'static str {
        match self {
            Self::Local => "跟随本机显示器",
            Self::Adaptive => "适应会话窗口",
            Self::P1080 => "1080P · 轻快",
            Self::P1440 => "2K · 清晰",
            Self::P2160 => "4K · 高清",
        }
    }
    fn size(self) -> (u16, u16) {
        match self {
            Self::Adaptive | Self::Local => (0, 0),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::P2160 => (3840, 2160),
        }
    }
}

/// What the file panel is doing right now.
#[derive(Clone)]
struct TransferView {
    label: String,
    done: u64,
    total: u64,
    finished: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Connecting,
    Session,
}

/// Internet reachability of our own connection code.
#[derive(Clone, PartialEq)]
enum Internet {
    Idle,
    Connecting,
    Ready,
    Failed(String),
}

struct Hosting {
    handle: Option<tokio::task::JoinHandle<()>>,
    shutdown: Option<watch::Sender<bool>>,
    events: UnboundedReceiver<AgentEvent>,
    password: String,
    lan: Vec<SocketAddrV4>,
    internet_code: String,
    internet: Internet,
    listener_port: u16,
    next_check: std::time::Instant,
    ready: bool,
    error: String,
    enabled: bool,
}

struct DecodedFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}
type FrameSlot = Arc<Mutex<Option<DecodedFrame>>>;
struct App {
    latest: FrameSlot,
    held_keys: HashSet<String>,
    held_buttons: HashSet<MouseButton>,
    ctx: egui::Context,
    #[cfg(target_os = "linux")]
    linux_theme: watch::Receiver<Option<egui::Theme>>,
    theme_preference: egui::ThemePreference,
    rt: tokio::runtime::Runtime,
    brand_icon: Option<TextureHandle>,
    home_tab: home::HomeTab,

    cmd_tx: UnboundedSender<Cmd>,
    ev_rx: Receiver<Ev>,

    page: Page,
    code_input: String,
    password_input: String,
    /// Extra rendezvous brokers; empty means the built-in public defaults.
    brokers: Vec<String>,
    broker_draft: String,
    internet: network_status::Internet,
    probe: Option<tokio::sync::oneshot::Receiver<bool>>,
    probe_result: Option<(bool, std::time::Instant)>,
    name_draft: String,
    device_name: String,
    viewer_task: Option<tokio::task::JoinHandle<()>>,
    input_warning: String,
    inbound: usize,
    joined_at: Option<std::time::Instant>,
    #[cfg(target_os = "linux")]
    interaction_status: String,
    keyboard_active: bool,
    composition: ime::Composition,
    compact_remote: bool,
    modifiers: u8,
    show_settings: bool,
    settings_tab: usize,
    ipv6_listener: bool,
    show_password: bool,
    autostart: bool,
    start_minimized: bool,
    close_to_background: bool,
    firewall_offered: bool,
    /// Reused instead of a random password when non-empty (unattended access).
    fixed_password: String,
    local_screens: Vec<display::LocalScreen>,
    remote_root: Option<String>,
    shared_dir: std::path::PathBuf,
    list_pending: bool,
    list_error: String,
    jobs: std::collections::VecDeque<Cmd>,
    file_active: bool,
    auto_close_files: bool,
    batch_errors: Vec<String>,
    dropped: Vec<std::path::PathBuf>,
    /// Optional self-hosted signal server; enables 9-digit codes + relay.
    signal_url: Option<String>,
    signal_draft: String,
    /// Set when the user really wants to quit, so the close handler stops
    /// turning the request into "minimise to background".
    quitting: bool,
    started: bool,
    /// Settings write-through: any change is persisted on the next frame.
    dirty: bool,
    startup_changed: bool,
    restart_requested: bool,
    /// Session: max resolution to request from the agent.
    resolution: Resolution,
    /// Negotiated size of the local monitor or canvas in physical pixels.
    adaptive_size: (u16, u16),
    last_size_sent: (u16, u16),
    last_size_at: std::time::Instant,
    /// Session: two-pane transfer window.
    show_files: bool,
    local_dir: std::path::PathBuf,
    local_draft: String,
    remote_draft: String,
    shared_draft: String,
    shared_error: Option<String>,
    local_entries: Vec<browser::LocalEntry>,
    local_error: Option<String>,
    local_selected: Option<std::path::PathBuf>,
    remote_selected: Option<String>,
    remote_path: String,
    files: Vec<vr_proto::FileEntry>,
    transfer: Option<TransferView>,
    error: String,

    hosting: Hosting,

    peer: String,
    transport: String,
    stats: Stats,
    quality: u8,
    fps: u8,
    displays: Vec<DisplayInfo>,
    display: u32,
    texture: Option<TextureHandle>,
    frame_size: (u32, u32),
    fullscreen: bool,
}

impl App {
    fn with_config(ctx: &egui::Context, cfg: &Config, rt: tokio::runtime::Runtime) -> Self {
        let theme_preference = theme::configure(ctx, cfg.theme_preference);
        theme::apply(ctx);
        let brand_icon = eframe::icon_data::from_png_bytes(include_bytes!(
            "../../../assets/brand/png/appicon/velarouter-128.png"
        ))
        .ok()
        .map(|icon| {
            ctx.load_texture(
                "velarouter-brand",
                ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                ),
                TextureOptions::LINEAR,
            )
        });

        let (cmd_tx, _) = unbounded_channel();
        let (_, ev_rx) = std::sync::mpsc::channel();
        let (_tx, events) = unbounded_channel();

        Self {
            ctx: ctx.clone(),
            latest: Default::default(),
            held_keys: Default::default(),
            held_buttons: Default::default(),
            #[cfg(target_os = "linux")]
            linux_theme: watch::channel(None).1,
            theme_preference,
            rt,
            brand_icon,
            home_tab: home::HomeTab::Share,
            cmd_tx,
            ev_rx,
            page: Page::Home,
            code_input: String::new(),
            password_input: String::new(),
            name_draft: String::new(),
            brokers: cfg.brokers.split_whitespace().map(str::to_owned).collect(),
            broker_draft: cfg.brokers.clone(),
            internet: network_status::system_hint(),
            probe: None,
            probe_result: None,
            device_name: cfg.device_name.clone(),
            viewer_task: None,
            input_warning: String::new(),
            inbound: 0,
            joined_at: None,
            #[cfg(target_os = "linux")]
            interaction_status: String::new(),
            keyboard_active: false,
            composition: Default::default(),
            compact_remote: false,
            modifiers: 0,
            show_settings: false,
            settings_tab: 0,
            ipv6_listener: false,
            show_password: false,
            autostart: cfg.autostart.unwrap_or(true),
            start_minimized: cfg.start_minimized,
            close_to_background: cfg.close_to_background,
            firewall_offered: cfg.firewall_offered,
            fixed_password: cfg.fixed_password.clone(),
            local_screens: Vec::new(),
            remote_root: None,
            shared_dir: if cfg.shared_dir.is_empty() {
                vr_agent::default_files_root()
            } else {
                cfg.shared_dir.as_str().into()
            },
            list_pending: false,
            list_error: String::new(),
            jobs: Default::default(),
            file_active: false,
            batch_errors: Vec::new(),
            dropped: Vec::new(),
            auto_close_files: cfg.auto_close_files,
            signal_url: (!cfg.signal_url.trim().is_empty()).then(|| cfg.signal_url.clone()),
            signal_draft: cfg.signal_url.clone(),
            quitting: false,
            started: false,
            dirty: false,
            startup_changed: false,
            restart_requested: false,
            resolution: cfg.resolution,
            adaptive_size: (1600, 900),
            last_size_sent: (0, 0),
            last_size_at: std::time::Instant::now(),
            show_files: false,
            local_dir: if std::path::Path::new(&cfg.save_dir).is_dir() {
                cfg.save_dir.as_str().into()
            } else {
                files::downloads_dir()
            },
            local_draft: String::new(),
            remote_draft: String::new(),
            shared_draft: String::new(),
            shared_error: None,
            local_entries: Vec::new(),
            local_error: None,
            local_selected: None,
            remote_selected: None,
            remote_path: String::new(),
            files: Vec::new(),
            transfer: None,
            error: String::new(),

            hosting: Hosting {
                handle: None,
                shutdown: None,
                events,
                password: String::new(),
                lan: Vec::new(),
                internet_code: cfg.internet_code.clone(),
                internet: Internet::Idle,
                listener_port: 0,
                next_check: std::time::Instant::now(),
                ready: false,
                error: String::new(),
                enabled: false,
            },
            peer: String::new(),
            transport: String::new(),
            stats: Stats::default(),
            quality: 70,
            fps: 30,
            displays: Vec::new(),
            display: 0,
            texture: None,
            frame_size: (0, 0),
            fullscreen: false,
        }
    }

    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, rt: tokio::runtime::Runtime) -> Self {
        let mut app = Self::with_config(&cc.egui_ctx, &cfg, rt);
        #[cfg(target_os = "linux")]
        {
            app.linux_theme = theme::watch_linux(&app.rt, app.ctx.clone());
        }
        if app.device_name.trim().is_empty() {
            app.device_name = vr_agent::default_device_name();
        }
        app.test_outbound();
        app.start_hosting();
        if !cfg.connect.trim().is_empty() {
            app.code_input = cfg.connect.trim().to_owned();
            app.password_input = cfg.password.clone();
            app.connect();
        }
        // First run of a packaged build registers itself for login once; the
        // switch in preferences is authoritative afterwards.
        // Always rewrite while enabled: the app may have been moved (dist →
        // /Applications), and the login item must follow it.
        if app.autostart && startup::is_bundled() {
            if let Err(e) = startup::set(true) {
                tracing::warn!(%e, "cannot register launch at login");
            }
        } else if !app.autostart && startup::is_enabled() {
            let _ = startup::set(false);
        }
        // Without an inbound rule Windows silently drops every punched packet,
        // so the very first run asks once (UAC) instead of letting the user
        // discover a mysterious "对方没有响应".
        if cfg!(windows)
            && app.autostart
            && !app.firewall_offered
            && !startup::firewall_rule_exists()
        {
            app.firewall_offered = true;
            app.dirty = true;
            if let Err(e) = startup::allow_firewall() {
                tracing::warn!(%e, "cannot open the firewall prompt");
            }
        }
        // `VELAROUTER_PAGE=settings` opens preferences directly (kiosk setups and
        // screenshot checks), `=session` is ignored until a session exists.
        match std::env::var("VELAROUTER_PAGE").as_deref() {
            Ok("settings") => app.open_settings(),
            Ok("permissions") => {
                app.open_settings();
                app.settings_tab = 1;
            }
            Ok("network") => {
                app.open_settings();
                app.settings_tab = 2;
            }
            Ok("about") => {
                app.open_settings();
                app.settings_tab = 3;
            }
            _ => {}
        }
        app
    }

    // ---- hosting -----------------------------------------------------------

    fn start_hosting(&mut self) {
        let previous = self.hosting.handle.take();
        self.stop_hosting();
        let password = if self.fixed_password.trim().len() >= 6 {
            self.fixed_password.trim().to_owned()
        } else {
            generate_password()
        };
        self.hosting.password = password.clone();
        if !vr_proto::is_internet_code(&self.hosting.internet_code) {
            self.hosting.internet_code = vr_agent::online::fresh_code();
        }
        self.hosting.internet = Internet::Connecting;
        self.hosting.ready = false;
        self.hosting.error.clear();
        self.hosting.lan.clear();
        self.hosting.enabled = true;

        let (tx, mut incoming) = unbounded_channel();
        let (ui_tx, rx) = unbounded_channel();
        let wake = self.ctx.clone();
        self.rt.spawn(async move {
            while let Some(event) = incoming.recv().await {
                if ui_tx.send(event).is_err() {
                    break;
                }
                wake.request_repaint();
            }
        });
        self.hosting.events = rx;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.hosting.shutdown = Some(shutdown_tx);

        let config = AgentConfig {
            name: self.device_name.clone(),
            bind: ([0, 0, 0, 0], vr_proto::DEFAULT_AGENT_PORT).into(),
            signal_url: self.signal_url.clone(),
            online: None,
            fixed_code: Some(password),
            quality: 70,
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            display: 0,
            state_dir: None,
            discoverable: false,
            files_root: self.shared_dir.clone(),
            rendezvous_port: vr_proto::DEFAULT_RENDEZVOUS_PORT,
            meet: Some((self.hosting.internet_code.clone(), self.brokers.clone())),
            shutdown: Some(shutdown_rx),
        };
        let ctx = self.ctx.clone();
        self.hosting.handle = Some(self.rt.spawn(async move {
            if let Some(task) = previous {
                let _ = task.await;
            }
            if let Err(e) = vr_agent::run_with_events(config, Some(tx.clone())).await {
                let _ = tx.send(AgentEvent::HostingFailed(format!(
                    "无法启动共享：{e}（请检查端口占用）"
                )));
            }
            ctx.request_repaint();
        }));
    }

    fn stop_hosting(&mut self) {
        if let Some(tx) = self.hosting.shutdown.take() {
            let _ = tx.send(true);
        }
        self.inbound = 0;
        self.ipv6_listener = false;
        self.hosting.enabled = false;
        self.hosting.ready = false;
        self.hosting.lan.clear();
        self.hosting.internet = Internet::Idle;
    }
    fn drain_agent(&mut self) {
        while let Ok(ev) = self.hosting.events.try_recv() {
            match ev {
                AgentEvent::SessionJoined => {
                    if self.hosting.enabled {
                        self.inbound += 1;
                        self.ctx
                            .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        self.ctx
                            .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                                egui::UserAttentionType::Informational,
                            ));
                    }
                }
                AgentEvent::SessionLeft => self.inbound = self.inbound.saturating_sub(1),
                #[cfg(target_os = "linux")]
                AgentEvent::InteractionStatus(message) => self.interaction_status = message,
                AgentEvent::LanListening { port, ipv6 } => {
                    if !self.hosting.enabled {
                        continue;
                    }
                    self.ipv6_listener = ipv6;
                    self.hosting.ready = self.hosting.enabled;
                    self.hosting.listener_port = port;
                    self.refresh_network();
                }
                AgentEvent::HostingFailed(e) => {
                    self.hosting.error = e;
                    self.hosting.enabled = false;
                    self.hosting.ready = false;
                }
                AgentEvent::Registered { device_code, .. } => {
                    self.hosting.internet_code = device_code;
                    self.hosting.internet = Internet::Ready;
                }
                AgentEvent::InternetReady { code } => {
                    // A self-hosted signal server assigns a 9-digit device code
                    // and can relay; keep that one instead of overwriting it
                    // with the broker rendezvous code.
                    if self.signal_url.is_none() && vr_proto::is_internet_code(&code) {
                        self.hosting.internet_code = code;
                    }
                    if self.signal_url.is_none() {
                        self.hosting.internet = Internet::Ready;
                    }
                }
                AgentEvent::InternetFailed(reason) if self.signal_url.is_none() => {
                    self.hosting.internet = Internet::Failed(reason);
                }
                AgentEvent::SignalUnavailable(reason) if self.signal_url.is_some() => {
                    self.hosting.internet = Internet::Failed(format!("自建信令不可用：{reason}"));
                }
                AgentEvent::SignalUnavailable(_) | AgentEvent::InternetFailed(_) => {}
            }
        }
    }

    fn refresh_network(&mut self) {
        let hint = network_status::system_hint();
        let addrs: Vec<_> = vr_net::ranked_addresses()
            .into_iter()
            .filter_map(|ip| match ip {
                std::net::IpAddr::V4(v) if !v.is_loopback() => {
                    Some(SocketAddrV4::new(v, self.hosting.listener_port))
                }
                _ => None,
            })
            .collect();
        let changed = self.hosting.lan != addrs;
        if changed || self.internet != hint || self.probe_result.is_none() {
            self.test_outbound();
        }
        self.internet = hint;
        self.hosting.lan = addrs;
        self.hosting.next_check = std::time::Instant::now() + Duration::from_secs(10);
    }
    fn test_outbound(&mut self) {
        if self.probe.is_some() {
            return;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.probe = Some(rx);
        self.probe_result = None;
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let ok = network_status::check_https().await;
            let _ = tx.send(ok);
            ctx.request_repaint();
        });
    }
    fn poll_probe(&mut self) {
        if let Some(rx) = self.probe.as_mut() {
            match rx.try_recv() {
                Ok(ok) => {
                    self.probe_result = Some((ok, std::time::Instant::now()));
                    self.probe = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => self.probe = None,
                _ => {}
            }
        }
        if self
            .probe_result
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(60))
        {
            self.probe_result = None;
        }
    }
    fn open_settings(&mut self) {
        if self.page == Page::Session {
            let _ = self.cmd_tx.send(Cmd::Input(InputEvent::ReleaseAll));
            self.held_keys.clear();
            self.keyboard_active = false;
            self.composition = Default::default();
            self.modifiers = 0;
        }
        self.shared_draft = self.shared_dir.display().to_string();
        self.shared_error = None;
        self.name_draft = self.device_name.clone();
        self.broker_draft = self.brokers.join("\n");
        self.show_settings = true;
    }
    // ---- viewer ------------------------------------------------------------

    fn connect(&mut self) {
        self.home_tab = home::HomeTab::Connect;
        self.error.clear();
        let code: String = self.code_input.split_whitespace().collect();
        if self.password_input.is_empty() {
            self.error = "请输入连接密码。".into();
            return;
        }
        let target = match reachability::parse(&code) {
            Ok(reachability::Destination::Meet(code)) => {
                if self.hosting.enabled && code == self.hosting.internet_code {
                    self.error = "这是本机的联机码，请输入另一台设备的码。".into();
                    return;
                }
                Target::Meet { code }
            }
            Ok(reachability::Destination::Signal(code)) => {
                let Some(base) = self.signal_url.clone() else {
                    self.error = "这是自建信令服务器的 9 位编码：请先在「设置 → 网络」里填写信令服务器地址。".into();
                    return;
                };
                Target::Signal { base, code }
            }
            Ok(reachability::Destination::Direct(a))
                if self.hosting.lan.iter().any(|v| SocketAddr::V4(*v) == a) =>
            {
                self.error = "这是本机的联机码，请输入另一台设备的码。".into();
                return;
            }
            Ok(reachability::Destination::Direct(a)) => Target::Direct(a),
            Err(e) => {
                self.error = e.into();
                return;
            }
        };
        // A previous worker may still be dialling; two live workers would fight
        // over the same event channel and the UI would look "stuck".
        if let Some(task) = self.viewer_task.take() {
            task.abort();
        }
        self.page = Page::Connecting;

        self.input_warning.clear();
        self.held_keys.clear();
        self.latest = Default::default();
        self.keyboard_active = false;
        self.composition = Default::default();
        self.compact_remote = false;
        self.modifiers = 0;
        self.display = 0;
        self.displays.clear();
        self.files.clear();
        self.remote_path.clear();
        self.remote_draft.clear();
        self.remote_root = None;
        self.dropped.clear();
        self.jobs.clear();
        self.batch_errors.clear();
        self.file_active = false;
        self.list_pending = false;
        self.list_error.clear();
        self.remote_selected = None;
        self.local_screens = display::LocalScreen::all();
        self.adaptive_size =
            Self::adaptive_target(&self.ctx, self.resolution, self.ctx.content_rect().size());
        self.last_size_sent = self.target_size();
        self.transfer = None;
        self.show_files = false;
        self.stats = Stats::default();
        let req = ConnectReq {
            target,
            password: self.password_input.clone(),
            brokers: self.brokers.clone(),
            initial_size: self.target_size(),
        };
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        self.cmd_tx = cmd_tx;
        self.ev_rx = ev_rx;
        let ctx = self.ctx.clone();
        self.viewer_task =
            Some(
                self.rt
                    .spawn(worker(req, cmd_rx, ev_tx, ctx, self.latest.clone())),
            );
    }

    fn disconnect(&mut self) {
        if self.page == Page::Connecting {
            if let Some(t) = self.viewer_task.take() {
                t.abort();
            }
        }
        if !self.held_buttons.is_empty() || !self.held_keys.is_empty() {
            let _ = self.cmd_tx.send(Cmd::Input(InputEvent::ReleaseAll));
            self.held_buttons.clear();
            self.held_keys.clear();
        }
        let _ = self.cmd_tx.send(Cmd::Disconnect);
        self.ev_rx = std::sync::mpsc::channel().1;
        self.page = Page::Home;
        self.texture = None;
        self.peer.clear();
        self.transport.clear();
    }

    fn drain_viewer(&mut self) {
        while let Ok(ev) = self.ev_rx.try_recv() {
            match ev {
                Ev::Connected { peer, transport } => {
                    self.joined_at = Some(std::time::Instant::now());
                    self.page = Page::Session;
                    if let Ok(path) = std::env::var("VELAROUTER_AUTO_DOWNLOAD") {
                        self.enqueue_file(Cmd::Download {
                            path,
                            save_dir: Some(self.local_dir.clone()),
                        });
                    }
                    if let Ok(local) = std::env::var("VELAROUTER_AUTO_UPLOAD") {
                        self.enqueue_file(Cmd::Upload {
                            local: local.into(),
                            remote_dir: String::new(),
                        });
                    }
                    if std::env::var("VELAROUTER_FILES").is_ok() {
                        self.open_files();
                    }
                    self.peer = peer;
                    self.transport = transport.into();
                }
                Ev::Displays(list) => {
                    if let Some(primary) = list.iter().find(|d| d.primary) {
                        self.display = primary.id;
                    } else if let Some(first) = list.first() {
                        self.display = first.id;
                    }
                    self.displays = list;
                    self.apply_video();
                }
                Ev::InputWarning(w) => self.input_warning = w,
                Ev::ListError(message) => {
                    self.list_pending = false;
                    self.list_error = message;
                }
                Ev::Files {
                    path,
                    root,
                    entries,
                } => {
                    self.remote_selected = None;
                    self.list_pending = false;
                    self.list_error.clear();
                    self.remote_path = path;
                    self.remote_root = root;
                    self.files = entries;
                }
                Ev::Transfer(view) => {
                    let finished = view.finished;
                    if let Some(reason) = &view.error {
                        self.batch_errors.push(format!("{}：{reason}", view.label));
                    }
                    self.transfer = Some(view);
                    if finished {
                        self.next_file();
                        if !self.file_active && self.show_files {
                            if self.auto_close_files
                                && self.batch_errors.is_empty()
                                && self.dropped.is_empty()
                            {
                                self.show_files = false;
                            } else {
                                self.refresh_local();
                                if !self.list_pending && self.remote_draft == self.remote_path {
                                    self.list_remote(self.remote_path.clone());
                                }
                            }
                        }
                    }
                }
                Ev::Stats(s) => self.stats = s,
                Ev::Closed(reason) => {
                    self.page = Page::Home;
                    self.texture = None;
                    self.error = if reason.is_empty() {
                        "会话已断开".into()
                    } else {
                        format!("会话已断开：{reason}")
                    };
                }
                Ev::Error(e) => {
                    self.page = Page::Home;
                    self.texture = None;
                    self.error = e;
                }
            }
        }
        if self.page == Page::Session {
            if let Some(DecodedFrame {
                width,
                height,
                rgba,
            }) = self.latest.lock().unwrap().take()
            {
                let image =
                    ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                self.frame_size = (width, height);
                match &mut self.texture {
                    Some(tex) => tex.set(image, TextureOptions::LINEAR),
                    None => {
                        self.texture = Some(self.ctx.load_texture(
                            "remote",
                            image,
                            TextureOptions::LINEAR,
                        ))
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    #[cfg(target_os = "linux")]
    fn raw_input_hook(&mut self, _: &egui::Context, input: &mut egui::RawInput) {
        if let Some(theme) = *self.linux_theme.borrow() {
            input.system_theme = Some(theme);
        }
    }
    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        if self.page == Page::Session && !self.show_settings {
            self.collect_drops(ctx);
        }
        if self.dirty {
            self.dirty = false;
            if let Some(storage) = frame.storage_mut() {
                self.save(storage);
                // eframe only flushes every 30 s or on exit; a settings change
                // must be on disk before the user can lose it in a crash.
                storage.flush();
            }
        }
        if self.startup_changed {
            self.startup_changed = false;
            // `reg add` on Windows takes tens of milliseconds: never on the UI thread.
            let enabled = self.autostart;
            std::thread::spawn(move || {
                if let Err(e) = startup::set(enabled) {
                    tracing::warn!(%e, "cannot update launch at login");
                }
            });
        }
        if self.restart_requested {
            self.restart_requested = false;
            if self.hosting.enabled {
                self.start_hosting();
            }
        }
        if !self.started {
            self.started = true;
            if self.start_minimized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            if std::env::var("VELAROUTER_FULLSCREEN").is_ok() {
                self.fullscreen = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            } else if std::env::var("VELAROUTER_IMMERSIVE").is_ok() {
                // Layout-only check: immersive rendering inside a normal window.
                self.fullscreen = true;
            }
        }
        if !self.quitting
            && self.close_to_background
            && ctx.input(|i| i.viewport().close_requested())
        {
            // Keep serving in the background instead of dropping the session.
            // Guarded by `minimized` so a repeated request cannot flash the
            // window by minimising it over and over.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            self.dirty = true;
        }
        self.drain_agent();
        self.drain_viewer();
        self.poll_probe();
        if self.hosting.enabled && !self.hosting.ready {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.hosting.ready && std::time::Instant::now() >= self.hosting.next_check {
            self.refresh_network();
        }
        if self.hosting.enabled {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        let immersive = self.page == Page::Session && self.fullscreen;
        if self.inbound > 0
            || (self.page == Page::Session
                && self
                    .joined_at
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(5)))
        {
            egui::Panel::top("authenticated-session-notice").show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if self.inbound > 0 {
                        ui.strong(format!(
                            "远程操作已接入 · {} 个会话正在访问本机",
                            self.inbound
                        ));
                        if ui.button("停止共享").clicked() {
                            self.stop_hosting();
                        }
                    }
                    if self.page == Page::Session
                        && self
                            .joined_at
                            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
                    {
                        ui.label(format!("已接入远程会话：{}", self.peer));
                        ctx.request_repaint_after(Duration::from_secs(1));
                    }
                });
            });
        }
        if !immersive {
            egui::Panel::top("brand")
                .frame(
                    egui::Frame::new()
                        .fill(theme::page(ctx))
                        .inner_margin(Margin::symmetric(18, 8)),
                )
                .show(root, |ui| {
                    theme::centered_width(ui, 1020.0, |ui| {
                        ui.horizontal(|ui| {
                            self.brand(ui);
                            ui.add_space(6.0);
                            let (label, color) = if self.page == Page::Session {
                                ("远程会话", muted(ui))
                            } else if !self.hosting.enabled {
                                ("已暂停", muted(ui))
                            } else if self.hosting.internet == Internet::Ready {
                                ("本机共享中", OK)
                            } else if self.hosting.ready {
                                ("本机共享中", accent(ui))
                            } else {
                                ("正在启动", WARN)
                            };
                            if ui.max_rect().width() >= 480.0 {
                                theme::pill(ui, label, color);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let text = if self.show_settings {
                                    "← 返回"
                                } else {
                                    "设置"
                                };
                                if theme::secondary(ui, text).clicked() {
                                    if self.show_settings {
                                        self.show_settings = false;
                                    } else {
                                        self.open_settings();
                                    }
                                }
                            });
                        });
                    });
                });
        }
        if self.page == Page::Session && self.show_files {
            self.files_window(ctx);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(if immersive {
                        Color32::BLACK
                    } else {
                        theme::page(ctx)
                    })
                    .inner_margin(if immersive {
                        Margin::ZERO
                    } else {
                        Margin::symmetric(if self.page == Page::Session { 10 } else { 18 }, 4)
                    }),
            )
            .show(root, |ui| {
                if self.show_settings {
                    self.settings_page(ui);
                } else {
                    match self.page {
                        Page::Home => self.home_ui(ui),
                        Page::Connecting => self.connecting_ui(ui),
                        Page::Session => self.session_ui(ui),
                    }
                }
            });
    }

    fn on_exit(&mut self) {
        self.stop_hosting();
        // The tokio runtime shutdown waits for every blocking task (clipboard,
        // file IO). On Windows that turned "quit" into a freeze, so leave the
        // process once the sockets are released. Settings are already on disk.
        std::process::exit(0);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let cfg = StoredConfig {
            brokers: self.brokers.join(" "),
            device_name: self.device_name.clone(),
            internet_code: self.hosting.internet_code.clone(),
            autostart: Some(self.autostart),
            theme_preference: Some(self.theme_preference),
            start_minimized: self.start_minimized,
            close_to_background: self.close_to_background,
            firewall_offered: self.firewall_offered,
            fixed_password: self.fixed_password.clone(),
            save_dir: self.local_dir.to_string_lossy().into_owned(),
            shared_dir: self.shared_dir.to_string_lossy().into_owned(),
            auto_close_files: self.auto_close_files,
            resolution: self.resolution,
            signal_url: self.signal_url.clone().unwrap_or_default(),
        };
        if let Ok(json) = serde_json::to_string(&cfg) {
            storage.set_string("velarouter.config", json);
        }
    }
}

impl App {
    fn connecting_ui(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(140.0);
            ui.spinner();
            ui.add_space(18.0);
            ui.heading("正在连接对方设备…");
            ui.weak("正在通过会合通道寻找对方");
            ui.add_space(6.0);
            ui.label(
                RichText::new("如果对方刚结束上一次会话，通常 5–10 秒内会自动连上。")
                    .size(13.5)
                    .color(muted(ui)),
            );
            if ui.button("取消").clicked() {
                self.disconnect();
            }
        });
    }

    fn session_ui(&mut self, ui: &mut egui::Ui) {
        if self.fullscreen {
            return self.immersive_session(ui);
        }
        let narrow = ui.available_width() < 700.;
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if ui.button("← 返回").clicked() {
                    self.disconnect();
                }
                ui.add_space(2.0);
                ui.label(RichText::new(&self.peer).size(15.0).strong());
                theme::pill(
                    ui,
                    if self.transport == "p2p" {
                        "P2P · QUIC"
                    } else {
                        "局域网 · TCP"
                    },
                    theme::accent(ui),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(if self.fullscreen {
                            "退出全屏"
                        } else {
                            "全屏"
                        })
                        .clicked()
                    {
                        self.fullscreen = !self.fullscreen;
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                    }
                    if ui.button("断开").clicked() {
                        self.disconnect();
                    }
                    ui.label(
                        RichText::new(format!(
                            "{}×{}   {} fps   {} kbps   {} ms",
                            self.frame_size.0,
                            self.frame_size.1,
                            self.stats.fps.round() as u32,
                            self.stats.kbps,
                            self.stats.rtt_ms
                        ))
                        .size(13.5)
                        .color(muted(ui)),
                    );
                });
            });
        });
        if !self.input_warning.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(&self.input_warning).size(14.0).color(WARN));
        }
        ui.add_space(8.0);

        let footer = if narrow { 108.0 } else { 62.0 };
        let avail = ui.available_size();
        let canvas = Vec2::new(avail.x, (avail.y - footer).max(120.0));
        let (rect, response) = ui.allocate_exact_size(canvas, Sense::click_and_drag());
        self.sync_adaptive_size(ui.ctx(), rect.shrink(8.).size());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 16.0, Color32::from_rgb(9, 11, 16));

        if let Some(tex) = &self.texture {
            let (fw, fh) = self.frame_size;
            if fw > 0 && fh > 0 {
                let inner = rect.shrink(8.0);
                let scale = (inner.width() / fw as f32).min(inner.height() / fh as f32);
                let size = Vec2::new(fw as f32 * scale, fh as f32 * scale);
                let image_rect = Rect::from_center_size(inner.center(), size);
                painter.image(
                    tex.id(),
                    image_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                if self.input_warning.is_empty() {
                    self.forward_input(ui, response, image_rect);
                }
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "等待画面…",
                egui::FontId::proportional(15.0),
                Color32::from_rgb(150, 156, 170),
            );
        }

        ui.add_space(8.0);
        egui::Frame::new()
            .fill(theme::card(ui))
            .stroke(egui::Stroke::new(1.0, theme::border(ui)))
            .corner_radius(14)
            .inner_margin(Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("画质").size(13.0).color(muted(ui)));
                    for q in QUALITY_LEVELS {
                        if ui
                            .selectable_label(self.quality == q, format!("{q}"))
                            .clicked()
                        {
                            self.quality = q;
                            self.apply_video();
                        }
                    }
                    self.display_controls(ui, rect.shrink(8.).size());
                    ui.add_space(6.0);
                    if ui
                        .selectable_label(self.show_files, "文件")
                        .on_hover_text("浏览远端文件、上传与下载")
                        .clicked()
                    {
                        self.open_files();
                    }
                });
            });
    }

    /// Fullscreen means the remote desktop only: no title bar, no toolbars, no
    /// letterboxing caused by chrome. Esc or the floating button returns.
    fn immersive_session(&mut self, ui: &mut egui::Ui) {
        let ime_was_active = self.composition.composing();
        let avail = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        self.sync_adaptive_size(ui.ctx(), rect.size());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        if let Some(tex) = &self.texture {
            let (fw, fh) = self.frame_size;
            if fw > 0 && fh > 0 {
                let scale = (rect.width() / fw as f32).min(rect.height() / fh as f32);
                let size = Vec2::new(fw as f32 * scale, fh as f32 * scale);
                let image_rect = Rect::from_center_size(rect.center(), size);
                painter.image(
                    tex.id(),
                    image_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                if self.input_warning.is_empty() {
                    self.forward_input(ui, response, image_rect);
                }
            }
        }
        // Drawn in the same layer as the canvas so nothing can cover it.
        let stats = format!(
            "{}×{}   {} fps   {} ms",
            self.frame_size.0,
            self.frame_size.1,
            self.stats.fps.round() as u32,
            self.stats.rtt_ms
        );
        painter.text(
            Pos2::new(rect.right() - 188.0, rect.top() + 22.0),
            egui::Align2::RIGHT_CENTER,
            stats,
            egui::FontId::proportional(13.0),
            Color32::from_rgb(190, 195, 210),
        );
        let exit_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 176.0, rect.top() + 34.0),
            Vec2::new(162.0, 32.0),
        );
        let exit = ui.put(
            exit_rect,
            egui::Button::new(
                RichText::new("退出全屏 (Esc)")
                    .size(13.5)
                    .color(Color32::WHITE),
            )
            .fill(Color32::from_black_alpha(120))
            .corner_radius(9),
        );
        if exit.clicked() {
            self.fullscreen = false;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && !ime_was_active
            && !self.composition.composing()
        {
            self.fullscreen = false;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn transfer_status(&mut self, ui: &mut egui::Ui) {
        let Some(t) = &self.transfer else {
            return;
        };
        let ok = t.finished && t.error.is_none();
        let fraction = if ok {
            1.0
        } else if t.total > 0 {
            (t.done as f32 / t.total as f32).clamp(0., 1.)
        } else {
            0.
        };
        let summary = format!(
            "{} · {} / {}",
            t.label,
            files::human_size(t.done),
            files::human_size(t.total)
        );
        ui.add(egui::Label::new(&summary).truncate())
            .on_hover_text(summary);
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_height(5.)
                .fill(if ok { theme::OK } else { theme::accent(ui) }),
        );
        if t.finished && ui.small_button("清除记录").clicked() {
            self.transfer = None;
            self.batch_errors.clear();
        }
    }

    fn forward_input(&mut self, ui: &egui::Ui, response: egui::Response, image: Rect) {
        let ctx = ui.ctx();
        let (pointer, mods_state, focused) =
            ctx.input(|i| (i.pointer.clone(), i.modifiers, i.focused));
        let hover = pointer.hover_pos();
        let over = response.contains_pointer() && hover.is_some_and(|p| image.contains(p));
        if focused && !self.show_settings && over && pointer.any_pressed() {
            response.request_focus();
            if pointer.primary_pressed() {
                if let Some(p) = hover {
                    self.composition.place(image, p);
                }
            }
        }
        // Keyboard follows the pointer as well as the click: on a session page
        // there is nothing else to type into, and requiring a precise click
        // first is the most common "keyboard does not work" report.
        let active = focused
            && !self.show_settings
            && !egui::Popup::is_any_open(ctx)
            && (response.has_focus() || over);
        if self.keyboard_active && !active {
            self.composition = Default::default();
            let _ = self.cmd_tx.send(Cmd::Input(InputEvent::ReleaseAll));
            self.modifiers = 0;
            self.held_keys.clear();
            self.held_buttons.clear();
        }
        self.keyboard_active = active;
        if !focused || self.show_settings {
            return;
        }

        let norm = |p: Pos2| -> (u16, u16) {
            let x = ((p.x - image.left()) / image.width()).clamp(0.0, 1.0);
            let y = ((p.y - image.top()) / image.height()).clamp(0.0, 1.0);
            ((x * 65535.0).round() as u16, (y * 65535.0).round() as u16)
        };
        // Position used for buttons: the pointer when it is known, otherwise the
        // last place it was inside the canvas.
        let anchor = hover.unwrap_or(image.center());
        let (ax, ay) = norm(anchor);

        if over {
            if let Some(p) = hover {
                if pointer.delta() != Vec2::ZERO {
                    let (x, y) = norm(p);
                    let _ = self.cmd_tx.send(Cmd::Input(InputEvent::MouseMove {
                        x,
                        y,
                        display: self.display,
                    }));
                }
            }
        }

        // Presses need the canvas, releases do not: a drag that ends outside the
        // window must still lift the remote button.
        for (button, native) in [
            (egui::PointerButton::Primary, MouseButton::Left),
            (egui::PointerButton::Secondary, MouseButton::Right),
            (egui::PointerButton::Middle, MouseButton::Middle),
        ] {
            // A press must originate inside the canvas. Accepting presses while
            // the canvas merely has focus turns the remote pointer's own events
            // into new presses (a runaway click loop when self-testing).
            if pointer.button_pressed(button) && over {
                self.held_buttons.insert(native);
                let _ = self.cmd_tx.send(Cmd::Input(InputEvent::MouseButton {
                    button: native,
                    down: true,
                    x: ax,
                    y: ay,
                    display: self.display,
                }));
            }
            if pointer.button_released(button) && self.held_buttons.remove(&native) {
                let _ = self.cmd_tx.send(Cmd::Input(InputEvent::MouseButton {
                    button: native,
                    down: false,
                    x: ax,
                    y: ay,
                    display: self.display,
                }));
            }
        }

        if over {
            let scroll = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => Some(
                            *delta
                                * match unit {
                                    egui::MouseWheelUnit::Point => 1.,
                                    egui::MouseWheelUnit::Line => 40.,
                                    egui::MouseWheelUnit::Page => image.height(),
                                },
                        ),
                        _ => None,
                    })
                    .fold(Vec2::ZERO, |a, b| a + b)
            });
            if scroll.y.abs() > 0.5 || scroll.x.abs() > 0.5 {
                let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Wheel {
                    dx: (scroll.x / 40.0).round() as i32,
                    dy: (scroll.y / 40.0).round() as i32,
                    x: ax,
                    y: ay,
                    display: self.display,
                }));
            }
        }

        if !active {
            return;
        }
        let mut bitmap = 0u8;
        if mods_state.shift {
            bitmap |= mods::SHIFT;
        }
        if mods_state.ctrl {
            bitmap |= mods::CONTROL;
        }
        if mods_state.alt {
            bitmap |= mods::ALT;
        }
        if mods_state.mac_cmd {
            bitmap |= mods::META;
        }
        for (mask, code) in [
            (mods::SHIFT, "ShiftLeft"),
            (mods::CONTROL, "ControlLeft"),
            (mods::ALT, "AltLeft"),
            (mods::META, "MetaLeft"),
        ] {
            if (bitmap ^ self.modifiers) & mask != 0 {
                let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Key {
                    code: code.into(),
                    down: bitmap & mask != 0,
                    modifiers: bitmap,
                }));
            }
        }
        self.modifiers = bitmap;
        for event in ctx.input(|i| i.events.clone()) {
            match event {
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    ..
                } => {
                    if repeat || (pressed && self.composition.composing()) {
                        continue;
                    }
                    if let Some(code) = w3c_code(key) {
                        if pressed {
                            if bitmap & (mods::CONTROL | mods::ALT | mods::META) == 0
                                && printable(&code)
                            {
                                continue;
                            }
                            self.held_keys.insert(code.clone());
                        } else if !self.held_keys.remove(&code) {
                            continue;
                        }
                        let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Key {
                            code,
                            down: pressed,
                            modifiers: bitmap,
                        }));
                    }
                }
                egui::Event::Text(text)
                    if !self.composition.composing()
                        && !text.is_empty()
                        && !text.chars().any(|c| c.is_control()) =>
                {
                    let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Text { text }));
                }
                egui::Event::Copy | egui::Event::Cut => {
                    let code = if matches!(event, egui::Event::Copy) {
                        "KeyC"
                    } else {
                        "KeyX"
                    };
                    for down in [true, false] {
                        let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Key {
                            code: code.into(),
                            down,
                            modifiers: bitmap,
                        }));
                    }
                }
                egui::Event::Ime(event) => {
                    if let Some(text) = self.composition.event(event) {
                        let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Text { text }));
                    }
                }
                egui::Event::Paste(text) => {
                    let _ = self.cmd_tx.send(Cmd::Input(InputEvent::Text { text }));
                }
                _ => {}
            }
        }
        self.composition.present(ui, image);
    }
}

// ---- styling helpers ------------------------------------------------------

fn generate_password() -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let mut rng = rand::rand_core::UnwrapErr(rand::rngs::SysRng);
    (0..10)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}
fn printable(code: &str) -> bool {
    code.starts_with("Key")
        || code.starts_with("Digit")
        || matches!(
            code,
            "Space"
                | "Minus"
                | "Equal"
                | "BracketLeft"
                | "BracketRight"
                | "Backslash"
                | "Semicolon"
                | "Quote"
                | "Comma"
                | "Period"
                | "Slash"
                | "Backquote"
        )
}
fn w3c_code(key: egui::Key) -> Option<String> {
    use egui::Key as K;
    let name = match key {
        K::A => "KeyA",
        K::B => "KeyB",
        K::C => "KeyC",
        K::D => "KeyD",
        K::E => "KeyE",
        K::F => "KeyF",
        K::G => "KeyG",
        K::H => "KeyH",
        K::I => "KeyI",
        K::J => "KeyJ",
        K::K => "KeyK",
        K::L => "KeyL",
        K::M => "KeyM",
        K::N => "KeyN",
        K::O => "KeyO",
        K::P => "KeyP",
        K::Q => "KeyQ",
        K::R => "KeyR",
        K::S => "KeyS",
        K::T => "KeyT",
        K::U => "KeyU",
        K::V => "KeyV",
        K::W => "KeyW",
        K::X => "KeyX",
        K::Y => "KeyY",
        K::Z => "KeyZ",
        K::Num0 => "Digit0",
        K::Num1 => "Digit1",
        K::Num2 => "Digit2",
        K::Num3 => "Digit3",
        K::Num4 => "Digit4",
        K::Num5 => "Digit5",
        K::Num6 => "Digit6",
        K::Num7 => "Digit7",
        K::Num8 => "Digit8",
        K::Num9 => "Digit9",
        K::Enter => "Enter",
        K::Escape => "Escape",
        K::Tab => "Tab",
        K::Backspace => "Backspace",
        K::Delete => "Delete",
        K::Space => "Space",
        K::ArrowUp => "ArrowUp",
        K::ArrowDown => "ArrowDown",
        K::ArrowLeft => "ArrowLeft",
        K::ArrowRight => "ArrowRight",
        K::Home => "Home",
        K::End => "End",
        K::PageUp => "PageUp",
        K::PageDown => "PageDown",
        K::Insert => "Insert",
        K::F1 => "F1",
        K::F2 => "F2",
        K::F3 => "F3",
        K::F4 => "F4",
        K::F5 => "F5",
        K::F6 => "F6",
        K::F7 => "F7",
        K::F8 => "F8",
        K::F9 => "F9",
        K::F10 => "F10",
        K::F11 => "F11",
        K::F12 => "F12",
        K::Minus => "Minus",
        K::Plus | K::Equals => "Equal",
        K::OpenBracket => "BracketLeft",
        K::CloseBracket => "BracketRight",
        K::Backslash => "Backslash",
        K::Semicolon => "Semicolon",
        K::Quote => "Quote",
        K::Comma => "Comma",
        K::Period => "Period",
        K::Slash => "Slash",
        K::Backtick => "Backquote",
        _ => return None,
    };
    Some(name.to_string())
}

#[derive(Default, Clone)]
struct Config {
    /// Space separated broker URLs; empty means the built-in defaults.
    brokers: String,
    device_name: String,
    internet_code: String,
    /// `--connect CODE` dials immediately after start-up.
    connect: String,
    /// `--password PASSWORD` pairs with `--connect`.
    password: String,
    /// Launch at login. Defaults to on for packaged builds.
    autostart: Option<bool>,
    theme_preference: Option<egui::ThemePreference>,
    /// Open the window minimised (only the menu-bar / tray icon shows).
    start_minimized: bool,
    /// Closing the window keeps sharing in the background instead of quitting.
    close_to_background: bool,
    /// Windows: we already offered to add the inbound firewall rule.
    firewall_offered: bool,
    /// Long-term access: when set, this password is reused instead of a random one.
    fixed_password: String,
    /// Where downloads are written; empty means `~/Downloads`.
    save_dir: String,
    shared_dir: String,
    auto_close_files: bool,
    resolution: Resolution,
    /// Optional self-hosted signal server URL.
    signal_url: String,
}
/// Values from an argument list, so `--connect` works without pulling clap into
/// the GUI (which only ever has two optional flags).
fn cli_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StoredConfig {
    #[serde(default)]
    brokers: String,
    #[serde(default)]
    device_name: String,
    /// Kept across launches so the number the user wrote down stays valid; the
    /// password next to it is rotated on every start.
    #[serde(default)]
    internet_code: String,
    #[serde(default)]
    autostart: Option<bool>,
    #[serde(default)]
    theme_preference: Option<egui::ThemePreference>,
    #[serde(default)]
    start_minimized: bool,
    #[serde(default)]
    close_to_background: bool,
    #[serde(default)]
    firewall_offered: bool,
    #[serde(default)]
    fixed_password: String,
    #[serde(default)]
    save_dir: String,
    #[serde(default)]
    resolution: Resolution,
    #[serde(default)]
    auto_close_files: bool,
    #[serde(default)]
    shared_dir: String,
    #[serde(default)]
    signal_url: String,
}

impl From<StoredConfig> for Config {
    fn from(stored: StoredConfig) -> Self {
        Self {
            brokers: stored.brokers,
            device_name: stored.device_name,
            internet_code: stored.internet_code,
            connect: String::new(),
            password: String::new(),
            autostart: stored.autostart,
            theme_preference: stored.theme_preference,
            start_minimized: stored.start_minimized,
            close_to_background: stored.close_to_background,
            firewall_offered: stored.firewall_offered,
            fixed_password: stored.fixed_password,
            save_dir: stored.save_dir,
            shared_dir: stored.shared_dir,
            auto_close_files: stored.auto_close_files,
            resolution: stored.resolution,
            signal_url: stored.signal_url,
        }
    }
}

/// Windows reports "logical" monitor sizes to DPI-unaware processes. Without
/// this, a 1920×1080 desktop at 150 % scaling is captured and announced as
/// 1261×857 — which looks blurry no matter what quality the viewer picks.
#[cfg(windows)]
fn make_process_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // Fails harmlessly when the manifest already declared awareness.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
fn make_process_dpi_aware() {}

fn main() -> eframe::Result<()> {
    make_process_dpi_aware();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../../../assets/brand/png/appicon/velarouter-256.png"
    ))
    .ok();
    // Window size can be pinned by the environment, which makes it easy to
    // check the narrow layout (and to open the app on a small secondary screen).
    let (start_w, start_h) = std::env::var("VELAROUTER_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((1100.0, 790.0));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([start_w, start_h])
            .with_min_inner_size([360.0, 360.0])
            .with_app_id("Velarouter")
            .with_title("Velarouter")
            .with_icon(icon.map(Arc::new).unwrap_or_default()),
        // A pinned size must win over the remembered geometry, otherwise the
        // narrow-layout check silently reopens at the old size.
        persist_window: std::env::var("VELAROUTER_WINDOW").is_err(),
        ..Default::default()
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    eframe::run_native(
        "Velarouter",
        options,
        Box::new(|cc| {
            let stored: Config = cc
                .storage
                .and_then(|s| s.get_string("velarouter.config"))
                .and_then(|json| serde_json::from_str::<StoredConfig>(&json).ok())
                .unwrap_or_default()
                .into();
            let cfg = Config {
                connect: cli_value("--connect").unwrap_or_default(),
                password: cli_value("--password").unwrap_or_default(),
                ..stored
            };
            Ok(Box::new(App::new(cc, cfg, rt)))
        }),
    )
}

// ---- worker ---------------------------------------------------------------

async fn worker(
    req: ConnectReq,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    ev_tx: Sender<Ev>,
    ctx: egui::Context,
    latest: FrameSlot,
) {
    let send = |ev: Ev| {
        let _ = ev_tx.send(ev);
        ctx.request_repaint();
    };
    let mut caps = vr_client::default_capabilities();
    caps.max_width = 3840;
    caps.max_height = 2160;
    let connected = match req.target {
        Target::Direct(addr) => {
            tokio::time::timeout(
                Duration::from_secs(8),
                vr_client::connect_lan(
                    &format!("ws://{addr}/vr"),
                    &req.password,
                    "velarouter-desktop/0.1",
                    caps,
                ),
            )
            .await
        }
        Target::Meet { code } => {
            tokio::time::timeout(
                Duration::from_secs(55),
                vr_client::connect_meet(
                    &code,
                    &req.password,
                    &req.brokers,
                    "velarouter-desktop/0.2",
                    caps,
                ),
            )
            .await
        }
        Target::Signal { base, code } => {
            tokio::time::timeout(
                Duration::from_secs(55),
                vr_client::connect_p2p(&base, &code, &req.password, "velarouter-desktop/0.2", caps),
            )
            .await
        }
    };
    let connected = match connected {
        Ok(r) => r,
        Err(_) => {
            send(Ev::Error(
                "网络错误 · 连接超时。请确认对方仍在共享，并检查端口映射、防火墙或 UDP 限制。"
                    .into(),
            ));
            return;
        }
    };
    let mut viewer: Viewer = match connected {
        Ok(v) => v,
        Err(e) => {
            let hint = match e.downcast_ref::<vr_session::SessionError>() {
                Some(vr_session::SessionError::Auth(_) | vr_session::SessionError::Rejected(_)) => {
                    "密码不对，或对方已更换密码。请让对方重新念一遍当前密码。"
                }
                _ => "没能建立连接。请确认对方仍在共享、联机码没有过期，并且两边都能访问互联网。",
            };
            send(Ev::Error(format!("{hint}\n{e:#}")));
            return;
        }
    };
    let peer_name = viewer
        .peer
        .device
        .as_ref()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "远端设备".into());
    send(Ev::Connected {
        peer: peer_name,
        transport: viewer.transport,
    });

    let (max_w, max_h) = req.initial_size;
    if let Err(e) = vr_client::request_video(&mut viewer, 70, 30, 0, max_w, max_h).await {
        send(Ev::Error(format!("{e:#}")));
        return;
    }

    // ---- file transfers ---------------------------------------------------
    let mut next_transfer: u32 = 1;
    let mut download: Option<files::Download> = None;
    let mut upload: Option<files::Upload> = None;
    let mut listing: Option<(u32, String)> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(Cmd::Input(event)) => {
                        if let Err(e) = viewer.sender.send_control(Control::Input { event }).await {
                            send(Ev::Error(format!("发送输入失败：{e}")));
                            break;
                        }
                    }
                    Some(Cmd::Video { quality, fps, display, max_w, max_h }) => {
                        let (max_width, max_height) = (max_w, max_h);
                        if let Err(e) = viewer.sender.send_control(Control::VideoConfig(vr_proto::VideoConfig {
                            codec: vr_proto::Codec::Jpeg,
                            quality, fps, max_width, max_height, display,
                        })).await { send(Ev::Closed(e.to_string())); break; }
                    }
                    Some(Cmd::ListFiles { path }) => {
                        let request_id = next_transfer;
                        next_transfer = next_transfer.wrapping_add(1).max(1);
                        listing = Some((request_id,path.clone()));
                        if let Err(e) = viewer.sender.send_control(Control::FileListRequest {
                            request_id,
                            path,
                        }).await { send(Ev::Closed(e.to_string())); break; }
                    }
                    Some(Cmd::Download { path, save_dir }) => {
                        let transfer_id = next_transfer;
                        next_transfer = next_transfer.wrapping_add(1).max(1);
                        match files::Download::start(transfer_id, &path, save_dir.as_deref()).await {
                            Ok(state) => {
                                if let Err(e) = viewer.sender.send_control(Control::FilePull {
                                    transfer_id,
                                    path: path.clone(),
                                    offset: 0,
                                }).await { state.discard().await; send(Ev::Closed(e.to_string())); break; }
                                download = Some(state);
                            }
                            Err(e) => send(Ev::Transfer(TransferView {
                                label: format!("下载 {path}"),
                                done: 0,
                                total: 0,
                                finished: true,
                                error: Some(format!("无法创建本地文件：{e}")),
                            })),
                        }
                    }
                    Some(Cmd::Upload { local, remote_dir }) => {
                        let transfer_id = next_transfer;
                        next_transfer = next_transfer.wrapping_add(1).max(1);
                        let source = local.display().to_string();
                        match files::Upload::start(transfer_id, local).await {
                            Ok(state) => {
                                let name = if remote_dir.is_empty() {
                                    state.name.clone()
                                } else {
                                    format!("{}/{}", remote_dir.trim_end_matches('/'), state.name)
                                };
                                if let Err(e) = viewer.sender.send_control(Control::FilePush {
                                    transfer_id,
                                    name,
                                    size: state.total,
                                    offset: 0,
                                    overwrite: false,
                                }).await { send(Ev::Closed(e.to_string())); break; }
                                send(Ev::Transfer(TransferView {
                                    label: format!("上传 {}", state.name),
                                    done: 0,
                                    total: state.total,
                                    finished: false,
                                    error: None,
                                }));
                                upload = Some(state);
                            }
                            Err(e) => send(Ev::Transfer(TransferView {
                                label: format!("上传 {source}"),
                                done: 0,
                                total: 0,
                                finished: true,
                                error: Some(format!("无法读取本地文件：{e}")),
                            })),
                        }
                    }
                    Some(Cmd::Disconnect) | None => {
                        let _ = viewer.sender.send_control(Control::Bye { reason: "desktop closed".into() }).await;
                        break;
                    }
                }
            }
            chunk = async {
                match upload.as_mut() {
                    Some(state) if !state.finished_sending() => state.next_chunk().await,
                    _ => std::future::pending().await,
                }
            } => {
                match chunk {
                    Some(Ok((header, payload, done))) => {
                        if let Err(err) = viewer.sender.send_file_chunk(header, &payload).await { send(Ev::Closed(err.to_string())); break; } else {
                            if let Some(state) = upload.as_mut() {
                                state.sent = done;
                                if done == state.reported + 1_048_576 {
                                    state.reported = done;
                                    send(Ev::Transfer(TransferView {
                                        label: format!("上传 {}", state.name),
                                        done,
                                        total: state.total,
                                        finished: false,
                                        error: None,
                                    }));
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        let label = upload
                            .as_ref()
                            .map(|u| format!("上传 {}", u.name))
                            .unwrap_or_else(|| "上传".into());
                        if let Some(u) = upload.take() { let _ = viewer.sender.send_control(Control::FileCancel { transfer_id:u.id }).await; }
                        send(Ev::Transfer(TransferView {
                            label,
                            done: 0,
                            total: 0,
                            finished: true,
                            error: Some(format!("读取失败：{e}")),
                        }));
                    }
                    None => unreachable!("a finished upload waits for FileComplete"),
                }
            }
            ev = viewer.receiver.recv_auto_pong(&mut viewer.sender) => {
                match ev {
                    Ok(Event::Media { header: _, payload }) => {
                        match image::load_from_memory_with_format(&payload, image::ImageFormat::Jpeg) {
                            Ok(img) => {
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                *latest.lock().unwrap()=Some(DecodedFrame { width:w,height:h,rgba:rgba.into_raw() }); ctx.request_repaint();
                            }
                            Err(_) => continue,
                        }
                    }
                    Ok(Event::Control(Control::DisplayList { displays })) => {
                        send(Ev::Displays(displays));
                    }
                    Ok(Event::Control(Control::Stats(stats))) => send(Ev::Stats(stats)),
                    Ok(Event::Control(Control::Clipboard(ClipboardMsg { direction, text }))) => {
                        if direction == vr_proto::ClipboardDirection::AgentToViewer {
                            ctx.copy_text(text);
                        }
                    }
                    Ok(Event::Control(Control::Warning { code, .. })) if code == "input_ready" => send(Ev::InputWarning(String::new())),
                    Ok(Event::Control(Control::Warning { code, message })) if code == "input" => {send(Ev::InputWarning(format!("仅观看 · {message}")));}
                    Ok(Event::Control(Control::Warning { message, .. })) => {
                        tracing::warn!(%message, "agent warning");
                    }
                    Ok(Event::Control(Control::Bye { reason })) => {
                        send(Ev::Closed(reason));
                        break;
                    }
                    Ok(Event::Control(Control::FileList { request_id, root, entries, .. })) => {
                        if listing.as_ref().is_some_and(|(id,_)| *id == request_id) {
                            let (_,path) = listing.take().unwrap(); // Older agents echo an empty path for subdirectories.
                            send(Ev::Files { path, root, entries });
                        }
                    }
                    Ok(Event::Control(Control::FileListError { request_id, message, .. })) => {
                        if listing.as_ref().is_some_and(|(id,_)| *id == request_id) { listing = None; send(Ev::ListError(message)); }
                    }
                    Ok(Event::Control(Control::FileProgress { transfer_id, offset, total })) => {
                        if let Some(state) = download.as_mut().filter(|d| d.id == transfer_id) {
                            state.total = total;
                            let view = TransferView {
                                label: state.label.clone(),
                                done: offset.max(state.written),
                                total,
                                finished: false,
                                error: None,
                            };
                            send(Ev::Transfer(view));
                        } else if let Some(state) = upload.as_mut().filter(|u| u.id == transfer_id) {
                            let view = TransferView {
                                label: format!("上传 {}", state.name),
                                done: offset,
                                total,
                                finished: false,
                                error: None,
                            };
                            send(Ev::Transfer(view));
                        }
                    }
                    Ok(Event::Control(Control::FileComplete { transfer_id, sha256, .. })) => {
                        if let Some(state) = download.as_mut().filter(|d| d.id == transfer_id) {
                            let ok = state.finish(&sha256).await;
                            let done = state.written;
                            let label = state.label.clone();
                            download = None;
                            send(Ev::Transfer(TransferView {
                                label,
                                done,
                                total: done,
                                finished: true,
                                error: if ok { None } else { Some("校验失败，请重试".into()) },
                            }));
                        } else if upload.as_ref().is_some_and(|u| u.id == transfer_id) {
                            if let Some(state) = upload.take() {
                                send(Ev::Transfer(TransferView {
                                    label: format!("上传 {}", state.name),
                                    done: state.total,
                                    total: state.total,
                                    finished: true,
                                    error: None,
                                }));
                            }
                        }
                    }
                    Ok(Event::Control(Control::FileError { transfer_id, message, .. })) => {
                        let label;
                        let total;
                        if download.as_ref().is_some_and(|d| d.id == transfer_id) {
                            let state = download.take().unwrap();
                            state.discard().await;
                            label = state.label;
                            total = state.written;
                        } else if upload.as_ref().is_some_and(|u| u.id == transfer_id) {
                            let state = upload.take().unwrap();
                            label = format!("上传 {}", state.name);
                            total = state.total;
                        } else { continue; }
                        send(Ev::Transfer(TransferView {
                            label,
                            done: 0,
                            total,
                            finished: true,
                            error: Some(message),
                        }));
                    }
                    Ok(Event::Control(_)) | Ok(Event::Pong { .. }) => {}
                    Ok(Event::File { header, payload }) => {
                        if let Some(state) = download.as_mut().filter(|d| d.id == header.transfer_id) {
                            match state.write(&payload).await {
                                Ok(done) => {
                                    if done == state.reported + 1_048_576 || header.is_last() {
                                        state.reported = done;
                                        send(Ev::Transfer(TransferView {
                                            label: state.label.clone(),
                                            done,
                                            total: state.total,
                                            finished: false,
                                            error: None,
                                        }));
                                    }
                                }
                                Err(e) => {
                                    let label = state.label.clone();
                                    state.discard().await;
                                    download = None;
                                    send(Ev::Transfer(TransferView {
                                        label,
                                        done: 0,
                                        total: 0,
                                        finished: true,
                                        error: Some(format!("写入失败：{e}")),
                                    }));
                                }
                            }
                        }
                    }
                    Ok(Event::Closed(reason)) => {
                        send(Ev::Closed(reason.unwrap_or_default()));
                        break;
                    }
                    Err(e) => {
                        send(Ev::Closed(e.to_string()));
                        break;
                    }
                }
            }
        }
    }
    if let Some(state) = download {
        state.discard().await;
    }
}

#[allow(dead_code)]
fn _unused(_: TouchPhase, _: TouchPoint) {}
