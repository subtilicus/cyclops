//! Image decoding: fast path for JPEG via zune-jpeg, everything else via `image`.

use std::path::Path;

use image::RgbaImage;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "webp", "tif", "tiff",
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
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["jpg", "jpeg", "jpe", "jfif"].iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub struct Decoded {
    pub image: RgbaImage,
    /// Whether every pixel is opaque (lets us skip premultiplication).
    pub opaque: bool,
}

pub fn decode(path: &Path) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if is_jpeg(path) {
        // Some files have a .jpg extension but are something else; fall back to `image`.
        if let Ok(d) = decode_jpeg(&bytes) {
            return Ok(d);
        }
    }
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let opaque = !img.color().has_alpha();
    Ok(Decoded { image: img.to_rgba8(), opaque })
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
    Ok(Decoded { image: apply_orientation(image, orientation), opaque: true })
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
