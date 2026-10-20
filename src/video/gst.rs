//! Linux (and Windows) video via GStreamer: Discoverer for metadata, and a `playbin` whose
//! video goes into an appsink we pull RGBA frames from. `videoflip video-direction=auto`
//! applies the rotation phones store in the file; playbin plays the sound itself.

use std::path::Path;
use std::sync::OnceLock;

use chrono::NaiveDate;
use eframe::egui::{self, ColorImage};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils as pbutils;
use gstreamer_pbutils::prelude::*;
use gstreamer_video as gst_video;
use image::{DynamicImage, RgbaImage};

use super::VideoInfo;

const SINK_BIN: &str =
    "videoflip video-direction=auto ! videoconvert ! videoscale ! video/x-raw,format=RGBA,pixel-aspect-ratio=1/1 ! appsink name=sink";

fn init() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| gst::init().is_ok())
}

fn uri(path: &Path) -> Option<String> {
    let abs = std::fs::canonicalize(path).ok()?;
    gst::glib::filename_to_uri(abs, None).ok().map(|u| u.to_string())
}

pub fn probe(path: &Path) -> Option<VideoInfo> {
    if !init() {
        return None;
    }
    let discoverer = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10)).ok()?;
    let info = discoverer.discover_uri(&uri(path)?).ok()?;
    let stream = info.video_streams().into_iter().next()?;

    let par = stream.par();
    let mut width = (stream.width() as f64 * par.numer() as f64 / par.denom().max(1) as f64).round() as u32;
    let mut height = stream.height();
    let rotated = stream
        .tags()
        .and_then(|t| t.get::<gst::tags::ImageOrientation>().map(|v| v.get().to_string()))
        .is_some_and(|o| o.contains("90") || o.contains("270"));
    if rotated {
        std::mem::swap(&mut width, &mut height);
    }

    let duration = info.duration().map(|d| d.seconds_f64() as f32).unwrap_or(0.0);
    let created = info.tags().and_then(|t| t.get::<gst::tags::DateTime>()).and_then(|v| {
        let dt = v.get();
        let date = NaiveDate::from_ymd_opt(dt.year(), dt.month()? as u32, dt.day()? as u32)?;
        date.and_hms_opt(dt.hour().unwrap_or(0) as u32, dt.minute().unwrap_or(0) as u32, dt.second().unwrap_or(0) as u32)
    });
    Some(VideoInfo { width, height, duration, created })
}

fn playbin(path: &Path, with_audio: bool) -> Option<(gst::Element, gst_app::AppSink)> {
    if !init() {
        return None;
    }
    let playbin = gst::ElementFactory::make("playbin").property("uri", uri(path)?).build().ok()?;
    let bin = gst::parse::bin_from_description(SINK_BIN, true).ok()?;
    let sink = bin.by_name("sink")?.downcast::<gst_app::AppSink>().ok()?;
    sink.set_max_buffers(1);
    sink.set_drop(true);
    playbin.set_property("video-sink", &bin);
    if !with_audio {
        let fake = gst::ElementFactory::make("fakesink").build().ok()?;
        playbin.set_property("audio-sink", &fake);
    }
    Some((playbin, sink))
}

fn sample_to_rgba(sample: &gst::Sample) -> Option<RgbaImage> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    let (w, h) = (info.width() as usize, info.height() as usize);
    let stride = info.stride()[0] as usize;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        rgba.extend_from_slice(map.get(y * stride..y * stride + w * 4)?);
    }
    RgbaImage::from_raw(w as u32, h as u32, rgba)
}

pub fn thumbnail(path: &Path, max_side: u32) -> Option<DynamicImage> {
    let (pipeline, sink) = playbin(path, false)?;
    let result = (|| {
        pipeline.set_state(gst::State::Paused).ok()?;
        pipeline.state(gst::ClockTime::from_seconds(10)).0.ok()?;
        // A second in skips black fade-ins, but stay inside very short clips.
        let duration = pipeline.query_duration::<gst::ClockTime>().map(|d| d.seconds_f64()).unwrap_or(0.0);
        let at = gst::ClockTime::from_seconds_f64((duration / 2.0).min(1.0));
        if pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, at).is_ok() {
            let _ = pipeline.state(gst::ClockTime::from_seconds(10));
        }
        let sample = sink.pull_preroll().ok()?;
        let img = DynamicImage::ImageRgba8(sample_to_rgba(&sample)?);
        Some(if img.width() > max_side || img.height() > max_side { img.thumbnail(max_side, max_side) } else { img })
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

pub struct Player {
    pipeline: gst::Element,
    sink: gst_app::AppSink,
    playing: bool,
    muted: bool,
    ended: bool,
    error: Option<String>,
    duration: f64,
}

impl Player {
    pub fn open(path: &Path, ctx: &egui::Context) -> Result<Self, String> {
        let (pipeline, sink) = playbin(path, true).ok_or("GStreamer isn't available.")?;
        // Wake the UI whenever a frame arrives; the sample stays queued for `take_frame` to pull.
        let ctx = ctx.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |_| {
                    ctx.request_repaint();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        Ok(Self { pipeline, sink, playing: true, muted: false, ended: false, error: None, duration: 0.0 })
    }

    fn drain_bus(&mut self) {
        let Some(bus) = self.pipeline.bus() else { return };
        while let Some(msg) = bus.pop() {
            match msg.view() {
                gst::MessageView::Eos(_) => {
                    self.ended = true;
                    self.playing = false;
                }
                gst::MessageView::Error(e) => {
                    self.error = Some(e.error().to_string());
                    self.playing = false;
                }
                _ => {}
            }
        }
    }

    pub fn play(&mut self) {
        if self.ended {
            self.seek(0.0);
            self.ended = false;
        }
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.playing = true;
    }

    pub fn pause(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.playing = false;
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn position(&self) -> f64 {
        self.pipeline.query_position::<gst::ClockTime>().map(|p| p.seconds_f64()).unwrap_or(0.0)
    }

    pub fn duration(&self) -> f64 {
        self.pipeline.query_duration::<gst::ClockTime>().map(|d| d.seconds_f64()).unwrap_or(self.duration)
    }

    pub fn seek(&mut self, secs: f64) {
        let at = gst::ClockTime::from_seconds_f64(secs.max(0.0));
        let _ = self.pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, at);
        self.ended = false;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.pipeline.set_property("mute", muted);
        self.muted = muted;
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    /// The frame to show now, if a new one arrived since the last call.
    pub fn take_frame(&mut self) -> Option<ColorImage> {
        self.drain_bus();
        let d = self.duration();
        if d > 0.0 {
            self.duration = d;
        }
        let zero = gst::ClockTime::ZERO;
        // While paused (e.g. after seeking) the new frame comes as a preroll sample.
        let sample = self.sink.try_pull_sample(zero).or_else(|| if self.playing { None } else { self.sink.try_pull_preroll(zero) })?;
        let img = sample_to_rgba(&sample)?;
        Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
    }

    pub fn ended(&mut self) -> bool {
        self.drain_bus();
        self.ended
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
