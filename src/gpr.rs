//! GoPro GPR files: DNGs whose raw data is VC-5 compressed, which LibRaw can't decode itself.
//! The vendored GPR SDK (`third_party/gpr`) converts them to plain DNGs in memory.

use std::ffi::c_void;

unsafe extern "C" {
    fn cyc_gpr_to_dng(data: *const u8, len: usize, out: *mut *mut u8, out_len: *mut usize) -> i32;
    fn cyc_gpr_free(p: *mut c_void);
}

/// A standard DNG with the same image and metadata.
pub fn to_dng(gpr: &[u8]) -> Result<Vec<u8>, String> {
    let (mut out, mut len) = (std::ptr::null_mut(), 0);
    if unsafe { cyc_gpr_to_dng(gpr.as_ptr(), gpr.len(), &mut out, &mut len) } == 0 {
        return Err("GoPro GPR: couldn't decode the VC-5 data".into());
    }
    let dng = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
    unsafe { cyc_gpr_free(out.cast()) };
    Ok(dng)
}
