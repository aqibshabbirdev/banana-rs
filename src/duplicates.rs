//! Duplicates: exact copies of a file (same bytes, wherever they are), and similar shots taken
//! moments apart, such as a burst or the same scene shot a few times. Copies are found by size
//! and then a SHA-256 of the contents; similar shots by comparing the CLIP embeddings made
//! during analysis of photos taken close together. All on this machine.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use sha2::{Digest, Sha256};

/// Two photos are "similar" when their CLIP embeddings are at least this close (cosine)...
/// Tuned on a family library: retakes of the same moment score 0.90-0.99, different
/// moments of the same outing mostly below 0.88.
const SIMILAR_MIN: f32 = 0.90;
/// ...and they were taken at most this many seconds apart. Bursts and retakes are seconds apart;
/// a longer window starts joining different moments of the same outing.
const SIMILAR_WINDOW_SECS: i64 = 120;
/// A shot joins a run of similar shots when it is close to the newest one; it must also stay
/// this close to the first, so a slowly changing scene doesn't chain into one huge group.
const SIMILAR_TO_FIRST: f32 = 0.86;
/// Documents and screenshots all look alike to CLIP (pages of text, app screens), so two of
/// them only count as similar when nearly identical.
const SIMILAR_MIN_STRICT: f32 = 0.985;

/// What the finder needs to know about one photo or video.
pub struct Candidate {
    pub id: usize,
    pub path: PathBuf,
    pub size: u64,
    pub taken: NaiveDateTime,
    pub is_video: bool,
    /// Favourite or in an album: keep this one rather than another copy.
    pub preferred: bool,
    /// A document or screenshot: see `SIMILAR_MIN_STRICT`.
    pub strict: bool,
    /// Unit-length CLIP embedding, when analysis made one.
    pub embedding: Option<Vec<f32>>,
}

pub struct DupGroup {
    /// Photo ids, the one to keep first.
    pub ids: Vec<usize>,
    /// Every item is a byte-for-byte copy of the first.
    pub exact: bool,
    /// Bytes freed by removing all but the first.
    pub saving: u64,
}

/// Finds groups of copies and similar shots, newest first. Runs for a while on a big
/// library (it reads every file that shares its size with another), so call it off the UI thread.
pub fn find(cands: &[Candidate]) -> Vec<DupGroup> {
    let mut sets = Sets::new(cands.len());

    // Exact copies: only files of the same size can be equal, so only those are read.
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate().filter(|(_, c)| c.size > 0) {
        by_size.entry(c.size).or_default().push(i);
    }
    let mut hash: Vec<Option<[u8; 32]>> = vec![None; cands.len()];
    for same in by_size.values().filter(|v| v.len() > 1) {
        let mut by_hash: HashMap<[u8; 32], usize> = HashMap::new();
        for &i in same {
            let Some(h) = file_hash(&cands[i].path) else { continue };
            hash[i] = Some(h);
            match by_hash.get(&h) {
                Some(&first) => sets.join(first, i),
                None => {
                    by_hash.insert(h, i);
                }
            }
        }
    }

    // Similar shots: photos in time order, each compared with the run of shots just before it.
    let mut timed: Vec<usize> = (0..cands.len()).filter(|&i| !cands[i].is_video && cands[i].embedding.is_some()).collect();
    timed.sort_by_key(|&i| cands[i].taken);
    let mut run: Vec<usize> = Vec::new();
    for &i in &timed {
        let e = cands[i].embedding.as_deref().unwrap();
        let joins = run.last().is_some_and(|&last| {
            let near = (cands[i].taken - cands[last].taken).num_seconds() <= SIMILAR_WINDOW_SECS;
            let (min, to_first) = if cands[i].strict || cands[last].strict {
                (SIMILAR_MIN_STRICT, SIMILAR_MIN_STRICT)
            } else {
                (SIMILAR_MIN, SIMILAR_TO_FIRST)
            };
            near && cosine(e, cands[last].embedding.as_deref().unwrap()) >= min
                && cosine(e, cands[run[0]].embedding.as_deref().unwrap()) >= to_first
        });
        if joins {
            sets.join(run[0], i);
        } else {
            run.clear();
        }
        run.push(i);
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..cands.len() {
        members.entry(sets.root(i)).or_default().push(i);
    }
    let mut groups: Vec<(NaiveDateTime, DupGroup)> = members
        .into_values()
        .filter(|m| m.len() > 1)
        .map(|mut m| {
            let exact = m.iter().all(|&i| hash[i].is_some() && hash[i] == hash[m[0]]);
            m.sort_by(|&a, &b| keep_order(&cands[a], &cands[b]));
            let saving = m[1..].iter().map(|&i| cands[i].size).sum();
            let newest = m.iter().map(|&i| cands[i].taken).max().unwrap();
            (newest, DupGroup { ids: m.iter().map(|&i| cands[i].id).collect(), exact, saving })
        })
        .collect();
    groups.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.saving.cmp(&a.1.saving)));
    groups.into_iter().map(|(_, g)| g).collect()
}

/// Which copy to keep, best first: one that can't be removed anyway (inside an Apple Photos
/// library), a favourite or album photo, the biggest file (most detail), then the shortest path.
fn keep_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    in_photos_library(&b.path)
        .cmp(&in_photos_library(&a.path))
        .then(b.preferred.cmp(&a.preferred))
        .then(b.size.cmp(&a.size))
        .then(a.path.as_os_str().len().cmp(&b.path.as_os_str().len()))
        .then(a.path.cmp(&b.path))
}

/// Photos inside an Apple Photos library belong to Photos.app; this app must not delete them.
pub fn in_photos_library(path: &Path) -> bool {
    path.ancestors().any(|a| a.extension().is_some_and(|e| e.eq_ignore_ascii_case("photoslibrary")))
}

/// Moves files to the system Trash: Finder's on macOS, the freedesktop one on Linux (which
/// file managers show, with "Restore").
pub fn trash_context() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        // Finder (the default) is driven by AppleScript, which the App Sandbox doesn't allow.
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn file_hash(path: &Path) -> Option<[u8; 32]> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().into())
}

/// Union-find over candidate indices.
struct Sets(Vec<usize>);

impl Sets {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra != rb {
            self.0[rb] = ra;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: usize, path: &Path, size: u64, secs: i64, emb: Option<Vec<f32>>) -> Candidate {
        let taken = chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap().naive_utc();
        Candidate { id, path: path.to_path_buf(), size, taken, is_video: false, preferred: false, strict: false, embedding: emb }
    }

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = cosine(v, v).sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn finds_copies_and_bursts_but_not_different_photos() {
        let dir = std::env::temp_dir().join(format!("banana-dups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("copy")).unwrap();
        let (a, b, c) = (dir.join("a.jpg"), dir.join("copy/a.jpg"), dir.join("c.jpg"));
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        std::fs::write(&c, b"other byte").unwrap(); // same size, different content
        let x = unit(&[1.0, 0.0, 0.0]);
        let x2 = unit(&[1.0, 0.2, 0.0]); // cosine 0.98 with x
        let y = unit(&[0.0, 1.0, 0.0]);
        let cands = vec![
            cand(0, &a, 10, 0, None),
            cand(1, &b, 10, 5000, None),
            cand(2, &c, 10, 9000, None),
            cand(3, &dir.join("burst1.jpg"), 300, 20_000, Some(x.clone())),
            cand(4, &dir.join("burst2.jpg"), 500, 20_003, Some(x2)),
            // Same scene, but long after: a different moment.
            cand(5, &dir.join("later.jpg"), 400, 30_000, Some(x)),
            // Right after the burst, but a different picture.
            cand(6, &dir.join("other.jpg"), 400, 20_005, Some(y.clone())),
            // Two pages of a document a second apart: alike to CLIP, but not the same page.
            Candidate { strict: true, ..cand(7, &dir.join("page1.jpg"), 100, 40_000, Some(y.clone())) },
            Candidate { strict: true, ..cand(8, &dir.join("page2.jpg"), 100, 40_001, Some(unit(&[0.3, 1.0, 0.0]))) },
        ];
        let groups = find(&cands);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(groups.len(), 2);
        // Newest first: the burst, keeping the bigger file.
        assert_eq!(groups[0].ids, vec![4, 3]);
        assert!(!groups[0].exact);
        assert_eq!(groups[0].saving, 300);
        // The copies, keeping the shorter path.
        assert_eq!(groups[1].ids, vec![0, 1]);
        assert!(groups[1].exact);
    }

    /// Only on Linux, where HOME can point at a throwaway folder; elsewhere it would fill the real Trash.
    #[cfg(target_os = "linux")]
    #[test]
    fn trash_moves_file_into_freedesktop_trash() {
        let home = std::env::var("HOME").unwrap();
        let f = std::env::temp_dir().join(format!("banana-trash-{}.jpg", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        trash_context().delete(&f).unwrap();
        assert!(!f.exists());
        let info = Path::new(&home).join(".local/share/Trash/info").join(format!("{}.trashinfo", f.file_name().unwrap().to_string_lossy()));
        // /tmp may be its own filesystem; then the file goes to that disk's .Trash-<uid> instead.
        let elsewhere = std::fs::read_dir("/tmp").unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".Trash-"));
        assert!(info.exists() || elsewhere, "no trash entry for {}", f.display());
    }

    /// Dev tool: finds duplicates in a real library from its `clip.bin` and writes contact
    /// sheets, one group per row, to judge the thresholds by eye. Paths in clip.bin can be
    /// rewritten (when it was made in a container):
    ///   BANANA_DUP_CLIP=clip.bin BANANA_DUP_FROM=/pics BANANA_DUP_TO=~/piclib BANANA_DUP_OUT=out \
    ///   cargo test --release dup_eval -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dup_eval() {
        use image::{imageops, RgbImage};
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let (from, to, out) = (PathBuf::from(var("BANANA_DUP_FROM")), PathBuf::from(var("BANANA_DUP_TO")), PathBuf::from(var("BANANA_DUP_OUT")));
        std::fs::create_dir_all(&out).unwrap();
        let entries = crate::clip::Embeddings::read_file(Path::new(&var("BANANA_DUP_CLIP")));
        let mut cands = Vec::new();
        for (p, stored) in &entries {
            let host = p.strip_prefix(&from).map(|r| to.join(r)).unwrap_or_else(|_| p.clone());
            let Some(photo) = crate::library::read_photo(&host) else { continue };
            let embedding = (stored.version().1 == photo.size).then(|| stored.to_vec()).filter(|v| v.iter().any(|x| *x != 0.0));
            let strict = crate::library::is_screenshot(&host) || embedding.as_deref().and_then(crate::people::vision::Models::document).is_some();
            cands.push(Candidate { id: cands.len(), path: host, size: photo.size, taken: photo.taken, is_video: photo.is_video, preferred: false, strict, embedding });
        }
        let with_emb = cands.iter().filter(|c| c.embedding.is_some()).count();
        let t = std::time::Instant::now();
        let groups = find(&cands);
        let (exact, similar): (Vec<&DupGroup>, Vec<&DupGroup>) = groups.iter().partition(|g| g.exact);
        println!("{} photos ({} with embeddings), found in {:?}", cands.len(), with_emb, t.elapsed());
        for (name, gs) in [("exact", &exact), ("similar", &similar)] {
            let items: usize = gs.iter().map(|g| g.ids.len()).sum();
            let bytes: u64 = gs.iter().map(|g| g.saving).sum();
            let biggest = gs.iter().map(|g| g.ids.len()).max().unwrap_or(0);
            println!("{name}: {} groups, {items} items, biggest {biggest}, {} MB to free", gs.len(), bytes / 1_000_000);
        }

        const T: u32 = 150;
        let thumb = |path: &Path| -> RgbImage {
            crate::thumbs::thumbnail_jpeg(path)
                .and_then(|j| image::load_from_memory(&j).ok())
                .map(|i| imageops::resize(&i.to_rgb8(), T, T, imageops::FilterType::Triangle))
                .unwrap_or_else(|| RgbImage::new(T, T))
        };
        let sheet = |rows: Vec<Vec<usize>>, name: &str| {
            let cols = rows.iter().map(Vec::len).max().unwrap_or(1).min(8) as u32;
            let mut img = RgbImage::from_pixel(cols * (T + 4), rows.len() as u32 * (T + 10), image::Rgb([30, 30, 30]));
            for (r, row) in rows.iter().enumerate() {
                for (c, &i) in row.iter().take(8).enumerate() {
                    imageops::overlay(&mut img, &thumb(&cands[i].path), c as i64 * (T as i64 + 4), r as i64 * (T as i64 + 10));
                }
            }
            img.save(out.join(name)).unwrap();
        };
        // Rows are groups; ids are candidate indices here.
        let pick = |gs: &[&DupGroup], n: usize| gs.iter().step_by((gs.len() / n).max(1)).take(n).map(|g| g.ids.clone()).collect::<Vec<_>>();
        sheet(pick(&similar, 30), "similar.jpg");
        let list: Vec<String> = pick(&similar, 30).iter().flatten().map(|&i| cands[i].path.display().to_string()).collect();
        std::fs::write(out.join("similar.txt"), list.join("\n")).unwrap();
        sheet(pick(&exact, 20), "exact.jpg");
        let mut big: Vec<&&DupGroup> = similar.iter().collect();
        big.sort_by_key(|g| std::cmp::Reverse(g.ids.len()));
        sheet(big.iter().take(10).map(|g| g.ids.clone()).collect(), "similar_biggest.jpg");

        // Near misses: neighbours in time just under the threshold, to see what is left out.
        let mut timed: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].embedding.is_some() && !cands[i].is_video).collect();
        timed.sort_by_key(|&i| cands[i].taken);
        let mut near = Vec::new();
        for w in timed.windows(2) {
            let (a, b) = (w[0], w[1]);
            let cos = cosine(cands[a].embedding.as_deref().unwrap(), cands[b].embedding.as_deref().unwrap());
            if (cands[b].taken - cands[a].taken).num_seconds() <= SIMILAR_WINDOW_SECS && (0.84..SIMILAR_MIN).contains(&cos) && !cands[a].strict && !cands[b].strict {
                near.push((cos, vec![a, b]));
            }
        }
        near.sort_by(|x, y| y.0.total_cmp(&x.0));
        println!("near misses (cosine just under {SIMILAR_MIN}): {}", near.len());
        for (cos, _) in near.iter().take(30) {
            print!("{cos:.3} ");
        }
        println!();
        sheet(near.iter().take(30).map(|(_, v)| v.clone()).collect(), "near_misses.jpg");
    }
}
