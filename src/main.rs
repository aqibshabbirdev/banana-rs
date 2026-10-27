// A window app on Windows: no console window next to it (debug builds keep one for logs).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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
mod sync;
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

    let viewport = eframe::egui::ViewportBuilder::default().with_title("Banana Photos").with_inner_size(size).with_min_inner_size([480.0, 360.0]);
    // The 🍌 in the title bar and taskbar (macOS takes it from the app bundle).
    #[cfg(not(target_os = "macos"))]
    let viewport = viewport.with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).unwrap_or_default());
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    // Windows: Direct3D 12 instead of OpenGL. Without a GPU driver (the Microsoft Store's test
    // machines, VMs, Remote Desktop) Windows only has OpenGL 1.1, so the window never opened and
    // the app quit at launch; Direct3D falls back to Windows' software renderer.
    #[cfg(windows)]
    {
        options.renderer = eframe::Renderer::Wgpu;
        options.wgpu_options = windows_wgpu();
    }
    // BANANA_RENDERER=glow|wgpu picks one (glow is the only one built in off Windows).
    match std::env::var("BANANA_RENDERER").as_deref() {
        Ok("glow") => options.renderer = eframe::Renderer::Glow,
        #[cfg(windows)]
        Ok("wgpu") => options.renderer = eframe::Renderer::Wgpu,
        _ => {}
    }

    let result = eframe::run_native("banana-rs", options, Box::new(|cc| Ok(Box::new(app::PhotosApp::new(cc, start)))));
    // A window app has nowhere to show why it couldn't start, so it's written down.
    if let Err(e) = &result {
        eprintln!("Banana Photos couldn't start: {e}");
        if let Some(dir) = store::data_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("startup-error.txt"), format!("{e}\n"));
        }
    }
    result
}

/// Direct3D 12 (or Vulkan) on a GPU when there is one, else Windows' software renderer (WARP),
/// which wgpu's own choice passes over: on a PC without a GPU driver it found nothing.
#[cfg(windows)]
fn windows_wgpu() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    use eframe::wgpu;
    let mut setup = WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
    setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        let usable: Vec<&wgpu::Adapter> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).collect();
        // A real GPU first (a discrete one before an integrated one), software last.
        let rank = |a: &&&wgpu::Adapter| match a.get_info().device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
            wgpu::DeviceType::Cpu => 3,
        };
        usable.iter().min_by_key(rank).map(|a| (*a).clone()).ok_or_else(|| {
            let found: Vec<String> = adapters.iter().map(|a| format!("{} ({:?}, {:?})", a.get_info().name, a.get_info().device_type, a.get_info().backend)).collect();
            format!("no graphics adapter can draw the window; found: {}", if found.is_empty() { "none".into() } else { found.join(", ") })
        })
    }));
    WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() }
}
