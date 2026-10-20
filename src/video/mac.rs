//! macOS video via AVFoundation: AVAssetImageGenerator for thumbnails, AVPlayer (which also
//! plays the sound) with an AVPlayerItemVideoOutput we pull frames from each UI frame.
#![allow(deprecated)] // The synchronous AVAsset getters are deprecated but fine off the UI thread.

use std::path::Path;

use chrono::{DateTime, Local};
use eframe::egui::{self, ColorImage};
use image::DynamicImage;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVAsset, AVAssetImageGenerator, AVAssetTrack, AVMediaTypeVideo, AVPlayer, AVPlayerItem, AVPlayerItemVideoOutput,
    AVURLAsset,
};
use objc2_core_foundation::{CFString, CGSize};
use objc2_core_media::{kCMTimeZero, CMTime};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::VideoInfo;

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn asset(path: &Path) -> Retained<AVURLAsset> {
    unsafe { AVURLAsset::URLAssetWithURL_options(&url(path), None) }
}

fn video_track(asset: &AVAsset) -> Option<Retained<AVAssetTrack>> {
    let kind = unsafe { AVMediaTypeVideo }?;
    unsafe { asset.tracksWithMediaType(kind) }.firstObject()
}

/// Clockwise rotation (0/90/180/270) the phone asked for in the track's transform.
fn rotation(track: &AVAssetTrack) -> u32 {
    let t = unsafe { track.preferredTransform() };
    let deg = t.b.atan2(t.a).to_degrees().round() as i32;
    deg.rem_euclid(360) as u32 / 90 * 90
}

fn seconds(t: CMTime) -> f64 {
    let s = unsafe { t.seconds() };
    if s.is_finite() { s } else { 0.0 }
}

pub fn probe(path: &Path) -> Option<VideoInfo> {
    autoreleasepool(|_| {
        let asset = asset(path);
        let track = video_track(&asset)?;
        let size = unsafe { track.naturalSize() };
        let (w, h) = (size.width.abs().round() as u32, size.height.abs().round() as u32);
        let (width, height) = if rotation(&track) % 180 == 90 { (h, w) } else { (w, h) };
        let duration = seconds(unsafe { asset.duration() }) as f32;
        let created = unsafe { asset.creationDate() }
            .and_then(|m| unsafe { m.dateValue() })
            .and_then(|d| DateTime::from_timestamp(d.timeIntervalSince1970() as i64, 0))
            .map(|utc| utc.with_timezone(&Local).naive_local());
        Some(VideoInfo { width, height, duration, created })
    })
}

pub fn thumbnail(path: &Path, max_side: u32) -> Option<DynamicImage> {
    autoreleasepool(|_| {
        let asset = asset(path);
        let duration = seconds(unsafe { asset.duration() });
        let generator = unsafe { AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset) };
        unsafe {
            generator.setAppliesPreferredTrackTransform(true);
            generator.setMaximumSize(CGSize { width: max_side as f64, height: max_side as f64 });
        }
        // A second in skips black fade-ins, but stay inside very short clips.
        let at = unsafe { CMTime::with_seconds((duration / 2.0).min(1.0), 600) };
        let image = unsafe { generator.copyCGImageAtTime_actualTime_error(at, std::ptr::null_mut()) }.ok()?;
        crate::imageio::cgimage_to_image(Retained::as_ptr(&image) as *const std::ffi::c_void)
    })
}

pub struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    rotation: u32,
    fallback_duration: f64,
    playing: bool,
    muted: bool,
}

impl Player {
    pub fn open(path: &Path, _ctx: &egui::Context) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("video must be opened on the main thread")?;
        let asset = asset(path);
        let track = video_track(&asset).ok_or("No video track in this file.")?;
        let rotation = rotation(&track);
        let fallback_duration = seconds(unsafe { asset.duration() });

        // Ask for BGRA frames, which we swizzle to RGBA for egui.
        let key: &NSString = unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const CFString as *const NSString) };
        let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
        let value: &AnyObject = &format;
        let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[value]);

        unsafe {
            let item = AVPlayerItem::playerItemWithURL(&url(path), mtm);
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(&attrs));
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.play();
            Ok(Self { player, item, output, rotation, fallback_duration, playing: true, muted: false })
        }
    }

    pub fn play(&mut self) {
        if self.ended() {
            self.seek(0.0);
        }
        unsafe { self.player.play() };
        self.playing = true;
    }

    pub fn pause(&mut self) {
        unsafe { self.player.pause() };
        self.playing = false;
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn position(&self) -> f64 {
        seconds(unsafe { self.item.currentTime() })
    }

    pub fn duration(&self) -> f64 {
        let d = seconds(unsafe { self.item.duration() });
        if d > 0.0 { d } else { self.fallback_duration }
    }

    pub fn seek(&mut self, secs: f64) {
        unsafe {
            let t = CMTime::with_seconds(secs.max(0.0), 600);
            self.player.seekToTime_toleranceBefore_toleranceAfter(t, kCMTimeZero, kCMTimeZero);
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        unsafe { self.player.setMuted(muted) };
        self.muted = muted;
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    /// The frame to show now, if it changed since the last call.
    pub fn take_frame(&mut self) -> Option<ColorImage> {
        autoreleasepool(|_| unsafe {
            let t = self.item.currentTime();
            if !self.output.hasNewPixelBufferForItemTime(t) {
                return None;
            }
            let pb = self.output.copyPixelBufferForItemTime_itemTimeForDisplay(t, std::ptr::null_mut())?;
            if CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) != 0 {
                return None;
            }
            let (w, h, stride) = (CVPixelBufferGetWidth(&pb), CVPixelBufferGetHeight(&pb), CVPixelBufferGetBytesPerRow(&pb));
            let base = CVPixelBufferGetBaseAddress(&pb) as *const u8;
            let mut rgba = Vec::with_capacity(w * h * 4);
            if !base.is_null() {
                for y in 0..h {
                    let row = std::slice::from_raw_parts(base.add(y * stride), w * 4);
                    for px in row.chunks_exact(4) {
                        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                    }
                }
            }
            CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly);
            if rgba.len() != w * h * 4 {
                return None;
            }
            let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
            let img = match self.rotation {
                90 => image::imageops::rotate90(&img),
                180 => image::imageops::rotate180(&img),
                270 => image::imageops::rotate270(&img),
                _ => img,
            };
            Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
        })
    }

    pub fn ended(&mut self) -> bool {
        let d = self.duration();
        let done = d > 0.0 && self.position() >= d - 0.05 && unsafe { self.player.rate() } == 0.0;
        if done {
            self.playing = false;
        }
        done
    }

    pub fn error(&self) -> Option<String> {
        unsafe { self.item.error() }.map(|e| e.localizedDescription().to_string())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { self.player.pause() };
    }
}
