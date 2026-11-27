//! The geometry RapidOCR's detector leans on, ported from the code it runs:
//! OpenCV 5.0's `convexHull` (Sklansky), `minAreaRect` (rotating calipers)
//! and `RotatedRect::points`; Clipper 6.4.2's polygon offset with round
//! joins (pyclipper 1.4.0, the "unclip"); shapely's area and length; and
//! RapidOCR's own box ordering.

/// A point with float coordinates (OpenCV's Point2f).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct P32 {
    pub x: f32,
    pub y: f32,
}

/// OpenCV's RotatedRect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotatedRect {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    pub angle: f32,
}

fn sign_i64(v: i64) -> i32 {
    (v > 0) as i32 - (v < 0) as i32
}

/// `Sklansky_<int, int64>` over sorted points (indices into `pts`).
fn sklansky(
    pts: &[(i32, i32)],
    order: &[usize],
    start: isize,
    end: isize,
    stack: &mut [usize],
    nsign: i32,
    sign2: i32,
) -> usize {
    let at = |i: isize| pts[order[i as usize]];
    let incr: isize = if end > start { 1 } else { -1 };
    let (mut pprev, mut pcur) = (start, start + incr);
    let mut pnext = pcur + incr;
    let mut stacksize = 3usize;
    if start == end || at(start) == at(end) {
        stack[0] = start as usize;
        return 1;
    }
    stack[0] = pprev as usize;
    stack[1] = pcur as usize;
    stack[2] = pnext as usize;
    let end = end + incr;
    while pnext != end {
        let cury = at(pcur).1;
        let nexty = at(pnext).1;
        let by = nexty - cury;
        if sign_i64(by as i64) != nsign {
            let a = (at(pcur).0 - at(pprev).0, cury - at(pprev).1);
            let b = (at(pnext).0 - at(pcur).0, by);
            let convexity = a.1 as i64 * b.0 as i64 - a.0 as i64 * b.1 as i64;
            if sign_i64(convexity) == sign2 && (a.0 != 0 || a.1 != 0) {
                pprev = pcur;
                pcur = pnext;
                pnext += incr;
                stack[stacksize] = pnext as usize;
                stacksize += 1;
            } else if pprev == start {
                pcur = pnext;
                stack[1] = pcur as usize;
                pnext += incr;
                stack[2] = pnext as usize;
            } else {
                stack[stacksize - 2] = pnext as usize;
                pcur = pprev;
                pprev = stack[stacksize - 4] as isize;
                stacksize -= 1;
            }
        } else {
            pnext += incr;
            stack[stacksize - 1] = pnext as usize;
        }
    }
    stacksize - 1
}

/// `cv::convexHull(points, hull, clockwise=false, returnPoints=true)` for
/// integer points, in OpenCV's order.
pub fn convex_hull(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let total = pts.len();
    if total == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..total).collect();
    // By x, then y, then position (CHullCmpPoints compares the pointers).
    order.sort_by(|&a, &b| pts[a].cmp(&pts[b]).then(a.cmp(&b)));
    let (mut miny, mut maxy) = (0usize, 0usize);
    for i in 1..total {
        let y = pts[order[i]].1;
        if pts[order[miny]].1 > y {
            miny = i;
        }
        if pts[order[maxy]].1 < y {
            maxy = i;
        }
    }
    let mut hull: Vec<usize> = Vec::with_capacity(total);
    let mut stack = vec![0usize; total + 2];
    if pts[order[0]] == pts[order[total - 1]] {
        hull.push(0);
    } else {
        // upper half
        let tl_count = sklansky(pts, &order, 0, maxy as isize, &mut stack, -1, 1);
        let tl: Vec<usize> = stack[..tl_count].to_vec();
        let tr_count = sklansky(
            pts,
            &order,
            total as isize - 1,
            maxy as isize,
            &mut stack[tl_count..],
            -1,
            -1,
        );
        let tr: Vec<usize> = stack[tl_count..tl_count + tr_count].to_vec();
        // !clockwise: swap
        let (tl, tr) = (tr, tl);
        let (tl_count, tr_count) = (tr_count, tl_count);
        for &v in &tl[..tl_count - 1] {
            hull.push(v);
        }
        for i in (1..tr_count).rev() {
            hull.push(tr[i]);
        }
        let stop_idx: isize = if tr_count > 2 {
            tr[1] as isize
        } else if tl_count > 2 {
            tl[tl_count - 2] as isize
        } else {
            -1
        };
        // lower half
        let mut bl_count = sklansky(pts, &order, 0, miny as isize, &mut stack, 1, -1);
        let bl: Vec<usize> = stack[..bl_count].to_vec();
        let mut br_count = sklansky(
            pts,
            &order,
            total as isize - 1,
            miny as isize,
            &mut stack[bl_count..],
            1,
            1,
        );
        let br: Vec<usize> = stack[bl_count..bl_count + br_count].to_vec();
        if stop_idx >= 0 {
            let check_idx: isize = if bl_count > 2 {
                bl[1] as isize
            } else if bl_count + br_count > 2 {
                br[2 - bl_count] as isize
            } else {
                -1
            };
            if check_idx == stop_idx
                || (check_idx >= 0
                    && pts[order[check_idx as usize]] == pts[order[stop_idx as usize]])
            {
                bl_count = bl_count.min(2);
                br_count = br_count.min(2);
            }
        }
        for &v in &bl[..bl_count - 1] {
            hull.push(v);
        }
        for i in (1..br_count).rev() {
            hull.push(br[i]);
        }
    }
    // to indices of the input
    let mut idx: Vec<usize> = hull.iter().map(|&i| order[i]).collect();
    let nout = idx.len();
    // the cyclic shift that makes the indices ascend or descend
    if nout >= 3 {
        let (mut min_idx, mut max_idx, mut lt) = (0usize, 0usize, 0usize);
        for i in 1..nout {
            let v = idx[i];
            lt += (idx[i - 1] < v) as usize;
            if lt > 1 && lt + 2 <= i {
                break;
            }
            if v < idx[min_idx] {
                min_idx = i;
            }
            if v > idx[max_idx] {
                max_idx = i;
            }
        }
        let mmdist = (max_idx as isize - min_idx as isize).unsigned_abs();
        if (mmdist == 1 || mmdist == nout - 1) && (lt <= 1 || lt + 2 >= nout) {
            let ascending = (max_idx + 1) % nout == min_idx;
            let i0 = if ascending { min_idx } else { max_idx };
            if i0 > 0 {
                let mut out = vec![0usize; nout];
                let mut j = i0;
                let mut i = 0;
                while i < nout {
                    let curr = idx[j];
                    out[i] = curr;
                    let next_j = if j + 1 < nout { j + 1 } else { 0 };
                    let next = idx[next_j];
                    if i < nout - 1 && (ascending != (curr < next)) {
                        break;
                    }
                    j = next_j;
                    i += 1;
                }
                if i == nout {
                    idx = out;
                }
            }
        }
    }
    idx.iter().map(|&i| pts[i]).collect()
}

fn rotate90cw(v: P32) -> P32 {
    P32 { x: v.y, y: -v.x }
}
fn rotate90ccw(v: P32) -> P32 {
    P32 { x: -v.y, y: v.x }
}
fn rotate180(v: P32) -> P32 {
    P32 { x: -v.x, y: -v.y }
}
fn first_vec_is_right(v1: P32, v2: P32) -> bool {
    let t = rotate90cw(v1);
    t.x * v2.x + t.y * v2.y < 0.0
}

/// `rotatingCalipers(..., CALIPERS_MINAREARECT)`: corner, vector1, vector2.
fn rotating_calipers(points: &[P32], orientation: f32) -> [P32; 3] {
    let n = points.len();
    let mut minarea = f32::MAX;
    let mut vect = vec![P32 { x: 0.0, y: 0.0 }; n];
    let mut inv = vec![0f32; n];
    let (mut left, mut bottom, mut right, mut top) = (0usize, 0usize, 0usize, 0usize);
    let mut pt0 = points[0];
    let (mut left_x, mut right_x) = (pt0.x, pt0.x);
    let (mut top_y, mut bottom_y) = (pt0.y, pt0.y);
    for i in 0..n {
        if pt0.x < left_x {
            left_x = pt0.x;
            left = i;
        }
        if pt0.x > right_x {
            right_x = pt0.x;
            right = i;
        }
        if pt0.y > top_y {
            top_y = pt0.y;
            top = i;
        }
        if pt0.y < bottom_y {
            bottom_y = pt0.y;
            bottom = i;
        }
        let pt = points[if i + 1 < n { i + 1 } else { 0 }];
        // float differences, widened (C++: double dx = pt.x - pt0.x)
        let dx = (pt.x - pt0.x) as f64;
        let dy = (pt.y - pt0.y) as f64;
        vect[i] = P32 {
            x: dx as f32,
            y: dy as f32,
        };
        inv[i] = (1.0 / (dx * dx + dy * dy).sqrt()) as f32;
        pt0 = pt;
    }
    let _ = (left_x, right_x, top_y, bottom_y);
    // OpenCV starts the base at (orientation, 0); the first edge always
    // replaces it before it is read.
    let _ = orientation;
    let (mut base_a, mut base_b): (f32, f32);
    let mut seq = [bottom, right, top, left];
    // buffer: [leftist idx, base_a, width, base_b, height, bottom idx, area]
    let (mut b_left, mut b_a, mut b_w, mut b_b, mut b_h, mut b_bottom) =
        (0usize, 0f32, 0f32, 0f32, 0f32, 0usize);
    for _ in 0..n {
        let rot = [
            vect[seq[0]],
            rotate90cw(vect[seq[1]]),
            rotate180(vect[seq[2]]),
            rotate90ccw(vect[seq[3]]),
        ];
        let mut main = 0usize;
        for (i, r) in rot.iter().enumerate().skip(1) {
            if first_vec_is_right(*r, rot[main]) {
                main = i;
            }
        }
        let p = seq[main];
        let lead_x = vect[p].x * inv[p];
        let lead_y = vect[p].y * inv[p];
        match main {
            0 => {
                base_a = lead_x;
                base_b = lead_y;
            }
            1 => {
                base_a = lead_y;
                base_b = -lead_x;
            }
            2 => {
                base_a = -lead_x;
                base_b = -lead_y;
            }
            _ => {
                base_a = -lead_y;
                base_b = lead_x;
            }
        }
        seq[main] += 1;
        if seq[main] == n {
            seq[main] = 0;
        }
        let dx = points[seq[1]].x - points[seq[3]].x;
        let dy = points[seq[1]].y - points[seq[3]].y;
        let width = dx * base_a + dy * base_b;
        let dx = points[seq[2]].x - points[seq[0]].x;
        let dy = points[seq[2]].y - points[seq[0]].y;
        let height = -dx * base_b + dy * base_a;
        let area = width * height;
        if area <= minarea {
            minarea = area;
            b_left = seq[3];
            b_a = base_a;
            b_w = width;
            b_b = base_b;
            b_h = height;
            b_bottom = seq[0];
        }
    }
    let (a1, b1) = (b_a, b_b);
    let (a2, b2) = (-b_b, b_a);
    let c1 = a1 * points[b_left].x + points[b_left].y * b1;
    let c2 = a2 * points[b_bottom].x + points[b_bottom].y * b2;
    let idet = 1.0f32 / (a1 * b2 - a2 * b1);
    let px = (c1 * b2 - c2 * b1) * idet;
    let py = (a1 * c2 - a2 * c1) * idet;
    [
        P32 { x: px, y: py },
        P32 {
            x: a1 * b_w,
            y: b1 * b_w,
        },
        P32 {
            x: a2 * b_h,
            y: b2 * b_h,
        },
    ]
}

/// `cv::minAreaRect` of integer points.
pub fn min_area_rect(pts: &[(i32, i32)]) -> RotatedRect {
    let hull: Vec<P32> = convex_hull(pts)
        .iter()
        .map(|&(x, y)| P32 {
            x: x as f32,
            y: y as f32,
        })
        .collect();
    let n = hull.len();
    let mut angle = -std::f64::consts::PI / 2.0;
    let mut r = RotatedRect {
        cx: 0.0,
        cy: 0.0,
        w: 0.0,
        h: 0.0,
        angle: 0.0,
    };
    if n > 2 {
        let out = rotating_calipers(&hull, 1.0);
        r.cx = out[0].x + (out[1].x + out[2].x) * 0.5;
        r.cy = out[0].y + (out[1].y + out[2].y) * 0.5;
        r.w = (out[2].x as f64 * out[2].x as f64 + out[2].y as f64 * out[2].y as f64).sqrt() as f32;
        r.h = (out[1].x as f64 * out[1].x as f64 + out[1].y as f64 * out[1].y as f64).sqrt() as f32;
        if out[1].x == 0.0 && out[1].y > 0.0 {
            std::mem::swap(&mut r.w, &mut r.h);
        } else {
            angle = -(out[1].x as f64).atan2(out[1].y as f64);
        }
    } else if n == 2 {
        r.cx = (hull[0].x + hull[1].x) * 0.5;
        r.cy = (hull[0].y + hull[1].y) * 0.5;
        let dx = hull[0].x as f64 - hull[1].x as f64;
        let dy = hull[0].y as f64 - hull[1].y as f64;
        r.w = 0.0;
        r.h = (dx * dx + dy * dy).sqrt() as f32;
        if dx == 0.0 {
            std::mem::swap(&mut r.w, &mut r.h);
        } else if dy < 0.0 {
            angle = dy.atan2(dx);
            std::mem::swap(&mut r.w, &mut r.h);
        } else if dy > 0.0 {
            angle = -dx.atan2(dy);
        }
    } else if n == 1 {
        r.cx = hull[0].x;
        r.cy = hull[0].y;
    }
    r.angle = (angle * 180.0 / std::f64::consts::PI) as f32;
    r
}

/// `RotatedRect::points`.
pub fn box_points(r: &RotatedRect) -> [P32; 4] {
    let a_ = r.angle as f64 * std::f64::consts::PI / 180.0;
    let b = (a_.cos() as f32) * 0.5;
    let a = (a_.sin() as f32) * 0.5;
    let ah = a * r.h;
    let aw = a * r.w;
    let bh = b * r.h;
    let bw = b * r.w;
    [
        P32 {
            x: r.cx - ah - bw,
            y: r.cy + bh - aw,
        },
        P32 {
            x: r.cx + ah - bw,
            y: r.cy - bh - aw,
        },
        P32 {
            x: r.cx + ah + bw,
            y: r.cy - bh + aw,
        },
        P32 {
            x: r.cx - ah + bw,
            y: r.cy + bh + aw,
        },
    ]
}

/// DBPostProcess.get_mini_boxes: the min-area box's corners, left pair then
/// right pair, each top first - and its shorter side.
pub fn mini_box(pts: &[(i32, i32)]) -> ([P32; 4], f32) {
    let r = min_area_rect(pts);
    let mut p = box_points(&r);
    // sorted(key=x): Python's sort is stable
    p.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    let (i1, i4) = if p[1].y > p[0].y { (0, 1) } else { (1, 0) };
    let (i2, i3) = if p[3].y > p[2].y { (2, 3) } else { (3, 2) };
    ([p[i1], p[i2], p[i3], p[i4]], r.w.min(r.h))
}

/// shapely's Polygon(box).area and .length.
pub fn area_length(b: &[P32; 4]) -> (f64, f64) {
    let mut a = 0f64;
    let mut l = 0f64;
    for i in 0..4 {
        let (p, q) = (b[i], b[(i + 1) % 4]);
        a += p.x as f64 * q.y as f64 - q.x as f64 * p.y as f64;
        let (dx, dy) = (q.x as f64 - p.x as f64, q.y as f64 - p.y as f64);
        l += (dx * dx + dy * dy).sqrt();
    }
    (a.abs() * 0.5, l)
}

/// Clipper's Round.
fn cround(v: f64) -> i64 {
    if v < 0.0 {
        (v - 0.5) as i64
    } else {
        (v + 0.5) as i64
    }
}

fn clipper_area(p: &[(i64, i64)]) -> f64 {
    if p.len() < 3 {
        return 0.0;
    }
    let mut a = 0f64;
    let mut j = p.len() - 1;
    for i in 0..p.len() {
        a += (p[j].0 as f64 + p[i].0 as f64) * (p[j].1 as f64 - p[i].1 as f64);
        j = i;
    }
    -a * 0.5
}

fn unit_normal(a: (i64, i64), b: (i64, i64)) -> (f64, f64) {
    if a == b {
        return (0.0, 0.0);
    }
    let mut dx = (b.0 - a.0) as f64;
    let mut dy = (b.1 - a.1) as f64;
    let f = 1.0 / (dx * dx + dy * dy).sqrt();
    dx *= f;
    dy *= f;
    (dy, -dx)
}

/// pyclipper's `PyclipperOffset` (miter 2, arc tolerance 0.25) on one
/// closed polygon with round joins, `Execute(delta)`: the offset polygon's
/// points (Clipper's union afterwards only drops duplicate and collinear
/// points, which the hull that follows ignores). The corners are truncated
/// to integers on the way in, as pyclipper converts them.
pub fn offset_round(b: &[P32; 4], delta: f64) -> Vec<(i64, i64)> {
    let path: Vec<(i64, i64)> = b.iter().map(|p| (p.x as i64, p.y as i64)).collect();
    // AddPath: strip a repeated last point, drop repeats
    let mut high = path.len() - 1;
    while high > 0 && path[0] == path[high] {
        high -= 1;
    }
    let mut src = vec![path[0]];
    let mut k = 0usize;
    for &p in &path[1..=high] {
        if *src.last().unwrap() != p {
            src.push(p);
            let j = src.len() - 1;
            if p.1 > src[k].1 || (p.1 == src[k].1 && p.0 < src[k].0) {
                k = j;
            }
        }
    }
    let _ = k;
    if src.len() < 3 {
        return Vec::new();
    }
    // FixOrientations: one polygon, so it is the lowest
    if clipper_area(&src) < 0.0 {
        src.reverse();
    }
    if delta.abs() < 1.0e-20 {
        return src;
    }
    let arc_tol = 0.25f64;
    let y = if arc_tol > delta.abs() * 0.25 {
        delta.abs() * 0.25
    } else {
        arc_tol
    };
    let pi = std::f64::consts::PI;
    let mut steps = pi / (1.0 - y / delta.abs()).acos();
    if steps > delta.abs() * pi {
        steps = delta.abs() * pi;
    }
    let mut m_sin = (2.0 * pi / steps).sin();
    let m_cos = (2.0 * pi / steps).cos();
    let steps_per_rad = steps / (2.0 * pi);
    if delta < 0.0 {
        m_sin = -m_sin;
    }
    let len = src.len();
    let mut normals: Vec<(f64, f64)> = (0..len - 1)
        .map(|j| unit_normal(src[j], src[j + 1]))
        .collect();
    normals.push(unit_normal(src[len - 1], src[0]));
    let mut dest: Vec<(i64, i64)> = Vec::new();
    let mut k = len - 1;
    for j in 0..len {
        let (nk, nj) = (normals[k], normals[j]);
        let mut sin_a = nk.0 * nj.1 - nj.0 * nk.1;
        let p = src[j];
        let off = |n: (f64, f64)| {
            (
                cround(p.0 as f64 + n.0 * delta),
                cround(p.1 as f64 + n.1 * delta),
            )
        };
        if (sin_a * delta).abs() < 1.0 {
            let cos_a = nk.0 * nj.0 + nj.1 * nk.1;
            if cos_a > 0.0 {
                // returns before `k = j`, as Clipper's OffsetPoint does
                dest.push(off(nk));
                continue;
            }
        } else {
            // Clipper: else if (m_sinA > 1.0) ... else if (m_sinA < -1.0)
            sin_a = sin_a.clamp(-1.0, 1.0);
        }
        if sin_a * delta < 0.0 {
            dest.push(off(nk));
            dest.push(p);
            dest.push(off(nj));
        } else {
            // DoRound
            let a = sin_a.atan2(nk.0 * nj.0 + nk.1 * nj.1);
            let n = (cround(steps_per_rad * a.abs()) as i32).max(1);
            let (mut x, mut yy) = nk;
            for _ in 0..n {
                dest.push((
                    cround(p.0 as f64 + x * delta),
                    cround(p.1 as f64 + yy * delta),
                ));
                let x2 = x;
                x = x * m_cos - m_sin * yy;
                yy = x2 * m_sin + yy * m_cos;
            }
            dest.push(off(nj));
        }
        k = j;
    }
    dest
}

/// RapidOCR's order_points_clockwise: [top-left, top-right, bottom-right,
/// bottom-left] by x, then y within each side (numpy's sorts are stable on
/// four points).
pub fn order_clockwise(b: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    let mut xs = b;
    xs.sort_by(|p, q| p[0].partial_cmp(&q[0]).unwrap_or(std::cmp::Ordering::Equal));
    let mut left = [xs[0], xs[1]];
    let mut right = [xs[2], xs[3]];
    left.sort_by(|p, q| p[1].partial_cmp(&q[1]).unwrap_or(std::cmp::Ordering::Equal));
    right.sort_by(|p, q| p[1].partial_cmp(&q[1]).unwrap_or(std::cmp::Ordering::Equal));
    [left[0], right[0], right[1], left[1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upright_rectangle_is_its_own_box() {
        let pts: Vec<(i32, i32)> = (10..=20).flat_map(|x| [(x, 5), (x, 9)]).collect();
        let (b, side) = mini_box(&pts);
        assert_eq!(side, 4.0);
        let xs: Vec<(f32, f32)> = b.iter().map(|p| (p.x, p.y)).collect();
        assert_eq!(xs, vec![(10.0, 5.0), (20.0, 5.0), (20.0, 9.0), (10.0, 9.0)]);
    }

    #[test]
    fn the_unclip_matches_pyclipper() {
        // pyclipper 1.4.0, measured: a 10x4 box offset by 2.2857 spans
        // x 8..22, y 3..11 in 8 points; 10.7 truncates to 10 first.
        for start in [10.0f32, 10.7] {
            let b = [
                P32 {
                    x: start,
                    y: start - 5.0,
                },
                P32 {
                    x: start + 10.0,
                    y: start - 5.0,
                },
                P32 {
                    x: start + 10.0,
                    y: start - 1.0,
                },
                P32 {
                    x: start,
                    y: start - 1.0,
                },
            ];
            let (a, l) = area_length(&b);
            let e = offset_round(&b, a * 1.6 / l);
            // pyclipper's points, all among ours (Clipper's union then
            // drops the four that sit on straight edges; the hull ignores
            // them)
            for p in [
                (22, 4),
                (22, 9),
                (21, 11),
                (10, 11),
                (8, 10),
                (8, 5),
                (9, 3),
                (20, 3),
            ] {
                assert!(e.contains(&p), "{p:?} in {e:?}");
            }
            assert_eq!(e.len(), 12);
            let (minx, maxx) = (
                e.iter().map(|p| p.0).min().unwrap(),
                e.iter().map(|p| p.0).max().unwrap(),
            );
            let (miny, maxy) = (
                e.iter().map(|p| p.1).min().unwrap(),
                e.iter().map(|p| p.1).max().unwrap(),
            );
            assert_eq!((minx, maxx, miny, maxy), (8, 22, 3, 11));
        }
    }
}
