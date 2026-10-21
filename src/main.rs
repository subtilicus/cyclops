#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chrome;
mod decode;
mod diag;
mod editor;
mod fs;
mod gputex;
mod glass;
mod hdr_gpu;
mod icons;
mod loader;
mod ops;
mod perflog;
mod shell;
mod theme;
mod video;
mod wic;

use std::path::PathBuf;

fn main() -> eframe::Result {
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
        ..Default::default()
    };
    eframe::run_native(
        "Cyclops",
        options,
        Box::new(move |cc| Ok(Box::new(app::CyclopsApp::new(cc, start)))),
    )
}
