//! Viewer-side composition. The remote host only receives committed text.
use egui::{Color32, FontId, ImeEvent, Pos2, Rect, Ui, Vec2};

#[derive(Default)]
pub(super) struct Composition {
    marked: String,
    // Normalized to the video, so resizing does not move the hint off the image.
    insertion: Option<Vec2>,
}

impl Composition {
    pub fn place(&mut self, image: Rect, click: Pos2) {
        self.insertion = Some((click - image.min) / image.size());
    }

    pub fn composing(&self) -> bool {
        !self.marked.is_empty()
    }

    pub fn event(&mut self, event: ImeEvent) -> Option<String> {
        match event {
            ImeEvent::DeleteSurrounding { .. } => None,
            ImeEvent::Preedit { text: s, .. } => {
                self.marked = s;
                None
            }
            ImeEvent::Commit(s) => {
                self.marked.clear();
                (!s.is_empty()).then_some(s)
            }
            _ => {
                self.marked.clear();
                None
            }
        }
    }

    pub fn present(&self, ui: &Ui, image: Rect) {
        let point = image.min + self.insertion.unwrap_or(Vec2::splat(0.5)) * image.size();
        let painter = ui.painter_at(image);
        let galley = self.composing().then(|| {
            painter.layout_no_wrap(
                self.marked.clone(),
                FontId::proportional(16.),
                Color32::WHITE,
            )
        });
        let size = galley
            .as_ref()
            .map_or(Vec2::new(2., 20.), |g| g.size() + Vec2::new(16., 8.))
            .min(image.size());
        let pos = (point + Vec2::new(2., 4.))
            .min(image.max - size)
            .max(image.min);
        let rect = Rect::from_min_size(pos, size);
        // Keep both composition and caret bounds local to the insertion point.
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                should_interrupt_composition: false,
                rect,
                cursor_rect: Rect::from_min_max(
                    Pos2::new((rect.right() - 1.).max(rect.left()), rect.top()),
                    rect.max,
                ),
            })
        });
        if let Some(galley) = galley {
            painter.rect_filled(rect, 5., Color32::from_rgb(35, 39, 52));
            painter
                .with_clip_rect(rect)
                .galley(pos + Vec2::new(8., 4.), galley, Color32::WHITE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_candidate_anchor_stays_inside_resized_video() {
        let ctx = egui::Context::default();
        let initial = Rect::from_min_size(Pos2::new(12., 18.), Vec2::new(900., 600.));
        let mut c = Composition::default();
        c.place(initial, initial.max);
        c.event(ImeEvent::Preedit {
            text: "nihao".into(),
            active_range_chars: None,
        });
        let image = Rect::from_min_size(Pos2::new(40., 70.), Vec2::new(320., 200.));
        let mut output = ctx.run_ui(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| c.present(ui, image));
        });
        output.textures_delta.clear(); // This headless test has no renderer.
        let ime = output.platform_output.ime.unwrap();
        assert!(image.contains_rect(ime.rect));
        assert!(ime.rect.width() < image.width() && ime.rect.bottom() == image.bottom());
        assert!(ime.cursor_rect.width() <= 1.);
    }

    #[test]
    fn preedit_stays_local_until_commit_or_cancel() {
        let mut c = Composition::default();
        assert_eq!(
            c.event(ImeEvent::Preedit {
                text: "ni".into(),
                active_range_chars: None
            }),
            None
        );
        assert!(c.composing());
        assert_eq!(c.event(ImeEvent::Commit("你".into())), Some("你".into()));
        assert!(!c.composing());
        c.event(ImeEvent::Preedit {
            text: "hao".into(),
            active_range_chars: None,
        });
        assert_eq!(
            c.event(ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None
            }),
            None
        );
        assert!(!c.composing());
        assert_eq!(c.event(ImeEvent::Commit(String::new())), None);
    }
}
