//! `cv::fillPoly(mask, [pts], 1)` on an 8-bit mask - lineType 8, shift 0 -
//! ported from OpenCV 5.0's drawing.cpp: the outline drawn as 8-connected
//! lines (LineIterator, clipLine), then the inside filled scanline by
//! scanline from the edges in 16.16 fixed point (CollectPolyEdges,
//! FillEdgeCollection). RapidOCR's box score is the mean of the detector's
//! map under this mask.

const XY_SHIFT: i64 = 16;
const XY_ONE: i64 = 1 << XY_SHIFT;

/// clipLine(Size2l, pt1, pt2): false when the line misses the image.
fn clip_line(w: i64, h: i64, p1: &mut (i64, i64), p2: &mut (i64, i64)) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    let (right, bottom) = (w - 1, h - 1);
    let (x1, y1) = (&mut p1.0, &mut p1.1);
    let (x2, y2) = (&mut p2.0, &mut p2.1);
    let code = |x: i64, y: i64| {
        (x < 0) as i32 + (x > right) as i32 * 2 + (y < 0) as i32 * 4 + (y > bottom) as i32 * 8
    };
    let mut c1 = code(*x1, *y1);
    let mut c2 = code(*x2, *y2);
    if (c1 & c2) == 0 && (c1 | c2) != 0 {
        if c1 & 12 != 0 {
            let a = if c1 < 8 { 0 } else { bottom };
            *x1 += ((a - *y1) as f64 * (*x2 - *x1) as f64 / (*y2 - *y1) as f64) as i64;
            *y1 = a;
            c1 = (*x1 < 0) as i32 + (*x1 > right) as i32 * 2;
        }
        if c2 & 12 != 0 {
            let a = if c2 < 8 { 0 } else { bottom };
            *x2 += ((a - *y2) as f64 * (*x2 - *x1) as f64 / (*y2 - *y1) as f64) as i64;
            *y2 = a;
            c2 = (*x2 < 0) as i32 + (*x2 > right) as i32 * 2;
        }
        if (c1 & c2) == 0 && (c1 | c2) != 0 {
            if c1 != 0 {
                let a = if c1 == 1 { 0 } else { right };
                *y1 += ((a - *x1) as f64 * (*y2 - *y1) as f64 / (*x2 - *x1) as f64) as i64;
                *x1 = a;
                c1 = 0;
            }
            if c2 != 0 {
                let a = if c2 == 1 { 0 } else { right };
                *y2 += ((a - *x2) as f64 * (*y2 - *y1) as f64 / (*x2 - *x1) as f64) as i64;
                *x2 = a;
                c2 = 0;
            }
        }
    }
    (c1 | c2) == 0
}

/// Line(img, pt1, pt2, color, 8): LineIterator(img, pt1, pt2, 8, true).
fn line(mask: &mut [u8], w: usize, h: usize, p1: (i64, i64), p2: (i64, i64), v: u8) {
    let (mut p1, mut p2) = (p1, p2);
    let out = |p: (i64, i64)| (p.0 as u64) >= w as u64 || (p.1 as u64) >= h as u64;
    if (out(p1) || out(p2)) && !clip_line(w as i64, h as i64, &mut p1, &mut p2) {
        return;
    }
    let (mut dx, mut dy) = (p2.0 - p1.0, p2.1 - p1.1);
    let (mut delta_x, mut delta_y) = (1i64, 1i64);
    let mut start = p1;
    if dx < 0 {
        // leftToRight
        dx = -dx;
        dy = -dy;
        start = p2;
    }
    if dy < 0 {
        dy = -dy;
        delta_y = -1;
    }
    let vert = dy > dx;
    if vert {
        std::mem::swap(&mut dx, &mut dy);
        std::mem::swap(&mut delta_x, &mut delta_y);
    }
    let mut err = dx - (dy + dy);
    let plus_delta = dx + dx;
    let minus_delta = -(dy + dy);
    let (mut minus_shift, mut plus_shift) = (delta_x, 0i64);
    let (mut minus_step, mut plus_step) = (0i64, delta_y);
    let count = dx + 1;
    if vert {
        std::mem::swap(&mut plus_step, &mut plus_shift);
        std::mem::swap(&mut minus_step, &mut minus_shift);
    }
    // pointer steps in pixels: step*w + shift
    let (mut x, mut y) = start;
    for _ in 0..count {
        mask[y as usize * w + x as usize] = v;
        let m = if err < 0 { -1i64 } else { 0 };
        err += minus_delta + (plus_delta & m);
        x += minus_shift + (plus_shift & m);
        y += minus_step + (plus_step & m);
    }
}

#[derive(Clone, Copy)]
struct Edge {
    y0: i32,
    y1: i32,
    x: i64,
    dx: i64,
    next: usize, // index; NONE for none
}
const NONE: usize = usize::MAX;

/// `cv::fillPoly(mask, [pts], v)` with lineType 8 and shift 0.
pub fn fill_poly(mask: &mut [u8], w: usize, h: usize, pts: &[(i32, i32)], v: u8) {
    let count = pts.len();
    if count == 0 {
        return;
    }
    let mut edges: Vec<Edge> = Vec::with_capacity(count + 1);
    let mut pt0 = (pts[count - 1].0 as i64) << XY_SHIFT;
    let mut pt0y = pts[count - 1].1 as i64;
    for &(px, py) in pts {
        let pt1 = (px as i64) << XY_SHIFT;
        let pt1y = py as i64;
        let (mut p0c, mut p1c) = ((pt0, pt0y), (pt1, pt1y));
        let mut t0 = ((pt0 + (XY_ONE >> 1)) >> XY_SHIFT, pt0y);
        let mut t1 = ((pt1 + (XY_ONE >> 1)) >> XY_SHIFT, pt1y);
        line(mask, w, h, t0, t1, v);
        let out = |p: (i64, i64)| (p.0 as u64) >= w as u64 || (p.1 as u64) >= h as u64;
        if out(t0) || out(t1) {
            clip_line(w as i64, h as i64, &mut t0, &mut t1);
            if t0.1 != t1.1 {
                p0c.1 = t0.1;
                p1c.1 = t1.1;
            }
        }
        p0c.0 = t0.0 << XY_SHIFT;
        p1c.0 = t1.0 << XY_SHIFT;
        if pt0y != pt1y {
            let dx = (p1c.0 - p0c.0) / (p1c.1 - p0c.1);
            let e = if pt0y < pt1y {
                Edge {
                    y0: pt0y as i32,
                    y1: pt1y as i32,
                    x: p0c.0 + (pt0y - p0c.1) * dx,
                    dx,
                    next: NONE,
                }
            } else {
                Edge {
                    y0: pt1y as i32,
                    y1: pt0y as i32,
                    x: p1c.0 + (pt1y - p1c.1) * dx,
                    dx,
                    next: NONE,
                }
            };
            edges.push(e);
        }
        pt0 = pt1;
        pt0y = pt1y;
    }
    fill_edges(mask, w, h, &mut edges, v);
}

fn fill_edges(mask: &mut [u8], w: usize, h: usize, edges: &mut Vec<Edge>, v: u8) {
    let total = edges.len();
    if total < 2 {
        return;
    }
    let (mut y_max, mut y_min) = (i32::MIN, i32::MAX);
    let (mut x_max, mut x_min) = (-1i64, i64::MAX);
    for e in edges.iter() {
        let x1 = e.x + (e.y1 - e.y0) as i64 * e.dx;
        y_min = y_min.min(e.y0);
        y_max = y_max.max(e.y1);
        x_min = x_min.min(e.x).min(x1);
        x_max = x_max.max(e.x).max(x1);
    }
    if y_max < 0 || y_min >= h as i32 || x_max < 0 || x_min >= (w as i64) << XY_SHIFT {
        return;
    }
    // CmpEdges: y0, then x, then dx (std::sort; ties of all three are
    // identical edges for drawing purposes)
    edges.sort_by(|a, b| a.y0.cmp(&b.y0).then(a.x.cmp(&b.x)).then(a.dx.cmp(&b.dx)));
    edges.push(Edge {
        y0: i32::MAX,
        y1: 0,
        x: 0,
        dx: 0,
        next: NONE,
    });
    let tmp = edges.len(); // the list head lives past the end
    let mut head_next = NONE;
    let get_next = |edges: &Vec<Edge>, head_next: usize, i: usize| {
        if i == tmp { head_next } else { edges[i].next }
    };
    let mut i = 0usize;
    let mut e = 0usize;
    let y_max = y_max.min(h as i32);
    let delta = XY_ONE - 1;
    let mut y = edges[e].y0;
    while y < y_max {
        let mut draw = false;
        let clipline = y < 0;
        let mut prelast = tmp;
        let mut last = head_next;
        loop {
            if !(last != NONE || edges[e].y0 == y) {
                break;
            }
            if last != NONE && edges[last].y1 == y {
                // drop the edge that ends here
                let n = edges[last].next;
                if prelast == tmp {
                    head_next = n
                } else {
                    edges[prelast].next = n
                }
                last = n;
                continue;
            }
            let keep_prelast = prelast;
            if last != NONE && (edges[e].y0 > y || edges[last].x < edges[e].x) {
                prelast = last;
                last = edges[last].next;
            } else if i < total {
                if prelast == tmp {
                    head_next = e
                } else {
                    edges[prelast].next = e
                }
                edges[e].next = last;
                prelast = e;
                i += 1;
                e = i;
            } else {
                break;
            }
            if draw {
                if !clipline {
                    let (kx, px) = (
                        if keep_prelast == tmp {
                            0
                        } else {
                            edges[keep_prelast].x
                        },
                        edges[prelast].x,
                    );
                    let (mut x1, mut x2) = if kx > px {
                        (((px + delta) >> XY_SHIFT) as i32, (kx >> XY_SHIFT) as i32)
                    } else {
                        (((kx + delta) >> XY_SHIFT) as i32, (px >> XY_SHIFT) as i32)
                    };
                    if x1 < w as i32 && x2 >= 0 {
                        x1 = x1.max(0);
                        x2 = x2.min(w as i32 - 1);
                        let row = y as usize * w;
                        for x in x1..=x2 {
                            mask[row + x as usize] = v;
                        }
                    }
                }
                if keep_prelast != tmp {
                    let d = edges[keep_prelast].dx;
                    edges[keep_prelast].x += d;
                }
                let d = edges[prelast].dx;
                edges[prelast].x += d;
            }
            draw = !draw;
        }
        // bubble sort of the active list by x
        let mut keep_prelast = NONE;
        loop {
            let mut prelast = tmp;
            let mut last = head_next;
            let mut last_exchange = NONE;
            while last != keep_prelast && last != NONE && edges[last].next != NONE {
                let te = edges[last].next;
                if edges[last].x > edges[te].x {
                    if prelast == tmp {
                        head_next = te
                    } else {
                        edges[prelast].next = te
                    }
                    edges[last].next = edges[te].next;
                    edges[te].next = last;
                    prelast = te;
                    last_exchange = prelast;
                } else {
                    prelast = last;
                    last = te;
                }
            }
            if last_exchange == NONE {
                break;
            }
            keep_prelast = last_exchange;
            if keep_prelast == get_next(edges, head_next, tmp) || keep_prelast == tmp {
                break;
            }
        }
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(w: usize, h: usize, pts: &[(i32, i32)]) -> Vec<u8> {
        let mut m = vec![0u8; w * h];
        fill_poly(&mut m, w, h, pts, 1);
        m
    }

    #[test]
    fn a_rectangle_is_filled_edges_included() {
        let m = filled(8, 6, &[(1, 1), (5, 1), (5, 4), (1, 4)]);
        for y in 0..6 {
            for x in 0..8 {
                let inside = (1..=5).contains(&x) && (1..=4).contains(&y);
                assert_eq!(m[y * 8 + x], inside as u8, "({x},{y})");
            }
        }
    }
}
