//! Videos: metadata, poster-frame thumbnails and in-app playback.
//!
//! macOS uses AVFoundation (hardware decoding, sound, nothing to install). Windows reads MP4/MOV
//! files itself and makes thumbnails with Media Foundation. Elsewhere GStreamer is used when
//! built with the `video` feature. Without a backend, videos still show up in the
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
#[cfg(all(not(any(target_os = "macos", feature = "video")), not(target_os = "windows")))]
use none as backend;

// Windows: length and size from the file itself, thumbnails through Media Foundation, and
// playback in the default player.
#[cfg(all(target_os = "windows", not(feature = "video")))]
mod win;
#[cfg(all(target_os = "windows", not(feature = "video")))]
use win as backend;

pub use backend::{probe, thumbnail, Player};

pub const EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi", "3gp", "mts", "m2ts", "wmv"];

pub struct VideoInfo {
    /// Display size, i.e. after the rotation phones store in the file.
    pub width: u32,
    pub height: u32,
    pub duration: f32,
    /// When the camera recorded it, if the file says.
    pub created: Option<NaiveDateTime>,
    /// Where (latitude, longitude), if the file says.
    pub location: Option<(f32, f32)>,
}

/// The `moov` box of an MP4/MOV: the movie's index (tracks, length, metadata), usually a few KB
/// at its start or end.
fn read_moov(path: &Path) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut at = 0u64;
    // Top-level boxes: size (u32, 1 = a u64 follows, 0 = to the end) and type.
    while at + 8 <= len {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut head = [0u8; 16];
        f.read_exact(&mut head[..8]).ok()?;
        let (mut size, kind) = (u32::from_be_bytes(head[..4].try_into().ok()?) as u64, [head[4], head[5], head[6], head[7]]);
        let mut header = 8;
        if size == 1 {
            f.read_exact(&mut head[8..16]).ok()?;
            size = u64::from_be_bytes(head[8..16].try_into().ok()?);
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header {
            return None;
        }
        if &kind == b"moov" {
            // A broken size isn't read whole.
            let mut moov = vec![0u8; (size - header).min(64 << 20) as usize];
            f.read_exact(&mut moov).ok()?;
            return Some(moov);
        }
        at += size;
    }
    None
}

/// Where an MP4/MOV was recorded, read from its `moov` box: the ISO 6709 string phones store
/// under `com.apple.quicktime.location.ISO6709` (iPhone) or in a `©xyz` box (Android). For when
/// the video backend doesn't report it (GStreamer doesn't read the first kind).
pub fn location_from_file(path: &Path) -> Option<(f32, f32)> {
    location_in(&read_moov(path)?)
}

/// The boxes directly inside `buf`, as (type, contents).
fn boxes(mut buf: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if buf.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes(buf[..4].try_into().ok()?) as usize;
        let kind: [u8; 4] = buf[4..8].try_into().ok()?;
        let (header, size) = match size {
            0 => (8, buf.len()),
            1 => (16, u64::from_be_bytes(buf.get(8..16)?.try_into().ok()?) as usize),
            n => (8, n),
        };
        if size < header || size > buf.len() {
            return None;
        }
        let body = &buf[header..size];
        buf = &buf[size..];
        Some((kind, body))
    })
}

/// Length, picture size (upright) and recording time of an MP4/MOV, from its `moov` box alone:
/// for Windows, where the video framework is only used for thumbnails.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn probe_mp4(path: &Path) -> Option<VideoInfo> {
    let moov = read_moov(path)?;
    let be32 = |b: &[u8], at: usize| b.get(at..at + 4).map(|x| u32::from_be_bytes(x.try_into().unwrap()));
    let be64 = |b: &[u8], at: usize| b.get(at..at + 8).map(|x| u64::from_be_bytes(x.try_into().unwrap()));
    // mvhd: version, then (32 or 64-bit) creation and modification times, timescale, duration.
    let (_, mvhd) = boxes(&moov).find(|(k, _)| k == b"mvhd")?;
    let (created, timescale, duration) = if mvhd.first() == Some(&1) {
        (be64(mvhd, 4)?, be32(mvhd, 20)?, be64(mvhd, 24)?)
    } else {
        (be32(mvhd, 4)? as u64, be32(mvhd, 12)?, be32(mvhd, 16)? as u64)
    };
    // The video track: the trak whose mdia/hdlr says "vide"; its tkhd has the size and matrix.
    let tkhd = boxes(&moov).filter(|(k, _)| k == b"trak").find_map(|(_, trak)| {
        let (_, mdia) = boxes(trak).find(|(k, _)| k == b"mdia")?;
        let (_, hdlr) = boxes(mdia).find(|(k, _)| k == b"hdlr")?;
        (hdlr.get(8..12)? == b"vide").then(|| boxes(trak).find(|(k, _)| k == b"tkhd").map(|(_, t)| t)).flatten()
    })?;
    let (w, h, turn) = track_shape(tkhd)?;
    let (width, height) = if turn % 180 == 90 { (h, w) } else { (w, h) };
    // iPhones say when recording started ("2025-08-31T19:53:21+0500"); mvhd's time (seconds
    // since 1904, UTC; 0 when unknown) can be when it was saved.
    let created = creationdate_in(&moov).or((created > 2_082_844_800)
        .then(|| chrono::DateTime::from_timestamp(created as i64 - 2_082_844_800, 0))
        .flatten()
        .map(|utc| utc.with_timezone(&chrono::Local).naive_local()));
    let duration = if timescale > 0 { duration as f32 / timescale as f32 } else { 0.0 };
    Some(VideoInfo { width, height, duration, created, location: location_in(&moov) })
}

/// The video track's stored size, and how far it's turned clockwise for showing (0, 90, 180,
/// 270), from its `tkhd`.
fn track_shape(tkhd: &[u8]) -> Option<(u32, u32, u32)> {
    let be32 = |at: usize| tkhd.get(at..at + 4).map(|x| u32::from_be_bytes(x.try_into().unwrap()));
    let at = if tkhd.first() == Some(&1) { 4 + 32 } else { 4 + 20 };
    // After times, ids and duration: reserved (8), layer, group, volume, reserved (8), matrix (36).
    let matrix = at + 8 + 8;
    let (a, b) = (be32(matrix)? as i32, be32(matrix + 4)? as i32);
    let (w, h) = (be32(matrix + 36)? >> 16, be32(matrix + 40)? >> 16);
    // (a b) is (0 1) for a quarter turn clockwise, (0 -1) the other way, (-1 0) upside down.
    let turn = match (a, b) {
        (0, b) if b > 0 => 90,
        (0, b) if b < 0 => 270,
        (a, 0) if a < 0 => 180,
        _ => 0,
    };
    Some((w, h, turn))
}

/// How far an MP4/MOV's picture is turned clockwise for showing.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn turn_mp4(path: &Path) -> u32 {
    (|| {
        let moov = read_moov(path)?;
        let turn = boxes(&moov).filter(|(k, _)| k == b"trak").find_map(|(_, trak)| {
            let (_, mdia) = boxes(trak).find(|(k, _)| k == b"mdia")?;
            let (_, hdlr) = boxes(mdia).find(|(k, _)| k == b"hdlr")?;
            if hdlr.get(8..12)? != b"vide" {
                return None;
            }
            track_shape(boxes(trak).find(|(k, _)| k == b"tkhd")?.1).map(|(_, _, t)| t)
        });
        turn
    })()
    .unwrap_or(0)
}

/// The `com.apple.quicktime.creationdate` value, as local time.
fn creationdate_in(moov: &[u8]) -> Option<chrono::NaiveDateTime> {
    let key = b"quicktime.creationdate";
    let from = moov.windows(key.len()).position(|w| w == key)? + key.len();
    let rest = &moov[from..];
    // The first "YYYY-MM-DDTHH:MM:SS±hhmm" from there on (the values come after the keys).
    (0..=rest.len().saturating_sub(24)).find_map(|i| {
        let text = std::str::from_utf8(rest.get(i..i + 24)?).ok()?;
        chrono::DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%z").ok()
    })
    .map(|t| t.with_timezone(&chrono::Local).naive_local())
}

fn location_in(moov: &[u8]) -> Option<(f32, f32)> {
    let find = |needle: &[u8]| moov.windows(needle.len()).position(|w| w == needle);
    // The string comes after its key (in `ilst`, after the list of keys) or right after the
    // `©xyz` box header: the first thing from there on that reads as a point.
    let from = find(b"location.ISO6709").or_else(|| find(b"\xa9xyz"))?;
    let rest = &moov[from..];
    (0..rest.len()).filter(|&i| rest[i] == b'+' || rest[i] == b'-').find_map(|i| {
        let text: String = rest[i..].iter().take(40).take_while(|&&c| c.is_ascii_digit() || b"+-./".contains(&c)).map(|&c| c as char).collect();
        parse_iso6709(&text)
    })
}

/// An ISO 6709 point as phones write it in videos: "+33.8912+073.4472+512.000/".
pub fn parse_iso6709(s: &str) -> Option<(f32, f32)> {
    let s = s.trim().trim_end_matches('/');
    // Each number starts at a sign: latitude, longitude, then maybe altitude.
    let starts: Vec<usize> = s.char_indices().filter(|(_, c)| *c == '+' || *c == '-').map(|(i, _)| i).collect();
    let (&a, &b) = (starts.first()?, starts.get(1)?);
    let lat: f32 = s.get(a..b)?.parse().ok()?;
    let lon: f32 = s.get(b..*starts.get(2).unwrap_or(&s.len()))?.parse().ok()?;
    (lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0)).then_some((lat, lon))
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
    fn video_locations() {
        assert_eq!(super::parse_iso6709("+33.6303+073.1222+512.000/"), Some((33.6303, 73.1222)));
        assert_eq!(super::parse_iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(super::parse_iso6709("+00.0000+000.0000/"), None);
        // As in an iPhone's moov: the key list, binary sizes (with '+' and '-' bytes), then the value.
        let mut moov = b"keys\0\0\0\x2dmdtacom.apple.quicktime.location.ISO6709\0\0\0\x2bmdtacom.apple.quicktime.make".to_vec();
        moov.extend_from_slice(b"ilst\0\0\0\x2d\0\0\0\x01\0\0\0\x25data\0\0\0\x01\0\0\0\0+33.6303+073.1222+512.000/");
        assert_eq!(super::location_in(&moov), Some((33.6303, 73.1222)));
        assert_eq!(super::location_in(b"moov without one"), None);
    }

    /// Video thumbnails come out upright on every platform: tests/data/turned.mp4 is wide.mp4
    /// (blue left half, red right half) stored turned a quarter, as phones do.
    #[test]
    fn thumbnails_come_upright() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let blue = |img: &image::DynamicImage, x: u32, y: u32| {
            let p = img.to_rgb8().get_pixel(x, y).0;
            p[2] > 150 && p[0] < 100
        };
        assert_eq!((super::turn_mp4(&data.join("wide.mp4")), super::turn_mp4(&data.join("turned.mp4"))), (0, 270));
        let info = super::probe_mp4(&data.join("turned.mp4")).unwrap();
        assert_eq!((info.width, info.height), (64, 96));
        let Some(wide) = super::thumbnail(&data.join("wide.mp4"), 200) else {
            eprintln!("no video backend in this build");
            return;
        };
        assert!(wide.width() > wide.height() && blue(&wide, wide.width() / 4, wide.height() / 2));
        let turned = super::thumbnail(&data.join("turned.mp4"), 200).unwrap();
        // Turned back upright, blue (the left of the picture as filmed) ends up at the bottom.
        assert!(turned.height() > turned.width());
        assert!(blue(&turned, turned.width() / 2, turned.height() * 3 / 4), "blue isn't at the bottom");
    }

    /// Reading an MP4/MOV's index ourselves gives what the video framework does. By hand, with
    /// a phone video: BANANA_TEST_VIDEO=IMG_1234.MOV cargo test --release probe_mp4 -- --ignored
    #[test]
    #[ignore]
    fn probe_mp4_matches_the_backend() {
        let src = std::path::PathBuf::from(std::env::var("BANANA_TEST_VIDEO").expect("BANANA_TEST_VIDEO"));
        let (a, b) = (super::probe(&src).unwrap(), super::probe_mp4(&src).unwrap());
        println!("backend {}x{} {:.2}s {:?} · ours {}x{} {:.2}s {:?}", a.width, a.height, a.duration, a.created, b.width, b.height, b.duration, b.created);
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert!((a.duration - b.duration).abs() < 0.1);
        assert_eq!(a.created, b.created);
    }

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
