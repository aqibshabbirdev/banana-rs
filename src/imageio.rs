//! macOS: decode HEIC/HEIF/AVIF with the system's ImageIO framework, so the app needs no
//! bundled libheif. ImageIO also applies the EXIF/HEIF rotation and scales while decoding.
//! Raw FFI keeps this free of extra crates.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use image::{DynamicImage, RgbaImage};

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
struct CFDictionaryKeyCallBacks([usize; 6]);
#[repr(C)]
struct CFDictionaryValueCallBacks([usize; 5]);

const K_CF_NUMBER_SINT32: CFIndex = 3;
const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    fn CFRelease(cf: CFTypeRef);
    fn CFURLCreateFromFileSystemRepresentation(alloc: CFTypeRef, buf: *const u8, len: CFIndex, is_dir: u8) -> CFTypeRef;
    fn CFDataCreate(alloc: CFTypeRef, bytes: *const u8, len: CFIndex) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> u8;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_cb: *const CFDictionaryKeyCallBacks,
        value_cb: *const CFDictionaryValueCallBacks,
    ) -> CFTypeRef;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImagePropertyPixelWidth: CFTypeRef;
    static kCGImagePropertyPixelHeight: CFTypeRef;
    static kCGImagePropertyOrientation: CFTypeRef;
    fn CGImageSourceCreateWithURL(url: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCopyPropertiesAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(image: CFTypeRef) -> usize;
    fn CGImageGetHeight(image: CFTypeRef) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CFTypeRef,
        bitmap_info: u32,
    ) -> CFTypeRef;
    fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, image: CFTypeRef);
}

/// Releases a CoreFoundation object when dropped.
struct Cf(CFTypeRef);

impl Cf {
    fn new(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

fn source(path: &Path) -> Option<Cf> {
    let bytes = path.as_os_str().as_bytes();
    unsafe {
        let url = Cf::new(CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex, 0))?;
        Cf::new(CGImageSourceCreateWithURL(url.0, std::ptr::null()))
    }
}

fn number(dict: CFTypeRef, key: CFTypeRef) -> Option<i32> {
    unsafe {
        let n = CFDictionaryGetValue(dict, key);
        let mut v: i32 = 0;
        (!n.is_null() && CFNumberGetValue(n, K_CF_NUMBER_SINT32, &mut v as *mut i32 as *mut c_void) != 0).then_some(v)
    }
}

/// Width and height as displayed (after rotation), read from the header only.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let src = source(path)?;
    unsafe {
        let props = Cf::new(CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()))?;
        let w = number(props.0, kCGImagePropertyPixelWidth)?;
        let h = number(props.0, kCGImagePropertyPixelHeight)?;
        let rotated = number(props.0, kCGImagePropertyOrientation).is_some_and(|o| o >= 5);
        Some(if rotated { (h as u32, w as u32) } else { (w as u32, h as u32) })
    }
}

/// Decodes the image upright, scaled so its longest side is at most `max_side`.
pub fn decode(path: &Path, max_side: u32) -> Option<DynamicImage> {
    decode_source(source(path)?, max_side)
}

/// Same as `decode`, from bytes already in memory (Locked Folder never writes them to disk).
pub fn decode_data(bytes: &[u8], max_side: u32) -> Option<DynamicImage> {
    let src = unsafe {
        let data = Cf::new(CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex))?;
        Cf::new(CGImageSourceCreateWithData(data.0, std::ptr::null()))?
    };
    decode_source(src, max_side)
}

fn decode_source(src: Cf, max_side: u32) -> Option<DynamicImage> {
    unsafe {
        let max = max_side as i32;
        let max_num = Cf::new(CFNumberCreate(std::ptr::null(), K_CF_NUMBER_SINT32, &max as *const i32 as *const c_void))?;
        let keys = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        ];
        let values = [kCFBooleanTrue, kCFBooleanTrue, max_num.0];
        let opts = Cf::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as CFIndex,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ))?;
        let img = Cf::new(CGImageSourceCreateThumbnailAtIndex(src.0, 0, opts.0))?;
        cgimage_to_image(img.0)
    }
}

/// Draws a CGImage (passed as a raw `CGImageRef`) into an RGBA buffer.
pub fn cgimage_to_image(img: *const c_void) -> Option<DynamicImage> {
    unsafe {
        let (w, h) = (CGImageGetWidth(img), CGImageGetHeight(img));
        if w == 0 || h == 0 {
            return None;
        }
        let mut pixels = vec![0u8; w * h * 4];
        let space = Cf::new(CGColorSpaceCreateDeviceRGB())?;
        let ctx = Cf::new(CGBitmapContextCreate(
            pixels.as_mut_ptr() as *mut c_void,
            w,
            h,
            8,
            w * 4,
            space.0,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG,
        ))?;
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
        CGContextDrawImage(ctx.0, rect, img);
        drop(ctx);

        RgbaImage::from_raw(w as u32, h as u32, pixels).map(DynamicImage::ImageRgba8)
    }
}
