use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Margin, Rect, Sense, Shape, Stroke,
    TextureHandle, TextureOptions,
};
use egui_phosphor::regular as icon;

use crate::library::{is_screenshot, spawn_scan, Photo, ScanMsg};
use crate::people::{self, FaceRef, Person, Pet};
use crate::store::Store;
use crate::thumbs::{cache_key, spawn_full, Job, Loaded, ThumbLoader};
use crate::video;

const MAX_TEXTURES: usize = 3000;
const KEEP_TEXTURES: usize = 2000;

// Layout.
const GAP: f32 = 4.0;
const DAY_GAP: f32 = 28.0;
const DAY_LABEL_H: f32 = 40.0;
const MONTH_H: f32 = 76.0;
const MEMORIES_H: f32 = 290.0;
const SIDEBAR_W: f32 = 256.0;
const CARD: f32 = 210.0;

// Dark palette.
const BG: Color32 = Color32::from_rgb(27, 27, 28);
const CONTENT_BG: Color32 = Color32::from_rgb(19, 19, 20);
const FIELD_BG: Color32 = Color32::from_rgb(40, 42, 44);
const HOVER_BG: Color32 = Color32::from_rgb(44, 45, 47);
const PILL_BG: Color32 = Color32::from_rgb(0, 74, 119);
const PILL_TEXT: Color32 = Color32::from_rgb(194, 231, 255);
const TEXT: Color32 = Color32::from_rgb(227, 227, 227);
const TEXT_DIM: Color32 = Color32::from_rgb(160, 162, 166);
const ACCENT: Color32 = Color32::from_rgb(168, 199, 250);
const HEART: Color32 = Color32::from_rgb(242, 139, 130);
const BANANA: Color32 = Color32::from_rgb(253, 214, 99);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Photos,
    Favorites,
    Videos,
    People,
    Person(usize),
    Pets(Pet),
    Screenshots,
    Recent,
    Archive,
    Albums,
    Album(usize),
}

impl Page {
    fn key(self) -> usize {
        match self {
            Page::Photos => 0,
            Page::Favorites => 1,
            Page::Screenshots => 2,
            Page::Recent => 3,
            Page::Archive => 4,
            Page::Albums => 5,
            Page::Videos => 6,
            Page::People => 7,
            Page::Pets(Pet::Dog) => 8,
            Page::Pets(Pet::Cat) => 9,
            Page::Album(i) => 100 + 2 * i,
            Page::Person(i) => 101 + 2 * i,
        }
    }
}

enum Dialog {
    /// `photos` are added to the album once it is created; empty when started from the sidebar.
    NewAlbum { name: String, photos: Vec<PathBuf> },
    Rename { album: usize, name: String },
    DeleteAlbum(usize),
    /// Name (or rename) a person; an empty name removes it.
    NamePerson { person: usize, name: String },
}

enum Click {
    Open(usize),
    Toggle(usize),
    Range(usize),
    Day(usize),
}

struct Group {
    date: NaiveDate,
    /// Range into `order`.
    start: usize,
    len: usize,
}

/// A "N years ago" card: photos from around today's date in an earlier year.
struct Memory {
    years: i32,
    date: NaiveDate,
    /// Position in `order` of the first photo of that day.
    pos: usize,
    count: usize,
}

struct Item {
    pos: usize,
    x: f32,
    w: f32,
}

enum RowKind {
    Memories,
    Month(String),
    Photos {
        /// Height reserved above the photos for day labels (0 when no day starts in this row).
        label_h: f32,
        img_h: f32,
        items: Vec<Item>,
        /// (x, group index) of each day that starts in this row.
        labels: Vec<(f32, usize)>,
    },
}

struct LaidRow {
    y: f32,
    h: f32,
    kind: RowKind,
}

struct Layout {
    key: (i32, i32, u64),
    rows: Vec<LaidRow>,
    height: f32,
}

struct Viewer {
    /// Position in `order`.
    pos: usize,
    texture: Option<TextureHandle>,
    failed: bool,
    /// Set when the open item is a video that the backend could start.
    player: Option<video::Player>,
    video_error: Option<String>,
}

pub struct PhotosApp {
    root: Option<PathBuf>,
    gen: u64,

    photos: Vec<Photo>,
    path_index: HashMap<PathBuf, usize>,
    library_bytes: u64,
    dirty: bool,
    last_rebuild: f64,

    /// What the current page shows: filtered and sorted photo ids, grouped by day.
    page: Page,
    search: String,
    order: Vec<usize>,
    groups: Vec<Group>,
    memories: Vec<Memory>,
    view_dirty: bool,
    view_version: u64,
    layout: Option<Layout>,

    selected: HashSet<usize>,
    /// Position in `order` of the last toggled photo, for shift-click ranges.
    anchor: Option<usize>,

    store: Store,
    dialog: Option<Dialog>,
    albums_open: bool,

    // People & pets
    people_index: people::Index,
    people: Vec<Person>,
    /// Lower-case names of the people in each photo, for search.
    photo_people: HashMap<PathBuf, String>,
    people_dirty: bool,
    people_index_dirty: bool,
    last_cluster: f64,
    last_index_save: f64,
    analysing: bool,
    analysed: usize,
    analyse_total: usize,
    analysis_gen: Arc<AtomicU64>,
    analysis_rx: Receiver<people::Msg>,
    analysis_tx: Sender<people::Msg>,
    /// Why People & pets can't run (models missing, failed to load).
    people_problem: Option<String>,
    avatars: HashMap<String, Option<TextureHandle>>,
    show_hidden_people: bool,

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

    /// Target row height of the justified grid.
    row_height: f32,
    #[cfg(debug_assertions)]
    debug_done: bool,
    #[cfg(debug_assertions)]
    debug_shot_sent: bool,
    #[cfg(debug_assertions)]
    debug_done_at: f64,
}

impl PhotosApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<PathBuf>) -> Self {
        setup_style(&cc.egui_ctx);
        let (scan_tx, scan_rx) = channel();
        let (full_tx, full_rx) = channel();
        let (analysis_tx, analysis_rx) = channel();
        let mut app = Self {
            root: None,
            gen: 0,
            photos: Vec::new(),
            path_index: HashMap::new(),
            library_bytes: 0,
            dirty: false,
            last_rebuild: 0.0,
            page: Page::Photos,
            search: String::new(),
            order: Vec::new(),
            groups: Vec::new(),
            memories: Vec::new(),
            view_dirty: false,
            view_version: 0,
            layout: None,
            selected: HashSet::new(),
            anchor: None,
            store: Store::load(),
            dialog: None,
            albums_open: true,
            people_index: people::Index::load(),
            people: Vec::new(),
            photo_people: HashMap::new(),
            people_dirty: true,
            people_index_dirty: false,
            last_cluster: 0.0,
            last_index_save: 0.0,
            analysing: false,
            analysed: 0,
            analyse_total: 0,
            analysis_gen: Arc::new(AtomicU64::new(0)),
            analysis_rx,
            analysis_tx,
            people_problem: None,
            avatars: HashMap::new(),
            show_hidden_people: false,
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
            row_height: 210.0,
            #[cfg(debug_assertions)]
            debug_done: false,
            #[cfg(debug_assertions)]
            debug_shot_sent: false,
            #[cfg(debug_assertions)]
            debug_done_at: 0.0,
        };
        if let Some(dir) = start.or_else(load_last_folder).or_else(dirs::picture_dir) {
            app.open_folder(dir, &cc.egui_ctx);
        }
        app
    }

    fn open_folder(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.gen += 1;
        self.photos.clear();
        self.path_index.clear();
        self.library_bytes = 0;
        self.order.clear();
        self.groups.clear();
        self.memories.clear();
        self.selected.clear();
        self.anchor = None;
        self.thumbs.clear();
        self.failed.clear();
        self.viewer = None;
        self.scan_total = 0;
        self.scanning = true;
        self.view_dirty = true;
        save_last_folder(&dir);
        self.analysis_gen.store(self.gen, Ordering::Relaxed);
        self.analysing = false;
        self.people.clear();
        self.people_dirty = true;
        spawn_scan(dir.clone(), self.gen, self.scan_tx.clone(), ctx.clone());
        self.root = Some(dir);
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(root) = &self.root {
            dialog = dialog.set_directory(root);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(dir, ctx);
        }
    }

    fn set_page(&mut self, page: Page) {
        self.page = page;
        self.selected.clear();
        self.anchor = None;
        self.viewer = None;
        self.view_dirty = true;
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
                    self.start_analysis(ctx);
                }
                _ => {}
            }
        }

        // Re-sorting on every batch would stall a big scan, so do it a few times a second at most.
        let now = ctx.input(|i| i.time);
        if self.dirty && (!self.scanning || now - self.last_rebuild > 0.4) {
            self.dirty = false;
            self.path_index = self.photos.iter().enumerate().map(|(i, p)| (p.path.clone(), i)).collect();
            self.library_bytes = self.photos.iter().map(|p| p.size).sum();
            self.view_dirty = true;
            self.last_rebuild = now;
        }
        self.poll_people(now);
        if self.view_dirty {
            self.apply_view(ctx);
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

    /// The date a photo is filed under on the current page.
    fn when(&self, id: usize) -> NaiveDateTime {
        let p = &self.photos[id];
        if self.page == Page::Recent { p.added } else { p.taken }
    }

    fn apply_view(&mut self, ctx: &egui::Context) {
        self.view_dirty = false;
        self.view_version += 1;
        // Keep the open photo open even though its position in `order` may move.
        let viewing = self.viewer.as_ref().and_then(|v| self.order.get(v.pos).copied());

        let album: Option<HashSet<&PathBuf>> = match self.page {
            Page::Album(i) => self.store.albums.get(i).map(|a| a.photos.iter().collect()),
            _ => None,
        };
        let terms: Vec<String> = self.search.to_lowercase().split_whitespace().map(String::from).collect();
        let mut order: Vec<usize> = (0..self.photos.len())
            .filter(|&id| {
                let p = &self.photos[id];
                let archived = self.store.is_archived(&p.path);
                let on_page = match self.page {
                    Page::Photos | Page::Recent => !archived,
                    Page::Favorites => self.store.is_favorite(&p.path),
                    Page::Videos => !archived && p.is_video,
                    Page::Screenshots => !archived && is_screenshot(&p.path),
                    Page::Archive => archived,
                    Page::Albums | Page::People => false,
                    Page::Person(i) => self.people.get(i).is_some_and(|person| person.photos.contains(&p.path)),
                    Page::Pets(kind) => !archived && self.people_index.entries.get(&p.path).is_some_and(|a| a.pet == Some(kind)),
                    Page::Album(_) => album.as_ref().is_some_and(|a| a.contains(&p.path)),
                };
                on_page && (terms.is_empty() || matches_search(p, &terms, self.photo_people.get(&p.path).map(String::as_str).unwrap_or("")))
            })
            .collect();
        order.sort_by(|&a, &b| self.when(b).cmp(&self.when(a)).then_with(|| self.photos[a].path.cmp(&self.photos[b].path)));
        self.order = order;

        self.groups.clear();
        for pos in 0..self.order.len() {
            let date = self.when(self.order[pos]).date();
            match self.groups.last_mut() {
                Some(g) if g.date == date => g.len += 1,
                _ => self.groups.push(Group { date, start: pos, len: 1 }),
            }
        }

        self.memories.clear();
        if self.page == Page::Photos && terms.is_empty() {
            let today = Local::now().date_naive();
            for years in 1..=30 {
                let Some(day) = today.with_year(today.year() - years).or_else(|| today.pred_opt()?.with_year(today.year() - years)) else {
                    continue;
                };
                // Closest day with photos within a week of this date, that year.
                let best = self
                    .groups
                    .iter()
                    .filter(|g| (g.date - day).num_days().abs() <= 7)
                    .min_by_key(|g| (g.date - day).num_days().abs());
                if let Some(g) = best {
                    self.memories.push(Memory { years, date: g.date, pos: g.start, count: g.len });
                }
            }
        }

        let in_view: HashSet<usize> = self.order.iter().copied().collect();
        self.selected.retain(|id| in_view.contains(id));
        self.anchor = None;

        if let Some(v) = &mut self.viewer {
            match viewing.and_then(|id| self.order.iter().position(|&i| i == id)) {
                Some(pos) => v.pos = pos,
                // The open photo left this view (e.g. un-favourited on the Favourites page).
                None if self.order.is_empty() => self.viewer = None,
                None => {
                    let pos = v.pos.min(self.order.len() - 1);
                    self.open_viewer(pos, ctx);
                }
            }
        }
    }

    fn open_viewer(&mut self, pos: usize, ctx: &egui::Context) {
        let Some(&id) = self.order.get(pos) else { return };
        // Drop the previous player first so its sound stops before the next one starts.
        self.viewer = None;
        let path = self.photos[id].path.clone();
        if self.photos[id].is_video {
            let (player, video_error) = match video::Player::open(&path, ctx) {
                Ok(p) => (Some(p), None),
                Err(e) => (None, Some(e)),
            };
            self.viewer = Some(Viewer { pos, texture: None, failed: false, player, video_error });
        } else {
            self.viewer = Some(Viewer { pos, texture: None, failed: false, player: None, video_error: None });
            spawn_full(path, self.gen, id, self.full_tx.clone(), ctx.clone());
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        // In view order, so albums keep a sensible order.
        self.order.iter().filter(|id| self.selected.contains(id)).map(|&id| self.photos[id].path.clone()).collect()
    }

    fn toggle_favorites(&mut self, paths: &[PathBuf]) {
        self.store.toggle_favorites(paths);
        self.view_dirty = true;
    }

    fn toggle_archive(&mut self, paths: &[PathBuf]) {
        self.store.toggle_archive(paths);
        self.view_dirty = true;
    }

    /// "Add to album" drop-down for `paths`. Returns true once something was chosen.
    fn album_menu(&mut self, ui: &mut egui::Ui, paths: &[PathBuf]) -> bool {
        let mut done = false;
        ui.menu_button(egui::RichText::new(icon::PLUS).size(20.0), |ui| {
            ui.set_min_width(200.0);
            ui.weak("Add to album");
            if ui.button(format!("{}  New album…", icon::PLUS)).clicked() {
                self.dialog = Some(Dialog::NewAlbum { name: String::new(), photos: paths.to_vec() });
                ui.close_menu();
            }
            if !self.store.albums.is_empty() {
                ui.separator();
            }
            let names: Vec<String> = self.store.albums.iter().map(|a| a.name.clone()).collect();
            for (i, name) in names.iter().enumerate() {
                if ui.button(format!("{}  {name}", icon::IMAGES)).clicked() {
                    self.store.add_to_album(i, paths);
                    self.view_dirty = true;
                    done = true;
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text("Add to album");
        done
    }

    /// Google Photos-style justified rows: photos keep their shape, each row is stretched to the
    /// full width, and small days share a row with the next day instead of wasting a line.
    fn build_layout(&self, width: f32) -> Layout {
        let target = self.row_height;
        let mut rows = Vec::new();
        let mut y = 0.0;

        if !self.memories.is_empty() {
            rows.push(LaidRow { y, h: MEMORIES_H, kind: RowKind::Memories });
            y += MEMORIES_H;
        }

        // (pos, aspect, group index if this photo starts a day)
        let mut pending: Vec<(usize, f32, Option<usize>)> = Vec::new();
        let flush = |pending: &mut Vec<(usize, f32, Option<usize>)>, rows: &mut Vec<LaidRow>, y: &mut f32, justify: bool| {
            if pending.is_empty() {
                return;
            }
            let gaps: f32 = pending.iter().skip(1).map(|(_, _, g)| if g.is_some() { DAY_GAP } else { GAP }).sum();
            let aspect_sum: f32 = pending.iter().map(|(_, a, _)| a).sum();
            let fit = (width - gaps) / aspect_sum;
            let img_h = if justify { fit } else { fit.min(target) };
            let mut x = 0.0;
            let mut items = Vec::new();
            let mut labels = Vec::new();
            for (k, (pos, a, g)) in pending.iter().enumerate() {
                if k > 0 {
                    x += if g.is_some() { DAY_GAP } else { GAP };
                }
                if let Some(gi) = g {
                    labels.push((x, *gi));
                }
                let w = a * img_h;
                items.push(Item { pos: *pos, x, w });
                x += w;
            }
            let label_h = if labels.is_empty() { 0.0 } else { DAY_LABEL_H };
            let h = label_h + img_h + GAP;
            rows.push(LaidRow { y: *y, h, kind: RowKind::Photos { label_h, img_h, items, labels } });
            *y += h;
            pending.clear();
        };

        let this_year = Local::now().year();
        let mut month = None;
        for (gi, g) in self.groups.iter().enumerate() {
            let m = (g.date.year(), g.date.month());
            if month != Some(m) {
                flush(&mut pending, &mut rows, &mut y, false);
                month = Some(m);
                let title = if g.date.year() == this_year {
                    g.date.format("%B").to_string()
                } else {
                    g.date.format("%B %Y").to_string()
                };
                rows.push(LaidRow { y, h: MONTH_H, kind: RowKind::Month(title) });
                y += MONTH_H;
            }
            for k in 0..g.len {
                let pos = g.start + k;
                let aspect = self.photos[self.order[pos]].aspect.clamp(0.4, 3.0);
                pending.push((pos, aspect, (k == 0).then_some(gi)));
                let gaps: f32 = pending.iter().skip(1).map(|(_, _, g)| if g.is_some() { DAY_GAP } else { GAP }).sum();
                let natural: f32 = pending.iter().map(|(_, a, _)| a * target).sum::<f32>() + gaps;
                if natural >= width {
                    flush(&mut pending, &mut rows, &mut y, true);
                }
            }
        }
        flush(&mut pending, &mut rows, &mut y, false);

        Layout { key: (0, 0, 0), rows, height: y + 24.0 }
    }

    // ---------------------------------------------------------------- top bar & sidebar

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.allocate_ui(vec2(SIDEBAR_W - 8.0, 48.0), |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(20.0);
                    ui.label(egui::RichText::new(icon::IMAGES).size(28.0).color(BANANA));
                    ui.label(egui::RichText::new("Banana").size(22.0).color(TEXT).strong());
                    ui.label(egui::RichText::new("Photos").size(22.0).color(TEXT_DIM));
                });
            });

            if !self.selected.is_empty() {
                self.selection_bar(ui);
                return;
            }

            let search_w = (ui.available_width() - 220.0).clamp(240.0, 760.0);
            egui::Frame::new()
                .fill(FIELD_BG)
                .corner_radius(CornerRadius::same(24))
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(search_w - 32.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(icon::MAGNIFYING_GLASS).size(20.0).color(TEXT_DIM));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("Search your photos: name, year, month, day")
                                .frame(false)
                                .font(FontId::proportional(16.0))
                                .desired_width(ui.available_width() - 30.0),
                        );
                        if r.changed() {
                            self.view_dirty = true;
                        }
                        if !self.search.is_empty() && icon_button(ui, icon::X, 18.0, "Clear search").clicked() {
                            self.search.clear();
                            self.view_dirty = true;
                        }
                    });
                });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                ui.menu_button(egui::RichText::new(icon::SLIDERS).size(22.0), |ui| {
                    ui.set_min_width(220.0);
                    ui.label("Photo size");
                    ui.add(egui::Slider::new(&mut self.row_height, 120.0..=360.0).show_value(false));
                })
                .response
                .on_hover_text("Photo size");
                if icon_button(ui, icon::FOLDER_OPEN, 22.0, "Open folder…").clicked() {
                    self.pick_folder(ui.ctx());
                }
                if icon_button(ui, icon::PLUS, 22.0, "New album").clicked() {
                    self.dialog = Some(Dialog::NewAlbum { name: String::new(), photos: Vec::new() });
                }
                if self.scanning {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(format!("Scanning {} / {}", self.photos.len(), self.scan_total)).color(TEXT_DIM));
                    ui.spinner();
                }
            });
        });
    }

    fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let paths = self.selected_paths();
        if icon_button(ui, icon::X, 22.0, "Clear selection (Esc)").clicked() {
            self.selected.clear();
        }
        ui.label(egui::RichText::new(format!("{} selected", paths.len())).size(18.0).color(TEXT));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(16.0);
            if icon_button(ui, icon::SELECTION_ALL, 22.0, "Select all").clicked() {
                self.selected.extend(self.order.iter().copied());
            }
            if let Page::Album(i) = self.page {
                if icon_button(ui, icon::IMAGE_SQUARE, 22.0, "Remove from album").clicked() {
                    self.store.remove_from_album(i, &paths);
                    self.selected.clear();
                    self.view_dirty = true;
                }
            }
            let archived = paths.iter().all(|p| self.store.is_archived(p));
            if icon_button(ui, icon::ARCHIVE, 22.0, if archived { "Unarchive" } else { "Archive" }).clicked() {
                self.toggle_archive(&paths);
                self.selected.clear();
            }
            if self.album_menu(ui, &paths) {
                self.selected.clear();
            }
            let all_fav = paths.iter().all(|p| self.store.is_favorite(p));
            let heart = egui::RichText::new(icon::HEART).size(22.0).color(if all_fav { HEART } else { TEXT });
            if ui.add(egui::Button::new(heart).frame(false)).on_hover_text(if all_fav { "Unfavorite" } else { "Favorite" }).clicked() {
                self.toggle_favorites(&paths);
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let mut go = None;
        if nav_item(ui, icon::IMAGE, "Photos", self.page == Page::Photos, None).clicked() {
            go = Some(Page::Photos);
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(30.0);
            ui.label(egui::RichText::new("Collections").size(15.0).color(TEXT));
        });
        ui.add_space(6.0);

        let caret = if self.albums_open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
        let r = nav_item(ui, icon::BOOK, "Albums", self.page == Page::Albums, Some(caret));
        if r.clicked() {
            // The caret folds the list; the rest of the row opens the Albums page.
            if r.interact_pointer_pos().is_some_and(|p| p.x < r.rect.left() + 26.0) {
                self.albums_open = !self.albums_open;
            } else {
                go = Some(Page::Albums);
            }
        }
        if self.albums_open {
            for i in 0..self.store.albums.len() {
                let name = self.store.albums[i].name.clone();
                if sub_item(ui, &name, self.store.albums[i].photos.len(), self.page == Page::Album(i)).clicked() {
                    go = Some(Page::Album(i));
                }
            }
        }

        let items = [
            (icon::STAR, "Favorites", Page::Favorites),
            (icon::PLAY_CIRCLE, "Videos", Page::Videos),
            (icon::USERS_THREE, "People & pets", Page::People),
            (icon::DEVICE_MOBILE, "Screenshots & recordings", Page::Screenshots),
            (icon::CLOCK, "Recently added", Page::Recent),
            (icon::ARCHIVE, "Archive", Page::Archive),
        ];
        for (ic, label, page) in items {
            let on = self.page == page || (page == Page::People && matches!(self.page, Page::Person(_) | Page::Pets(_)));
            if nav_item(ui, ic, label, on, None).clicked() {
                go = Some(page);
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.label(egui::RichText::new(icon::HARD_DRIVE).size(20.0).color(TEXT));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Library").size(15.0).color(TEXT));
        });
        ui.horizontal(|ui| {
            ui.add_space(30.0);
            ui.label(
                egui::RichText::new(format!("{} photos · {}", self.photos.len(), human_bytes(self.library_bytes)))
                    .size(13.0)
                    .color(TEXT_DIM),
            );
        });
        if let Some(root) = &self.root {
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                ui.add(egui::Label::new(egui::RichText::new(root.display().to_string()).size(12.0).color(TEXT_DIM)).truncate());
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            let b = egui::Button::new(egui::RichText::new("Open another folder").color(ACCENT))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::new(1.0_f32, Color32::from_gray(110)))
                .corner_radius(CornerRadius::same(20))
                .min_size(vec2(180.0, 38.0));
            if ui.add(b).clicked() {
                self.pick_folder(ui.ctx());
            }
        });

        if let Some(p) = go {
            self.set_page(p);
        }
    }

    // ---------------------------------------------------------------- pages

    fn page_header(&mut self, ui: &mut egui::Ui) {
        let title = match self.page {
            Page::Photos | Page::Albums | Page::People => return,
            Page::Person(i) => {
                self.person_header(ui, i);
                return;
            }
            Page::Pets(Pet::Dog) => "Dogs",
            Page::Pets(Pet::Cat) => "Cats",
            Page::Favorites => "Favorites",
            Page::Videos => "Videos",
            Page::Screenshots => "Screenshots & recordings",
            Page::Recent => "Recently added",
            Page::Archive => "Archive",
            Page::Album(i) => {
                let Some(album) = self.store.albums.get(i) else { return };
                let missing = if self.scanning {
                    0
                } else {
                    album.photos.iter().filter(|p| !self.path_index.contains_key(*p)).count()
                };
                let name = album.name.clone();
                let count = album.photos.len();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&name).size(30.0).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, icon::TRASH, 22.0, "Delete album").clicked() {
                            self.dialog = Some(Dialog::DeleteAlbum(i));
                        }
                        if icon_button(ui, icon::PENCIL, 22.0, "Rename album").clicked() {
                            self.dialog = Some(Dialog::Rename { album: i, name: name.clone() });
                        }
                    });
                });
                let mut sub = format!("{count} item{}", if count == 1 { "" } else { "s" });
                if missing > 0 {
                    sub += &format!(" · {missing} outside the open folder");
                }
                ui.label(egui::RichText::new(sub).color(TEXT_DIM));
                ui.add_space(4.0);
                return;
            }
        };
        ui.label(egui::RichText::new(title).size(30.0).color(TEXT));
        let sub = match self.page {
            Page::Archive => "Archived photos are hidden from Photos.".to_string(),
            _ => format!("{} item{}", self.order.len(), if self.order.len() == 1 { "" } else { "s" }),
        };
        ui.label(egui::RichText::new(sub).color(TEXT_DIM));
        ui.add_space(4.0);
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        if self.order.is_empty() {
            let msg = if self.scanning {
                "Looking for photos…"
            } else if !self.search.trim().is_empty() {
                "No photos match your search."
            } else {
                match self.page {
                    Page::Photos => "No photos here. Use Open folder to pick where your photos are.",
                    Page::Favorites => "No favorites yet. Select photos and tap the heart, or press F in the viewer.",
                    Page::Screenshots => "No screenshots or screen recordings found.",
                    Page::Videos => "No videos in this folder.",
                    Page::Person(_) | Page::Pets(_) => "No photos.",
                    Page::Archive => "Nothing archived. Archive photos to hide them from Photos.",
                    _ => "This album is empty. Go to Photos, select some, then add them to this album.",
                }
            };
            ui.centered_and_justified(|ui| ui.label(egui::RichText::new(msg).size(15.0).color(TEXT_DIM)));
            self.loader.set_wanted(Vec::new());
            return;
        }

        let width = ui.available_width() - 12.0;
        let key = (width as i32, self.row_height as i32, self.view_version);
        if self.layout.as_ref().map(|l| l.key) != Some(key) {
            let mut layout = self.build_layout(width);
            layout.key = key;
            self.layout = Some(layout);
        }
        let layout = self.layout.take().unwrap();

        let today = Local::now().date_naive();
        let mods = ui.input(|i| i.modifiers);
        let selecting = !self.selected.is_empty();
        let mut click = None;
        let mut wanted_now = Vec::new();
        let mut wanted_soon = Vec::new();

        egui::ScrollArea::vertical()
            .id_salt(("grid", self.page.key()))
            .auto_shrink(false)
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.max_rect().min;
                ui.set_height(layout.height);
                ui.set_width(width);

                let prefetch_to = viewport.max.y + viewport.height();
                let first = layout.rows.partition_point(|r| r.y + r.h < viewport.min.y);
                for r in layout.rows[first..].iter().take_while(|r| r.y < prefetch_to) {
                    let visible = r.y < viewport.max.y;
                    match &r.kind {
                        RowKind::Memories if visible => {
                            if let Some(pos) = self.draw_memories(ui, Rect::from_min_size(origin + vec2(0.0, r.y), vec2(width, r.h)), &mut wanted_now) {
                                click = Some(Click::Open(pos));
                            }
                        }
                        RowKind::Month(title) if visible => {
                            ui.painter().text(
                                origin + vec2(0.0, r.y + MONTH_H - 22.0),
                                Align2::LEFT_BOTTOM,
                                title,
                                FontId::proportional(30.0),
                                TEXT,
                            );
                        }
                        RowKind::Memories | RowKind::Month(_) => {}
                        RowKind::Photos { label_h, img_h, items, labels } => {
                            if visible {
                                for (k, (x, gi)) in labels.iter().enumerate() {
                                    // Clip each day label to its own segment so neighbours never overlap.
                                    let end = labels.get(k + 1).map(|(nx, _)| nx - DAY_GAP).unwrap_or(width);
                                    let rect = Rect::from_min_max(origin + vec2(*x, r.y), origin + vec2(end, r.y + label_h));
                                    if self.draw_day_label(ui, rect, *gi, today, selecting) {
                                        click = Some(Click::Day(*gi));
                                    }
                                }
                            }
                            for item in items {
                                let id = self.order[item.pos];
                                let rect = Rect::from_min_size(origin + vec2(item.x, r.y + label_h), vec2(item.w, *img_h));

                                if !self.thumbs.contains_key(&id) && !self.failed.contains(&id) {
                                    let job = make_job(self.gen, id, &self.photos[id]);
                                    if visible { wanted_now.push(job) } else { wanted_soon.push(job) }
                                }
                                if !visible {
                                    continue;
                                }

                                let resp = ui.interact(rect, ui.id().with(("photo", id)), Sense::click());
                                let is_sel = self.selected.contains(&id);
                                let is_fav = self.store.is_favorite(&self.photos[id].path);
                                let check = Rect::from_center_size(rect.min + vec2(18.0, 18.0), vec2(22.0, 22.0));
                                let badge = self.photos[id].is_video.then(|| self.photos[id].duration.map(|d| video::format_duration(d as f64)).unwrap_or_default());
                                draw_cell(
                                    ui,
                                    rect,
                                    self.thumbs.get_mut(&id).map(|(t, seen)| {
                                        *seen = self.frame;
                                        &*t
                                    }),
                                    self.failed.contains(&id),
                                    is_sel,
                                    is_fav,
                                    resp.hovered() || selecting,
                                    badge,
                                );

                                if resp.clicked() {
                                    let on_check = resp.interact_pointer_pos().is_some_and(|p| check.expand(4.0).contains(p));
                                    click = Some(if mods.shift && self.anchor.is_some() {
                                        Click::Range(item.pos)
                                    } else if mods.command || on_check || selecting {
                                        Click::Toggle(item.pos)
                                    } else {
                                        Click::Open(item.pos)
                                    });
                                }
                            }
                        }
                    }
                }
            });
        self.layout = Some(layout);

        // Workers take from the back: off-screen prefetch first in the list, then visible
        // rows bottom-to-top, so the top-left of the screen fills in first.
        wanted_soon.reverse();
        wanted_now.reverse();
        wanted_soon.extend(wanted_now);
        self.loader.set_wanted(wanted_soon);

        match click {
            Some(Click::Open(pos)) => self.open_viewer(pos, ui.ctx()),
            Some(Click::Toggle(pos)) => {
                let id = self.order[pos];
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.anchor = Some(pos);
            }
            Some(Click::Range(pos)) => {
                let a = self.anchor.unwrap_or(pos);
                let (lo, hi) = (a.min(pos), a.max(pos));
                self.selected.extend(self.order[lo..=hi].iter().copied());
                self.anchor = Some(pos);
            }
            Some(Click::Day(gi)) => {
                let g = &self.groups[gi];
                let ids: Vec<usize> = self.order[g.start..g.start + g.len].to_vec();
                if ids.iter().all(|id| self.selected.contains(id)) {
                    for id in ids {
                        self.selected.remove(&id);
                    }
                } else {
                    self.selected.extend(ids);
                }
            }
            None => {}
        }
    }

    /// Day heading such as "Wed, Aug 26". Clicking it (or its circle) selects the whole day.
    fn draw_day_label(&self, ui: &egui::Ui, rect: Rect, gi: usize, today: NaiveDate, selecting: bool) -> bool {
        let g = &self.groups[gi];
        let resp = ui.interact(rect, ui.id().with(("day", gi)), Sense::click());
        let all_sel = selecting && self.order[g.start..g.start + g.len].iter().all(|id| self.selected.contains(id));
        let painter = ui.painter().with_clip_rect(rect);
        let mut x = rect.left();
        let cy = rect.center().y + 2.0;
        if resp.hovered() || selecting {
            let c = pos2(x + 10.0, cy);
            if all_sel {
                painter.circle_filled(c, 10.0, ACCENT);
                draw_tick(&painter, c, CONTENT_BG);
            } else {
                painter.circle_stroke(c, 9.0, Stroke::new(1.5_f32, TEXT_DIM));
            }
            x += 28.0;
        }
        painter.text(pos2(x, cy), Align2::LEFT_CENTER, day_label(g.date, today), FontId::proportional(15.0), TEXT);
        resp.clicked()
    }

    /// The "N years ago" strip. Returns the `order` position to open when a card is clicked.
    fn draw_memories(&mut self, ui: &egui::Ui, area: Rect, wanted: &mut Vec<Job>) -> Option<usize> {
        let card = vec2(((area.width() - 3.0 * 18.0) / 4.0).clamp(220.0, 380.0), MEMORIES_H - 50.0);
        let mut out = None;
        for (k, m) in self.memories.iter().enumerate() {
            let x = area.left() + 4.0 + k as f32 * (card.x + 18.0);
            if x + card.x > area.right() {
                break;
            }
            let rect = Rect::from_min_size(pos2(x, area.top() + 12.0), card);
            let id = self.order[m.pos];
            let resp = ui.interact(rect, ui.id().with(("memory", k)), Sense::click());
            match self.thumbs.get_mut(&id) {
                Some((t, seen)) => {
                    *seen = self.frame;
                    egui::Image::from_texture(&*t)
                        .uv(fill_uv(t.size_vec2(), rect.size()))
                        .corner_radius(CornerRadius::same(16))
                        .paint_at(ui, rect);
                }
                None => {
                    if !self.failed.contains(&id) {
                        wanted.push(make_job(self.gen, id, &self.photos[id]));
                    }
                    ui.painter().rect_filled(rect, CornerRadius::same(16), FIELD_BG);
                }
            }
            // Darken the bottom so the white caption stays readable on bright photos.
            let shade = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 80.0), rect.max);
            ui.painter().rect_filled(shade, CornerRadius { nw: 0, ne: 0, sw: 16, se: 16 }, Color32::from_black_alpha(110));
            if resp.hovered() {
                ui.painter().rect_stroke(rect, CornerRadius::same(16), Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
            }
            let title = if m.years == 1 { "1 year ago".to_string() } else { format!("{} years ago", m.years) };
            ui.painter().text(rect.left_bottom() + vec2(18.0, -38.0), Align2::LEFT_BOTTOM, title, FontId::proportional(24.0), Color32::WHITE);
            ui.painter().text(
                rect.left_bottom() + vec2(18.0, -16.0),
                Align2::LEFT_BOTTOM,
                format!("{} · {} photo{}", m.date.format("%-d %B %Y"), m.count, if m.count == 1 { "" } else { "s" }),
                FontId::proportional(14.0),
                Color32::from_gray(225),
            );
            if resp.clicked() {
                out = Some(m.pos);
            }
        }
        out
    }

    fn albums_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Albums").size(30.0).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = egui::Button::new(egui::RichText::new(format!("{}  Create album", icon::PLUS)).color(ACCENT))
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0_f32, Color32::from_gray(110)))
                    .corner_radius(CornerRadius::same(20))
                    .min_size(vec2(150.0, 38.0));
                if ui.add(b).clicked() {
                    self.dialog = Some(Dialog::NewAlbum { name: String::new(), photos: Vec::new() });
                }
            });
        });
        ui.add_space(16.0);

        if self.store.albums.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new("No albums yet. Select photos (Ctrl/Cmd-click), then use + to add them to an album.").color(TEXT_DIM))
            });
            self.loader.set_wanted(Vec::new());
            return;
        }

        let mut open = None;
        let mut wanted = Vec::new();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(20.0, 20.0);
                for (i, album) in self.store.albums.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(vec2(CARD, CARD + 52.0), Sense::click());
                    let cover_rect = Rect::from_min_size(rect.min, vec2(CARD, CARD));
                    // Cover is the most recently added photo that is in the open folder.
                    let cover = album.photos.iter().rev().find_map(|p| self.path_index.get(p).copied());
                    match cover.and_then(|id| self.thumbs.get_mut(&id).map(|(t, seen)| (id, t, seen))) {
                        Some((_, t, seen)) => {
                            *seen = self.frame;
                            egui::Image::from_texture(&*t)
                                .uv(fill_uv(t.size_vec2(), cover_rect.size()))
                                .corner_radius(CornerRadius::same(14))
                                .paint_at(ui, cover_rect);
                        }
                        None => {
                            if let Some(id) = cover.filter(|id| !self.failed.contains(id)) {
                                wanted.push(make_job(self.gen, id, &self.photos[id]));
                            }
                            ui.painter().rect_filled(cover_rect, CornerRadius::same(14), FIELD_BG);
                            ui.painter().text(cover_rect.center(), Align2::CENTER_CENTER, icon::IMAGES, FontId::proportional(40.0), TEXT_DIM);
                        }
                    }
                    if resp.hovered() {
                        ui.painter().rect_stroke(cover_rect, CornerRadius::same(14), Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
                    }
                    let painter = ui.painter();
                    painter.text(pos2(rect.min.x + 2.0, cover_rect.max.y + 10.0), Align2::LEFT_TOP, &album.name, FontId::proportional(16.0), TEXT);
                    painter.text(
                        pos2(rect.min.x + 2.0, cover_rect.max.y + 32.0),
                        Align2::LEFT_TOP,
                        format!("{} item{}", album.photos.len(), if album.photos.len() == 1 { "" } else { "s" }),
                        FontId::proportional(13.0),
                        TEXT_DIM,
                    );
                    if resp.clicked() {
                        open = Some(i);
                    }
                }
            });
        });
        wanted.reverse();
        self.loader.set_wanted(wanted);
        if let Some(i) = open {
            self.set_page(Page::Album(i));
        }
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(v) = &self.viewer else { return };
        let pos = v.pos;
        let id = self.order[pos];
        let photo = &self.photos[id];
        let path = photo.path.clone();
        let is_fav = self.store.is_favorite(&path);
        let is_archived = self.store.is_archived(&path);

        let typing = ctx.memory(|m| m.focused().is_some()) || self.dialog.is_some();
        let (close, prev, next, fav_key) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::F),
            )
        });

        let mut go: Option<usize> = None;
        let mut want_close = close && !typing;
        let mut toggle_fav = fav_key && !typing;
        let mut toggle_archive = false;
        let mut remove = false;
        let title = photo.taken.format("%a, %-d %b %Y · %H:%M").to_string();
        let from_exif = photo.from_exif;
        let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if icon_button(ui, icon::ARROW_LEFT, 24.0, "Back (Esc)").clicked() {
                want_close = true;
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).size(15.0).color(TEXT));
                let mut sub = file_name;
                if !from_exif {
                    sub += "  ·  file date (no camera date)";
                }
                ui.label(egui::RichText::new(sub).size(12.0).color(TEXT_DIM));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(format!("{} / {}", pos + 1, self.order.len())).color(TEXT_DIM));
                if let Page::Album(_) = self.page {
                    if icon_button(ui, icon::IMAGE_SQUARE, 22.0, "Remove from album").clicked() {
                        remove = true;
                    }
                }
                if icon_button(ui, icon::ARCHIVE, 22.0, if is_archived { "Unarchive" } else { "Archive" }).clicked() {
                    toggle_archive = true;
                }
                self.album_menu(ui, std::slice::from_ref(&path));
                let heart = egui::RichText::new(if is_fav { "♥" } else { icon::HEART }).size(22.0).color(if is_fav { HEART } else { TEXT });
                if ui.add(egui::Button::new(heart).frame(false)).on_hover_text(if is_fav { "Unfavorite (F)" } else { "Favorite (F)" }).clicked() {
                    toggle_fav = true;
                }
            });
        });
        ui.add_space(6.0);

        if !typing && prev && pos > 0 {
            go = Some(pos - 1);
        }
        if !typing && next && pos + 1 < self.order.len() {
            go = Some(pos + 1);
        }

        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, Color32::BLACK);
        if self.photos[id].is_video {
            self.video_frame(&ctx, typing);
        }
        let v = self.viewer.as_ref().unwrap();
        // Show the blurry thumbnail while the full image decodes.
        let tex = v.texture.as_ref().or_else(|| self.thumbs.get(&id).map(|(t, _)| t));
        if let Some(tex) = tex {
            let size = tex.size_vec2();
            // Photos never upscale past 100%; video frames and placeholder thumbnails fill the view.
            let cap = if v.texture.is_some() && v.player.is_none() { 1.0 } else { f32::MAX };
            let scale = (area.width() / size.x).min(area.height() / size.y).min(cap);
            let rect = Rect::from_center_size(area.center(), size * scale);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        if v.failed {
            ui.painter().text(area.center(), Align2::CENTER_CENTER, "Couldn't open this image", FontId::proportional(18.0), Color32::LIGHT_GRAY);
        } else if v.texture.is_none() && v.video_error.is_none() {
            ui.put(Rect::from_center_size(area.center(), vec2(32.0, 32.0)), egui::Spinner::new());
        }

        // Big round arrows on the sides, like Google Photos.
        for (dir, ic, enabled) in [(-1, icon::CARET_LEFT, pos > 0), (1, icon::CARET_RIGHT, pos + 1 < self.order.len())] {
            if !enabled {
                continue;
            }
            let c = if dir < 0 { pos2(area.left() + 40.0, area.center().y) } else { pos2(area.right() - 40.0, area.center().y) };
            let r = Rect::from_center_size(c, vec2(48.0, 48.0));
            let resp = ui.interact(r, ui.id().with(("nav", dir)), Sense::click());
            let alpha = if resp.hovered() { 200 } else { 120 };
            ui.painter().circle_filled(c, 24.0, Color32::from_black_alpha(alpha));
            ui.painter().text(c, Align2::CENTER_CENTER, ic, FontId::proportional(24.0), Color32::WHITE);
            if resp.clicked() {
                go = Some(if dir < 0 { pos - 1 } else { pos + 1 });
            }
        }

        if self.photos[id].is_video {
            self.video_controls(ui, area, &path);
        }

        if toggle_fav {
            self.toggle_favorites(std::slice::from_ref(&path));
        }
        if toggle_archive {
            self.toggle_archive(std::slice::from_ref(&path));
        }
        if remove {
            if let Page::Album(i) = self.page {
                self.store.remove_from_album(i, std::slice::from_ref(&path));
                self.view_dirty = true;
            }
        }
        if want_close {
            self.viewer = None;
        } else if let Some(p) = go {
            self.open_viewer(p, &ctx);
        }
    }

    /// Pulls the current video frame into the viewer texture and handles Space.
    fn video_frame(&mut self, ctx: &egui::Context, typing: bool) {
        let Some(v) = &mut self.viewer else { return };
        let Some(p) = &mut v.player else { return };
        if let Some(err) = p.error() {
            v.video_error = Some(err);
            v.player = None;
            return;
        }
        if !typing && ctx.input(|i| i.key_pressed(Key::Space)) {
            if p.playing() { p.pause() } else { p.play() }
        }
        if let Some(frame) = p.take_frame() {
            match &mut v.texture {
                Some(t) => t.set(frame, TextureOptions::LINEAR),
                None => v.texture = Some(ctx.load_texture("video", frame, TextureOptions::LINEAR)),
            }
        }
        p.ended();
        // Frames are polled, so keep repainting while the video runs.
        if p.playing() {
            ctx.request_repaint();
        }
    }

    /// Play/pause, time, seek bar, mute and "open in default player", over the bottom of the video.
    fn video_controls(&mut self, ui: &mut egui::Ui, area: Rect, path: &std::path::Path) {
        let Some(v) = &mut self.viewer else { return };

        if let Some(err) = &v.video_error {
            let c = area.center();
            ui.painter().rect_filled(Rect::from_center_size(c, vec2(420.0, 150.0)), CornerRadius::same(16), Color32::from_black_alpha(190));
            ui.painter().text(c + vec2(0.0, -40.0), Align2::CENTER_CENTER, "Can't play this video here", FontId::proportional(18.0), Color32::WHITE);
            ui.painter().text(c + vec2(0.0, -14.0), Align2::CENTER_CENTER, err, FontId::proportional(13.0), TEXT_DIM);
            let b = egui::Button::new(egui::RichText::new(format!("{}  Open in default player", icon::ARROW_SQUARE_OUT)).color(Color32::from_rgb(6, 46, 111)))
                .fill(ACCENT)
                .corner_radius(CornerRadius::same(18));
            if ui.put(Rect::from_center_size(c + vec2(0.0, 30.0), vec2(240.0, 38.0)), b).clicked() {
                video::open_externally(path);
            }
            return;
        }
        let Some(p) = &mut v.player else { return };

        let bar = Rect::from_min_max(pos2(area.left() + 90.0, area.bottom() - 70.0), pos2(area.right() - 90.0, area.bottom() - 16.0));
        ui.painter().rect_filled(bar, CornerRadius::same(14), Color32::from_black_alpha(160));
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(14.0, 6.0))), |ui| {
            ui.horizontal_centered(|ui| {
                let (ic, hint) = if p.playing() { (icon::PAUSE, "Pause (Space)") } else { (icon::PLAY, "Play (Space)") };
                if icon_button(ui, ic, 26.0, hint).clicked() {
                    if p.playing() { p.pause() } else { p.play() }
                }
                let (pos, dur) = (p.position(), p.duration());
                ui.label(
                    egui::RichText::new(format!("{} / {}", video::format_duration(pos), video::format_duration(dur)))
                        .color(Color32::WHITE)
                        .monospace(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, icon::ARROW_SQUARE_OUT, 22.0, "Open in default player").clicked() {
                        p.pause();
                        video::open_externally(path);
                    }
                    let (ic, hint) = if p.muted() { (icon::SPEAKER_SLASH, "Unmute") } else { (icon::SPEAKER_HIGH, "Mute") };
                    if icon_button(ui, ic, 22.0, hint).clicked() {
                        let m = !p.muted();
                        p.set_muted(m);
                    }
                    ui.spacing_mut().slider_width = (ui.available_width() - 16.0).max(60.0);
                    let mut at = pos;
                    let seek = ui.add(egui::Slider::new(&mut at, 0.0..=dur.max(0.01)).show_value(false));
                    if seek.changed() {
                        p.seek(at);
                    }
                });
            });
        });
    }

    /// Starts finding faces and pets in photos not analysed yet (runs after every scan).
    fn start_analysis(&mut self, ctx: &egui::Context) {
        let Some(models) = people::models_dir() else {
            self.people_problem = Some(
                "Face recognition models aren't installed. Run scripts/fetch-models.sh, or put the three .onnx files in a \"models\" folder next to the app.".into(),
            );
            return;
        };
        self.people_problem = None;
        let jobs: Vec<people::Job> = self
            .photos
            .iter()
            .filter(|p| !p.is_video && !self.people_index.is_current(&p.path, p.mtime, p.size))
            .map(|p| people::Job { path: p.path.clone(), mtime: p.mtime, size: p.size })
            .collect();
        self.analysed = 0;
        self.analyse_total = jobs.len();
        self.people_dirty = true;
        self.analysing = !jobs.is_empty();
        if self.analysing {
            people::spawn(jobs, self.gen, self.analysis_gen.clone(), models, self.analysis_tx.clone(), ctx.clone());
        }
    }

    fn poll_people(&mut self, now: f64) {
        while let Ok(msg) = self.analysis_rx.try_recv() {
            match msg {
                people::Msg::Analysed { gen, path, analysis } if gen == self.gen => {
                    self.people_index.entries.insert(path, analysis);
                    self.analysed += 1;
                    self.people_dirty = true;
                    self.people_index_dirty = true;
                }
                people::Msg::Finished { gen } if gen == self.gen => {
                    self.analysing = false;
                    self.people_dirty = true;
                }
                people::Msg::Failed { gen, error } if gen == self.gen => {
                    self.analysing = false;
                    self.people_problem = Some(format!("Couldn't load the face models: {error}"));
                }
                _ => {}
            }
        }
        if self.people_index_dirty && (!self.analysing || now - self.last_index_save > 10.0) {
            self.people_index.save();
            self.people_index_dirty = false;
            self.last_index_save = now;
        }
        // Grouping is quick but not free, so while analysis runs it happens every few seconds.
        if self.people_dirty && !self.scanning && (!self.analysing || now - self.last_cluster > 3.0) {
            self.last_cluster = now;
            self.recluster();
        }
    }

    /// Re-groups faces into people, keeping an open person open by following its cover face.
    fn recluster(&mut self) {
        self.people_dirty = false;
        let open = match self.page {
            Page::Person(i) => self.people.get(i).map(|p| p.cover.clone()),
            _ => None,
        };
        self.people = people::cluster(
            &self.people_index,
            |p| self.path_index.contains_key(p),
            &self.store.face_names,
            &self.store.hidden_faces,
        );
        self.photo_people.clear();
        for person in &self.people {
            let Some(name) = &person.name else { continue };
            for photo in &person.photos {
                let names = self.photo_people.entry(photo.clone()).or_default();
                names.push(' ');
                names.push_str(&name.to_lowercase());
            }
        }
        if let Page::Person(_) = self.page {
            match open.and_then(|c| self.people.iter().position(|p| p.faces.contains(&c))) {
                Some(i) => self.page = Page::Person(i),
                None => self.page = Page::People,
            }
        }
        self.view_dirty = true;
    }

    /// The round avatar picture of a face, loading at most `budget` new ones per frame.
    fn face_texture(&mut self, ctx: &egui::Context, face: &FaceRef, budget: &mut usize) -> Option<TextureHandle> {
        let key = face.key();
        if let Some(t) = self.avatars.get(&key) {
            return t.clone();
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let a = self.people_index.entries.get(&face.path)?;
        let tex = people::crop_path(&face.path, a.mtime, a.size, face.index)
            .and_then(|p| image::open(p).ok())
            .map(|img| {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                ctx.load_texture(format!("face {key}"), egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), TextureOptions::LINEAR)
            });
        self.avatars.insert(key, tex.clone());
        tex
    }

    /// Photos of one kind of pet in the open folder, newest first.
    fn pet_photos(&self, kind: Pet) -> Vec<usize> {
        let mut ids: Vec<usize> = (0..self.photos.len())
            .filter(|&id| {
                let p = &self.photos[id];
                !self.store.is_archived(&p.path) && self.people_index.entries.get(&p.path).is_some_and(|a| a.pet == Some(kind))
            })
            .collect();
        ids.sort_by(|&a, &b| self.photos[b].taken.cmp(&self.photos[a].taken));
        ids
    }

    fn people_page(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("People & pets").size(30.0).color(TEXT));
        let shown = self.people.iter().filter(|p| self.show_hidden_people || !p.hidden).count();
        let status = if let Some(problem) = &self.people_problem {
            problem.clone()
        } else if self.analysing {
            format!("Looking for faces and pets… {} of {} photos", self.analysed, self.analyse_total)
        } else {
            format!("{shown} {} · faces are recognised on this computer, nothing is uploaded", if shown == 1 { "person" } else { "people" })
        };
        ui.label(egui::RichText::new(status).color(TEXT_DIM));
        if self.analysing && self.analyse_total > 0 {
            ui.add(egui::ProgressBar::new(self.analysed as f32 / self.analyse_total as f32).desired_width(360.0));
        }
        ui.add_space(14.0);

        let ctx = ui.ctx().clone();
        let mut budget = 24usize;
        let mut go = None;
        let mut wanted = Vec::new();
        let cards: Vec<(usize, FaceRef, Option<String>, usize, bool)> = self
            .people
            .iter()
            .enumerate()
            .filter(|(_, p)| self.show_hidden_people || !p.hidden)
            .map(|(i, p)| (i, p.cover.clone(), p.name.clone(), p.photos.len(), p.hidden))
            .collect();
        let hidden_count = self.people.iter().filter(|p| p.hidden).count();
        let pets: Vec<(Pet, &str, Vec<usize>)> = [(Pet::Dog, "Dogs"), (Pet::Cat, "Cats")]
            .into_iter()
            .map(|(k, label)| (k, label, self.pet_photos(k)))
            .filter(|(_, _, ids)| !ids.is_empty())
            .collect();

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(egui::RichText::new("People").size(18.0).color(TEXT));
            ui.add_space(8.0);
            if cards.is_empty() {
                let msg = if self.analysing { "People will appear here as photos are analysed." } else { "No people found yet. A person shows up once their face is in at least two photos." };
                ui.label(egui::RichText::new(msg).color(TEXT_DIM));
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(18.0, 18.0);
                for (i, cover, name, count, hidden) in &cards {
                    let (rect, resp) = ui.allocate_exact_size(vec2(124.0, 170.0), Sense::click());
                    let circle = Rect::from_center_size(pos2(rect.center().x, rect.top() + 58.0), vec2(112.0, 112.0));
                    match self.face_texture(&ctx, cover, &mut budget) {
                        Some(t) => egui::Image::from_texture(&t).corner_radius(CornerRadius::same(56)).paint_at(ui, circle),
                        None => {
                            ui.painter().circle_filled(circle.center(), 56.0, FIELD_BG);
                            ui.painter().text(circle.center(), Align2::CENTER_CENTER, icon::USER, FontId::proportional(40.0), TEXT_DIM);
                        }
                    }
                    if *hidden {
                        ui.painter().circle_filled(circle.center(), 56.0, Color32::from_black_alpha(150));
                        ui.painter().text(circle.center(), Align2::CENTER_CENTER, icon::EYE_SLASH, FontId::proportional(28.0), TEXT);
                    }
                    if resp.hovered() {
                        ui.painter().circle_stroke(circle.center(), 59.0, Stroke::new(2.0_f32, ACCENT));
                    }
                    let (label, color) = match name {
                        Some(n) => (n.clone(), TEXT),
                        None => ("Add a name".to_string(), ACCENT),
                    };
                    let text_clip = ui.painter().with_clip_rect(rect);
                    text_clip.text(pos2(rect.center().x, circle.bottom() + 16.0), Align2::CENTER_CENTER, label, FontId::proportional(14.5), color);
                    text_clip.text(
                        pos2(rect.center().x, circle.bottom() + 36.0),
                        Align2::CENTER_CENTER,
                        format!("{count} photo{}", if *count == 1 { "" } else { "s" }),
                        FontId::proportional(12.5),
                        TEXT_DIM,
                    );
                    if resp.clicked() {
                        go = Some(Page::Person(*i));
                    }
                }
            });

            if !pets.is_empty() {
                ui.add_space(24.0);
                ui.label(egui::RichText::new("Pets").size(18.0).color(TEXT));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(18.0, 18.0);
                    for (kind, label, ids) in &pets {
                        let (rect, resp) = ui.allocate_exact_size(vec2(124.0, 170.0), Sense::click());
                        let circle = Rect::from_center_size(pos2(rect.center().x, rect.top() + 58.0), vec2(112.0, 112.0));
                        let id = ids[0];
                        match self.thumbs.get_mut(&id) {
                            Some((t, seen)) => {
                                *seen = self.frame;
                                egui::Image::from_texture(&*t)
                                    .uv(fill_uv(t.size_vec2(), circle.size()))
                                    .corner_radius(CornerRadius::same(56))
                                    .paint_at(ui, circle);
                            }
                            None => {
                                if !self.failed.contains(&id) {
                                    wanted.push(make_job(self.gen, id, &self.photos[id]));
                                }
                                ui.painter().circle_filled(circle.center(), 56.0, FIELD_BG);
                            }
                        }
                        let ic = if *kind == Pet::Dog { icon::DOG } else { icon::CAT };
                        let badge = circle.right_bottom() - vec2(16.0, 16.0);
                        ui.painter().circle_filled(badge, 15.0, CONTENT_BG);
                        ui.painter().text(badge, Align2::CENTER_CENTER, ic, FontId::proportional(18.0), TEXT);
                        if resp.hovered() {
                            ui.painter().circle_stroke(circle.center(), 59.0, Stroke::new(2.0_f32, ACCENT));
                        }
                        ui.painter().text(pos2(rect.center().x, circle.bottom() + 16.0), Align2::CENTER_CENTER, *label, FontId::proportional(14.5), TEXT);
                        ui.painter().text(
                            pos2(rect.center().x, circle.bottom() + 36.0),
                            Align2::CENTER_CENTER,
                            format!("{} photo{}", ids.len(), if ids.len() == 1 { "" } else { "s" }),
                            FontId::proportional(12.5),
                            TEXT_DIM,
                        );
                        if resp.clicked() {
                            go = Some(Page::Pets(*kind));
                        }
                    }
                });
            }

            if hidden_count > 0 {
                ui.add_space(24.0);
                let label = if self.show_hidden_people { "Hide hidden people".to_string() } else { format!("Show {hidden_count} hidden") };
                if ui.add(egui::Button::new(egui::RichText::new(label).color(ACCENT)).frame(false)).clicked() {
                    self.show_hidden_people = !self.show_hidden_people;
                }
            }
        });
        self.loader.set_wanted(wanted);
        if let Some(p) = go {
            self.set_page(p);
        }
    }

    /// Avatar, name (click to name), photo count, and rename / hide buttons.
    fn person_header(&mut self, ui: &mut egui::Ui, i: usize) {
        let Some(p) = self.people.get(i) else { return };
        let (cover, name, count, hidden, keys) = (p.cover.clone(), p.name.clone(), p.photos.len(), p.hidden, p.keys());
        let ctx = ui.ctx().clone();
        let mut budget = 4;
        let tex = self.face_texture(&ctx, &cover, &mut budget);
        let mut rename = false;
        let mut toggle_hidden = false;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(68.0, 68.0), Sense::hover());
            match &tex {
                Some(t) => egui::Image::from_texture(t).corner_radius(CornerRadius::same(34)).paint_at(ui, r),
                None => {
                    ui.painter().circle_filled(r.center(), 34.0, FIELD_BG);
                }
            }
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(6.0);
                let (title, color) = match &name {
                    Some(n) => (n.clone(), TEXT),
                    None => ("Add a name".to_string(), ACCENT),
                };
                if ui.add(egui::Label::new(egui::RichText::new(title).size(28.0).color(color)).sense(Sense::click())).on_hover_text("Name this person").clicked() {
                    rename = true;
                }
                ui.label(egui::RichText::new(format!("{count} photo{}", if count == 1 { "" } else { "s" })).color(TEXT_DIM));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (ic, hint) = if hidden { (icon::EYE, "Show in People & pets") } else { (icon::EYE_SLASH, "Hide this person") };
                if icon_button(ui, ic, 22.0, hint).clicked() {
                    toggle_hidden = true;
                }
                if icon_button(ui, icon::PENCIL, 22.0, "Name this person").clicked() {
                    rename = true;
                }
            });
        });
        ui.add_space(6.0);
        if rename {
            self.dialog = Some(Dialog::NamePerson { person: i, name: name.unwrap_or_default() });
        }
        if toggle_hidden {
            self.store.hide_faces(&keys, !hidden);
            self.recluster();
            if !hidden {
                self.set_page(Page::People);
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else { return };
        let mut confirm = false;
        let mut cancel = false;

        let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            let name_field = |ui: &mut egui::Ui, name: &mut String| {
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("Album name")
                        .font(FontId::proportional(16.0))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
            };
            let ok_label = match dialog {
                Dialog::NewAlbum { name, photos } => {
                    ui.label(egui::RichText::new("New album").size(20.0).color(TEXT));
                    if !photos.is_empty() {
                        ui.label(egui::RichText::new(format!("{} photo{} will be added.", photos.len(), if photos.len() == 1 { "" } else { "s" })).color(TEXT_DIM));
                    }
                    ui.add_space(8.0);
                    confirm |= name_field(ui, name);
                    "Create"
                }
                Dialog::Rename { name, .. } => {
                    ui.label(egui::RichText::new("Rename album").size(20.0).color(TEXT));
                    ui.add_space(8.0);
                    confirm |= name_field(ui, name);
                    "Rename"
                }
                Dialog::NamePerson { name, .. } => {
                    ui.label(egui::RichText::new("Who is this?").size(20.0).color(TEXT));
                    ui.label(egui::RichText::new("Use the same name as another person to merge them.").color(TEXT_DIM));
                    ui.add_space(8.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(name).hint_text("Name").font(FontId::proportional(16.0)).desired_width(f32::INFINITY),
                    );
                    r.request_focus();
                    confirm |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    "Save"
                }
                Dialog::DeleteAlbum(i) => {
                    let name = self.store.albums.get(*i).map(|a| a.name.as_str()).unwrap_or("");
                    ui.label(egui::RichText::new("Delete album?").size(20.0).color(TEXT));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("\"{name}\" will be deleted. The photos themselves stay on disk.")).color(TEXT_DIM));
                    "Delete"
                }
            };
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = egui::Button::new(egui::RichText::new(ok_label).color(Color32::from_rgb(6, 46, 111)))
                    .fill(ACCENT)
                    .corner_radius(CornerRadius::same(18))
                    .min_size(vec2(90.0, 36.0));
                if ui.add(ok).clicked() {
                    confirm = true;
                }
                let no = egui::Button::new(egui::RichText::new("Cancel").color(ACCENT))
                    .frame(false)
                    .min_size(vec2(80.0, 36.0));
                if ui.add(no).clicked() {
                    cancel = true;
                }
            });
        });

        if confirm {
            match self.dialog.take() {
                Some(Dialog::NewAlbum { name, photos }) => {
                    let i = self.store.create_album(&name, &photos);
                    if photos.is_empty() {
                        self.set_page(Page::Album(i));
                    } else {
                        self.selected.clear();
                        self.view_dirty = true;
                    }
                }
                Some(Dialog::Rename { album, name }) => self.store.rename_album(album, &name),
                Some(Dialog::DeleteAlbum(i)) => {
                    self.store.delete_album(i);
                    self.set_page(Page::Albums);
                }
                Some(Dialog::NamePerson { person, name }) => {
                    if let Some(p) = self.people.get(person) {
                        let keys = p.keys();
                        self.store.name_faces(&keys, &name);
                        self.recluster();
                    }
                }
                None => {}
            }
        } else if cancel || modal.should_close() {
            self.dialog = None;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        // Trackpad pinch (or Ctrl/Cmd + scroll) resizes the photos, like Photos.app.
        let (zoom, open) = ctx.input(|i| (i.zoom_delta(), i.modifiers.command && i.key_pressed(Key::O)));
        if zoom != 1.0 {
            self.row_height = (self.row_height * zoom).clamp(120.0, 360.0);
        }
        if open {
            self.pick_folder(ctx);
        }
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (esc, all) = ctx.input(|i| (i.key_pressed(Key::Escape), i.modifiers.command && i.key_pressed(Key::A)));
        if esc {
            self.selected.clear();
        }
        if all && self.page != Page::Albums {
            self.selected.extend(self.order.iter().copied());
        }
    }
}

impl eframe::App for PhotosApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame += 1;
        self.poll(ctx);
        #[cfg(debug_assertions)]
        self.debug_hooks(ctx);

        if self.viewer.is_some() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::BLACK))
                .show(ctx, |ui| self.viewer(ui));
        } else {
            // The content panel's outer margin would otherwise show egui's default grey.
            ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.screen_rect(), 0.0, BG);
            self.shortcuts(ctx);
            egui::TopBottomPanel::top("top")
                .exact_height(72.0)
                .frame(egui::Frame::new().fill(BG))
                .show_separator_line(false)
                .show(ctx, |ui| self.top_bar(ui));
            egui::SidePanel::left("side")
                .exact_width(SIDEBAR_W)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 12, right: 12, top: 0, bottom: 12 }))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.sidebar(ui));
                });
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(CONTENT_BG)
                        .corner_radius(CornerRadius::same(18))
                        .inner_margin(Margin { left: 22, right: 6, top: 14, bottom: 0 })
                        .outer_margin(Margin { left: 0, right: 14, top: 0, bottom: 14 }),
                )
                .show(ctx, |ui| {
                    self.page_header(ui);
                    if self.page == Page::Albums {
                        self.albums_page(ui);
                    } else if self.page == Page::People {
                        self.people_page(ui);
                    } else {
                        self.grid(ui);
                    }
                });
        }
        self.dialogs(ctx);
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = Color32::from_rgb(40, 40, 42);
    v.extreme_bg_color = FIELD_BG;
    v.faint_bg_color = FIELD_BG;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = PILL_BG;
    v.selection.stroke = Stroke::new(1.0_f32, PILL_TEXT);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(20);
    v.menu_corner_radius = CornerRadius::same(10);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.widgets.inactive.weak_bg_fill = FIELD_BG;
    v.widgets.hovered.weak_bg_fill = HOVER_BG;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.button_padding = vec2(10.0, 6.0);
        s.spacing.menu_margin = Margin::same(8);
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.5));
    });
}

/// Sidebar row: icon + label, with a rounded pill when selected. `caret` is drawn at the far left.
fn nav_item(ui: &mut egui::Ui, ic: &str, label: &str, selected: bool, caret: Option<&str>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(24), PILL_BG);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(24), HOVER_BG);
    }
    let color = if selected { PILL_TEXT } else { TEXT };
    if let Some(c) = caret {
        painter.text(pos2(rect.left() + 14.0, rect.center().y), Align2::CENTER_CENTER, c, FontId::proportional(13.0), TEXT_DIM);
    }
    painter.text(pos2(rect.left() + 40.0, rect.center().y), Align2::CENTER_CENTER, ic, FontId::proportional(22.0), color);
    let font = FontId::proportional(15.5);
    let galley = painter.layout(label.to_string(), font, color, rect.width() - 76.0);
    painter.galley(pos2(rect.left() + 70.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// Album entry nested under "Albums" in the sidebar.
fn sub_item(ui: &mut egui::Ui, name: &str, count: usize, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(18), PILL_BG);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(18), HOVER_BG);
    }
    let color = if selected { PILL_TEXT } else { TEXT };
    let clip = painter.with_clip_rect(rect.shrink2(vec2(12.0, 0.0)));
    clip.text(pos2(rect.left() + 70.0, rect.center().y), Align2::LEFT_CENTER, name, FontId::proportional(14.0), color);
    painter.text(pos2(rect.right() - 16.0, rect.center().y), Align2::RIGHT_CENTER, count.to_string(), FontId::proportional(12.5), TEXT_DIM);
    resp
}

fn icon_button(ui: &mut egui::Ui, ic: &str, size: f32, hint: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(ic).size(size).color(TEXT)).frame(false)).on_hover_text(hint)
}

fn make_job(gen: u64, id: usize, p: &Photo) -> Job {
    Job { gen, id, path: p.path.clone(), key: cache_key(&p.path, p.mtime, p.size) }
}

fn draw_tick(painter: &egui::Painter, c: egui::Pos2, color: Color32) {
    painter.add(Shape::line(vec![c + vec2(-5.0, 0.0), c + vec2(-1.5, 3.5), c + vec2(5.0, -3.5)], Stroke::new(2.0_f32, color)));
}

fn draw_cell(
    ui: &egui::Ui,
    rect: Rect,
    tex: Option<&TextureHandle>,
    failed: bool,
    selected: bool,
    favorite: bool,
    show_check: bool,
    video_badge: Option<String>,
) {
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(40, 52, 70));
    }
    // Selected photos shrink a little inside a tinted frame, like Google Photos.
    let img_rect = if selected { rect.shrink(rect.height().min(rect.width()) * 0.08) } else { rect };
    match tex {
        Some(t) => {
            let radius = if selected { 10 } else { 0 };
            egui::Image::from_texture(t)
                .uv(fill_uv(t.size_vec2(), img_rect.size()))
                .corner_radius(CornerRadius::same(radius))
                .paint_at(ui, img_rect);
        }
        None => {
            painter.rect_filled(img_rect, 0.0, Color32::from_rgb(36, 37, 39));
            if failed {
                painter.text(img_rect.center(), Align2::CENTER_CENTER, icon::IMAGE, FontId::proportional(28.0), TEXT_DIM);
            }
        }
    }

    if show_check || selected {
        let c = rect.min + vec2(18.0, 18.0);
        if selected {
            painter.circle_filled(c, 11.0, ACCENT);
            draw_tick(painter, c, CONTENT_BG);
        } else {
            // Gradient-ish shade so the white ring shows on light photos too.
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(rect.width(), 44.0f32.min(rect.height()))),
                0.0,
                Color32::from_black_alpha(50),
            );
            painter.circle_stroke(c, 10.0, Stroke::new(2.0_f32, Color32::WHITE));
        }
    }
    if let Some(text) = video_badge {
        // Duration and a play mark in the top-right corner, like Google Photos.
        let font = FontId::proportional(13.0);
        let label = if text.is_empty() { icon::PLAY.to_string() } else { format!("{text}  {}", icon::PLAY) };
        let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
        let pill = Rect::from_min_size(
            pos2(img_rect.max.x - galley.size().x - 18.0, img_rect.min.y + 8.0),
            galley.size() + vec2(12.0, 6.0),
        );
        painter.rect_filled(pill, CornerRadius::same(10), Color32::from_black_alpha(130));
        painter.galley(pill.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    }
    if favorite {
        let p = pos2(img_rect.min.x + 8.0, img_rect.max.y - 6.0);
        painter.text(p + vec2(1.0, 1.0), Align2::LEFT_BOTTOM, "♥", FontId::proportional(18.0), Color32::from_black_alpha(140));
        painter.text(p, Align2::LEFT_BOTTOM, "♥", FontId::proportional(18.0), Color32::WHITE);
    }
}

/// Every word must match the file name, a person's name, or some form of the date ("2024", "october", "oct", "sunday", "2024-10-06").
fn matches_search(p: &Photo, terms: &[String], people: &str) -> bool {
    let name = p.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut date = p.taken.format("%Y %B %b %A %a %-d %Y-%m-%d").to_string().to_lowercase();
    if p.is_video {
        date += " video videos";
    }
    terms.iter().all(|t| name.contains(t.as_str()) || date.contains(t.as_str()) || people.contains(t.as_str()))
}

fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    match (today - date).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if date.year() == today.year() => date.format("%a, %b %-d").to_string(),
        _ => date.format("%a, %b %-d, %Y").to_string(),
    }
}

/// UV rect that crops the texture to fill `target` without stretching, like `object-fit: cover`.
fn fill_uv(tex: egui::Vec2, target: egui::Vec2) -> Rect {
    let (ta, ra) = (tex.x / tex.y, target.x / target.y);
    if ta > ra {
        let u = ra / ta;
        let x0 = (1.0 - u) / 2.0;
        Rect::from_min_max(pos2(x0, 0.0), pos2(x0 + u, 1.0))
    } else {
        let v = ta / ra;
        let y0 = (1.0 - v) / 2.0;
        Rect::from_min_max(pos2(0.0, y0), pos2(1.0, y0 + v))
    }
}

fn human_bytes(b: u64) -> String {
    let gb = b as f64 / 1e9;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", b as f64 / 1e6) }
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

/// Dev-only hooks for screenshots without touching the mouse:
/// `BANANA_SCREENSHOT=out.png` saves the window once the scan settles, then quits.
/// `BANANA_PAGE=favorites|albums|album<N>|archive|recent|screenshots`, `BANANA_SELECT=0,1,2`,
/// `BANANA_SEARCH=text` and `BANANA_OPEN=<n>` set the scene first.
#[cfg(debug_assertions)]
impl PhotosApp {
    fn debug_hooks(&mut self, ctx: &egui::Context) {
        let Some(out) = std::env::var_os("BANANA_SCREENSHOT") else { return };
        let var = |k: &str| std::env::var(k).ok();
        let t = ctx.input(|i| i.time);
        if t > 1.5 && !self.scanning && !self.analysing && !self.people_dirty && !self.debug_done {
            self.debug_done = true;
            self.debug_done_at = t;
            match var("BANANA_PAGE").as_deref() {
                Some("favorites") => self.set_page(Page::Favorites),
                Some("videos") => self.set_page(Page::Videos),
                Some("people") => self.set_page(Page::People),
                Some("dogs") => self.set_page(Page::Pets(Pet::Dog)),
                Some("cats") => self.set_page(Page::Pets(Pet::Cat)),
                Some(p) if p.starts_with("person") => self.set_page(Page::Person(p[6..].parse().unwrap_or(0))),
                Some("albums") => self.set_page(Page::Albums),
                Some("archive") => self.set_page(Page::Archive),
                Some("recent") => self.set_page(Page::Recent),
                Some("screenshots") => self.set_page(Page::Screenshots),
                Some(p) if p.starts_with("album") => self.set_page(Page::Album(p[5..].parse().unwrap_or(0))),
                _ => {}
            }
            if let Some(s) = var("BANANA_SEARCH") {
                self.search = s;
                self.view_dirty = true;
            }
            self.apply_view(ctx);
            if let Some(list) = var("BANANA_SELECT") {
                for pos in list.split(',').filter_map(|s| s.parse::<usize>().ok()) {
                    if let Some(&id) = self.order.get(pos) {
                        self.selected.insert(id);
                    }
                }
            }
            if let Some(n) = var("BANANA_OPEN").and_then(|s| s.parse().ok()) {
                self.open_viewer(n, ctx);
            }
        }
        if self.debug_done && t > self.debug_done_at + 2.5 && !self.debug_shot_sent {
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
