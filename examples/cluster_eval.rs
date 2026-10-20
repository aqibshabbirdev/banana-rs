//! Compares face-grouping algorithms on a real people.json and writes contact sheets of the
//! biggest groups. Dev tool:
//!   cargo run --release --example cluster_eval -- <people.json> <out-dir> [threshold]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use image::{imageops, RgbImage};

struct Face {
    path: String,
    index: usize,
    mtime: u64,
    size: u64,
    emb: Vec<f32>,
    score: f32,
    width: f32,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

#[allow(dead_code)]
/// The old way: clearest first, join the most similar group average, then merge groups.
fn greedy(faces: &[Face]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..faces.len()).collect();
    order.sort_by(|&a, &b| (faces[b].score * faces[b].width).total_cmp(&(faces[a].score * faces[a].width)));
    let mut groups: Vec<(Vec<f32>, Vec<usize>)> = Vec::new();
    for i in order {
        let best = groups.iter().enumerate().map(|(g, (s, _))| (g, dot(&norm(s), &faces[i].emb))).max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((g, sim)) if sim >= 0.40 => {
                groups[g].0.iter_mut().zip(&faces[i].emb).for_each(|(s, e)| *s += e);
                groups[g].1.push(i);
            }
            _ => groups.push((faces[i].emb.clone(), vec![i])),
        }
    }
    loop {
        let c: Vec<Vec<f32>> = groups.iter().map(|(s, _)| norm(s)).collect();
        let pair = (0..groups.len()).flat_map(|a| (a + 1..groups.len()).map(move |b| (a, b))).find(|&(a, b)| dot(&c[a], &c[b]) >= 0.48);
        let Some((a, b)) = pair else { break };
        let (s, m) = groups.remove(b);
        groups[a].0.iter_mut().zip(&s).for_each(|(x, y)| *x += y);
        groups[a].1.extend(m);
    }
    groups.into_iter().map(|(_, m)| m).collect()
}

/// Chinese whispers on the graph of pairs at least `t` similar: every face repeatedly takes
/// the label its neighbours carry most strongly. No averages, so big groups don't swallow.
fn whispers(faces: &[Face], t: f32) -> Vec<Vec<usize>> {
    let n = faces.len();
    let mut nbrs: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in i + 1..n {
            let s = dot(&faces[i].emb, &faces[j].emb);
            if s >= t {
                nbrs[i].push((j, s));
                nbrs[j].push((i, s));
            }
        }
    }
    let mut label: Vec<usize> = (0..n).collect();
    let mut seed: u64 = 0x9e3779b97f4a7c15;
    for _ in 0..30 {
        let mut order: Vec<usize> = (0..n).collect();
        for k in (1..n).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(k, (seed % (k as u64 + 1)) as usize);
        }
        let mut changed = 0;
        for &i in &order {
            if nbrs[i].is_empty() {
                continue;
            }
            let mut w: HashMap<usize, f32> = HashMap::new();
            for &(j, s) in &nbrs[i] {
                *w.entry(label[j]).or_default() += s;
            }
            let best = w.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0))).unwrap().0;
            if best != label[i] {
                label[i] = best;
                changed += 1;
            }
        }
        if changed == 0 {
            break;
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, l) in label.into_iter().enumerate() {
        groups.entry(l).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Drops members whose average similarity to the rest of their group is below `min_avg`:
/// faces that only joined through a chain of look-alikes.
fn refine(faces: &[Face], groups: Vec<Vec<usize>>, min_avg: f32) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    for g in groups {
        if g.len() < 3 {
            out.push(g);
            continue;
        }
        let mut sum = vec![0f32; faces[0].emb.len()];
        for &i in &g {
            sum.iter_mut().zip(&faces[i].emb).for_each(|(s, e)| *s += e);
        }
        let (keep, drop): (Vec<usize>, Vec<usize>) = g.iter().partition(|&&i| {
            // mean of dot(i, j) over j != i  ==  (dot(i, sum) - 1) / (n - 1)
            (dot(&faces[i].emb, &sum) - 1.0) / (g.len() - 1) as f32 >= min_avg
        });
        out.push(keep);
        out.extend(drop.into_iter().map(|i| vec![i]));
    }
    out.retain(|g| !g.is_empty());
    out
}

fn report(name: &str, faces: &[Face], mut groups: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let photos = |g: &Vec<usize>| g.iter().map(|&i| &faces[i].path).collect::<std::collections::HashSet<_>>().len();
    let people = groups.iter().filter(|g| photos(g) >= 2).count();
    let top: Vec<usize> = groups.iter().take(12).map(photos).collect();
    println!("{name:>14}: {} groups, {people} with 2+ photos; biggest (photos): {top:?}", groups.len());
    groups
}

fn fnv(path: &str, mtime: u64, size: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.as_bytes().iter().chain(&mtime.to_le_bytes()).chain(&size.to_le_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn sheet(faces: &[Face], groups: &[Vec<usize>], out: &Path, crops: &Path) {
    const S: u32 = 72;
    const PER: usize = 12;
    let rows = groups.len().min(10) as u32;
    let mut img = RgbImage::new(S * PER as u32, S * rows);
    for (r, g) in groups.iter().take(10).enumerate() {
        // Spread the picks over the group, not just its first members.
        for k in 0..PER.min(g.len()) {
            let f = &faces[g[k * g.len() / PER.min(g.len())]];
            let p = crops.join(format!("{:016x}_{}.jpg", fnv(&f.path, f.mtime, f.size), f.index));
            if let Ok(c) = image::open(&p) {
                let c = imageops::resize(&c.to_rgb8(), S, S, imageops::FilterType::Triangle);
                imageops::replace(&mut img, &c, (k as u32 * S) as i64, (r as u32 * S) as i64);
            }
        }
    }
    img.save(out).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
    let out = PathBuf::from(&args[2]);
    let t: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    // Faces narrower than this fraction of the photo, or less certain than this, are left out.
    let min_width: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let min_score: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let crops = dirs::cache_dir().unwrap().join("banana-rs/faces");

    let mut all = Vec::new();
    for (path, a) in json["entries"].as_object().unwrap() {
        for (i, f) in a["faces"].as_array().unwrap().iter().enumerate() {
            let emb: Vec<f32> = f["embedding"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            all.push(Face {
                path: path.clone(),
                index: i,
                mtime: a["mtime"].as_u64().unwrap(),
                size: a["size"].as_u64().unwrap(),
                emb: norm(&emb),
                score: f["score"].as_f64().unwrap() as f32,
                width: f["bbox"][2].as_f64().unwrap() as f32,
            });
        }
    }
    let before = all.len();
    all.retain(|f| f.width >= min_width && f.score >= min_score);
    println!("{} faces ({} left out as too small or unsure)", all.len(), before - all.len());

    // How similar is each face to its closest other face?
    let mut nn: Vec<f32> = (0..all.len())
        .map(|i| (0..all.len()).filter(|&j| j != i).map(|j| dot(&all[i].emb, &all[j].emb)).fold(f32::MIN, f32::max))
        .collect();
    nn.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f32| nn[((nn.len() - 1) as f32 * p) as usize];
    println!("nearest-neighbour similarity quartiles: {:.2} {:.2} {:.2} {:.2}", q(0.1), q(0.25), q(0.5), q(0.75));

    let min_avg: f32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    if min_avg > 0.0 {
        let g = whispers(&all, t);
        for (k, grp) in { let mut g2 = g.clone(); g2.sort_by_key(|x| std::cmp::Reverse(x.len())); g2 }.iter().take(6).enumerate() {
            let mut sum = vec![0f32; all[0].emb.len()];
            for &i in grp { sum.iter_mut().zip(&all[i].emb).for_each(|(s, e)| *s += e); }
            let avg: f32 = grp.iter().map(|&i| (dot(&all[i].emb, &sum) - 1.0) / (grp.len().max(2) - 1) as f32).sum::<f32>() / grp.len() as f32;
            println!("  group {k}: {} faces, average similarity {avg:.2}", grp.len());
        }
        let g = report(&format!("refined {min_avg:.2}"), &all, refine(&all, g, min_avg));
        sheet(&all, &g, &out.join(format!("sheet_refined_{min_avg:.2}.png")), &crops);
        return;
    }
    for tt in [t - 0.05, t, t + 0.05] {
        let g = report(&format!("whispers {tt:.2}"), &all, whispers(&all, tt));
        if (tt - t).abs() < 1e-3 {
            sheet(&all, &g, &out.join(format!("sheet_{t:.2}_{min_width}_{min_score}.png")), &crops);
        }
    }
}
