//! Syncing two computers' libraries over the home network: this computer (the one that paired)
//! talks to the other's Connect server (remote/api.rs, `/api/sync/…`) with its token, over TLS
//! pinned to the certificate seen when pairing.
//!
//! Photos and videos are matched by their path inside the library folder, so both keep the same
//! folders. Each pair goes both ways, or one way (one computer keeps a copy of the other).
//! Deleting is never synced; a photo deleted on one side isn't sent back to it either (see
//! `remote::was_deleted`). Favourites, archive, albums and people's names are merged with what
//! both had at the last sync, so taking a favourite off on one side takes it off on the other.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Favourites, archive, albums and names, with photos as paths inside the library folder.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct Meta {
    #[serde(default)]
    pub favorites: BTreeSet<String>,
    #[serde(default)]
    pub archived: BTreeSet<String>,
    #[serde(default)]
    pub albums: BTreeMap<String, BTreeSet<String>>,
    /// People's names, per face: "<path>#<face index>" → name.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

/// A path inside `root` as "a/b/c.jpg" (the same on every system), if it is inside.
pub fn rel(root: &Path, path: &Path) -> Option<String> {
    let parts: Vec<String> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The file a "a/b/c.jpg" from the other computer means here. None for anything that could
/// point outside `root` ("..", absolute paths, drive letters, backslashes).
pub fn abs(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':', '\0']) {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

impl Meta {
    /// What `store` has for photos in `root`.
    pub fn from_store(root: &Path, store: &Store) -> Self {
        let set = |paths: &mut dyn Iterator<Item = &PathBuf>| paths.filter_map(|p| rel(root, p)).collect::<BTreeSet<_>>();
        let albums = store
            .albums
            .iter()
            .filter(|a| a.photos.is_empty() || a.photos.iter().any(|p| p.starts_with(root)))
            .map(|a| (a.name.clone(), set(&mut a.photos.iter())))
            .collect();
        let names = store
            .face_names
            .iter()
            .filter_map(|(key, name)| {
                let (path, face) = key.rsplit_once('#')?;
                Some((format!("{}#{face}", rel(root, Path::new(path))?), name.clone()))
            })
            .collect();
        Meta { favorites: set(&mut store.favorites.iter()), archived: set(&mut store.archived.iter()), albums, names }
    }

    /// Makes `store` say this for photos in `root` (leaving other folders' entries alone).
    pub fn apply_to(&self, root: &Path, store: &mut Store) {
        let here = |p: &Path| p.starts_with(root);
        let paths = |set: &BTreeSet<String>| set.iter().filter_map(|r| abs(root, r)).collect::<Vec<_>>();
        store.favorites.retain(|p| !here(p));
        store.favorites.extend(paths(&self.favorites));
        store.archived.retain(|p| !here(p));
        store.archived.extend(paths(&self.archived));
        // Albums of this folder that aren't wanted any more go; the rest get their photos here.
        store.albums.retain(|a| self.albums.contains_key(&a.name) || !(a.photos.is_empty() || a.photos.iter().all(|p| here(p))));
        for (name, photos) in &self.albums {
            match store.albums.iter_mut().find(|a| &a.name == name) {
                Some(a) => {
                    a.photos.retain(|p| !here(p));
                    a.photos.extend(paths(photos));
                }
                None => store.albums.push(crate::store::Album { name: name.clone(), photos: paths(photos) }),
            }
        }
        store.face_names.retain(|key, _| !key.rsplit_once('#').is_some_and(|(p, _)| here(Path::new(p))));
        for (key, name) in &self.names {
            let Some((r, face)) = key.rsplit_once('#') else { continue };
            if let Some(p) = abs(root, r) {
                store.face_names.insert(format!("{}#{face}", p.display()), name.clone());
            }
        }
        store.save();
    }

    /// Both sides' changes since `base` (the last sync): kept where both have it or one side
    /// added it, gone where one side took it away. For names, a change on either side wins
    /// (this computer's, when both changed the same face).
    pub fn merge(base: &Meta, here: &Meta, there: &Meta) -> Meta {
        let albums = merge_set(&base.albums.keys().cloned().collect(), &here.albums.keys().cloned().collect(), &there.albums.keys().cloned().collect())
            .into_iter()
            .map(|name| {
                let none = BTreeSet::new();
                let get = |m: &Meta| m.albums.get(&name).unwrap_or(&none).clone();
                let photos = merge_set(&get(base), &get(here), &get(there));
                (name, photos)
            })
            .collect();
        let keys: BTreeSet<&String> = here.names.keys().chain(there.names.keys()).chain(base.names.keys()).collect();
        let names = keys
            .into_iter()
            .filter_map(|k| {
                let (b, h, t) = (base.names.get(k), here.names.get(k), there.names.get(k));
                let v = if h == t || h != b { h } else { t };
                v.map(|v| (k.clone(), v.clone()))
            })
            .collect();
        Meta {
            favorites: merge_set(&base.favorites, &here.favorites, &there.favorites),
            archived: merge_set(&base.archived, &here.archived, &there.archived),
            albums,
            names,
        }
    }
}

/// In both, or added on one side since `base`.
fn merge_set(base: &BTreeSet<String>, a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    a.iter().filter(|x| b.contains(*x) || !base.contains(*x)).chain(b.iter().filter(|x| !base.contains(*x))).cloned().collect()
}

// --- Computers paired from this one ------------------------------------------------------

/// Which way photos go with a computer.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum Direction {
    /// Each gets what the other has.
    Both,
    /// This computer's photos go there; that one keeps a copy of this one.
    Send,
    /// That computer's photos come here; this one keeps a copy of that one.
    Get,
}

/// A computer this one syncs with, as saved in computers.json.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Computer {
    pub name: String,
    /// SHA-256 of its certificate: the only one trusted for it.
    pub fingerprint: String,
    /// Addresses to try, the last one that worked first.
    pub hosts: Vec<String>,
    pub port: u16,
    pub token: String,
    pub direction: Direction,
    /// When the last sync finished (seconds since 1970), and what it did.
    #[serde(default)]
    pub last_sync: Option<i64>,
    #[serde(default)]
    pub last_note: String,
}

fn computers_file() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("computers.json"))
}

pub fn load_computers() -> Vec<Computer> {
    computers_file().map(|f| crate::store::load_json(&f)).unwrap_or_default()
}

pub fn save_computers(list: &[Computer]) {
    if let (Some(f), Ok(json)) = (computers_file(), serde_json::to_vec_pretty(list)) {
        crate::store::write_atomic(&f, &json);
    }
}

/// What both computers had at the last sync with this one, for the next merge, in `data`.
fn base_file(data: &Path, fingerprint: &str) -> PathBuf {
    data.join(format!("sync-{}.json", &fingerprint[..fingerprint.len().min(12)]))
}

/// Forgets a computer (it's also removed on the other side the next time it tries).
pub fn forget(fingerprint: &str) {
    let mut list = load_computers();
    list.retain(|c| c.fingerprint != fingerprint);
    save_computers(&list);
    if let Some(data) = crate::store::data_dir() {
        let _ = std::fs::remove_file(base_file(&data, fingerprint));
    }
}

/// A computer running Banana Photos with Connect on, seen on the network.
#[derive(Clone, Debug)]
pub struct Found {
    pub name: String,
    pub fingerprint: String,
    pub hosts: Vec<String>,
    pub port: u16,
}

/// The computers announcing themselves on the network within `wait`, other than this one.
pub fn discover(own_fingerprint: &str, wait: Duration) -> Vec<Found> {
    let Ok(daemon) = mdns_sd::ServiceDaemon::new() else { return Vec::new() };
    let Ok(rx) = daemon.browse(crate::remote::SERVICE) else { return Vec::new() };
    let deadline = Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else { break };
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            let Some(fp) = info.get_property_val_str("f").map(String::from) else { continue };
            if fp == own_fingerprint || found.iter().any(|f| f.fingerprint == fp) {
                continue;
            }
            let mut hosts: Vec<String> = info.get_addresses().iter().filter(|a| a.is_ipv4()).map(|a| a.to_string()).collect();
            hosts.sort_by_key(|h| !h.starts_with("192.168."));
            let name = info.get_fullname().split("._bananaphotos").next().unwrap_or("Computer").replace("\\ ", " ");
            found.push(Found { name, fingerprint: fp, hosts, port: info.get_port() });
        }
    }
    let _ = daemon.shutdown();
    // Testing (debug builds): BANANA_SYNC_FOUND="name,fingerprint,host,port" adds a computer
    // mDNS can't see (in a container, say).
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var("BANANA_SYNC_FOUND").ok() {
        let p: Vec<&str> = v.split(',').collect();
        if let [name, fp, host, port] = p[..] {
            found.push(Found { name: name.into(), fingerprint: fp.into(), hosts: vec![host.into()], port: port.parse().unwrap_or(0) });
        }
    }
    found
}

fn url(host: &str, port: u16, path: &str) -> String {
    format!("https://{host}:{port}{path}")
}

/// Pairs with `found` using the six digits it shows under Connect.
/// One typed in by its address ("192.168.1.11", or with ":port") goes without a fingerprint,
/// which is read from it here.
pub fn pair(found: &Found, pin: &str, my_name: &str) -> Result<Computer, String> {
    let mut found = found.clone();
    if found.fingerprint.is_empty() {
        let host = found.hosts.first().cloned().unwrap_or_default();
        found.fingerprint = crate::remote::fingerprint_at(&host, found.port).map_err(|e| format!("Couldn't reach {host} ({e})"))?;
    }
    let found = &found;
    let agent = crate::remote::pinned_agent(&found.fingerprint);
    let mut last = String::from("no address");
    for host in &found.hosts {
        let body = serde_json::json!({ "code": pin.replace(' ', ""), "name": my_name, "computer": true });
        match agent.post(&url(host, found.port, "/api/pair")).send_json(body) {
            Ok(r) => {
                let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
                let token = v["token"].as_str().ok_or("no token")?.to_string();
                let mut hosts = vec![host.clone()];
                hosts.extend(found.hosts.iter().filter(|h| *h != host).cloned());
                return Ok(Computer {
                    name: v["name"].as_str().unwrap_or(&found.name).to_string(),
                    fingerprint: found.fingerprint.clone(),
                    hosts,
                    port: found.port,
                    token,
                    direction: Direction::Both,
                    last_sync: None,
                    last_note: String::new(),
                });
            }
            Err(ureq::Error::Status(403, _)) => return Err("Wrong or expired code. Check the six digits under Connect on that computer.".into()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("Couldn't reach {} ({last})", found.name))
}

/// What this computer has, for a sync: its library folder, the files in it, and favourites,
/// albums and names (made on the app's thread).
pub struct Local {
    pub root: PathBuf,
    pub files: Vec<(String, u64)>,
    pub meta: Meta,
    /// The data folder, where what was synced last time is kept.
    pub data: PathBuf,
}

pub enum Msg {
    Progress { fingerprint: String, text: String },
    /// `computer` comes back with the address that worked and the result noted.
    Done { computer: Box<Computer>, result: Result<Outcome, String> },
}

pub struct Outcome {
    pub received: usize,
    pub sent: usize,
    /// Favourites, archive, albums and names to take on here.
    pub meta: Option<Meta>,
}

/// Syncs with `computer` on a background thread; reports through `tx`.
pub fn spawn(mut computer: Computer, local: Local, tx: Sender<Msg>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let fp = computer.fingerprint.clone();
        let progress = |text: String| {
            let _ = tx.send(Msg::Progress { fingerprint: fp.clone(), text });
            ctx.request_repaint();
        };
        let result = run(&mut computer, &local, &progress);
        let now = chrono::Utc::now().timestamp();
        match &result {
            Ok(o) => {
                computer.last_sync = Some(now);
                computer.last_note = match (o.received, o.sent) {
                    (0, 0) => "nothing new".into(),
                    (r, 0) => format!("{r} received"),
                    (0, s) => format!("{s} sent"),
                    (r, s) => format!("{r} received, {s} sent"),
                };
            }
            Err(e) => computer.last_note = e.clone(),
        }
        let _ = tx.send(Msg::Done { computer: Box::new(computer), result });
        ctx.request_repaint();
    });
}

struct Conn {
    agent: ureq::Agent,
    host: String,
    port: u16,
    auth: String,
}

impl Conn {
    fn get(&self, path: &str) -> Result<ureq::Response, String> {
        self.agent.get(&url(&self.host, self.port, path)).set("Authorization", &self.auth).call().map_err(status_error)
    }
}

fn status_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => "That computer no longer knows this one: pair again.".into(),
        ureq::Error::Status(503, _) => "No library folder is open on that computer.".into(),
        ureq::Error::Status(code, r) => r.into_string().ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["error"].as_str().map(String::from)).unwrap_or(format!("error {code}")),
        e => e.to_string(),
    }
}

/// Reaches the computer at an address that worked before, or finds it again on the network.
fn connect(c: &mut Computer) -> Result<Conn, String> {
    let agent = crate::remote::pinned_agent(&c.fingerprint);
    let auth = format!("Bearer {}", c.token);
    let try_hosts = |hosts: &[String], port: u16| {
        hosts.iter().find_map(|h| {
            let conn = Conn { agent: agent.clone(), host: h.clone(), port, auth: auth.clone() };
            match conn.get("/api/info") {
                Ok(_) => Some(Ok(conn)),
                Err(e) if e.contains("pair again") => Some(Err(e)),
                Err(_) => None,
            }
        })
    };
    if let Some(r) = try_hosts(&c.hosts, c.port) {
        return r;
    }
    let found = discover("", Duration::from_secs(3)).into_iter().find(|f| f.fingerprint == c.fingerprint);
    if let Some(f) = found {
        if let Some(r) = try_hosts(&f.hosts, f.port) {
            c.hosts = f.hosts;
            c.port = f.port;
            return r;
        }
    }
    Err(format!("Can't reach {}. Is Banana Photos open there, with Connect on?", c.name))
}

/// (lower-case file name, size): the same photo, wherever it is.
fn same_file(rel: &str, size: u64) -> (String, u64) {
    (rel.rsplit('/').next().unwrap_or(rel).to_lowercase(), size)
}

fn run(c: &mut Computer, local: &Local, progress: &dyn Fn(String)) -> Result<Outcome, String> {
    progress(format!("Connecting to {}…", c.name));
    let conn = connect(c)?;
    let state: serde_json::Value = conn.get("/api/sync/state")?.into_json().map_err(|e| e.to_string())?;
    if let Some(name) = state["name"].as_str() {
        c.name = name.to_string();
    }
    let theirs: Vec<(String, u64)> = serde_json::from_value(state["files"].clone()).map_err(|e| e.to_string())?;
    let their_meta: Meta = serde_json::from_value(state["meta"].clone()).map_err(|e| e.to_string())?;

    let here: HashMap<&str, u64> = local.files.iter().map(|(r, s)| (r.as_str(), *s)).collect();
    let here_any: HashSet<(String, u64)> = local.files.iter().map(|(r, s)| same_file(r, *s)).collect();
    let there: HashMap<&str, u64> = theirs.iter().map(|(r, s)| (r.as_str(), *s)).collect();
    let there_any: HashSet<(String, u64)> = theirs.iter().map(|(r, s)| same_file(r, *s)).collect();
    let wanted = |r: &str, s: u64, have: &HashMap<&str, u64>, have_any: &HashSet<(String, u64)>| {
        let (name, size) = same_file(r, s);
        !have.contains_key(r) && !have_any.contains(&(name.clone(), size)) && !crate::remote::was_deleted(&name, size)
    };
    let pull: Vec<&(String, u64)> = if c.direction == Direction::Send { Vec::new() } else { theirs.iter().filter(|(r, s)| wanted(r, *s, &here, &here_any)).collect() };
    let push: Vec<&(String, u64)> = if c.direction == Direction::Get { Vec::new() } else { local.files.iter().filter(|(r, s)| wanted(r, *s, &there, &there_any)).collect() };

    let mut out = Outcome { received: 0, sent: 0, meta: None };
    for (i, (rel, size)) in pull.iter().enumerate() {
        progress(format!("Receiving {} of {} from {}…", i + 1, pull.len(), c.name));
        if receive(&conn, &local.root, rel, *size, &c.name)? {
            out.received += 1;
        }
    }
    for (i, (rel, size)) in push.iter().enumerate() {
        progress(format!("Sending {} of {} to {}…", i + 1, push.len(), c.name));
        let Some(path) = abs(&local.root, rel) else { continue };
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let r = conn
            .agent
            .post(&url(&conn.host, conn.port, &format!("/api/sync/upload?path={}", crate::transfer::google::encode(rel))))
            .set("Authorization", &conn.auth)
            .set("Content-Length", &size.to_string())
            .set("Content-Type", "application/octet-stream")
            .send(file)
            .map_err(status_error)?;
        let v: serde_json::Value = r.into_json().unwrap_or_default();
        if v["status"] == "imported" {
            out.sent += 1;
        }
    }

    // Favourites, archive, albums and names.
    let base: Meta = crate::store::load_json(&base_file(&local.data, &c.fingerprint));
    let merged = match c.direction {
        Direction::Both => Meta::merge(&base, &local.meta, &their_meta),
        Direction::Send => local.meta.clone(),
        Direction::Get => their_meta.clone(),
    };
    if merged != their_meta && c.direction != Direction::Get {
        conn.agent
            .post(&url(&conn.host, conn.port, "/api/sync/meta"))
            .set("Authorization", &conn.auth)
            .send_json(serde_json::to_value(&merged).map_err(|e| e.to_string())?)
            .map_err(status_error)?;
    }
    if merged != local.meta && c.direction != Direction::Send {
        out.meta = Some(merged.clone());
    }
    if let Ok(json) = serde_json::to_vec(&merged) {
        crate::store::write_atomic(&base_file(&local.data, &c.fingerprint), &json);
    }
    Ok(out)
}

/// Downloads one file to the same place in this library (next to a different file of the same
/// name, as "name (from <computer>).ext"). False when it was here already.
fn receive(conn: &Conn, root: &Path, rel: &str, size: u64, from: &str) -> Result<bool, String> {
    let Some(mut dest) = abs(root, rel) else { return Ok(false) };
    if std::fs::metadata(&dest).is_ok_and(|m| m.len() == size) {
        return Ok(false);
    }
    if dest.exists() {
        let stem = dest.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        dest.set_file_name(format!("{stem} (from {from}){ext}"));
    }
    let r = conn.get(&format!("/api/sync/file?path={}", crate::transfer::google::encode(rel)))?;
    let part = dest.with_extension("banana-part");
    let written = (|| {
        std::fs::create_dir_all(dest.parent().ok_or_else(|| std::io::Error::other("no folder"))?)?;
        let mut f = std::fs::File::create(&part)?;
        let n = std::io::copy(&mut r.into_reader(), &mut f)?;
        f.sync_all()?;
        Ok::<u64, std::io::Error>(n)
    })();
    match written {
        Ok(n) if n == size => std::fs::rename(&part, &dest).map(|_| true).map_err(|e| e.to_string()),
        Ok(_) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("{rel}: the download was cut short"))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("{rel}: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paths_stay_inside_the_library() {
        let root = Path::new("/lib");
        assert_eq!(rel(root, Path::new("/lib/2024/a.jpg")).as_deref(), Some("2024/a.jpg"));
        assert_eq!(rel(root, Path::new("/other/a.jpg")), None);
        assert_eq!(abs(root, "2024/a.jpg"), Some(PathBuf::from("/lib/2024/a.jpg")));
        for bad in ["../etc/passwd", "a/../../b", "/etc/passwd", "", "a//b", "C:\\x", "a\\..\\b"] {
            assert_eq!(abs(root, bad), None, "{bad}");
        }
    }

    #[test]
    fn changes_on_either_side_win() {
        let base = Meta { favorites: set(&["a", "b"]), names: [("p#0".into(), "Ali".into())].into(), ..Default::default() };
        // Here: b unfavourited, c favourited, Ali renamed. There: d favourited.
        let here = Meta { favorites: set(&["a", "c"]), names: [("p#0".into(), "Ali Khan".into())].into(), ..Default::default() };
        let there = Meta { favorites: set(&["a", "b", "d"]), names: [("p#0".into(), "Ali".into()), ("q#1".into(), "Sara".into())].into(), ..Default::default() };
        let m = Meta::merge(&base, &here, &there);
        assert_eq!(m.favorites, set(&["a", "c", "d"]));
        assert_eq!(m.names.get("p#0").map(String::as_str), Some("Ali Khan"));
        assert_eq!(m.names.get("q#1").map(String::as_str), Some("Sara"));
        // The same merge from the other side gives the same result.
        assert_eq!(Meta::merge(&base, &there, &here).favorites, m.favorites);
    }

    #[test]
    fn albums_merge_photo_by_photo() {
        let base = Meta { albums: [("Trip".into(), set(&["1", "2"]))].into(), ..Default::default() };
        let here = Meta { albums: [("Trip".into(), set(&["1", "2", "3"])), ("New".into(), set(&["9"]))].into(), ..Default::default() };
        let there = Meta { albums: [("Trip".into(), set(&["2"]))].into(), ..Default::default() };
        let m = Meta::merge(&base, &here, &there);
        assert_eq!(m.albums.get("Trip"), Some(&set(&["2", "3"])));
        assert_eq!(m.albums.get("New"), Some(&set(&["9"])));
        // An album deleted on one side (and not changed on the other) goes.
        let gone = Meta { albums: BTreeMap::new(), ..Default::default() };
        assert!(Meta::merge(&base, &gone, &base).albums.is_empty());
    }

    fn jpeg(shade: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(32, 24, image::Rgb([shade, 100, 50]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode_image(&img).unwrap();
        out
    }

    /// Two libraries synced over a real (local) connection: pairing with the six digits, files
    /// both ways at the same paths, favourites and albums merged, a second sync with nothing to
    /// do, and taking a favourite off on one side.
    #[test]
    fn two_computers_sync() {
        let base = std::env::temp_dir().join(format!("banana-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (there_root, here_root) = (base.join("desktop"), base.join("laptop"));
        std::fs::create_dir_all(there_root.join("2024")).unwrap();
        std::fs::create_dir_all(&here_root).unwrap();
        std::env::set_var("BANANA_DATA", base.join("data"));
        std::fs::write(there_root.join("a.jpg"), jpeg(10)).unwrap();
        std::fs::write(there_root.join("2024/b.jpg"), jpeg(20)).unwrap();
        std::fs::write(here_root.join("c.jpg"), jpeg(30)).unwrap();

        // The other computer ("desktop"): a favourite and an album.
        let r = crate::remote::Remote::start_on(eframe::egui::Context::default(), 0, 0).unwrap();
        let mut store = Store::default();
        store.favorites.insert(there_root.join("a.jpg"));
        store.albums.push(crate::store::Album { name: "Trip".into(), photos: vec![there_root.join("2024/b.jpg")] });
        let photos: Vec<_> = ["a.jpg", "2024/b.jpg"].iter().filter_map(|p| crate::library::read_photo(&there_root.join(p))).collect();
        r.set_library(crate::remote::snapshot(Some(there_root.clone()), &photos, &store, &[], &crate::people::Index::default()));

        // Pairing: a wrong code fails, the six digits work.
        r.new_pairing_code();
        let found = Found { name: "desktop".into(), fingerprint: r.shared.fingerprint.clone(), hosts: vec!["127.0.0.1".into()], port: r.shared.port };
        assert!(pair(&found, "000000x", "laptop").is_err());
        let mut c = pair(&found, &r.pairing_pin().unwrap(), "laptop").unwrap();
        assert!(r.phones()[0].computer);

        // This computer ("laptop"): its own photo, a favourite.
        let local = |favorites: &[&str]| Local {
            root: here_root.clone(),
            files: std::fs::read_dir(&here_root).unwrap().flatten().filter(|e| e.path().is_file()).map(|e| (e.file_name().to_string_lossy().into(), e.metadata().unwrap().len())).collect(),
            meta: Meta { favorites: set(favorites), ..Default::default() },
            data: base.join("laptop-data"),
        };
        let out = run(&mut c, &local(&["c.jpg"]), &|_| {}).unwrap();
        assert_eq!((out.received, out.sent), (2, 1));
        assert!(here_root.join("2024/b.jpg").is_file() && there_root.join("c.jpg").is_file());
        let merged = out.meta.unwrap();
        assert_eq!(merged.favorites, set(&["a.jpg", "c.jpg"]));
        assert_eq!(merged.albums.get("Trip"), Some(&set(&["2024/b.jpg"])));
        let pushed = r.events.try_iter().find_map(|e| match e {
            crate::remote::Event::SyncMeta(m) => Some(m),
            _ => None,
        });
        assert_eq!(pushed.map(|m| m.favorites), Some(set(&["a.jpg", "c.jpg"])));

        // The other computer took the merge on; nothing new to do now.
        merged.apply_to(&there_root, &mut store);
        let photos: Vec<_> = ["a.jpg", "2024/b.jpg", "c.jpg"].iter().filter_map(|p| crate::library::read_photo(&there_root.join(p))).collect();
        r.set_library(crate::remote::snapshot(Some(there_root.clone()), &photos, &store, &[], &crate::people::Index::default()));
        let mut files_here = local(&["a.jpg", "c.jpg"]);
        files_here.files.push(("2024/b.jpg".into(), std::fs::metadata(here_root.join("2024/b.jpg")).unwrap().len()));
        files_here.meta.albums.insert("Trip".into(), set(&["2024/b.jpg"]));
        let again = run(&mut c, &files_here, &|_| {}).unwrap();
        assert_eq!((again.received, again.sent), (0, 0));
        assert!(again.meta.is_none());

        // Taking a favourite off here takes it off there.
        files_here.meta.favorites = set(&["c.jpg"]);
        run(&mut c, &files_here, &|_| {}).unwrap();
        let pushed = r.events.try_iter().filter_map(|e| match e {
            crate::remote::Event::SyncMeta(m) => Some(m),
            _ => None,
        }).last();
        assert_eq!(pushed.map(|m| m.favorites), Some(set(&["c.jpg"])));

        // One way: "Get" brings the other's new photo here but sends nothing back; "Send" the
        // reverse.
        std::fs::write(there_root.join("d.jpg"), jpeg(40)).unwrap();
        std::fs::write(here_root.join("e.jpg"), jpeg(50)).unwrap();
        let photos: Vec<_> = ["a.jpg", "2024/b.jpg", "c.jpg", "d.jpg"].iter().filter_map(|p| crate::library::read_photo(&there_root.join(p))).collect();
        r.set_library(crate::remote::snapshot(Some(there_root.clone()), &photos, &store, &[], &crate::people::Index::default()));
        let mut files_here = local(&["c.jpg"]);
        files_here.files.push(("2024/b.jpg".into(), std::fs::metadata(here_root.join("2024/b.jpg")).unwrap().len()));
        c.direction = Direction::Get;
        let got = run(&mut c, &files_here, &|_| {}).unwrap();
        assert_eq!((got.received, got.sent), (1, 0));
        assert!(here_root.join("d.jpg").is_file() && !there_root.join("e.jpg").exists());
        c.direction = Direction::Send;
        files_here.files.push(("d.jpg".into(), std::fs::metadata(here_root.join("d.jpg")).unwrap().len()));
        let sent = run(&mut c, &files_here, &|_| {}).unwrap();
        assert_eq!((sent.received, sent.sent), (0, 1));
        assert!(there_root.join("e.jpg").is_file());

        drop(r);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_round_trip_keeps_other_folders() {
        std::env::set_var("BANANA_DATA", std::env::temp_dir().join(format!("banana-sync-store-{}", std::process::id())));
        let root = Path::new("/lib");
        let mut store = Store::default();
        store.favorites.insert(PathBuf::from("/lib/a.jpg"));
        store.favorites.insert(PathBuf::from("/elsewhere/x.jpg"));
        store.face_names.insert("/lib/a.jpg#0".into(), "Ali".into());
        store.albums.push(crate::store::Album { name: "Trip".into(), photos: vec![PathBuf::from("/lib/a.jpg")] });
        let meta = Meta::from_store(root, &store);
        assert_eq!(meta.favorites, set(&["a.jpg"]));
        assert_eq!(meta.names.get("a.jpg#0").map(String::as_str), Some("Ali"));
        let mut changed = meta.clone();
        changed.favorites = set(&["b.jpg"]);
        changed.albums.insert("Trip".into(), set(&["a.jpg", "b.jpg"]));
        changed.apply_to(root, &mut store);
        assert!(store.favorites.contains(Path::new("/lib/b.jpg")) && store.favorites.contains(Path::new("/elsewhere/x.jpg")));
        assert!(!store.favorites.contains(Path::new("/lib/a.jpg")));
        assert_eq!(store.albums[0].photos.len(), 2);
        assert_eq!(Meta::from_store(root, &store), changed);
    }
}
