//! Background image decoding: small cached thumbnails for the grid, full-size images for the viewer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui::{self, ColorImage};
use image::{DynamicImage, ImageReader};

pub const THUMB_SIZE: u32 = 320;
const FULL_MAX: u32 = 4096;

pub struct Job {
    pub gen: u64,
    pub id: usize,
    pub path: PathBuf,
    /// Cache key derived from path, mtime and size, so edited files get fresh thumbnails.
    pub key: u64,
}

pub struct Loaded {
    pub gen: u64,
    pub id: usize,
    pub image: Option<ColorImage>,
}

#[derive(Default)]
struct Queue {
    /// Workers pop from the back, so the most important jobs go last.
    jobs: Vec<Job>,
    in_flight: HashSet<(u64, usize)>,
}

pub struct ThumbLoader {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    pub rx: Receiver<Loaded>,
}

impl ThumbLoader {
    pub fn new(ctx: egui::Context) -> Self {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (tx, rx) = channel();
        let cache = cache_dir();
        let _ = std::fs::create_dir_all(&cache);

        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).max(2);
        for _ in 0..workers {
            let shared = shared.clone();
            let tx: Sender<Loaded> = tx.clone();
            let ctx = ctx.clone();
            let cache = cache.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let (lock, cv) = &*shared;
                    let mut q = lock.lock().unwrap();
                    loop {
                        if let Some(job) = q.jobs.pop() {
                            q.in_flight.insert((job.gen, job.id));
                            break job;
                        }
                        q = cv.wait(q).unwrap();
                    }
                };

                let image = load_thumb(&job.path, &cache.join(format!("{:016x}.jpg", job.key)));
                shared.0.lock().unwrap().in_flight.remove(&(job.gen, job.id));
                if tx.send(Loaded { gen: job.gen, id: job.id, image }).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }

        Self { shared, rx }
    }

    /// Replaces the queue with exactly these jobs (least important first). Called every frame
    /// with whatever is on screen, so photos that were scrolled past are never decoded.
    pub fn set_wanted(&self, jobs: Vec<Job>) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let in_flight = &q.in_flight;
        let jobs: Vec<Job> = jobs.into_iter().filter(|j| !in_flight.contains(&(j.gen, j.id))).collect();
        let any = !jobs.is_empty();
        q.jobs = jobs;
        if any {
            cv.notify_all();
        }
    }
}

pub fn spawn_full(path: PathBuf, gen: u64, id: usize, tx: Sender<Loaded>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let image = decode(&path, FULL_MAX);
        let _ = tx.send(Loaded { gen, id, image: image.map(to_color_image) });
        ctx.request_repaint();
    });
}

pub fn cache_key(path: &Path, mtime: u64, size: u64) -> u64 {
    // FNV-1a: stable across runs and Rust versions, unlike DefaultHasher.
    let mut h: u64 = 0xcbf29ce484222325;
    let bytes = path.to_string_lossy();
    for b in bytes.as_bytes().iter().chain(&mtime.to_le_bytes()).chain(&size.to_le_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("banana-rs").join("thumbs")
}

fn load_thumb(path: &Path, cached: &Path) -> Option<ColorImage> {
    if let Ok(img) = image::open(cached) {
        return Some(to_color_image(img));
    }
    let thumb = if crate::video::is_video(path) {
        crate::video::thumbnail(path, THUMB_SIZE)?
    } else {
        decode(path, THUMB_SIZE)?
    };
    let _ = thumb.to_rgb8().save_with_format(cached, image::ImageFormat::Jpeg);
    Some(to_color_image(thumb))
}

/// Decodes upright (EXIF rotation applied, so phone photos are not sideways) and no bigger
/// than `max_side` on its longest edge.
pub fn decode(path: &Path, max_side: u32) -> Option<DynamicImage> {
    #[cfg(target_os = "macos")]
    if crate::library::is_heif(path) {
        return crate::imageio::decode(path, max_side);
    }
    let img = decode_any(path)?;
    Some(if img.width() > max_side || img.height() > max_side {
        if max_side <= THUMB_SIZE {
            img.thumbnail(max_side, max_side)
        } else {
            img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
        }
    } else {
        img
    })
}

/// Decodes an image held in memory (upright, at most `max_side`), for Locked Folder.
pub fn decode_bytes(bytes: &[u8], name: &Path, max_side: u32) -> Option<DynamicImage> {
    use image::ImageDecoder;
    #[cfg(target_os = "macos")]
    if crate::library::is_heif(name) {
        return crate::imageio::decode_data(bytes, max_side);
    }
    let _ = name;
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(if img.width() > max_side || img.height() > max_side {
        img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
    } else {
        img
    })
}

/// A small JPEG of a photo or video frame, kept in memory (Locked Folder stores it encrypted).
pub fn thumbnail_jpeg(path: &Path) -> Option<Vec<u8>> {
    let img = if crate::video::is_video(path) { crate::video::thumbnail(path, THUMB_SIZE)? } else { decode(path, THUMB_SIZE)? };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8()).ok()?;
    Some(out)
}

/// Removes the cached thumbnail of a photo (when it moves into Locked Folder).
pub fn forget_cached(path: &Path, mtime: u64, size: u64) {
    let _ = std::fs::remove_file(cache_dir().join(format!("{:016x}.jpg", cache_key(path, mtime, size))));
}

fn decode_any(path: &Path) -> Option<DynamicImage> {
    use image::ImageDecoder;
    let mut decoder = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(img)
}

fn to_color_image(img: DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
}
