//! Frosted-glass overlays.
//!
//! egui can't sample what's behind a shape, but over the viewer we know exactly what's there:
//! the image. Each image (and video frame) gets a tiny pre-blurred copy; glass panels draw the
//! matching part of it under a tint, which reads as real frosted glass and costs almost nothing.

use egui::{Color32, ColorImage, CornerRadius, Painter, Pos2, Rect, Stroke};

/// Longest side of the blurred copy. Tiny on purpose: it is magnified a lot, which is the blur.
const SIDE: usize = 72;

/// What sits behind the overlays this frame: the blurred texture and where the image is drawn.
#[derive(Clone, Copy)]
pub struct Backdrop {
    pub texture: egui::TextureId,
    pub draw: Rect,
}

/// A small, blurred, opaque copy of `pixels` (`size` = [w, h]).
pub fn blurred(pixels: &[Color32], size: [usize; 2]) -> ColorImage {
    let [w, h] = size;
    let scale = SIDE as f32 / w.max(h).max(1) as f32;
    let (bw, bh) = (((w as f32 * scale).round() as usize).max(1), ((h as f32 * scale).round() as usize).max(1));
    // Area-average downsample using a 4×4 grid of samples per output pixel (cheap at any size).
    let mut buf = vec![[0f32; 3]; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let mut acc = [0f32; 3];
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (((bx * 4 + sx) * w) / (bw * 4)).min(w - 1);
                    let y = (((by * 4 + sy) * h) / (bh * 4)).min(h - 1);
                    let c = pixels[y * w + x];
                    acc[0] += c.r() as f32;
                    acc[1] += c.g() as f32;
                    acc[2] += c.b() as f32;
                }
            }
            buf[by * bw + bx] = acc.map(|v| v / 16.0);
        }
    }
    // Three box-blur passes approximate a Gaussian.
    for _ in 0..3 {
        buf = box_blur(&buf, bw, bh, 2);
    }
    let pixels = buf.iter().map(|c| Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8)).collect();
    ColorImage::new([bw, bh], pixels)
}

fn box_blur(src: &[[f32; 3]], w: usize, h: usize, r: isize) -> Vec<[f32; 3]> {
    let pass = |src: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0f32; 3]; src.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0f32; 3];
                for d in -r..=r {
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    let c = src[sy * w + sx];
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                }
                out[y * w + x] = acc.map(|v| v / (2 * r + 1) as f32);
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

/// Paint a glass panel. Without a backdrop it falls back to tinted translucent dark.
pub fn paint(painter: &Painter, rect: Rect, radius: f32, backdrop: Option<Backdrop>) {
    let r = radius.min(rect.height() / 2.0).min(rect.width() / 2.0);
    let corners = CornerRadius::same(r.round() as u8);
    painter.rect_filled(rect, corners, Color32::from_black_alpha(150));
    if let Some(b) = backdrop {
        let inside = rect.intersect(b.draw);
        if inside.is_positive() {
            let uv = |p: Pos2| ((p - b.draw.min) / b.draw.size()).to_pos2();
            // Only round the corners that coincide with the panel's own corners; where the image
            // edge cuts through the panel the boundary is straight.
            let round = |a: Pos2, b: Pos2| if (a - b).length() < 0.5 { r.round() as u8 } else { 0 };
            let shape_corners = CornerRadius {
                nw: round(inside.left_top(), rect.left_top()),
                ne: round(inside.right_top(), rect.right_top()),
                sw: round(inside.left_bottom(), rect.left_bottom()),
                se: round(inside.right_bottom(), rect.right_bottom()),
            };
            let shape = egui::epaint::RectShape::filled(inside, shape_corners, Color32::WHITE)
                .with_texture(b.texture, Rect::from_min_max(uv(inside.min), uv(inside.max)));
            painter.add(shape);
        }
    }
    // Darken for legibility, then a hairline edge and a soft top highlight.
    painter.rect_filled(rect, corners, Color32::from_black_alpha(105));
    painter.rect_stroke(rect, corners, Stroke::new(1.0, Color32::from_white_alpha(30)), egui::StrokeKind::Inside);
    let top = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.min.y + (rect.height() * 0.5)));
    painter.rect_filled(
        top,
        CornerRadius { nw: corners.nw, ne: corners.ne, sw: 0, se: 0 },
        Color32::from_white_alpha(6),
    );
}
