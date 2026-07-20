#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod annotations;
mod app;
mod bridge;
mod camera3d;
mod catalog;
mod export;
mod loader;
mod plot_ui;
mod probe;
mod protocol;
mod render;
mod render3d;
mod scene;
mod streamlines;

use app::ViewerApp;

fn app_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../packaging/icons/batsview.png"))
        .expect("the embedded BATSView icon must be a valid PNG")
}

fn main() -> eframe::Result {
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("BATSView")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1080.0, 680.0])
            .with_icon(app_icon()),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 24,
        ..Default::default()
    };
    eframe::run_native(
        "BATSView",
        options,
        Box::new(move |context| Ok(Box::new(ViewerApp::new(context, initial_path)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_app_icon_is_valid_and_full_resolution() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }
}
