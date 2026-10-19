//! Image decoding: zune-jpeg for JPEG, Windows Imaging Component for HEIC/AVIF/JXL/RAW,
//! resvg for SVG, the `psd` crate for Photoshop composites, and `image` for everything else
//! (including animated GIF/WebP/APNG).

use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "bmp", "webp", "tif", "tiff", "ico", "tga", "dds", "hdr",
    "exr", "qoi", "pbm", "pgm", "ppm", "pnm", "pam", "svg", "svgz", "psd",
];

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "wmv", "avi", "mkv", "webm", "3gp", "3g2", "mts", "m2ts", "ts", "mpg", "mpeg",
];

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Anything the browser lists: images and videos.
pub fn is_media(path: &Path) -> bool {
    is_image(path) || is_video(path)
}

pub fn is_image(path: &Path) -> bool {
    crate::wic::handles(path)
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["jpg", "jpeg", "jpe", "jfif"].iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub struct Decoded {
    /// The (first) image; may already be scaled down for WIC and SVG sources.
    pub image: RgbaImage,
    /// Whether every pixel is opaque (lets us skip premultiplication).
    pub opaque: bool,
    /// Full pixel size of the source, after orientation.
    pub source_size: [u32; 2],
    /// Animation frames with their display time in seconds (empty for stills).
    pub frames: Vec<(RgbaImage, f32)>,
    /// Linear-light data for HDR/EXR, for exposure changes.
    pub hdr: Option<Hdr>,
}

impl Decoded {
    fn still(image: RgbaImage, opaque: bool) -> Self {
        let source_size = [image.width(), image.height()];
        Self { image, opaque, source_size, frames: Vec::new(), hdr: None }
    }
}

/// Decode `path`. Raster formats that can scale while decoding (WIC) stop at `max_side`;
/// vector formats render with their longest side at `vector_side`. `animate` marks a full-size
/// request: animated GIF/WebP/APNG return all frames and HDR/EXR keep their linear data. With `animate`, animated
/// GIF/WebP/APNG return all frames.
pub fn decode(path: &Path, max_side: u32, vector_side: u32, animate: bool) -> Result<Decoded, String> {
    let ext = extension(path);
    if crate::wic::handles(path) {
        let wic = crate::wic::decode(path, max_side).map_err(|e| codec_hint(&ext, e))?;
        let image = apply_orientation(wic.image, wic.orientation);
        let opaque = is_opaque(&image);
        return Ok(Decoded { image, opaque, source_size: wic.source_size, frames: Vec::new(), hdr: None });
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match ext.as_str() {
        "svg" | "svgz" => return decode_svg(&bytes, vector_side),
        "psd" => return decode_psd(&bytes),
        "hdr" | "exr" => return decode_hdr(&bytes, max_side, animate),
        _ => {}
    }
    if is_jpeg(path) {
        // Some files have a .jpg extension but are something else; fall back to `image`.
        if let Ok(d) = decode_jpeg(&bytes) {
            return Ok(d);
        }
    }
    if animate {
        if let Some(frames) = decode_animation(&bytes, &ext) {
            let image = frames[0].0.clone();
            let source_size = [image.width(), image.height()];
            return Ok(Decoded { image, opaque: false, source_size, frames, hdr: None });
        }
    }
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let opaque = !img.color().has_alpha();
    Ok(Decoded::still(img.to_rgba8(), opaque))
}

/// Point at the free Store codec when Windows can't decode a format.
fn codec_hint(ext: &str, err: String) -> String {
    let hint = match ext {
        "heic" | "heif" | "hif" => "install HEIF Image Extensions (and HEVC Video Extensions) from the Microsoft Store",
        "avif" => "install AV1 Video Extension from the Microsoft Store",
        "jxl" => "install JPEG XL Image Extension from the Microsoft Store",
        "jxr" | "wdp" | "hdp" => return err,
        _ => "install Raw Image Extension from the Microsoft Store",
    };
    format!("{err}\n{hint}")
}

fn is_opaque(image: &RgbaImage) -> bool {
    image.as_raw().chunks_exact(4).all(|p| p[3] == 255)
}

/// All frames of an animated GIF, WebP or APNG, or `None` for stills (or absurdly large animations).
fn decode_animation(bytes: &[u8], ext: &str) -> Option<Vec<(RgbaImage, f32)>> {
    use image::AnimationDecoder;
    use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
    const BUDGET: usize = 768 << 20;

    let frames = match ext {
        "gif" => GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames(),
        "webp" => {
            let d = WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.has_animation() {
                return None;
            }
            d.into_frames()
        }
        "png" | "apng" => {
            let d = PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            d.apng().ok()?.into_frames()
        }
        _ => return None,
    };
    let mut out = Vec::new();
    let mut used = 0usize;
    for frame in frames {
        let frame = frame.ok()?;
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = num as f32 / den.max(1) as f32;
        // Browsers treat near-zero delays as 100 ms; so do we.
        let secs = if ms <= 10.0 { 0.1 } else { ms / 1000.0 };
        let image = frame.into_buffer();
        used += image.as_raw().len();
        if used > BUDGET {
            return None;
        }
        out.push((image, secs));
    }
    (out.len() > 1).then_some(out)
}

fn decode_svg(bytes: &[u8], side: u32) -> Result<Decoded, String> {
    use resvg::{tiny_skia, usvg};
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let mut options = usvg::Options::default();
    options.fontdb = FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone();
    let tree = usvg::Tree::from_data(bytes, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = side.max(16) as f32 / size.width().max(size.height());
    let (w, h) = (((size.width() * scale).ceil() as u32).max(1), ((size.height() * scale).ceil() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("SVG is too large to render")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let image = RgbaImage::from_raw(w, h, rgba).ok_or("SVG buffer size mismatch")?;
    let opaque = is_opaque(&image);
    Ok(Decoded::still(image, opaque))
}

/// One mip level of linear-light RGBA as half floats, ready for a GPU texture.
pub struct HdrLevel {
    pub size: [u32; 2],
    pub texels: Vec<u16>,
}

/// Linear-light HDR/EXR data for GPU tone mapping (exposure can change without re-decoding).
pub struct Hdr {
    /// Full mip chain, largest first.
    pub levels: Vec<HdrLevel>,
    /// Exposure multiplier that maps the scene's log-average luminance to middle grey.
    pub auto_exposure: f32,
}

fn auto_exposure(pixels: &[f32]) -> f32 {
    let (mut log_sum, mut count) = (0.0f64, 0usize);
    for p in pixels.chunks_exact(4).step_by(7) {
        let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        if l.is_finite() && l > 0.0 {
            log_sum += (l as f64 + 1e-4).ln();
            count += 1;
        }
    }
    let average = if count > 0 { (log_sum / count as f64).exp() as f32 } else { 0.18 };
    (0.18 / average.max(1e-6)).clamp(1.0 / 64.0, 64.0)
}

/// CPU tone map (auto exposure, ACES, sRGB) for thumbnails and the first frame on screen.
fn tonemap(pixels: &[f32], size: [u32; 2], exposure: f32) -> RgbaImage {
    use rayon::prelude::*;
    let aces = |x: f32| {
        let x = if x.is_finite() { (x * exposure).max(0.0) } else { 0.0 };
        ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
    };
    let to_srgb = |c: f32| {
        let v = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (v * 255.0 + 0.5) as u8
    };
    let mut out = vec![0u8; pixels.len()];
    out.par_chunks_mut(4).zip(pixels.par_chunks(4)).for_each(|(o, p)| {
        let a = if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 1.0 };
        o.copy_from_slice(&[to_srgb(aces(p[0])), to_srgb(aces(p[1])), to_srgb(aces(p[2])), (a * 255.0 + 0.5) as u8]);
    });
    RgbaImage::from_raw(size[0], size[1], out).expect("tonemap buffer size")
}

/// Half-float mip chain built by 2×2 box filtering (sanitising NaN/inf on the way).
fn hdr_mips(pixels: Vec<f32>, size: [u32; 2]) -> Vec<HdrLevel> {
    use half::f16;
    let to_half = |v: &[f32]| v.iter().map(|&x| f16::from_f32(if x.is_finite() { x } else { 0.0 }).to_bits()).collect();
    let mut levels = vec![HdrLevel { size, texels: to_half(&pixels) }];
    let (mut cur, [mut w, mut h]) = (pixels, size);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0f32; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((x * 2 + dx).min(w - 1), (y * 2 + dy).min(h - 1));
                    let i = ((sy * w + sx) * 4) as usize;
                    for c in 0..4 {
                        let v = cur[i + c];
                        acc[c] += if v.is_finite() { v } else { 0.0 };
                    }
                }
                let o = ((y * nw + x) * 4) as usize;
                for c in 0..4 {
                    next[o + c] = acc[c] * 0.25;
                }
            }
        }
        levels.push(HdrLevel { size: [nw, nh], texels: to_half(&next) });
        (cur, w, h) = (next, nw, nh);
    }
    levels
}

/// Radiance HDR and OpenEXR hold linear scene light. Returns a CPU tone-mapped image (for
/// thumbnails and as a fallback) plus, with `keep_linear`, the data for GPU tone mapping,
/// scaled to at most `max_side`.
fn decode_hdr(bytes: &[u8], max_side: u32, keep_linear: bool) -> Result<Decoded, String> {
    let mut img = image::load_from_memory(bytes).map_err(|e| e.to_string())?.into_rgba32f();
    let (w, h) = img.dimensions();
    if w.max(h) > max_side {
        let scale = max_side as f32 / w.max(h) as f32;
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    }
    let size = [img.width(), img.height()];
    let pixels = img.into_raw();
    let exposure = auto_exposure(&pixels);
    let image = tonemap(&pixels, size, exposure);
    let hdr = keep_linear.then(|| Hdr { levels: hdr_mips(pixels, size), auto_exposure: exposure });
    let opaque = is_opaque(&image);
    Ok(Decoded { hdr, source_size: [w, h], ..Decoded::still(image, opaque) })
}

/// Photoshop files carry a flattened composite of all layers; show that.
fn decode_psd(bytes: &[u8]) -> Result<Decoded, String> {
    let psd = psd::Psd::from_bytes(bytes).map_err(|e| e.to_string())?;
    let image = RgbaImage::from_raw(psd.width(), psd.height(), psd.rgba()).ok_or("PSD buffer size mismatch")?;
    let opaque = is_opaque(&image);
    Ok(Decoded::still(image, opaque))
}

fn decode_jpeg(bytes: &[u8]) -> Result<Decoded, String> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(u16::MAX as usize)
        .set_max_height(u16::MAX as usize)
        .set_strict_mode(false);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = decoder.decode().map_err(|e| format!("{e:?}"))?;
    let info = decoder.info().ok_or("missing jpeg info")?;
    let orientation = decoder.exif().and_then(|e| exif_orientation(e)).unwrap_or(1);
    let image = RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
        .ok_or("jpeg buffer size mismatch")?;
    Ok(Decoded::still(apply_orientation(image, orientation), true))
}

/// Downscale (area-averaging) so that neither side exceeds `max_side`.
pub fn fit_within(image: RgbaImage, max_side: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w <= max_side && h <= max_side {
        return image;
    }
    let scale = max_side as f32 / w.max(h) as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    image::imageops::thumbnail(&image, nw, nh)
}

fn apply_orientation(img: RgbaImage, orientation: u16) -> RgbaImage {
    use image::imageops::*;
    match orientation {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

/// Minimal TIFF/EXIF walker that only extracts the Orientation tag (0x0112) from IFD0.
fn exif_orientation(exif: &[u8]) -> Option<u16> {
    let exif = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let le = match exif.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = exif.get(o..o + 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = exif.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).find_map(|i| {
        let entry = ifd + 2 + i * 12;
        (u16_at(entry)? == 0x0112).then(|| u16_at(entry + 8)).flatten()
    })
}
