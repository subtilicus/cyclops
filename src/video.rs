//! Video playback through the Windows Media Foundation media engine.
//!
//! Decoding (hardware accelerated), audio output and A/V sync are all done by the OS, using
//! the codecs that ship with Windows, so Cyclops bundles no codecs and carries no codec
//! licensing obligations. Each new frame is rendered into a D3D11 texture by the engine and
//! copied into an egui texture.

pub fn format_time(seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds.max(0.0) as u64 } else { 0 };
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

pub use imp::VideoPlayer;

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
    use windows::Win32::Foundation::{HMODULE, RECT, S_OK};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::{BSTR, Interface, implement};

    #[derive(Default)]
    struct Shared {
        loaded: AtomicBool,
        error: AtomicBool,
    }

    #[implement(IMFMediaEngineNotify)]
    struct Notify {
        shared: Arc<Shared>,
        ctx: egui::Context,
    }

    impl IMFMediaEngineNotify_Impl for Notify_Impl {
        fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
            match MF_MEDIA_ENGINE_EVENT(event as i32) {
                MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => self.shared.loaded.store(true, Ordering::Relaxed),
                MF_MEDIA_ENGINE_EVENT_ERROR => self.shared.error.store(true, Ordering::Relaxed),
                _ => {}
            }
            self.ctx.request_repaint();
            Ok(())
        }
    }

    struct Surfaces {
        size: [u32; 2],
        target: ID3D11Texture2D,
        staging: ID3D11Texture2D,
    }

    pub struct VideoPlayer {
        engine: IMFMediaEngine,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        shared: Arc<Shared>,
        surfaces: Option<Surfaces>,
        texture: Option<TextureHandle>,
        pixels: Vec<Color32>,
    }

    fn startup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        });
    }

    fn create_device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
        let mut last_err = None;
        // Video support lets the engine decode on the GPU; fall back without it on odd drivers.
        for flags in [D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_CREATE_DEVICE_BGRA_SUPPORT] {
            let (mut device, mut context) = (None, None);
            let result = unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (result, device, context) {
                (Ok(()), Some(d), Some(c)) => return Ok((d, c)),
                (Err(e), ..) => last_err = Some(e),
                _ => {}
            }
        }
        Err(last_err.unwrap_or_else(windows::core::Error::empty))
    }

    impl VideoPlayer {
        pub fn open(path: &Path, ctx: &egui::Context, muted: bool) -> windows::core::Result<Self> {
            startup();
            let (device, context) = create_device()?;
            unsafe {
                let mt: ID3D11Multithread = device.cast()?;
                let _ = mt.SetMultithreadProtected(true);

                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                let manager = manager.ok_or_else(windows::core::Error::empty)?;
                manager.ResetDevice(&device, token)?;

                let shared = Arc::new(Shared::default());
                let notify: IMFMediaEngineNotify = Notify { shared: shared.clone(), ctx: ctx.clone() }.into();
                let mut attributes = None;
                MFCreateAttributes(&mut attributes, 3)?;
                let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
                attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
                attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)?;
                attributes.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32)?;

                let factory: IMFMediaEngineClassFactory =
                    CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
                let engine = factory.CreateInstance(0, &attributes)?;
                engine.SetMuted(muted)?;
                engine.SetAutoPlay(true)?;
                engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref()))?;

                Ok(Self { engine, device, context, shared, surfaces: None, texture: None, pixels: Vec::new() })
            }
        }

        pub fn failed(&self) -> bool {
            self.shared.error.load(Ordering::Relaxed)
        }

        pub fn size(&self) -> Option<[u32; 2]> {
            if !self.shared.loaded.load(Ordering::Relaxed) {
                return None;
            }
            let (mut w, mut h) = (0u32, 0u32);
            unsafe { self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).ok()? };
            (w > 0 && h > 0).then_some([w, h])
        }

        pub fn texture(&self) -> Option<&TextureHandle> {
            self.texture.as_ref()
        }

        pub fn is_playing(&self) -> bool {
            unsafe { !self.engine.IsPaused().as_bool() && !self.engine.IsEnded().as_bool() }
        }

        pub fn toggle(&mut self) {
            unsafe {
                if self.is_playing() {
                    let _ = self.engine.Pause();
                } else {
                    if self.engine.IsEnded().as_bool() {
                        let _ = self.engine.SetCurrentTime(0.0);
                    }
                    let _ = self.engine.Play();
                }
            }
        }

        pub fn position(&self) -> f64 {
            unsafe { self.engine.GetCurrentTime() }
        }

        pub fn duration(&self) -> f64 {
            let d = unsafe { self.engine.GetDuration() };
            if d.is_finite() { d } else { 0.0 }
        }

        pub fn seek(&mut self, seconds: f64) {
            let _ = unsafe { self.engine.SetCurrentTime(seconds.clamp(0.0, self.duration())) };
        }

        pub fn muted(&self) -> bool {
            unsafe { self.engine.GetMuted().as_bool() }
        }

        pub fn set_muted(&mut self, muted: bool) {
            let _ = unsafe { self.engine.SetMuted(muted) };
        }

        /// Pull the latest decoded frame (if a new one is due) into the egui texture.
        pub fn update(&mut self, ctx: &egui::Context) {
            let Some([w, h]) = self.size() else { return };
            // OnVideoStreamTick returns S_FALSE when there is no new frame; the Result
            // wrapper hides that, so call through the vtable to see the raw HRESULT.
            let mut pts = 0i64;
            let hr = unsafe { (Interface::vtable(&self.engine).OnVideoStreamTick)(Interface::as_raw(&self.engine), &mut pts) };
            if hr != S_OK {
                return;
            }
            if self.surfaces.as_ref().is_none_or(|s| s.size != [w, h]) {
                match self.create_surfaces(w, h) {
                    Ok(s) => self.surfaces = Some(s),
                    Err(_) => return,
                }
            }
            let Some(surfaces) = &self.surfaces else { return };
            unsafe {
                let dst = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
                let black = MFARGB { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbAlpha: 255 };
                if self.engine.TransferVideoFrame(&surfaces.target, None, &dst, Some(&black)).is_err() {
                    return;
                }
                self.context.CopyResource(&surfaces.staging, &surfaces.target);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if self.context.Map(&surfaces.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).is_err() {
                    return;
                }
                let (w, h) = (w as usize, h as usize);
                self.pixels.clear();
                self.pixels.reserve(w * h);
                for y in 0..h {
                    let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 4);
                    self.pixels.extend(row.chunks_exact(4).map(|p| Color32::from_rgb(p[0], p[1], p[2])));
                }
                self.context.Unmap(&surfaces.staging, 0);
                let image = ColorImage::new([w, h], std::mem::take(&mut self.pixels));
                match &mut self.texture {
                    Some(t) => t.set(image, TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("video", image, TextureOptions::LINEAR)),
                }
            }
        }

        fn create_surfaces(&self, w: u32, h: u32) -> windows::core::Result<Surfaces> {
            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let (mut target, mut staging) = (None, None);
            unsafe {
                self.device.CreateTexture2D(&desc, None, Some(&mut target))?;
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            }
            let err = windows::core::Error::empty;
            Ok(Surfaces { size: [w, h], target: target.ok_or_else(err)?, staging: staging.ok_or_else(err)? })
        }
    }

    impl Drop for VideoPlayer {
        fn drop(&mut self) {
            unsafe {
                let _ = self.engine.Shutdown();
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use egui::TextureHandle;

    /// Video playback is only implemented on Windows for now.
    pub struct VideoPlayer;

    impl VideoPlayer {
        pub fn open(_path: &Path, _ctx: &egui::Context, _muted: bool) -> Result<Self, String> {
            Err("video playback is not supported on this platform yet".into())
        }
        pub fn failed(&self) -> bool {
            true
        }
        pub fn size(&self) -> Option<[u32; 2]> {
            None
        }
        pub fn texture(&self) -> Option<&TextureHandle> {
            None
        }
        pub fn is_playing(&self) -> bool {
            false
        }
        pub fn toggle(&mut self) {}
        pub fn position(&self) -> f64 {
            0.0
        }
        pub fn duration(&self) -> f64 {
            0.0
        }
        pub fn seek(&mut self, _seconds: f64) {}
        pub fn muted(&self) -> bool {
            false
        }
        pub fn set_muted(&mut self, _muted: bool) {}
        pub fn update(&mut self, _ctx: &egui::Context) {}
    }
}

