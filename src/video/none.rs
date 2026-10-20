//! No video backend in this build: videos are listed but play in the default app.

use std::path::Path;

use eframe::egui::{self, ColorImage};
use image::DynamicImage;

use super::VideoInfo;

pub fn probe(_path: &Path) -> Option<VideoInfo> {
    None
}

pub fn thumbnail(_path: &Path, _max_side: u32) -> Option<DynamicImage> {
    None
}

pub struct Player;

impl Player {
    pub fn open(_path: &Path, _ctx: &egui::Context) -> Result<Self, String> {
        Err("This build can't play videos.".into())
    }
    pub fn play(&mut self) {}
    pub fn pause(&mut self) {}
    pub fn playing(&self) -> bool {
        false
    }
    pub fn position(&self) -> f64 {
        0.0
    }
    pub fn duration(&self) -> f64 {
        0.0
    }
    pub fn seek(&mut self, _secs: f64) {}
    pub fn set_muted(&mut self, _muted: bool) {}
    pub fn muted(&self) -> bool {
        false
    }
    pub fn take_frame(&mut self) -> Option<ColorImage> {
        None
    }
    pub fn ended(&mut self) -> bool {
        false
    }
    pub fn error(&self) -> Option<String> {
        None
    }
}
