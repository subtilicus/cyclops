//! Cyclops skins. egui holds dark and light visuals and resolves "follow system";
//! custom painting reads the active palette through [`p`].

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, CornerRadius, Stroke, Theme, Visuals};

pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// Emphasised text and icons (selected / hovered).
    pub text_strong: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub viewer_bg: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(13, 14, 18),
    panel: Color32::from_rgb(20, 22, 28),
    surface: Color32::from_rgb(28, 31, 39),
    surface_hover: Color32::from_rgb(38, 42, 53),
    border: Color32::from_rgb(44, 48, 60),
    text: Color32::from_rgb(222, 226, 235),
    text_dim: Color32::from_rgb(130, 137, 153),
    text_strong: Color32::WHITE,
    accent: Color32::from_rgb(96, 165, 250),
    accent_soft: Color32::from_rgb(30, 49, 74),
    viewer_bg: Color32::from_rgb(8, 8, 10),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(241, 243, 246),
    panel: Color32::from_rgb(250, 250, 252),
    surface: Color32::WHITE,
    surface_hover: Color32::from_rgb(232, 237, 245),
    border: Color32::from_rgb(218, 222, 230),
    text: Color32::from_rgb(30, 33, 40),
    text_dim: Color32::from_rgb(105, 113, 128),
    text_strong: Color32::from_rgb(8, 10, 14),
    accent: Color32::from_rgb(37, 99, 235),
    accent_soft: Color32::from_rgb(214, 228, 253),
    viewer_bg: Color32::from_rgb(230, 232, 236),
};

static IS_DARK: AtomicBool = AtomicBool::new(true);

/// The active palette.
pub fn p() -> &'static Palette {
    if IS_DARK.load(Ordering::Relaxed) { &DARK } else { &LIGHT }
}

/// Track egui's resolved theme (call once per frame, before painting).
pub fn sync(ctx: &egui::Context) {
    IS_DARK.store(ctx.theme() == Theme::Dark, Ordering::Relaxed);
}

pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, Visuals::dark()));
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, Visuals::light()));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.scroll.floating = true;
        style.animation_time = 0.12;
    });
}

fn visuals(p: &Palette, mut v: Visuals) -> Visuals {
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_dim);
    // Slider rails and checkbox wells; `border` stays visible against the panel in both skins.
    w.inactive.bg_fill = p.border;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.active.bg_fill = p.accent_soft;
    w.active.weak_bg_fill = p.accent_soft;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.open.weak_bg_fill = p.surface;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = radius;
    }
    v
}
