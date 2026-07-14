#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod annotations;
mod app;
mod bridge;
mod catalog;
mod export;
mod plot_ui;
mod protocol;
mod render;
mod scene;

use app::ViewerApp;

fn main() -> eframe::Result {
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("BATSView")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1080.0, 680.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "BATSView",
        options,
        Box::new(move |context| Ok(Box::new(ViewerApp::new(context, initial_path)))),
    )
}
