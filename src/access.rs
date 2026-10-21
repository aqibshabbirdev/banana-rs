//! Keeps access to folders and files the user picked, across launches.
//!
//! The Mac App Store build runs in the App Sandbox, where the app can only read a folder the
//! user chose in an Open panel, and only until it quits. For each pick a security-scoped
//! bookmark is saved in the data folder; at launch every bookmark is resolved and access
//! restarted, so the last library, import sources and export folders keep working. Outside
//! the sandbox (Developer ID build, Linux) this is harmless: bookmarks resolve and nothing changes.

use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("bookmarks"))
}

fn file_for(path: &Path) -> Option<PathBuf> {
    Some(dir()?.join(format!("{:016x}.bookmark", crate::thumbs::cache_key(path, 0, 0))))
}

/// Remembers access to a folder or file the user just picked.
pub fn remember(path: &Path) {
    #[cfg(target_os = "macos")]
    if let (Some(bytes), Some(file)) = (mac::bookmark(path), file_for(path)) {
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(file, bytes);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = file_for(path);
}

/// Restarts access to everything remembered. Call once at launch, before opening the last folder.
/// Returns the remembered places that were moved or renamed since, as (bookmark file, new path);
/// see `new_location`.
pub fn restore() -> Vec<(PathBuf, PathBuf)> {
    #[allow(unused_mut)]
    let mut moved = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let Some(entries) = dir().and_then(|d| std::fs::read_dir(d).ok()) else { return moved };
        for entry in entries.flatten() {
            let file = entry.path();
            let Ok(bytes) = std::fs::read(&file) else { continue };
            // A bookmark that doesn't resolve is kept: the folder may be on a drive that's
            // unplugged right now, and removing it would lose access for good once it's back.
            let Some((path, stale)) = mac::resolve(&bytes) else { continue };
            // Moved or renamed since: save a fresh bookmark under the new path.
            if stale {
                let _ = std::fs::remove_file(&file);
                remember(&path);
                moved.push((file, path));
            }
        }
    }
    moved
}

/// Where `old` is now, if `restore` found it was moved or renamed.
pub fn new_location(moved: &[(PathBuf, PathBuf)], old: &Path) -> Option<PathBuf> {
    let file = file_for(old)?;
    moved.iter().find(|(f, _)| *f == file).map(|(_, p)| p.clone())
}

/// Opens a file in its default app. NSWorkspace rather than `open`, which the sandbox may refuse to run.
#[cfg(target_os = "macos")]
pub fn open_file(path: &Path) {
    if let Some(p) = path.to_str() {
        mac::open(&objc2_foundation::NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(p)));
    }
}

/// Opens a web address in the default browser.
#[cfg(target_os = "macos")]
pub fn open_url(url: &str) {
    if let Some(u) = objc2_foundation::NSURL::URLWithString(&objc2_foundation::NSString::from_str(url)) {
        mac::open(&u);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};

    use objc2::runtime::Bool;
    use objc2_foundation::{NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL};

    pub fn open(url: &NSURL) {
        objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(url);
    }

    pub fn bookmark(path: &Path) -> Option<Vec<u8>> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
        let data = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            )
            .ok()?;
        Some(data.to_vec())
    }

    /// The bookmarked path, with access started (kept for the rest of the run), and whether
    /// the bookmark is stale.
    pub fn resolve(bytes: &[u8]) -> Option<(PathBuf, bool)> {
        let data = NSData::with_bytes(bytes);
        let mut stale = Bool::NO;
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithSecurityScope,
                None,
                &mut stale,
            )
        }
        .ok()?;
        unsafe { url.startAccessingSecurityScopedResource() };
        let path = PathBuf::from(url.path()?.to_string());
        Some((path, stale.as_bool()))
    }
}
