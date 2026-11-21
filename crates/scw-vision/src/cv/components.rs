//! `cv::connectedComponentsWithStats`, with OpenCV's label order.
//!
//! The order is what matters for byte parity, and it follows from how OpenCV
//! scans: its 8-connectivity algorithm (Spaghetti, after BBDT) walks 2x2
//! blocks in raster order and gives a component the label created at its
//! first block; its 4-connectivity one walks single pixels. So label 1 is the
//! component met first in that scan, label 2 the next, and so on. Within a
//! 2x2 block every foreground pixel is 8-connected to every other, so no two
//! 8-connected components share a block, and the order is total.
//!
//! The work is done on runs of foreground pixels, not pixels: HUD masks are a
//! few percent ink, so a frame is mostly skipped in one pass.

use crate::Image;

/// One label's statistics: left, top, width, height, area (OpenCV's
/// CC_STAT_* order). Label 0 is the background.
pub type Stats = [i32; 5];

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        parent[i as usize] = parent[parent[i as usize] as usize];
        i = parent[i as usize];
    }
    i
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        parent[hi as usize] = lo;
    }
}

/// A run of foreground pixels: row, first column, one past the last.
#[derive(Clone, Copy)]
struct Run {
    y: u32,
    x0: u32,
    x1: u32,
}

/// The first non-zero byte at or after `x`, eight bytes at a time over zeros.
fn next_ink(row: &[u8], mut x: usize) -> usize {
    while x + 8 <= row.len() && u64::from_ne_bytes(row[x..x + 8].try_into().unwrap()) == 0 {
        x += 8;
    }
    while x < row.len() && row[x] == 0 {
        x += 1;
    }
    x
}

/// Labels the non-zero pixels of a single-channel mask. `connectivity` is 4
/// or 8. Returns the label image and one [`Stats`] per label, background
/// first.
pub fn connected_components(mask: &Image<u8>, connectivity: u8) -> (Image<i32>, Vec<Stats>) {
    assert_eq!(
        mask.channels, 1,
        "connected_components takes a single-channel mask"
    );
    assert!(
        connectivity == 4 || connectivity == 8,
        "connectivity is 4 or 8"
    );
    let (w, h) = (mask.width, mask.height);
    let mut runs: Vec<Run> = Vec::new();
    let mut row_start: Vec<usize> = Vec::with_capacity(h + 1);
    for y in 0..h {
        row_start.push(runs.len());
        let row = mask.row(y);
        let mut x = next_ink(row, 0);
        while x < w {
            let s = x;
            while x < w && row[x] != 0 {
                x += 1;
            }
            runs.push(Run {
                y: y as u32,
                x0: s as u32,
                x1: x as u32,
            });
            x = next_ink(row, x);
        }
    }
    row_start.push(runs.len());

    // Join each run with the runs above it that it touches: overlapping
    // columns (4), or overlapping or diagonally next to them (8).
    let reach = if connectivity == 8 { 1 } else { 0 };
    let mut parent: Vec<u32> = (0..runs.len() as u32).collect();
    for y in 1..h {
        let (above, here) = (
            row_start[y - 1]..row_start[y],
            row_start[y]..row_start[y + 1],
        );
        let mut first = above.start;
        for j in here {
            let r = runs[j];
            while first < above.end && runs[first].x1 + reach <= r.x0 {
                first += 1;
            }
            let mut k = first;
            while k < above.end && runs[k].x0 < r.x1 + reach {
                union(&mut parent, j as u32, k as u32);
                k += 1;
            }
        }
    }

    // Each component's place in OpenCV's scan: its first pixel (4) or its
    // first 2x2 block (8), as a raster index - the least over its runs.
    let mut key = vec![u64::MAX; runs.len()];
    for (i, r) in runs.iter().enumerate() {
        let root = find(&mut parent, i as u32) as usize;
        let k = if connectivity == 8 {
            (r.y as u64 / 2) * w as u64 + r.x0 as u64 / 2
        } else {
            r.y as u64 * w as u64 + r.x0 as u64
        };
        key[root] = key[root].min(k);
    }
    let mut roots: Vec<(u64, u32)> = (0..runs.len())
        .filter(|&i| parent[i] == i as u32)
        .map(|i| (key[i], i as u32))
        .collect();
    roots.sort_unstable();
    let mut label_of = vec![0i32; runs.len()];
    for (label, &(_, root)) in roots.iter().enumerate() {
        label_of[root as usize] = label as i32 + 1;
    }

    // Labels, and per-label bounds (left/top as minimums, right/bottom as
    // maximums, widths last).
    let count = roots.len() + 1;
    let mut stats: Vec<Stats> = vec![[i32::MAX, i32::MAX, i32::MIN, i32::MIN, 0]; count];
    let mut labels = Image::<i32>::new(w, h, 1);
    let mut ink = 0i64;
    for (i, r) in runs.iter().enumerate() {
        let l = label_of[find(&mut parent, i as u32) as usize];
        let (y, x0, x1) = (r.y as usize, r.x0 as usize, r.x1 as usize);
        labels.row_mut(y)[x0..x1].fill(l);
        let s = &mut stats[l as usize];
        s[0] = s[0].min(x0 as i32);
        s[1] = s[1].min(y as i32);
        s[2] = s[2].max(x1 as i32 - 1);
        s[3] = s[3].max(y as i32);
        s[4] += (x1 - x0) as i32;
        ink += (x1 - x0) as i64;
    }
    // The background: per row, its first and last pixels lie just outside
    // the first and last runs.
    let bg = &mut stats[0];
    for y in 0..h {
        let row_runs = &runs[row_start[y]..row_start[y + 1]];
        let first_bg = match row_runs.first() {
            Some(r) if r.x0 == 0 => r.x1 as usize,
            _ => 0,
        };
        if first_bg >= w {
            continue; // the whole row is ink
        }
        let last_bg = match row_runs.last() {
            Some(r) if r.x1 as usize == w => r.x0 as usize - 1,
            _ => w - 1,
        };
        bg[0] = bg[0].min(first_bg as i32);
        bg[1] = bg[1].min(y as i32);
        bg[2] = bg[2].max(last_bg as i32);
        bg[3] = bg[3].max(y as i32);
    }
    bg[4] = ((w * h) as i64 - ink) as i32;
    for s in &mut stats {
        if s[4] == 0 {
            // A label with no pixels - only the background of an
            // all-foreground mask - reads as OpenCV 5 reports it.
            *s = [-1, i32::MAX, 0, 0, 0];
        } else {
            s[2] = s[2] - s[0] + 1;
            s[3] = s[3] - s[1] + 1;
        }
    }
    (labels, stats)
}
