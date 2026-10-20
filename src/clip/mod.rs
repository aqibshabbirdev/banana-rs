//! Search photos by what's in them ("beach", "red car"): CLIP turns each photo (during
//! analysis) and the search text into 512 numbers in the same space; photos whose numbers
//! point the same way as the text's match it. All on this machine.

pub mod tokenizer;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use tract_onnx::prelude::*;

/// Text half of OpenAI CLIP ViT-B/32 (MIT), int8-quantised by Xenova, and its tokenizer.
pub const TEXT_MODEL: &str = "clip_text_quantized.onnx";
pub const TOKENIZER: &str = "clip_tokenizer.json";

/// A photo is a result when it is at least this much more similar to "a photo of <text>" than
/// to just "a photo". Raw CLIP similarities run high for some pictures whatever the text, so
/// the plain caption is the baseline; tuned on a mixed library, where photos of the thing
/// searched for gain 0.02-0.08 and searches for things not in it ("giraffe", "guitar") stay below.
const MIN_GAIN: f32 = 0.02;
/// ...and within this much of the best match, so a search for something the library clearly
/// has doesn't trail off into loosely related photos.
const BELOW_BEST: f32 = 0.04;
const MAX_RESULTS: usize = 300;

/// The text model and tokenizer, if installed next to the other models.
pub fn text_files() -> Option<(PathBuf, PathBuf)> {
    let dir = crate::people::models_dir()?;
    let (model, tok) = (dir.join(TEXT_MODEL), dir.join(TOKENIZER));
    (model.is_file() && tok.is_file()).then_some((model, tok))
}

/// L2-normalises in place.
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter_mut().for_each(|x| *x /= n);
}

pub struct TextEncoder {
    tokenizer: tokenizer::Tokenizer,
    plan: Arc<TypedRunnableModel>,
}

impl TextEncoder {
    pub fn load(model: &Path, tokenizer: &Path) -> Result<Self, String> {
        let tokenizer = tokenizer::Tokenizer::from_file(tokenizer)?;
        let plan = (|| {
            tract_onnx::onnx()
                .with_ignore_output_shapes(true)
                .with_ignore_value_info(true)
                .model_for_path(model)?
                .with_input_fact(0, i64::fact([1, tokenizer::CONTEXT]).into())?
                .into_optimized()?
                .into_runnable()
        })()
        .map_err(|e: TractError| e.to_string())?;
        Ok(Self { tokenizer, plan })
    }

    /// What search ranks photos by: the embedding of "a photo of <text>" minus that of
    /// "a photo", so a photo's dot product with it is its gain over the baseline caption.
    pub fn query(&self, text: &str) -> Result<Vec<f32>, String> {
        let t = self.embed(&format!("a photo of {text}"))?;
        let base = self.embed("a photo")?;
        Ok(t.iter().zip(&base).map(|(a, b)| a - b).collect())
    }

    /// The text's unit-length embedding. Ids are padded with the end marker to 77, like CLIP
    /// was trained: the model reads its output at the first end marker, and its causal mask
    /// keeps the padding from affecting that position.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut ids: Vec<i64> = self.tokenizer.encode(text).into_iter().map(i64::from).collect();
        ids.resize(tokenizer::CONTEXT, tokenizer::END as i64);
        let run = || -> TractResult<Vec<f32>> {
            let input = tract_ndarray::Array2::from_shape_vec((1, tokenizer::CONTEXT), ids)?;
            let out = self.plan.run(tvec!(Tensor::from(input).into()))?;
            Ok(out[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec())
        };
        let mut v = run().map_err(|e| e.to_string())?;
        normalize(&mut v);
        Ok(v)
    }
}

/// Turns search text into embeddings on a background thread, loading the model on first use.
pub struct QueryWorker {
    tx: Sender<String>,
    pub rx: Receiver<(String, Result<Vec<f32>, String>)>,
}

impl QueryWorker {
    pub fn spawn(model: PathBuf, tokenizer: PathBuf, ctx: eframe::egui::Context) -> Self {
        let (tx, jobs) = channel::<String>();
        let (done, rx) = channel();
        std::thread::spawn(move || {
            let encoder = TextEncoder::load(&model, &tokenizer);
            while let Ok(mut text) = jobs.recv() {
                // Typing queues a query per key press; only the newest one matters.
                while let Ok(newer) = jobs.try_recv() {
                    text = newer;
                }
                let result = encoder.as_ref().map_err(Clone::clone).and_then(|e| e.query(&text));
                if done.send((text, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { tx, rx }
    }

    pub fn request(&self, text: String) {
        let _ = self.tx.send(text);
    }
}

/// One photo's embedding as 8-bit values times a scale: 516 bytes instead of 2 KB, and the
/// cosine with a text embedding stays within about 0.002 of the full-precision one.
pub struct Stored {
    mtime: u64,
    size: u64,
    scale: f32,
    /// Empty when the photo couldn't be read, so it isn't retried on every launch.
    values: Vec<i8>,
}

impl Stored {
    pub fn quantize(mtime: u64, size: u64, v: &[f32]) -> Self {
        let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
        let scale = if max > 0.0 { max / 127.0 } else { 1.0 };
        Self { mtime, size, scale, values: v.iter().map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8).collect() }
    }

    pub fn dot(&self, query: &[f32]) -> f32 {
        self.scale * self.values.iter().zip(query).map(|(&q, t)| q as f32 * t).sum::<f32>()
    }
}

/// Photo embeddings, kept in memory and saved to `clip.bin` in the data folder.
/// File: b"BCLIP\0\0\x01", count u32, then per photo: path length u32, path bytes, mtime u64,
/// size u64, scale f32, value count u16, values i8. Little-endian.
#[derive(Default)]
pub struct Embeddings {
    entries: HashMap<PathBuf, Stored>,
    pub dirty: bool,
}

const MAGIC: &[u8; 8] = b"BCLIP\0\0\x01";

impl Embeddings {
    fn file() -> Option<PathBuf> {
        Some(crate::store::data_dir()?.join("clip.bin"))
    }

    pub fn load() -> Self {
        let entries = Self::file().and_then(|f| std::fs::read(f).ok()).and_then(|b| decode(&b)).unwrap_or_default();
        Self { entries, dirty: false }
    }

    pub fn save(&mut self) {
        let Some(file) = Self::file() else { return };
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let tmp = file.with_extension("bin.tmp");
        let ok = std::fs::File::create(&tmp).and_then(|f| {
            let mut w = std::io::BufWriter::new(f);
            w.write_all(&encode(&self.entries))?;
            w.flush()
        });
        if ok.is_ok() && std::fs::rename(&tmp, &file).is_ok() {
            self.dirty = false;
        }
    }

    pub fn insert(&mut self, path: PathBuf, mtime: u64, size: u64, embedding: &[f32]) {
        self.entries.insert(path, Stored::quantize(mtime, size, embedding));
        self.dirty = true;
    }

    /// True when the photo has an entry for this version of the file.
    pub fn has(&self, path: &Path, mtime: u64, size: u64) -> bool {
        self.entries.get(path).is_some_and(|s| s.mtime == mtime && s.size == size)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.values().all(|s| s.values.is_empty())
    }

    pub fn get(&self, path: &Path, mtime: u64, size: u64) -> Option<&Stored> {
        self.entries.get(path).filter(|s| s.mtime == mtime && s.size == size && !s.values.is_empty())
    }
}

/// Indices into `candidates` of the photos that match `query` (from `TextEncoder::query`), best first.
pub fn rank<'a>(query: &[f32], candidates: impl Iterator<Item = (usize, &'a Stored)>) -> Vec<usize> {
    let mut scored: Vec<(usize, f32)> = candidates.map(|(i, s)| (i, s.dot(query))).filter(|(_, s)| *s >= MIN_GAIN).collect();
    let best = scored.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
    scored.retain(|(_, s)| *s >= best - BELOW_BEST);
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(MAX_RESULTS);
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes()
}

#[cfg(unix)]
fn path_from(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> &[u8] {
    p.to_str().unwrap_or_default().as_bytes()
}

#[cfg(not(unix))]
fn path_from(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}

fn encode(entries: &HashMap<PathBuf, Stored>) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + entries.len() * 640);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (path, s) in entries {
        let p = path_bytes(path);
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(p);
        out.extend_from_slice(&s.mtime.to_le_bytes());
        out.extend_from_slice(&s.size.to_le_bytes());
        out.extend_from_slice(&s.scale.to_le_bytes());
        out.extend_from_slice(&(s.values.len() as u16).to_le_bytes());
        out.extend(s.values.iter().map(|&v| v as u8));
    }
    out
}

fn decode(mut b: &[u8]) -> Option<HashMap<PathBuf, Stored>> {
    fn take<const N: usize>(b: &mut &[u8]) -> Option<[u8; N]> {
        let mut a = [0; N];
        b.read_exact(&mut a).ok()?;
        Some(a)
    }
    fn bytes<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (head, rest) = (b.get(..n)?, b.get(n..)?);
        *b = rest;
        Some(head)
    }
    if &take::<8>(&mut b)? != MAGIC {
        return None;
    }
    let count = u32::from_le_bytes(take(&mut b)?) as usize;
    let mut entries = HashMap::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let len = u32::from_le_bytes(take(&mut b)?) as usize;
        let path = path_from(bytes(&mut b, len)?);
        let mtime = u64::from_le_bytes(take(&mut b)?);
        let size = u64::from_le_bytes(take(&mut b)?);
        let scale = f32::from_le_bytes(take(&mut b)?);
        let n = u16::from_le_bytes(take(&mut b)?) as usize;
        let values = bytes(&mut b, n)?.iter().map(|&v| v as i8).collect();
        entries.insert(path, Stored { mtime, size, scale, values });
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn quantized_dot_is_close() {
        let a = unit(&(0..512).map(|i| ((i * 37 % 101) as f32 - 50.0) / 7.0).collect::<Vec<_>>());
        let b = unit(&(0..512).map(|i| ((i * 53 % 89) as f32 - 44.0) / 3.0).collect::<Vec<_>>());
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let s = Stored::quantize(0, 0, &a);
        assert!((s.dot(&b) - exact).abs() < 0.003, "{} vs {exact}", s.dot(&b));
        assert!((s.dot(&a) - 1.0).abs() < 0.003);
    }

    /// Photos whose gain over the baseline caption is `gains` (query = text - baseline).
    fn photos_with_gains(gains: &[f32]) -> (Vec<f32>, Vec<Stored>) {
        let q = vec![1.0, 0.0, 0.0];
        let photos = gains.iter().map(|&g| Stored::quantize(0, 0, &unit(&[g, (1.0 - g * g).sqrt(), 0.0]))).collect();
        (q, photos)
    }

    #[test]
    fn ranks_best_first_above_threshold() {
        let (q, photos) = photos_with_gains(&[0.06, 0.01, 0.05, -0.03, 0.015, 0.03]);
        // 0.01, 0.015 and -0.03 are below MIN_GAIN; 0.03 is within BELOW_BEST of the best.
        assert_eq!(rank(&q, photos.iter().enumerate()), vec![0, 2, 5]);
        let (q, photos) = photos_with_gains(&[0.09, 0.03, 0.06]);
        // 0.03 is more than BELOW_BEST under the best match.
        assert_eq!(rank(&q, photos.iter().enumerate()), vec![0, 2]);
    }

    #[test]
    fn nothing_matches_a_search_for_something_missing() {
        let (q, photos) = photos_with_gains(&[0.012, -0.01, 0.0, 0.016]);
        assert!(rank(&q, photos.iter().enumerate()).is_empty());
    }

    #[test]
    fn file_round_trip() {
        let mut entries = HashMap::new();
        entries.insert(PathBuf::from("/photos/a.jpg"), Stored::quantize(5, 6, &[0.5, -0.5, 0.25]));
        entries.insert(PathBuf::from("/photos/broken.jpg"), Stored { mtime: 1, size: 2, scale: 1.0, values: Vec::new() });
        let back = decode(&encode(&entries)).unwrap();
        assert_eq!(back.len(), 2);
        let a = &back[Path::new("/photos/a.jpg")];
        assert_eq!((a.mtime, a.size, a.values.clone()), (5, 6, vec![127, -127, 64]));
        assert!(back[Path::new("/photos/broken.jpg")].values.is_empty());
        assert!(decode(b"nope").is_none());
        // A cut-off file is ignored rather than half-read.
        let bytes = encode(&entries);
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
    }

    /// Padding to 77 gives the same embedding as the unpadded prompts in doc_prompts.json.
    #[test]
    fn text_encoder_matches_precomputed_prompts() {
        let Some((model, tok)) = text_files() else { return };
        let enc = TextEncoder::load(&model, &tok).unwrap();
        #[derive(serde::Deserialize)]
        struct P {
            prompt: String,
            embedding: Vec<f32>,
        }
        let prompts: Vec<P> = serde_json::from_str(include_str!("../people/doc_prompts.json")).unwrap();
        for p in prompts.iter().take(5) {
            let v = enc.embed(&p.prompt).unwrap();
            let cos: f32 = v.iter().zip(&p.embedding).map(|(a, b)| a * b).sum();
            assert!(cos > 0.999, "{}: {cos}", p.prompt);
        }
    }

    /// Prints how a folder of photos ranks for some searches, for tuning the thresholds:
    ///   BANANA_EVAL_DIR=~/photos BANANA_EVAL_QUERIES="dog,cat" cargo test --release search_eval -- --ignored --nocapture
    #[test]
    #[ignore]
    fn search_eval() {
        let dir = PathBuf::from(std::env::var("BANANA_EVAL_DIR").expect("BANANA_EVAL_DIR"));
        let models = crate::people::models_dir().expect("models");
        let vision = crate::people::vision::Models::load(&models).unwrap();
        let (model, tok) = text_files().unwrap();
        let text = TextEncoder::load(&model, &tok).unwrap();
        let mut photos: Vec<(String, Stored)> = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for f in files {
            let Some(img) = crate::thumbs::decode(&f, 1280) else { continue };
            let Some(e) = vision.clip(&img.to_rgb8()).unwrap() else { continue };
            photos.push((f.file_name().unwrap().to_string_lossy().into_owned(), Stored::quantize(0, 0, &e)));
        }
        let queries = std::env::var("BANANA_EVAL_QUERIES").unwrap_or_else(|_| "dog,cat,receipt,man in a suit".into());
        for q in queries.split(',') {
            let t = text.query(q).unwrap();
            let mut all: Vec<(f32, &str)> = photos.iter().map(|(n, s)| (s.dot(&t), n.as_str())).collect();
            all.sort_by(|a, b| b.0.total_cmp(&a.0));
            let hits = rank(&t, photos.iter().map(|(_, s)| s).enumerate());
            let line: Vec<String> = all.iter().take(hits.len() + 2).map(|(g, n)| format!("{g:+.3} {n}")).collect();
            println!("{q:>20}: {} | {}", hits.len(), line.join(", "));
        }
    }
}
