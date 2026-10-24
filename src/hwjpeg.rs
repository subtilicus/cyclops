//! Hardware JPEG decoding on the GPU's video engine, through the Media Foundation MJPEG decoder
//! MFTs that AMD, NVIDIA and Intel drivers install.
//!
//! The hardware does the entropy decoding and IDCT and hands back the JPEG's own Y'CbCr planes
//! as NV12; we upsample the chroma like libjpeg's "fancy" upsampling and convert to RGB
//! ourselves, matching the CPU decoder to within rounding. NV12 is exactly 4:2:0, so only
//! baseline 8-bit 4:2:0 files go to the hardware (nearly every camera and phone JPEG): drivers
//! return NV12 even for 4:2:2 sources, which would halve their chroma. Progressive, 4:2:2,
//! 4:4:4, 12-bit, CMYK, RGB-coded and grayscale JPEGs stay on the CPU decoder, as do small
//! images, which the CPU finishes before a round trip to the GPU would.
//!
//! It is used for full-size viewer images, where one image's latency matters. Thumbnails are
//! many small jobs that the parallel CPU workers finish faster, and keeping them off the
//! hardware leaves its sessions free (and warm) for the viewer. A worker that finds every
//! session busy decodes on the CPU instead, so the hardware never makes anyone wait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use image::RgbaImage;
use rayon::prelude::*;

/// What we need to know about a JPEG before choosing a decoder.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// Baseline 8-bit 4:2:0 Y'CbCr: the hardware decodes it exactly.
    pub hardware: bool,
    /// Byte range of the EXIF block (APP1 "Exif\0\0..."), for orientation.
    pub exif: Option<(usize, usize)>,
}

impl Header {
    pub fn exif<'a>(&self, bytes: &'a [u8]) -> Option<&'a [u8]> {
        self.exif.and_then(|(start, len)| bytes.get(start..start + len))
    }
}

/// Parse markers up to the frame header. `None` if this doesn't look like a JPEG.
pub fn header(bytes: &[u8]) -> Option<Header> {
    if bytes.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    let mut adobe_transform = None;
    let mut exif = None;
    loop {
        // Skip to the next marker (fill bytes are 0xFF).
        while *bytes.get(i)? != 0xFF {
            i += 1;
        }
        while *bytes.get(i)? == 0xFF {
            i += 1;
        }
        let marker = *bytes.get(i)?;
        i += 1;
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            continue;
        }
        let len = u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]) as usize;
        let seg = bytes.get(i + 2..i + len)?;
        match marker {
            // Adobe APP14: transform 0 means the three channels are RGB, not Y'CbCr.
            0xEE if seg.starts_with(b"Adobe") => adobe_transform = seg.get(11).copied(),
            0xE1 if exif.is_none() && seg.starts_with(b"Exif\0\0") => exif = Some((i + 2, len - 2)),
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let precision = *seg.first()?;
                let height = u16::from_be_bytes([*seg.get(1)?, *seg.get(2)?]) as u32;
                let width = u16::from_be_bytes([*seg.get(3)?, *seg.get(4)?]) as u32;
                let count = *seg.get(5)? as usize;
                let factors: Option<Vec<(u8, u8)>> =
                    (0..count).map(|c| seg.get(7 + c * 3).map(|hv| (hv >> 4, hv & 15))).collect();
                // Baseline or extended Huffman, 8-bit, three Y'CbCr components, 4:2:0.
                let hardware = matches!(marker, 0xC0 | 0xC1)
                    && precision == 8
                    && adobe_transform != Some(0)
                    && width > 0
                    && height > 0
                    && factors? == [(2, 2), (1, 1), (1, 1)];
                return Some(Header { width, height, hardware, exif });
            }
            // Scan data before a frame header: malformed.
            0xDA | 0xD9 => return None,
            _ => {}
        }
        i += len;
    }
}

/// Decode on the GPU if a hardware session is free and the file suits it.
/// `None` means "use the CPU decoder" (not suitable, all sessions busy, or the hardware failed).
pub fn decode(bytes: &[u8], header: &Header) -> Option<RgbaImage> {
    if !ENABLED.load(Ordering::Relaxed) || !header.hardware || (header.width as u64 * header.height as u64) < MIN_PIXELS {
        return None;
    }
    // While `warm_up` is still opening sessions, the CPU decodes instead of waiting.
    let pool = POOL.get()?;
    // Take an idle session, never waiting for one; prefer one already set up for this size,
    // since changing size costs a reconfigure and a slower first frame.
    let mut idle: Vec<_> = pool.iter().filter_map(|slot| slot.try_lock().ok()).filter(|g| g.is_some()).collect();
    if idle.is_empty() {
        return None;
    }
    let pick = idle.iter().position(|g| g.as_ref().is_some_and(|s| s.is_configured_for(header.width, header.height)));
    let mut guard = idle.swap_remove(pick.unwrap_or(0));
    drop(idle);
    let session = guard.as_mut()?;
    match session.decode(bytes, header.width, header.height) {
        Ok(image) => {
            static FIRST: std::sync::Once = std::sync::Once::new();
            FIRST.call_once(|| log::info!("hardware JPEG: first decode ({}x{})", header.width, header.height));
            Some(image)
        }
        Err(e) => {
            log::warn!("hardware JPEG decode failed on {}: {e}", session.name);
            // A failed session may be wedged; replace it.
            let index = session.index;
            *guard = imp::Session::open_index(index).ok();
            None
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static POOL: OnceLock<Vec<Mutex<Option<imp::Session>>>> = OnceLock::new();
/// The decoder's name, once the pool is open (read without touching the sessions).
static NAME: OnceLock<Option<String>> = OnceLock::new();

/// Turn hardware decoding on or off (the Settings switch). Turning it on opens the decoder
/// sessions in the background the first time.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        warm_up();
    }
}

/// Open the decoder sessions on a background thread (opening one takes ~100 ms).
pub fn warm_up() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("hwjpeg-init".into()).spawn(|| {
            pool();
        });
    });
}

/// What the About page shows: the hardware decoder's name, or why there is none.
pub fn status() -> String {
    if !ENABLED.load(Ordering::Relaxed) {
        return "Off (CPU)".into();
    }
    match NAME.get() {
        None => "Starting…".into(),
        Some(Some(name)) => name.clone(),
        Some(None) => "No hardware decoder (CPU)".into(),
    }
}

const SESSIONS: usize = 2;
/// A small 4:2:0 JPEG decoded once per session at startup (128 px: decoders refuse 64).
const PRIME: &[u8] = include_bytes!("hwjpeg_prime.jpg");
/// Below about 2 megapixels the CPU decoder is faster (measured on an RTX 5090 and Ryzen 9950X).
const MIN_PIXELS: u64 = 2_000_000;

fn pool() -> Option<&'static [Mutex<Option<imp::Session>>]> {
    let pool = POOL.get_or_init(|| {
        if std::env::var("CYCLOPS_HWJPEG").is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("off")) {
            let _ = NAME.set(None);
            return Vec::new();
        }
        let started = std::time::Instant::now();
        let sessions: Vec<_> = (0..SESSIONS)
            .map_while(|_| imp::Session::open().ok())
            .map(|mut s| {
                // The first frame a session decodes pays for driver setup; pay it now.
                if let Err(e) = s.decode(PRIME, 128, 128) {
                    log::info!("hardware JPEG: priming failed: {e}");
                }
                Mutex::new(Some(s))
            })
            .collect();
        let name = sessions.first().and_then(|s| s.lock().ok()?.as_ref().map(|s| s.name.clone()));
        match &name {
            Some(name) => log::info!("hardware JPEG: {} x {name} ({:.0} ms)", sessions.len(), started.elapsed().as_secs_f64() * 1e3),
            None => log::info!("hardware JPEG: no decoder available"),
        }
        let _ = NAME.set(name);
        sessions
    });
    (!pool.is_empty()).then_some(pool.as_slice())
}

/// Full-range BT.601 (JFIF) Y'CbCr → RGB, 16.16 fixed point. `cb`/`cr` arrive scaled by 16
/// (from the upsampling weights), which is folded into the constants.
#[inline(always)]
fn rgb(y: i32, cb: i32, cr: i32) -> [u8; 4] {
    let (cb, cr) = (cb - 128 * 16, cr - 128 * 16);
    let y = y << 16;
    let r = y + 5743 * cr; // 1.402 * 65536 / 16
    let g = y - 1410 * cb - 2925 * cr; // 0.344136, 0.714136
    let b = y + 7258 * cb; // 1.772
    let c = |v: i32| ((v + 32768) >> 16).clamp(0, 255) as u8;
    [c(r), c(g), c(b), 255]
}

/// NV12 → RGBA, in parallel by row. Chroma is upsampled with libjpeg's triangle filter: each
/// output pixel takes 3/4 of the nearer chroma sample and 1/4 of the farther one, both ways.
fn convert_420(width: usize, height: usize, y_plane: &[u8], y_pitch: usize, uv_plane: &[u8], uv_pitch: usize) -> RgbaImage {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let mut out = vec![0u8; width * height * 4];
    out.par_chunks_mut(width * 4).enumerate().for_each(|(y, dst)| {
        let luma = &y_plane[y * y_pitch..y * y_pitch + width];
        // Row y sits 1/4 above or below chroma row y/2.
        let near = y / 2;
        let far = if y % 2 == 0 { near.saturating_sub(1) } else { (near + 1).min(ch - 1) };
        let rn = &uv_plane[near * uv_pitch..near * uv_pitch + cw * 2];
        let rf = &uv_plane[far * uv_pitch..far * uv_pitch + cw * 2];
        // Vertically filtered chroma (x4) for this output row.
        let col: Vec<i32> = rn.iter().zip(rf).map(|(&n, &f)| 3 * n as i32 + f as i32).collect();
        for x in 0..width {
            let c = x / 2;
            let f = if x % 2 == 0 { c.saturating_sub(1) } else { (c + 1).min(cw - 1) };
            let cb = 3 * col[c * 2] + col[f * 2];
            let cr = 3 * col[c * 2 + 1] + col[f * 2 + 1];
            dst[x * 4..x * 4 + 4].copy_from_slice(&rgb(luma[x] as i32, cb, cr));
        }
    });
    RgbaImage::from_raw(width as u32, height as u32, out).expect("buffer size")
}

#[cfg(windows)]
mod imp {
    use std::mem::ManuallyDrop;
    use std::time::{Duration, Instant};

    use image::RgbaImage;
    use windows::Win32::Foundation::{HMODULE, LUID};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::Interface;

    type Result<T> = std::result::Result<T, String>;

    fn err(context: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |e| format!("{context}: {e}")
    }

    /// Give up on a decode that takes longer than this (a wedged driver must not hang a worker).
    const TIMEOUT: Duration = Duration::from_secs(3);

    pub struct Session {
        pub name: String,
        pub index: usize,
        transform: IMFTransform,
        events: IMFMediaEventGenerator,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        _manager: IMFDXGIDeviceManager,
        /// Frame size the transform is set up for.
        configured: Option<(u32, u32)>,
        /// Outstanding METransformNeedInput requests.
        need_input: u32,
        staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    }

    // The COM objects are free-threaded (the device is multithread-protected), and a session is
    // only used by one thread at a time, behind the pool's mutex.
    unsafe impl Send for Session {}

    fn hardware_decoders() -> Result<Vec<IMFActivate>> {
        static STARTUP: std::sync::Once = std::sync::Once::new();
        STARTUP.call_once(|| unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        });
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_MJPG };
            let mut list = std::ptr::null_mut();
            let mut count = 0;
            MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, Some(&input), None, &mut list, &mut count)
                .map_err(err("MFTEnumEx"))?;
            let mut out = Vec::new();
            for i in 0..count as usize {
                if let Some(a) = std::ptr::read(list.add(i)) {
                    out.push(a);
                }
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(list as _));
            Ok(out)
        }
    }

    impl Session {
        /// The first hardware decoder that opens, preferring NVIDIA (a discrete GPU is usually
        /// fastest) unless `CYCLOPS_HWJPEG` names another vendor.
        pub fn open() -> Result<Self> {
            let decoders = hardware_decoders()?;
            let mut order: Vec<usize> = (0..decoders.len()).collect();
            let want = std::env::var("CYCLOPS_HWJPEG").unwrap_or_else(|_| "nvidia".into()).to_ascii_lowercase();
            order.sort_by_key(|&i| !friendly_name(&decoders[i]).to_ascii_lowercase().contains(&want));
            let mut last = "no hardware MJPEG decoder".to_string();
            for i in order {
                match Self::from_activate(&decoders[i], i) {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        log::info!("hardware JPEG: {} unavailable: {e}", friendly_name(&decoders[i]));
                        last = e;
                    }
                }
            }
            Err(last)
        }

        pub fn is_configured_for(&self, width: u32, height: u32) -> bool {
            self.configured == Some((width, height))
        }

        pub fn open_index(index: usize) -> Result<Self> {
            let decoders = hardware_decoders()?;
            Self::from_activate(decoders.get(index).ok_or("decoder gone")?, index)
        }

        fn from_activate(activate: &IMFActivate, index: usize) -> Result<Self> {
            unsafe {
                let name = friendly_name(activate);
                let adapter = decoder_adapter(activate)?;
                let (mut device, mut context) = (None, None);
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
                .map_err(err("D3D11 device"))?;
                let device = device.ok_or("no device")?;
                let context = context.ok_or("no context")?;
                let mt: ID3D11Multithread = device.cast().map_err(err("multithread"))?;
                let _ = mt.SetMultithreadProtected(true);
                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(err("device manager"))?;
                let manager = manager.ok_or("no manager")?;
                manager.ResetDevice(&device, token).map_err(err("reset device"))?;

                let transform: IMFTransform = activate.ActivateObject().map_err(err("activate"))?;
                let attributes = transform.GetAttributes().map_err(err("attributes"))?;
                if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
                    return Err(format!("{name} is not asynchronous"));
                }
                attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(err("unlock"))?;
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                transform
                    .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                    .map_err(err("set D3D manager"))?;
                let events: IMFMediaEventGenerator = transform.cast().map_err(err("events"))?;
                Ok(Self {
                    name,
                    index,
                    transform,
                    events,
                    device,
                    context,
                    _manager: manager,
                    configured: None,
                    need_input: 0,
                    staging: None,
                })
            }
        }

        unsafe fn set_input(&mut self, width: u32, height: u32) -> Result<()> {
            unsafe {
                let t = MFCreateMediaType().map_err(err("media type"))?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("major"))?;
                t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_MJPG).map_err(err("subtype"))?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64).map_err(err("size"))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1).map_err(err("rate"))?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(err("aspect"))?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("interlace"))?;
                self.transform.SetInputType(0, &t, 0).map_err(err("SetInputType"))
            }
        }

        unsafe fn set_output(&mut self) -> Result<()> {
            for i in 0.. {
                let Ok(t) = (unsafe { self.transform.GetOutputAvailableType(0, i) }) else { break };
                if unsafe { t.GetGUID(&MF_MT_SUBTYPE) } == Ok(MFVideoFormat_NV12) {
                    return unsafe { self.transform.SetOutputType(0, &t, 0).map_err(err("SetOutputType")) };
                }
            }
            Err("no NV12 output".into())
        }

        fn configure(&mut self, width: u32, height: u32) -> Result<()> {
            if self.configured == Some((width, height)) {
                return Ok(());
            }
            unsafe {
                if self.configured.is_some() {
                    let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                    let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                    let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
                }
                // A flush cancels outstanding input requests; new ones follow the restart.
                self.need_input = 0;
                self.configured = None;
                self.set_input(width, height)?;
                self.set_output()?;
                self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(err("begin streaming"))?;
                self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(err("start of stream"))?;
                self.configured = Some((width, height));
                Ok(())
            }
        }

        pub fn decode(&mut self, bytes: &[u8], width: u32, height: u32) -> Result<RgbaImage> {
            self.configure(width, height)?;
            let sample = unsafe { input_sample(bytes)? };
            let deadline = Instant::now() + TIMEOUT;
            let mut sent = false;
            loop {
                if !sent && self.need_input > 0 {
                    // MJPEG frames are independent: each input yields one output, no drain
                    // needed (NVIDIA's decoder stops requesting input after a drain).
                    unsafe { self.transform.ProcessInput(0, &sample, 0).map_err(err("ProcessInput"))? };
                    self.need_input -= 1;
                    sent = true;
                    continue;
                }
                let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(e) => e,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                        if Instant::now() > deadline {
                            return Err("timed out".into());
                        }
                        std::thread::sleep(Duration::from_micros(250));
                        continue;
                    }
                    Err(e) => return Err(format!("GetEvent: {e}")),
                };
                let kind = unsafe { event.GetType() }.map_err(err("event type"))?;
                let status = unsafe { event.GetStatus() }.map_err(err("event status"))?;
                if status.is_err() {
                    return Err(format!("decoder event {kind}: {status:?}"));
                }
                const NEED_INPUT: u32 = METransformNeedInput.0 as u32;
                const HAVE_OUTPUT: u32 = METransformHaveOutput.0 as u32;
                const ERROR: u32 = MEError.0 as u32;
                match kind {
                    NEED_INPUT => self.need_input += 1,
                    HAVE_OUTPUT => {
                        if let Some(img) = self.take_output(width, height)? {
                            return Ok(img);
                        }
                    }
                    ERROR => return Err("decoder error event".into()),
                    _ => {}
                }
            }
        }

        fn take_output(&mut self, width: u32, height: u32) -> Result<Option<RgbaImage>> {
            unsafe {
                let info = self.transform.GetOutputStreamInfo(0).map_err(err("stream info"))?;
                let provides = info.dwFlags
                    & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
                    != 0;
                let own = if provides {
                    None
                } else {
                    let s = MFCreateSample().map_err(err("sample"))?;
                    s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize).map_err(err("buffer"))?).map_err(err("add buffer"))?;
                    Some(s)
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(own),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut out, &mut status);
                let sample = ManuallyDrop::take(&mut out[0].pSample);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                match result {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        // The decoder wants its output type confirmed again (e.g. aligned size).
                        self.set_output()?;
                        return Ok(None);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                    Err(e) => return Err(format!("ProcessOutput: {e}")),
                }
                let buffer = sample.ok_or("no output sample")?.GetBufferByIndex(0).map_err(err("output buffer"))?;
                if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
                    let mut texture: Option<ID3D11Texture2D> = None;
                    dxgi.GetResource(&ID3D11Texture2D::IID, &mut texture as *mut _ as *mut _).map_err(err("texture"))?;
                    let texture = texture.ok_or("no texture")?;
                    let sub = dxgi.GetSubresourceIndex().map_err(err("subresource"))?;
                    self.read_texture(&texture, sub, width, height).map(Some)
                } else {
                    let b2d: IMF2DBuffer = buffer.cast().map_err(err("2D buffer"))?;
                    let mut data = std::ptr::null_mut();
                    let mut pitch = 0;
                    b2d.Lock2D(&mut data, &mut pitch).map_err(err("lock"))?;
                    // The chroma plane follows the (possibly padded) luma rows, which make up two
                    // thirds of the buffer.
                    let len = buffer.GetCurrentLength().unwrap_or(0) as usize;
                    let rows = if pitch > 0 { (len * 2 / 3) / pitch as usize } else { 0 };
                    let result = if rows >= height as usize && pitch as usize >= width as usize {
                        Ok(Some(convert(width, height, std::slice::from_raw_parts(data, len), pitch as usize, rows)))
                    } else {
                        Err("unexpected NV12 layout".to_string())
                    };
                    let _ = b2d.Unlock2D();
                    result
                }
            }
        }

        fn read_texture(&mut self, texture: &ID3D11Texture2D, sub: u32, width: u32, height: u32) -> Result<RgbaImage> {
            unsafe {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                if desc.Format != DXGI_FORMAT_NV12 || desc.Width < width || desc.Height < height {
                    return Err(format!("unexpected output {:?} {}x{}", desc.Format, desc.Width, desc.Height));
                }
                let want = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: desc.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let reuse = self.staging.as_ref().is_some_and(|(_, d)| d.Width == want.Width && d.Height == want.Height);
                if !reuse {
                    let mut t = None;
                    self.device.CreateTexture2D(&want, None, Some(&mut t)).map_err(err("staging texture"))?;
                    self.staging = Some((t.ok_or("no staging")?, want));
                }
                let staging = &self.staging.as_ref().expect("staging").0;
                self.context.CopySubresourceRegion(staging, 0, 0, 0, 0, texture, sub, None);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(err("map"))?;
                let pitch = mapped.RowPitch as usize;
                let rows = desc.Height as usize;
                let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * rows + pitch * rows.div_ceil(2));
                let image = convert(width, height, bytes, pitch, rows);
                self.context.Unmap(staging, 0);
                Ok(image)
            }
        }
    }

    /// NV12 with `rows` luma rows (the chroma plane starts after them).
    fn convert(width: u32, height: u32, bytes: &[u8], pitch: usize, rows: usize) -> RgbaImage {
        let (y, uv) = bytes.split_at(pitch * rows);
        super::convert_420(width as usize, height as usize, y, pitch, uv, pitch)
    }

    unsafe fn input_sample(bytes: &[u8]) -> Result<IMFSample> {
        unsafe {
            let len = u32::try_from(bytes.len()).map_err(|_| "file too large")?;
            let buffer = MFCreateMemoryBuffer(len).map_err(err("input buffer"))?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(err("lock input"))?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buffer.Unlock().map_err(err("unlock input"))?;
            buffer.SetCurrentLength(len).map_err(err("length"))?;
            let sample = MFCreateSample().map_err(err("input sample"))?;
            sample.AddBuffer(&buffer).map_err(err("add input"))?;
            sample.SetSampleTime(0).map_err(err("time"))?;
            sample.SetSampleDuration(333_333).map_err(err("duration"))?;
            Ok(sample)
        }
    }

    /// The GPU a hardware decoder runs on: by adapter LUID when the driver reports one, else the
    /// first adapter from the decoder's vendor ("VEN_10DE" and so on).
    unsafe fn decoder_adapter(activate: &IMFActivate) -> Result<IDXGIAdapter> {
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1().map_err(err("DXGI factory"))?;
            if let Ok(luid) = activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) {
                let luid = LUID { LowPart: luid as u32, HighPart: (luid >> 32) as i32 };
                if let Ok(adapter) = factory.EnumAdapterByLuid(luid) {
                    return Ok(adapter);
                }
            }
            let mut text = windows::core::PWSTR::null();
            let mut len = 0;
            activate.GetAllocatedString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &mut text, &mut len).map_err(err("vendor id"))?;
            let vendor = text.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(text.0 as _));
            let id = vendor
                .strip_prefix("VEN_")
                .and_then(|v| u32::from_str_radix(v.get(..4)?, 16).ok())
                .ok_or_else(|| format!("unknown vendor id {vendor:?}"))?;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                if adapter.GetDesc1().is_ok_and(|d| d.VendorId == id) {
                    return adapter.cast().map_err(err("adapter"));
                }
            }
            Err(format!("no adapter for {vendor}"))
        }
    }

    fn friendly_name(activate: &IMFActivate) -> String {
        unsafe {
            let mut name = windows::core::PWSTR::null();
            let mut len = 0;
            if activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len).is_err() {
                return "hardware MJPEG decoder".into();
            }
            let s = name.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as _));
            s
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct Session {
        pub name: String,
        pub index: usize,
    }
    impl Session {
        pub fn open() -> Result<Self, String> {
            Err("hardware JPEG decoding is Windows-only".into())
        }
        pub fn open_index(_: usize) -> Result<Self, String> {
            Self::open()
        }
        pub fn is_configured_for(&self, _: u32, _: u32) -> bool {
            false
        }
        pub fn decode(&mut self, _: &[u8], _: u32, _: u32) -> Result<image::RgbaImage, String> {
            Err("hardware JPEG decoding is Windows-only".into())
        }
    }
}

#[allow(unused_imports)]
pub use imp::Session;
