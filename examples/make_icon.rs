//! Draws the app icon (a banana on a dark rounded square) as a 1024×1024 PNG.
//! `cargo run --release --example make_icon -- assets/icon.png`
//! For the phone app: `--full` fills the whole square (iOS rounds the corners itself), and
//! `--foreground` draws only the banana on transparent, inside the safe circle of Android's adaptive icons.
//!   cargo run --release --example make_icon -- phone/assets/icon-full.png --full
//!   cargo run --release --example make_icon -- phone/assets/icon-foreground.png --foreground

use image::{Rgba, RgbaImage};

const SIZE: u32 = 1024;
const SAMPLES: u32 = 4;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    MacOs,
    Full,
    Foreground,
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "assets/icon.png".into());
    let mode = match std::env::args().nth(2).as_deref() {
        Some("--full") => Mode::Full,
        Some("--foreground") => Mode::Foreground,
        _ => Mode::MacOs,
    };
    let mut img = RgbaImage::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut acc = [0f32; 4];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let c = shade_for(mode, px, py);
                    // Premultiply so edges blend correctly.
                    for i in 0..3 {
                        acc[i] += c[i] * c[3];
                    }
                    acc[3] += c[3];
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let a = acc[3] / n;
            let px = if a > 0.0 {
                [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], a]
            } else {
                [0.0; 4]
            };
            img.put_pixel(x, y, Rgba(px.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)));
        }
    }
    if let Some(dir) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    img.save(&out).unwrap();
    println!("wrote {out}");
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn hex(c: u32) -> [f32; 3] {
    [((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

/// Maps a pixel of the output to the macOS design (whose square is 100..924) and draws it.
fn shade_for(mode: Mode, x: f32, y: f32) -> [f32; 4] {
    match mode {
        Mode::MacOs => shade(x, y, true),
        // The square fills the canvas, corners included.
        Mode::Full => shade(100.0 + x * 824.0 / SIZE as f32, 100.0 + y * 824.0 / SIZE as f32, false),
        // The banana, centred and sized so all of it (stem too) sits inside the circle Android
        // always shows (66/108 of the icon): it reaches 355 px from (495, 450) in the design.
        Mode::Foreground => {
            let k = 355.0 / (SIZE as f32 / 2.0 * 66.0 / 108.0 * 0.95);
            let (sx, sy) = (495.0 + (x - 512.0) * k, 450.0 + (y - 512.0) * k);
            let c = shade(sx, sy, true);
            if banana_or_stem(sx, sy) { c } else { [0.0; 4] }
        }
    }
}

/// Whether a point of the macOS design is part of the banana or its stem (not background).
fn banana_or_stem(x: f32, y: f32) -> bool {
    let bg = background(x, y);
    let c = shade(x, y, true);
    c[3] > 0.0 && (c[0] - bg[0]).abs() + (c[1] - bg[1]).abs() + (c[2] - bg[2]).abs() > 0.01
}

fn background(x: f32, y: f32) -> [f32; 3] {
    let t = (y - 100.0) / 824.0;
    let c = mix(hex(0x2c3e74), hex(0x111830), t);
    let glow = ((x - 512.0).powi(2) + (y - 540.0).powi(2)).sqrt();
    mix(c, hex(0x3d5296), (1.0 - glow / 420.0).clamp(0.0, 1.0) * 0.6)
}

/// macOS icon grid: an 824px rounded square centred in the 1024 canvas.
fn in_squircle(x: f32, y: f32) -> bool {
    let (lo, hi, r) = (100.0, 924.0, 186.0);
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    x >= lo && x <= hi && y >= lo && y <= hi && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn shade(x: f32, y: f32, clip: bool) -> [f32; 4] {
    if clip && !in_squircle(x, y) {
        return [0.0; 4];
    }
    // Background: deep blue, lighter at the top, with a soft glow behind the banana.
    let mut c = background(x, y);

    // Banana: the part of circle A outside circle B, a crescent whose tips are where they cross.
    let (ax, ay, ar) = (500.0f32, 470.0f32, 300.0f32);
    let (bx, by, br) = (600.0f32, 350.0f32, 285.0f32);
    let da = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt();
    let db = ((x - bx).powi(2) + (y - by).powi(2)).sqrt();
    if da <= ar && db >= br {
        // Light along the inner curve, deeper yellow toward the outer edge, dark rim.
        let thick = (ar - da) + (db - br);
        let across = ((ar - da) / thick.max(1.0)).clamp(0.0, 1.0);
        let mut b = mix(hex(0xf2b600), hex(0xfff3a8), across.powf(0.8));
        b = mix(hex(0xb57a00), b, ((ar - da) / 14.0).clamp(0.0, 1.0));
        // Brown tips: near either point where the circles intersect.
        let (dx, dy) = (bx - ax, by - ay);
        let d = (dx * dx + dy * dy).sqrt();
        let a = (ar * ar - br * br + d * d) / (2.0 * d);
        let h = (ar * ar - a * a).max(0.0).sqrt();
        let (px, py) = (ax + a * dx / d, ay + a * dy / d);
        for (tx, ty, r) in [(px + h * dy / d, py - h * dx / d, 70.0), (px - h * dy / d, py + h * dx / d, 55.0)] {
            let dt = ((x - tx).powi(2) + (y - ty).powi(2)).sqrt();
            if dt < r {
                b = mix(hex(0x5e3d14), b, ((dt - r * 0.55) / (r * 0.45)).clamp(0.0, 1.0));
            }
        }
        // A faint ridge down the length, as real bananas have.
        let ridge = (across - 0.42).abs();
        if ridge < 0.025 && thick > 60.0 {
            b = mix(b, hex(0xd99a00), 0.35 * (1.0 - ridge / 0.025));
        }
        c = b;
    }

    // Stem sticking out of the upper tip.
    let (dx, dy) = (bx - ax, by - ay);
    let d = (dx * dx + dy * dy).sqrt();
    let a = (ar * ar - br * br + d * d) / (2.0 * d);
    let h = (ar * ar - a * a).max(0.0).sqrt();
    let (tx, ty) = (ax + a * dx / d + h * dy / d, ay + a * dy / d - h * dx / d);
    let (sx, sy) = (tx + 4.0, ty - 62.0);
    let t = (((x - tx) * (sx - tx) + (y - ty) * (sy - ty)) / ((sx - tx).powi(2) + (sy - ty).powi(2))).clamp(0.0, 1.0);
    let (qx, qy) = (tx + t * (sx - tx), ty + t * (sy - ty));
    if ((x - qx).powi(2) + (y - qy).powi(2)).sqrt() < 17.0 {
        c = mix(hex(0x5e3d14), hex(0x8a9a3a), t);
    }
    [c[0], c[1], c[2], 1.0]
}
