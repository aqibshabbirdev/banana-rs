//! Phones and cameras on macOS, through Image Capture (ImageCaptureCore), as Photos and the
//! Image Capture app do: iPhones and iPads, cameras, and Android phones whose USB mode is
//! "Transfer photos (PTP)". macOS doesn't speak MTP, Android's default "File transfer" mode.
//!
//! Image Capture calls its delegates on the main thread, so everything here lives on the UI
//! thread: the app asks for the device list a couple of times a second, and an import downloads
//! one file at a time, started from the UI loop when the previous one is done.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDateTime};
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSData, NSDate, NSDictionary, NSError, NSNumber, NSString, NSURL};
use objc2_image_capture_core::{
    ICCameraDevice, ICCameraDeviceDelegate, ICCameraDeviceDownloadDelegate, ICCameraFile, ICCameraItem, ICDevice, ICDeviceBrowser,
    ICDeviceBrowserDelegate, ICDeviceDelegate, ICDeviceLocationTypeMask, ICDeviceTypeMask, ICDownloadsDirectoryURL, ICOverwrite,
    ICSaveAsFilename,
};

use super::{Device, IcInfo, Kind};
use crate::transfer::{self, Cancel, Filter, Msg, Reporter};

struct Ivars {
    ctx: egui::Context,
    /// Result of the download in flight: Some(Ok) once the file is saved, Some(Err) on failure.
    downloaded: Rc<RefCell<Option<Result<(), String>>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BananaImageCaptureDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // Devices coming and going, sessions and catalogs: the app re-reads the device list on the
    // next frame, so these only wake it up.
    unsafe impl ICDeviceBrowserDelegate for Delegate {
        #[unsafe(method(deviceBrowser:didAddDevice:moreComing:))]
        fn did_add_device(&self, _browser: &ICDeviceBrowser, _device: &ICDevice, _more: bool) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(deviceBrowser:didRemoveDevice:moreGoing:))]
        fn did_remove_device(&self, _browser: &ICDeviceBrowser, _device: &ICDevice, _more: bool) {
            self.ivars().ctx.request_repaint();
        }
    }

    unsafe impl ICDeviceDelegate for Delegate {
        #[unsafe(method(device:didCloseSessionWithError:))]
        fn did_close_session(&self, _device: &ICDevice, _error: Option<&NSError>) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(didRemoveDevice:))]
        fn did_remove(&self, _device: &ICDevice) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(device:didOpenSessionWithError:))]
        fn did_open_session(&self, _device: &ICDevice, _error: Option<&NSError>) {
            self.ivars().ctx.request_repaint();
        }
    }

    unsafe impl ICCameraDeviceDelegate for Delegate {
        #[unsafe(method(cameraDevice:didAddItems:))]
        fn did_add_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(cameraDevice:didRemoveItems:))]
        fn did_remove_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(cameraDevice:didReceiveThumbnail:forItem:error:))]
        fn did_receive_thumbnail(&self, _camera: &ICCameraDevice, _thumbnail: Option<&CGImage>, _item: &ICCameraItem, _error: Option<&NSError>) {}

        #[unsafe(method(cameraDevice:didReceiveMetadata:forItem:error:))]
        fn did_receive_metadata(&self, _camera: &ICCameraDevice, _metadata: Option<&NSDictionary>, _item: &ICCameraItem, _error: Option<&NSError>) {}

        #[unsafe(method(cameraDevice:didRenameItems:))]
        fn did_rename_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}

        #[unsafe(method(cameraDeviceDidChangeCapability:))]
        fn did_change_capability(&self, _camera: &ICCameraDevice) {}

        #[unsafe(method(cameraDevice:didReceivePTPEvent:))]
        fn did_receive_ptp_event(&self, _camera: &ICCameraDevice, _event: &NSData) {}

        #[unsafe(method(deviceDidBecomeReadyWithCompleteContentCatalog:))]
        fn did_become_ready(&self, _device: &ICCameraDevice) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(cameraDeviceDidRemoveAccessRestriction:))]
        fn did_remove_access_restriction(&self, _device: &ICDevice) {
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(cameraDeviceDidEnableAccessRestriction:))]
        fn did_enable_access_restriction(&self, _device: &ICDevice) {
            self.ivars().ctx.request_repaint();
        }
    }

    unsafe impl ICCameraDeviceDownloadDelegate for Delegate {
        #[unsafe(method(didDownloadFile:error:options:contextInfo:))]
        fn did_download(&self, _file: &ICCameraFile, error: Option<&NSError>, _options: &NSDictionary<NSString, AnyObject>, _context: *mut c_void) {
            let result = match error {
                Some(e) => Err(e.localizedDescription().to_string()),
                None => Ok(()),
            };
            *self.ivars().downloaded.borrow_mut() = Some(result);
            self.ivars().ctx.request_repaint();
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, ctx: egui::Context) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { ctx, downloaded: Rc::new(RefCell::new(None)) });
        unsafe { msg_send![super(this), init] }
    }
}

/// One photo or video on a device.
struct Item {
    file: Retained<ICCameraFile>,
    name: String,
    taken: NaiveDateTime,
    size: u64,
}

/// An import running on the UI thread: one download in flight at a time.
struct Import {
    camera: Retained<ICCameraDevice>,
    items: Vec<Item>,
    next: usize,
    dest: PathBuf,
    /// Downloads land here (hidden, so the library scan skips it) before being filed by date.
    tmp: PathBuf,
    cancel: Cancel,
    r: Reporter,
    /// (index into items, temporary file name) of the download in flight.
    in_flight: Option<(usize, String)>,
    copied: usize,
    skipped: usize,
    failed: usize,
}

pub struct ImageCapture {
    browser: Retained<ICDeviceBrowser>,
    delegate: Retained<Delegate>,
    /// Devices whose session was requested, by UUID; a session lists the photos.
    opened: HashSet<String>,
    import: Option<Import>,
}

impl ImageCapture {
    /// Starts looking for phones and cameras connected over USB.
    pub fn new(mtm: MainThreadMarker, ctx: egui::Context) -> Self {
        let delegate = Delegate::new(mtm, ctx);
        let browser = unsafe { ICDeviceBrowser::new() };
        unsafe {
            browser.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            browser.setBrowsedDeviceTypeMask(ICDeviceTypeMask(ICDeviceTypeMask::Camera.0 | ICDeviceLocationTypeMask::Local.0));
            browser.start();
        }
        Self { browser, delegate, opened: HashSet::new(), import: None }
    }

    fn cameras(&self) -> Vec<Retained<ICCameraDevice>> {
        let Some(list) = (unsafe { self.browser.devices() }) else { return Vec::new() };
        list.iter().filter_map(|d| d.downcast::<ICCameraDevice>().ok()).collect()
    }

    /// The phones and cameras connected now. Opens a session on each new one, which is what
    /// makes it list its photos (and an iPhone ask "Trust this computer?").
    pub fn devices(&mut self) -> Vec<Device> {
        let mut out = Vec::new();
        for cam in self.cameras() {
            let id = unsafe { cam.UUIDString() }.map(|s| s.to_string()).unwrap_or_default();
            if !unsafe { cam.hasOpenSession() } && self.opened.insert(id.clone()) {
                unsafe {
                    cam.setDelegate(Some(ProtocolObject::from_ref(&*self.delegate)));
                    cam.requestOpenSession();
                }
            }
            let name = unsafe { cam.name() }.map(|s| s.to_string()).unwrap_or_else(|| "Camera".into());
            let product = unsafe { cam.productKind() }.map(|s| s.to_string()).unwrap_or_default();
            let phone = ["iPhone", "iPad", "iPod", "Phone", "Android"].iter().any(|k| product.contains(k) || name.contains(k));
            let locked = unsafe { cam.isLocked() || cam.isAccessRestrictedAppleDevice() };
            let ready = unsafe { cam.hasOpenSession() && cam.contentCatalogPercentCompleted() >= 100 };
            let items = if ready { files(&cam) } else { Vec::new() };
            let ic = IcInfo { id, locked, ready, count: items.len(), bytes: items.iter().map(|i| i.size).sum() };
            out.push(Device {
                name,
                root: None,
                camera_dirs: Vec::new(),
                picture_dirs: Vec::new(),
                kind: if phone { Kind::Phone } else { Kind::Camera },
                ic: Some(ic),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Imports the photos on device `id` that pass `filter` into `dest`, in year/month folders,
    /// skipping ones imported before. Progress goes to `r`; call `poll` every frame.
    pub fn start_import(&mut self, id: &str, dest: PathBuf, filter: Filter, cancel: Cancel, r: Reporter) {
        let camera = self.cameras().into_iter().find(|c| unsafe { c.UUIDString() }.is_some_and(|s| s.to_string() == id));
        let Some(camera) = camera else {
            r.send(Msg::Done("The device was disconnected.".into()));
            return;
        };
        let items: Vec<Item> = files(&camera)
            .into_iter()
            .filter(|i| filter.matches(&i.name, i.taken, !crate::library::is_image(std::path::Path::new(&i.name))))
            .collect();
        r.send(Msg::Progress { done: 0, total: items.len() });
        let tmp = dest.join(".banana-download");
        self.import = Some(Import { camera, items, next: 0, dest, tmp, cancel, r, in_flight: None, copied: 0, skipped: 0, failed: 0 });
    }

    /// Advances a running import: files a finished download, starts the next one.
    pub fn poll(&mut self) {
        let Some(job) = &mut self.import else { return };
        if let Some((i, tmp_name)) = job.in_flight.clone() {
            let Some(result) = self.delegate.ivars().downloaded.borrow_mut().take() else { return };
            job.in_flight = None;
            let item = &job.items[i];
            let tmp = job.tmp.join(&tmp_name);
            let saved = result.and_then(|()| transfer::save_file(&job.dest, &item.name, item.taken, &tmp).map_err(|e| e.to_string()));
            match saved {
                Ok(Some(p)) => {
                    job.copied += 1;
                    job.r.send(Msg::Imported(p));
                }
                Ok(None) => job.skipped += 1,
                Err(e) => {
                    job.failed += 1;
                    let _ = std::fs::remove_file(&tmp);
                    job.r.send(Msg::Failed(format!("{}: {e}", item.name)));
                }
            }
            job.r.send(Msg::Progress { done: i + 1, total: job.items.len() });
        }
        // Files imported before are skipped without downloading; then one download goes out.
        while job.next < job.items.len() && !job.cancel.cancelled() {
            let i = job.next;
            job.next += 1;
            let item = &job.items[i];
            if transfer::already_imported(&job.dest, &item.name, item.taken, item.size) {
                job.skipped += 1;
                job.r.send(Msg::Progress { done: i + 1, total: job.items.len() });
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&job.tmp) {
                job.failed += 1;
                job.r.send(Msg::Failed(format!("{}: {e}", item.name)));
                continue;
            }
            let tmp_name = format!("{i}-{}", item.name);
            let options = download_options(&job.tmp, &tmp_name);
            unsafe {
                job.camera.requestDownloadFile_options_downloadDelegate_didDownloadSelector_contextInfo(
                    &item.file,
                    &options,
                    ProtocolObject::from_ref(&*self.delegate),
                    sel!(didDownloadFile:error:options:contextInfo:),
                    std::ptr::null_mut(),
                );
            }
            job.in_flight = Some((i, tmp_name));
            return;
        }
        let job = self.import.take().unwrap();
        let _ = std::fs::remove_dir(&job.tmp);
        job.r.send(Msg::Done(transfer::summary_for("Imported", job.copied, job.skipped, job.failed, job.cancel.cancelled())));
    }
}

/// The photos and videos on a device, with their names, dates and sizes.
fn files(cam: &ICCameraDevice) -> Vec<Item> {
    let Some(media) = (unsafe { cam.mediaFiles() }) else { return Vec::new() };
    media
        .iter()
        .filter_map(|item| item.downcast::<ICCameraFile>().ok())
        .filter_map(|file| {
            let name = unsafe { file.name() }?.to_string();
            if !transfer::is_media(std::path::Path::new(&name)) {
                return None;
            }
            let date = unsafe { file.exifCreationDate().or_else(|| file.creationDate()) };
            let taken = date.and_then(|d| to_local(&d)).unwrap_or_else(transfer::now);
            let size = unsafe { file.fileSize() }.max(0) as u64;
            Some(Item { file, name, taken, size })
        })
        .collect()
}

fn to_local(d: &NSDate) -> Option<NaiveDateTime> {
    let secs = d.timeIntervalSince1970();
    DateTime::from_timestamp(secs.floor() as i64, 0).map(|t| t.with_timezone(&Local).naive_local())
}

/// Download into `dir` as `name`, replacing a leftover from an earlier, interrupted import.
fn download_options(dir: &std::path::Path, name: &str) -> Retained<NSDictionary<NSString, AnyObject>> {
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&dir.to_string_lossy()), true);
    let keys: [&NSString; 3] = unsafe { [ICDownloadsDirectoryURL, ICSaveAsFilename, ICOverwrite] };
    let values: [Retained<AnyObject>; 3] = [url.into(), NSString::from_str(name).into(), NSNumber::new_bool(true).into()];
    NSDictionary::from_retained_objects(&keys, &values)
}
