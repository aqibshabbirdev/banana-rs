//! People & pets: find faces, group them into people, and spot dogs and cats. Everything runs
//! on this machine; results are cached in `people.json` so each photo is analysed once.

mod vision;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use vision::{CLASSIFIER, CLIP, DETECTOR, RECOGNIZER};

// Grouping settings, tuned on a real 3,500-photo family library (4,087 faces): the old
// "join the nearest group average" approach put ~900 photos of different people in one group,
// because a big group's average face looks like everyone.
/// Two faces at least this similar (cosine of their SFace fingerprints) are linked.
const LINK: f32 = 0.70;
/// After grouping, a face whose average similarity to the rest of its group is below this
/// only got in through a chain of look-alikes, and is taken out.
const MIN_GROUP_AVERAGE: f32 = 0.55;
/// Only clear faces are grouped: small or unsure ones have washed-out fingerprints that bridge
/// different people. They stay in their photos, just not in a person.
const GROUP_MIN_WIDTH: f32 = 0.06;
const GROUP_MIN_SCORE: f32 = 0.88;
/// Faces smaller than this (pixels, in the 1280px analysis copy) are too blurry to recognise.
const MIN_FACE: f32 = 28.0;
const ANALYSIS_SIZE: u32 = 1280;
/// In photos of pets, faces need this detector score to count as a person.
const PET_PHOTO_FACE: f32 = 0.9;
const CROP_SIZE: u32 = 160;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pet {
    Dog,
    Cat,
}

/// Google Photos-style document groups.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DocKind {
    Receipt,
    Notes,
    Social,
    Recipe,
    Identity,
}

impl DocKind {
    pub const ALL: [DocKind; 5] = [DocKind::Receipt, DocKind::Notes, DocKind::Social, DocKind::Recipe, DocKind::Identity];

    pub fn label(self) -> &'static str {
        match self {
            DocKind::Receipt => "Receipts",
            DocKind::Notes => "Notes",
            DocKind::Social => "Social",
            DocKind::Recipe => "Recipes & menus",
            DocKind::Identity => "Identity",
        }
    }

    /// Extra words a search can match.
    pub fn keywords(self) -> &'static str {
        match self {
            DocKind::Receipt => "document receipt receipts bill invoice",
            DocKind::Notes => "document notes note handwriting whiteboard",
            DocKind::Social => "document social post meme chat",
            DocKind::Recipe => "document recipe recipes menu menus",
            DocKind::Identity => "document identity id card passport license licence",
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Face {
    /// x, y, w, h as fractions of the photo.
    pub bbox: [f32; 4],
    pub score: f32,
    pub embedding: Vec<f32>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Analysis {
    pub mtime: u64,
    pub size: u64,
    pub faces: Vec<Face>,
    pub pet: Option<Pet>,
    #[serde(default)]
    pub doc: Option<DocKind>,
    /// Bumped when analysis gains a new kind of result, so older entries are redone.
    #[serde(default)]
    pub version: u32,
    /// CLIP embedding for search. Not saved in people.json: the app moves it into the compact
    /// clip.bin (see crate::clip::Embeddings).
    #[serde(skip)]
    pub clip: Option<Vec<f32>>,
}

/// 2 added Documents, 3 CLIP embeddings for search.
pub const ANALYSIS_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Default)]
pub struct Index {
    #[serde(default)]
    pub entries: HashMap<PathBuf, Analysis>,
}

impl Index {
    fn file() -> Option<PathBuf> {
        Some(crate::store::data_dir()?.join("people.json"))
    }

    pub fn load() -> Self {
        Self::file()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(file) = Self::file() else { return };
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let tmp = file.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_vec(self) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }

    /// True when the photo was analysed and hasn't changed since.
    pub fn is_current(&self, path: &Path, mtime: u64, size: u64) -> bool {
        self.entries.get(path).is_some_and(|a| a.mtime == mtime && a.size == size && a.version >= ANALYSIS_VERSION)
    }
}

/// One face in one photo.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FaceRef {
    pub path: PathBuf,
    pub index: usize,
}

impl FaceRef {
    /// Stable id used to remember names and hidden people.
    pub fn key(&self) -> String {
        format!("{}#{}", self.path.display(), self.index)
    }
}

pub struct Person {
    pub name: Option<String>,
    pub hidden: bool,
    pub faces: Vec<FaceRef>,
    pub photos: HashSet<PathBuf>,
    /// The clearest face, used as the round avatar.
    pub cover: FaceRef,
}

impl Person {
    pub fn keys(&self) -> Vec<String> {
        self.faces.iter().map(FaceRef::key).collect()
    }
}

/// Folder holding the three .onnx files: next to the app (macOS bundle Resources, a `models`
/// folder beside the binary, ../share/banana-rs/models), the data folder, or the source tree.
pub fn models_dir() -> Option<PathBuf> {
    let complete = |d: &Path| [DETECTOR, RECOGNIZER, CLASSIFIER].iter().all(|f| d.join(f).is_file());
    let mut candidates = Vec::new();
    if let Some(d) = std::env::var_os("BANANA_MODELS") {
        candidates.push(PathBuf::from(d));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("../Resources/models"));
        candidates.push(dir.join("models"));
        candidates.push(dir.join("../share/banana-rs/models"));
    }
    if let Some(d) = crate::store::data_dir() {
        candidates.push(d.join("models"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/models"));
    candidates.into_iter().find(|d| complete(d))
}

/// Small square picture of a face, made during analysis, shown as the person's avatar.
pub fn crop_path(path: &Path, mtime: u64, size: u64, index: usize) -> Option<PathBuf> {
    let key = crate::thumbs::cache_key(path, mtime, size);
    Some(dirs::cache_dir()?.join("banana-rs").join("faces").join(format!("{key:016x}_{index}.jpg")))
}

pub struct Job {
    pub path: PathBuf,
    pub mtime: u64,
    pub size: u64,
}

pub enum Msg {
    Analysed { gen: u64, path: PathBuf, analysis: Analysis },
    Finished { gen: u64 },
    Failed { gen: u64, error: String },
}

/// Analyses `jobs` on background threads (half the cores, so the app stays responsive).
/// Stops early once `current` moves past `gen`, e.g. when another folder is opened.
pub fn spawn(jobs: Vec<Job>, gen: u64, current: Arc<AtomicU64>, models: PathBuf, tx: Sender<Msg>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let models = match vision::Models::load(&models) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(Msg::Failed { gen, error: e.to_string() });
                ctx.request_repaint();
                return;
            }
        };
        let threads = std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(2).max(1);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        pool.install(|| {
            jobs.par_iter().for_each(|job| {
                if current.load(Ordering::Relaxed) != gen {
                    return;
                }
                let analysis = analyse(&models, job).unwrap_or(Analysis {
                    mtime: job.mtime,
                    size: job.size,
                    faces: Vec::new(),
                    pet: None,
                    doc: None,
                    version: ANALYSIS_VERSION,
                    clip: None,
                });
                let _ = tx.send(Msg::Analysed { gen, path: job.path.clone(), analysis });
                ctx.request_repaint();
            });
        });
        let _ = tx.send(Msg::Finished { gen });
        ctx.request_repaint();
    });
}

fn analyse(models: &vision::Models, job: &Job) -> Option<Analysis> {
    let img = crate::thumbs::decode(&job.path, ANALYSIS_SIZE)?.to_rgb8();
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let pet = models.pet(&img).ok().flatten();
    // The detector also fires on dog and cat faces, a bit less confidently than on people.
    let min_score = if pet.is_some() { PET_PHOTO_FACE } else { 0.0 };
    let mut faces = Vec::new();
    for det in models.detect(&img).ok()?.into_iter().filter(|d| d.w >= MIN_FACE && d.h >= MIN_FACE && d.score >= min_score) {
        let Ok(embedding) = models.embed(&img, &det) else { continue };
        save_crop(&img, &det, crop_path(&job.path, job.mtime, job.size, faces.len()));
        faces.push(Face { bbox: [det.x / iw, det.y / ih, det.w / iw, det.h / ih], score: det.score, embedding });
    }
    let clip = models.clip(&img).ok().flatten();
    let doc = clip.as_deref().and_then(vision::Models::document);
    Some(Analysis { mtime: job.mtime, size: job.size, faces, pet, doc, version: ANALYSIS_VERSION, clip })
}

/// Square crop around the face with some margin, for the round avatar.
fn save_crop(img: &image::RgbImage, det: &vision::Detection, out: Option<PathBuf>) {
    let Some(out) = out else { return };
    let side = (det.w.max(det.h) * 1.6).min(img.width().min(img.height()) as f32);
    let (cx, cy) = (det.x + det.w / 2.0, det.y + det.h / 2.0);
    let x = (cx - side / 2.0).clamp(0.0, img.width() as f32 - side) as u32;
    let y = (cy - side / 2.0).clamp(0.0, img.height() as f32 - side) as u32;
    let crop = image::imageops::crop_imm(img, x, y, side as u32, side as u32).to_image();
    let crop = image::imageops::resize(&crop, CROP_SIZE, CROP_SIZE, image::imageops::FilterType::Triangle);
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = crop.save_with_format(out, image::ImageFormat::Jpeg);
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

/// Everything grouping needs, copied out of the app so it can run on a background thread.
pub struct Snapshot {
    faces: Vec<(FaceRef, Vec<f32>, f32)>,
    names: HashMap<String, String>,
    hidden: HashSet<String>,
}

/// The clear faces of photos for which `present` is true, plus the names and hidden flags.
pub fn snapshot(index: &Index, present: impl Fn(&Path) -> bool, names: &HashMap<String, String>, hidden: &HashSet<String>) -> Snapshot {
    let mut faces: Vec<(FaceRef, Vec<f32>, f32)> = index
        .entries
        .iter()
        .filter(|(p, _)| present(p))
        .flat_map(|(p, a)| {
            a.faces
                .iter()
                .enumerate()
                .filter(|(_, f)| f.bbox[2] >= GROUP_MIN_WIDTH && f.score >= GROUP_MIN_SCORE)
                .map(move |(i, f)| (FaceRef { path: p.clone(), index: i }, normalized(&f.embedding), f.score * (f.bbox[2] * f.bbox[3]).sqrt()))
        })
        .collect();
    // Clearest first, so each person's cover is their best face; ties by path for stable output.
    faces.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.path.cmp(&b.0.path)).then(a.0.index.cmp(&b.0.index)));
    Snapshot { faces, names: names.clone(), hidden: hidden.clone() }
}

/// Chinese whispers: link faces at least `LINK` similar, then let every face repeatedly take
/// the label its links carry most weight for. Unlike averaging, a big group can't swallow
/// everyone; then members only loosely tied to their group are dropped.
fn group(faces: &[(FaceRef, Vec<f32>, f32)]) -> Vec<Vec<usize>> {
    let n = faces.len();
    let links: Vec<Vec<(usize, f32)>> = (0..n)
        .into_par_iter()
        .map(|i| {
            (0..n)
                .filter(|&j| j != i)
                .filter_map(|j| {
                    let s = dot(&faces[i].1, &faces[j].1);
                    (s >= LINK).then_some((j, s))
                })
                .collect()
        })
        .collect();

    let mut label: Vec<usize> = (0..n).collect();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..30 {
        // Same shuffled order every run, so groups don't change from one launch to the next.
        let mut order: Vec<usize> = (0..n).collect();
        for k in (1..n).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(k, (seed % (k as u64 + 1)) as usize);
        }
        let mut changed = false;
        for &i in &order {
            if links[i].is_empty() {
                continue;
            }
            let mut weight: HashMap<usize, f32> = HashMap::new();
            for &(j, s) in &links[i] {
                *weight.entry(label[j]).or_default() += s;
            }
            let best = weight.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0))).unwrap().0;
            if best != label[i] {
                label[i] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut by_label: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, l) in label.into_iter().enumerate() {
        by_label.entry(l).or_default().push(i);
    }
    let mut groups = Vec::new();
    for mut members in by_label.into_values() {
        members.sort_unstable();
        if members.len() >= 3 {
            let mut sum = vec![0f32; faces[members[0]].1.len()];
            for &i in &members {
                sum.iter_mut().zip(&faces[i].1).for_each(|(s, e)| *s += e);
            }
            // Mean similarity to the others = (dot(face, sum) - 1) / (n - 1) for unit vectors.
            let k = (members.len() - 1) as f32;
            members.retain(|&i| (dot(&faces[i].1, &sum) - 1.0) / k >= MIN_GROUP_AVERAGE);
        }
        if !members.is_empty() {
            groups.push(members);
        }
    }
    groups
}

/// Groups faces into people. Names and hidden flags come from the per-face maps. Run it on a
/// background thread: it compares every pair of faces.
pub fn cluster(snapshot: Snapshot) -> Vec<Person> {
    let Snapshot { faces, names, hidden } = snapshot;
    let groups = group(&faces);
    let faces: Vec<FaceRef> = faces.into_iter().map(|(r, _, _)| r).collect();
    let names = &names;
    let hidden = &hidden;

    // Groups the user gave the same name are one person.
    let mut people: Vec<Person> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for members in groups {
        let refs: Vec<FaceRef> = members.iter().map(|&i| faces[i].clone()).collect();
        let mut votes: HashMap<&String, usize> = HashMap::new();
        for r in &refs {
            if let Some(n) = names.get(&r.key()) {
                *votes.entry(n).or_default() += 1;
            }
        }
        let name = votes.into_iter().max_by_key(|(_, c)| *c).map(|(n, _)| n.clone());
        let is_hidden = refs.iter().filter(|r| hidden.contains(&r.key())).count() * 2 > refs.len();
        let photos: HashSet<PathBuf> = refs.iter().map(|r| r.path.clone()).collect();

        if let Some(&p) = name.as_ref().and_then(|n| by_name.get(n)) {
            let person: &mut Person = &mut people[p];
            person.photos.extend(photos);
            person.faces.extend(refs);
            continue;
        }
        if let Some(n) = &name {
            by_name.insert(n.clone(), people.len());
        }
        let cover = refs[0].clone();
        people.push(Person { name, hidden: is_hidden, faces: refs, photos, cover });
    }

    // A face seen in a single photo is usually a passer-by; keep it only if named.
    people.retain(|p| p.photos.len() >= 2 || p.name.is_some());
    people.sort_by(|a, b| b.photos.len().cmp(&a.photos.len()).then_with(|| a.name.cmp(&b.name)));
    people
}
