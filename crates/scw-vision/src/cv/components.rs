//! `cv::connectedComponentsWithStats`, with OpenCV's label order.
//!
//! The order is what matters for byte parity, and it follows from how OpenCV
//! scans: its 8-connectivity algorithm (Spaghetti, after BBDT) walks 2x2
//! blocks in raster order and gives a component the label created at its
//! first block; its 4-connectivity one walks single pixels. So label 1 is the
//! component met first in that scan, label 2 the next, and so on. Within a
//! 2x2 block every foreground pixel is 8-connected to every other, so no two
//! 8-connected components share a block, and the order is total.

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
    let n = w * h;
    // Union-find over pixel indices; background pixels never join anything.
    let mut parent: Vec<u32> = (0..n as u32).collect();
    let fg = |x: usize, y: usize| mask.data[y * w + x] != 0;
    for y in 0..h {
        for x in 0..w {
            if !fg(x, y) {
                continue;
            }
            let i = (y * w + x) as u32;
            if x > 0 && fg(x - 1, y) {
                union(&mut parent, i, i - 1);
            }
            if y > 0 {
                if fg(x, y - 1) {
                    union(&mut parent, i, i - w as u32);
                }
                if connectivity == 8 {
                    if x > 0 && fg(x - 1, y - 1) {
                        union(&mut parent, i, i - w as u32 - 1);
                    }
                    if x + 1 < w && fg(x + 1, y - 1) {
                        union(&mut parent, i, i - w as u32 + 1);
                    }
                }
            }
        }
    }
    // Each component's place in OpenCV's scan: its first pixel (4) or its
    // first 2x2 block (8), as a raster index.
    let mut first: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if !fg(x, y) {
                continue;
            }
            let root = find(&mut parent, (y * w + x) as u32);
            let key = if connectivity == 8 {
                ((y / 2) as u64) * (w as u64) + (x / 2) as u64
            } else {
                (y * w + x) as u64
            };
            first
                .entry(root)
                .and_modify(|k| *k = (*k).min(key))
                .or_insert(key);
        }
    }
    let mut roots: Vec<(u64, u32)> = first.into_iter().map(|(r, k)| (k, r)).collect();
    roots.sort_unstable();
    let mut label_of = std::collections::HashMap::with_capacity(roots.len());
    for (label, &(_, root)) in roots.iter().enumerate() {
        label_of.insert(root, label as i32 + 1);
    }
    let count = roots.len() + 1;
    // left/top as minimums, right/bottom as maximums, then widths.
    let mut stats: Vec<Stats> = vec![[i32::MAX, i32::MAX, i32::MIN, i32::MIN, 0]; count];
    let mut labels = Image::<i32>::new(w, h, 1);
    for y in 0..h {
        for x in 0..w {
            let l = if fg(x, y) {
                label_of[&find(&mut parent, (y * w + x) as u32)]
            } else {
                0
            };
            labels.data[y * w + x] = l;
            let s = &mut stats[l as usize];
            s[0] = s[0].min(x as i32);
            s[1] = s[1].min(y as i32);
            s[2] = s[2].max(x as i32);
            s[3] = s[3].max(y as i32);
            s[4] += 1;
        }
    }
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
