//! Landing page, connection form and preferences.
//!
//! Layout rules: one accent colour, two cards, large type, and every status is
//! phrased as either "ready" or "what is missing" — never a bare error code.
use super::*;
use crate::theme::{self, field, ACCENT};
use std::net::SocketAddrV4;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const TWO_COLUMNS_AT: f32 = 760.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum HomeTab {
    Share,
    Connect,
    Lan,
}

#[cfg(target_os = "macos")]
fn accessibility_enabled() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    // A read-only TCC query; does not prompt or change permissions.
    unsafe { AXIsProcessTrusted() }
}
fn surface(ui: &egui::Ui) -> Color32 {
    theme::inset(ui)
}

/// `1963 4674 4765` in three chips, so a code can be read out loud.
fn code_digits(ui: &mut egui::Ui, code: &str) {
    if code.len() == 9 && code.bytes().all(|b| b.is_ascii_digit()) {
        ui.label(
            RichText::new(format!("{} {} {}", &code[..3], &code[3..6], &code[6..]))
                .size(32.0)
                .family(egui::FontFamily::Monospace)
                .strong(),
        );
        return;
    }
    if vr_proto::is_internet_code(code) {
        let (font_size, padding) = if ui.available_width() < 300.0 {
            (22.0, 7)
        } else {
            (28.0, 10)
        };
        ui.horizontal_wrapped(|ui| {
            for chunk in [&code[..4], &code[4..8], &code[8..]] {
                egui::Frame::new()
                    .fill(theme::card(ui))
                    .stroke(egui::Stroke::new(1.0, theme::border(ui)))
                    .corner_radius(12)
                    .inner_margin(Margin::symmetric(padding, 3))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(chunk)
                                .size(font_size)
                                .family(egui::FontFamily::Monospace)
                                .strong(),
                        );
                    });
            }
        });
    } else {
        ui.label(
            RichText::new(code)
                .size(24.0)
                .family(egui::FontFamily::Monospace)
                .strong(),
        );
    }
}

/// Section title with a copy action on the same line, so the code block below
/// contains nothing but the code.
fn code_header(ui: &mut egui::Ui, label: &str, code: String) {
    ui.horizontal(|ui| {
        theme::section_label(ui, label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::secondary(ui, "复制").clicked() {
                ui.ctx().copy_text(code);
            }
        });
    });
}

fn status_line(ui: &mut egui::Ui, color: Color32, text: impl Into<String>) {
    let text = text.into();
    ui.horizontal_wrapped(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.6, color);
        ui.label(RichText::new(text).size(14.0).color(color));
    });
}

impl App {
    pub(super) fn brand(&self, ui: &mut egui::Ui) {
        if let Some(icon) = &self.brand_icon {
            ui.add(egui::Image::new(icon).fit_to_exact_size(Vec2::splat(42.0)));
        }
        ui.label(
            RichText::new("Velarouter")
                .size(25.0)
                .strong()
                .color(ui.visuals().strong_text_color()),
        );
    }

    pub(super) fn home_ui(&mut self, ui: &mut egui::Ui) {
        self.home_content(ui);
    }

    fn home_content(&mut self, ui: &mut egui::Ui) -> egui::scroll_area::ScrollAreaOutput<()> {
        // Choose layout from the panel, not the scroll area: showing a scrollbar
        // must not switch the layout back and forth at the breakpoint.
        let wide = ui.available_width() >= TWO_COLUMNS_AT;
        let show_headline = wide && ui.available_height() >= 565.0;
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.spacing_mut().button_padding = Vec2::new(10.0, 5.0);
        ui.spacing_mut().interact_size.y = 28.0;
        ui.add_space(6.0);

        if !wide {
            theme::centered_width(ui, 540.0, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (tab, name) in [
                        (HomeTab::Share, "本机共享"),
                        (HomeTab::Connect, "连接电脑"),
                        (HomeTab::Lan, "局域网"),
                    ] {
                        if ui.selectable_label(self.home_tab == tab, name).clicked() {
                            self.home_tab = tab;
                        }
                    }
                });
            });
            ui.add_space(4.0);
        }

        egui::ScrollArea::vertical()
            .id_salt((wide, (!wide).then_some(self.home_tab)))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::centered_width(ui, if wide { 1020.0 } else { 540.0 }, |ui| {
                    if show_headline {
                        ui.label(
                            RichText::new("输入联机码，连接远程桌面")
                                .size(27.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                    }
                    if wide {
                        let gap = 16.0;
                        let width = ui.available_width() - gap;
                        let share_width = width * 0.55;
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            ui.allocate_ui_with_layout(
                                Vec2::new(share_width, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| self.device_card(ui),
                            );
                            ui.allocate_ui_with_layout(
                                Vec2::new(width - share_width, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| self.connect_card(ui),
                            );
                        });
                        if !self.hosting.lan.is_empty() {
                            ui.add_space(4.0);
                            self.lan_card(ui);
                        }
                    } else {
                        match self.home_tab {
                            HomeTab::Share => self.device_card(ui),
                            HomeTab::Connect => self.connect_card(ui),
                            HomeTab::Lan => self.lan_card(ui),
                        }
                    }
                    ui.add_space(6.0);
                });
            })
    }

    fn device_card(&mut self, ui: &mut egui::Ui) {
        home_card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("本机共享").size(20.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::secondary(
                        ui,
                        if self.hosting.enabled { "暂停共享" } else { "开始共享" },
                    )
                    .clicked()
                    {
                        if self.hosting.enabled {
                            self.stop_hosting();
                        } else {
                            self.start_hosting();
                        }
                    }
                });
            });
            ui.add(egui::Label::new(RichText::new(&self.device_name).size(14.0).color(theme::muted(ui))).truncate())
                .on_hover_text(&self.device_name);
            ui.add_space(2.0);

            if !self.hosting.enabled {
                if self.hosting.error.is_empty() {
                    ui.label("共享已暂停。点击「开始共享」后即可接收连接。");
                } else {
                    ui.colored_label(theme::DANGER, &self.hosting.error);
                }
                return;
            } else {
                let code = self.hosting.internet_code.clone();
                surface_frame(ui, |ui| {
                    code_header(ui, "互联网联机码", code.clone());
                    code_digits(ui, &code);
                    ui.add_space(2.0);
                    match self.hosting.internet.clone() {
                        Internet::Ready => {
                            status_line(ui, OK, "互联网联机已就绪")
                        }
                        Internet::Connecting => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("正在自动连接会合服务…")
                                        .size(14.0)
                                        .color(WARN),
                                );
                            });
                        }
                        Internet::Failed(reason) => {
                            status_line(ui, theme::DANGER, reason);
                            ui.label(
                                RichText::new(if cfg!(windows) {
                                    "局域网仍可用。跨网连不上时，请到「设置 → 权限」允许防火墙访问 Velarouter。"
                                } else {
                                    "局域网仍可用；若一直失败，可在设置里换成自建会合服务。"
                                })
                                .size(13.5)
                                .color(theme::muted(ui)),
                            );
                        }
                        Internet::Idle => {
                            status_line(ui, theme::muted(ui), "共享未启动");
                        }
                    }
                });
            }

            ui.add_space(3.0);
            theme::section_label(ui, "连接密码");
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(if self.show_password {
                        self.hosting.password.clone()
                    } else {
                        "••••••••••".to_owned()
                    })
                    .size(19.0)
                    .family(egui::FontFamily::Monospace),
                );
                if ui
                    .button(if self.show_password {
                        "隐藏"
                    } else {
                        "显示"
                    })
                    .clicked()
                {
                    self.show_password = !self.show_password;
                }
                if ui.button("复制").clicked() {
                    ui.ctx().copy_text(self.hosting.password.clone());
                }
                if ui.button("换一个").clicked() {
                    self.start_hosting();
                }
            });

            #[cfg(target_os = "macos")]
            if self.hosting.ready && !accessibility_enabled() {
                ui.add_space(6.0);
                status_line(ui, WARN, "当前只能观看：请在「设置 · 权限」中允许键鼠控制");
            }
            #[cfg(target_os = "linux")]
            if self.hosting.enabled && vr_agent::portal_input::is_wayland() {
                ui.add_space(6.0);
                let ready = self
                    .interaction_status
                    .contains("已允许远程键鼠交互");
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        if ready { OK } else { WARN },
                        if ready { "已授权键鼠控制" } else { "Wayland 键鼠未授权" },
                    );
                    if ui.link("查看权限").clicked() {
                        self.open_settings();
                        self.settings_tab = 1;
                    }
                });
            }
            if !self.hosting.error.is_empty() {
                ui.add_space(6.0);
                ui.colored_label(theme::DANGER, &self.hosting.error);
            }
        });
    }

    fn connect_card(&mut self, ui: &mut egui::Ui) {
        home_card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("连接另一台电脑").size(20.0).strong());
            theme::section_label(ui, "对方的联机码");
            let code = theme::input(ui, &mut self.code_input, "12 位数字或局域网码", false);
            theme::section_label(ui, "连接密码");
            let password = theme::input(ui, &mut self.password_input, "由对方提供", true);
            ui.add_space(3.0);
            let pressed = theme::primary(ui, "连接");
            if pressed.clicked()
                || ((code.lost_focus() || password.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.connect();
            }
            if !self.error.is_empty() {
                ui.label(RichText::new(&self.error).size(14.0).color(theme::DANGER));
            }
        });
    }

    fn lan_card(&self, ui: &mut egui::Ui) {
        home_card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section_label(ui, "局域网直连 · 不经过互联网");
            if let Some((first, rest)) = self.hosting.lan.split_first() {
                lan_row(ui, *first);
                if !rest.is_empty() {
                    ui.collapsing(format!("其他网卡（{}）", rest.len()), |ui| {
                        for addr in rest {
                            lan_row(ui, *addr);
                        }
                    });
                }
            } else {
                ui.label(if self.hosting.enabled {
                    "暂无局域网码，请检查网络连接。"
                } else {
                    "请先开始共享。"
                });
            }
        });
    }

    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        theme::centered_width(ui, 780.0, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (index, title) in ["设备", "权限", "网络", "关于"].into_iter().enumerate()
                {
                    if ui
                        .selectable_label(self.settings_tab == index, title)
                        .clicked()
                    {
                        self.settings_tab = index;
                    }
                }
            });
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt(("settings", self.settings_tab))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::centered_width(ui, 780.0, |ui| {
                    theme::card_frame(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match self.settings_tab {
                            0 => self.settings_device(ui),
                            1 => self.settings_permissions(ui),
                            2 => self.settings_network(ui),
                            _ => self.settings_about(ui),
                        }
                    });
                });
            });
    }

    fn settings_about(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| self.brand(ui));
        ui.label(concat!("版本 ", env!("CARGO_PKG_VERSION")));
        ui.label("局域网优先的加密远程桌面");
        ui.hyperlink_to(REPOSITORY, REPOSITORY);
        ui.hyperlink_to("下载与版本说明", format!("{REPOSITORY}/releases"));
    }

    fn settings_device(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("界面主题").size(19.0).strong());
        let previous = self.theme_preference;
        ui.horizontal_wrapped(|ui| {
            for (preference, label) in [
                (egui::ThemePreference::Light, "浅色"),
                (egui::ThemePreference::Dark, "深色"),
                (egui::ThemePreference::System, "跟随系统"),
            ] {
                ui.selectable_value(&mut self.theme_preference, preference, label);
            }
        });
        if previous != self.theme_preference {
            ui.ctx().set_theme(self.theme_preference);
            ui.ctx().request_repaint();
            self.dirty = true;
        }
        ui.add_space(12.0);
        ui.label(RichText::new("设备名称").size(19.0).strong());
        ui.label(
            RichText::new("连接成功后，对方会在会话标题里看到这个名字。")
                .size(14.0)
                .color(theme::muted(ui)),
        );
        ui.add_space(10.0);
        let name = field(ui, "显示名称", &mut self.name_draft, "这台电脑");
        if name.lost_focus() {
            let trimmed = self.name_draft.trim().to_owned();
            if !trimmed.is_empty() && trimmed != self.device_name {
                self.device_name = trimmed;
                self.restart_requested = true;
            }
        }
        ui.add_space(14.0);
        ui.label(
            RichText::new("固定连接密码（长期访问）")
                .size(17.0)
                .strong(),
        );
        ui.label(
            RichText::new(
                "留空则每次启动随机生成 10 位密码。填写后会一直使用这个密码，\
                 适合无人值守/长期连接；至少 6 位，建议 12 位以上。",
            )
            .size(13.0)
            .color(theme::muted(ui)),
        );
        ui.add_space(4.0);
        let mut password_draft = self.fixed_password.clone();
        let response = theme::input(ui, &mut password_draft, "留空 = 每次随机", true);
        if response.changed() {
            self.fixed_password = password_draft;
            self.dirty = true;
        }
        if response.lost_focus() && self.hosting.enabled {
            self.restart_requested = true;
        }
        ui.add_space(14.0);
        ui.label(RichText::new("本机共享目录").size(17.0).strong());
        ui.label(format!("当前共享：{}", self.shared_dir.display()));
        ui.weak("输入已有目录的绝对路径。应用后重启本机共享，对方需重新连接。");
        if super::file_window::folder_input(
            ui,
            &mut self.shared_draft,
            "本机共享目录绝对路径",
            "应用目录",
        ) {
            match browser::local_folder(&self.shared_draft) {
                Ok(dir) => {
                    self.shared_error = None;
                    if dir != self.shared_dir {
                        self.shared_dir = dir;
                        self.dirty = true;
                        self.restart_requested = true;
                    }
                }
                Err(error) => self.shared_error = Some(error),
            }
        }
        if let Some(error) = &self.shared_error {
            ui.colored_label(theme::DANGER, error);
        }
        if ui
            .button("选择对方可访问的目录…")
            .on_hover_text("更改后重启本机共享；对方只能访问此目录及其子目录")
            .clicked()
        {
            if let Some(dir) = rfd::FileDialog::new()
                .set_directory(&self.shared_dir)
                .pick_folder()
            {
                self.shared_draft = dir.display().to_string();
                self.shared_error = None;
                self.shared_dir = dir;
                self.restart_requested = true;
                self.dirty = true;
            }
        }
        self.dirty |= ui
            .checkbox(
                &mut self.auto_close_files,
                "文件全部传输成功后关闭本地传输窗口",
            )
            .changed();
        ui.add_space(12.);
        ui.label(RichText::new("启动与窗口").size(17.0).strong());
        ui.add_space(4.0);
        // Every toggle takes effect and is written to disk immediately: there is
        // no "save" step to forget, which is what people expect from a tray app.
        if ui
            .checkbox(&mut self.autostart, "开机自动启动并开始共享")
            .changed()
        {
            self.startup_changed = true;
            self.dirty = true;
        }
        if ui
            .checkbox(&mut self.start_minimized, "启动后在后台最小化运行")
            .changed()
        {
            self.dirty = true;
        }
        if ui
            .checkbox(
                &mut self.close_to_background,
                "关闭窗口时保持共享（最小化到后台）",
            )
            .changed()
        {
            self.dirty = true;
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("想彻底停止时用下面的「退出 Velarouter」，或右键 Dock 图标退出。")
                .size(13.0)
                .color(theme::muted(ui)),
        );
        ui.add_space(12.0);
        self.save_row(ui);
    }

    fn settings_permissions(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("键鼠与屏幕权限").size(19.0).strong());
        ui.add_space(6.0);
        #[cfg(target_os = "macos")]
        {
            ui.label(
                RichText::new(if accessibility_enabled() {
                    "键鼠控制已授权，对方可以操作这台 Mac。"
                } else {
                    "尚未授权键鼠控制：对方能看画面，但不能点击或输入。"
                })
                .size(15.0),
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for (label, pane) in [
                    ("打开辅助功能设置", "Accessibility"),
                    ("打开屏幕录制设置", "ScreenCapture"),
                ] {
                    if ui.button(label).clicked() {
                        let _ = std::process::Command::new("open")
                            .arg(format!(
                                "x-apple.systempreferences:com.apple.preference.security?Privacy_{pane}"
                            ))
                            .spawn();
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "授权后请完全退出并重新打开 Velarouter；第一次授权前建立的会话不会自动获得权限。\
                     更新安装包后如果又变成「仅观看」，请在辅助功能列表里移除 Velarouter 再重新添加当前 App。",
                )
                .size(13.5)
                .color(theme::muted(ui)),
            );
        }
        #[cfg(target_os = "linux")]
        {
            if vr_agent::portal_input::is_wayland() {
                ui.label("Wayland：请选择与远端画面一致的屏幕，打开系统弹窗的「允许远程交互」，再点「分享」。取消或不打开开关，只能观看。");
                ui.label(&self.interaction_status);
                if ui.button("重新申请权限（重启本机共享）").clicked() {
                    self.start_hosting();
                }
            } else {
                ui.label("X11：请在已登录的图形会话中启动，无需 Wayland 授权弹窗。不需要以 root 身份运行。");
            }
        }
        #[cfg(windows)]
        {
            ui.label(
                RichText::new(
                    "可控制普通窗口。系统登录界面、UAC 安全桌面和部分受保护应用不会接受远程输入。",
                )
                .size(15.0),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("若目标程序以管理员身份运行，请同样以管理员身份启动 Velarouter。")
                    .size(13.5)
                    .color(theme::muted(ui)),
            );
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(10.0);
            ui.label(RichText::new("网络与防火墙").size(19.0).strong());
            ui.label(
                RichText::new(
                    "Windows 防火墙默认拦截入站 UDP，P2P 打洞就会失败（表现为「对方没有响应」）。\
                     下面这条规则只针对当前的 Velarouter.exe，需要你在 UAC 提示里点一次「是」。",
                )
                .size(14.0)
                .color(theme::muted(ui)),
            );
            ui.add_space(8.0);
            if ui.button("允许防火墙访问 Velarouter").clicked() {
                if let Err(e) = crate::startup::allow_firewall() {
                    tracing::warn!(%e, "cannot open the firewall rule prompt");
                }
            }
        }
    }

    fn settings_network(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("网络").size(19.0).strong());
        ui.add_space(6.0);
        let internet = self
            .probe_result
            .map(|(ok, _)| {
                if ok {
                    "可访问互联网"
                } else {
                    "无法访问互联网"
                }
            })
            .unwrap_or("正在检测互联网…");
        let lan_value = if self.hosting.lan.is_empty() {
            "没有可用网卡".to_string()
        } else {
            format!("{} 个地址", self.hosting.lan.len())
        };
        for (label, value, color) in [
            (
                "互联网",
                internet.to_string(),
                if self.probe_result.map(|(ok, _)| ok) == Some(true) {
                    OK
                } else {
                    theme::muted(ui)
                },
            ),
            ("局域网", lan_value, theme::muted(ui)),
            (
                "会合通道",
                match &self.hosting.internet {
                    Internet::Ready => "在线",
                    Internet::Connecting => "连接中",
                    Internet::Failed(_) => "离线",
                    Internet::Idle => "未启动",
                }
                .to_string(),
                match &self.hosting.internet {
                    Internet::Ready => OK,
                    Internet::Connecting => WARN,
                    Internet::Failed(_) => theme::DANGER,
                    Internet::Idle => theme::muted(ui),
                },
            ),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(14.0).color(theme::muted(ui)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(value).size(14.5).color(color));
                });
            });
        }
        ui.add_space(12.0);
        ui.label(RichText::new("会合服务（可选）").size(15.0).strong());
        ui.label(
            RichText::new(
                "留空即使用内置公共会合：只转发加密的地址信息，不传画面、不存密码。\
                 自建时填 MQTT over TLS 地址，每行一个。",
            )
            .size(13.5)
            .color(theme::muted(ui)),
        );
        ui.add_space(6.0);
        let mut draft = self.broker_draft.clone();
        let response = egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(egui::Stroke::new(1.0, theme::border(ui)))
            .corner_radius(11)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut draft)
                        .frame(egui::Frame::NONE)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("wss://broker.example.com:8084/mqtt")
                        .font(egui::TextStyle::Body),
                )
            })
            .inner;
        if response.changed() {
            self.broker_draft = draft;
        }
        if response.lost_focus() {
            let brokers: Vec<String> = self
                .broker_draft
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            if brokers != self.brokers {
                self.brokers = brokers;
                self.restart_requested = true;
                self.dirty = true;
            }
        }
        ui.add_space(14.0);
        ui.label(
            RichText::new("自建信令 / 中继服务器（可选）")
                .size(15.0)
                .strong(),
        );
        ui.label(
            RichText::new(
                "需要自己管理联机码时，在服务器的项目目录运行 \
                 bash scripts/deploy-signal.sh 你的域名，把 HTTPS 地址填在这里。两端改用它登记（9 位编码），\
                 当前桌面连接仍需 UDP 打洞成功。",
            )
            .size(13.0)
            .color(theme::muted(ui)),
        );
        ui.add_space(4.0);
        let mut signal = self.signal_draft.clone();
        let response = theme::input(
            ui,
            &mut signal,
            "https://signal.example.com（可留空）",
            false,
        );
        if response.changed() {
            self.signal_draft = signal;
        }
        if response.lost_focus() {
            let trimmed = self.signal_draft.trim().to_owned();
            let next = (!trimmed.is_empty()).then_some(trimmed);
            if next != self.signal_url {
                self.signal_url = next;
                self.restart_requested = true;
                self.dirty = true;
            }
        }
        ui.add_space(12.0);
        self.save_row(ui);
    }

    fn save_row(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("立即应用").size(15.0).color(Color32::WHITE))
                        .fill(ACCENT)
                        .corner_radius(10),
                )
                .clicked()
            {
                let trimmed = self.name_draft.trim().to_owned();
                if !trimmed.is_empty() {
                    self.device_name = trimmed;
                }
                self.brokers = self
                    .broker_draft
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                self.restart_requested = true;
                self.dirty = true;
                self.startup_changed = true;
            }
            if ui.button("返回").clicked() {
                self.show_settings = false;
            }
            if ui
                .add(
                    egui::Button::new(RichText::new("退出 Velarouter").size(14.0))
                        .fill(Color32::TRANSPARENT)
                        .stroke(egui::Stroke::new(1.0, theme::DANGER)),
                )
                .clicked()
            {
                self.quitting = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.add_space(2.0);
        ui.label(
            RichText::new("修改会立即保存；名称与网络设置会在应用后重启共享。")
                .size(12.5)
                .color(theme::muted(ui)),
        );
    }
}

fn lan_row(ui: &mut egui::Ui, addr: SocketAddrV4) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(addr.ip().to_string())
                .size(13.5)
                .color(theme::muted(ui)),
        );
        ui.label(
            RichText::new(vr_net::direct::encode(addr))
                .size(15.0)
                .family(egui::FontFamily::Monospace),
        );
        if ui.button("复制").clicked() {
            ui.ctx().copy_text(vr_net::direct::encode(addr));
        }
    });
}

fn home_card(ui: &egui::Ui) -> egui::Frame {
    theme::card_frame(ui).inner_margin(Margin::symmetric(14, 12))
}

fn surface_frame(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(surface(ui))
        .corner_radius(14)
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_controls_fit_without_scrolling_at_common_window_sizes() {
        let ctx = egui::Context::default();
        let config = Config {
            device_name:
                "A long workstation name that should not push other controls outside the card"
                    .into(),
            ..Default::default()
        };
        let mut app = App::with_config(&ctx, &config, tokio::runtime::Runtime::new().unwrap());
        app.hosting.enabled = true;
        app.hosting.ready = true;
        app.hosting.internet = Internet::Ready;
        app.hosting.internet_code = "352001977805".into();
        app.hosting.lan = vec![
            "192.168.1.3:34789".parse().unwrap(),
            "10.0.0.5:34789".parse().unwrap(),
        ];

        for (width, height, tab) in [
            (1100.0, 790.0, HomeTab::Share),
            (800.0, 600.0, HomeTab::Share),
            (430.0, 510.0, HomeTab::Share),
            (360.0, 480.0, HomeTab::Connect),
            (360.0, 420.0, HomeTab::Lan),
        ] {
            app.home_tab = tab;
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, height))),
                    ..Default::default()
                }, |root| {
                    // Reserve the same space as the brand bar and panel margins.
                    root.add_space(64.0);
                    egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin::symmetric(18, 4))).show(root, |ui| {
                        let layout = app.home_content(ui);
                        assert!(layout.content_size.x <= layout.inner_rect.width() + 1.0,
                            "{width}x{height} {tab:?} overflowed horizontally: {:?} vs {:?}", layout.content_size, layout.inner_rect.size());
                        assert!(layout.content_size.y <= layout.inner_rect.height() + 1.0,
                            "{width}x{height} {tab:?} overflowed vertically: {:?} vs {:?}", layout.content_size, layout.inner_rect.size());
                    });
                });
                output.textures_delta.clear();
            }
        }
    }
}
