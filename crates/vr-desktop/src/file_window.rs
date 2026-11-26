//! The transfer window belongs to the viewer, never to the captured desktop.
use super::*;
use std::path::PathBuf;

/// Enter and the button perform the same explicit navigation; merely editing a path
/// must not silently redirect a queued transfer.
pub(super) fn folder_input(ui: &mut egui::Ui, text: &mut String, hint: &str, action: &str) -> bool {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 80.).max(40.);
        let input = ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width(width),
        );
        let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.button(action).clicked() || enter
    })
    .inner
}

const TWO_PANES: f32 = 720.;

fn pane_card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    theme::card_frame(ui)
        .inner_margin(Margin::same(12))
        .corner_radius(12)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing = Vec2::new(6., 4.);
            ui.spacing_mut().button_padding = Vec2::new(6., 2.);
            ui.spacing_mut().interact_size.y = ROW_HEIGHT;
            contents(ui);
        });
}

fn location(ui: &mut egui::Ui, kind: &str, path: &str) {
    ui.horizontal(|ui| {
        theme::pill(ui, kind, theme::accent(ui));
        ui.add(egui::Label::new(RichText::new(path).strong()).truncate())
            .on_hover_text(path);
    });
    ui.add_space(4.);
}

/// Fixed-height rows are shared by both panes so large directories can render
/// just the visible slice without scrolling jumps.
fn file_row(ui: &mut egui::Ui, name: &str, is_dir: bool, selected: bool) -> egui::Response {
    let text = format!("{}  {}", if is_dir { "▸" } else { "·" }, name);
    let color = if is_dir {
        theme::accent(ui)
    } else {
        ui.visuals().text_color()
    };
    ui.add_sized(
        [ui.available_width(), ROW_HEIGHT],
        egui::Button::new(RichText::new(text).color(color))
            .selected(selected)
            .frame(selected)
            .truncate()
            .sense(Sense::click_and_drag()),
    )
}
const ROW_HEIGHT: f32 = 26.;

impl App {
    pub(super) fn open_files(&mut self) {
        if self.show_files {
            self.ctx.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of("file-manager"),
                egui::ViewportCommand::Focus,
            );
            return;
        }
        self.show_files = true;
        self.local_draft = self.local_dir.display().to_string();
        self.refresh_local();
        self.list_remote(self.remote_path.clone());
    }

    pub(super) fn list_remote(&mut self, path: String) {
        if self.list_pending {
            return;
        }
        self.remote_draft.clone_from(&path);
        self.list_pending = true;
        self.list_error.clear();
        self.remote_selected = None;
        if self.cmd_tx.send(Cmd::ListFiles { path }).is_err() {
            self.list_pending = false;
            self.list_error = "会话已断开，请重新连接".into();
        }
    }

    pub(super) fn refresh_local(&mut self) {
        match browser::list(&self.local_dir) {
            Ok(entries) => {
                self.local_entries = entries;
                self.local_error = None;
            }
            Err(e) => {
                self.local_entries.clear();
                self.local_selected = None;
                self.local_error = Some(format!("无法读取本地目录：{e}"));
            }
        }
    }

    fn set_local(&mut self, dir: PathBuf) {
        self.local_draft = dir.display().to_string();
        self.local_dir = dir;
        self.refresh_local();
        self.local_selected = None;
        self.dirty = true;
    }

    pub(super) fn collect_drops(&mut self, ctx: &egui::Context) {
        let files = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        if files.is_empty() {
            return;
        }
        if !self.show_files {
            self.open_files();
        }
        self.compact_remote = true;
        self.dropped
            .extend(files.into_iter().map(|f| f.path().to_path_buf()));
    }

    pub(super) fn enqueue_file(&mut self, job: Cmd) {
        if !self.file_active {
            self.batch_errors.clear();
        }
        self.jobs.push_back(job);
        if !self.file_active {
            self.next_file();
        }
    }

    pub(super) fn next_file(&mut self) {
        self.file_active = false;
        if let Some(job) = self.jobs.pop_front() {
            self.file_active = self.cmd_tx.send(job).is_ok();
        }
    }

    fn remote_ready(&self) -> bool {
        !self.list_pending && self.list_error.is_empty() && self.remote_draft == self.remote_path
    }

    fn local_ready(&self) -> bool {
        self.local_error.is_none() && self.local_draft == self.local_dir.to_string_lossy()
    }

    fn upload(&mut self, local: PathBuf) {
        self.enqueue_file(Cmd::Upload {
            local,
            remote_dir: self.remote_path.clone(),
        });
    }

    fn download(&mut self, path: String) {
        self.enqueue_file(Cmd::Download {
            path,
            save_dir: Some(self.local_dir.clone()),
        });
    }

    pub(super) fn files_window(&mut self, ctx: &egui::Context) {
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("file-manager"),
            egui::ViewportBuilder::default()
                .with_app_id("Velarouter")
                .with_title(format!("文件传输 · {}", self.peer))
                .with_inner_size([820., 540.])
                .with_min_inner_size([320., 350.])
                .with_drag_and_drop(true),
            |root, _| {
                let ctx = &root.ctx().clone();
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.show_files = false;
                    return;
                }
                self.collect_drops(ctx);
                egui::Panel::bottom("transfer-status")
                    .frame(
                        egui::Frame::central_panel(root.style())
                            .inner_margin(Margin::symmetric(12, 8)),
                    )
                    .show(root, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6., 4.);
                        ui.spacing_mut().button_padding = Vec2::new(8., 3.);
                        ui.spacing_mut().interact_size.y = ROW_HEIGHT;
                        egui::ScrollArea::vertical()
                            .id_salt("file-status-scroll")
                            .max_height(ctx.content_rect().height() * 0.31)
                            .show(ui, |ui| self.file_status_bar(ui));
                    });
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(theme::page(ctx))
                            .inner_margin(Margin::same(10)),
                    )
                    .show(root, |ui| {
                        let compact = ui.available_width() < TWO_PANES;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("文件互传").size(20.).strong());
                            if compact {
                                ui.selectable_value(&mut self.compact_remote, false, "本地");
                                ui.selectable_value(&mut self.compact_remote, true, "远端");
                            } else {
                                ui.weak("选择两侧目录，然后拖动文件或点击传输");
                            }
                        });
                        ui.add_space(6.);
                        if compact {
                            let destination = if self.compact_remote {
                                format!("下载到：{}", self.local_dir.display())
                            } else {
                                format!("上传到：{}", self.remote_location())
                            };
                            ui.add(egui::Label::new(&destination).truncate())
                                .on_hover_text(&destination);
                            pane_card(ui, |ui| {
                                if self.compact_remote {
                                    self.remote_pane(ui)
                                } else {
                                    self.local_pane(ui)
                                }
                            });
                        } else {
                            ui.columns(2, |cols| {
                                pane_card(&mut cols[0], |ui| self.local_pane(ui));
                                pane_card(&mut cols[1], |ui| self.remote_pane(ui));
                            });
                        }
                    });
            },
        );
    }

    fn file_status_bar(&mut self, ui: &mut egui::Ui) {
        if !self.dropped.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(format!(
                        "{} 项 → {}",
                        self.dropped.len(),
                        self.remote_location()
                    ))
                    .truncate(),
                )
                .on_hover_text(self.remote_location());
                if ui
                    .add_enabled(self.remote_ready(), egui::Button::new("确认上传"))
                    .clicked()
                {
                    for path in std::mem::take(&mut self.dropped) {
                        self.upload(path);
                    }
                }
                if ui.button("取消").clicked() {
                    self.dropped.clear();
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            self.dirty |= ui
                .checkbox(&mut self.auto_close_files, "全部传输成功后关闭窗口")
                .changed();
            if self.file_active {
                ui.spinner();
                ui.label(format!("待传 {} 项", self.jobs.len()));
            }
            if ui
                .button("关闭窗口")
                .on_hover_text("不影响会话和进行中的传输")
                .clicked()
            {
                self.show_files = false;
            }
        });
        self.transfer_status(ui);
        if let Some(error) = self.batch_errors.last() {
            let summary = format!("{} 项失败 · {error}", self.batch_errors.len());
            ui.add(egui::Label::new(RichText::new(&summary).color(theme::DANGER)).truncate())
                .on_hover_text(summary);
        }
    }

    fn local_pane(&mut self, ui: &mut egui::Ui) {
        location(ui, "本地", &self.local_dir.display().to_string());
        if folder_input(ui, &mut self.local_draft, "本地绝对路径", "前往") {
            match browser::local_folder(&self.local_draft) {
                Ok(dir) => self.set_local(dir),
                Err(error) => self.local_error = Some(error),
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("选择目录…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_directory(&self.local_dir)
                    .pick_folder()
                {
                    self.set_local(dir);
                }
            }
            if ui.button("上一级").clicked() {
                if let Some(dir) = browser::parent(&self.local_dir) {
                    self.set_local(dir);
                }
            }
            if ui.button("刷新").clicked() {
                self.set_local(self.local_dir.clone());
            }
        });
        let ready = self.remote_ready() && self.local_ready();
        if ui
            .add_enabled(
                ready && self.local_selected.is_some(),
                egui::Button::new(RichText::new("上传到远端 →").color(Color32::WHITE))
                    .fill(theme::ACCENT),
            )
            .on_hover_text(self.remote_location())
            .clicked()
        {
            if let Some(path) = self.local_selected.clone() {
                self.upload(path);
            }
        }
        ui.separator();
        if let Some(message) = &self.local_error {
            ui.colored_label(theme::DANGER, message);
            return;
        }
        if self.local_entries.is_empty() {
            ui.weak("没有可显示的文件");
        }
        let mut enter = None;
        let mut upload = None;
        egui::ScrollArea::vertical()
            .id_salt("local-files")
            .show_rows(ui, ROW_HEIGHT, self.local_entries.len(), |ui, rows| {
                for entry in &self.local_entries[rows] {
                    let r = file_row(
                        ui,
                        &entry.name,
                        entry.is_dir,
                        self.local_selected.as_ref() == Some(&entry.path),
                    )
                    .on_hover_text(&entry.name);
                    if entry.is_dir {
                        if r.clicked() {
                            enter = Some(entry.path.clone());
                        }
                    } else {
                        r.dnd_set_drag_payload(entry.path.clone());
                        if ready && r.double_clicked() {
                            upload = Some(entry.path.clone());
                        }
                        if r.clicked() {
                            self.local_selected = Some(entry.path.clone());
                        }
                        r.on_hover_text(format!(
                            "{} · 拖到右侧上传",
                            files::human_size(entry.size)
                        ));
                    }
                }
            });
        if let Some(path) = upload {
            self.upload(path);
        }
        if let Some(dir) = enter {
            self.set_local(dir);
        }
    }

    fn remote_location(&self) -> String {
        let root = self.remote_root.as_deref().unwrap_or("远端共享根目录");
        if self.remote_path.is_empty() {
            root.to_owned()
        } else {
            format!(
                "{}/{}",
                root.trim_end_matches(['/', '\\']),
                self.remote_path
            )
        }
    }

    fn remote_pane(&mut self, ui: &mut egui::Ui) {
        location(ui, "远端", &self.remote_location());
        ui.add_enabled_ui(!self.list_pending, |ui| {
            if folder_input(
                ui,
                &mut self.remote_draft,
                "共享根内相对路径（留空为根）",
                "前往",
            ) {
                match browser::remote_folder(&self.remote_draft) {
                    Ok(path) => self.list_remote(path),
                    Err(error) => self.list_error = error,
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("共享根目录").clicked() {
                self.list_remote(String::new());
            }
            if ui.button("上一级").clicked() {
                self.list_remote(
                    self.remote_path
                        .rsplit_once('/')
                        .map_or("", |v| v.0)
                        .to_owned(),
                );
            }
            if ui.button("刷新").clicked() {
                self.list_remote(self.remote_path.clone());
            }
        });
        if ui
            .add_enabled(
                self.remote_selected.is_some() && self.remote_ready() && self.local_ready(),
                egui::Button::new(RichText::new("← 下载到本地").color(Color32::WHITE))
                    .fill(theme::ACCENT),
            )
            .clicked()
        {
            if let Some(path) = self.remote_selected.clone() {
                self.download(path);
            }
        }
        ui.separator();
        if self.list_pending {
            ui.spinner();
            return;
        }
        if !self.list_error.is_empty() {
            ui.colored_label(theme::DANGER, &self.list_error);
            return;
        }
        let mut enter = None;
        let mut download = None;
        let (_, dropped) = ui.dnd_drop_zone::<PathBuf, _>(egui::Frame::group(ui.style()), |ui| {
            ui.set_min_size(ui.available_size());
            ui.weak(if self.files.is_empty() {
                "此目录为空，可拖入文件"
            } else {
                "点击目录进入 · 拖到这里上传"
            });
            egui::ScrollArea::vertical()
                .id_salt("remote-files")
                .show_rows(ui, ROW_HEIGHT, self.files.len(), |ui, rows| {
                    for entry in &self.files[rows] {
                        let r = file_row(
                            ui,
                            &entry.name,
                            entry.is_dir,
                            self.remote_selected.as_ref() == Some(&entry.path),
                        )
                        .on_hover_text(&entry.name);
                        if entry.is_dir && r.clicked() {
                            enter = Some(entry.path.clone());
                        } else if !entry.is_dir
                            && r.double_clicked()
                            && self.local_ready()
                            && self.remote_ready()
                        {
                            download = Some(entry.path.clone());
                        }
                        if r.clicked() {
                            self.remote_selected = (!entry.is_dir).then(|| entry.path.clone());
                        }
                    }
                });
        });
        if let Some(path) = download {
            self.download(path);
        }
        if let Some(path) = dropped.filter(|_| self.remote_ready()) {
            self.upload((*path).clone());
        }
        if let Some(path) = enter {
            self.list_remote(path);
        }
    }
}
