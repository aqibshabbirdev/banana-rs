mod app;
#[cfg(target_os = "macos")]
mod imageio;
mod library;
mod people;
mod store;
mod thumbs;
mod transfer;
mod vault;
mod video;

use std::path::PathBuf;

fn main() -> eframe::Result {
    // Teaches the `image` crate to open HEIC/HEIF/AVIF. (macOS uses ImageIO instead, see imageio.rs.)
    #[cfg(all(feature = "heic", not(target_os = "macos")))]
    libheif_rs::integration::image::register_all_decoding_hooks();

    // `banana-rs ~/Pictures` opens that folder; otherwise the last folder, then the Pictures folder.
    let start = std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_dir());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Banana Photos")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };

    eframe::run_native("banana-rs", options, Box::new(|cc| Ok(Box::new(app::PhotosApp::new(cc, start)))))
}
