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

/// Makes an H.264/AAC MP4, 1080p at most, that any phone plays: for videos some phones can't
/// (HEVC, Dolby Vision, AVI, MKV…). `progress` gets 0–1. `dst` appears only once it's complete.
pub fn convert_for_phone(src: &Path, dst: &Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    let part = dst.with_extension("part.mp4");
    let _ = std::fs::remove_file(&part);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    match convert(src, &part, progress) {
        Ok(()) => std::fs::rename(&part, dst).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(target_os = "macos")]
use mac::convert;

/// With ffmpeg, where it's installed (it isn't part of the app).
#[cfg(not(target_os = "macos"))]
fn convert(src: &Path, dst: &Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};
    let duration = probe(src).map(|i| i.duration as f64).filter(|d| *d > 0.0);
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-i"])
        .arg(src)
        .args(["-map", "0:v:0", "-map", "0:a:0?"])
        // Long side at most 1920 (portrait or landscape), even sizes, 8-bit 4:2:0 for every decoder.
        .args(["-vf", "scale=w='if(gt(iw,ih),min(1920,iw),-2)':h='if(gt(iw,ih),-2,min(1920,ih))',format=yuv420p"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"])
        .arg(dst)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "ffmpeg isn't installed on the computer (sudo apt install ffmpeg)".to_string(),
            _ => e.to_string(),
        })?;
    if let (Some(out), Some(total)) = (child.stdout.take(), duration) {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                progress((us / 1e6 / total).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    match child.wait() {
        Ok(s) if s.success() => Ok(()),
        _ => Err(err.lines().last().unwrap_or("ffmpeg failed").to_string()),
    }
}

/// Hands the file to the system's default app (QuickTime, VLC, …).
pub fn open_externally(path: &Path) {
    #[cfg(target_os = "macos")]
    let cmd = crate::access::open_file(path);
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let cmd = std::process::Command::new("xdg-open").arg(path).spawn();
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    /// Converts the video in BANANA_TEST_VIDEO (say an HEVC one from an iPhone) and checks the
    /// result. Needs AVFoundation or ffmpeg, so it's run by hand:
    ///   BANANA_TEST_VIDEO=clip.mov cargo test --release convert_for_phone -- --ignored
    #[test]
    #[ignore]
    fn convert_for_phone() {
        let src = std::path::PathBuf::from(std::env::var("BANANA_TEST_VIDEO").expect("BANANA_TEST_VIDEO"));
        let dst = std::env::temp_dir().join(format!("banana-convert-{}.mp4", std::process::id()));
        let last = std::sync::Mutex::new(0.0f32);
        super::convert_for_phone(&src, &dst, &|p| *last.lock().unwrap() = p).unwrap();
        let (a, b) = (super::probe(&src).unwrap(), super::probe(&dst).unwrap());
        assert!((a.duration - b.duration).abs() < 0.5, "{} vs {}", a.duration, b.duration);
        assert!(b.width.max(b.height) <= 1920);
        // Upright like the original (phones store rotation in the file).
        assert_eq!(a.width > a.height, b.width > b.height);
        assert!(*last.lock().unwrap() > 0.0);
        let _ = std::fs::remove_file(dst);
    }
}
