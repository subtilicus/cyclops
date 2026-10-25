//! Camera RAW through the vendored LibRaw (CDDL-1.0, unmodified) in `third_party/`: full sensor
//! bit depth, developed to linear light so exposure can be changed afterwards.

use std::ffi::{CStr, c_char, c_void};

// LibRaw calls zlib's `uncompress` (deflate DNGs); referencing the crate keeps it linked.
use libz_sys as _;

unsafe extern "C" {
    fn cyc_raw_open(data: *const u8, len: usize, error: *mut i32) -> *mut c_void;
    fn cyc_raw_close(handle: *mut c_void);
    fn cyc_raw_strerror(error: i32) -> *const c_char;
    fn cyc_raw_info(handle: *mut c_void, out: *mut Info);
    fn cyc_raw_develop(handle: *mut c_void, half: i32, quality: i32, image: *mut *mut c_void, w: *mut u32, h: *mut u32) -> i32;
    fn cyc_raw_pixels(image: *mut c_void) -> *const u16;
    fn cyc_raw_thumb(
        handle: *mut c_void,
        image: *mut *mut c_void,
        data: *mut *const u8,
        len: *mut usize,
        kind: *mut i32,
        w: *mut u32,
        h: *mut u32,
    ) -> i32;
    fn cyc_raw_free(image: *mut c_void);
}

#[repr(C)]
struct Info {
    width: u32,
    height: u32,
    bits: u32,
    flip: i32,
    make: [c_char; 64],
    model: [c_char; 64],
}

fn error(code: i32) -> String {
    let text = unsafe { CStr::from_ptr(cyc_raw_strerror(code)) };
    format!("LibRaw: {}", text.to_string_lossy())
}

pub struct Metadata {
    /// Developed size after orientation.
    pub size: [u32; 2],
    pub bits: u8,
    /// EXIF-style orientation (1, 3, 6 or 8) of the sensor data and embedded preview.
    pub orientation: u16,
    /// "Make Model", for the info bar.
    pub camera: String,
}

pub enum Preview {
    Jpeg(Vec<u8>),
    Rgb { width: u32, height: u32, pixels: Vec<u8> },
}

/// A RAW file opened from memory (the bytes must outlive it).
pub struct Raw<'a> {
    handle: *mut c_void,
    _bytes: std::marker::PhantomData<&'a [u8]>,
}

impl Drop for Raw<'_> {
    fn drop(&mut self) {
        unsafe { cyc_raw_close(self.handle) };
    }
}

/// A developed image owned by LibRaw.
struct Developed(*mut c_void);

impl Drop for Developed {
    fn drop(&mut self) {
        unsafe { cyc_raw_free(self.0) };
    }
}

impl<'a> Raw<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let mut code = 0;
        let handle = unsafe { cyc_raw_open(bytes.as_ptr(), bytes.len(), &mut code) };
        if handle.is_null() {
            return Err(error(code));
        }
        Ok(Self { handle, _bytes: std::marker::PhantomData })
    }

    pub fn metadata(&self) -> Metadata {
        let mut info = Info { width: 0, height: 0, bits: 0, flip: 0, make: [0; 64], model: [0; 64] };
        unsafe { cyc_raw_info(self.handle, &mut info) };
        let text = |s: &[c_char; 64]| unsafe { CStr::from_ptr(s.as_ptr()) }.to_string_lossy().trim().to_owned();
        let (make, model) = (text(&info.make), text(&info.model));
        let camera = if model.to_lowercase().starts_with(&make.to_lowercase()) || make.is_empty() {
            model
        } else {
            format!("{make} {model}")
        };
        let orientation = match info.flip {
            3 => 3,
            5 => 8,
            6 => 6,
            _ => 1,
        };
        Metadata { size: [info.width, info.height], bits: info.bits.clamp(1, 16) as u8, orientation, camera }
    }

    /// Linear 16-bit RGB (sRGB primaries, camera white balance; 65535 = sensor saturation),
    /// already oriented. `half`: half resolution without demosaicing (much faster).
    pub fn develop<T>(&self, half: bool, f: impl FnOnce(u32, u32, &[u16]) -> T) -> Result<T, String> {
        let (mut image, mut w, mut h) = (std::ptr::null_mut(), 0, 0);
        // Quality 3: AHD demosaicing.
        let code = unsafe { cyc_raw_develop(self.handle, half as i32, 3, &mut image, &mut w, &mut h) };
        if code != 0 {
            return Err(error(code));
        }
        let developed = Developed(image);
        let pixels = unsafe { std::slice::from_raw_parts(cyc_raw_pixels(developed.0), w as usize * h as usize * 3) };
        Ok(f(w, h, pixels))
    }

    /// The camera's embedded preview (not oriented).
    pub fn preview(&self) -> Result<Preview, String> {
        let (mut image, mut data, mut len, mut kind, mut w, mut h) = (std::ptr::null_mut(), std::ptr::null(), 0, 0, 0, 0);
        let code = unsafe { cyc_raw_thumb(self.handle, &mut image, &mut data, &mut len, &mut kind, &mut w, &mut h) };
        if code != 0 {
            return Err(error(code));
        }
        let mem = Developed(image);
        let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        drop(mem);
        Ok(if kind == 1 { Preview::Jpeg(bytes) } else { Preview::Rgb { width: w, height: h, pixels: bytes } })
    }
}
