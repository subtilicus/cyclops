//! Crisp vector icons painted directly, so no icon font is needed.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, vec2};

use crate::theme;

#[derive(Clone, Copy)]
pub enum Icon {
    Back,
    Up,
    Down,
    Refresh,
    ChevronRight,
    ChevronDown,
    Folder,
    Drive,
    Grid,
    Warning,
}

/// Paint `icon` centred in `rect`, drawn on a 16×16 design grid scaled to fit.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - vec2(8.0, 8.0) * s;
    let p = |x: f32, y: f32| -> Pos2 { o + vec2(x, y) * s };
    let stroke = Stroke::new((1.5 * s).max(1.0), color);
    let line = |pts: &[Pos2]| {
        painter.add(Shape::line(pts.to_vec(), stroke));
    };

    match icon {
        Icon::Back => {
            line(&[p(13.0, 8.0), p(3.5, 8.0)]);
            line(&[p(7.5, 4.0), p(3.5, 8.0), p(7.5, 12.0)]);
        }
        Icon::Up => {
            line(&[p(8.0, 13.0), p(8.0, 3.5)]);
            line(&[p(4.0, 7.5), p(8.0, 3.5), p(12.0, 7.5)]);
        }
        Icon::Down => {
            line(&[p(8.0, 3.0), p(8.0, 12.5)]);
            line(&[p(4.0, 8.5), p(8.0, 12.5), p(12.0, 8.5)]);
        }
        Icon::Refresh => {
            let c = p(8.0, 8.0);
            let r = 5.0 * s;
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = (-60.0 + i as f32 * 12.5).to_radians();
                    c + r * Vec2::angled(a)
                })
                .collect();
            let end = *pts.last().unwrap_or(&c);
            painter.add(Shape::line(pts, stroke));
            let head = (-60.0f32 + 300.0).to_radians();
            let tangent = Vec2::angled(head + std::f32::consts::FRAC_PI_2);
            let normal = Vec2::angled(head);
            line(&[end - tangent * 3.0 * s + normal * 2.0 * s, end, end - tangent * 3.0 * s - normal * 2.0 * s]);
        }
        Icon::ChevronRight => line(&[p(6.0, 4.0), p(10.0, 8.0), p(6.0, 12.0)]),
        Icon::ChevronDown => line(&[p(4.0, 6.0), p(8.0, 10.0), p(12.0, 6.0)]),
        Icon::Folder => {
            let tab = vec![p(1.5, 4.0), p(1.5, 3.0), p(6.0, 3.0), p(7.5, 4.5), p(14.5, 4.5), p(14.5, 13.0), p(1.5, 13.0)];
            painter.add(Shape::convex_polygon(tab, color.gamma_multiply(0.35), stroke));
            painter.line_segment([p(1.5, 6.5), p(14.5, 6.5)], Stroke::new(stroke.width * 0.7, color.gamma_multiply(0.7)));
        }
        Icon::Drive => {
            let r = Rect::from_min_max(p(1.5, 5.0), p(14.5, 11.5));
            painter.rect_stroke(r, CornerRadius::same((2.0 * s) as u8), stroke, egui::StrokeKind::Middle);
            painter.circle_filled(p(11.5, 8.25), 1.0 * s, color);
        }
        Icon::Grid => {
            for (x, y) in [(2.0, 2.0), (9.0, 2.0), (2.0, 9.0), (9.0, 9.0)] {
                let r = Rect::from_min_size(p(x, y), vec2(5.0, 5.0) * s);
                painter.rect_stroke(r, CornerRadius::same((1.5 * s) as u8), stroke, egui::StrokeKind::Middle);
            }
        }
        Icon::Warning => {
            let tri = vec![p(8.0, 2.0), p(15.0, 14.0), p(1.0, 14.0)];
            painter.add(Shape::convex_polygon(tri, Color32::TRANSPARENT, stroke));
            line(&[p(8.0, 6.5), p(8.0, 10.0)]);
            painter.circle_filled(p(8.0, 12.0), 0.9 * s, color);
        }
    }
}

/// A frameless square toolbar button that shows a hover/press surface.
pub fn button(ui: &mut Ui, icon: Icon, enabled: bool, tooltip: &str) -> Response {
    let size = vec2(30.0, 28.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
            let fill = if resp.is_pointer_button_down_on() { theme::ACCENT_SOFT } else { theme::SURFACE_HOVER };
            painter.rect_filled(rect, CornerRadius::same(6), fill);
        }
        let color = if !enabled {
            theme::TEXT_DIM.gamma_multiply(0.5)
        } else if resp.hovered() {
            Color32::WHITE
        } else {
            theme::TEXT
        };
        paint(painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, color);
    }
    resp.on_hover_text(tooltip)
}
