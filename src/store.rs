//! Favourites and albums, saved as JSON in the user's data folder. Photos are referenced by
//! absolute path, so albums survive restarts and re-scans.

use std::collections::HashSet;
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
}

impl Store {
    pub fn load() -> Self {
        file()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes to a temp file then renames, so a crash mid-write can't wipe the albums.
    pub fn save(&self) {
        let Some(file) = file() else { return };
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let tmp = file.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
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
    if let Some(dir) = std::env::var_os("BANANA_DATA") {
        return Some(PathBuf::from(dir).join("library.json"));
    }
    Some(dirs::data_dir()?.join("banana-rs").join("library.json"))
}
