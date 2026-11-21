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

/// Filters every column of `src` at once: out row y = op over input rows
/// y - anchor ..= y - anchor + k - 1 (outside the image neutral). The van Herk
/// / Gil-Werman recurrences run a whole row at a time, so each step is a
/// plain element-wise min or max the compiler can vectorise.
fn filter_columns(src: &Image<u8>, k: usize, op: Op) -> Image<u8> {
    let (w, h) = (src.width, src.height);
    if k <= 1 || w == 0 || h == 0 {
        return src.clone();
    }
    let anchor = k / 2;
    let len = h + k - 1;
    let neutral = vec![op.neutral(); w];
    let pad = |y: usize| -> &[u8] {
        if y < anchor || y >= anchor + h {
            &neutral
        } else {
            src.row(y - anchor)
        }
    };
    // g: running op from each block's start; hh: from each block's end.
    let mut g = vec![0u8; len * w];
    let mut hh = vec![0u8; len * w];
    for start in (0..len).step_by(k) {
        let end = (start + k).min(len);
        g[start * w..(start + 1) * w].copy_from_slice(pad(start));
        for y in start + 1..end {
            let (prev, cur) = g[(y - 1) * w..(y + 1) * w].split_at_mut(w);
            for ((c, &p), &v) in cur.iter_mut().zip(prev.iter()).zip(pad(y)) {
                *c = op.apply(p, v);
            }
        }
        hh[(end - 1) * w..end * w].copy_from_slice(pad(end - 1));
        for y in (start..end - 1).rev() {
            let (cur, next) = hh[y * w..(y + 2) * w].split_at_mut(w);
            for ((c, &n), &v) in cur.iter_mut().zip(next.iter()).zip(pad(y)) {
                *c = op.apply(n, v);
            }
        }
    }
    let mut out = Image::new(w, h, 1);
    for y in 0..h {
        let a = &hh[y * w..(y + 1) * w];
        let b = &g[(y + k - 1) * w..(y + k) * w];
        for ((o, &x), &z) in out.row_mut(y).iter_mut().zip(a).zip(b) {
            *o = op.apply(x, z);
        }
    }
    out
}

fn transpose(src: &Image<u8>) -> Image<u8> {
    let (w, h) = (src.width, src.height);
    let mut out = Image::new(h, w, 1);
    const B: usize = 32;
    for y0 in (0..h).step_by(B) {
        for x0 in (0..w).step_by(B) {
            for y in y0..(y0 + B).min(h) {
                let row = src.row(y);
                for x in x0..(x0 + B).min(w) {
                    out.data[x * h + y] = row[x];
                }
            }
        }
    }
    out
}

fn morph(src: &Image<u8>, kw: usize, kh: usize, op: Op) -> Image<u8> {
    assert_eq!(src.channels, 1, "morphology takes a single-channel image");
    assert!(kw >= 1 && kh >= 1, "kernel sides must be at least 1");
    // A rectangle is separable: rows (as the columns of the transpose), then columns.
    let rows_done = if kw > 1 {
        transpose(&filter_columns(&transpose(src), kw, op))
    } else {
        src.clone()
    };
    filter_columns(&rows_done, kh, op)
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
