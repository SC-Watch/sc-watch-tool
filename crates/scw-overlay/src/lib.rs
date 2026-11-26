//! The mining overlay: IRIS's label (OverlayPainter.MiningLayout) drawn in
//! Rust - the same pixels on every system - and shown in a click-through,
//! always-on-top window over the game ([`OverlayWindow`]).
//!
//! The label sits centred under the scanner's signal: the rock and how
//! many, in the HUD's own text colour on a half-transparent rounded box of
//! the background's colour, its letters sized from the signal's digits.
//! The font is one the system has (Segoe UI Bold on Windows, the desktop's
//! bold sans on Linux): nothing is downloaded or bundled.

use std::path::PathBuf;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::OverlayWindow;

/// What to draw, in the captured monitor's pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    /// The signal (icon and digits): x, y, width, height.
    pub signal: [f64; 4],
    pub digit_height: f64,
    /// Blue, green, red.
    pub text_bgr: [u8; 3],
    pub back_bgr: [u8; 3],
}

/// A drawn label: where on the monitor, and its pixels - blue, green, red,
/// alpha, premultiplied, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

/// IRIS's layout numbers (MiningSettings, OverlayPainter).
const CAP_HEIGHT_EM: f64 = 0.7;
const GAP_LINES: f64 = 0.6;
const PAD_X_LINES: f64 = 0.5;
const PAD_Y_LINES: f64 = 0.25;
const BACK_ALPHA: f64 = 128.0 / 255.0;

/// Where the system's bold sans font may be, most wanted first.
pub fn font_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        for f in ["segoeuib.ttf", "arialbd.ttf", "segoeui.ttf", "arial.ttf"] {
            out.push(fonts.join(f));
        }
    }
    #[cfg(not(windows))]
    {
        // fontconfig's own answer first
        if let Ok(o) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", "sans-serif:bold"])
            .output()
            && o.status.success()
        {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
        for f in [
            "/usr/share/fonts/google-noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
        ] {
            out.push(PathBuf::from(f));
        }
    }
    out
}

/// Draws labels with one font.
pub struct Painter {
    font: FontVec,
    /// Where the font came from, for the banner.
    pub font_path: PathBuf,
}

impl Painter {
    /// With the first system font that loads.
    pub fn system() -> Result<Painter, String> {
        let tried = font_candidates();
        for p in &tried {
            if let Ok(bytes) = std::fs::read(p)
                && let Ok(font) = FontVec::try_from_vec(bytes)
            {
                return Ok(Painter {
                    font,
                    font_path: p.clone(),
                });
            }
        }
        Err(format!(
            "no bold sans font found (looked for {})",
            tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Painter, String> {
        Ok(Painter {
            font: FontVec::try_from_vec(bytes).map_err(|e| e.to_string())?,
            font_path: PathBuf::new(),
        })
    }

    /// The label laid out as IRIS lays it out (`MiningLayout`), `scale`
    /// times the signal's digit height; None for an empty text.
    pub fn draw(&self, label: &Label, scale: f64) -> Option<Drawn> {
        if label.text.trim().is_empty() {
            return None;
        }
        let h = (label.digit_height * scale).max(6.0);
        let px = PxScale::from((h / CAP_HEIGHT_EM) as f32);
        let font = self.font.as_scaled(px);
        // the text's width with its trailing spaces, and its line height
        let mut width = 0f32;
        let mut prev = None;
        let glyphs: Vec<_> = label
            .text
            .chars()
            .map(|c| {
                let id = font.glyph_id(c);
                if let Some(p) = prev {
                    width += font.kern(p, id);
                }
                let at = width;
                width += font.h_advance(id);
                prev = Some(id);
                (id, at)
            })
            .collect();
        let line = font.ascent() - font.descent() + font.line_gap();
        let (pad_x, pad_y) = (PAD_X_LINES * h, PAD_Y_LINES * h);
        let (bw, bh) = (width as f64 + 2.0 * pad_x, line as f64 + 2.0 * pad_y);
        let [sx, sy, sw, shh] = label.signal;
        let centre = sx + sw / 2.0;
        let top = sy + shh + GAP_LINES * h;
        let (x, y) = (
            (centre - bw / 2.0).round_ties_even() as i32,
            top.round_ties_even() as i32,
        );
        let (w, hh) = (bw.ceil().max(1.0) as usize, bh.ceil().max(1.0) as usize);
        let mut bgra = vec![0u8; w * hh * 4];
        // the box: rounded, its colour at half alpha, edges antialiased
        let radius = (hh as f64 * 0.25)
            .max(2.0)
            .min(w as f64 / 2.0)
            .min(hh as f64 / 2.0);
        let back = label.back_bgr.map(|c| c as f64);
        for py in 0..hh {
            for pxl in 0..w {
                let cov = rounded_cover(
                    pxl as f64 + 0.5,
                    py as f64 + 0.5,
                    w as f64,
                    hh as f64,
                    radius,
                );
                if cov <= 0.0 {
                    continue;
                }
                let a = BACK_ALPHA * cov;
                let i = (py * w + pxl) * 4;
                bgra[i] = (back[0] * a).round() as u8;
                bgra[i + 1] = (back[1] * a).round() as u8;
                bgra[i + 2] = (back[2] * a).round() as u8;
                bgra[i + 3] = (a * 255.0).round() as u8;
            }
        }
        // the text over it, in the HUD's colour
        let text = label.text_bgr.map(|c| c as f32);
        let (ox, base) = ((x as f64 + pad_x) as f32, (pad_y as f32) + font.ascent());
        for (id, at) in glyphs {
            let glyph = id.with_scale_and_position(px, point(ox - x as f32 + at, base));
            let Some(outline) = self.font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let (qx, qy) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if qx < 0 || qy < 0 || qx as usize >= w || qy as usize >= hh {
                    return;
                }
                let c = c.clamp(0.0, 1.0);
                let i = (qy as usize * w + qx as usize) * 4;
                for k in 0..3 {
                    let dst = bgra[i + k] as f32;
                    bgra[i + k] = (text[k] * c + dst * (1.0 - c)).round() as u8;
                }
                let da = bgra[i + 3] as f32;
                bgra[i + 3] = (255.0 * c + da * (1.0 - c)).round() as u8;
            });
        }
        Some(Drawn {
            x,
            y,
            width: w,
            height: hh,
            bgra,
        })
    }
}

/// How much of the pixel at (x, y) a w x h box with corners of radius r
/// covers, antialiased over one pixel.
fn rounded_cover(x: f64, y: f64, w: f64, h: f64, r: f64) -> f64 {
    let dx = (x - w / 2.0).abs() - (w / 2.0 - r);
    let dy = (y - h / 2.0).abs() - (h / 2.0 - r);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    let inside = dx.max(dy).min(0.0);
    let d = outside + inside - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// A window that shows one drawn label at a time, over everything,
/// passing clicks through - on systems without one yet, nothing.
#[cfg(not(windows))]
pub struct OverlayWindow;

#[cfg(not(windows))]
impl OverlayWindow {
    /// The window for a monitor whose top-left corner is at (left, top) on
    /// the desktop.
    pub fn open(_left: i32, _top: i32) -> Result<OverlayWindow, String> {
        Err("the overlay window is not built for this system yet".into())
    }

    pub fn show(&self, _drawn: Option<Drawn>) {}
}

#[cfg(test)]
mod tests {
    use super::rounded_cover;

    #[test]
    fn the_box_is_full_inside_and_empty_outside_its_corners() {
        assert_eq!(rounded_cover(10.5, 5.5, 40.0, 12.0, 3.0), 1.0);
        assert_eq!(rounded_cover(0.1, 0.1, 40.0, 12.0, 3.0), 0.0);
        let edge = rounded_cover(0.5, 6.0, 40.0, 12.0, 3.0);
        assert!(edge > 0.0 && edge <= 1.0);
    }
}
