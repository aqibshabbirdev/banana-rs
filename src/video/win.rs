//! Windows: an MP4/MOV's length, size and date come from the file (see `probe_mp4`), and a
//! thumbnail from a frame Media Foundation decodes. Videos play in the default player.

use std::path::Path;
use std::sync::OnceLock;

use image::{DynamicImage, RgbaImage};
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::VideoInfo;

pub use super::none::Player;

pub fn probe(path: &Path) -> Option<VideoInfo> {
    super::probe_mp4(path)
}

/// Media Foundation, started once for the process (and COM on each thread that uses it).
fn started() -> bool {
    static STARTED: OnceLock<bool> = OnceLock::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    *STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok() })
}

/// A frame from about a second in (or the first), upright, at most `max_side` pixels.
pub fn thumbnail(path: &Path, max_side: u32) -> Option<DynamicImage> {
    if !started() {
        return None;
    }
    let frame = unsafe { frame(path) }.ok()??;
    let img = match super::turn_mp4(path) {
        90 => image::imageops::rotate90(&frame),
        180 => image::imageops::rotate180(&frame),
        270 => image::imageops::rotate270(&frame),
        _ => frame,
    };
    Some(DynamicImage::ImageRgba8(img).thumbnail(max_side, max_side))
}

unsafe fn frame(path: &Path) -> windows::core::Result<Option<RgbaImage>> {
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.unwrap();
    // Lets the reader convert whatever the codec gives into RGB32.
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
    let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)?;
    reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false.into())?;
    reader.SetStreamSelection(video, true.into())?;
    let wanted = MFCreateMediaType()?;
    wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    reader.SetCurrentMediaType(video, None, &wanted)?;
    let current = reader.GetCurrentMediaType(video)?;
    let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
    let (w, h) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
    let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);
    // The first frame about a second in (in 100 ns units), which skips black first frames; a
    // shorter video gives its last one.
    let mut last = None;
    for _ in 0..45 {
        let (mut flags, mut at, mut sample) = (0u32, 0i64, None::<IMFSample>);
        reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut at), Some(&mut sample))?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        let buffer = sample.ConvertToContiguousBuffer()?;
        let (mut data, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
        buffer.Lock(&mut data, None, Some(&mut len))?;
        let bytes = std::slice::from_raw_parts(data, len as usize);
        let mut img = RgbaImage::new(w, h);
        let row_len = stride.unsigned_abs() as usize;
        for y in 0..h as usize {
            // A negative stride means the rows are stored bottom-up.
            let src_row = if stride < 0 { h as usize - 1 - y } else { y };
            let Some(row) = bytes.get(src_row * row_len..src_row * row_len + w as usize * 4) else { break };
            for (x, px) in row.chunks_exact(4).enumerate() {
                // RGB32 is B, G, R, unused.
                img.put_pixel(x as u32, y as u32, image::Rgba([px[2], px[1], px[0], 255]));
            }
        }
        buffer.Unlock()?;
        last = Some(img);
        if at >= 10_000_000 {
            break;
        }
    }
    Ok(last)
}
