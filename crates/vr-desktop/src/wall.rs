//! Explicit, bounded view-only sessions. No credentials or frames are persisted.
use super::*;

const MAX_TILES: usize = 4;
use vr_proto::{WALL_FPS as FPS, WALL_HEIGHT as HEIGHT, WALL_WIDTH as WIDTH};
const NO_FRAME_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub(super) struct Wall {
    tiles: Vec<Tile>,
    code: String,
    password: String,
    group: String,
    error: String,
    filter: Option<String>,
    enlarged: Option<u64>,
    next_id: u64,
}

struct Tile {
    id: u64,
    code: String,
    group: String,
    name: Option<String>,
    status: String,
    frame: FrameSlot,
    events: Receiver<Update>,
    task: tokio::task::JoinHandle<()>,
    texture: Option<TextureHandle>,
}

impl Drop for Tile {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Update {
    Authenticated(String),
    State(String),
    End(String),
}

impl Wall {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn update(&mut self, ctx: &egui::Context) {
        for tile in &mut self.tiles {
            while let Ok(event) = tile.events.try_recv() {
                match event {
                    Update::Authenticated(name) => {
                        tile.name = Some(name);
                        tile.status = "已认证 · 等待画面".into();
                    }
                    Update::State(status) => tile.status = status,
                    Update::End(status) => {
                        tile.status = status;
                        tile.texture = None;
                        tile.frame.lock().unwrap().take();
                    }
                }
            }
            if let Some(frame) = tile.frame.lock().unwrap().take() {
                let image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                if let Some(texture) = &mut tile.texture {
                    texture.set(image, TextureOptions::LINEAR);
                } else {
                    tile.texture = Some(ctx.load_texture(
                        format!("wall-{}", tile.id),
                        image,
                        TextureOptions::LINEAR,
                    ));
                }
            }
        }
    }

    fn add(&mut self, app: &App, ctx: &egui::Context) -> Result<(), &'static str> {
        if self.tiles.len() >= MAX_TILES {
            return Err("最多预览 4 台，请先移除一台。");
        }
        let code = self
            .code
            .split_whitespace()
            .collect::<String>()
            .to_ascii_uppercase();
        if self.tiles.iter().any(|tile| tile.code == code) {
            return Err("此联机码已经在桌面墙中。");
        }
        if self.password.is_empty() {
            return Err("请输入连接密码。");
        }
        let target = app.parse_target(&code)?;
        let request = ConnectReq {
            target,
            entered_code: String::new(),
            password: std::mem::take(&mut self.password),
            viewer_identity: app.viewer_identity.clone(),
            brokers: app.brokers.clone(),
            initial_size: (WIDTH, HEIGHT),
        };
        self.start(code, request, app.rt.handle(), ctx);
        self.code.clear();
        Ok(())
    }

    fn start(
        &mut self,
        code: String,
        request: ConnectReq,
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        let frame = FrameSlot::default();
        let (tx, events) = std::sync::mpsc::channel();
        let task = runtime.spawn(preview(request, frame.clone(), tx, ctx.clone()));
        let group = self.group.trim().chars().take(24).collect::<String>();
        self.tiles.push(Tile {
            id,
            code,
            group: if group.is_empty() {
                "未分组".into()
            } else {
                group
            },
            name: None,
            status: "正在连接…".into(),
            frame,
            events,
            task,
            texture: None,
        });
        self.filter = None;
        self.enlarged = None;
    }
}

impl App {
    pub(super) fn wall_ui(&mut self, ui: &mut egui::Ui) {
        let mut wall = std::mem::take(&mut self.wall);
        wall.update(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            ui.heading("桌面墙 · 只读预览");
            if ui.button("返回首页并停止预览").clicked() {
                self.page = Page::Home;
            }
        });
        if self.page != Page::Wall {
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.weak("最多 4 台，每台最高 480×270 / 3 fps。手动连接，不保存密码。需两端使用支持桌面墙的版本。");
            egui::CollapsingHeader::new("添加设备")
                .default_open(true)
                .show(ui, |ui| {
                theme::field(ui, "对方的联机码", &mut wall.code, "原有联机码");
                theme::section_label(ui, "连接密码");
                let password = theme::input(ui, &mut wall.password, "由对方提供", true);
                theme::field(ui, "分组（可选）", &mut wall.group, "例如：工作室");
                if ui.button("开始只读预览").clicked()
                    || (password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    wall.error = wall.add(self, ui.ctx()).err().unwrap_or_default().into();
                }
                if !wall.error.is_empty() { ui.colored_label(theme::DANGER, &wall.error); }
            });
            if wall.tiles.is_empty() { ui.label("展开「添加设备」，用对方的联机码和密码开始预览。"); return; }
            let groups: std::collections::BTreeSet<_> = wall.tiles.iter().map(|tile| tile.group.clone()).collect();
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(wall.filter.is_none() && wall.enlarged.is_none(), "全部").clicked() {
                    wall.filter = None;
                    wall.enlarged = None;
                }
                for group in groups {
                    if ui.selectable_label(wall.filter.as_deref() == Some(group.as_str()) && wall.enlarged.is_none(), &group).clicked() {
                        wall.filter = Some(group);
                        wall.enlarged = None;
                    }
                }
            });
            let mut remove = None;
            let visible: Vec<_> = wall.tiles.iter().filter(|tile| match wall.enlarged {
                Some(id) => tile.id == id,
                None => wall.filter.as_ref().is_none_or(|group| group == &tile.group),
            }).collect();
            let columns = if wall.enlarged.is_none() && ui.available_width() >= 640. { 2 } else { 1 };
            for row in visible.chunks(columns) {
                ui.columns(columns, |cols| {
                    for (ui, tile) in cols.iter_mut().zip(row) {
                        theme::card_frame(ui).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(egui::Label::new(format!("{} · {}", tile.group, tile.name.as_deref().unwrap_or(&tile.code))).truncate()).on_hover_text(&tile.code);
                            ui.weak(&tile.status);
                            let width = ui.available_width();
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * 9. / 16.), Sense::hover());
                            ui.painter().rect_filled(rect, 5., Color32::BLACK);
                            if let Some(texture) = &tile.texture {
                                let size = texture.size_vec2();
                                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                                ui.painter().image(texture.id(), Rect::from_center_size(rect.center(), size * scale), Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)), Color32::WHITE);
                            }
                            ui.horizontal_wrapped(|ui| {
                                if ui.button(if wall.enlarged.is_some() { "返回网格" } else { "放大预览" }).clicked() {
                                    wall.enlarged = if wall.enlarged.is_some() { None } else { Some(tile.id) };
                                }
                                if ui.button("移除").clicked() { remove = Some(tile.id); }
                            });
                        });
                    }
                });
            }
            if let Some(id) = remove {
                wall.tiles.retain(|tile| tile.id != id);
                wall.enlarged = None;
                if !wall.tiles.iter().any(|tile| wall.filter.as_ref() == Some(&tile.group)) { wall.filter = None; }
            }
        });
        self.wall = wall;
    }
}

fn capabilities() -> vr_proto::Capabilities {
    vr_proto::Capabilities {
        codecs: vec![vr_proto::Codec::Jpeg],
        max_width: WIDTH,
        max_height: HEIGHT,
        max_fps: FPS,
        input: false,
        clipboard: false,
        multi_display: false,
        audio: false,
    }
}

async fn preview(req: ConnectReq, frame: FrameSlot, tx: Sender<Update>, ctx: egui::Context) {
    let publish = |event| {
        let _ = tx.send(event);
        ctx.request_repaint();
    };
    let app = format!("{}{}", vr_proto::WALL_APP_PREFIX, env!("CARGO_PKG_VERSION"));
    let mut viewer = match dial(
        req.target,
        &req.password,
        &req.brokers,
        req.viewer_identity.as_ref(),
        capabilities(),
        &app,
    )
    .await
    {
        Ok(viewer) => viewer,
        Err(error) => {
            publish(Update::End(connection_error(&error)));
            return;
        }
    };
    drop(req.password);
    let result = run_preview(&mut viewer, &frame, &ctx, &publish).await;
    publish(Update::End(
        result
            .err()
            .map_or_else(|| "预览已断开".into(), |e| e.to_string()),
    ));
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        viewer.sender.send_control(Control::Bye {
            reason: "preview ended".into(),
        }),
    )
    .await;
}

async fn run_preview(
    viewer: &mut Viewer,
    frame: &FrameSlot,
    ctx: &egui::Context,
    publish: impl Fn(Update),
) -> anyhow::Result<()> {
    // Do not accept even a frame until the host confirms server-side restrictions.
    let confirmation = tokio::time::timeout(Duration::from_secs(5), viewer.receiver.recv()).await;
    anyhow::ensure!(
        matches!(confirmation, Ok(Ok(Event::Control(Control::ViewOnly)))),
        "未收到只读确认。请确认对方仍在共享，并使用支持桌面墙的版本。"
    );
    vr_client::request_video(viewer, 40, FPS, 0, WIDTH, HEIGHT).await?;
    let name = viewer
        .peer
        .device
        .as_ref()
        .map(|device| device.name.as_str())
        .unwrap_or("远端设备");
    publish(Update::Authenticated(name.chars().take(64).collect()));
    let mut first = true;
    let first_deadline = tokio::time::Instant::now() + NO_FRAME_TIMEOUT;
    loop {
        let receive = viewer.receiver.recv_auto_pong(&mut viewer.sender);
        // JPEG skips unchanged frames; only the initial image has a deadline.
        let received = if first {
            tokio::time::timeout_at(first_deadline, receive)
                .await
                .map_err(|_| anyhow::anyhow!("未收到画面，请检查对方的屏幕共享权限。"))?
        } else {
            receive.await
        };
        match received {
            Ok(Event::Media { header, payload })
                if header.codec == vr_proto::Codec::Jpeg
                    && header.width <= WIDTH
                    && header.height <= HEIGHT =>
            {
                let Some(decoded) = decode_preview(&payload, header.width, header.height) else {
                    continue;
                };
                *frame.lock().unwrap() = Some(decoded);
                ctx.request_repaint();
                if first {
                    first = false;
                    publish(Update::State("实时 · 只读".into()));
                }
            }
            Ok(Event::Closed(_)) => return Ok(()),
            Ok(_) => {}
            Err(_) => anyhow::bail!("预览连接已中断。"),
        }
    }
}

fn decode_preview(bytes: &[u8], width: u16, height: u16) -> Option<DecodedFrame> {
    if bytes.len() > 512 * 1024 {
        return None;
    }
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(u32::from(WIDTH));
    limits.max_image_height = Some(u32::from(HEIGHT));
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.into_rgba8();
    (rgba.dimensions() == (u32::from(width), u32::from(height))).then(|| DecodedFrame {
        width: u32::from(width),
        height: u32::from(height),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    mod xvfb;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image::RgbImage::new(width, height))
            .unwrap();
        bytes
    }

    #[test]
    fn frame_dimensions_must_match_and_fit_preview_budget() {
        let normal = jpeg(WIDTH.into(), HEIGHT.into());
        assert!(decode_preview(&normal, WIDTH, HEIGHT).is_some());
        assert!(decode_preview(&normal, WIDTH - 1, HEIGHT).is_none());
        assert!(
            decode_preview(&jpeg(u32::from(WIDTH) + 1, HEIGHT.into()), WIDTH, HEIGHT).is_none()
        );
        assert!(decode_preview(&[0], 1, 1).is_none());
    }
}
