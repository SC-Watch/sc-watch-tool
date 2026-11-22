//! An audit's pictures - watch._crops_from and _save_audit_images: the two
//! label crops, lossless (they may be re-read, and a JPEG artefact on a
//! 12-pixel glyph is the difference between an I and a 1), and the whole
//! frame as JPEG, which is only ever looked at by eye. Written under the
//! data folder's audit/, and a path is recorded only if its file was
//! written.

use std::path::Path;

use chrono::NaiveDateTime;
use scw_vision::{Image, Rect};

/// The crops behind a reading, cut while the frame is still in hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crops {
    pub name: Option<Image<u8>>,
    pub range: Option<Image<u8>>,
}

impl Crops {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.range.is_none()
    }
}

/// Where an audit's pictures went, relative to the data folder; "" if none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditPaths {
    pub crop_name: String,
    pub crop_range: String,
    pub frame_path: String,
}

/// `box.crop(frame, pad)`: the box grown by `pad` on every side, clipped to
/// the frame.
pub fn crop(frame: &Image<u8>, b: Rect, pad: i32) -> Option<Image<u8>> {
    let x0 = (b.x - pad).max(0) as usize;
    let y0 = (b.y - pad).max(0) as usize;
    let x1 = ((b.x + b.w + pad).max(0) as usize).min(frame.width);
    let y1 = ((b.y + b.h + pad).max(0) as usize).min(frame.height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let c = frame.channels;
    let mut out = Image::new(x1 - x0, y1 - y0, c);
    for y in y0..y1 {
        let src = &frame.data[(y * frame.width + x0) * c..(y * frame.width + x1) * c];
        let dst = (y - y0) * out.width * c;
        out.data[dst..dst + src.len()].copy_from_slice(src);
    }
    Some(out)
}

/// The name and range crops of a reading (`_crops_from`, 10 px of margin).
pub fn crops_from(frame: &Image<u8>, name_box: Option<Rect>, range_box: Option<Rect>) -> Crops {
    Crops {
        name: name_box.and_then(|b| crop(frame, b, 10)),
        range: range_box.and_then(|b| crop(frame, b, 10)),
    }
}

fn rgb(img: &Image<u8>) -> (Vec<u8>, png::ColorType) {
    match img.channels {
        1 => (img.data.clone(), png::ColorType::Grayscale),
        3 => (
            img.data
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect(),
            png::ColorType::Rgb,
        ),
        _ => (
            img.data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            png::ColorType::Rgba,
        ),
    }
}

/// A BGR (or grey) image as a PNG file.
pub fn write_png(path: &Path, img: &Image<u8>) -> bool {
    let (data, colour) = rgb(img);
    let Ok(f) = std::fs::File::create(path) else {
        return false;
    };
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(f),
        img.width as u32,
        img.height as u32,
    );
    enc.set_color(colour);
    enc.set_depth(png::BitDepth::Eight);
    let ok = enc
        .write_header()
        .and_then(|mut w| w.write_image_data(&data))
        .is_ok();
    if !ok {
        let _ = std::fs::remove_file(path);
    }
    ok
}

/// A BGR image as a JPEG file at `quality`.
pub fn write_jpeg(path: &Path, img: &Image<u8>, quality: u8) -> bool {
    let (data, _) = rgb(img);
    let kind = match img.channels {
        1 => jpeg_encoder::ColorType::Luma,
        3 => jpeg_encoder::ColorType::Rgb,
        _ => jpeg_encoder::ColorType::Rgba,
    };
    let (Ok(w), Ok(h)) = (u16::try_from(img.width), u16::try_from(img.height)) else {
        return false;
    };
    let Ok(enc) = jpeg_encoder::Encoder::new_file(path, quality.clamp(1, 100)) else {
        return false;
    };
    let ok = enc.encode(&data, w, h, kind).is_ok();
    if !ok {
        let _ = std::fs::remove_file(path);
    }
    ok
}

/// Writes an audit's pictures under `data_root`/audit, named for `at` and
/// the handle; returns the paths actually written.
pub fn save_audit_images(
    data_root: &Path,
    name: &str,
    crops: &Crops,
    frame: Option<&Image<u8>>,
    quality: u8,
    at: NaiveDateTime,
) -> AuditPaths {
    let dir = data_root.join("audit");
    let mut out = AuditPaths::default();
    if std::fs::create_dir_all(&dir).is_err() {
        return out;
    }
    let short: String = name.chars().take(16).collect();
    let stem = format!("{}_{short}", at.format("%Y%m%d_%H%M%S"));
    for (key, img, slot) in [
        ("crop_name", &crops.name, &mut out.crop_name),
        ("crop_range", &crops.range, &mut out.crop_range),
    ] {
        let Some(img) = img.as_ref().filter(|i| !i.data.is_empty()) else {
            continue;
        };
        let file = format!("{stem}_{key}.png");
        if write_png(&dir.join(&file), img) {
            *slot = format!("audit/{file}");
        }
    }
    if let Some(f) = frame {
        let file = format!("{stem}_frame.jpg");
        if write_jpeg(&dir.join(&file), f, quality) {
            out.frame_path = format!("audit/{file}");
        }
    }
    out
}
