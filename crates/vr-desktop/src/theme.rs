//! One palette, one spacing scale, light and dark.
//!
//! The rules the UI follows everywhere:
//!
//! * one accent colour, used for the primary action and for "ready" states;
//! * content sits on white (light) or near-black (dark) cards over a tinted page;
//! * spacing is a 4 px scale, so nothing is "almost" aligned;
//! * type sizes are fixed, so the layout does not depend on the OS defaults.

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle};

pub const ACCENT: Color32 = Color32::from_rgb(79, 96, 214);
pub const OK: Color32 = Color32::from_rgb(21, 158, 116);
pub const WARN: Color32 = Color32::from_rgb(206, 132, 30);
pub const DANGER: Color32 = Color32::from_rgb(207, 73, 73);

/// Page background.
pub fn page(ctx: &egui::Context) -> Color32 {
    page_of(ctx.global_style().visuals.dark_mode)
}

pub fn page_of(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(15, 17, 22)
    } else {
        Color32::from_rgb(243, 245, 250)
    }
}

/// Card background.
pub fn card(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(24, 27, 34)
    } else {
        Color32::WHITE
    }
}

/// A slightly recessed block inside a card (code digits, hints).
pub fn inset(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(31, 35, 44)
    } else {
        Color32::from_rgb(246, 247, 252)
    }
}

pub fn border(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(44, 49, 60)
    } else {
        Color32::from_rgb(226, 229, 238)
    }
}

pub fn muted(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(148, 156, 172)
    } else {
        Color32::from_rgb(108, 115, 131)
    }
}

pub fn accent(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(150, 163, 255)
    } else {
        ACCENT
    }
}

#[cfg(target_os = "linux")]
pub fn watch_linux(
    rt: &tokio::runtime::Runtime,
    ctx: egui::Context,
) -> tokio::sync::watch::Receiver<Option<egui::Theme>> {
    use ashpd::desktop::settings::{ColorScheme, Settings};
    use futures_util::StreamExt;
    let (tx, rx) = tokio::sync::watch::channel(None);
    rt.spawn(async move {
        let Ok(settings) = Settings::new().await else {
            return;
        };
        let apply = |scheme| {
            tx.send_replace(match scheme {
                ColorScheme::PreferDark => Some(egui::Theme::Dark),
                ColorScheme::PreferLight => Some(egui::Theme::Light),
                _ => None,
            });
            ctx.request_repaint();
        };
        let changes = settings.receive_color_scheme_changed().await;
        if let Ok(scheme) = settings.color_scheme().await {
            apply(scheme);
        }
        if let Ok(mut changes) = changes {
            while let Some(scheme) = changes.next().await {
                apply(scheme);
            }
        }
    });
    rx
}

pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let dark = style.visuals.dark_mode;
        let mut v = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = Some(if dark {
            Color32::from_rgb(230, 233, 240)
        } else {
            Color32::from_rgb(38, 42, 54)
        });
        v.panel_fill = if dark {
            Color32::from_rgb(15, 17, 22)
        } else {
            Color32::from_rgb(243, 245, 250)
        };
        v.window_fill = if dark {
            Color32::from_rgb(24, 27, 34)
        } else {
            Color32::WHITE
        };
        v.extreme_bg_color = if dark {
            Color32::from_rgb(18, 21, 27)
        } else {
            Color32::WHITE
        };
        v.faint_bg_color = if dark {
            Color32::from_rgb(28, 31, 39)
        } else {
            Color32::from_rgb(248, 249, 252)
        };
        v.hyperlink_color = if dark {
            Color32::from_rgb(150, 163, 255)
        } else {
            ACCENT
        };
        v.selection.bg_fill = if dark {
            Color32::from_rgb(58, 70, 140)
        } else {
            Color32::from_rgb(214, 220, 255)
        };
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.window_corner_radius = 16.into();
        v.menu_corner_radius = 12.into();
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 10],
            blur: 30,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 90 } else { 26 }),
        };
        v.popup_shadow = v.window_shadow;
        v.widgets.noninteractive.bg_fill = v.panel_fill;
        v.widgets.noninteractive.weak_bg_fill = v.faint_bg_color;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border_of(dark));
        v.widgets.noninteractive.corner_radius = 10.into();
        for widget in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            widget.bg_stroke = Stroke::new(1.0, border_of(dark));
            widget.corner_radius = 10.into();
        }
        v.widgets.inactive.weak_bg_fill = if dark {
            Color32::from_rgb(32, 36, 45)
        } else {
            Color32::WHITE
        };
        v.widgets.inactive.bg_fill = v.widgets.inactive.weak_bg_fill;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, v.override_text_color.unwrap());
        v.widgets.hovered.weak_bg_fill = if dark {
            Color32::from_rgb(40, 45, 57)
        } else {
            Color32::from_rgb(246, 248, 255)
        };
        v.widgets.hovered.bg_fill = v.widgets.hovered.weak_bg_fill;
        v.widgets.hovered.bg_stroke = Stroke::new(
            1.0,
            if dark {
                Color32::from_rgb(72, 80, 100)
            } else {
                Color32::from_rgb(198, 206, 226)
            },
        );
        v.widgets.active.weak_bg_fill = if dark {
            Color32::from_rgb(46, 52, 66)
        } else {
            Color32::from_rgb(236, 240, 254)
        };
        v.widgets.active.bg_fill = v.widgets.active.weak_bg_fill;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals = v;

        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(16.0, 10.0);
        style.spacing.interact_size.y = 34.0;
        style.spacing.menu_margin = 8.into();
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(13.0)),
            (TextStyle::Body, FontId::proportional(16.0)),
            (TextStyle::Button, FontId::proportional(16.0)),
            (TextStyle::Heading, FontId::proportional(26.0)),
            (TextStyle::Monospace, FontId::monospace(16.0)),
        ]
        .into();
        style.visuals.text_cursor.stroke = Stroke::new(1.6, ACCENT);
    });
    install_fonts(ctx);
}

fn border_of(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(44, 49, 60)
    } else {
        Color32::from_rgb(226, 229, 238)
    }
}

/// macOS ships PingFang as an on-demand font asset, not at the old static
/// path. Its layout is `<FontN>/<hash>.asset/AssetData/PingFang.ttc`; without
/// walking that level the UI silently falls back to a different CJK face.
fn find_pingfang() -> Option<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, depth: u8) -> Option<std::path::PathBuf> {
        if depth == 0 {
            return None;
        }
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "PingFang.ttc") && path.is_file() {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = walk(&path, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    let root = std::path::Path::new("/System/Library/AssetsV2");
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_name().to_string_lossy().contains("Font") {
            if let Some(found) = walk(&entry.path(), 3) {
                return Some(found);
            }
        }
    }
    None
}

/// Prefers the nib-friendly system CJK faces; egui's bundled fonts have no
/// Chinese glyphs, so without this every label renders as boxes.
fn candidates() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = [
        // Older macOS installs, then the modern asset bundle below.
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    if let Some(path) = find_pingfang() {
        out.insert(0, path);
    }
    out
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in candidates() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let key = "cjk".to_owned();
        let mut data = egui::FontData::from_owned(bytes);
        // PingFang.ttc bundles HK/MO/TC/SC faces first, then their weights.
        // Index 0 is PingFang HK; index 3 is Simplified Chinese Regular.
        if path.file_name().is_some_and(|name| name == "PingFang.ttc") {
            data.index = 3;
        }
        fonts
            .font_data
            .insert(key.clone(), std::sync::Arc::new(data));
        // Proportional first (labels), monospace after (connection codes).
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, key.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(key);
        tracing::info!(path = %path.display(), "loaded CJK font");
        break;
    }
    ctx.set_fonts(fonts);
}

/// A card: white surface, hairline border, generous padding, soft corners.
pub fn card_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(card(ui))
        .stroke(Stroke::new(1.0, border(ui)))
        .corner_radius(18)
        .inner_margin(egui::Margin::symmetric(22, 20))
}

/// A pill: small label with an optional leading dot.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(if ui.visuals().dark_mode { 0.28 } else { 0.12 }))
        .corner_radius(255)
        .inner_margin(egui::Margin::symmetric(11, 5))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(13.5).color(color).strong());
        });
}

/// The primary button: accent fill, white label, full width.
pub fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let width = ui.available_width();
    ui.add_sized(
        [width, 46.0],
        egui::Button::new(
            egui::RichText::new(text)
                .size(16.5)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(ACCENT)
        .corner_radius(12),
    )
}

pub fn secondary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(14.5))
            .corner_radius(10)
            .min_size(egui::vec2(0.0, 32.0)),
    )
}

/// Text input with a visible border and room for 16 px type.
pub fn input(ui: &mut egui::Ui, value: &mut String, hint: &str, secret: bool) -> egui::Response {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(Stroke::new(1.0, border(ui)))
        .inner_margin(egui::Margin::symmetric(12, 9))
        .corner_radius(11)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .frame(egui::Frame::NONE)
                    .password(secret)
                    .desired_width(f32::INFINITY)
                    .hint_text(egui::RichText::new(hint).color(muted(ui)))
                    .font(TextStyle::Body),
            )
        })
        .inner
}

/// Label above a field, with consistent rhythm.
pub fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> egui::Response {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(label).size(14.5).color(muted(ui)));
    ui.add_space(2.0);
    let response = input(ui, value, hint, false);
    ui.add_space(2.0);
    response
}

pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .color(muted(ui))
            .strong(),
    );
}

pub fn centered_width(ui: &mut egui::Ui, max: f32, add: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().min(max);
    ui.vertical_centered(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            add,
        );
    });
}
