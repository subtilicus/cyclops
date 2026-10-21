//! Basis Universal (ETC1S / UASTC / UASTC HDR) KTX2 textures, via the vendored open-source
//! transcoder (Apache-2.0) in `third_party/`. LDR transcodes to RGBA8; HDR to half floats, so it
//! gets tone mapping and exposure like other HDR images.

use std::ffi::c_void;
use std::sync::Once;

use image::{Rgba32FImage, RgbaImage};

use crate::gputex::{Pixels, SubTexture, Texture, TextureSet};

unsafe extern "C" {
    fn cyc_basis_init();
    fn cyc_ktx2_open(data: *const u8, len: u32) -> *mut c_void;
    fn cyc_ktx2_close(handle: *mut c_void);
    fn cyc_ktx2_info(handle: *mut c_void, levels: *mut u32, layers: *mut u32, faces: *mut u32, kind: *mut i32);
    fn cyc_ktx2_image_size(handle: *mut c_void, level: u32, layer: u32, face: u32, w: *mut u32, h: *mut u32) -> i32;
    fn cyc_ktx2_transcode(handle: *mut c_void, level: u32, layer: u32, face: u32, out: *mut c_void, pixels: u32, hdr: i32) -> i32;
}

/// Owns a transcoder handle (one per decode, so decoding is safe on any worker thread).
struct Ktx2(*mut c_void);

impl Drop for Ktx2 {
    fn drop(&mut self) {
        unsafe { cyc_ktx2_close(self.0) };
    }
}

pub fn decode_ktx2(bytes: &[u8], first_only: bool) -> Result<TextureSet, String> {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe { cyc_basis_init() });

    let len = u32::try_from(bytes.len()).map_err(|_| "KTX2 file is too large")?;
    let handle = unsafe { cyc_ktx2_open(bytes.as_ptr(), len) };
    if handle.is_null() {
        return Err("KTX2: Basis Universal couldn't read this file".into());
    }
    let ktx = Ktx2(handle);
    let (mut levels, mut layers, mut faces, mut kind) = (1u32, 1u32, 1u32, 0i32);
    unsafe { cyc_ktx2_info(ktx.0, &mut levels, &mut layers, &mut faces, &mut kind) };
    let hdr = kind == 2;
    let label = match kind {
        0 => "Basis ETC1S",
        1 => "Basis UASTC",
        2 => "UASTC HDR",
        _ => "Basis",
    };

    let mut subs = Vec::new();
    'all: for level in 0..levels {
        for layer in 0..layers {
            for face in 0..faces {
                let (mut w, mut h) = (0u32, 0u32);
                if unsafe { cyc_ktx2_image_size(ktx.0, level, layer, face, &mut w, &mut h) } == 0 || w == 0 || h == 0 {
                    continue;
                }
                let pixels = w as usize * h as usize;
                let image = if hdr {
                    let mut halves = vec![0u16; pixels * 4];
                    let ok = unsafe { cyc_ktx2_transcode(ktx.0, level, layer, face, halves.as_mut_ptr().cast(), pixels as u32, 1) };
                    if ok == 0 {
                        return Err(format!("KTX2: couldn't transcode level {level}"));
                    }
                    let floats = halves.iter().map(|&b| half::f16::from_bits(b).to_f32()).collect();
                    Pixels::Float(Rgba32FImage::from_raw(w, h, floats).ok_or("KTX2 size mismatch")?)
                } else {
                    let mut rgba = vec![0u8; pixels * 4];
                    let ok = unsafe { cyc_ktx2_transcode(ktx.0, level, layer, face, rgba.as_mut_ptr().cast(), pixels as u32, 0) };
                    if ok == 0 {
                        return Err(format!("KTX2: couldn't transcode level {level}"));
                    }
                    Pixels::Ldr(RgbaImage::from_raw(w, h, rgba).ok_or("KTX2 size mismatch")?)
                };
                subs.push(SubTexture { level, layer, face, texture: Texture { pixels: image, format: label.to_owned() } });
                if first_only {
                    break 'all;
                }
            }
        }
    }
    if subs.is_empty() {
        return Err("KTX2 file has no image data".into());
    }
    Ok(TextureSet { subs, levels, layers, faces })
}
