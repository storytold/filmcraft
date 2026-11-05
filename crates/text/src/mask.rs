//! Coverage-mask operations used for text and shape appearance: strokes (outer / centre / inner)
//! from an exact Euclidean distance transform, Gaussian-like blur for shadows, offsets.

use crate::raster::Mask;

/// Where a stroke sits relative to the shape's edge (Premiere's stroke types).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StrokeKind {
    #[default]
    Outer,
    Center,
    Inner,
}

const INF: f32 = 1e20;

/// 1-D squared distance transform (Felzenszwalb & Huttenlocher) of `f` into `d`.
fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = -INF;
    z[1] = INF;
    for q in 1..n {
        let fq = f[q] + (q * q) as f32;
        let mut s;
        loop {
            let p = v[k];
            s = (fq - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                break;
            }
        }
        if s <= z[k] {
            // k == 0: q dominates everything so far
            v[0] = q;
            z[0] = -INF;
            z[1] = INF;
            continue;
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = INF;
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        d[q] = (dq * dq + f[p]).min(INF);
    }
}

/// Euclidean distance (in pixels, between pixel centres) from every pixel to the nearest pixel
/// where `seed` is true.
fn edt(w: usize, h: usize, seed: impl Fn(usize) -> bool) -> Vec<f32> {
    let mut g = vec![INF; w * h];
    for (i, v) in g.iter_mut().enumerate() {
        if seed(i) {
            *v = 0.0;
        }
    }
    let n = w.max(h);
    let mut f = vec![0.0f32; n];
    let mut d = vec![0.0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v[..h], &mut z[..h + 1]);
        for y in 0..h {
            g[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..y * w + w]);
        edt_1d(&f[..w], &mut d[..w], &mut v[..w], &mut z[..w + 1]);
        for x in 0..w {
            g[y * w + x] = d[x].sqrt();
        }
    }
    g
}

/// Approximate signed distance to the shape edge: negative inside, positive outside, with
/// sub-pixel refinement from the coverage values.
pub fn signed_distance(m: &Mask) -> Vec<f32> {
    let (w, h) = (m.w, m.h);
    let outside = edt(w, h, |i| m.a[i] >= 0.5);
    let inside = edt(w, h, |i| m.a[i] < 0.5);
    (0..w * h)
        .map(|i| {
            let c = m.a[i];
            if c > 0.0 && c < 1.0 {
                0.5 - c
            } else if c >= 0.5 {
                -(inside[i] - 0.5).max(0.0)
            } else {
                (outside[i] - 0.5).max(0.0)
            }
        })
        .collect()
}

/// Stroke coverage of width `width` pixels around the shape in `m`.
pub fn stroke(m: &Mask, width: f32, kind: StrokeKind) -> Mask {
    let sd = signed_distance(m);
    let a = sd
        .iter()
        .zip(&m.a)
        .map(|(&d, &c)| {
            let v = match kind {
                StrokeKind::Outer => (width + 0.5 - d).clamp(0.0, 1.0).max(c),
                StrokeKind::Center => (width / 2.0 + 0.5 - d.abs()).clamp(0.0, 1.0),
                StrokeKind::Inner => (width + 0.5 + d).clamp(0.0, 1.0).min(c),
            };
            if kind == StrokeKind::Outer { v } else { v.min(1.0) }
        })
        .collect();
    Mask { w: m.w, h: m.h, a }
}

/// How a box-filter pass walks a mask: `lanes` independent lines of `count` samples, `stride`
/// apart within a line and `lane_stride` apart between lines.
#[derive(Clone, Copy)]
struct Lines {
    stride: usize,
    count: usize,
    lanes: usize,
    lane_stride: usize,
}

/// One running-sum box filter of radius `r` along every line of `src` into `dst`.
fn box_pass(src: &[f32], dst: &mut [f32], lines: Lines, r: usize) {
    let Lines { stride, count, lanes, lane_stride } = lines;
    let norm = 1.0 / (2 * r + 1) as f32;
    for l in 0..lanes {
        let base = l * lane_stride;
        let at = |i: isize| -> f32 { if i < 0 || i as usize >= count { 0.0 } else { src[base + i as usize * stride] } };
        let mut acc = 0.0f32;
        for i in -(r as isize)..=(r as isize) {
            acc += at(i);
        }
        for i in 0..count {
            dst[base + i * stride] = acc * norm;
            acc += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
        }
    }
}

/// Blur a mask with three box passes per axis (≈ Gaussian of standard deviation `sigma`).
pub fn blur(m: &Mask, sigma: f32) -> Mask {
    if sigma < 0.3 || m.w == 0 || m.h == 0 {
        return m.clone();
    }
    // box radius for 3 passes ≈ sigma * sqrt(3)
    let r = ((sigma * sigma + 1.0).sqrt().round() as usize).max(1);
    let mut a = m.a.clone();
    let mut b = vec![0.0f32; a.len()];
    for _ in 0..3 {
        box_pass(&a, &mut b, Lines { stride: 1, count: m.w, lanes: m.h, lane_stride: m.w }, r);
        std::mem::swap(&mut a, &mut b);
        box_pass(&a, &mut b, Lines { stride: m.w, count: m.h, lanes: m.w, lane_stride: 1 }, r);
        std::mem::swap(&mut a, &mut b);
    }
    Mask { w: m.w, h: m.h, a }
}

/// Shift a mask by whole pixels (content moving out is dropped).
pub fn offset(m: &Mask, dx: i32, dy: i32) -> Mask {
    let mut o = Mask::new(m.w, m.h);
    o.add(m, dx, dy);
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::{Path, fill};

    #[test]
    fn distance_transform_is_euclidean() {
        let d = edt(9, 9, |i| i == 4 * 9 + 4);
        assert_eq!(d[4 * 9 + 4], 0.0);
        assert!((d[4 * 9 + 7] - 3.0).abs() < 1e-5);
        assert!((d[0] - (32.0f32).sqrt()).abs() < 1e-4);
        let none = edt(3, 3, |_| false);
        assert!(none.iter().all(|&v| v > 1e5));
    }

    #[test]
    fn strokes_sit_outside_centre_and_inside() {
        let m = fill(&Path::rect(10.0, 10.0, 30.0, 30.0), 40, 40, 0.0, 0.0);
        let outer = stroke(&m, 4.0, StrokeKind::Outer);
        assert!(outer.get(7, 20) > 0.99, "outer covers 3px outside");
        assert!(outer.get(4, 20) < 0.01);
        assert!(outer.get(20, 20) > 0.99, "outer includes the fill area");
        let inner = stroke(&m, 4.0, StrokeKind::Inner);
        assert!(inner.get(12, 20) > 0.99 && inner.get(7, 20) < 0.01 && inner.get(20, 20) < 0.01);
        let centre = stroke(&m, 4.0, StrokeKind::Center);
        assert!(centre.get(9, 20) > 0.99 && centre.get(11, 20) > 0.99 && centre.get(20, 20) < 0.01 && centre.get(5, 20) < 0.01);
    }

    #[test]
    fn blur_keeps_mass() {
        let m = fill(&Path::rect(20.0, 20.0, 30.0, 30.0), 50, 50, 0.0, 0.0);
        let b = blur(&m, 3.0);
        let (s0, s1): (f32, f32) = (m.a.iter().sum(), b.a.iter().sum());
        assert!((s0 - s1).abs() < 0.5, "{s0} {s1}");
        assert!(b.get(18, 25) > 0.05 && b.get(25, 25) < 1.0);
        let o = offset(&m, 5, -5);
        assert!(o.get(33, 18) > 0.9);
    }
}
