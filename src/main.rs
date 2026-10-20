#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chrome;
mod decode;
mod editor;
mod fs;
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
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    // Present through DirectComposition so the window can be translucent (Mica/Acrylic).
    // Plain HWND swapchains can't do alpha. Respect an explicit override.
    if std::env::var_os("WGPU_DX12_PRESENTATION_SYSTEM").is_none() {
        // SAFETY: single-threaded at this point, before any other thread or the renderer starts.
        unsafe { std::env::set_var("WGPU_DX12_PRESENTATION_SYSTEM", "Visual") };
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cyclops")
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
