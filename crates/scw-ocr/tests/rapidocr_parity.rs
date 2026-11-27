//! scw-ocr against RapidOCR 1.2.3 itself, on every crop sc-watch's legacy
//! OCR pipeline hands it over the five corpora (plates, focused reads, chat
//! regions): the detector's boxes (sorted, to the pixel), each box's text
//! and score, and the result `RapidOCR()(img)` returns.
//!
//! The crops show player names, so the data never enters the repository:
//! SCW_PARITY_DATA/ocr/ holds `crops/*.png` and `rapidocr.json`, written by
//! the Python's own RapidOCR (tests/fixtures/make_ocr_ref.py). Without
//! it the test reports that it skipped and passes. Only counts are printed.
//! SCW_OCR_LIMIT=n reads only the first n crops; SCW_OCR_ONLY=ab12,cd34 only
//! the crops whose file names start so.

use std::path::{Path, PathBuf};

use scw_ocr::{Ocr, det};
use scw_vision::Image;
use serde_json::Value;

/// rten and onnxruntime differ in the last bits of a probability (6e-6 on
/// the recogniser, measured); a score is held to this.
const SCORE_TOLERANCE: f64 = 1e-4;

fn data() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?).join("ocr");
    dir.join("rapidocr.json").is_file().then_some(dir)
}

/// A PNG as OpenCV's imread(IMREAD_COLOR) gives it: 8-bit BGR.
fn read_bgr(path: &Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let mut out = Vec::with_capacity(info.width as usize * info.height as usize * 3);
    for px in buf[..info.buffer_size()].chunks(n) {
        if n < 3 {
            out.extend([px[0]; 3]);
        } else {
            out.extend([px[2], px[1], px[0]]);
        }
    }
    Image::from_vec(info.width as usize, info.height as usize, 3, out)
}

fn quads(v: &Value) -> Vec<det::Quad> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let p = |i: usize| {
                [
                    b[i][0].as_f64().unwrap() as f32,
                    b[i][1].as_f64().unwrap() as f32,
                ]
            };
            [p(0), p(1), p(2), p(3)]
        })
        .collect()
}

/// rten and onnxruntime differ in the last bits of the detector's map too
/// (2.5e-5 measured): a pixel right at the threshold can land on the other
/// side and move a box edge. True when flipping one of our map's pixels
/// within 1e-4 of the threshold gives RapidOCR's boxes exactly - the
/// difference is then that pixel, not the steps after it.
fn one_pixel_at_threshold(ocr: &Ocr, img: &Image<u8>, want: &[det::Quad]) -> bool {
    let s = &ocr.det_settings;
    let Some(input) = det::prepare(img, s.limit) else {
        return false;
    };
    let map = ocr.det_map(&input).unwrap();
    let near: Vec<usize> = map
        .iter()
        .enumerate()
        .filter(|(_, p)| (**p - s.thresh).abs() < 1e-4)
        .map(|(i, _)| i)
        .take(50)
        .collect();
    near.iter().any(|&i| {
        let mut m = map.clone();
        m[i] = if m[i] > s.thresh {
            s.thresh - 1e-6
        } else {
            s.thresh + 1e-6
        };
        det::sort_boxes(det::boxes(
            &m,
            input.width,
            input.height,
            img.width,
            img.height,
            s,
        )) == want
    })
}

#[test]
fn reads_as_rapidocr() {
    let Some(dir) = data() else {
        eprintln!("skipped: SCW_PARITY_DATA holds no ocr/rapidocr.json");
        return;
    };
    let reference: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("rapidocr.json")).unwrap()).unwrap();
    let limit: usize = std::env::var("SCW_OCR_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let ocr = Ocr::embedded().unwrap();
    let (mut crops, mut box_same, mut rec_same, mut result_same) = (0, 0, 0, 0);
    let (mut boxes_total, mut recs_total, mut recs_compared) = (0, 0, 0);
    let mut worst_score = 0f64;
    let mut tie_ordered = 0usize;
    let mut threshold_pixel = 0usize;
    let orders: serde_json::Map<String, Value> =
        std::fs::read_to_string(dir.join("rapidocr_order.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    let mut fails: Vec<String> = Vec::new();
    let started = std::time::Instant::now();
    let only: Vec<String> = std::env::var("SCW_OCR_ONLY")
        .map(|v| v.split(',').map(|s| format!("crops/{s}")).collect())
        .unwrap_or_default();
    for e in reference["crops"].as_array().unwrap().iter().take(limit) {
        let file = e["file"].as_str().unwrap();
        if !only.is_empty() && !only.iter().any(|o| file.starts_with(o.as_str())) {
            continue;
        }
        crops += 1;
        let img = read_bgr(&dir.join(file));
        let kind = e["kind"].as_str().unwrap();
        let short = &file[6..14];
        // the boxes
        let want_boxes = quads(&e["boxes"]);
        boxes_total += want_boxes.len();
        let got_boxes = if e["skip_det"].as_bool().unwrap() {
            want_boxes.clone()
        } else {
            det::sort_boxes(ocr.detect(&img).unwrap())
        };
        let boxes_ok = got_boxes == want_boxes;
        let noise = !boxes_ok && one_pixel_at_threshold(&ocr, &img, &want_boxes);
        if boxes_ok {
            box_same += 1;
        } else if noise {
            threshold_pixel += 1;
        } else if fails.len() < 25 {
            // the first box that differs, and whether it is only the order
            let i = want_boxes
                .iter()
                .zip(&got_boxes)
                .position(|(a, b)| a != b)
                .unwrap_or(want_boxes.len().min(got_boxes.len()));
            let same_set = want_boxes.len() == got_boxes.len()
                && want_boxes.iter().all(|b| got_boxes.contains(b));
            fails.push(format!(
                "{kind} {short}: boxes {} vs {}, first difference at {i}{}: {:?} vs {:?}",
                want_boxes.len(),
                got_boxes.len(),
                if same_set {
                    " (same boxes, other order)"
                } else {
                    ""
                },
                want_boxes.get(i),
                got_boxes.get(i)
            ));
        }
        // each of RapidOCR's own boxes read by us
        let want_recs = e["recs"].as_array().unwrap();
        recs_total += want_recs.len();
        let pieces: Vec<Image<u8>> = if e["skip_det"].as_bool().unwrap() {
            vec![img.clone()]
        } else {
            want_boxes.iter().map(|q| scw_ocr::crop(&img, q)).collect()
        };
        // RapidOCR batches the crops in np.argsort order of their width /
        // height, and numpy's default sort (vectorised on this CPU) orders
        // equal ratios its own way once there are more than 16 - a crop's
        // batch sets its padding, so its score and even its text. Where that
        // is not the stable order the program uses (rapidocr_order.json, 19
        // crops), the lines are batched in numpy's order here, so the rest
        // is still held exactly; the whole call is then checked by those
        // reads filtered as RapidOCR filters them.
        let numpy_order = orders.get(file).map(|o| {
            o.as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as usize)
                .collect::<Vec<usize>>()
        });
        let got_recs = match &numpy_order {
            Some(o) => {
                tie_ordered += 1;
                ocr.recognize_ordered(&pieces, o).unwrap()
            }
            None => ocr.recognize(&pieces).unwrap(),
        };
        let mut all_rec = true;
        for (k, (w, (gt, gs))) in want_recs.iter().zip(&got_recs).enumerate() {
            recs_compared += 1;
            let (wt, ws) = (w[0].as_str().unwrap(), w[1].as_f64().unwrap());
            let d = (ws - gs).abs();
            worst_score = worst_score.max(d);
            if wt != gt || d > SCORE_TOLERANCE {
                all_rec = false;
                if fails.len() < 25 {
                    fails.push(format!(
                        "{kind} {short} box {k}: text {} ({} chars vs {}), score {ws:.6} vs {gs:.6}",
                        if wt == gt { "same" } else { "differs" },
                        wt.chars().count(),
                        gt.chars().count()
                    ));
                }
            }
        }
        if all_rec && want_recs.len() == got_recs.len() {
            rec_same += 1;
        }
        // the whole call
        // (with RapidOCR's boxes and batch order where those were the
        // difference: RapidOCR's own boxes read by us, filtered as it does)
        let got_t: Vec<String> = if numpy_order.is_some() || noise {
            got_recs
                .iter()
                .filter(|(_, s)| *s >= ocr.text_score)
                .map(|(t, _)| t.clone())
                .collect()
        } else {
            ocr.read(&img)
                .unwrap()
                .into_iter()
                .map(|l| l.text)
                .collect()
        };
        let want: Vec<&str> = e["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[1].as_str().unwrap())
            .collect();
        if want == got_t.iter().map(String::as_str).collect::<Vec<_>>() {
            result_same += 1;
        } else if fails.len() < 25 {
            fails.push(format!(
                "{kind} {short}: result {} lines vs {}",
                want.len(),
                got_t.len()
            ));
        }
    }
    let secs = started.elapsed().as_secs_f64();
    println!(
        "   {crops} crops ({:.1} s, {:.0} ms each): boxes identical {box_same}/{crops} ({boxes_total} boxes); \
         each box read the same {rec_same}/{crops} ({recs_compared} of {recs_total} texts, scores within {worst_score:.1e}); \
         results identical {result_same}/{crops}; batched in numpy's tie order: {tie_ordered}; boxes moved by one map pixel at the threshold (rten vs onnxruntime): {threshold_pixel}",
        secs,
        secs * 1000.0 / crops as f64 / 2.0
    );
    for f in &fails {
        println!("   - {f}");
    }
    assert!(
        fails.is_empty(),
        "{} differences (first {} above)",
        fails.len(),
        fails.len().min(25)
    );
}
