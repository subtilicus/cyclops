//! Windows Imaging Component decoding: HEIC/HEIF, AVIF, JPEG XL, JPEG XR and camera RAW.
//! The codecs are part of Windows (or free Store extensions), so nothing is bundled.

use std::path::Path;

use image::RgbaImage;

pub const EXTENSIONS: &[&str] = &[
    // HEIF family, AV1, JPEG XL / XR
    "heic", "heif", "hif", "avif", "jxl", "jxr", "wdp", "hdp",
    // Camera RAW (decoded by the Microsoft Raw Image Extension) and DNG
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "x3f",
    "erf", "kdc", "dcr", "mrw", "3fr", "iiq", "rwl",
];

pub fn handles(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub struct WicImage {
    pub image: RgbaImage,
    /// Pixel size of the full image after orientation (may exceed `image` when scaled down).
    pub source_size: [u32; 2],
    /// EXIF orientation (1–8) still to be applied to `image`.
    pub orientation: u16,
}

/// Decode `path`, scaled so its longest side is at most `max_side`.
pub fn decode(path: &Path, max_side: u32) -> Result<WicImage, String> {
    imp::decode(path, max_side).map_err(|e| format!("WIC: {e}"))
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use image::RgbaImage;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
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

    pub fn decode(path: &Path, max_side: u32) -> windows::core::Result<WicImage> {
        FACTORY.with(|factory| {
            let factory = factory.as_ref().ok_or_else(windows::core::Error::empty)?;
            unsafe {
                let decoder = factory.CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )?;
                let frame = decoder.GetFrame(0)?;
                let (mut w, mut h) = (0u32, 0u32);
                frame.GetSize(&mut w, &mut h)?;
                let orientation = orientation(&frame).unwrap_or(1);

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
                Ok(WicImage { image, source_size, orientation })
            }
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::WicImage;

    pub fn decode(_path: &Path, _max_side: u32) -> Result<WicImage, String> {
        Err("not available on this platform".into())
    }
}
