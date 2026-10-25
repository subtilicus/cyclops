//! Windows Imaging Component decoding: HEIC/HEIF, AVIF, JPEG XL, JPEG XR and camera RAW.
//! The codecs are part of Windows (or free Store extensions), so nothing is bundled.

use std::path::Path;

use image::RgbaImage;

pub const EXTENSIONS: &[&str] = &[
    // HEIF family, AV1, JPEG XL / XR
    "heic", "heif", "hif", "avif", "jxl", "jxr", "wdp", "hdp",
];

/// Camera RAW and DNG, decoded by Microsoft's Raw Image Decoder (the free Raw Image Extension).
pub const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "arq", "srf", "sr2", "raf", "orf", "ori", "rw2", "rwl",
    "pef", "srw", "x3f", "erf", "kdc", "dcr", "mrw", "3fr", "fff", "iiq", "mef", "mos", "sti", "gpr",
];

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| list.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn handles(path: &Path) -> bool {
    has_extension(path, EXTENSIONS) || is_camera_raw(path)
}

pub fn is_camera_raw(path: &Path) -> bool {
    has_extension(path, RAW_EXTENSIONS)
}

pub struct WicImage {
    pub image: RgbaImage,
    /// Pixel size of the full image after orientation (may exceed `image` when scaled down).
    pub source_size: [u32; 2],
    /// EXIF orientation (1–8) still to be applied to `image`.
    pub orientation: u16,
    /// Source bits per channel, and whether they are floating point.
    pub bits: u8,
    pub float: bool,
    /// Linear-light copy (scRGB) for sources deeper than 8 bits, when requested.
    pub linear: Option<image::Rgba32FImage>,
}

/// Decode `path`, scaled so its longest side is at most `max_side`. With `want_linear`, sources
/// deeper than 8 bits per channel also return a high-precision linear copy.
pub fn decode(path: &Path, max_side: u32, want_linear: bool) -> Result<WicImage, String> {
    imp::decode(path, max_side, want_linear).map_err(|e| format!("WIC: {e}"))
}

#[cfg(all(test, windows))]
pub fn describe(path: &Path) -> String {
    imp::describe(path)
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use image::RgbaImage;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    use windows::core::GUID;
    use windows::Win32::System::Variant::VT_UI2;
    use windows::core::{HSTRING, Interface, w};

    use super::WicImage;

    thread_local! {
        static FACTORY: Option<IWICImagingFactory> = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
        };
    }

    fn orientation(frame: &IWICBitmapFrameDecode) -> Option<u16> {
        unsafe {
            let reader = frame.GetMetadataQueryReader().ok()?;
            let mut value = PROPVARIANT::default();
            reader.GetMetadataByName(w!("System.Photo.Orientation"), &mut value).ok()?;
            let inner = &value.Anonymous.Anonymous;
            let result = (inner.vt == VT_UI2).then(|| inner.Anonymous.uiVal);
            let _ = PropVariantClear(&mut value);
            result
        }
    }

    /// Microsoft's Raw Image Decoder, found by the formats it declares (so it works in any
    /// language). `None` when the Raw Image Extension isn't installed.
    fn raw_decoder(factory: &IWICImagingFactory) -> Option<GUID> {
        static CLSID: std::sync::OnceLock<Option<GUID>> = std::sync::OnceLock::new();
        *CLSID.get_or_init(|| unsafe {
            let all = factory.CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateDefault.0 as u32).ok()?;
            loop {
                let mut item = [None];
                let mut fetched = 0;
                if all.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                    return None;
                }
                let Some(Ok(info)) = item[0].take().map(|u| u.cast::<IWICBitmapCodecInfo>()) else { continue };
                let mut buf = vec![0u16; 4096];
                let mut len = 0;
                if info.GetFileExtensions(&mut buf, &mut len).is_err() {
                    continue;
                }
                let extensions = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]).to_ascii_lowercase();
                if extensions.contains(".nef") && extensions.contains(".cr3") {
                    return info.GetCLSID().ok();
                }
            }
        })
    }

    /// Camera RAW goes to the Raw Image Decoder explicitly: left to choose, Windows hands many
    /// DNGs to its older DNG decoder and IIQ/MEF to the TIFF decoder, which only return the
    /// small embedded preview. Anything that decoder can't open falls back to Windows' choice.
    unsafe fn open(factory: &IWICImagingFactory, path: &Path) -> windows::core::Result<IWICBitmapDecoder> {
        unsafe {
            if super::is_camera_raw(path) {
                if let Some(clsid) = raw_decoder(factory) {
                    let raw = || -> windows::core::Result<IWICBitmapDecoder> {
                        let decoder: IWICBitmapDecoder = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
                        let stream = factory.CreateStream()?;
                        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_READ.0)?;
                        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
                        Ok(decoder)
                    };
                    if let Ok(decoder) = raw() {
                        return Ok(decoder);
                    }
                }
            }
            factory.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
        }
    }

    /// Bits per channel and float-ness of a WIC pixel format.
    fn depth(factory: &IWICImagingFactory, format: &windows::core::GUID) -> Option<(u8, bool)> {
        unsafe {
            let info: IWICPixelFormatInfo2 = factory.CreateComponentInfo(format).ok()?.cast().ok()?;
            let bpp = info.GetBitsPerPixel().ok()?;
            let channels = info.GetChannelCount().ok()?.max(1);
            let float = info.GetNumericRepresentation().is_ok_and(|r| r == WICPixelFormatNumericRepresentationFloat);
            Some(((bpp / channels) as u8, float))
        }
    }

    /// Decoder, frames and pixel formats of a file (survey test diagnostics).
    #[cfg(test)]
    pub fn describe(path: &Path) -> String {
        fn name(info: windows::core::Result<IWICComponentInfo>) -> String {
            unsafe {
                let Ok(info) = info else { return "?".into() };
                let mut buf = [0u16; 128];
                let mut len = 0;
                let _ = info.GetFriendlyName(&mut buf, &mut len);
                String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)])
            }
        }
        FACTORY.with(|factory| unsafe {
            let Some(factory) = factory.as_ref() else { return "no factory".into() };
            let decoder = match open(factory, path) {
                Ok(d) => d,
                Err(e) => return format!("no decoder: {e}"),
            };
            let codec = decoder.GetDecoderInfo().map(|i| name(i.cast())).unwrap_or_default();
            let count = decoder.GetFrameCount().unwrap_or(0);
            let mut out = format!("{codec}, {count} frame(s):");
            for i in 0..count {
                if let Ok(frame) = decoder.GetFrame(i) {
                    let (mut w, mut h) = (0, 0);
                    let _ = frame.GetSize(&mut w, &mut h);
                    let format = frame.GetPixelFormat().map(|f| name(factory.CreateComponentInfo(&f))).unwrap_or_default();
                    let develop = if frame.cast::<IWICDevelopRaw>().is_ok() { " develop-raw" } else { "" };
                    out += &format!(" [{w}x{h} {format}{develop}]");
                }
            }
            if let Ok(preview) = decoder.GetPreview() {
                let (mut w, mut h) = (0, 0);
                let _ = preview.GetSize(&mut w, &mut h);
                out += &format!(" preview {w}x{h}");
            }
            out
        })
    }

    pub fn decode(path: &Path, max_side: u32, want_linear: bool) -> windows::core::Result<WicImage> {
        FACTORY.with(|factory| {
            let factory = factory.as_ref().ok_or_else(windows::core::Error::empty)?;
            unsafe {
                let decoder = open(factory, path)?;
                let frame = decoder.GetFrame(0)?;
                let (mut w, mut h) = (0u32, 0u32);
                frame.GetSize(&mut w, &mut h)?;
                let orientation = orientation(&frame).unwrap_or(1);
                let (bits, float) = frame.GetPixelFormat().ok().and_then(|f| depth(factory, &f)).unwrap_or((8, false));

                let mut source: IWICBitmapSource = frame.cast()?;
                let (mut tw, mut th) = (w, h);
                if w.max(h) > max_side {
                    let scale = max_side as f64 / w.max(h) as f64;
                    tw = ((w as f64 * scale).round() as u32).max(1);
                    th = ((h as f64 * scale).round() as u32).max(1);
                    let scaler = factory.CreateBitmapScaler()?;
                    scaler.Initialize(&source, tw, th, WICBitmapInterpolationModeFant)?;
                    source = scaler.cast()?;
                }
                let converter = factory.CreateFormatConverter()?;
                converter.Initialize(
                    &source,
                    &GUID_WICPixelFormat32bppRGBA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )?;
                let mut pixels = vec![0u8; tw as usize * th as usize * 4];
                converter.CopyPixels(std::ptr::null(), tw * 4, &mut pixels)?;
                let image = RgbaImage::from_raw(tw, th, pixels).ok_or_else(windows::core::Error::empty)?;
                let source_size = if (5..=8).contains(&orientation) { [h, w] } else { [w, h] };
                // Deep sources: also fetch half-float scRGB (linear) for exposure editing.
                let linear = if want_linear && (bits > 8 || float) {
                    let converter = factory.CreateFormatConverter()?;
                    converter.Initialize(
                        &source,
                        &GUID_WICPixelFormat64bppRGBAHalf,
                        WICBitmapDitherTypeNone,
                        None,
                        0.0,
                        WICBitmapPaletteTypeCustom,
                    )?;
                    let mut halves = vec![0u16; tw as usize * th as usize * 4];
                    converter.CopyPixels(std::ptr::null(), tw * 8, bytemuck::cast_slice_mut(&mut halves))?;
                    let floats = halves.iter().map(|&h| half::f16::from_bits(h).to_f32()).collect();
                    image::Rgba32FImage::from_raw(tw, th, floats)
                } else {
                    None
                };
                Ok(WicImage { image, source_size, orientation, bits, float, linear })
            }
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::WicImage;

    pub fn decode(_path: &Path, _max_side: u32, _want_linear: bool) -> Result<WicImage, String> {
        Err("not available on this platform".into())
    }
}
