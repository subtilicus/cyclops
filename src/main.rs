#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about_fx;
mod app;
mod basis;
mod chrome;
mod clipboard;
mod decode;
mod diag;
mod editor;
mod fs;
mod gputex;
mod glass;
mod gpr;
mod hdr_gpu;
mod hwjpeg;
mod icons;
mod libraw;
mod loader;
mod ops;
mod perflog;
mod shell;
mod theme;
mod video;
mod wic;

use std::path::PathBuf;

/// eframe's defaults, but with the largest textures the GPU supports (egui-wgpu asks for 8192,
/// which would show large scans and panoramas downscaled even at 100%).
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, wgpu};
    let mut options = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(create) = &mut options.wgpu_setup {
        create.device_descriptor = std::sync::Arc::new(|adapter| wgpu::DeviceDescriptor {
            label: Some("cyclops"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d.max(8192),
                ..wgpu::Limits::default()
            },
            ..Default::default()
        });
    }
    options
}

fn main() -> eframe::Result {
    perflog::startup("main");
    diag::init();
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    // Windows: render with DirectX 12 and present through DirectComposition, the only
    // combination whose swapchain supports alpha, which the translucent Mica/Acrylic window
    // needs. (wgpu otherwise often picks Vulkan, whose Windows surfaces are opaque.)
    // Explicit overrides in the environment are respected.
    #[cfg(windows)]
    for (key, value) in [("WGPU_BACKEND", "dx12"), ("WGPU_DX12_PRESENTATION_SYSTEM", "Visual")] {
        if std::env::var_os(key).is_none() {
            // SAFETY: single-threaded at this point, before any other thread or the renderer starts.
            unsafe { std::env::set_var(key, value) };
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cyclops Image Browser")
            .with_app_id("cyclops")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([640.0, 400.0])
            .with_transparent(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    // Windows takes the icon from the exe's resources (see `chrome::style`). An empty icon keeps
    // eframe from setting its default egui logo over it later.
    #[cfg(windows)]
    let options = eframe::NativeOptions { viewport: options.viewport.with_icon(egui::IconData::default()), ..options };
    #[cfg(not(windows))]
    let options = eframe::NativeOptions {
        viewport: options.viewport.with_icon(std::sync::Arc::new(
            eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png")).expect("bundled icon"),
        )),
        ..options
    };
    perflog::startup("creating window");
    eframe::run_native(
        "Cyclops",
        options,
        Box::new(move |cc| Ok(Box::new(app::CyclopsApp::new(cc, start)))),
    )
}
