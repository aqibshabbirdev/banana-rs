//! Phones on the same network: the phone app (Android and iOS) shows this computer's library
//! and backs its camera roll up to it, like WhatsApp Web the other way round. Nothing leaves
//! the local network.
//!
//! Off until turned on under "Connect phone". Then this computer listens on `PORT` (HTTPS) and
//! `MEDIA_PORT` (plain HTTP, only for signed video links), and announces itself over mDNS as
//! `SERVICE`. A phone pairs by scanning the QR code shown there, which carries the addresses, a
//! one-time pairing code and the SHA-256 fingerprint of this computer's certificate: the phone
//! trusts exactly that certificate, and trades the code for a token it sends from then on. The
//! API is in `api`, and written up for the phone app in docs/phone-api.md.

pub mod api;
mod http;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use eframe::egui;
use sha2::{Digest, Sha256};

pub const PORT: u16 = 47820;
pub const MEDIA_PORT: u16 = 47821;
pub const SERVICE: &str = "_bananaphotos._tcp.local.";
/// How long a QR code's pairing code works.
const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong codes before the current one stops working (a new one shows when the page reopens).
const PAIRING_TRIES: u32 = 5;

/// One photo or video as the phone sees it. `id` is stable for a path (see `photo_id`).
#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub path: PathBuf,
    pub taken: NaiveDateTime,
    pub aspect: f32,
    pub is_video: bool,
    pub duration: Option<f32>,
    pub size: u64,
    pub mtime: u64,
    pub favorite: bool,
}

/// The library as the app last laid it out, newest first. Replaced whole when it changes.
#[derive(Default)]
pub struct Snapshot {
    pub root: Option<PathBuf>,
    pub items: Vec<Item>,
    pub by_id: HashMap<u64, usize>,
    /// Goes up with every change, so the phone can tell its list is stale.
    pub version: u64,
    pub albums: Arc<Vec<Album>>,
    /// People the computer recognised (not the hidden ones), most photos first.
    pub people: Arc<Vec<Person>>,
    /// Towns photos were taken in (see places.rs), most photos first.
    pub places: Arc<Vec<Place>>,
    /// For computers syncing with this one: every file in the folder (archived ones too), as
    /// (path inside the folder, size), and favourites, albums and names the same way.
    pub files: Arc<Vec<(String, u64)>>,
    pub meta: Arc<crate::sync::Meta>,
}

pub struct Place {
    /// The town's id in places.rs.
    pub id: u32,
    pub name: String,
    /// In timeline order (newest first).
    pub ids: Vec<u64>,
}

pub struct Album {
    pub name: String,
    /// In timeline order (newest first).
    pub ids: Vec<u64>,
}

pub struct Person {
    /// Stable while the person keeps the same cover face.
    pub id: u64,
    pub name: Option<String>,
    /// In timeline order (newest first).
    pub ids: Vec<u64>,
    /// The cover face's small square picture.
    pub face: Option<PathBuf>,
}

/// A copy of a snapshot (its big parts are shared), to change one part.
fn clone_snapshot(s: &Snapshot) -> Snapshot {
    Snapshot {
        root: s.root.clone(),
        items: s.items.clone(),
        by_id: s.by_id.clone(),
        version: s.version,
        albums: s.albums.clone(),
        people: s.people.clone(),
        places: s.places.clone(),
        files: s.files.clone(),
        meta: s.meta.clone(),
    }
}

pub fn photo_id(path: &Path) -> u64 {
    crate::thumbs::cache_key(path, 0, 0)
}

/// Things a phone did that the app has to act on.
pub enum Event {
    Paired(String),
    /// A photo or video was uploaded into the library folder; scan it.
    Uploaded,
    Favorite(PathBuf, bool),
    /// Moved to the Trash from a phone (path, mtime, size): forget them and re-scan.
    Trashed(Vec<(PathBuf, u64, u64)>),
    /// Favourites, archive, albums and names after a sync with another computer: take them on.
    SyncMeta(crate::sync::Meta),
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Phone {
    pub name: String,
    /// SHA-256 of its token (hex); the token itself is only on the phone.
    token_hash: String,
    pub paired: i64,
    pub last_seen: i64,
    /// Another computer syncing with this one (Connect → Computers), not a phone.
    #[serde(default)]
    pub computer: bool,
}

impl Phone {
    /// Short stable handle for the UI's "Remove".
    pub fn key(&self) -> &str {
        &self.token_hash[..12]
    }
}

struct Pairing {
    code: String,
    /// What a computer types instead of scanning: six digits.
    pin: String,
    until: Instant,
    misses: u32,
}

pub struct Shared {
    library: RwLock<Arc<Snapshot>>,
    pairing: Mutex<Option<Pairing>>,
    phones: Mutex<Vec<Phone>>,
    /// Signs video links for the media port; new each start, so old links stop working.
    media_key: [u8; 32],
    pub fingerprint: String,
    pub name: String,
    /// The ports actually listened on (`PORT` and `MEDIA_PORT`, except in tests).
    pub port: u16,
    pub media_port: u16,
    events: Mutex<Sender<Event>>,
    ctx: egui::Context,
    search: Mutex<api::Search>,
    /// Videos being converted for phones that can't play them (by id), shared with the
    /// converting thread; one is converted at a time.
    conversions: Arc<Mutex<HashMap<u64, Conversion>>>,
    converting: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub enum Conversion {
    /// Waiting its turn, or converting (0–1).
    Running(f32),
    Failed(String),
}

/// Where a video converted for phones is kept (it's made again if the video changes).
pub fn converted_path(it: &Item) -> Option<PathBuf> {
    let key = crate::thumbs::cache_key(&it.path, it.mtime, it.size);
    Some(dirs::cache_dir()?.join("banana-rs").join("phone-videos").join(format!("{key:016x}.mp4")))
}

/// Keeps converted videos under 4 GB, dropping the ones made longest ago.
fn prune_converted(dir: &Path) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, path) in files {
        if total <= 4 << 30 {
            break;
        }
        let _ = std::fs::remove_file(path);
        total -= len;
    }
}

impl Shared {
    fn library(&self) -> Arc<Snapshot> {
        self.library.read().unwrap().clone()
    }

    /// Changes the library the phones see right away, without waiting for the app's UI (which
    /// doesn't run while its window is minimised). The app sends a full snapshot again later.
    fn edit_library(&self, f: impl FnOnce(&mut Vec<Item>)) {
        let mut lib = self.library.write().unwrap();
        let mut items = lib.items.clone();
        f(&mut items);
        let by_id = items.iter().enumerate().map(|(i, it)| (it.id, i)).collect();
        *lib = Arc::new(Snapshot { root: lib.root.clone(), items, by_id, version: lib.version + 1, albums: lib.albums.clone(), people: lib.people.clone(), places: lib.places.clone(), files: lib.files.clone(), meta: lib.meta.clone() });
    }

    /// A photo a phone just uploaded, so it shows on the phones before the app re-scans.
    fn add_photo(&self, photo: &crate::library::Photo) {
        let item = Item {
            id: photo_id(&photo.path),
            path: photo.path.clone(),
            taken: photo.taken,
            aspect: photo.aspect,
            is_video: photo.is_video,
            duration: photo.duration,
            size: photo.size,
            mtime: photo.mtime,
            favorite: false,
        };
        self.edit_library(|items| {
            items.retain(|it| it.id != item.id);
            let at = items.partition_point(|it| it.taken > item.taken);
            items.insert(at, item);
        });
        // And to the files a syncing computer sees.
        let mut lib = self.library.write().unwrap();
        if let Some(rel) = lib.root.as_ref().and_then(|r| crate::sync::rel(r, &photo.path)) {
            let mut files = lib.files.as_ref().clone();
            files.retain(|(r, _)| r != &rel);
            files.push((rel, photo.size));
            let next = Snapshot { files: Arc::new(files), ..clone_snapshot(&lib) };
            *lib = Arc::new(next);
        }
    }

    fn set_favorite(&self, id: u64, on: bool) {
        self.edit_library(|items| {
            if let Some(it) = items.iter_mut().find(|it| it.id == id) {
                it.favorite = on;
            }
        });
    }

    fn remove_photos(&self, ids: &[u64]) {
        self.edit_library(|items| items.retain(|it| !ids.contains(&it.id)));
    }

    /// Where converting a video for phones stands; starts it the first time it's asked for.
    fn conversion(&self, it: &Item) -> Conversion {
        let mut all = self.conversions.lock().unwrap();
        if let Some(c) = all.get(&it.id) {
            return c.clone();
        }
        all.insert(it.id, Conversion::Running(0.0));
        let (id, src, dst) = (it.id, it.path.clone(), converted_path(it));
        let (conversions, converting) = (self.conversions.clone(), self.converting.clone());
        std::thread::spawn(move || {
            let _turn = converting.lock().unwrap();
            let set = |c: Conversion| {
                conversions.lock().unwrap().insert(id, c);
            };
            let Some(dst) = dst else { return set(Conversion::Failed("no cache folder".into())) };
            match crate::video::convert_for_phone(&src, &dst, &|p| set(Conversion::Running(p))) {
                Ok(()) => {
                    conversions.lock().unwrap().remove(&id);
                    if let Some(dir) = dst.parent() {
                        prune_converted(dir);
                    }
                }
                Err(e) => set(Conversion::Failed(e)),
            }
        });
        Conversion::Running(0.0)
    }

    fn send(&self, e: Event) {
        let _ = self.events.lock().unwrap().send(e);
        self.ctx.request_repaint();
    }

    /// The paired phone a bearer token belongs to (and notes that it was seen).
    fn phone_for(&self, token: &str) -> Option<Phone> {
        let hash = hex(&Sha256::digest(token.as_bytes()));
        let mut phones = self.phones.lock().unwrap();
        let p = phones.iter_mut().find(|p| p.token_hash == hash)?;
        let now = chrono::Utc::now().timestamp();
        // Saved at most every few minutes; it's only shown as "last seen".
        if now - p.last_seen > 300 {
            p.last_seen = now;
            let snapshot = phones.clone();
            save_phones(&snapshot);
            return snapshot.into_iter().find(|p| p.token_hash == hash);
        }
        Some(p.clone())
    }

    /// Trades the QR code's pairing code for a token. Returns None for a wrong or expired code.
    fn pair(&self, code: &str, phone_name: &str, computer: bool) -> Option<String> {
        let mut pairing = self.pairing.lock().unwrap();
        let p = pairing.as_mut()?;
        if Instant::now() > p.until {
            *pairing = None;
            return None;
        }
        let right = constant_time_eq(p.code.as_bytes(), code.as_bytes()) || (computer && constant_time_eq(p.pin.as_bytes(), code.as_bytes()));
        if !right {
            p.misses += 1;
            if p.misses >= PAIRING_TRIES {
                *pairing = None;
            }
            return None;
        }
        // One code, one phone.
        *pairing = None;
        drop(pairing);
        let token = random_hex(32);
        let now = chrono::Utc::now().timestamp();
        let name: String = phone_name.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_string();
        let name = if name.is_empty() { (if computer { "Computer" } else { "Phone" }).to_string() } else { name };
        let mut phones = self.phones.lock().unwrap();
        phones.push(Phone { name: name.clone(), token_hash: hex(&Sha256::digest(token.as_bytes())), paired: now, last_seen: now, computer });
        save_phones(&phones);
        drop(phones);
        self.send(Event::Paired(name));
        Some(token)
    }

    fn sign(&self, id: u64, expires: i64) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.media_key).expect("any key length works");
        mac.update(format!("{id:016x}:{expires}").as_bytes());
        hex(&mac.finalize().into_bytes())
    }
}

/// The running server. Dropping it stops listening and withdraws the mDNS announcement.
pub struct Remote {
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    /// The listening threads; waited for on drop, so the ports are free again right away.
    listeners: Vec<std::thread::JoinHandle<()>>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    pub events: Receiver<Event>,
}

impl Remote {
    pub fn start(ctx: egui::Context) -> Result<Self, String> {
        Self::start_on(ctx, PORT, MEDIA_PORT)
    }

    pub(crate) fn start_on(ctx: egui::Context, port: u16, media_port: u16) -> Result<Self, String> {
        let (cert, key) = load_or_make_cert()?;
        let fingerprint = hex(&Sha256::digest(&cert));
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(vec![cert.into()], rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()))
            .map_err(|e| e.to_string())?;
        let bind = |port: u16| {
            TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|e| format!("Port {port} is in use or blocked ({e}). Is Banana Photos already running?"))
        };
        let (main, media) = (bind(port)?, bind(media_port)?);
        let local_port = |l: &TcpListener| l.local_addr().map(|a| a.port()).unwrap_or(0);
        let (port, media_port) = (local_port(&main), local_port(&media));

        let (tx, events) = channel();
        let mut media_key = [0u8; 32];
        getrandom::getrandom(&mut media_key).map_err(|e| e.to_string())?;
        // Versions start from the clock, so they keep going up across restarts and a phone
        // never mistakes a new library for the one it has.
        let start_version = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let shared = Arc::new(Shared {
            library: RwLock::new(Arc::new(Snapshot { version: start_version, ..Snapshot::default() })),
            pairing: Mutex::new(None),
            phones: Mutex::new(load_phones()),
            media_key,
            fingerprint,
            name: computer_name(),
            port,
            media_port,
            events: Mutex::new(tx),
            ctx,
            search: Mutex::new(api::Search::default()),
            conversions: Arc::default(),
            converting: Arc::default(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let mut listeners = Vec::new();
        for (listener, tls) in [(main, Some(Arc::new(tls))), (media, None)] {
            let (shared, stop) = (shared.clone(), stop.clone());
            let media_only = tls.is_none();
            let handler: Arc<http::Handler> = Arc::new(move |req, body| api::handle(&shared, req, body, media_only));
            listeners.push(std::thread::spawn(move || http::serve(listener, tls, handler, stop)));
        }
        let mdns = announce(&shared).ok();
        Ok(Self { shared, stop, listeners, mdns, events })
    }

    pub fn set_library(&self, snapshot: Snapshot) {
        let mut lib = self.shared.library.write().unwrap();
        let version = lib.version + 1;
        *lib = Arc::new(Snapshot { version, ..snapshot });
    }

    /// A fresh one-time code for the QR code; the previous one stops working.
    pub fn new_pairing_code(&self) -> String {
        let code = random_hex(8);
        let mut n = [0u8; 4];
        getrandom::getrandom(&mut n).expect("the OS random source works");
        let pin = format!("{:06}", u32::from_le_bytes(n) % 1_000_000);
        *self.shared.pairing.lock().unwrap() = Some(Pairing { code: code.clone(), pin, until: Instant::now() + PAIRING_TTL, misses: 0 });
        code
    }

    /// The six digits another computer can pair with while the current code is valid.
    pub fn pairing_pin(&self) -> Option<String> {
        self.shared.pairing.lock().unwrap().as_ref().filter(|p| Instant::now() < p.until).map(|p| p.pin.clone())
    }

    /// What the QR code holds: everything the phone needs to find and trust this computer.
    pub fn qr_payload(&self, code: &str) -> String {
        let hosts: Vec<String> = lan_addresses().iter().map(|a| a.to_string()).collect();
        let enc = crate::transfer::google::encode;
        format!(
            "bananaphotos://pair?v=1&n={}&h={}&p={}&m={}&c={code}&f={}",
            enc(&self.shared.name),
            enc(&hosts.join(",")),
            self.shared.port,
            self.shared.media_port,
            self.shared.fingerprint
        )
    }

    pub fn phones(&self) -> Vec<Phone> {
        self.shared.phones.lock().unwrap().clone()
    }

    /// Unpairs a phone: its token stops working at once.
    pub fn forget_phone(&self, key: &str) {
        let mut phones = self.shared.phones.lock().unwrap();
        phones.retain(|p| p.key() != key);
        save_phones(&phones);
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // They notice within 100 ms and close their sockets.
        for t in self.listeners.drain(..) {
            let _ = t.join();
        }
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
    }
}

/// Builds the list the phone browses from the app's photos.
pub fn snapshot(
    root: Option<PathBuf>,
    photos: &[crate::library::Photo],
    store: &crate::store::Store,
    people: &[crate::people::Person],
    analysis: &crate::people::Index,
) -> Snapshot {
    let mut items: Vec<Item> = photos
        .iter()
        .filter(|p| !store.is_archived(&p.path))
        .map(|p| Item {
            id: photo_id(&p.path),
            path: p.path.clone(),
            taken: p.taken,
            aspect: p.aspect,
            is_video: p.is_video,
            duration: p.duration,
            size: p.size,
            mtime: p.mtime,
            favorite: store.is_favorite(&p.path),
        })
        .collect();
    items.sort_by(|a, b| b.taken.cmp(&a.taken).then_with(|| a.path.cmp(&b.path)));
    let by_id: HashMap<u64, usize> = items.iter().enumerate().map(|(i, it)| (it.id, i)).collect();
    // Photos as ids in timeline order, leaving out ones not in the timeline (archived, gone).
    let ids = |paths: &mut dyn Iterator<Item = &PathBuf>| {
        let mut at: Vec<usize> = paths.filter_map(|p| by_id.get(&photo_id(p)).copied()).collect();
        at.sort_unstable();
        at.dedup();
        at.into_iter().map(|i| items[i].id).collect::<Vec<u64>>()
    };
    let albums = store.albums.iter().map(|a| Album { name: a.name.clone(), ids: ids(&mut a.photos.iter()) }).collect();
    let mut people: Vec<Person> = people
        .iter()
        .filter(|p| !p.hidden)
        .map(|p| Person {
            id: photo_id(Path::new(&p.cover.key())),
            name: p.name.clone(),
            ids: ids(&mut p.photos.iter()),
            face: analysis.entries.get(&p.cover.path).and_then(|a| crate::people::crop_path(&p.cover.path, a.mtime, a.size, p.cover.index)),
        })
        .filter(|p| !p.ids.is_empty())
        .collect();
    // Named people first, then by how many photos they're in.
    people.sort_by(|a, b| b.name.is_some().cmp(&a.name.is_some()).then(b.ids.len().cmp(&a.ids.len())).then(a.id.cmp(&b.id)));
    let mut by_place: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    for p in photos.iter().filter(|p| !store.is_archived(&p.path)) {
        if let Some(place) = p.gps.and_then(|(lat, lon)| crate::places::nearest(lat, lon)) {
            by_place.entry(place).or_default().push(p.path.clone());
        }
    }
    let mut places: Vec<Place> = by_place.into_iter().map(|(id, paths)| Place { id, name: crate::places::name(id), ids: ids(&mut paths.iter()) }).collect();
    places.sort_by(|a, b| b.ids.len().cmp(&a.ids.len()).then_with(|| a.name.cmp(&b.name)));
    let (files, meta) = match &root {
        Some(r) => (photos.iter().filter_map(|p| Some((crate::sync::rel(r, &p.path)?, p.size))).collect(), crate::sync::Meta::from_store(r, store)),
        None => (Vec::new(), crate::sync::Meta::default()),
    };
    Snapshot { root, items, by_id, version: 0, albums: Arc::new(albums), people: Arc::new(people), places: Arc::new(places), files: Arc::new(files), meta: Arc::new(meta) }
}

fn dir() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("phone"))
}

/// This computer's certificate and key (DER), made once and kept, so paired phones keep trusting it.
fn load_or_make_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let dir = dir().ok_or("no data folder")?;
    let (cert_file, key_file) = (dir.join("cert.der"), dir.join("key.der"));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert_file), std::fs::read(&key_file)) {
        return Ok((c, k));
    }
    let made = rcgen::generate_simple_self_signed(vec!["banana-photos.local".to_string()]).map_err(|e| e.to_string())?;
    let (cert, key) = (made.cert.der().to_vec(), made.signing_key.serialize_der());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::store::write_atomic(&key_file, &key);
    crate::store::write_atomic(&cert_file, &cert);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600));
    }
    Ok((cert, key))
}

/// Files deleted from the library (lower-case name, size), from a phone or on the computer: a
/// phone's backup mustn't send them back. Kept in phone/deleted.json.
fn deleted() -> &'static Mutex<HashSet<(String, u64)>> {
    static DELETED: std::sync::OnceLock<Mutex<HashSet<(String, u64)>>> = std::sync::OnceLock::new();
    DELETED.get_or_init(|| {
        let list: Vec<(String, u64)> = dir().map(|d| crate::store::load_json(&d.join("deleted.json"))).unwrap_or_default();
        Mutex::new(list.into_iter().collect())
    })
}

/// Remembers files moved to the Trash (path, size), so backups from phones skip them.
pub fn note_deleted(files: &[(PathBuf, u64)]) {
    let mut set = deleted().lock().unwrap();
    for (path, size) in files {
        if let Some(name) = path.file_name() {
            set.insert((name.to_string_lossy().to_lowercase(), *size));
        }
    }
    let list: Vec<&(String, u64)> = set.iter().collect();
    if let (Some(d), Ok(json)) = (dir(), serde_json::to_vec(&list)) {
        crate::store::write_atomic(&d.join("deleted.json"), &json);
    }
}

pub fn was_deleted(name: &str, size: u64) -> bool {
    deleted().lock().unwrap().contains(&(name.to_lowercase(), size))
}

fn load_phones() -> Vec<Phone> {
    dir().map(|d| crate::store::load_json(&d.join("phones.json"))).unwrap_or_default()
}

fn save_phones(phones: &[Phone]) {
    if let (Some(d), Ok(json)) = (dir(), serde_json::to_vec_pretty(phones)) {
        crate::store::write_atomic(&d.join("phones.json"), &json);
    }
}

/// Announces `<name>._bananaphotos._tcp.local` so a paired phone finds this computer again
/// when its address changes.
fn announce(shared: &Shared) -> Result<mdns_sd::ServiceDaemon, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    let host: String = shared.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let ips: Vec<IpAddr> = lan_addresses().into_iter().map(IpAddr::V4).collect();
    let mut props = HashMap::new();
    props.insert("v".to_string(), "1".to_string());
    props.insert("m".to_string(), shared.media_port.to_string());
    props.insert("f".to_string(), shared.fingerprint.clone());
    let info = mdns_sd::ServiceInfo::new(SERVICE, &shared.name, &format!("{host}.local."), &ips[..], shared.port, props).map_err(|e| e.to_string())?;
    daemon.register(info).map_err(|e| e.to_string())?;
    Ok(daemon)
}

/// This computer's addresses on private networks, home Wi-Fi (192.168.x) first.
pub fn lan_addresses() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) if a.is_private() && !i.is_loopback() => Some(a),
            _ => None,
        })
        .collect();
    out.sort_by_key(|a| (a.octets()[0] != 192, *a));
    out.dedup();
    out
}

/// "Aqib's MacBook Pro", or the host name.
pub fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "Banana Photos".to_string())
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("the OS random source works");
    hex(&b)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// Trusts exactly one certificate, by its SHA-256, as the phone app does after the QR scan (and
/// another computer, syncing with this one).
#[derive(Debug)]
struct Pinned(String, Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if hex(&Sha256::digest(cert.as_ref())) == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("not the paired computer".into()))
        }
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.1.signature_verification_algorithms.supported_schemes()
    }
}

/// An HTTPS client for the computer with this certificate fingerprint.
pub fn pinned_agent(fingerprint: &str) -> ureq::Agent {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned(fingerprint.to_string(), provider)))
        .with_no_client_auth();
    ureq::AgentBuilder::new().tls_config(Arc::new(config)).timeout_connect(Duration::from_secs(5)).build()
}

/// Takes whatever certificate it's shown and notes its SHA-256: for adding a computer by its
/// address when it can't be found on the network (as with mDNS, the six digits pair them).
#[derive(Debug)]
struct Noted(Mutex<Option<String>>, Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for Noted {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(hex(&Sha256::digest(cert.as_ref())));
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.1.signature_verification_algorithms.supported_schemes()
    }
}

/// The certificate fingerprint of the Banana Photos at `host:port`.
pub fn fingerprint_at(host: &str, port: u16) -> Result<String, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let noted = Arc::new(Noted(Mutex::new(None), provider.clone()));
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(noted.clone())
        .with_no_client_auth();
    let agent = ureq::AgentBuilder::new().tls_config(Arc::new(config)).timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(10)).build();
    match agent.get(&format!("https://{host}:{port}/api/info")).call() {
        Ok(_) | Err(ureq::Error::Status(..)) => {}
        Err(e) => return Err(e.to_string()),
    }
    let fp = noted.0.lock().unwrap().clone();
    fp.ok_or_else(|| "no certificate".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(fingerprint: &str) -> ureq::Agent {
        pinned_agent(fingerprint)
    }

    fn status(r: Result<ureq::Response, ureq::Error>) -> u16 {
        match r {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(s, _)) => s,
            Err(e) => panic!("{e}"),
        }
    }

    fn jpeg(w: u32, h: u32, shade: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([shade, 100, 50]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode_image(&img).unwrap();
        out
    }

    /// The whole API over real TLS: pairing, browsing, media, video links, upload, unpairing.
    #[test]
    fn phone_api_end_to_end() {
        let base = std::env::temp_dir().join(format!("banana-remote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (data, root) = (base.join("data"), base.join("lib"));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("BANANA_DATA", &data);
        std::fs::write(root.join("beach.jpg"), jpeg(400, 300, 10)).unwrap();
        std::fs::write(root.join("clip.mp4"), b"0123456789").unwrap();
        let mut photos: Vec<crate::library::Photo> = ["beach.jpg", "clip.mp4"].iter().filter_map(|n| crate::library::read_photo(&root.join(n))).collect();
        // Taken in Lahore.
        photos.iter_mut().find(|p| p.path.ends_with("beach.jpg")).unwrap().gps = Some((31.52, 74.36));

        let r = Remote::start_on(egui::Context::default(), 0, 0).unwrap();
        let mut store = crate::store::Store::default();
        store.create_album("Holiday", &[root.join("clip.mp4"), root.join("beach.jpg")]);
        let face = |path: &str| crate::people::FaceRef { path: root.join(path), index: 0 };
        let person = |name: Option<&str>, hidden: bool| crate::people::Person {
            name: name.map(String::from),
            hidden,
            faces: vec![face("beach.jpg")],
            photos: [root.join("beach.jpg")].into(),
            cover: face("beach.jpg"),
        };
        let people = [person(Some("Sara"), false), person(None, true)];
        r.set_library(snapshot(Some(root.clone()), &photos, &store, &people, &crate::people::Index::default()));
        let url = |p: &str| format!("https://127.0.0.1:{}{p}", r.shared.port);
        let a = agent(&r.shared.fingerprint);
        // Added by its address, a computer's fingerprint is read from it.
        assert_eq!(fingerprint_at("127.0.0.1", r.shared.port).as_deref(), Ok(r.shared.fingerprint.as_str()));

        // A different certificate is refused by the phone; no token, no library.
        assert!(agent(&"0".repeat(64)).get(&url("/api/info")).call().is_err());
        assert_eq!(status(a.get(&url("/api/info")).call()), 401);

        // Pairing: the QR code carries the code and the fingerprint; a wrong code fails.
        let code = r.new_pairing_code();
        assert!(r.qr_payload(&code).contains(&format!("&c={code}&f={}", r.shared.fingerprint)));
        assert_eq!(status(a.post(&url("/api/pair")).send_json(json_pair("nope"))), 403);
        let resp: serde_json::Value = a.post(&url("/api/pair")).send_json(json_pair(&code)).unwrap().into_json().unwrap();
        let token = resp["token"].as_str().unwrap().to_string();
        assert!(matches!(r.events.try_recv(), Ok(Event::Paired(n)) if n == "Test phone"));
        // The code works once.
        assert_eq!(status(a.post(&url("/api/pair")).send_json(json_pair(&code))), 403);
        let auth = format!("Bearer {token}");
        let get = |p: &str| a.get(&url(p)).set("Authorization", &auth);

        let list: serde_json::Value = get("/api/photos").call().unwrap().into_json().unwrap();
        assert_eq!(list["total"], 2);
        let beach = list["items"].as_array().unwrap().iter().find(|i| i["name"] == "beach.jpg").unwrap()["id"].as_str().unwrap().to_string();
        let video = list["items"].as_array().unwrap().iter().find(|i| i["video"] == true).unwrap()["id"].as_str().unwrap().to_string();

        let mut thumb = Vec::new();
        get(&format!("/api/thumb/{beach}")).call().unwrap().into_reader().read_to_end(&mut thumb).unwrap();
        assert_eq!(&thumb[..2], &[0xFF, 0xD8]);
        let mut small = Vec::new();
        get(&format!("/api/photo/{beach}?max=100")).call().unwrap().into_reader().read_to_end(&mut small).unwrap();
        assert_eq!(image::load_from_memory(&small).unwrap().width(), 100);

        let part = get(&format!("/api/video/{video}")).set("Range", "bytes=2-4").call().unwrap();
        assert_eq!((part.status(), part.header("Content-Range").map(String::from)), (206, Some("bytes 2-4/10".into())));
        assert_eq!(part.into_string().unwrap(), "234");

        // The video player's link: plain HTTP, no token, signed for this file only.
        let link: serde_json::Value = get(&format!("/api/media-url/{video}")).call().unwrap().into_json().unwrap();
        let link = link["url"].as_str().unwrap().to_string();
        assert!(link.starts_with(&format!("http://127.0.0.1:{}/m/{video}?", r.shared.media_port)));
        assert_eq!(ureq::get(&link).call().unwrap().into_string().unwrap(), "0123456789");
        assert_eq!(status(ureq::get(&link.replace(&video, &beach)).call()), 403);
        assert_eq!(status(ureq::get(&url("/api/info").replace("https", "http").replace(&r.shared.port.to_string(), &r.shared.media_port.to_string())).call()), 404);

        // Albums and people, in timeline order; unknown ones are 404s; hidden people aren't listed.
        let albums: serde_json::Value = get("/api/albums").call().unwrap().into_json().unwrap();
        assert_eq!((albums["albums"][0]["name"].as_str(), albums["albums"][0]["count"].as_u64()), (Some("Holiday"), Some(2)));
        let holiday: serde_json::Value = get("/api/album/0").call().unwrap().into_json().unwrap();
        let first_two: Vec<_> = list["items"].as_array().unwrap().iter().map(|i| i["id"].clone()).collect();
        assert_eq!(holiday["ids"].as_array().unwrap(), &first_two);
        assert_eq!(status(get("/api/album/7").call()), 404);
        let people: serde_json::Value = get("/api/people").call().unwrap().into_json().unwrap();
        assert_eq!(people["people"].as_array().map(Vec::len), Some(1));
        assert_eq!((people["people"][0]["name"].as_str(), people["people"][0]["count"].as_u64()), (Some("Sara"), Some(1)));
        let sara: serde_json::Value = get(&format!("/api/person/{}", people["people"][0]["id"].as_str().unwrap())).call().unwrap().into_json().unwrap();
        assert_eq!(sara["ids"][0].as_str(), Some(beach.as_str()));
        // No face picture made (no analysis ran).
        assert_eq!(status(get(&format!("/api/face/{}", people["people"][0]["id"].as_str().unwrap())).call()), 404);
        assert_eq!(status(get("/api/person/0123456789abcdef").call()), 404);
        let places: serde_json::Value = get("/api/places").call().unwrap().into_json().unwrap();
        assert_eq!((places["places"][0]["name"].as_str(), places["places"][0]["count"].as_u64()), (Some("Lahore, Pakistan"), Some(1)));
        let lahore: serde_json::Value = get(&format!("/api/place/{}", places["places"][0]["id"].as_str().unwrap())).call().unwrap().into_json().unwrap();
        assert_eq!(lahore["ids"][0].as_str(), Some(beach.as_str()));

        // Converting for phones: a file that isn't really a video ends as failed, and a photo is refused.
        let mut compat = serde_json::Value::Null;
        for _ in 0..100 {
            compat = get(&format!("/api/compat/{video}")).call().unwrap().into_json().unwrap();
            if compat["status"] != "converting" {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(compat["status"], "failed", "{compat}");
        assert_eq!(status(get(&format!("/api/compat/{beach}")).call()), 400);

        let found: serde_json::Value = get("/api/search?q=beach").call().unwrap().into_json().unwrap();
        assert_eq!((found["kind"].as_str(), found["ids"][0].as_str()), (Some("name"), Some(beach.as_str())));

        // Backup from the phone: filed under Imported/<phone>, once.
        let upload = |name: &str| a.post(&url(&format!("/api/upload?name={name}&taken=1700000000"))).set("Authorization", &auth);
        let photo = jpeg(64, 48, 200);
        let up: serde_json::Value = upload("IMG_0001.jpg").send_bytes(&photo).unwrap().into_json().unwrap();
        assert_eq!(up["status"], "imported");
        assert!(matches!(r.events.try_recv(), Ok(Event::Uploaded)));
        // The phones see the upload at once, before the app (maybe minimised) re-scans.
        let after: serde_json::Value = get("/api/photos").call().unwrap().into_json().unwrap();
        assert_eq!(after["total"], 3);
        assert!(after["library_version"].as_u64() > list["library_version"].as_u64());
        a.post(&url(&format!("/api/favorite/{beach}?on=1"))).set("Authorization", &auth).call().unwrap();
        let fav: serde_json::Value = get("/api/photos").call().unwrap().into_json().unwrap();
        assert!(fav["items"].as_array().unwrap().iter().any(|i| i["id"] == beach.as_str() && i["favorite"] == true));
        let again: serde_json::Value = upload("IMG_0001.jpg").send_bytes(&photo).unwrap().into_json().unwrap();
        assert_eq!(again["status"], "skipped");
        let has: serde_json::Value = get(&format!("/api/has?name=IMG_0001.jpg&size={}&taken=1700000000", photo.len())).call().unwrap().into_json().unwrap();
        assert_eq!(has["have"], true);
        assert_eq!(status(upload("notes.txt").send_bytes(b"hi")), 415);
        // A photo the phone saved from the library isn't sent back as a copy.
        let beach_file = std::fs::read(root.join("beach.jpg")).unwrap();
        let has: serde_json::Value = get(&format!("/api/has?name=BEACH.jpg&size={}&taken=1600000000", beach_file.len())).call().unwrap().into_json().unwrap();
        assert_eq!(has["have"], true);
        let back: serde_json::Value = upload("beach.jpg").send_bytes(&beach_file).unwrap().into_json().unwrap();
        assert_eq!(back["status"], "skipped");
        let saved = crate::transfer::media_files(&root.join("Imported").join("Test phone"));
        assert_eq!(saved.len(), 1);

        // Several at once: favourites, and (on Linux, where the test can use a throwaway Trash)
        // moving to the Trash.
        let both = serde_json::json!({ "ids": [beach, video], "on": true });
        let done: serde_json::Value = a.post(&url("/api/favorites")).set("Authorization", &auth).send_json(both).unwrap().into_json().unwrap();
        assert_eq!(done["changed"], 2);
        let favs: serde_json::Value = get("/api/photos").call().unwrap().into_json().unwrap();
        assert_eq!(favs["items"].as_array().unwrap().iter().filter(|i| i["favorite"] == true).count(), 2);
        #[cfg(target_os = "linux")]
        {
            while r.events.try_recv().is_ok() {}
            let gone: serde_json::Value = a.post(&url("/api/trash")).set("Authorization", &auth).send_json(serde_json::json!({ "ids": [video, "ffff"] })).unwrap().into_json().unwrap();
            assert_eq!((gone["trashed"].as_u64(), gone["skipped"].as_u64()), (Some(1), Some(0)));
            assert!(!root.join("clip.mp4").exists());
            assert!(matches!(r.events.try_recv(), Ok(Event::Trashed(t)) if t[0].0 == root.join("clip.mp4")));
            let left: serde_json::Value = get("/api/photos").call().unwrap().into_json().unwrap();
            assert!(left["items"].as_array().unwrap().iter().all(|i| i["id"] != video.as_str()));
            // The phone still has it: its next backup mustn't send it back.
            let has: serde_json::Value = get("/api/has?name=clip.mp4&size=10&taken=1700000000").call().unwrap().into_json().unwrap();
            assert_eq!(has["have"], true);
        }

        // Syncing is for computers only.
        assert_eq!(status(get("/api/sync/state").call()), 403);

        // Removing the phone on the computer cuts it off at once.
        let key = r.phones()[0].key().to_string();
        r.forget_phone(&key);
        assert_eq!(status(get("/api/info").call()), 401);

        drop(r);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A phone browsing for `_bananaphotos._tcp` finds this computer, with its port and
    /// fingerprint. Needs a real network interface, so it's run by hand:
    ///   cargo test --release mdns_announcement -- --ignored
    #[test]
    #[ignore]
    fn mdns_announcement() {
        std::env::set_var("BANANA_DATA", std::env::temp_dir().join(format!("banana-mdns-{}", std::process::id())));
        let r = Remote::start_on(egui::Context::default(), 0, 0).unwrap();
        let browser = mdns_sd::ServiceDaemon::new().unwrap();
        let rx = browser.browse(SERVICE).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(Duration::from_millis(500)) {
                if info.get_port() == r.shared.port {
                    found = Some(info.get_property_val_str("f").map(String::from));
                }
            }
        }
        let _ = browser.shutdown();
        assert_eq!(found, Some(Some(r.shared.fingerprint.clone())));
    }

    /// Turning it off and straight back on (or a second start) gets the same ports again.
    #[test]
    fn restarts_on_the_same_ports() {
        std::env::set_var("BANANA_DATA", std::env::temp_dir().join(format!("banana-restart-{}", std::process::id())));
        let first = Remote::start_on(egui::Context::default(), 0, 0).unwrap();
        let (port, media) = (first.shared.port, first.shared.media_port);
        drop(first);
        let again = Remote::start_on(egui::Context::default(), port, media).unwrap();
        assert_eq!((again.shared.port, again.shared.media_port), (port, media));
    }

    fn json_pair(code: &str) -> serde_json::Value {
        serde_json::json!({ "code": code, "name": "Test phone" })
    }
}
