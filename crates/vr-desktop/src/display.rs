//! Local presentation geometry is independent of the agent's capture display ID.
use super::*;

pub(super) struct LocalScreen {
    name: String,
    origin: Pos2,
    size: Vec2,
}

impl LocalScreen {
    pub fn all() -> Vec<Self> {
        xcap::Monitor::all()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| {
                Some(Self {
                    name: m.friendly_name().or_else(|_| m.name()).ok()?,
                    origin: Pos2::new(m.x().ok()? as f32, m.y().ok()? as f32),
                    size: Vec2::new(m.width().ok()? as f32, m.height().ok()? as f32),
                })
            })
            .collect()
    }
}

/// egui's viewport geometry is in points. Preserve aspect ratio even when a
/// HiDPI/portrait monitor exceeds the capture service's 4K encoding box.
fn stream_box(points: Vec2, ppp: f32) -> (u16, u16) {
    let pixels = (points * ppp).max(Vec2::splat(1.));
    let fit = (3840. / pixels.x).min(2160. / pixels.y).min(1.);
    (
        (pixels.x * fit).round().max(320.) as u16,
        (pixels.y * fit).round().max(240.) as u16,
    )
}

impl App {
    pub(super) fn adaptive_target(
        ctx: &egui::Context,
        resolution: Resolution,
        canvas: Vec2,
    ) -> (u16, u16) {
        let points = if resolution == Resolution::Local {
            ctx.input(|i| i.viewport().monitor_size).unwrap_or(canvas)
        } else {
            canvas
        };
        stream_box(points, ctx.pixels_per_point())
    }

    pub(super) fn target_size(&self) -> (u16, u16) {
        match self.resolution {
            Resolution::Local | Resolution::Adaptive => self.adaptive_size,
            preset => preset.size(),
        }
    }

    pub(super) fn apply_video(&mut self) {
        let (max_w, max_h) = self.target_size();
        self.last_size_sent = (max_w, max_h);
        self.last_size_at = std::time::Instant::now();
        let _ = self.cmd_tx.send(Cmd::Video {
            quality: self.quality,
            fps: self.fps,
            display: self.display,
            max_w,
            max_h,
        });
    }

    pub(super) fn sync_adaptive_size(&mut self, ctx: &egui::Context, canvas: Vec2) {
        self.adaptive_size = Self::adaptive_target(ctx, self.resolution, canvas);
        if self.target_size() != self.last_size_sent {
            let delay = Duration::from_millis(350).saturating_sub(self.last_size_at.elapsed());
            if delay.is_zero() {
                self.apply_video();
            } else {
                ctx.request_repaint_after(delay);
            }
        }
    }

    pub(super) fn display_controls(&mut self, ui: &mut egui::Ui, canvas: Vec2) {
        let ctx = ui.ctx().clone();
        let screen = ctx
            .input(|i| i.viewport().monitor_size)
            .map(|s| s * ctx.pixels_per_point());
        ui.menu_button("显示设置 ▾", |ui| {
            ui.strong("本机显示器");
            if let Some(size) = screen {
                ui.weak(format!("当前屏幕  {:.0} × {:.0} px", size.x, size.y));
            }
            // macOS exposes global display bounds in logical points; on Windows
            // (per-monitor DPI aware) and X11 xcap returns desktop pixels.
            let factor = if cfg!(target_os = "macos") {
                ctx.zoom_factor()
            } else {
                ctx.pixels_per_point()
            };
            let center = ctx.input(|i| i.viewport().outer_rect.map(|r| r.center() * factor));
            for (index, s) in self.local_screens.iter().enumerate() {
                if ui
                    .selectable_label(
                        center.is_some_and(|p| Rect::from_min_size(s.origin, s.size).contains(p)),
                        format!("{} · {}", index + 1, s.name),
                    )
                    .on_hover_text("将会话窗口移到这块本机显示器")
                    .clicked()
                {
                    let pos = s.origin + Vec2::new(s.size.x * 0.04, s.size.y * 0.06);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new(
                        pos.x / factor,
                        pos.y / factor,
                    )));
                    ui.close();
                }
            }
            if ui.small_button("刷新本机显示器").clicked() {
                self.local_screens = LocalScreen::all();
            }
            ui.separator();
            ui.strong("画面分辨率");
            for r in [
                Resolution::Local,
                Resolution::Adaptive,
                Resolution::P1080,
                Resolution::P1440,
                Resolution::P2160,
            ] {
                if ui
                    .selectable_label(self.resolution == r, r.label())
                    .clicked()
                {
                    self.resolution = r;
                    self.adaptive_size = Self::adaptive_target(&ctx, r, canvas);
                    self.apply_video();
                    self.dirty = true;
                    ui.close();
                }
            }
            ui.separator();
            ui.strong("远端采集屏幕");
            let mut select = None;
            for d in &self.displays {
                if ui
                    .selectable_label(
                        d.id == self.display,
                        format!("{} · {}×{}", d.name, d.width, d.height),
                    )
                    .clicked()
                {
                    select = Some(d.id);
                }
            }
            if let Some(id) = select {
                self.display = id;
                self.apply_video();
                ui.close();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_size_uses_physical_pixels_and_fits_capture_limits() {
        assert_eq!(stream_box(Vec2::new(1512., 982.), 2.), (3024, 1964));
        assert_eq!(stream_box(Vec2::new(1440., 2560.), 1.), (1215, 2160));
    }
}
