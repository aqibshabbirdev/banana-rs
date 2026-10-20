# Banana Photos

An offline photo library written in Rust, laid out like Google Photos. Open a folder and every photo in it (and its sub-folders) is shown by month and day, newest first. Nothing is uploaded anywhere.

![Photos view](docs/screenshot.png)

## Features

- **Timeline**: month headings, day headings, and justified rows where photos keep their shape. Small days share a row with the next day, as in Google Photos.
- **Memories**: "1 year ago", "2 years ago"… cards for photos taken around today's date in past years.
- **Albums**: create, rename, delete, add and remove photos. Albums stay in the sidebar.
- **Favorites, Archive, Screenshots & recordings, Recently added** pages. Archived photos are hidden from Photos.
- **Selecting photos**: Ctrl/Cmd-click or the circle selects one photo, Shift-click selects a range, clicking a day heading selects the whole day, Ctrl/Cmd+A selects everything, Esc clears.
- **Search**: by file name or date ("2024", "october", "sunday", "2024-10-06").
- **Viewer**: ← / → to move between photos, F to favourite, Esc to go back.
- **Videos**: MP4, MOV, M4V, MKV, WebM, AVI, 3GP, MTS, WMV. They get poster-frame thumbnails with a duration badge, a Videos page, and in-app playback with sound: Space to play/pause, a seek bar, mute, and "open in default player". The recording date comes from the file's metadata. Phone videos shot in portrait show upright.
- **Formats**: JPEG, PNG, WebP, GIF, BMP, TIFF, and **HEIC / HEIF / AVIF** (iPhone photos).

Dates come from the camera's EXIF data, or from the file's modified time when there is none. Thumbnails are cached in `~/.cache/banana-rs/thumbs`. Albums, favourites and the archive list are saved in `~/.local/share/banana-rs/library.json` (on macOS: `~/Library/Application Support/banana-rs/`). The app never moves, edits or deletes your photos.

## Build on Linux

Ubuntu 24.04 / Debian 13 (the `libheif-dev` they ship is 1.17 or newer, which HEIC support needs). GStreamer plays the videos; `gstreamer1.0-libav` adds H.264/HEVC decoding:

```bash
sudo apt install build-essential pkg-config libheif-dev libheif-plugin-libde265 libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgl1-mesa-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release
./target/release/banana-rs ~/Pictures
```

Fedora: `sudo dnf install gcc libheif-devel gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-good libxkbcommon-devel mesa-libGL-devel libxcb-devel`.

On an older distro whose libheif is too old (e.g. Ubuntu 22.04), keep videos but drop HEIC:

```bash
cargo build --release --no-default-features --features video
```

Without the `video` feature, videos still appear in the library and open in the system's default player.

From a Mac, `scripts/build-linux.sh` builds the Linux binary inside Docker (Ubuntu 24.04, `scripts/linux.Dockerfile`) into `target/linux/release/banana-rs`.

## Mac (Apple Silicon, M1–M4)

```bash
scripts/build-macos.sh
```

This builds `dist/Banana Photos.app` and `dist/BananaPhotos-<version>-arm64.dmg`. It needs only Xcode command line tools and Rust. HEIC uses macOS's own ImageIO and videos use AVFoundation (hardware decoding), so there is no libheif, GStreamer or Homebrew dependency. Open the dmg and drag the app to Applications. It runs on macOS 11 (Big Sur) or newer.

The app is ad-hoc signed. On another Mac, the first launch needs right-click → **Open** (or `xattr -cr "/Applications/Banana Photos.app"`). To skip that, sign and notarize it with an Apple Developer ID: `SIGN_ID="Developer ID Application: …" scripts/build-macos.sh`, then `xcrun notarytool submit`.

Mac extras: trackpad pinch (or Cmd + scroll) resizes photos, Cmd+O opens a folder, and inside an Apple Photos library (`*.photoslibrary`) only the original photos are read, not its thumbnails.

The app icon is drawn by `cargo run --release --example make_icon -- assets/icon.png`.

## Run

```bash
banana-rs [folder]
```

With no folder it opens the last folder you used, or your Pictures folder.

## Layout

| File | What it does |
|---|---|
| `src/library.rs` | Walks the folder; reads each photo's date, shape and EXIF rotation |
| `src/imageio.rs` | macOS only: HEIC/HEIF/AVIF decoding through the system ImageIO framework |
| `src/video/` | Video metadata, thumbnails and playback: `mac.rs` (AVFoundation), `gst.rs` (GStreamer), `none.rs` (no backend) |
| `src/thumbs.rs` | Worker threads that decode thumbnails (visible ones first) and full images, with a disk cache |
| `src/store.rs` | Albums, favourites and archive, saved as JSON |
| `src/app.rs` | The egui UI: sidebar, justified timeline, memories, albums, viewer, dialogs |
