//! Where a row read's time goes, on the local reference crops
//! (SCW_PARITY_DATA/iris-golden): the mask, then the pieces (mostly the
//! letter model). Prints timings only.
use scw_vision::iris::{LetterModel, NameplateReader, ReaderSettings, RowReader};
use scw_vision::{Image, Rect};
use std::path::PathBuf;
use std::time::Instant;

fn read_bgr(path: &std::path::Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let mut data = Vec::new();
    for px in buf[..info.buffer_size()].chunks_exact(n) {
        if n < 3 {
            data.extend([px[0]; 3])
        } else {
            data.extend([px[2], px[1], px[0]])
        }
    }
    Image::from_vec(info.width as usize, info.height as usize, 3, data)
}

fn main() {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA").expect("SCW_PARITY_DATA"))
        .join("iris-golden");
    let recs: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(dir.join("golden.json")).unwrap()).unwrap();
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/iris-letters-model.bin");
    let rr = RowReader::new(
        NameplateReader::new(LetterModel::from_bytes(&std::fs::read(root).unwrap()).unwrap())
            .unwrap(),
        ReaderSettings::default(),
    );
    let (mut t_mask, mut t_pieces, mut calls_rows) = (0.0, 0.0, 0usize);
    let mut stage = [0.0f64; 4];
    let mut pixels = 0usize;
    for rec in &recs {
        let img = read_bgr(&dir.join(rec["crop"].as_str().unwrap()));
        for row in rec["rows"].as_array().unwrap() {
            let b: Vec<i32> = row["box"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap() as i32)
                .collect();
            let rect = Rect::new(b[0], b[1], b[2], b[3]);
            let t = Instant::now();
            let m = rr.mask(&img, rect, false);
            t_mask += t.elapsed().as_secs_f64();
            {
                use scw_vision::cv;
                let (x0, y0) = ((rect.x - 2).max(0) as usize, (rect.y - 2).max(0) as usize);
                let x1 = ((rect.x + rect.w + 2) as usize).min(img.width);
                let y1 = ((rect.y + rect.h + 2) as usize).min(img.height);
                let crop = img.crop(x0, y0, x1 - x0, y1 - y0);
                let t = Instant::now();
                let big = cv::resize_cubic_by(&crop, 4.0, 4.0);
                stage[0] += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let chans: Vec<_> = (0..3).map(|k| big.channel(k)).collect();
                stage[1] += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let e: Vec<_> = chans.iter().map(|c| cv::erode(c, 31, 31)).collect();
                stage[2] += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let d: Vec<_> = e.iter().map(|c| cv::dilate(c, 31, 31)).collect();
                stage[3] += t.elapsed().as_secs_f64();
                std::hint::black_box(d);
                pixels += big.width * big.height;
            }
            if let Some(m) = m {
                let t = Instant::now();
                std::hint::black_box(rr.reader.read_pieces(
                    &m,
                    Some(&rr.reader.name_alphabet),
                    0.02,
                    &rr.settings,
                ));
                t_pieces += t.elapsed().as_secs_f64();
            }
            calls_rows += 1;
        }
    }
    let n = calls_rows as f64;
    println!(
        "cubic {:.3} ms, split channels {:.3}, erode x3 {:.3}, dilate x3 {:.3} (mean 4x crop {} px)",
        1000.0 * stage[0] / n,
        1000.0 * stage[1] / n,
        1000.0 * stage[2] / n,
        1000.0 * stage[3] / n,
        pixels / calls_rows
    );
    println!(
        "{calls_rows} rows: mask {:.3} ms, name pieces {:.3} ms",
        1000.0 * t_mask / n,
        1000.0 * t_pieces / n
    );
}
