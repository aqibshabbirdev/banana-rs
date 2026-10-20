//! The three small ONNX models, run with tract (pure Rust, nothing to install):
//! - YuNet (OpenCV Zoo, MIT) finds faces and five landmarks,
//! - SFace (OpenCV Zoo, Apache-2.0) turns an aligned face into a 128-number fingerprint,
//! - MobileNetV2 (ONNX Model Zoo, Apache-2.0) tells dogs and cats apart from everything else.

use std::path::Path;

use image::{imageops::FilterType, RgbImage};
use tract_onnx::prelude::*;

use super::Pet;

pub const DETECTOR: &str = "face_detection_yunet_2023mar.onnx";
pub const RECOGNIZER: &str = "face_recognition_sface_2021dec.onnx";
pub const CLASSIFIER: &str = "mobilenetv2-12.onnx";

type Plan = Arc<TypedRunnableModel>;

pub struct Models {
    detector: Plan,
    recognizer: Plan,
    classifier: Plan,
}

/// A face found in an image, in that image's pixel coordinates.
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Right eye, left eye, nose tip, right and left mouth corners.
    pub landmarks: [[f32; 2]; 5],
}

const DET_SIZE: usize = 640;
const MIN_SCORE: f32 = 0.8;

fn load(dir: &Path, name: &str, shape: [usize; 4]) -> TractResult<Plan> {
    tract_onnx::onnx()
        .model_for_path(dir.join(name))?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()
}

impl Models {
    pub fn load(dir: &Path) -> TractResult<Self> {
        Ok(Self {
            detector: load(dir, DETECTOR, [1, 3, DET_SIZE, DET_SIZE])?,
            recognizer: load(dir, RECOGNIZER, [1, 3, 112, 112])?,
            classifier: load(dir, CLASSIFIER, [1, 3, 224, 224])?,
        })
    }

    /// YuNet on a 640×640 letterboxed copy (the model's input size is fixed).
    pub fn detect(&self, img: &RgbImage) -> TractResult<Vec<Detection>> {
        let scale = DET_SIZE as f32 / img.width().max(img.height()) as f32;
        let (w, h) = (((img.width() as f32 * scale).round() as u32).max(1), ((img.height() as f32 * scale).round() as u32).max(1));
        let small = image::imageops::resize(img, w, h, FilterType::Triangle);

        // BGR, 0..255, padded with black on the right and bottom.
        let input = tract_ndarray::Array4::from_shape_fn((1, 3, DET_SIZE, DET_SIZE), |(_, c, y, x)| {
            if x < w as usize && y < h as usize {
                small.get_pixel(x as u32, y as u32)[2 - c] as f32
            } else {
                0.0
            }
        });
        let out = self.detector.run(tvec!(Tensor::from(input).into()))?;

        let mut found = Vec::new();
        for (k, stride) in [8usize, 16, 32].into_iter().enumerate() {
            let cols = DET_SIZE / stride;
            let cls = out[k].try_as_plain_ram()?.as_slice::<f32>()?;
            let obj = out[3 + k].try_as_plain_ram()?.as_slice::<f32>()?;
            let bbox = out[6 + k].try_as_plain_ram()?.as_slice::<f32>()?;
            let kps = out[9 + k].try_as_plain_ram()?.as_slice::<f32>()?;
            for i in 0..cls.len() {
                let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
                if score < MIN_SCORE {
                    continue;
                }
                let (col, row) = ((i % cols) as f32, (i / cols) as f32);
                let s = stride as f32;
                let b = &bbox[i * 4..i * 4 + 4];
                let (cx, cy) = ((col + b[0]) * s, (row + b[1]) * s);
                let (bw, bh) = (b[2].exp() * s, b[3].exp() * s);
                let mut landmarks = [[0.0; 2]; 5];
                for (n, l) in landmarks.iter_mut().enumerate() {
                    *l = [(kps[i * 10 + 2 * n] + col) * s / scale, (kps[i * 10 + 2 * n + 1] + row) * s / scale];
                }
                found.push(Detection {
                    x: (cx - bw / 2.0) / scale,
                    y: (cy - bh / 2.0) / scale,
                    w: bw / scale,
                    h: bh / scale,
                    score,
                    landmarks,
                });
            }
        }
        Ok(nms(found, 0.3))
    }

    /// SFace fingerprint of one face (L2-normalised), from the full-resolution image.
    pub fn embed(&self, img: &RgbImage, face: &Detection) -> TractResult<Vec<f32>> {
        let aligned = align(img, &face.landmarks);
        let input = tract_ndarray::Array4::from_shape_fn((1, 3, 112, 112), |(_, c, y, x)| aligned.get_pixel(x as u32, y as u32)[c] as f32);
        let out = self.recognizer.run(tvec!(Tensor::from(input).into()))?;
        let mut v = out[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(v)
    }

    /// Dog, cat or neither, from ImageNet's dog breeds (151-268) and domestic cats (281-285).
    pub fn pet(&self, img: &RgbImage) -> TractResult<Option<Pet>> {
        // Resize the short side to 256, centre-crop 224, normalise like torchvision.
        let s = 256.0 / img.width().min(img.height()) as f32;
        let (w, h) = ((img.width() as f32 * s).round() as u32, (img.height() as f32 * s).round() as u32);
        let r = image::imageops::resize(img, w.max(224), h.max(224), FilterType::Triangle);
        let (ox, oy) = ((r.width() - 224) / 2, (r.height() - 224) / 2);
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let input = tract_ndarray::Array4::from_shape_fn((1, 3, 224, 224), |(_, c, y, x)| {
            (r.get_pixel(ox + x as u32, oy + y as u32)[c] as f32 / 255.0 - MEAN[c]) / STD[c]
        });
        let out = self.classifier.run(tvec!(Tensor::from(input).into()))?;
        let logits = out[0].try_as_plain_ram()?.as_slice::<f32>()?;
        let max = logits.iter().cloned().fold(f32::MIN, f32::max);
        let exp: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
        let total: f32 = exp.iter().sum();
        let dog: f32 = exp[151..=268].iter().sum::<f32>() / total;
        let cat: f32 = exp[281..=285].iter().sum::<f32>() / total;
        Ok(if dog > 0.5 {
            Some(Pet::Dog)
        } else if cat > 0.5 {
            Some(Pet::Cat)
        } else {
            None
        })
    }
}

fn iou(a: &Detection, b: &Detection) -> f32 {
    let (x1, y1) = (a.x.max(b.x), a.y.max(b.y));
    let (x2, y2) = ((a.x + a.w).min(b.x + b.w), (a.y + a.h).min(b.y + b.h));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    inter / (a.w * a.h + b.w * b.h - inter).max(1e-6)
}

fn nms(mut dets: Vec<Detection>, threshold: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| iou(k, &d) < threshold) {
            keep.push(d);
        }
    }
    keep
}

/// Where the five landmarks sit in SFace's 112×112 input (the ArcFace template).
const TEMPLATE: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// Rotates, scales and shifts the face so its landmarks land on the template (least-squares
/// similarity transform), sampling with bilinear filtering.
fn align(img: &RgbImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let n = 5.0;
    let (mut sx, mut sy, mut dx, mut dy) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..5 {
        sx += landmarks[i][0];
        sy += landmarks[i][1];
        dx += TEMPLATE[i][0];
        dy += TEMPLATE[i][1];
    }
    let (msx, msy, mdx, mdy) = (sx / n, sy / n, dx / n, dy / n);
    let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
    for i in 0..5 {
        let (xs, ys) = (landmarks[i][0] - msx, landmarks[i][1] - msy);
        let (xd, yd) = (TEMPLATE[i][0] - mdx, TEMPLATE[i][1] - mdy);
        num_a += xs * xd + ys * yd;
        num_b += xs * yd - ys * xd;
        den += xs * xs + ys * ys;
    }
    let (a, b) = (num_a / den.max(1e-6), num_b / den.max(1e-6));
    let (tx, ty) = (mdx - (a * msx - b * msy), mdy - (b * msx + a * msy));
    let s2 = (a * a + b * b).max(1e-9);

    RgbImage::from_fn(112, 112, |u, v| {
        // Inverse of [a -b; b a] * p + t.
        let (px, py) = (u as f32 + 0.5 - tx, v as f32 + 0.5 - ty);
        let x = (a * px + b * py) / s2 - 0.5;
        let y = (-b * px + a * py) / s2 - 0.5;
        sample(img, x, y)
    })
}

fn sample(img: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |xx: i64, yy: i64| -> [f32; 3] {
        if xx < 0 || yy < 0 || xx >= w || yy >= h {
            [0.0; 3]
        } else {
            let p = img.get_pixel(xx as u32, yy as u32);
            [p[0] as f32, p[1] as f32, p[2] as f32]
        }
    };
    let (p00, p10, p01, p11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] * (1.0 - fx) + p10[c] * fx;
        let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
        out[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(out)
}
