//! Turning the HDR desktop back into the picture the game drew
//! (sc-watch's hdr_capture.py, exactly).
//!
//! With Windows HDR on, the 8-bit desktop copy capture tools get treats 80
//! nits as white, so most of a bright scene - and the HUD text's glow -
//! arrives as flat white. Asked for R16G16B16A16_FLOAT, the same duplication
//! gives scRGB: linear light, 1.0 = 80 nits, nothing clipped. For a game
//! drawing an SDR picture, the desktop holds it at the "SDR white" level, so
//!
//! ```text
//! 8-bit = sRGB(clip(scRGB / (SDR white / 80), 0, 1))
//! ```
//!
//! is the game's own image. Done through a table over every float16 bit
//! pattern, so a frame is one table read per value.

use scw_vision::Image;

/// DXGI_FORMAT_R16G16B16A16_FLOAT.
pub const FP16: u32 = 10;
/// DXGI_FORMAT_B8G8R8A8_UNORM.
pub const BGRA8: u32 = 87;

/// One float16 bit pattern's value, exactly.
pub fn f16_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 10) & 0x1f) as i32;
    let frac = (bits & 0x3ff) as f64;
    match exp {
        0 => sign * frac * 2f64.powi(-24),
        31 if frac == 0.0 => sign * f64::INFINITY,
        31 => f64::NAN,
        e => sign * (1.0 + frac / 1024.0) * 2f64.powi(e - 15),
    }
}

fn srgb(x: f64) -> f64 {
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// The 8-bit value for every float16 bit pattern at an SDR white level:
/// SDR white and brighter -> 255, black, negative and NaN -> 0.
pub fn lut_for(sdr_white_nits: f64) -> Box<[u8; 65536]> {
    let white = sdr_white_nits.max(1.0) / 80.0;
    let mut lut = Box::new([0u8; 65536]);
    for (bits, out) in lut.iter_mut().enumerate() {
        let v = f16_to_f64(bits as u16);
        // numpy.nan_to_num: NaN -> 0, +inf -> 1e9, -inf -> 0.
        let v = if v.is_nan() {
            0.0
        } else if v == f64::INFINITY {
            1e9
        } else if v == f64::NEG_INFINITY {
            0.0
        } else {
            v
        };
        let s = srgb((v / white).clamp(0.0, 1.0));
        *out = (255.0 * s).round_ties_even() as u8;
    }
    lut
}

/// RGBA float16 rows (bit patterns, `pitch` values a row, at least
/// `width * 4`) to a BGR 8-bit image.
pub fn to_bgr(
    raw: &[u16],
    width: usize,
    height: usize,
    pitch: usize,
    lut: &[u8; 65536],
) -> Image<u8> {
    assert!(
        pitch >= width * 4 && raw.len() >= pitch * height,
        "short float frame"
    );
    let mut out = Image::new(width, height, 3);
    for y in 0..height {
        let src = &raw[y * pitch..y * pitch + width * 4];
        let dst = out.row_mut(y);
        for (d, s) in dst
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0)
        {
            d[0] = lut[s[2] as usize];
            d[1] = lut[s[1] as usize];
            d[2] = lut[s[0] as usize];
        }
    }
    out
}

/// BGRA 8-bit rows (`pitch` bytes a row) to BGR: alpha dropped, as dxcam's
/// BGR output does.
pub fn bgra_to_bgr(raw: &[u8], width: usize, height: usize, pitch: usize) -> Image<u8> {
    assert!(
        pitch >= width * 4 && raw.len() >= pitch * height,
        "short frame"
    );
    let mut out = Image::new(width, height, 3);
    for y in 0..height {
        let src = &raw[y * pitch..y * pitch + width * 4];
        for (d, s) in out
            .row_mut(y)
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0)
        {
            d.copy_from_slice(&s[..3]);
        }
    }
    out
}

/// What to do about Windows HDR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrCapture {
    /// The ordinary 8-bit capture, whatever the display does.
    Off,
    /// The float capture when Windows HDR is on for the display.
    Auto,
}

impl HdrCapture {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Use the float capture? Only when asked and HDR is known to be on.
    pub fn wanted(self, hdr_on: Option<bool>) -> bool {
        self == Self::Auto && hdr_on == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f64(0x3c00), 1.0);
        assert_eq!(f16_to_f64(0xc000), -2.0);
        assert_eq!(f16_to_f64(0x7bff), 65504.0);
        assert_eq!(f16_to_f64(0x0001), 2f64.powi(-24));
        assert!(f16_to_f64(0x7e00).is_nan());
        assert_eq!(f16_to_f64(0x7c00), f64::INFINITY);
    }

    #[test]
    fn wanted_only_when_on() {
        assert!(HdrCapture::Auto.wanted(Some(true)));
        assert!(!HdrCapture::Auto.wanted(None));
        assert!(!HdrCapture::Off.wanted(Some(true)));
    }
}
