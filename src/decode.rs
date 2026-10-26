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
    "exr", "qoi", "pbm", "pgm", "ppm", "pnm", "pam", "svg", "svgz", "psd", "ktx", "ktx2",
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

/// Bits per channel of the source, e.g. 8, 16, or 32-bit float.
#[derive(Clone, Copy, Debug)]
pub struct BitDepth {
    pub bits: u8,
    pub float: bool,
}

impl BitDepth {
    pub const EIGHT: Self = Self { bits: 8, float: false };

    pub fn label(self) -> String {
        if self.float { format!("{}-bit float", self.bits) } else { format!("{}-bit", self.bits) }
    }

    /// Deeper than an 8-bit display can show directly (worth an exposure control).
    pub fn is_deep(self) -> bool {
        self.bits > 8 || self.float
    }

    fn of(color: image::ColorType) -> Self {
        use image::ColorType::*;
        match color {
            L16 | La16 | Rgb16 | Rgba16 => Self { bits: 16, float: false },
            Rgb32F | Rgba32F => Self { bits: 32, float: true },
            _ => Self::EIGHT,
        }
    }
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
    /// High-precision linear data (HDR/EXR, 16-bit, deep WIC formats), for exposure changes.
    pub hdr: Option<Hdr>,
    pub bit_depth: Option<BitDepth>,
    /// Storage format worth showing (e.g. "BC7" for GPU textures).
    pub format: Option<String>,
    /// GPU textures: [levels, layers, faces], and the other sub-images (mips, layers, faces).
    /// The image above is sub-image `[0, 0, 0]`.
    pub sub_layout: Option<[u32; 3]>,
    pub subimages: Vec<([u32; 3], Decoded)>,
}

impl Decoded {
    fn still(image: RgbaImage, opaque: bool) -> Self {
        let source_size = [image.width(), image.height()];
        Self {
            image,
            opaque,
            source_size,
            frames: Vec::new(),
            hdr: None,
            bit_depth: None,
            format: None,
            sub_layout: None,
            subimages: Vec::new(),
        }
    }
}

/// Decode `path`. Raster formats that can scale while decoding (WIC) stop at `max_side`;
/// vector formats render with their longest side at `vector_side`. `animate` marks a full-size
/// request: animated GIF/WebP/APNG return all frames and HDR/EXR keep their linear data. With `animate`, animated
/// GIF/WebP/APNG return all frames.
pub fn decode(path: &Path, max_side: u32, vector_side: u32, animate: bool) -> Result<Decoded, String> {
    let ext = extension(path);
    if crate::wic::is_camera_raw(path) {
        // LibRaw first (full sensor bit depth); Windows' Raw Image Decoder for what it can't read.
        let raw = raw_bytes(path).and_then(|bytes| decode_raw(&bytes, max_side, animate));
        return raw.or_else(|e| from_wic(path, max_side, animate).map_err(|_| codec_hint(&ext, e)));
    }
    if crate::wic::handles(path) {
        return from_wic(path, max_side, animate).map_err(|e| codec_hint(&ext, e));
    }
    // TIFFs are often huge (scans, satellite, print). WIC scales while decoding, so a
    // 120-megapixel file never needs a full-size buffer; fall back to `image` if WIC can't.
    if ext == "tif" || ext == "tiff" {
        if let Ok(d) = from_wic(path, max_side, animate) {
            return Ok(d);
        }
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match ext.as_str() {
        "svg" | "svgz" => return decode_svg(&bytes, vector_side),
        "psd" => return decode_psd(&bytes),
        "dds" | "ktx" | "ktx2" => return decode_texture(&bytes, &ext, max_side, animate),
        "hdr" | "exr" => return decode_hdr(&bytes, max_side, animate),
        _ => {}
    }
    if is_jpeg(path) {
        // Some files have a .jpg extension but are something else; fall back to `image`.
        if let Ok(d) = decode_jpeg(&bytes, animate) {
            return Ok(d);
        }
    }
    if animate {
        if let Some(frames) = decode_animation(&bytes, &ext) {
            let image = frames[0].0.clone();
            return Ok(Decoded { frames, opaque: false, bit_depth: Some(BitDepth::EIGHT), ..Decoded::still(image, false) });
        }
    }
    let img = match load_unlimited(&bytes) {
        Ok(img) => img,
        // Unusual variants (e.g. exotic TIFF compressions) may still be readable by Windows.
        Err(e) => return from_wic(path, max_side, animate).map_err(|_| e),
    };
    let opaque = !img.color().has_alpha();
    let depth = BitDepth::of(img.color());
    // Deep images keep their precision for exposure editing (full-size requests only).
    let hdr = (animate && depth.is_deep()).then(|| {
        let light = if depth.float { Light::Scene } else { Light::DisplaySrgb };
        high_precision(img.to_rgba32f(), max_side, light)
    });
    Ok(Decoded { hdr, bit_depth: Some(depth), ..Decoded::still(img.to_rgba8(), opaque) })
}

/// How high-precision pixel values relate to light.
enum Light {
    /// Scene-referred linear (HDR, EXR, float TIFF): tone-mapped with ACES, auto exposure.
    Scene,
    /// Display-referred and already linear (WIC's scRGB): shown as-is at 0 EV.
    DisplayLinear,
    /// Display-referred, sRGB-encoded (16-bit PNG/TIFF): linearised, shown as-is at 0 EV.
    DisplaySrgb,
    /// Developed camera RAW: linear, 1.0 = sensor saturation. Shown clipped like a photo at
    /// `exposure`, so lowering exposure recovers highlights up to the sensor's own limit.
    Raw { exposure: f32 },
}

/// Build GPU exposure data from high-precision pixels (scaled to at most `max_side`).
fn high_precision(mut img: image::Rgba32FImage, max_side: u32, light: Light) -> Hdr {
    use rayon::prelude::*;
    let (w, h) = img.dimensions();
    if w.max(h) > max_side {
        let scale = max_side as f32 / w.max(h) as f32;
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    }
    let size = [img.width(), img.height()];
    let mut pixels = img.into_raw();
    if matches!(light, Light::DisplaySrgb) {
        let linear = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        pixels.par_chunks_mut(4).for_each(|p| {
            for c in &mut p[..3] {
                *c = linear(*c);
            }
        });
    }
    let (auto_exposure, tonemap) = match light {
        Light::Scene => (auto_exposure(&pixels), true),
        Light::Raw { exposure } => (exposure, false),
        _ => (1.0, false),
    };
    Hdr { levels: hdr_mips(pixels, size), auto_exposure, tonemap }
}

/// Formats whose grid thumbnails should come from Explorer's cache before decoding ourselves
/// (large or slow to decode). Not HDR/EXR: their look depends on tone mapping, and a
/// thumbnail made by another program (e.g. Photoshop) wouldn't match Cyclops's own view.
pub fn prefers_shell_thumbnail(path: &Path) -> bool {
    crate::wic::handles(path) || matches!(extension(path).as_str(), "tif" | "tiff" | "psd")
}

fn from_wic(path: &Path, max_side: u32, want_linear: bool) -> Result<Decoded, String> {
    let wic = crate::wic::decode(path, max_side, want_linear)?;
    let image = apply_orientation(wic.image, wic.orientation);
    let opaque = is_opaque(&image);
    let hdr = wic.linear.map(|l| high_precision(apply_orientation(l, wic.orientation), max_side, Light::DisplayLinear));
    let bit_depth = Some(BitDepth { bits: wic.bits, float: wic.float });
    Ok(Decoded { source_size: wic.source_size, hdr, bit_depth, ..Decoded::still(image, opaque) })
}

/// `image::load_from_memory` without the default 512 MB allocation cap: big images are
/// legitimate here, and they are downscaled right after decoding.
fn load_unlimited(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    reader.no_limits();
    reader.decode().map_err(|e| e.to_string())
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
    /// Exposure multiplier at 0 EV (maps a scene's log-average luminance to middle grey; 1 for
    /// display-referred images).
    pub auto_exposure: f32,
    /// Tone-map highlights with ACES (scene-referred); otherwise clip (display-referred).
    pub tonemap: bool,
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
/// DDS / KTX / KTX2 GPU textures. Full-size requests decode every mip level, array layer and
/// cube face so the viewer can step through them; thumbnails only the first.
fn decode_texture(bytes: &[u8], ext: &str, max_side: u32, full: bool) -> Result<Decoded, String> {
    let set = crate::gputex::decode_container(bytes, ext, !full)?;
    let mut subs = set.subs.into_iter();
    let first = subs.next().ok_or("texture has no image data")?;
    let mut decoded = texture_image(first.texture, max_side, full)?;
    if full && (set.levels > 1 || set.layers > 1 || set.faces > 1) {
        decoded.sub_layout = Some([set.levels, set.layers, set.faces]);
        for sub in subs {
            decoded.subimages.push(([sub.level, sub.layer, sub.face], texture_image(sub.texture, max_side, full)?));
        }
    }
    Ok(decoded)
}

fn texture_image(texture: crate::gputex::Texture, max_side: u32, keep_linear: bool) -> Result<Decoded, String> {
    use crate::gputex::Pixels;
    let format = Some(texture.format);
    match texture.pixels {
        Pixels::Ldr(image) => {
            let opaque = is_opaque(&image);
            Ok(Decoded { format, bit_depth: Some(BitDepth::EIGHT), ..Decoded::still(image, opaque) })
        }
        // Float textures (often HDR environment maps) get tone mapping and exposure.
        Pixels::Float(img) => {
            // BC6H and RGBA16F store half floats; the rest are full 32-bit floats.
            let half = matches!(format.as_deref(), Some("BC6H" | "RGBA16F" | "UASTC HDR"));
            let bit_depth = Some(BitDepth { bits: if half { 16 } else { 32 }, float: true });
            Ok(Decoded { format, bit_depth, ..from_float(img, max_side, keep_linear)? })
        }
    }
}

fn decode_hdr(bytes: &[u8], max_side: u32, keep_linear: bool) -> Result<Decoded, String> {
    from_float(load_unlimited(bytes)?.into_rgba32f(), max_side, keep_linear)
}

/// Scene-linear float pixels: CPU tone-mapped image plus, with `keep_linear`, GPU exposure data.
fn from_float(mut img: image::Rgba32FImage, max_side: u32, keep_linear: bool) -> Result<Decoded, String> {
    let (w, h) = img.dimensions();
    // Measure exposure on the full-resolution data, before any downscaling: shrinking averages
    // highlights into their surroundings and would give thumbnails a different (darker) exposure
    // than the full-size view.
    let exposure = auto_exposure(img.as_raw());
    if w.max(h) > max_side {
        let scale = max_side as f32 / w.max(h) as f32;
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    }
    let size = [img.width(), img.height()];
    let pixels = img.into_raw();
    let image = tonemap(&pixels, size, exposure);
    let hdr = keep_linear.then(|| Hdr { levels: hdr_mips(pixels, size), auto_exposure: exposure, tonemap: true });
    let opaque = is_opaque(&image);
    let bit_depth = Some(BitDepth { bits: 32, float: true });
    Ok(Decoded { hdr, bit_depth, source_size: [w, h], ..Decoded::still(image, opaque) })
}

/// Photoshop files carry a flattened composite of all layers; show that.
fn decode_psd(bytes: &[u8]) -> Result<Decoded, String> {
    let psd = psd::Psd::from_bytes(bytes).map_err(|e| e.to_string())?;
    let bits = psd.depth() as u8;
    let image = RgbaImage::from_raw(psd.width(), psd.height(), psd.rgba()).ok_or("PSD buffer size mismatch")?;
    let opaque = is_opaque(&image);
    Ok(Decoded { bit_depth: Some(BitDepth { bits, float: bits == 32 }), ..Decoded::still(image, opaque) })
}

/// A RAW file's bytes for LibRaw: GoPro GPRs are converted to plain DNGs first.
fn raw_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if extension(path) == "gpr" { crate::gpr::to_dng(&bytes) } else { Ok(bytes) }
}

/// Camera RAW through LibRaw. Viewer images are developed from the sensor data to linear 16-bit;
/// thumbnails use the camera's embedded preview when it is big enough, else a fast half-size
/// develop.
fn decode_raw(bytes: &[u8], max_side: u32, full: bool) -> Result<Decoded, String> {
    use rayon::prelude::*;
    let raw = crate::libraw::Raw::open(bytes)?;
    let meta = raw.metadata();
    let bit_depth = Some(BitDepth { bits: meta.bits, float: false });
    let format = Some(meta.camera.clone()).filter(|c| !c.is_empty());
    if !full {
        if let Some(image) = raw_preview(&raw, &meta, max_side) {
            return Ok(Decoded { source_size: meta.size, bit_depth, format, ..Decoded::still(fit_within(image, max_side), true) });
        }
    }
    let half = !full && meta.size[0].max(meta.size[1]) / 2 >= max_side;
    raw.develop(half, |w, h, rgb| {
        let exposure = raw_exposure(rgb);
        // 8-bit sRGB copy for thumbnails, the blurred backdrop and the screen-size copy.
        let lut: Vec<u8> = (0..=u16::MAX as u32)
            .map(|v| {
                let c = (v as f32 / 65535.0 * exposure).min(1.0);
                let s = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
                (s * 255.0 + 0.5) as u8
            })
            .collect();
        let mut srgb = vec![255u8; w as usize * h as usize * 4];
        srgb.par_chunks_mut(4).zip(rgb.par_chunks(3)).for_each(|(o, i)| {
            o[0] = lut[i[0] as usize];
            o[1] = lut[i[1] as usize];
            o[2] = lut[i[2] as usize];
        });
        let image = RgbaImage::from_raw(w, h, srgb).expect("buffer size");
        let hdr = full.then(|| {
            let mut linear = vec![1.0f32; w as usize * h as usize * 4];
            linear.par_chunks_mut(4).zip(rgb.par_chunks(3)).for_each(|(o, i)| {
                for c in 0..3 {
                    o[c] = i[c] as f32 / 65535.0;
                }
            });
            let linear = image::Rgba32FImage::from_raw(w, h, linear).expect("buffer size");
            high_precision(linear, max_side, Light::Raw { exposure })
        });
        let source_size = if half { meta.size } else { [w, h] };
        Decoded { source_size, hdr, bit_depth, format, ..Decoded::still(fit_within(image, max_side), true) }
    })
}

/// A camera RAW's embedded preview at up to `max_side`, with the RAW's real size and metadata:
/// shown in the viewer while the full develop runs. `None` if there is no usable preview.
pub fn raw_preview_file(path: &Path, max_side: u32) -> Option<Decoded> {
    // A GPR has to be fully decoded to read even its preview, so there's nothing to gain.
    if extension(path) == "gpr" {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let raw = crate::libraw::Raw::open(&bytes).ok()?;
    let meta = raw.metadata();
    let image = raw_preview(&raw, &meta, max_side)?;
    Some(Decoded {
        source_size: meta.size,
        bit_depth: Some(BitDepth { bits: meta.bits, float: false }),
        format: Some(meta.camera.clone()).filter(|c| !c.is_empty()),
        ..Decoded::still(fit_within(image, max_side), true)
    })
}

/// Exposure that puts the brightest 1% of pixels at white, like LibRaw's (and dcraw's) automatic
/// brightness, within sensible limits.
fn raw_exposure(rgb: &[u16]) -> f32 {
    let mut histogram = vec![0u32; 1 << 12];
    let mut count = 0u32;
    for p in rgb.chunks_exact(3).step_by(5) {
        histogram[(p[0].max(p[1]).max(p[2]) >> 4) as usize] += 1;
        count += 1;
    }
    let mut above = 0;
    let mut white = histogram.len();
    while white > 1 && above < count / 100 {
        white -= 1;
        above += histogram[white];
    }
    (histogram.len() as f32 / white.max(1) as f32).clamp(0.25, 64.0)
}

/// The camera's embedded preview, oriented, if it is large enough for a thumbnail (old cameras
/// often keep only a 160 px one).
fn raw_preview(raw: &crate::libraw::Raw, meta: &crate::libraw::Metadata, max_side: u32) -> Option<RgbaImage> {
    let image = match raw.preview().ok()? {
        crate::libraw::Preview::Jpeg(bytes) => {
            let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_strict_mode(false);
            let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes[..]), options);
            let pixels = decoder.decode().ok()?;
            let info = decoder.info()?;
            RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)?
        }
        crate::libraw::Preview::Rgb { width, height, pixels } => {
            let rgba = pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            RgbaImage::from_raw(width, height, rgba)?
        }
    };
    (image.width().max(image.height()) >= max_side.min(400)).then(|| apply_orientation(image, meta.orientation))
}

/// `full`: a viewer image, which may use the GPU's JPEG decoder (see `hwjpeg`).
fn decode_jpeg(bytes: &[u8], full: bool) -> Result<Decoded, String> {
    // Large baseline 4:2:0 photos go to the GPU's JPEG decoder when one is free.
    if let Some(header) = crate::hwjpeg::header(bytes).filter(|_| full) {
        if let Some(image) = crate::hwjpeg::decode(bytes, &header) {
            let orientation = header.exif(bytes).and_then(exif_orientation).unwrap_or(1);
            return Ok(Decoded { bit_depth: Some(BitDepth::EIGHT), ..Decoded::still(apply_orientation(image, orientation), true) });
        }
    }
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
    Ok(Decoded { bit_depth: Some(BitDepth::EIGHT), ..Decoded::still(apply_orientation(image, orientation), true) })
}

/// Downscale (area-averaging) so that neither side exceeds `max_side`.
pub fn fit_within(image: RgbaImage, max_side: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w <= max_side && h <= max_side {
        return image;
    }
    downscaled(&image, max_side)
}

/// A copy scaled down so that neither side exceeds `max_side` (a plain copy if it already fits).
///
/// Exact area averaging: every output pixel is the mean of the source area it covers, with
/// partially covered pixels weighted by coverage, so non-integer ratios stay even. Rows are
/// done in parallel, each from its few source rows, so no full-size intermediate is needed.
pub fn downscaled(image: &RgbaImage, max_side: u32) -> RgbaImage {
    use rayon::prelude::*;
    let (w, h) = image.dimensions();
    if w <= max_side && h <= max_side {
        return image.clone();
    }
    let scale = max_side as f32 / w.max(h) as f32;
    let nw = ((w as f32 * scale).round() as u32).clamp(1, w);
    let nh = ((h as f32 * scale).round() as u32).clamp(1, h);
    let (xs, ys) = (box_weights(w as usize, nw as usize), box_weights(h as usize, nh as usize));
    let (sw, src) = (w as usize * 4, image.as_raw());
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    out.par_chunks_mut(nw as usize * 4).zip(ys.par_iter()).for_each_init(
        || vec![0f32; sw],
        |acc, (dst, (y0, wy))| {
            // Vertical: weighted sum of this output row's source rows.
            acc.fill(0.0);
            for (k, &weight) in wy.iter().enumerate() {
                let row = &src[(y0 + k) * sw..(y0 + k + 1) * sw];
                for (a, &v) in acc.iter_mut().zip(row) {
                    *a += v as f32 * weight;
                }
            }
            // Horizontal.
            for (x, (x0, wx)) in xs.iter().enumerate() {
                let mut px = [0f32; 4];
                for (k, &weight) in wx.iter().enumerate() {
                    let s = &acc[(x0 + k) * 4..(x0 + k) * 4 + 4];
                    for c in 0..4 {
                        px[c] += s[c] * weight;
                    }
                }
                for c in 0..4 {
                    dst[x * 4 + c] = (px[c] + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        },
    );
    RgbaImage::from_raw(nw, nh, out).expect("buffer size")
}

/// For each of `dst` output samples: the first source sample it covers and the weight of each
/// covered source sample (summing to 1).
fn box_weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let (a, b) = (i as f64 * ratio, ((i + 1) as f64 * ratio).min(src as f64));
            let first = a.floor() as usize;
            let last = (b.ceil() as usize).clamp(first + 1, src);
            let weights = (first..last)
                .map(|j| ((b.min(j as f64 + 1.0) - a.max(j as f64)) / ratio) as f32)
                .collect();
            (first, weights)
        })
        .collect()
}

fn apply_orientation<P: image::Pixel + 'static>(
    img: image::ImageBuffer<P, Vec<P::Subpixel>>,
    orientation: u16,
) -> image::ImageBuffer<P, Vec<P::Subpixel>> {
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// Full-size decodes of every JPEG in `CYCLOPS_BENCH_DIR`, one after another like flipping
    /// through photos in the viewer, on the CPU and then on the GPU's JPEG decoder:
    /// `CYCLOPS_BENCH_DIR=<folder> cargo test --release bench_viewer_jpeg -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_viewer_jpeg() {
        let dir = std::env::var("CYCLOPS_BENCH_DIR").expect("set CYCLOPS_BENCH_DIR to a folder of JPEGs");
        let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| Some(e.ok()?.path())).filter(|p| super::is_jpeg(p)).collect();
        files.sort();
        assert!(!files.is_empty(), "no JPEGs");
        crate::hwjpeg::warm_up();
        let started = Instant::now();
        while crate::hwjpeg::status() == "Starting…" && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("hardware decoder: {} (ready after {:.0} ms)", crate::hwjpeg::status(), started.elapsed().as_secs_f64() * 1e3);
        for hw in [false, true, false, true] {
            crate::hwjpeg::set_enabled(hw);
            let (mut decode, mut screen) = (Duration::ZERO, Duration::ZERO);
            for f in &files {
                let t = Instant::now();
                let d = super::decode(f, 16384, 1024, true).unwrap();
                decode += t.elapsed();
                let t = Instant::now();
                std::hint::black_box(super::downscaled(&d.image, 2560));
                screen += t.elapsed();
            }
            let n = files.len() as f64;
            println!(
                "{}: decode {:.1} ms, screen copy {:.1} ms per image ({} files)",
                if hw { "GPU" } else { "CPU" },
                decode.as_secs_f64() * 1e3 / n,
                screen.as_secs_f64() * 1e3 / n,
                files.len()
            );
        }
    }
}

#[cfg(test)]
mod raw_survey {
    use std::time::Instant;

    /// Tries every file under `CYCLOPS_RAW_DIR` through WIC (whatever its extension) and reports
    /// what decodes: `CYCLOPS_RAW_DIR=<folder> cargo test --release raw_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn raw_survey() {
        let dir = std::env::var("CYCLOPS_RAW_DIR").expect("set CYCLOPS_RAW_DIR");
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !p.ends_with(".git") {
                        stack.push(p);
                    }
                } else if p.metadata().is_ok_and(|m| m.len() > 4096) {
                    files.push(p);
                }
            }
        }
        files.sort();
        let (mut ok, mut failed) = (0, 0);
        for f in &files {
            if let Some(out) = std::env::var_os("CYCLOPS_RAW_COMPARE") {
                // Developed image and embedded preview side by side, for checking colour/orientation.
                let Ok(bytes) = super::raw_bytes(f) else { continue };
                let (Ok(dev), Ok(pre)) = (super::decode_raw(&bytes, 16384, true), super::decode_raw(&bytes, 640, false)) else { continue };
                let dev = super::fit_within(dev.image, 640);
                let pre = super::fit_within(pre.image, 640);
                let mut sheet = image::RgbaImage::from_pixel(1290, 650, image::Rgba([40, 40, 40, 255]));
                image::imageops::overlay(&mut sheet, &dev, 0, 0);
                image::imageops::overlay(&mut sheet, &pre, 650, 0);
                let name = f.file_stem().unwrap().to_string_lossy().replace(' ', "_");
                sheet.save(std::path::Path::new(&out).join(format!("{name}.png"))).unwrap();
                ok += 1;
                continue;
            }
            if std::env::var_os("CYCLOPS_RAW_LIBRAW").is_some() {
                let Ok(bytes) = super::raw_bytes(f) else {
                    failed += 1;
                    println!("FAIL conversion  {}", f.display());
                    continue;
                };
                let t = Instant::now();
                let thumb = super::decode_raw(&bytes, 512, false);
                let thumb_ms = t.elapsed().as_secs_f64() * 1e3;
                let t = Instant::now();
                match super::decode_raw(&bytes, 16384, true) {
                    Ok(d) => {
                        ok += 1;
                        let ev = d.hdr.as_ref().map_or(0.0, |h| h.auto_exposure.log2());
                        println!(
                            "OK   full {:>6.0} ms  thumb {:>5.0} ms {:<9} {:>5}x{:<5} {:>2}-bit  {:+.1} EV  {}  [{}]",
                            t.elapsed().as_secs_f64() * 1e3,
                            thumb_ms,
                            match &thumb { Ok(t) if t.hdr.is_none() && t.image.width() >= 400 => "(preview)", Ok(_) => "(develop)", Err(_) => "(failed)" },
                            d.source_size[0],
                            d.source_size[1],
                            d.bit_depth.map_or(0, |b| b.bits),
                            ev,
                            f.file_name().unwrap().to_string_lossy(),
                            d.format.unwrap_or_default()
                        );
                    }
                    Err(e) => {
                        failed += 1;
                        println!("FAIL {e}  {}", f.file_name().unwrap().to_string_lossy());
                    }
                }
                continue;
            }
            if std::env::var_os("CYCLOPS_RAW_DESCRIBE").is_some() {
                println!("{}  {}", crate::wic::describe(f), f.file_name().unwrap().to_string_lossy());
                continue;
            }
            let t = Instant::now();
            let listed = crate::wic::handles(f);
            match crate::wic::decode(f, 512, false) {
                Ok(img) => {
                    ok += 1;
                    println!(
                        "OK   {:>7.0} ms  {:>5}x{:<5} {:>2}-bit {}  {}",
                        t.elapsed().as_secs_f64() * 1e3,
                        img.source_size[0],
                        img.source_size[1],
                        img.bits,
                        if listed { "      " } else { "UNLIST" },
                        f.display()
                    );
                }
                Err(e) => {
                    failed += 1;
                    println!("FAIL {} {}  {e}", if listed { "      " } else { "UNLIST" }, f.display());
                }
            }
        }
        println!("{ok} decoded, {failed} failed");
    }
}
