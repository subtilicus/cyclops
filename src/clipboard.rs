//! Copying files and images to the Windows clipboard, the way Explorer and image apps do: the
//! file itself (CF_HDROP: pastes as a file in Explorer, mail, chat) and, for images, the pixels
//! (CF_DIB and PNG: pastes as a picture in editors and documents).

use std::path::Path;

use image::RgbaImage;

/// Put `path` on the clipboard as a file, plus `image` as a picture if given. Returns the
/// clipboard's sequence number afterwards (to tell later whether something else was copied).
pub fn set(path: &Path, image: Option<&RgbaImage>) -> Result<u32, String> {
    imp::set(path, image)
}

/// The clipboard's current sequence number (changes whenever anything is copied).
pub fn sequence() -> u32 {
    imp::sequence()
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use image::RgbaImage;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_HDROP};
    use windows::Win32::UI::Shell::DROPFILES;
    use windows::core::w;

    /// Global memory holding `bytes`, as the clipboard requires (it takes ownership on success).
    unsafe fn global(bytes: &[u8]) -> Result<HGLOBAL, String> {
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
            let target = GlobalLock(memory) as *mut u8;
            if target.is_null() {
                return Err("couldn't lock clipboard memory".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            let _ = GlobalUnlock(memory);
            Ok(memory)
        }
    }

    fn drop_files(path: &Path) -> Vec<u8> {
        // DROPFILES header, then the path as UTF-16, double NUL-terminated.
        let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, fWide: true.into(), ..Default::default() };
        let mut bytes = unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>()) }.to_vec();
        for unit in path.as_os_str().encode_wide().chain([0, 0]) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    /// A 32-bit bottom-up DIB (BITMAPINFOHEADER + BGRA rows).
    fn dib(image: &RgbaImage) -> Vec<u8> {
        let (w, h) = image.dimensions();
        let mut bytes = Vec::with_capacity(40 + w as usize * h as usize * 4);
        for v in [40u32, w, h] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&1u16.to_le_bytes()); // planes
        bytes.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        bytes.extend_from_slice(&[0u8; 24]); // BI_RGB, size and resolution left to defaults
        for row in image.as_raw().chunks_exact(w as usize * 4).rev() {
            for p in row.chunks_exact(4) {
                bytes.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        bytes
    }

    fn png(image: &RgbaImage) -> Option<Vec<u8>> {
        use image::ImageEncoder;
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        let mut out = Vec::new();
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
            .write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8)
            .ok()?;
        Some(out)
    }

    pub fn set(path: &Path, image: Option<&RgbaImage>) -> Result<u32, String> {
        // Prepare everything before taking the clipboard, so it is held only briefly.
        let mut formats: Vec<(u32, Vec<u8>)> = vec![(CF_HDROP.0 as u32, drop_files(path))];
        if let Some(image) = image {
            if let Some(png) = png(image) {
                formats.push((unsafe { RegisterClipboardFormatW(w!("PNG")) }, png));
            }
            formats.push((CF_DIB.0 as u32, dib(image)));
        }
        unsafe {
            // Another program may hold the clipboard for a moment; retry briefly.
            let mut opened = OpenClipboard(None);
            for _ in 0..10 {
                if opened.is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                opened = OpenClipboard(None);
            }
            opened.map_err(|e| format!("Clipboard is busy: {e}"))?;
            let result = (|| {
                EmptyClipboard().map_err(|e| e.to_string())?;
                for (format, bytes) in &formats {
                    let memory = global(bytes)?;
                    SetClipboardData(*format, Some(HANDLE(memory.0))).map_err(|e| e.to_string())?;
                }
                Ok(GetClipboardSequenceNumber())
            })();
            let _ = CloseClipboard();
            result
        }
    }

    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn set(_: &Path, _: Option<&image::RgbaImage>) -> Result<u32, String> {
        Err("copying files is only supported on Windows".into())
    }

    pub fn sequence() -> u32 {
        0
    }
}
