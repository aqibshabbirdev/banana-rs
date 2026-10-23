mod access;
mod app;
mod clip;
mod devices;
mod duplicates;
#[cfg(target_os = "macos")]
mod imageio;
mod library;
#[cfg(target_os = "macos")]
mod menubar;
mod people;
mod places;
mod remote;
mod store;
mod thumbs;
mod transfer;
mod vault;
mod video;

use std::path::PathBuf;

fn main() -> eframe::Result {
    // glibc keeps the big buffers of decoded photos in per-thread heaps once its moving mmap
    // threshold has grown, so analysing a library climbed to gigabytes. A fixed threshold makes
    // them come from (and go back to) the system; fewer heaps cut the rest.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 32 << 20);
        libc::mallopt(libc::M_ARENA_MAX, 4);
    }

    // Teaches the `image` crate to open HEIC/HEIF/AVIF. (macOS uses ImageIO instead, see imageio.rs.)
    #[cfg(all(feature = "heic", not(target_os = "macos")))]
    libheif_rs::integration::image::register_all_decoding_hooks();

    // `banana-rs ~/Pictures` opens that folder; otherwise the last folder, then the Pictures folder.
    let start = std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_dir());

    // Dev-only: BANANA_WINDOW=1440x900 sets the window size (for App Store screenshots).
    #[cfg(debug_assertions)]
    let size = std::env::var("BANANA_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or([1200.0, 800.0]);
    #[cfg(not(debug_assertions))]
    let size = [1200.0, 800.0];

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Banana Photos")
            .with_inner_size(size)
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };

    eframe::run_native("banana-rs", options, Box::new(|cc| Ok(Box::new(app::PhotosApp::new(cc, start)))))
}
