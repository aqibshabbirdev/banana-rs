//! Favourites and albums, saved as JSON in the user's data folder. Photos are referenced by
//! absolute path, so albums survive restarts and re-scans.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Album {
    pub name: String,
    pub photos: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub favorites: HashSet<PathBuf>,
    #[serde(default)]
    pub albums: Vec<Album>,
    /// Hidden from Photos, shown only under Archive.
    #[serde(default)]
    pub archived: HashSet<PathBuf>,
    /// Names given to people, per face ("<path>#<face index>"). Every face of a person carries
    /// the name, so it survives re-clustering; two groups with the same name show as one person.
    #[serde(default)]
    pub face_names: HashMap<String, String>,
    /// Faces of people the user hid from People & pets.
    #[serde(default)]
    pub hidden_faces: HashSet<String>,
    /// Folders of imports kept apart: their photos stay out of the Photos timeline and show
    /// in their own album.
    #[serde(default)]
    pub separate: HashSet<PathBuf>,
}

impl Store {
    pub fn load() -> Self {
        file().map(|f| load_json(&f)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(file) = file() else { return };
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            write_atomic(&file, &json);
        }
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.contains(path)
    }

    /// Sets every path to favourite, unless all already are, in which case it clears them.
    pub fn toggle_favorites(&mut self, paths: &[PathBuf]) {
        if paths.iter().all(|p| self.favorites.contains(p)) {
            for p in paths {
                self.favorites.remove(p);
            }
        } else {
            self.favorites.extend(paths.iter().cloned());
        }
        self.save();
    }

    pub fn is_archived(&self, path: &Path) -> bool {
        self.archived.contains(path)
    }

    /// Same all-or-nothing toggle as favourites.
    pub fn toggle_archive(&mut self, paths: &[PathBuf]) {
        if paths.iter().all(|p| self.archived.contains(p)) {
            for p in paths {
                self.archived.remove(p);
            }
        } else {
            self.archived.extend(paths.iter().cloned());
        }
        self.save();
    }

    /// Names (or, with an empty name, un-names) every face of one person.
    pub fn name_faces(&mut self, keys: &[String], name: &str) {
        let name = name.trim();
        for k in keys {
            if name.is_empty() {
                self.face_names.remove(k);
            } else {
                self.face_names.insert(k.clone(), name.to_string());
            }
        }
        self.save();
    }

    pub fn hide_faces(&mut self, keys: &[String], hidden: bool) {
        for k in keys {
            if hidden {
                self.hidden_faces.insert(k.clone());
            } else {
                self.hidden_faces.remove(k);
            }
        }
        self.save();
    }

    /// Drops every reference to a photo that left the library (moved into Locked Folder), so
    /// its name doesn't linger in albums or favourites in plain text.
    pub fn forget(&mut self, path: &Path) {
        self.favorites.remove(path);
        self.archived.remove(path);
        for a in &mut self.albums {
            a.photos.retain(|p| p != path);
        }
        let prefix = format!("{}#", path.display());
        self.face_names.retain(|k, _| !k.starts_with(&prefix));
        self.hidden_faces.retain(|k| !k.starts_with(&prefix));
        self.save();
    }

    pub fn is_separate(&self, path: &Path) -> bool {
        self.separate.iter().any(|f| path.starts_with(f))
    }

    pub fn set_separate(&mut self, folder: &Path, separate: bool) {
        if separate {
            self.separate.insert(folder.to_path_buf());
        } else {
            self.separate.retain(|f| f != folder);
        }
        self.save();
    }

    pub fn create_album(&mut self, name: &str, paths: &[PathBuf]) -> usize {
        self.albums.push(Album { name: self.unique_name(name), photos: Vec::new() });
        let idx = self.albums.len() - 1;
        self.add_to_album(idx, paths);
        idx
    }

    pub fn add_to_album(&mut self, idx: usize, paths: &[PathBuf]) {
        let album = &mut self.albums[idx];
        let existing: HashSet<PathBuf> = album.photos.iter().cloned().collect();
        album.photos.extend(paths.iter().filter(|p| !existing.contains(*p)).cloned());
        self.save();
    }

    pub fn remove_from_album(&mut self, idx: usize, paths: &[PathBuf]) {
        let remove: HashSet<&PathBuf> = paths.iter().collect();
        self.albums[idx].photos.retain(|p| !remove.contains(p));
        self.save();
    }

    pub fn rename_album(&mut self, idx: usize, name: &str) {
        let name = name.trim();
        if !name.is_empty() && name != self.albums[idx].name {
            self.albums[idx].name = self.unique_name(name);
            self.save();
        }
    }

    pub fn delete_album(&mut self, idx: usize) {
        self.albums.remove(idx);
        self.save();
    }

    fn unique_name(&self, name: &str) -> String {
        let base = if name.trim().is_empty() { "Untitled album" } else { name.trim() };
        let taken = |n: &str| self.albums.iter().any(|a| a.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }
}

fn file() -> Option<PathBuf> {
    Some(data_dir()?.join("library.json"))
}

/// Where albums, favourites and the face index live (`BANANA_DATA` overrides it for tests).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BANANA_DATA") {
        return Some(PathBuf::from(dir));
    }
    Some(dirs::data_dir()?.join("banana-rs"))
}

/// Reads a JSON file, or the default when there is none. A file that exists but can't be read
/// is first renamed to "<name>.broken-<time>", so the next save can't overwrite what was in it.
pub fn load_json<T: Default + serde::de::DeserializeOwned>(file: &Path) -> T {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(_) => {
            set_aside(file);
            return T::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        set_aside(file);
        T::default()
    })
}

fn set_aside(file: &Path) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".broken-{secs}"));
    let _ = std::fs::rename(file, file.with_file_name(name));
}

/// Writes to a temp file, syncs it, then renames, so a crash or power cut mid-write can't
/// leave a cut-short file in place of the old one.
pub fn write_atomic(file: &Path, bytes: &[u8]) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let mut tmp_name = file.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = file.with_file_name(tmp_name);
    let ok = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}
