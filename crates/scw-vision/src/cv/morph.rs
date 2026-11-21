//! Erode, dilate, opening and top-hat with rectangular kernels, as OpenCV
//! does them with its default border: pixels outside the image never win
//! (they count as 255 for a minimum and 0 for a maximum).
//!
//! The window for output x is input x - anchor ..= x - anchor + k - 1, anchor
//! = k / 2, for erode and dilate alike (OpenCV does not mirror the kernel).
//! A rectangle is separable, so rows then columns, each with the van Herk /
//! Gil-Werman filter: three comparisons a pixel whatever the kernel size.

use super::basic::subtract;
use crate::Image;

#[derive(Clone, Copy)]
enum Op {
    Min,
    Max,
}

impl Op {
    #[inline]
    fn apply(self, a: u8, b: u8) -> u8 {
        match self {
            Op::Min => a.min(b),
            Op::Max => a.max(b),
        }
    }

    #[inline]
    fn neutral(self) -> u8 {
        match self {
            Op::Min => 255,
            Op::Max => 0,
        }
    }
}

/// Filters one line in place: out[i] = op(line[i - anchor ..= i - anchor + k - 1]),
/// positions outside the line neutral. `pad`, `g` and `h` are scratch.
fn filter_line(
    line: &mut [u8],
    k: usize,
    op: Op,
    pad: &mut Vec<u8>,
    g: &mut Vec<u8>,
    h: &mut Vec<u8>,
) {
    let n = line.len();
    if k <= 1 || n == 0 {
        return;
    }
    let anchor = k / 2;
    let len = n + k - 1;
    pad.clear();
    pad.resize(len, op.neutral());
    pad[anchor..anchor + n].copy_from_slice(line);
    g.clear();
    g.resize(len, 0);
    h.clear();
    h.resize(len, 0);
    // g: running op from each block's start; h: from each block's end.
    for start in (0..len).step_by(k) {
        let end = (start + k).min(len);
        g[start] = pad[start];
        for j in start + 1..end {
            g[j] = op.apply(g[j - 1], pad[j]);
        }
        h[end - 1] = pad[end - 1];
        for j in (start..end - 1).rev() {
            h[j] = op.apply(h[j + 1], pad[j]);
        }
    }
    for (i, out) in line.iter_mut().enumerate() {
        *out = op.apply(h[i], g[i + k - 1]);
    }
}

fn morph(src: &Image<u8>, kw: usize, kh: usize, op: Op) -> Image<u8> {
    assert_eq!(src.channels, 1, "morphology takes a single-channel image");
    assert!(kw >= 1 && kh >= 1, "kernel sides must be at least 1");
    let (w, hgt) = (src.width, src.height);
    let mut out = src.clone();
    let (mut pad, mut g, mut h) = (Vec::new(), Vec::new(), Vec::new());
    if kw > 1 {
        for y in 0..hgt {
            filter_line(out.row_mut(y), kw, op, &mut pad, &mut g, &mut h);
        }
    }
    if kh > 1 {
        let mut col = vec![0u8; hgt];
        for x in 0..w {
            for (y, c) in col.iter_mut().enumerate() {
                *c = out.data[y * w + x];
            }
            filter_line(&mut col, kh, op, &mut pad, &mut g, &mut h);
            for (y, &c) in col.iter().enumerate() {
                out.data[y * w + x] = c;
            }
        }
    }
    out
}

/// `cv::erode` with a `kw` x `kh` rectangle.
pub fn erode(src: &Image<u8>, kw: usize, kh: usize) -> Image<u8> {
    morph(src, kw, kh, Op::Min)
}

/// `cv::dilate` with a `kw` x `kh` rectangle.
pub fn dilate(src: &Image<u8>, kw: usize, kh: usize) -> Image<u8> {
    morph(src, kw, kh, Op::Max)
}

/// `cv::morphologyEx(MORPH_OPEN)`: erode, then dilate.
pub fn open(src: &Image<u8>, kw: usize, kh: usize) -> Image<u8> {
    dilate(&erode(src, kw, kh), kw, kh)
}

/// `cv::morphologyEx(MORPH_TOPHAT)`: the image minus its opening.
pub fn tophat(src: &Image<u8>, kw: usize, kh: usize) -> Image<u8> {
    subtract(src, &open(src, kw, kh))
}
