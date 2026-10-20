use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use chrono::{Local, NaiveDate};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Key, Rect, Sense, Stroke, TextureHandle, TextureOptions,
};

use crate::library::{spawn_scan, Photo, ScanMsg};
use crate::thumbs::{cache_key, spawn_full, Job, Loaded, ThumbLoader};

const GAP: f32 = 4.0;
const HEADER_H: f32 = 44.0;
const MAX_TEXTURES: usize = 3000;
const KEEP_TEXTURES: usize = 2000;

struct Group {
    date: NaiveDate,
    /// Range into `order`.
    start: usize,
    len: usize,
}

enum Row {
    Header(usize),
    Photos { start: usize, len: usize },
}

struct LaidRow {
    y: f32,
    h: f32,
    row: Row,
}

struct Viewer {
    /// Position in `order`.
    pos: usize,
    texture: Option<TextureHandle>,
    failed: bool,
}

pub struct PhotosApp {
    root: Option<PathBuf>,
    gen: u64,

    photos: Vec<Photo>,
    order: Vec<usize>,
    groups: Vec<Group>,
    dirty: bool,
    last_rebuild: f64,

    scan_rx: Receiver<ScanMsg>,
    scan_tx: Sender<ScanMsg>,
    scan_total: usize,
    scanning: bool,

    loader: ThumbLoader,
    thumbs: HashMap<usize, (TextureHandle, u64)>,
    failed: HashSet<usize>,
    frame: u64,

    full_rx: Receiver<Loaded>,
    full_tx: Sender<Loaded>,
    viewer: Option<Viewer>,

    thumb_size: f32,
    #[cfg(debug_assertions)]
    debug_opened: bool,
    #[cfg(debug_assertions)]
    debug_shot_sent: bool,
}

impl PhotosApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<PathBuf>) -> Self {
        let (scan_tx, scan_rx) = channel();
        let (full_tx, full_rx) = channel();
        let mut app = Self {
            root: None,
            gen: 0,
            photos: Vec::new(),
            order: Vec::new(),
            groups: Vec::new(),
            dirty: false,
            last_rebuild: 0.0,
            scan_rx,
            scan_tx,
            scan_total: 0,
            scanning: false,
            loader: ThumbLoader::new(cc.egui_ctx.clone()),
            thumbs: HashMap::new(),
            failed: HashSet::new(),
            frame: 0,
            full_rx,
            full_tx,
            viewer: None,
            thumb_size: 180.0,
            #[cfg(debug_assertions)]
            debug_opened: false,
            #[cfg(debug_assertions)]
            debug_shot_sent: false,
        };
        if let Some(dir) = start.or_else(load_last_folder).or_else(dirs::picture_dir) {
            app.open_folder(dir, &cc.egui_ctx);
        }
        app
    }

    fn open_folder(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.gen += 1;
        self.photos.clear();
        self.order.clear();
        self.groups.clear();
        self.thumbs.clear();
        self.failed.clear();
        self.viewer = None;
        self.scan_total = 0;
        self.scanning = true;
        save_last_folder(&dir);
        spawn_scan(dir.clone(), self.gen, self.scan_tx.clone(), ctx.clone());
        self.root = Some(dir);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.scan_rx.try_recv() {
            match msg {
                ScanMsg::Total(g, n) if g == self.gen => self.scan_total = n,
                ScanMsg::Batch(g, batch) if g == self.gen => {
                    self.photos.extend(batch);
                    self.dirty = true;
                }
                ScanMsg::Done(g) if g == self.gen => {
                    self.scanning = false;
                    self.dirty = true;
                }
                _ => {}
            }
        }

        // Re-sorting on every batch would stall a big scan, so do it a few times a second at most.
        let now = ctx.input(|i| i.time);
        if self.dirty && (!self.scanning || now - self.last_rebuild > 0.4) {
            self.rebuild();
            self.last_rebuild = now;
        }

        while let Ok(loaded) = self.loader.rx.try_recv() {
            if loaded.gen != self.gen {
                continue;
            }
            match loaded.image {
                Some(img) => {
                    let tex = ctx.load_texture(format!("t{}", loaded.id), img, TextureOptions::LINEAR);
                    self.thumbs.insert(loaded.id, (tex, self.frame));
                }
                None => {
                    self.failed.insert(loaded.id);
                }
            }
        }

        while let Ok(loaded) = self.full_rx.try_recv() {
            let Some(viewer) = &mut self.viewer else { continue };
            if loaded.gen != self.gen || self.order.get(viewer.pos) != Some(&loaded.id) {
                continue;
            }
            match loaded.image {
                Some(img) => viewer.texture = Some(ctx.load_texture("full", img, TextureOptions::LINEAR)),
                None => viewer.failed = true,
            }
        }

        if self.thumbs.len() > MAX_TEXTURES {
            let mut by_age: Vec<(usize, u64)> = self.thumbs.iter().map(|(id, (_, seen))| (*id, *seen)).collect();
            by_age.sort_by_key(|(_, seen)| *seen);
            for (id, _) in by_age.iter().take(self.thumbs.len() - KEEP_TEXTURES) {
                self.thumbs.remove(id);
            }
        }
    }

    fn rebuild(&mut self) {
        self.dirty = false;
        // Keep the open photo open even though its position in `order` may move.
        let viewing = self.viewer.as_ref().and_then(|v| self.order.get(v.pos).copied());

        self.order = (0..self.photos.len()).collect();
        let photos = &self.photos;
        self.order.sort_by(|&a, &b| photos[b].taken.cmp(&photos[a].taken).then_with(|| photos[a].path.cmp(&photos[b].path)));

        self.groups.clear();
        for (pos, &i) in self.order.iter().enumerate() {
            let date = self.photos[i].taken.date();
            match self.groups.last_mut() {
                Some(g) if g.date == date => g.len += 1,
                _ => self.groups.push(Group { date, start: pos, len: 1 }),
            }
        }

        if let (Some(id), Some(v)) = (viewing, &mut self.viewer) {
            if let Some(pos) = self.order.iter().position(|&i| i == id) {
                v.pos = pos;
            }
        }
    }

    fn open_viewer(&mut self, pos: usize, ctx: &egui::Context) {
        let Some(&id) = self.order.get(pos) else { return };
        self.viewer = Some(Viewer { pos, texture: None, failed: false });
        spawn_full(self.photos[id].path.clone(), self.gen, id, self.full_tx.clone(), ctx.clone());
    }

    fn layout(&self, width: f32) -> (Vec<LaidRow>, usize, f32) {
        let cell = self.thumb_size;
        let cols = (((width + GAP) / (cell + GAP)).floor() as usize).max(1);
        let mut rows = Vec::new();
        let mut y = 0.0;
        for (gi, g) in self.groups.iter().enumerate() {
            rows.push(LaidRow { y, h: HEADER_H, row: Row::Header(gi) });
            y += HEADER_H;
            let mut start = g.start;
            let end = g.start + g.len;
            while start < end {
                let len = cols.min(end - start);
                rows.push(LaidRow { y, h: cell + GAP, row: Row::Photos { start, len } });
                y += cell + GAP;
                start += len;
            }
            y += GAP * 3.0;
        }
        (rows, cols, y)
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open folder…").clicked() {
                let mut dialog = rfd::FileDialog::new();
                if let Some(root) = &self.root {
                    dialog = dialog.set_directory(root);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.open_folder(dir, ui.ctx());
                }
            }
            if let Some(root) = &self.root {
                let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
                ui.strong(name).on_hover_text(root.display().to_string());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Slider::new(&mut self.thumb_size, 100.0..=320.0).show_value(false).text("Size"));
                ui.separator();
                if self.scanning {
                    ui.spinner();
                    ui.label(format!("Scanning {} / {}", self.photos.len(), self.scan_total));
                } else {
                    ui.label(format!("{} photos · {} days", self.photos.len(), self.groups.len()));
                }
            });
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(if self.scanning { "Looking for photos…" } else { "No photos here. Open a folder to start." });
            });
            return;
        }

        let width = ui.available_width();
        let (rows, _cols, total_h) = self.layout(width);
        let cell = self.thumb_size;
        let today = Local::now().date_naive();
        let mut clicked = None;
        let mut wanted_now = Vec::new();
        let mut wanted_soon = Vec::new();

        egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
            let origin = ui.max_rect().min;
            ui.set_height(total_h);
            ui.set_width(width);

            let prefetch_to = viewport.max.y + viewport.height();
            let first = rows.partition_point(|r| r.y + r.h < viewport.min.y);
            for r in rows[first..].iter().take_while(|r| r.y < prefetch_to) {
                let visible = r.y < viewport.max.y;
                match r.row {
                    Row::Header(gi) if visible => {
                        let g = &self.groups[gi];
                        let pos = origin + vec2(0.0, r.y + HEADER_H * 0.6);
                        let text = day_label(g.date, today);
                        let galley = ui.painter().text(
                            pos,
                            Align2::LEFT_CENTER,
                            text,
                            FontId::proportional(18.0),
                            ui.visuals().strong_text_color(),
                        );
                        ui.painter().text(
                            pos2(galley.right() + 10.0, pos.y),
                            Align2::LEFT_CENTER,
                            format!("{} photo{}", g.len, if g.len == 1 { "" } else { "s" }),
                            FontId::proportional(13.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    Row::Header(_) => {}
                    Row::Photos { start, len } => {
                        for k in 0..len {
                            let pos = start + k;
                            let id = self.order[pos];
                            let rect = Rect::from_min_size(origin + vec2(k as f32 * (cell + GAP), r.y), vec2(cell, cell));

                            if !self.thumbs.contains_key(&id) && !self.failed.contains(&id) {
                                let p = &self.photos[id];
                                let job = Job { gen: self.gen, id, path: p.path.clone(), key: cache_key(&p.path, p.mtime, p.size) };
                                if visible { wanted_now.push(job) } else { wanted_soon.push(job) }
                            }
                            if !visible {
                                continue;
                            }

                            let resp = ui.interact(rect, ui.id().with(("photo", id)), Sense::click());
                            let painter = ui.painter();
                            if let Some((tex, seen)) = self.thumbs.get_mut(&id) {
                                *seen = self.frame;
                                let uv = cover_uv(tex.size_vec2());
                                painter.image(tex.id(), rect, uv, Color32::WHITE);
                            } else {
                                painter.rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
                                if self.failed.contains(&id) {
                                    painter.text(rect.center(), Align2::CENTER_CENTER, "?", FontId::proportional(24.0), ui.visuals().weak_text_color());
                                }
                            }
                            if resp.hovered() {
                                painter.rect_stroke(rect, 2.0_f32, Stroke::new(2.0_f32, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                            }
                            if resp.clicked() {
                                clicked = Some(pos);
                            }
                        }
                    }
                }
            }
        });

        // Workers take from the back: off-screen prefetch first in the list, then visible
        // rows bottom-to-top, so the top-left of the screen fills in first.
        wanted_soon.reverse();
        wanted_now.reverse();
        wanted_soon.extend(wanted_now);
        self.loader.set_wanted(wanted_soon);

        if let Some(pos) = clicked {
            self.open_viewer(pos, ui.ctx());
        }
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(v) = &self.viewer else { return };
        let pos = v.pos;
        let id = self.order[pos];
        let photo = &self.photos[id];

        let (close, prev, next) = ctx.input(|i| {
            (i.key_pressed(Key::Escape), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight))
        });

        let mut go: Option<usize> = None;
        let mut want_close = close;
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                want_close = true;
            }
            ui.separator();
            ui.strong(photo.taken.format("%A, %-d %B %Y · %H:%M").to_string());
            if !photo.from_exif {
                ui.weak("(file date)").on_hover_text("No camera date in this photo, so the file's modified date is used.");
            }
            ui.weak(photo.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("{} / {}", pos + 1, self.order.len()));
                if ui.add_enabled(pos + 1 < self.order.len(), egui::Button::new("▶")).clicked() {
                    go = Some(pos + 1);
                }
                if ui.add_enabled(pos > 0, egui::Button::new("◀")).clicked() {
                    go = Some(pos - 1);
                }
            });
        });

        if prev && pos > 0 {
            go = Some(pos - 1);
        }
        if next && pos + 1 < self.order.len() {
            go = Some(pos + 1);
        }

        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, Color32::from_gray(12));
        let v = self.viewer.as_ref().unwrap();
        // Show the blurry thumbnail while the full image decodes.
        let tex = v.texture.as_ref().or_else(|| self.thumbs.get(&id).map(|(t, _)| t));
        if let Some(tex) = tex {
            let size = tex.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y).min(if v.texture.is_some() { 1.0 } else { f32::MAX });
            let rect = Rect::from_center_size(area.center(), size * scale);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        if v.failed {
            ui.painter().text(area.center(), Align2::CENTER_CENTER, "Couldn't open this image", FontId::proportional(18.0), Color32::LIGHT_GRAY);
        } else if v.texture.is_none() {
            ui.put(Rect::from_center_size(area.center(), vec2(32.0, 32.0)), egui::Spinner::new());
        }

        if want_close {
            self.viewer = None;
        } else if let Some(p) = go {
            self.open_viewer(p, &ctx);
        }
    }
}

impl eframe::App for PhotosApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame += 1;
        self.poll(ctx);
        #[cfg(debug_assertions)]
        self.debug_screenshot(ctx);

        if self.viewer.is_some() {
            egui::CentralPanel::default().show(ctx, |ui| self.viewer(ui));
            return;
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| self.grid(ui));
    }
}

fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    let days = (today - date).num_days();
    if days == 0 {
        "Today".into()
    } else if days == 1 {
        "Yesterday".into()
    } else if (0..7).contains(&days) {
        date.format("%A").to_string()
    } else if date.format("%Y").to_string() == today.format("%Y").to_string() {
        date.format("%a, %-d %b").to_string()
    } else {
        date.format("%a, %-d %b %Y").to_string()
    }
}

/// UV rect that crops the texture to a centred square, like `object-fit: cover`.
fn cover_uv(size: egui::Vec2) -> Rect {
    let aspect = size.x / size.y;
    if aspect > 1.0 {
        let u = 1.0 / aspect;
        let x0 = (1.0 - u) / 2.0;
        Rect::from_min_max(pos2(x0, 0.0), pos2(x0 + u, 1.0))
    } else {
        let v = aspect;
        let y0 = (1.0 - v) / 2.0;
        Rect::from_min_max(pos2(0.0, y0), pos2(1.0, y0 + v))
    }
}

fn last_folder_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("banana-rs").join("last_folder"))
}

fn load_last_folder() -> Option<PathBuf> {
    let path = PathBuf::from(std::fs::read_to_string(last_folder_file()?).ok()?.trim());
    path.is_dir().then_some(path)
}

fn save_last_folder(dir: &std::path::Path) {
    if let Some(file) = last_folder_file() {
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(file, dir.to_string_lossy().as_bytes());
    }
}

/// Dev-only: `BANANA_SCREENSHOT=out.png` saves a screenshot once the scan settles, then quits.
/// `BANANA_OPEN=<n>` opens the n-th photo in the viewer first.
#[cfg(debug_assertions)]
impl PhotosApp {
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Some(out) = std::env::var_os("BANANA_SCREENSHOT") else { return };
        let t = ctx.input(|i| i.time);
        if t > 1.5 && self.viewer.is_none() && !self.scanning {
            if let Some(n) = std::env::var("BANANA_OPEN").ok().and_then(|s| s.parse().ok()) {
                if !self.debug_opened {
                    self.debug_opened = true;
                    self.open_viewer(n, ctx);
                }
            }
        }
        if t > 3.0 && !self.debug_shot_sent {
            self.debug_shot_sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let _ = image::save_buffer(out, &bytes, w as u32, h as u32, image::ColorType::Rgba8);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
