//! Videos: metadata, poster-frame thumbnails and in-app playback.
//!
//! macOS uses AVFoundation (hardware decoding, sound, nothing to install). Elsewhere GStreamer
//! is used when built with the `video` feature. Without a backend, videos still show up in the
//! library and open in the system's default player.

use std::path::Path;

use chrono::NaiveDateTime;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as backend;

#[cfg(all(not(target_os = "macos"), feature = "video"))]
mod gst;
#[cfg(all(not(target_os = "macos"), feature = "video"))]
use gst as backend;

#[cfg(not(any(target_os = "macos", feature = "video")))]
mod none;
#[cfg(not(any(target_os = "macos", feature = "video")))]
use none as backend;

pub use backend::{probe, thumbnail, Player};

pub const EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi", "3gp", "mts", "m2ts", "wmv"];

pub struct VideoInfo {
    /// Display size, i.e. after the rotation phones store in the file.
    pub width: u32,
    pub height: u32,
    pub duration: f32,
    /// When the camera recorded it, if the file says.
    pub created: Option<NaiveDateTime>,
}

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// "0:07", "4:32", "1:02:05".
pub fn format_duration(secs: f64) -> String {
    let s = if secs.is_finite() { secs.max(0.0).round() as u64 } else { 0 };
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Hands the file to the system's default app (QuickTime, VLC, …).
pub fn open_externally(path: &Path) {
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let cmd = std::process::Command::new("xdg-open").arg(path).spawn();
    let _ = cmd;
}
