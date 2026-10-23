//! The phone app's API, in docs/phone-api.md. Every route but pairing needs
//! `Authorization: Bearer <token>`. The media port serves only `/m/<id>`, with a signature
//! from `/api/media-url/<id>` instead of a token, for video players that can't be told to
//! trust this computer's certificate or send headers.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, NaiveDateTime};
use serde_json::{json, Value};

use super::http::{Request, Response};
use super::{Event, Item, Shared, Snapshot};
use crate::clip;

/// Video links work this long (seconds); the phone asks again for a new one after.
const MEDIA_LINK_TTL: i64 = 6 * 3600;
/// Uploads bigger than this are refused (a long 4K video is a few GB).
const MAX_UPLOAD: u64 = 16 << 30;
/// Longest side of /api/photo when the phone doesn't say.
const PHOTO_MAX: u32 = 2048;

/// Search state kept between requests: the text model loads on the first search, and the
/// photos' embeddings are re-read when the app has saved new ones.
#[derive(Default)]
pub struct Search {
    encoder: Option<Result<clip::TextEncoder, String>>,
    embeddings: Option<(SystemTime, clip::Embeddings)>,
}

pub fn handle(shared: &Shared, req: &Request, body: &mut dyn Read, media_only: bool) -> Response {
    if media_only {
        return media(shared, req);
    }
    let parts: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    if req.method == "POST" && parts == ["api", "pair"] {
        return pair(shared, body, req.body_len);
    }
    let token = req.header("Authorization").and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("");
    let Some(phone) = shared.phone_for(token.trim()) else {
        return Response::error(401, "not paired: scan the QR code under Connect phone on the computer");
    };
    let lib = shared.library();
    let item = || parts.get(2).and_then(|id| u64::from_str_radix(id, 16).ok()).and_then(|id| lib.by_id.get(&id)).map(|&i| &lib.items[i]);
    match (req.method.as_str(), parts.get(1).copied().unwrap_or(""), parts.len()) {
        ("GET", "info", 2) => Response::json(200, &info(shared, &lib)),
        ("GET", "photos", 2) => photos(&lib, req),
        ("GET" | "HEAD", "thumb", 3) => match item() {
            Some(it) => thumb(it),
            None => Response::error(404, "no such photo"),
        },
        ("GET" | "HEAD", "photo", 3) => match item() {
            Some(it) => photo(it, req),
            None => Response::error(404, "no such photo"),
        },
        ("GET" | "HEAD", "video", 3) => match item() {
            Some(it) => Response::file(it.path.clone(), content_type(&it.path), req.header("Range")),
            None => Response::error(404, "no such photo"),
        },
        ("GET", "media-url", 3) => match item() {
            Some(it) => media_url(shared, it, req, false),
            None => Response::error(404, "no such photo"),
        },
        // A video the phone can't play, converted on the computer (see docs/phone-api.md).
        ("GET", "compat", 3) => match item() {
            Some(it) if it.is_video => compat(shared, it, req),
            Some(_) => Response::error(400, "not a video"),
            None => Response::error(404, "no such photo"),
        },
        ("GET", "albums", 2) => Response::json(200, &albums(&lib)),
        ("GET", "album", 3) => match parts[2].parse::<usize>().ok().and_then(|i| lib.albums.get(i)) {
            Some(a) => Response::json(200, &json!({ "name": a.name, "ids": hex_ids(&a.ids) })),
            None => Response::error(404, "no such album"),
        },
        ("GET", "people", 2) => Response::json(200, &people(&lib)),
        ("GET", "places", 2) => {
            let list: Vec<_> = lib
                .places
                .iter()
                .map(|p| json!({ "id": p.id.to_string(), "name": p.name, "count": p.ids.len(), "cover": p.ids.first().map(|id| format!("{id:016x}")) }))
                .collect();
            Response::json(200, &json!({ "places": list }))
        }
        ("GET", "place", 3) => match parts[2].parse::<u32>().ok().and_then(|id| lib.places.iter().find(|p| p.id == id)) {
            Some(p) => Response::json(200, &json!({ "name": p.name, "ids": hex_ids(&p.ids) })),
            None => Response::error(404, "no such place"),
        },
        ("GET", "person", 3) => match person(&lib, parts[2]) {
            Some(p) => Response::json(200, &json!({ "name": p.name, "ids": hex_ids(&p.ids) })),
            None => Response::error(404, "no such person"),
        },
        ("GET" | "HEAD", "face", 3) => match person(&lib, parts[2]).and_then(|p| p.face.clone()).filter(|f| f.is_file()) {
            Some(f) => {
                let mut r = Response::file(f, "image/jpeg", None);
                r.headers.push(("Cache-Control".into(), "private, max-age=31536000, immutable".into()));
                r
            }
            None => Response::error(404, "no face picture"),
        },
        ("GET", "search", 2) => search(shared, &lib, req.param("q").unwrap_or("")),
        ("GET", "has", 2) => has(&lib, &phone.name, req),
        ("POST", "upload", 2) => upload(shared, &lib, &phone.name, req, body),
        ("POST", "favorite", 3) => match item() {
            Some(it) => {
                let on = req.param("on") != Some("0");
                shared.set_favorite(it.id, on);
                shared.send(Event::Favorite(it.path.clone(), on));
                Response::json(200, &json!({ "favorite": on }))
            }
            None => Response::error(404, "no such photo"),
        },
        ("POST", "favorites", 2) => match read_json(body, req.body_len) {
            Some(v) => {
                let on = v["on"].as_bool().unwrap_or(true);
                let items = items_of(&lib, &v);
                for it in &items {
                    shared.set_favorite(it.id, on);
                    shared.send(Event::Favorite(it.path.clone(), on));
                }
                Response::json(200, &json!({ "changed": items.len() }))
            }
            None => Response::error(400, "needs {\"ids\": [...], \"on\": true|false}"),
        },
        ("POST", "trash", 2) => match read_json(body, req.body_len) {
            Some(v) => trash(shared, &items_of(&lib, &v)),
            None => Response::error(400, "needs {\"ids\": [...]}"),
        },
        _ => Response::error(404, "no such route"),
    }
}

/// A small JSON body (a list of ids at most).
fn read_json(body: &mut dyn Read, len: u64) -> Option<Value> {
    if len > 4 << 20 {
        return None;
    }
    let mut raw = Vec::new();
    body.take(len).read_to_end(&mut raw).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// The library items named in `{"ids": [...]}`; unknown ids are left out.
fn items_of<'a>(lib: &'a Snapshot, v: &Value) -> Vec<&'a Item> {
    let ids = v["ids"].as_array().map(Vec::as_slice).unwrap_or_default();
    ids.iter()
        .filter_map(|id| u64::from_str_radix(id.as_str()?, 16).ok())
        .filter_map(|id| lib.by_id.get(&id))
        .map(|&i| &lib.items[i])
        .collect()
}

/// Moves photos to the computer's Trash (where they can be put back), like Move to Trash in
/// the app. Ones in an Apple Photos library are left to Photos.
fn trash(shared: &Shared, items: &[&Item]) -> Response {
    let ctx = crate::duplicates::trash_context();
    let (mut moved, mut skipped, mut errors) = (Vec::new(), 0, Vec::new());
    for it in items {
        if crate::duplicates::in_photos_library(&it.path) {
            skipped += 1;
            continue;
        }
        match ctx.delete(&it.path) {
            Ok(()) => moved.push(*it),
            Err(e) => errors.push(format!("{}: {e}", it.path.file_name().unwrap_or_default().to_string_lossy())),
        }
    }
    if !moved.is_empty() {
        super::note_deleted(&moved.iter().map(|it| (it.path.clone(), it.size)).collect::<Vec<_>>());
        shared.remove_photos(&moved.iter().map(|it| it.id).collect::<Vec<_>>());
        shared.send(Event::Trashed(moved.iter().map(|it| (it.path.clone(), it.mtime, it.size)).collect()));
    }
    Response::json(200, &json!({ "trashed": moved.len(), "skipped": skipped, "errors": errors }))
}

fn pair(shared: &Shared, body: &mut dyn Read, len: u64) -> Response {
    if len > 4096 {
        return Response::error(413, "too big");
    }
    let mut raw = Vec::new();
    if body.take(len).read_to_end(&mut raw).is_err() {
        return Response::error(400, "unreadable body");
    }
    let v: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    let (code, name) = (v["code"].as_str().unwrap_or(""), v["name"].as_str().unwrap_or(""));
    match shared.pair(code, name) {
        Some(token) => Response::json(200, &json!({ "token": token, "name": shared.name, "fingerprint": shared.fingerprint })),
        None => Response::error(403, "wrong or expired code: open Connect phone on the computer and scan the new QR code"),
    }
}

fn info(shared: &Shared, lib: &Snapshot) -> Value {
    json!({
        "name": shared.name,
        "version": env!("CARGO_PKG_VERSION"),
        "api": 1,
        "library_version": lib.version,
        "count": lib.items.len(),
        "ready": lib.root.is_some(),
    })
}

fn item_json(it: &Item) -> Value {
    let mut v = json!({
        "id": format!("{:016x}", it.id),
        "name": it.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "taken": local_timestamp(it.taken),
        "aspect": (it.aspect * 1000.0).round() / 1000.0,
        "size": it.size,
        "video": it.is_video,
        "favorite": it.favorite,
    });
    if let Some(d) = it.duration {
        v["duration"] = json!((d * 10.0).round() / 10.0);
    }
    v
}

/// The timeline, newest first, a page at a time (`offset`, `limit`, at most 5000).
fn photos(lib: &Snapshot, req: &Request) -> Response {
    let offset: usize = req.param("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: usize = req.param("limit").and_then(|v| v.parse().ok()).unwrap_or(1000).min(5000);
    let page: Vec<Value> = lib.items.iter().skip(offset).take(limit).map(item_json).collect();
    Response::json(200, &json!({ "library_version": lib.version, "total": lib.items.len(), "offset": offset, "items": page }))
}

/// The 320-pixel thumbnail the app keeps on disk (made now if it hasn't been yet).
fn thumb(it: &Item) -> Response {
    match crate::thumbs::cached_thumb_jpeg(&it.path, it.mtime, it.size) {
        Some(jpeg) => {
            let mut r = Response::bytes(200, "image/jpeg", jpeg);
            r.headers.push(("Cache-Control".into(), "private, max-age=31536000, immutable".into()));
            r.headers.push(("ETag".into(), format!("\"{:016x}\"", crate::thumbs::cache_key(&it.path, it.mtime, it.size))));
            r
        }
        None => Response::error(404, "no thumbnail"),
    }
}

fn hex_ids(ids: &[u64]) -> Vec<String> {
    ids.iter().map(|id| format!("{id:016x}")).collect()
}

fn albums(lib: &Snapshot) -> serde_json::Value {
    let list: Vec<_> = lib
        .albums
        .iter()
        .enumerate()
        .map(|(i, a)| json!({ "id": i.to_string(), "name": a.name, "count": a.ids.len(), "cover": a.ids.first().map(|id| format!("{id:016x}")) }))
        .collect();
    json!({ "albums": list })
}

fn people(lib: &Snapshot) -> serde_json::Value {
    let list: Vec<_> = lib
        .people
        .iter()
        .map(|p| json!({ "id": format!("{:016x}", p.id), "name": p.name, "count": p.ids.len(), "cover": p.ids.first().map(|id| format!("{id:016x}")) }))
        .collect();
    json!({ "people": list })
}

fn person<'a>(lib: &'a Snapshot, id: &str) -> Option<&'a super::Person> {
    let id = u64::from_str_radix(id, 16).ok()?;
    lib.people.iter().find(|p| p.id == id)
}

/// The photo upright as JPEG, at most `max` pixels on its long side (2048 by default), so HEIC
/// and RAW show on any phone; `original=1` sends the file as it is.
fn photo(it: &Item, req: &Request) -> Response {
    if it.is_video || req.param("original") == Some("1") {
        return Response::file(it.path.clone(), content_type(&it.path), req.header("Range"));
    }
    let max = req.param("max").and_then(|m| m.parse().ok()).unwrap_or(PHOTO_MAX).clamp(64, 8192);
    let Some(img) = crate::thumbs::decode(&it.path, max) else { return Response::error(415, "can't read this photo") };
    let mut out = Vec::new();
    if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).encode_image(&img.to_rgb8()).is_err() {
        return Response::error(500, "couldn't encode");
    }
    Response::bytes(200, "image/jpeg", out)
}

/// A link the phone's video player can open without headers: plain HTTP on the media port,
/// signed for this one file, working for a few hours.
fn media_url(shared: &Shared, it: &Item, req: &Request, converted: bool) -> Response {
    let expires = chrono::Utc::now().timestamp() + MEDIA_LINK_TTL;
    let sig = shared.sign(it.id, expires);
    // The address the phone reached us on is one it can reach again.
    let host = req.header("Host").and_then(|h| h.rsplit_once(':').map(|(h, _)| h.to_string()).or(Some(h.to_string()))).unwrap_or_default();
    let variant = if converted { "&v=c" } else { "" };
    let url = format!("http://{host}:{}/m/{:016x}?e={expires}&s={sig}{variant}", shared.media_port, it.id);
    Response::json(200, &json!({ "status": "ready", "url": url, "expires": expires }))
}

/// The video converted to H.264 for phones that can't play it: a link once it's ready, else how
/// far converting has got (it starts on the first ask).
fn compat(shared: &Shared, it: &Item, req: &Request) -> Response {
    if super::converted_path(it).is_some_and(|p| p.is_file()) {
        return media_url(shared, it, req, true);
    }
    match shared.conversion(it) {
        super::Conversion::Running(p) => Response::json(200, &json!({ "status": "converting", "progress": p })),
        super::Conversion::Failed(e) => Response::json(200, &json!({ "status": "failed", "error": e })),
    }
}

/// The media port: only signed links to one file each.
fn media(shared: &Shared, req: &Request) -> Response {
    let parts: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    let (Some(id), Some(e), Some(s)) = (
        (parts.len() == 2 && parts[0] == "m").then(|| u64::from_str_radix(parts[1], 16).ok()).flatten(),
        req.param("e").and_then(|e| e.parse::<i64>().ok()),
        req.param("s"),
    ) else {
        return Response::error(404, "no such link");
    };
    if e < chrono::Utc::now().timestamp() || !super::constant_time_eq(shared.sign(id, e).as_bytes(), s.as_bytes()) {
        return Response::error(403, "link expired or not valid");
    }
    let lib = shared.library();
    match lib.by_id.get(&id).map(|&i| &lib.items[i]) {
        Some(it) if req.param("v") == Some("c") => match super::converted_path(it).filter(|p| p.is_file()) {
            Some(p) => Response::file(p, "video/mp4", req.header("Range")),
            None => Response::error(404, "not converted (any more)"),
        },
        Some(it) => Response::file(it.path.clone(), content_type(&it.path), req.header("Range")),
        None => Response::error(404, "no such photo"),
    }
}

/// Same idea as the app's search: words that match the file name or the date ("2024",
/// "october", "sunday", "2024-10-06"); otherwise by what's in the photos, with CLIP.
fn search(shared: &Shared, lib: &Snapshot, q: &str) -> Response {
    let terms: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
    if terms.is_empty() {
        return Response::json(200, &json!({ "kind": "none", "ids": [] }));
    }
    let named: Vec<Value> = lib.items.iter().filter(|it| words_match(it, &terms)).map(|it| json!(format!("{:016x}", it.id))).collect();
    if !named.is_empty() {
        return Response::json(200, &json!({ "kind": "name", "ids": named }));
    }
    let mut s = shared.search.lock().unwrap();
    let clip_file = crate::store::data_dir().map(|d| d.join("clip.bin"));
    let modified = clip_file.as_ref().and_then(|f| std::fs::metadata(f).ok()?.modified().ok());
    if let Some(m) = modified {
        if s.embeddings.as_ref().map(|(t, _)| *t) != Some(m) {
            s.embeddings = Some((m, clip::Embeddings::load()));
        }
    }
    if s.encoder.is_none() {
        s.encoder = Some(match clip::text_files() {
            Some((model, tok)) => clip::TextEncoder::load(&model, &tok),
            None => Err("the search model isn't installed".into()),
        });
    }
    let query = match s.encoder.as_ref().unwrap() {
        Ok(enc) => enc.query(&terms.join(" ")),
        Err(e) => Err(e.clone()),
    };
    let (Ok(query), Some((_, emb))) = (query, s.embeddings.as_ref()) else {
        return Response::json(200, &json!({ "kind": "none", "ids": [] }));
    };
    let candidates = lib.items.iter().enumerate().filter_map(|(i, it)| emb.get(&it.path, it.mtime, it.size).map(|e| (i, e)));
    let ids: Vec<Value> = clip::rank(&query, candidates).into_iter().map(|i| json!(format!("{:016x}", lib.items[i].id))).collect();
    Response::json(200, &json!({ "kind": "content", "ids": ids }))
}

fn words_match(it: &Item, terms: &[String]) -> bool {
    let name = it.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let d = it.taken;
    let dates = format!(
        "{} {} {} {} {}",
        d.format("%Y %B %b %A %a").to_string().to_lowercase(),
        d.format("%Y-%m-%d"),
        d.format("%Y-%m"),
        d.year(),
        d.format("%d.%m.%Y")
    );
    terms.iter().all(|t| name.contains(t.as_str()) || dates.split(' ').any(|w| w == t))
}

/// Where uploads from this phone go: `Imported/<phone name>` in the library folder.
fn upload_dir(lib: &Snapshot, phone: &str) -> Option<PathBuf> {
    let name: String = phone.chars().map(|c| if c == '/' || c == '\\' || c == ':' { '-' } else { c }).collect();
    let name = name.trim().trim_start_matches('.');
    Some(lib.root.as_ref()?.join("Imported").join(if name.is_empty() { "Phone" } else { name }))
}

/// Whether a phone photo is already here, so the phone can skip uploading it.
fn has(lib: &Snapshot, phone: &str, req: &Request) -> Response {
    let (Some(dir), Some(name), Some(size), Some(taken)) = (
        upload_dir(lib, phone),
        req.param("name"),
        req.param("size").and_then(|s| s.parse::<u64>().ok()),
        req.param("taken").and_then(|t| t.parse::<i64>().ok()).and_then(from_local_timestamp),
    ) else {
        return Response::error(400, "needs name, size and taken");
    };
    let have = crate::transfer::already_imported(&dir, name, taken, size) || in_library(lib, name, size) || super::was_deleted(name, size);
    Response::json(200, &json!({ "have": have }))
}

/// A file of this name and size anywhere in the library: one the phone saved from here, which
/// a backup shouldn't send back as a copy.
fn in_library(lib: &Snapshot, name: &str, size: u64) -> bool {
    lib.items.iter().any(|it| it.size == size && it.path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name)))
}

/// A photo or video from the phone, as the raw file in the body (with Content-Length). It's
/// filed in year/month folders by its EXIF date, else by `taken` (seconds since 1970, local
/// time), like any import; one that's already there is skipped.
fn upload(shared: &Shared, lib: &Snapshot, phone: &str, req: &Request, body: &mut dyn Read) -> Response {
    let Some(dir) = upload_dir(lib, phone) else { return Response::error(503, "no library folder is open on the computer") };
    let Some(name) = req.param("name").filter(|n| !n.is_empty()) else { return Response::error(400, "needs name") };
    if !crate::transfer::is_media(Path::new(name)) {
        return Response::error(415, "not a photo or video");
    }
    if req.body_len == 0 {
        return Response::error(411, "needs Content-Length");
    }
    if req.body_len > MAX_UPLOAD {
        return Response::error(413, "too big");
    }
    let tmp_dir = dir.join(".banana-upload");
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = tmp_dir.join(format!("{}.{ext}", super::random_hex(8)));
    let written = (|| {
        std::fs::create_dir_all(&tmp_dir)?;
        let mut f = std::fs::File::create(&tmp)?;
        let n = std::io::copy(&mut body.take(req.body_len), &mut f)?;
        f.sync_all()?;
        Ok::<u64, std::io::Error>(n)
    })();
    match written {
        Ok(n) if n == req.body_len => {}
        _ => {
            let _ = std::fs::remove_file(&tmp);
            return Response::error(400, "upload cut short");
        }
    }
    if in_library(lib, name, req.body_len) || super::was_deleted(name, req.body_len) {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir(&tmp_dir);
        return Response::json(200, &json!({ "status": "skipped" }));
    }
    let read = crate::library::read_photo(&tmp);
    let taken = read
        .as_ref()
        .filter(|p| p.from_exif)
        .map(|p| p.taken)
        .or_else(|| req.param("taken").and_then(|t| t.parse().ok()).and_then(from_local_timestamp))
        .unwrap_or_else(crate::transfer::now);
    let result = crate::transfer::save_file(&dir, name, taken, &tmp);
    let _ = std::fs::remove_dir(&tmp_dir);
    match result {
        Ok(Some(p)) => {
            if let Some(photo) = crate::library::read_photo(&p) {
                shared.add_photo(&photo);
            }
            shared.send(Event::Uploaded);
            Response::json(200, &json!({ "status": "imported", "name": p.file_name().map(|n| n.to_string_lossy().to_string()) }))
        }
        Ok(None) => Response::json(200, &json!({ "status": "skipped" })),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Response::error(500, &e.to_string())
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("heic" | "heif" | "hif") => "image/heic",
        Some("avif") => "image/avif",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("3gp") => "video/3gpp",
        _ => "application/octet-stream",
    }
}

/// Taken times are wall-clock times without a zone (as cameras write them); on the wire they're
/// seconds since 1970 as if that wall-clock time were UTC, so the phone shows the same date.
fn local_timestamp(t: NaiveDateTime) -> i64 {
    t.and_utc().timestamp()
}

fn from_local_timestamp(secs: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(secs, 0).map(|d| d.naive_utc())
}
