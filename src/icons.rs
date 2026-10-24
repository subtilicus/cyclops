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
    ChevronLeft,
    ChevronDown,
    Folder,
    Drive,
    Grid,
    Warning,
    Cog,
    Image,
    Sphere,
    Globe,
    Close,
    TwoPane,
    ThreePane,
    Sort,
    Play,
    Pause,
    Volume,
    Mute,
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
        Icon::ChevronLeft => line(&[p(10.0, 4.0), p(6.0, 8.0), p(10.0, 12.0)]),
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
        Icon::Close => {
            line(&[p(4.0, 4.0), p(12.0, 12.0)]);
            line(&[p(12.0, 4.0), p(4.0, 12.0)]);
        }
        Icon::Image => {
            let r = Rect::from_min_max(p(1.5, 3.0), p(14.5, 13.0));
            painter.rect_stroke(r, CornerRadius::same((2.0 * s) as u8), stroke, egui::StrokeKind::Middle);
            line(&[p(3.5, 11.0), p(7.0, 7.0), p(9.5, 9.5), p(11.0, 8.0), p(12.5, 11.0)]);
            painter.circle_filled(p(11.0, 5.6), 1.0 * s, color);
        }
        Icon::Sphere => {
            painter.circle_stroke(p(8.0, 8.0), 6.2 * s, stroke);
            painter.circle_filled(p(5.6, 5.6), 1.6 * s, color);
            let arc: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = (100.0 + i as f32 * 7.0f32).to_radians();
                    p(8.0, 8.0) + 4.4 * s * Vec2::angled(a)
                })
                .collect();
            painter.add(Shape::line(arc, Stroke::new(stroke.width * 0.8, color.gamma_multiply(0.7))));
        }
        Icon::Globe => {
            let c = p(8.0, 8.0);
            painter.circle_stroke(c, 6.2 * s, stroke);
            line(&[p(1.8, 8.0), p(14.2, 8.0)]);
            let meridian: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = (i as f32 / 20.0) * std::f32::consts::TAU;
                    c + vec2(2.8 * a.cos(), 6.2 * a.sin()) * s
                })
                .collect();
            painter.add(Shape::line(meridian, stroke));
        }
        Icon::Cog => {
            let c = p(8.0, 8.0);
            let teeth = Stroke::new(2.6 * s, color);
            for i in 0..8 {
                let dir = Vec2::angled(i as f32 * std::f32::consts::FRAC_PI_4);
                painter.line_segment([c + dir * 4.8 * s, c + dir * 7.0 * s], teeth);
            }
            painter.circle_stroke(c, 4.6 * s, stroke);
            painter.circle_stroke(c, 1.9 * s, stroke);
        }
        Icon::TwoPane | Icon::ThreePane => {
            let r = Rect::from_min_max(p(1.5, 2.5), p(14.5, 13.5));
            painter.rect_stroke(r, CornerRadius::same((2.0 * s) as u8), stroke, egui::StrokeKind::Middle);
            let dividers: &[f32] = if matches!(icon, Icon::TwoPane) { &[6.0] } else { &[5.5, 10.5] };
            for &x in dividers {
                line(&[p(x, 2.5), p(x, 13.5)]);
            }
        }
        Icon::Sort => {
            for (y, len) in [(4.0, 12.0), (8.0, 8.5), (12.0, 5.0)] {
                line(&[p(2.0, y), p(2.0 + len, y)]);
            }
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(vec![p(4.5, 2.5), p(13.5, 8.0), p(4.5, 13.5)], color, Stroke::NONE));
        }
        Icon::Pause => {
            for x in [4.0, 9.5] {
                painter.rect_filled(Rect::from_min_size(p(x, 3.0), vec2(2.8, 10.0) * s), CornerRadius::same(1), color);
            }
        }
        Icon::Volume | Icon::Mute => {
            let speaker = vec![p(1.5, 6.0), p(4.5, 6.0), p(8.5, 2.5), p(8.5, 13.5), p(4.5, 10.0), p(1.5, 10.0)];
            painter.add(Shape::convex_polygon(speaker, color, Stroke::NONE));
            if matches!(icon, Icon::Mute) {
                line(&[p(10.5, 5.5), p(14.5, 10.5)]);
                line(&[p(14.5, 5.5), p(10.5, 10.5)]);
            } else {
                let c = p(8.5, 8.0);
                for r in [3.0, 5.5] {
                    let pts: Vec<Pos2> = (0..=10).map(|i| c + r * s * Vec2::angled((-45.0 + i as f32 * 9.0f32).to_radians())).collect();
                    painter.add(Shape::line(pts, stroke));
                }
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
    button_impl(ui, icon, enabled, false, tooltip)
}

/// Like [`button`], drawn highlighted while `selected`.
pub fn toggle(ui: &mut Ui, icon: Icon, selected: bool, tooltip: &str) -> Response {
    button_impl(ui, icon, true, selected, tooltip)
}

fn button_impl(ui: &mut Ui, icon: Icon, enabled: bool, selected: bool, tooltip: &str) -> Response {
    let size = vec2(30.0, 28.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, CornerRadius::same(6), theme::p().accent_soft);
        } else if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
            let fill = if resp.is_pointer_button_down_on() { theme::p().accent_soft } else { theme::p().surface_hover };
            painter.rect_filled(rect, CornerRadius::same(6), fill);
        }
        let color = if !enabled {
            theme::p().text_dim.gamma_multiply(0.5)
        } else if selected {
            theme::p().accent
        } else if resp.hovered() {
            theme::p().text_strong
        } else {
            theme::p().text
        };
        paint(painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, color);
    }
    resp.on_hover_text(tooltip)
}
