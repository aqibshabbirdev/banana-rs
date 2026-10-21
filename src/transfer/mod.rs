//! Import & export: copy photos out of the library (with filters), and bring them in from a
//! folder or drive, a Google Takeout download, Google Photos (sign in, pick), or Mac Photos.
//! Everything runs on a background thread and reports through `Msg`.

pub mod google;
#[cfg(target_os = "macos")]
pub mod macphotos;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime};
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaKind {
    #[default]
    All,
    Photos,
    Videos,
}

/// The filters shared by every import source and by export. Library-only filters (documents,
/// people, favourites, albums) are applied by the app, which knows them.
#[derive(Clone, Default)]
pub struct Filter {
    /// "2024", "2024-05" or "2024-05-17"; empty for no limit.
    pub from: String,
    pub to: String,
    /// Part of the file name (for export, also a person's name).
    pub name: String,
    pub kind: MediaKind,
}

fn parse_date(s: &str, end: bool) -> Option<NaiveDate> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(['-', '/', '.']).collect();
    let y: i32 = parts.first()?.parse().ok()?;
    match parts.len() {
        1 => if end { NaiveDate::from_ymd_opt(y, 12, 31) } else { NaiveDate::from_ymd_opt(y, 1, 1) },
        2 => {
            let m: u32 = parts[1].parse().ok()?;
            let first = NaiveDate::from_ymd_opt(y, m, 1)?;
            if end { first.checked_add_months(chrono::Months::new(1))?.pred_opt() } else { Some(first) }
        }
        _ => NaiveDate::from_ymd_opt(y, parts[1].parse().ok()?, parts[2].parse().ok()?),
    }
}

impl Filter {
    /// A problem with the typed dates, to show under the fields.
    pub fn date_error(&self) -> Option<&'static str> {
        let bad = |s: &str, end| !s.trim().is_empty() && parse_date(s, end).is_none();
        if bad(&self.from, false) || bad(&self.to, true) {
            return Some("Dates look like 2024, 2024-05 or 2024-05-17.");
        }
        match (parse_date(&self.from, false), parse_date(&self.to, true)) {
            (Some(a), Some(b)) if a > b => Some("\"From\" is after \"To\"."),
            _ => None,
        }
    }

    pub fn has_dates(&self) -> bool {
        !self.from.trim().is_empty() || !self.to.trim().is_empty()
    }

    pub fn date_ok(&self, t: NaiveDateTime) -> bool {
        let d = t.date();
        parse_date(&self.from, false).is_none_or(|from| d >= from) && parse_date(&self.to, true).is_none_or(|to| d <= to)
    }

    pub fn name_ok(&self, name: &str) -> bool {
        let want = self.name.trim().to_lowercase();
        want.is_empty() || name.to_lowercase().contains(&want)
    }

    pub fn kind_ok(&self, is_video: bool) -> bool {
        match self.kind {
            MediaKind::All => true,
            MediaKind::Photos => !is_video,
            MediaKind::Videos => is_video,
        }
    }

    /// Everything a source can check without opening the file.
    pub fn matches(&self, name: &str, taken: NaiveDateTime, is_video: bool) -> bool {
        self.date_ok(taken) && self.name_ok(name) && self.kind_ok(is_video)
    }
}

pub enum Msg {
    /// What's happening, e.g. "Waiting for you to pick photos in the browser…".
    Status(String),
    Progress { done: usize, total: usize },
    /// A file that couldn't be copied, and why.
    Failed(String),
    /// A file an import just added to the library.
    Imported(PathBuf),
    /// Finished; the summary to show.
    Done(String),
}

/// Lets the UI stop a running job between files.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Reporter {
    pub fn new(tx: Sender<Msg>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
    pub fn status(&self, s: impl Into<String>) {
        self.send(Msg::Status(s.into()));
    }
}

pub fn is_media(path: &Path) -> bool {
    crate::library::is_image(path) || crate::video::is_video(path)
}

/// `dir/name`, or `dir/name (2).ext`… if taken.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap()
}

/// A name from a zip or a server, cut down to one plain file name, so "../x", "a\\..\\x" or
/// "/etc/x" can't write outside the destination.
fn plain_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let last: String = last.chars().filter(|c| !c.is_control() && *c != ':').collect();
    if last.is_empty() || last == "." || last == ".." { "unnamed".to_string() } else { last }
}

/// Where a file goes: in a year/month subfolder (or flat), skipping one that's already there
/// with the same size. `None` means "already have it".
fn target(dest: &Path, name: &str, taken: NaiveDateTime, size: u64, by_month: bool) -> std::io::Result<Option<PathBuf>> {
    let name = &plain_name(name);
    let dir = if by_month { dest.join(format!("{}", taken.year())).join(taken.format("%m %B").to_string()) } else { dest.to_path_buf() };
    std::fs::create_dir_all(&dir)?;
    let same = dir.join(name);
    if std::fs::metadata(&same).is_ok_and(|m| m.len() == size) {
        return Ok(None);
    }
    Ok(Some(free_name(&dir, name)))
}

/// Sets the file's modified time, which the library uses when a photo has no EXIF date.
fn set_mtime(path: &Path, t: NaiveDateTime) {
    if let Some(local) = t.and_local_timezone(Local).earliest() {
        let secs = local.timestamp().max(0) as u64;
        if let Ok(f) = File::options().write(true).open(path) {
            let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(secs));
        }
    }
}

fn summary(verb: &str, copied: usize, skipped: usize, failed: usize, cancelled: bool) -> String {
    let mut s = format!("{verb} {copied} item{}", if copied == 1 { "" } else { "s" });
    if skipped > 0 {
        s += &format!(", skipped {skipped} already there");
    }
    if failed > 0 {
        s += &format!(", {failed} failed");
    }
    if cancelled {
        s += " (stopped)";
    }
    s + "."
}

// ---------------------------------------------------------------- export

pub struct ExportItem {
    pub src: PathBuf,
    pub taken: NaiveDateTime,
}

pub fn spawn_export(items: Vec<ExportItem>, dest: PathBuf, by_month: bool, cancel: Cancel, r: Reporter) {
    std::thread::spawn(move || {
        let total = items.len();
        let (mut copied, mut skipped, mut failed) = (0, 0, 0);
        for (i, item) in items.iter().enumerate() {
            if cancel.cancelled() {
                break;
            }
            let name = item.src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let result = (|| {
                let meta = std::fs::metadata(&item.src)?;
                let Some(out) = target(&dest, &name, item.taken, meta.len(), by_month)? else { return Ok(false) };
                std::fs::copy(&item.src, &out)?;
                if let Ok(m) = meta.modified() {
                    let _ = File::options().write(true).open(&out).and_then(|f| f.set_modified(m));
                }
                Ok::<bool, std::io::Error>(true)
            })();
            match result {
                Ok(true) => copied += 1,
                Ok(false) => skipped += 1,
                Err(e) => {
                    failed += 1;
                    r.send(Msg::Failed(format!("{name}: {e}")));
                }
            }
            r.send(Msg::Progress { done: i + 1, total });
        }
        r.send(Msg::Done(summary("Exported", copied, skipped, failed, cancel.cancelled())));
    });
}

// ---------------------------------------------------------------- folder or drive

/// Copies the photos and videos in `srcs` (a folder or drive, or a phone's DCIM folders).
pub fn spawn_folder_import(srcs: Vec<PathBuf>, dest: PathBuf, filter: Filter, cancel: Cancel, r: Reporter) {
    std::thread::spawn(move || {
        let mut files: Vec<PathBuf> = Vec::new();
        for src in &srcs {
            r.status(format!("Looking through {}…", src.display()));
            files.extend(media_files(src));
        }
        let picked: Vec<(PathBuf, NaiveDateTime, u64)> = files
            .into_iter()
            .filter_map(|p| {
                let photo = crate::library::read_photo(&p)?;
                let name = p.file_name()?.to_string_lossy().to_string();
                filter.matches(&name, photo.taken, photo.is_video).then_some((p, photo.taken, photo.size))
            })
            .collect();
        let total = picked.len();
        let (mut copied, mut skipped, mut failed) = (0, 0, 0);
        for (i, (p, taken, size)) in picked.iter().enumerate() {
            if cancel.cancelled() {
                break;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let result = (|| {
                let Some(out) = target(&dest, &name, *taken, *size, true)? else { return Ok(None) };
                std::fs::copy(p, &out)?;
                set_mtime(&out, *taken);
                Ok::<Option<PathBuf>, std::io::Error>(Some(out))
            })();
            match result {
                Ok(Some(p)) => {
                    copied += 1;
                    r.send(Msg::Imported(p));
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    failed += 1;
                    r.send(Msg::Failed(format!("{name}: {e}")));
                }
            }
            r.send(Msg::Progress { done: i + 1, total });
        }
        r.send(Msg::Done(summary("Imported", copied, skipped, failed, cancel.cancelled())));
    });
}

/// Photos and videos in a folder and its sub-folders, skipping hidden ones (".thumbnails").
pub fn media_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_media(e.path()))
        .map(|e| e.into_path())
        .collect()
}

// ---------------------------------------------------------------- Google Takeout

/// Takeout's JSON sidecar fields we use.
#[derive(serde::Deserialize)]
struct Sidecar {
    title: Option<String>,
    #[serde(rename = "photoTakenTime")]
    photo_taken_time: Option<Timestamp>,
}

#[derive(serde::Deserialize)]
struct Timestamp {
    timestamp: String,
}

/// When each photo in a Takeout was taken, from the JSON sidecars.
#[derive(Default)]
struct Dates {
    /// (folder inside the archive, original file name) → time.
    by_title: HashMap<(String, String), NaiveDateTime>,
    /// folder → (lower-case name without extension, time), for the fuzzier lookups.
    by_dir: HashMap<String, Vec<(String, NaiveDateTime)>>,
}

fn stem_lower(name: &str) -> String {
    Path::new(name).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

impl Dates {
    fn insert(&mut self, dir: String, title: String, t: NaiveDateTime) {
        self.by_dir.entry(dir.clone()).or_default().push((stem_lower(&title), t));
        self.by_title.insert((dir, title), t);
    }
}

fn sidecar_date(bytes: &[u8]) -> Option<(String, NaiveDateTime)> {
    let s: Sidecar = serde_json::from_slice(bytes).ok()?;
    let secs: i64 = s.photo_taken_time?.timestamp.parse().ok()?;
    let local = DateTime::from_timestamp(secs, 0)?.with_timezone(&Local).naive_local();
    Some((s.title?, local))
}

fn split(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((d, n)) => (d.to_string(), n.to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// The sidecar date for a media file: by its own name; for "IMG_1-edited.jpg" and
/// "IMG_1(1).jpg" the original's; for the video half of a Live/Motion photo ("IMG_1.MP4") the
/// photo's ("IMG_1.HEIC"); and for names Takeout cut short, the sidecar whose full title starts
/// with them.
fn takeout_date(dates: &Dates, dir: &str, name: &str) -> Option<NaiveDateTime> {
    let key = |n: &str| (dir.to_string(), n.to_string());
    if let Some(t) = dates.by_title.get(&key(name)) {
        return Some(*t);
    }
    let p = Path::new(name);
    let (stem, ext) = (p.file_stem()?.to_string_lossy().to_string(), p.extension().map(|e| e.to_string_lossy().to_string()));
    let ext = ext.map(|e| format!(".{e}")).unwrap_or_default();
    let mut base = stem.clone();
    for suffix in ["-edited", "-bearbeitet", "-modifié", "-editado"] {
        if let Some(b) = base.strip_suffix(suffix) {
            base = b.to_string();
        }
    }
    if let Some(i) = base.rfind('(').filter(|_| base.ends_with(')')) {
        base.truncate(i);
    }
    if let Some(t) = dates.by_title.get(&key(&format!("{base}{ext}"))) {
        return Some(*t);
    }
    let in_dir = dates.by_dir.get(dir)?;
    let base = base.to_lowercase();
    if let Some((_, t)) = in_dir.iter().find(|(s, _)| *s == base) {
        return Some(*t);
    }
    if base.chars().count() >= 30 {
        if let Some((_, t)) = in_dir.iter().find(|(s, _)| s.starts_with(&base)) {
            return Some(*t);
        }
    }
    None
}

/// Google Takeout download: one or more .zip files or an already-extracted folder. Dates come
/// from the JSON next to each photo (Takeout strips some EXIF and resets file times), and the
/// copies that Takeout puts in every album folder are imported once.
pub fn spawn_takeout_import(sources: Vec<PathBuf>, dest: PathBuf, filter: Filter, cancel: Cancel, r: Reporter) {
    std::thread::spawn(move || {
        let mut seen: HashSet<(String, u64)> = HashSet::new();
        let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);

        // Pass 1: every sidecar's date, and the media files to consider.
        r.status("Reading Takeout's photo dates…");
        let mut dates = Dates::default();
        // (source index, path inside source, size, the file's own modified time)
        let mut media: Vec<(usize, String, u64, Option<NaiveDateTime>)> = Vec::new();
        for (si, src) in sources.iter().enumerate() {
            if src.is_dir() {
                for e in walkdir::WalkDir::new(src).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()) {
                    let rel = e.path().strip_prefix(src).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                    if rel.ends_with(".json") {
                        if let Some((title, t)) = std::fs::read(e.path()).ok().and_then(|b| sidecar_date(&b)) {
                            dates.insert(split(&rel).0, title, t);
                        }
                    } else if is_media(e.path()) && e.metadata().is_ok_and(|m| m.len() > 0) {
                        let meta = e.metadata().ok();
                        let mtime = meta.as_ref().and_then(|m| m.modified().ok()).map(|t| DateTime::<Local>::from(t).naive_local());
                        media.push((si, rel, meta.map(|m| m.len()).unwrap_or(0), mtime));
                    }
                }
            } else {
                let Ok(mut zip) = File::open(src).map_err(|e| e.to_string()).and_then(|f| zip::ZipArchive::new(f).map_err(|e| e.to_string())) else {
                    failed += 1;
                    r.send(Msg::Failed(format!("{}: not a readable zip file", src.display())));
                    continue;
                };
                for i in 0..zip.len() {
                    let Ok(mut entry) = zip.by_index(i) else { continue };
                    if entry.is_dir() {
                        continue;
                    }
                    let name = entry.name().to_string();
                    if name.ends_with(".json") && entry.size() < 1 << 20 {
                        let mut buf = Vec::new();
                        if entry.read_to_end(&mut buf).is_ok() {
                            if let Some((title, t)) = sidecar_date(&buf) {
                                dates.insert(split(&name).0, title, t);
                            }
                        }
                    } else if is_media(Path::new(&name)) && entry.size() > 0 {
                        let mtime = entry.last_modified().and_then(|d| {
                            NaiveDate::from_ymd_opt(d.year() as i32, d.month() as u32, d.day() as u32)?
                                .and_hms_opt(d.hour() as u32, d.minute() as u32, d.second() as u32)
                        });
                        media.push((si, name, entry.size(), mtime));
                    }
                }
            }
        }

        // Pass 2: filter and copy.
        let mut plan: Vec<(usize, String, u64, NaiveDateTime)> = Vec::new();
        let mut undated_left_out = 0;
        for (si, rel, size, mtime) in media {
            let (dir, name) = split(&rel);
            let taken = match takeout_date(&dates, &dir, &name) {
                Some(t) => t,
                // No sidecar: with a date filter we can't tell, so leave it out (and say so);
                // otherwise import it and let the library read its EXIF date.
                None if filter.has_dates() => {
                    undated_left_out += 1;
                    continue;
                }
                None => mtime.unwrap_or_else(now),
            };
            if filter.matches(&name, taken, crate::video::is_video(Path::new(&name))) && seen.insert((name.clone(), size)) {
                plan.push((si, rel, size, taken));
            }
        }
        let total = plan.len();
        r.status(format!("Importing {total} items…"));
        let mut zips: HashMap<usize, zip::ZipArchive<File>> = HashMap::new();
        for (n, (si, rel, size, taken)) in plan.into_iter().enumerate() {
            if cancel.cancelled() {
                break;
            }
            let (_, name) = split(&rel);
            let result = (|| {
                let Some(out) = target(&dest, &name, taken, size, true)? else { return Ok(None) };
                let src = &sources[si];
                if src.is_dir() {
                    std::fs::copy(src.join(&rel), &out)?;
                } else {
                    if let std::collections::hash_map::Entry::Vacant(v) = zips.entry(si) {
                        v.insert(zip::ZipArchive::new(File::open(src)?).map_err(std::io::Error::other)?);
                    }
                    let zip = zips.get_mut(&si).unwrap();
                    let mut entry = zip.by_name(&rel).map_err(std::io::Error::other)?;
                    let tmp = out.with_extension("banana-part");
                    let mut f = File::create(&tmp)?;
                    std::io::copy(&mut entry, &mut f)?;
                    f.flush()?;
                    drop(f);
                    std::fs::rename(&tmp, &out)?;
                }
                set_mtime(&out, taken);
                Ok::<Option<PathBuf>, std::io::Error>(Some(out))
            })();
            match result {
                Ok(Some(p)) => {
                    copied += 1;
                    r.send(Msg::Imported(p));
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    failed += 1;
                    r.send(Msg::Failed(format!("{name}: {e}")));
                }
            }
            r.send(Msg::Progress { done: n + 1, total });
        }
        let mut done = summary("Imported", copied, skipped, failed, cancel.cancelled());
        if undated_left_out > 0 {
            done += &format!(" {undated_left_out} file{} without a Takeout date left out by the date filter.", if undated_left_out == 1 { "" } else { "s" });
        }
        r.send(Msg::Done(done));
    });
}

/// "now" for sources that don't say when a photo was taken.
pub fn now() -> NaiveDateTime {
    DateTime::<Local>::from(SystemTime::now()).naive_local()
}

/// Moves an already-written temp file into the library (year/month folders), dated `taken`.
/// Returns where it went, or `None` when the same file was already there.
pub fn save_file(dest: &Path, name: &str, taken: NaiveDateTime, file: &Path) -> std::io::Result<Option<PathBuf>> {
    let size = std::fs::metadata(file)?.len();
    let Some(out) = target(dest, name, taken, size, true)? else {
        let _ = std::fs::remove_file(file);
        return Ok(None);
    };
    if std::fs::rename(file, &out).is_err() {
        std::fs::copy(file, &out)?;
        let _ = std::fs::remove_file(file);
    }
    set_mtime(&out, taken);
    Ok(Some(out))
}

pub fn summary_for(verb: &str, copied: usize, skipped: usize, failed: usize, cancelled: bool) -> String {
    summary(verb, copied, skipped, failed, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_filter_accepts_years_months_and_days() {
        let f = Filter { from: "2024-05".into(), to: "2024".into(), ..Default::default() };
        let t = |s: &str| NaiveDateTime::parse_from_str(&format!("{s} 12:00"), "%Y-%m-%d %H:%M").unwrap();
        assert!(!f.date_ok(t("2024-04-30")));
        assert!(f.date_ok(t("2024-05-01")));
        assert!(f.date_ok(t("2024-12-31")));
        assert!(!f.date_ok(t("2025-01-01")));
        assert!(Filter { from: "2024-13".into(), ..Default::default() }.date_error().is_some());
        assert!(Filter { from: "2025".into(), to: "2024".into(), ..Default::default() }.date_error().is_some());
    }

    #[test]
    fn imported_names_stay_inside_the_destination() {
        assert_eq!(plain_name("../../evil.jpg"), "evil.jpg");
        assert_eq!(plain_name("a/..\\..\\evil.jpg"), "evil.jpg");
        assert_eq!(plain_name("/etc/passwd"), "passwd");
        assert_eq!(plain_name(".."), "unnamed");
        assert_eq!(plain_name("dir/"), "unnamed");
        assert_eq!(plain_name("IMG_0001.HEIC"), "IMG_0001.HEIC");
    }

    fn run(start: impl FnOnce(Cancel, Reporter)) -> (String, Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        start(Cancel::default(), Reporter::new(tx, egui::Context::default()));
        let mut errors = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).expect("job finished") {
                Msg::Done(s) => return (s, errors),
                Msg::Failed(e) => errors.push(e),
                _ => {}
            }
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("banana-transfer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A Takeout-shaped zip: sidecars give the dates, an "-edited" copy shares its original's,
    /// and the album folder repeats a photo that must be imported once.
    #[test]
    fn takeout_zip_import_restores_dates_dedupes_and_filters() {
        let dir = temp("takeout");
        let zip_path = dir.join("takeout-001.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            let mut put = |name: &str, bytes: &[u8]| {
                z.start_file(name, opts).unwrap();
                z.write_all(bytes).unwrap();
            };
            let json = |title: &str, ts: i64| format!(r#"{{"title":"{title}","photoTakenTime":{{"timestamp":"{ts}"}}}}"#);
            // 2019-07-01 and 2023-03-15 (noon UTC, so the local date is the same in any timezone)
            put("Takeout/Google Photos/Photos from 2019/IMG_1.jpg", b"one");
            put("Takeout/Google Photos/Photos from 2019/IMG_1.jpg.supplemental-metadata.json", json("IMG_1.jpg", 1561982400).as_bytes());
            put("Takeout/Google Photos/Photos from 2019/IMG_1-edited.jpg", b"one, edited");
            put("Takeout/Google Photos/Photos from 2023/IMG_2.jpg", b"two");
            put("Takeout/Google Photos/Photos from 2023/IMG_2.jpg.json", json("IMG_2.jpg", 1678881600).as_bytes());
            put("Takeout/Google Photos/Trip/IMG_1.jpg", b"one");
            put("Takeout/Google Photos/Trip/IMG_1.jpg.json", json("IMG_1.jpg", 1561982400).as_bytes());
            put("Takeout/Google Photos/Trip/metadata.json", br#"{"title":"Trip"}"#);
            z.finish().unwrap();
        }
        let dest = dir.join("library");
        let (summary, errors) = run(|c, r| spawn_takeout_import(vec![zip_path.clone()], dest.clone(), Filter::default(), c, r));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(summary, "Imported 3 items.", "IMG_1 once, IMG_1-edited, IMG_2");
        let july = dest.join("2019").join("07 July");
        assert_eq!(std::fs::read(july.join("IMG_1.jpg")).unwrap(), b"one");
        assert!(july.join("IMG_1-edited.jpg").exists());
        assert!(dest.join("2023").join("03 March").join("IMG_2.jpg").exists());
        let mtime: DateTime<Local> = std::fs::metadata(july.join("IMG_1.jpg")).unwrap().modified().unwrap().into();
        assert_eq!(mtime.date_naive(), NaiveDate::from_ymd_opt(2019, 7, 1).unwrap());

        // Running again skips what's there; a date filter limits what's considered.
        let (summary, _) = run(|c, r| spawn_takeout_import(vec![zip_path.clone()], dest.clone(), Filter::default(), c, r));
        assert_eq!(summary, "Imported 0 items, skipped 3 already there.");
        let only_2023 = Filter { from: "2023".into(), ..Default::default() };
        let (summary, _) = run(|c, r| spawn_takeout_import(vec![zip_path.clone()], dir.join("other"), only_2023, c, r));
        assert_eq!(summary, "Imported 1 item.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_copies_into_month_folders_and_skips_existing() {
        let dir = temp("export");
        let src = dir.join("a.jpg");
        std::fs::write(&src, b"photo").unwrap();
        let taken = NaiveDate::from_ymd_opt(2022, 12, 24).unwrap().and_hms_opt(9, 0, 0).unwrap();
        let out = dir.join("out");
        let item = || vec![ExportItem { src: src.clone(), taken }];
        assert_eq!(run(|c, r| spawn_export(item(), out.clone(), true, c, r)).0, "Exported 1 item.");
        assert_eq!(std::fs::read(out.join("2022").join("12 December").join("a.jpg")).unwrap(), b"photo");
        assert_eq!(run(|c, r| spawn_export(item(), out.clone(), true, c, r)).0, "Exported 0 items, skipped 1 already there.");
        assert_eq!(run(|c, r| spawn_export(item(), out.clone(), false, c, r)).0, "Exported 1 item.");
        assert!(out.join("a.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn takeout_dates_follow_edited_and_numbered_copies() {
        let mut d = Dates::default();
        let t = NaiveDate::from_ymd_opt(2019, 7, 1).unwrap().and_hms_opt(10, 0, 0).unwrap();
        let dir = "Takeout/Google Photos/Trip";
        d.insert(dir.into(), "IMG_1.jpg".into(), t);
        d.insert(dir.into(), "IMG_7.HEIC".into(), t);
        d.insert(dir.into(), "Screenshot_2024-05-01-10-11-12-345_com.example.someverylongapp.jpg".into(), t);
        assert_eq!(takeout_date(&d, dir, "IMG_1-edited.jpg"), Some(t));
        assert_eq!(takeout_date(&d, dir, "IMG_1(1).jpg"), Some(t));
        assert_eq!(takeout_date(&d, dir, "IMG_7.MP4"), Some(t), "Live Photo video takes the photo's date");
        assert_eq!(takeout_date(&d, dir, "Screenshot_2024-05-01-10-11-12-345_com.example.j.jpg"), None);
        assert_eq!(takeout_date(&d, dir, "Screenshot_2024-05-01-10-11-12-345_com.exampl.jpg"), Some(t), "name cut short by Takeout");
        assert_eq!(takeout_date(&d, "Takeout/Google Photos/Other", "IMG_1.jpg"), None);
    }
}
