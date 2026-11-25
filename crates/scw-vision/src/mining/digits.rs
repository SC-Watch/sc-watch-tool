//! `DigitTemplates`: the mining digit shapes and how a cell is matched to
//! them. A cell's patch (14x20, values 0..1) is compared with every shape of
//! every digit by correlation at the best of nine one-pixel shifts; a near
//! tie between the two best digits is settled by the pixels where their
//! closest shapes differ.
//!
//! The shapes are IRIS's, copied verbatim into models/iris-mining-digits.txt.

use std::sync::OnceLock;

use super::{base64_decode, net_max};

pub const WIDTH: usize = 14;
pub const HEIGHT: usize = 20;
pub const LEN: usize = WIDTH * HEIGHT;

/// One cell, resampled to the shapes' size.
pub type Patch = [f32; LEN];

const DATA: &str = include_str!("../../../../models/iris-mining-digits.txt");

struct Shapes {
    /// `Templates`: the full set and the mid set, per digit, in file order.
    normal: [Vec<Patch>; 10],
    /// `SmallTemplates`: the full set and the small set.
    small: [Vec<Patch>; 10],
}

fn shapes() -> &'static Shapes {
    static S: OnceLock<Shapes> = OnceLock::new();
    S.get_or_init(|| {
        let mut sections: Vec<(String, Vec<(usize, Patch)>)> = Vec::new();
        for line in DATA.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                sections.push((name.to_string(), Vec::new()));
                continue;
            }
            let (digit, text) = line.split_once(':').expect("digit:base64");
            let bytes = base64_decode(text).expect("a digit shape's base64");
            assert_eq!(bytes.len(), LEN, "a mining digit shape has the wrong size");
            let mut patch = [0f32; LEN];
            for (p, &b) in patch.iter_mut().zip(&bytes) {
                *p = b as f32 / 255.0;
            }
            let d = digit.parse::<usize>().expect("a digit");
            sections.last_mut().expect("a [section]").1.push((d, patch));
        }
        let section = |name: &str| -> &[(usize, Patch)] {
            &sections.iter().find(|s| s.0 == name).expect(name).1
        };
        let build = |extra: &str| -> [Vec<Patch>; 10] {
            let mut out: [Vec<Patch>; 10] = Default::default();
            for &(d, p) in section("full").iter().chain(section(extra)) {
                out[d].push(p);
            }
            out
        };
        Shapes {
            normal: build("mid"),
            small: build("small"),
        }
    })
}

/// `Correlation`: Pearson correlation of a w x h window of `a` at (x0a,
/// y0a) with one of `b` at (x0b, y0b).
#[allow(clippy::too_many_arguments)]
fn correlation(
    a: &Patch,
    b: &Patch,
    x0a: usize,
    y0a: usize,
    x0b: usize,
    y0b: usize,
    w: usize,
    h: usize,
) -> f64 {
    let (mut ma, mut mb) = (0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            ma += a[(y0a + y) * WIDTH + x0a + x] as f64;
            mb += b[(y0b + y) * WIDTH + x0b + x] as f64;
        }
    }
    let n = (w * h) as f64;
    ma /= n;
    mb /= n;
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            let da = a[(y0a + y) * WIDTH + x0a + x] as f64 - ma;
            let db = b[(y0b + y) * WIDTH + x0b + x] as f64 - mb;
            ab += da * db;
            aa += da * da;
            bb += db * db;
        }
    }
    ab / (aa * bb + 1e-9).sqrt()
}

/// `ShiftedCorrelation`: the best correlation over shifts of -1..1 each way.
fn shifted_correlation(a: &Patch, b: &Patch) -> f64 {
    let mut best = -1.0;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let c = correlation(
                a,
                b,
                dx.max(0) as usize,
                dy.max(0) as usize,
                (-dx).max(0) as usize,
                (-dy).max(0) as usize,
                WIDTH - dx.unsigned_abs() as usize,
                HEIGHT - dy.unsigned_abs() as usize,
            );
            best = net_max(best, c);
        }
    }
    best
}

fn shifted(t: &Patch, dx: i32, dy: i32) -> Patch {
    let mut q = [0f32; LEN];
    for y in 0..HEIGHT as i32 {
        for x in 0..WIDTH as i32 {
            let (sx, sy) = (x - dx, y - dy);
            if (0..WIDTH as i32).contains(&sx) && (0..HEIGHT as i32).contains(&sy) {
                q[(y as usize) * WIDTH + x as usize] = t[(sy as usize) * WIDTH + sx as usize];
            }
        }
    }
    q
}

/// `BestShift`: the shape moved by the shift that correlates best with `p`.
fn best_shift(p: &Patch, t: &Patch) -> Patch {
    let mut best = *t;
    let mut best_score = f64::NEG_INFINITY;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let q = shifted(t, dx, dy);
            let score = correlation(p, &q, 0, 0, 0, 0, WIDTH, HEIGHT);
            if score > best_score {
                best_score = score;
                best = q;
            }
        }
    }
    best
}

/// `Standardised`: mean 0, standard deviation 1 (LINQ's Average: summed in
/// order, then divided).
fn standardised(v: &Patch) -> Vec<f64> {
    let n = v.len() as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let sd = (v
        .iter()
        .map(|&x| (x as f64 - mean) * (x as f64 - mean))
        .sum::<f64>()
        / n)
        .sqrt();
    v.iter().map(|&x| (x as f64 - mean) / (sd + 1e-6)).collect()
}

/// `PrefersSecond`: does the patch sit closer to `b` than to `a`, judged
/// only where the two shapes differ?
fn prefers_second(patch: &Patch, a: &Patch, b: &Patch) -> bool {
    let weights: Vec<f64> = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x - y).abs() as f64)
        .collect();
    let total: f64 = weights.iter().sum();
    let p = standardised(patch);
    let error = |t: &Patch| -> f64 {
        let q = standardised(&best_shift(patch, t));
        let mut e = 0.0;
        for i in 0..q.len() {
            e += weights[i] / (total + 1e-9) * (p[i] - q[i]) * (p[i] - q[i]);
        }
        e
    };
    error(a) > error(b)
}

/// `Match`: the digit the patch is, its score, and every digit's score.
pub fn match_digit(patch: &Patch, tie_margin: f64, small: bool) -> (u8, f64, [f64; 10]) {
    let set = if small {
        &shapes().small
    } else {
        &shapes().normal
    };
    let mut scores = [f64::NEG_INFINITY; 10];
    let mut closest: [Option<&Patch>; 10] = [None; 10];
    for d in 0..10 {
        for t in &set[d] {
            let score = shifted_correlation(patch, t);
            if score > scores[d] {
                scores[d] = score;
                closest[d] = Some(t);
            }
        }
    }
    let mut order: Vec<usize> = (0..10).collect();
    // OrderByDescending(score).ThenBy(digit): a stable sort, highest first
    order.sort_by(|&x, &y| {
        scores[y]
            .partial_cmp(&scores[x])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.cmp(&y))
    });
    let (mut best, second) = (order[0], order[1]);
    if scores[best] - scores[second] < tie_margin
        && let (Some(a), Some(b)) = (closest[best], closest[second])
        && prefers_second(patch, a, b)
    {
        best = second;
    }
    (b'0' + best as u8, scores[best], scores)
}

/// `BestScore`: the best score of any shape of any digit.
pub fn best_score(patch: &Patch, small: bool) -> f64 {
    let set = if small {
        &shapes().small
    } else {
        &shapes().normal
    };
    let mut best = f64::NEG_INFINITY;
    for digit in set {
        for t in digit {
            best = net_max(best, shifted_correlation(patch, t));
        }
    }
    best
}

/// How many shapes each digit has, normal and small (for tests).
pub fn shape_counts() -> ([usize; 10], [usize; 10]) {
    let s = shapes();
    (
        std::array::from_fn(|d| s.normal[d].len()),
        std::array::from_fn(|d| s.small[d].len()),
    )
}
