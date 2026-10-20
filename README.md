# Banana Photos

An offline photo library written in Rust. Open a folder and every photo in it (and its sub-folders) is shown grouped by the day it was taken, newest first, like Google Photos. Nothing is uploaded anywhere.

![Grid view](docs/screenshot.png)

- The date comes from the camera's EXIF data. If a photo has none, the file's modified date is used, and the viewer marks it "(file date)".
- Thumbnails are made in the background and cached in `~/.cache/banana-rs/thumbs`, so the second time you open a folder it loads at once.
- Click a photo to open it. Use ← / → to move between photos and Esc to go back.
- Supported formats: JPEG, PNG, WebP, GIF, BMP, TIFF. HEIC is not supported yet.

## Build on Linux

Ubuntu / Debian:

```bash
sudo apt install build-essential pkg-config libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgl1-mesa-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release
./target/release/banana-rs ~/Pictures
```

Fedora: `sudo dnf install gcc libxkbcommon-devel mesa-libGL-devel libxcb-devel`.

The same code also builds on macOS and Windows with a plain `cargo build --release`.

## Run

```bash
banana-rs [folder]
```

With no folder it opens the last folder you used, or your Pictures folder.

## Layout

| File | What it does |
|---|---|
| `src/library.rs` | Walks the folder and reads each photo's date (EXIF, else file time) |
| `src/thumbs.rs` | Worker threads that decode thumbnails (visible ones first) and full images, with a disk cache |
| `src/app.rs` | The egui UI: day-grouped grid that only draws visible rows, and the full-screen viewer |
