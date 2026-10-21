//! Phones and cameras plugged into this computer, for importing from them.
//!
//! On Linux the desktop (GNOME, Cinnamon, MATE, Xfce...) talks to Android phones over MTP and to
//! cameras and iPhones over PTP through gvfs, which shows each one as a folder under
//! `$XDG_RUNTIME_DIR/gvfs/`. The app reads phones through those folders rather than opening the
//! USB device itself, which gvfs already holds. A phone that is plugged in but not mounted yet
//! (gvfs lists it as a volume) is mounted with `gio mount`, as the file manager does when its
//! icon is clicked. Camera cards and phones in USB-drive mode under /media or /run/media count
//! too when they have a DCIM folder. Other systems: nothing is listed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

#[derive(Clone, PartialEq, Debug)]
pub struct Device {
    /// "Google Pixel 7", "Canon EOS 250D", or a card's volume name.
    pub name: String,
    /// The folder gvfs (or the system) shows the device as; None while it isn't mounted.
    pub root: Option<PathBuf>,
    /// Folders photos are imported from: DCIM on each storage (internal, SD card).
    pub camera_dirs: Vec<PathBuf>,
    /// Pictures on each storage: screenshots and pictures saved by other apps.
    pub picture_dirs: Vec<PathBuf>,
    pub kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Phone,
    Camera,
    Card,
}

impl Device {
    /// Mounted but nothing readable: an Android phone shows no storage until it is unlocked
    /// and set to "File transfer".
    pub fn locked(&self) -> bool {
        self.root.is_some() && self.camera_dirs.is_empty() && self.picture_dirs.is_empty()
    }
}

/// Watches for phones and cameras: sends the full list whenever it changes, and mounts phones
/// that appear unmounted (once each).
pub fn watch(ctx: eframe::egui::Context) -> Receiver<Vec<Device>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut last: Option<Vec<Device>> = None;
        let mut tried_mount: Vec<String> = Vec::new();
        loop {
            let (devices, unmounted) = scan();
            for uri in unmounted {
                if !tried_mount.contains(&uri) {
                    mount(&uri);
                    tried_mount.push(uri);
                }
            }
            if last.as_ref() != Some(&devices) {
                if tx.send(devices.clone()).is_err() {
                    return;
                }
                ctx.request_repaint();
                last = Some(devices);
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
    rx
}

/// Devices found now, and the gvfs activation URIs of phones and cameras not mounted yet.
fn scan() -> (Vec<Device>, Vec<String>) {
    if !cfg!(target_os = "linux") {
        return (Vec::new(), Vec::new());
    }
    let mut devices = Vec::new();
    if let Some(gvfs) = gvfs_dir() {
        for e in std::fs::read_dir(gvfs).into_iter().flatten().flatten() {
            let dir_name = e.file_name().to_string_lossy().to_string();
            let kind = if dir_name.starts_with("mtp:") {
                Kind::Phone
            } else if dir_name.starts_with("gphoto2:") {
                Kind::Camera
            } else {
                continue;
            };
            let root = e.path();
            let name = display_name(&root).unwrap_or_else(|| name_from_host(&dir_name));
            devices.push(with_photo_dirs(Device { name, root: Some(root), camera_dirs: Vec::new(), picture_dirs: Vec::new(), kind }));
        }
    }
    devices.extend(cards());

    // Plugged in, not mounted: listed by gvfs's volume monitors. Shown right away (so the
    // phone appears as soon as it's connected) while it gets mounted.
    let mut unmounted = Vec::new();
    for (name, uri) in gio_volumes() {
        let host = uri.split("://").nth(1).unwrap_or("").trim_end_matches('/');
        let mounted = devices.iter().any(|d| d.root.as_ref().is_some_and(|r| r.to_string_lossy().contains(host)));
        if !mounted {
            let kind = if uri.starts_with("mtp:") { Kind::Phone } else { Kind::Camera };
            devices.push(Device { name, root: None, camera_dirs: Vec::new(), picture_dirs: Vec::new(), kind });
            unmounted.push(uri);
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    (devices, unmounted)
}

fn gvfs_dir() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self").ok()?.uid();
            Some(PathBuf::from(format!("/run/user/{uid}")))
        }
        #[cfg(not(unix))]
        None
    })?;
    Some(runtime.join("gvfs"))
}

/// Fills in the DCIM and Pictures folders of each storage ("Internal shared storage", "SD card";
/// a camera's storage may itself be the DCIM parent).
fn with_photo_dirs(mut d: Device) -> Device {
    let Some(root) = d.root.clone() else { return d };
    let mut storages = vec![root.clone()];
    storages.extend(std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    for s in storages {
        for (sub, list) in [("DCIM", &mut d.camera_dirs), ("Pictures", &mut d.picture_dirs)] {
            let dir = s.join(sub);
            if dir.is_dir() && !list.contains(&dir) {
                list.push(dir);
            }
        }
    }
    d
}

/// Camera cards and phones in USB-drive mode: mounted volumes with a DCIM folder.
fn cards() -> Vec<Device> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut out = Vec::new();
    for base in [PathBuf::from("/media").join(&user), PathBuf::from("/run/media").join(&user)] {
        for e in std::fs::read_dir(base).into_iter().flatten().flatten() {
            let root = e.path();
            if root.join("DCIM").is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                out.push(with_photo_dirs(Device { name, root: Some(root), camera_dirs: Vec::new(), picture_dirs: Vec::new(), kind: Kind::Card }));
            }
        }
    }
    out
}

/// The name the file manager shows for a gvfs mount ("Pixel 7"), from `gio info`.
fn display_name(root: &Path) -> Option<String> {
    let out = std::process::Command::new("gio").args(["info", "-a", "standard::display-name"]).arg(root).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text.lines().find_map(|l| l.trim().strip_prefix("standard::display-name:"))?.trim().to_string();
    // Off gvfs (or with an old gvfs) gio just echoes the folder name, "mtp:host=...".
    let raw = name.contains("host=") || root.file_name().is_some_and(|f| f.to_string_lossy() == name);
    (!name.is_empty() && !raw).then_some(name)
}

/// "mtp:host=Google_Pixel_7_2A101FDH2004XG" → "Google Pixel 7": the host is vendor, model and
/// serial number joined by underscores; the serial is dropped.
fn name_from_host(dir_name: &str) -> String {
    let host = dir_name.split_once("host=").map(|(_, h)| h).unwrap_or(dir_name);
    let host = host.split(',').next().unwrap_or(host).replace("%20", " ").replace("%5B", "[").replace("%5D", "]");
    let mut parts: Vec<&str> = host.split(['_', ' ']).filter(|p| !p.is_empty()).collect();
    let serial = |p: &str| p.len() >= 6 && p.chars().any(|c| c.is_ascii_digit()) && p.chars().all(|c| c.is_ascii_alphanumeric());
    if parts.len() > 1 && parts.last().is_some_and(|p| serial(p)) {
        parts.pop();
    }
    // "SAMSUNG_SAMSUNG_Android" repeats the vendor.
    parts.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    if parts.is_empty() { "Phone".to_string() } else { parts.join(" ") }
}

/// Phones and cameras gvfs knows about, as (name, activation URI), from `gio mount -li`:
///   Volume(0): Pixel 7
///     ...
///     activation_root=mtp://Google_Pixel_7_2A101FDH2004XG/
fn gio_volumes() -> Vec<(String, String)> {
    let Ok(out) = std::process::Command::new("gio").args(["mount", "-li"]).output() else { return Vec::new() };
    parse_gio_volumes(&String::from_utf8_lossy(&out.stdout))
}

fn parse_gio_volumes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut volume: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Volume(") {
            volume = rest.split_once("): ").map(|(_, n)| n.trim().to_string());
        } else if t.starts_with("Drive(") || t.starts_with("Mount(") {
            volume = None;
        } else if let (Some(uri), Some(name)) = (t.strip_prefix("activation_root="), &volume) {
            if uri.starts_with("mtp://") || uri.starts_with("gphoto2://") {
                out.push((name.clone(), uri.to_string()));
            }
        }
    }
    out
}

fn mount(uri: &str) {
    let _ = std::process::Command::new("gio").args(["mount", uri]).output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_gvfs_hosts() {
        assert_eq!(name_from_host("mtp:host=Google_Pixel_7_2A101FDH2004XG"), "Google Pixel 7");
        assert_eq!(name_from_host("mtp:host=SAMSUNG_SAMSUNG_Android_R58M12ABCDE"), "SAMSUNG Android");
        assert_eq!(name_from_host("gphoto2:host=Canon_Inc._Canon_Digital_Camera"), "Canon Inc. Canon Digital Camera");
        assert_eq!(name_from_host("mtp:host=Xiaomi_Redmi_Note_12_5b2c3d"), "Xiaomi Redmi Note 12");
    }

    #[test]
    fn parses_gio_volume_list() {
        let text = "Drive(0): Samsung SSD\n  Type: GProxyDrive\n  Volume(0): Data\n    activation_root=file:///x\n\
                    Volume(0): Pixel 7\n  Type: GProxyVolume (GProxyVolumeMonitorMTP)\n  ids:\n   unix-device: '/dev/bus/usb/001/005'\n  \
                    activation_root=mtp://Google_Pixel_7_2A101FDH2004XG/\n  themed icons:  [phone]\n\
                    Mount(0): Home -> file:///home/a\n";
        assert_eq!(parse_gio_volumes(text), vec![("Pixel 7".to_string(), "mtp://Google_Pixel_7_2A101FDH2004XG/".to_string())]);
    }

    #[test]
    fn finds_dcim_and_pictures_on_each_storage() {
        let root = std::env::temp_dir().join(format!("banana-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["Internal shared storage/DCIM/Camera", "Internal shared storage/Pictures/Screenshots", "SD card/DCIM", "SD card/Music"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let d = with_photo_dirs(Device { name: "P".into(), root: Some(root.clone()), camera_dirs: Vec::new(), picture_dirs: Vec::new(), kind: Kind::Phone });
        let _ = std::fs::remove_dir_all(&root);
        let mut cams = d.camera_dirs.clone();
        cams.sort();
        assert_eq!(cams, vec![root.join("Internal shared storage/DCIM"), root.join("SD card/DCIM")]);
        assert_eq!(d.picture_dirs, vec![root.join("Internal shared storage/Pictures")]);
        assert!(!d.locked());
        let empty = Device { name: "P".into(), root: Some(root), camera_dirs: Vec::new(), picture_dirs: Vec::new(), kind: Kind::Phone };
        assert!(empty.locked());
    }
}
