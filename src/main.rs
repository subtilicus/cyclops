#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decode;
mod fs;
mod icons;
mod loader;
mod ops;
mod shell;
mod theme;
mod video;
mod wic;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cyclops")
            .with_app_id("cyclops")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([640.0, 400.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Cyclops",
        options,
        Box::new(move |cc| Ok(Box::new(app::CyclopsApp::new(cc, start)))),
    )
}
