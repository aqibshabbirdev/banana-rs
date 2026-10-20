//! Import from the Mac's Photos app through PhotoKit. macOS asks once for permission. Items
//! kept only in iCloud are downloaded; edited photos come out as edited.

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use block2::RcBlock;
use chrono::{DateTime, Local, NaiveDateTime};
use objc2::rc::autoreleasepool;
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_photos::{
    PHAccessLevel, PHAsset, PHAssetMediaType, PHAssetResource, PHAssetResourceManager, PHAssetResourceRequestOptions,
    PHAssetResourceType, PHAuthorizationStatus, PHPhotoLibrary,
};

use super::{save_file, Cancel, Filter, Msg, Reporter};

/// Asks for (or checks) read access to the Photos library. Blocks; call off the UI thread.
fn has_access() -> bool {
    let level = PHAccessLevel::ReadWrite;
    let status = unsafe { PHPhotoLibrary::authorizationStatusForAccessLevel(level) };
    if status == PHAuthorizationStatus::Authorized || status == PHAuthorizationStatus::Limited {
        return true;
    }
    if status != PHAuthorizationStatus::NotDetermined {
        return false;
    }
    let (tx, rx) = channel();
    let block = RcBlock::new(move |s: PHAuthorizationStatus| {
        let _ = tx.send(s);
    });
    unsafe { PHPhotoLibrary::requestAuthorizationForAccessLevel_handler(level, &block) };
    matches!(rx.recv_timeout(Duration::from_secs(600)), Ok(s) if s == PHAuthorizationStatus::Authorized || s == PHAuthorizationStatus::Limited)
}

/// The resource to export: the edited render if there is one, else the original.
fn best_resource(asset: &PHAsset, is_video: bool) -> Option<objc2::rc::Retained<PHAssetResource>> {
    let resources = unsafe { PHAssetResource::assetResourcesForAsset(asset) };
    let (full, original) = if is_video {
        (PHAssetResourceType::FullSizeVideo, PHAssetResourceType::Video)
    } else {
        (PHAssetResourceType::FullSizePhoto, PHAssetResourceType::Photo)
    };
    let all: Vec<_> = resources.iter().collect();
    all.iter()
        .find(|r| unsafe { r.r#type() } == full)
        .or_else(|| all.iter().find(|r| unsafe { r.r#type() } == original))
        .cloned()
}

struct Pick {
    resource: objc2::rc::Retained<PHAssetResource>,
    name: String,
    taken: NaiveDateTime,
}

pub fn spawn_import(dest: PathBuf, filter: Filter, favorites_only: bool, cancel: Cancel, r: Reporter) {
    std::thread::spawn(move || {
        r.status("Asking macOS for access to Photos…");
        if !has_access() {
            r.send(Msg::Done(
                "Banana Photos isn't allowed to read your Photos library. Allow it in System Settings → Privacy & Security → Photos.".into(),
            ));
            return;
        }

        let picks: Vec<Pick> = autoreleasepool(|_| {
            let assets = unsafe { PHAsset::fetchAssetsWithOptions(None) };
            let n = unsafe { assets.count() };
            r.status(format!("Checking {n} items in Photos…"));
            (0..n)
                .filter_map(|i| {
                    let asset = unsafe { assets.objectAtIndex(i) };
                    let kind = unsafe { asset.mediaType() };
                    let is_video = kind == PHAssetMediaType::Video;
                    if kind != PHAssetMediaType::Image && !is_video {
                        return None;
                    }
                    if favorites_only && !unsafe { asset.isFavorite() } {
                        return None;
                    }
                    let taken = unsafe { asset.creationDate() }
                        .and_then(|d| DateTime::from_timestamp(d.timeIntervalSince1970() as i64, 0))
                        .map(|t| t.with_timezone(&Local).naive_local())
                        .unwrap_or_else(super::now);
                    let resource = best_resource(&asset, is_video)?;
                    let name = unsafe { resource.originalFilename() }.to_string();
                    filter.matches(&name, taken, is_video).then_some(Pick { resource, name, taken })
                })
                .collect()
        });

        let total = picks.len();
        r.status(format!("Importing {total} items from Photos…"));
        let manager = unsafe { PHAssetResourceManager::defaultManager() };
        let (mut copied, mut skipped, mut failed) = (0, 0, 0);
        for (i, pick) in picks.iter().enumerate() {
            if cancel.cancelled() {
                break;
            }
            let result: Result<bool, String> = autoreleasepool(|_| {
                std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                let tmp = dest.join(format!(".banana-photos-{i}-{}", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                let url = NSURL::fileURLWithPath(&NSString::from_str(&tmp.to_string_lossy()));
                let options = unsafe { PHAssetResourceRequestOptions::new() };
                unsafe { options.setNetworkAccessAllowed(true) };
                let (tx, rx) = channel();
                let done = RcBlock::new(move |err: *mut NSError| {
                    let msg = if err.is_null() { None } else { Some(unsafe { (*err).localizedDescription() }.to_string()) };
                    let _ = tx.send(msg);
                });
                unsafe { manager.writeDataForAssetResource_toFile_options_completionHandler(&pick.resource, &url, Some(&options), &done) };
                match rx.recv_timeout(Duration::from_secs(600)) {
                    Ok(None) => save_file(&dest, &pick.name, pick.taken, &tmp).map_err(|e| e.to_string()),
                    Ok(Some(e)) => {
                        let _ = std::fs::remove_file(&tmp);
                        Err(e)
                    }
                    Err(_) => Err("timed out downloading from iCloud".into()),
                }
            });
            match result {
                Ok(true) => copied += 1,
                Ok(false) => skipped += 1,
                Err(e) => {
                    failed += 1;
                    r.send(Msg::Failed(format!("{}: {e}", pick.name)));
                }
            }
            r.send(Msg::Progress { done: i + 1, total });
        }
        r.send(Msg::Done(super::summary_for("Imported", copied, skipped, failed, cancel.cancelled())));
    });
}
