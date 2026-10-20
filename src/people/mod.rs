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

pub use vision::{CLASSIFIER, DETECTOR, RECOGNIZER};

/// Two faces this similar (cosine of their fingerprints) are taken to be the same person.
/// SFace's own verification threshold is 0.363; a little stricter keeps strangers apart.
const SAME_PERSON: f32 = 0.40;
/// Groups whose average faces are this similar are merged after the first pass.
const MERGE_GROUPS: f32 = 0.48;
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
}

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
        self.entries.get(path).is_some_and(|a| a.mtime == mtime && a.size == size)
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
                let analysis = analyse(&models, job).unwrap_or(Analysis { mtime: job.mtime, size: job.size, faces: Vec::new(), pet: None });
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
    Some(Analysis { mtime: job.mtime, size: job.size, faces, pet })
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

/// Groups the faces of photos for which `present` is true into people. Faces are taken
/// clearest first, each joining the most similar group or starting a new one; similar groups
/// are then merged. Names and hidden flags come from the per-face maps.
pub fn cluster(
    index: &Index,
    present: impl Fn(&Path) -> bool,
    names: &HashMap<String, String>,
    hidden: &HashSet<String>,
) -> Vec<Person> {
    struct F<'a> {
        r: FaceRef,
        emb: &'a [f32],
        quality: f32,
    }
    let mut faces: Vec<F> = index
        .entries
        .iter()
        .filter(|(p, _)| present(p))
        .flat_map(|(p, a)| {
            a.faces.iter().enumerate().map(move |(i, f)| F {
                r: FaceRef { path: p.clone(), index: i },
                emb: &f.embedding,
                quality: f.score * (f.bbox[2] * f.bbox[3]).sqrt(),
            })
        })
        .collect();
    faces.sort_by(|a, b| b.quality.total_cmp(&a.quality).then_with(|| a.r.path.cmp(&b.r.path)));

    // (sum of member fingerprints, members)
    let mut groups: Vec<(Vec<f32>, Vec<usize>)> = Vec::new();
    for (i, f) in faces.iter().enumerate() {
        let best = groups
            .iter()
            .enumerate()
            .map(|(g, (sum, _))| (g, dot(&normalized(sum), f.emb)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((g, sim)) if sim >= SAME_PERSON => {
                groups[g].0.iter_mut().zip(f.emb).for_each(|(s, e)| *s += e);
                groups[g].1.push(i);
            }
            _ => groups.push((f.emb.to_vec(), vec![i])),
        }
    }
    loop {
        let centers: Vec<Vec<f32>> = groups.iter().map(|(s, _)| normalized(s)).collect();
        let pair = (0..groups.len())
            .flat_map(|a| (a + 1..groups.len()).map(move |b| (a, b)))
            .find(|&(a, b)| dot(&centers[a], &centers[b]) >= MERGE_GROUPS);
        let Some((a, b)) = pair else { break };
        let (sum_b, members_b) = groups.remove(b);
        groups[a].0.iter_mut().zip(&sum_b).for_each(|(s, e)| *s += e);
        groups[a].1.extend(members_b);
    }

    // Groups the user gave the same name are one person.
    let mut people: Vec<Person> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (_, members) in groups {
        let refs: Vec<FaceRef> = members.iter().map(|&i| faces[i].r.clone()).collect();
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
