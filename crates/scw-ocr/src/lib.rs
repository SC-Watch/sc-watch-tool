//! scw-ocr: sc-watch's OCR - PaddleOCR's PP-OCRv3 text detector and
//! recogniser (the models the Python runs through RapidOCR 1.2.3, shipped
//! in models/ppocr/, Apache-2.0), run by rten in pure Rust, with RapidOCR's
//! own steps around them ported to give its answers: [`Ocr::read`] is
//! `RapidOCR()(img)` with the angle classifier off, as sc_detector.make_ocr
//! sets it.
//!
//! Used for chat reads, and by the legacy OCR reader for players who keep
//! the game's HDR on (PORT.md, "OCR (phase 5)"). Never on IRIS's plates.

pub mod det;
pub mod fill;
pub mod geom;
pub mod legacy;
pub mod rec;

use std::sync::Arc;

use rten::{Model, RunOptions, ThreadPool};
use rten_tensor::NdTensor;
use rten_tensor::prelude::*;
use scw_vision::Image;

pub use det::{DetSettings, Limit, Quad};

static DET_MODEL: &[u8] = include_bytes!("../../../models/ppocr/ch_PP-OCRv3_det_infer.onnx");
static REC_MODEL: &[u8] = include_bytes!("../../../models/ppocr/ch_PP-OCRv3_rec_infer.onnx");
static REC_KEYS: &str = include_str!("../../../models/ppocr/ppocr_keys.txt");

/// One line of text found: its box, text and RapidOCR's score.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub quad: Quad,
    pub text: String,
    pub score: f64,
}

/// The engine.
pub struct Ocr {
    det: Model,
    rec: Model,
    /// Class i's text: the blank, the model's characters, a space.
    chars: Vec<String>,
    pub det_settings: DetSettings,
    /// RapidOCR's text_score: lines scored under it are dropped.
    pub text_score: f64,
    /// Images this tall or less, or wider than `width_height_ratio` times
    /// their height, are read whole without the detector (RapidOCR's
    /// min_height 30, width_height_ratio 8).
    pub min_height: usize,
    pub width_height_ratio: f64,
    pool: Option<Arc<ThreadPool>>,
}

impl Ocr {
    /// The models built into the program.
    pub fn embedded() -> Result<Ocr, String> {
        let det = Model::load_static_slice(DET_MODEL).map_err(|e| format!("OCR detector: {e}"))?;
        let rec =
            Model::load_static_slice(REC_MODEL).map_err(|e| format!("OCR recogniser: {e}"))?;
        Ok(Self::with_models(det, rec, REC_KEYS))
    }

    fn with_models(det: Model, rec: Model, keys: &str) -> Ocr {
        let mut chars = vec!["blank".to_string()];
        chars.extend(keys.lines().map(str::to_string));
        chars.push(" ".to_string());
        Ocr {
            det,
            rec,
            chars,
            det_settings: DetSettings::default(),
            text_score: 0.5,
            min_height: 30,
            width_height_ratio: 8.0,
            pool: None,
        }
    }

    /// Runs on `n` threads of its own rather than one per core (the game
    /// runs beside it).
    pub fn with_threads(mut self, n: usize) -> Self {
        self.pool = Some(Arc::new(ThreadPool::with_num_threads(n.max(1))));
        self
    }

    fn opts(&self) -> Option<RunOptions> {
        self.pool
            .as_ref()
            .map(|p| RunOptions::default().with_thread_pool(Some(p.clone())))
    }

    /// The detector's probability map for a prepared image (h x w floats).
    pub fn det_map(&self, img: &Image<u8>) -> Result<Vec<f32>, String> {
        let (w, h) = (img.width, img.height);
        let x = NdTensor::from_data([1, 3, h, w], det::normalise(img));
        let out = self
            .det
            .run_one(x.view().into(), self.opts())
            .map_err(|e| format!("OCR detector: {e}"))?;
        let t: rten_tensor::Tensor<f32> =
            out.try_into().map_err(|e| format!("OCR detector: {e:?}"))?;
        Ok(t.to_vec())
    }

    /// TextDetector: the text boxes in a BGR image, as RapidOCR lists them
    /// before sorting.
    pub fn detect(&self, img: &Image<u8>) -> Result<Vec<Quad>, String> {
        let Some(input) = det::prepare(img, self.det_settings.limit) else {
            return Ok(Vec::new());
        };
        let map = self.det_map(&input)?;
        Ok(det::boxes(
            &map,
            input.width,
            input.height,
            img.width,
            img.height,
            &self.det_settings,
        ))
    }

    /// TextRecognizer: each crop's text and score, in the crops' order.
    /// Batched by width / height with equal ratios in input order (a
    /// stable sort; RapidOCR's np.argsort orders ties its own way once there
    /// are more than 16 crops, and a crop's batch sets its padding).
    pub fn recognize(&self, crops: &[Image<u8>]) -> Result<Vec<(String, f64)>, String> {
        self.recognize_ordered(crops, &rec::order(crops))
    }

    /// As [`Self::recognize`], batched in `order` (the tests give it
    /// numpy's).
    pub fn recognize_ordered(
        &self,
        crops: &[Image<u8>],
        order: &[usize],
    ) -> Result<Vec<(String, f64)>, String> {
        let mut out = vec![(String::new(), 0.0); crops.len()];
        for batch in order.chunks(rec::BATCH) {
            let mut max_ratio = 0f64;
            for &i in batch {
                let r = crops[i].width as f64 / crops[i].height as f64;
                if r > max_ratio {
                    max_ratio = r;
                }
            }
            let mut data = Vec::new();
            let mut width = 0;
            for &i in batch {
                let (d, w) = rec::resize_norm(&crops[i], max_ratio);
                data.extend(d);
                width = w;
            }
            let x = NdTensor::from_data([batch.len(), 3, rec::HEIGHT, width], data);
            let y = self
                .rec
                .run_one(x.view().into(), self.opts())
                .map_err(|e| format!("OCR recogniser: {e}"))?;
            let t: rten_tensor::Tensor<f32> =
                y.try_into().map_err(|e| format!("OCR recogniser: {e:?}"))?;
            let shape = t.shape().to_vec();
            let classes = shape[2];
            let per = shape[1] * classes;
            let v = t.to_vec();
            for (k, &i) in batch.iter().enumerate() {
                out[i] = rec::decode(&v[k * per..(k + 1) * per], classes, &self.chars);
            }
        }
        Ok(out)
    }

    /// `RapidOCR()(img)` without the angle classifier: the lines read, in
    /// reading order, those scored under `text_score` dropped.
    pub fn read(&self, img: &Image<u8>) -> Result<Vec<Line>, String> {
        let (w, h) = (img.width, img.height);
        if w == 0 || h == 0 {
            return Ok(Vec::new());
        }
        let whole = h <= self.min_height
            || (self.width_height_ratio != -1.0 && w as f64 / h as f64 > self.width_height_ratio);
        let (quads, crops) = if whole {
            let q: Quad = [
                [0.0, 0.0],
                [w as f32, 0.0],
                [w as f32, h as f32],
                [0.0, h as f32],
            ];
            (vec![q], vec![img.clone()])
        } else {
            let quads = det::sort_boxes(self.detect(img)?);
            if quads.is_empty() {
                return Ok(Vec::new());
            }
            let crops: Vec<Image<u8>> = quads.iter().map(|q| crop(img, q)).collect();
            (quads, crops)
        };
        let recs = self.recognize(&crops)?;
        Ok(quads
            .into_iter()
            .zip(recs)
            .filter(|(_, (_, s))| *s >= self.text_score)
            .map(|(quad, (text, score))| Line { quad, text, score })
            .collect())
    }

    /// sc_detector.make_ocr's `ocr(img)`: the lines' texts joined.
    pub fn text(&self, img: &Image<u8>) -> Result<String, String> {
        Ok(self.read(img)?.into_iter().map(|l| l.text).collect())
    }
}

/// get_rotate_crop_image: the box's pixels, upright (a box at least 1.5
/// times as tall as wide is turned a quarter anticlockwise, np.rot90).
pub fn crop(img: &Image<u8>, q: &Quad) -> Image<u8> {
    let norm = |a: [f32; 2], b: [f32; 2]| {
        let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
        (dx * dx + dy * dy).sqrt()
    };
    let cw = norm(q[0], q[1]).max(norm(q[2], q[3])) as usize;
    let ch = norm(q[0], q[3]).max(norm(q[1], q[2])) as usize;
    let upright =
        q[0][1] == q[1][1] && q[2][1] == q[3][1] && q[0][0] == q[3][0] && q[1][0] == q[2][0];
    let out = if upright {
        // warpPerspective by a whole-pixel translation copies the pixels;
        // BORDER_REPLICATE past the image's edge
        let (x0, y0) = (q[0][0] as i64, q[0][1] as i64);
        let c = img.channels;
        let mut o = Image::<u8>::new(cw, ch, c);
        for y in 0..ch {
            let sy = (y0 + y as i64).clamp(0, img.height as i64 - 1) as usize;
            let src = img.row(sy);
            let dst = o.row_mut(y);
            for x in 0..cw {
                let sx = (x0 + x as i64).clamp(0, img.width as i64 - 1) as usize;
                dst[x * c..(x + 1) * c].copy_from_slice(&src[sx * c..(sx + 1) * c]);
            }
        }
        o
    } else {
        warp::perspective_crop(img, q, cw, ch)
    };
    if out.height as f64 / out.width.max(1) as f64 >= 1.5 {
        rot90(&out)
    } else {
        out
    }
}

/// np.rot90: a quarter turn anticlockwise.
fn rot90(img: &Image<u8>) -> Image<u8> {
    let (w, h, c) = (img.width, img.height, img.channels);
    let mut o = Image::<u8>::new(h, w, c);
    for r in 0..w {
        for col in 0..h {
            let s = &img.row(col)[(w - 1 - r) * c..(w - r) * c];
            o.row_mut(r)[col * c..(col + 1) * c].copy_from_slice(s);
        }
    }
    o
}

pub mod warp;
