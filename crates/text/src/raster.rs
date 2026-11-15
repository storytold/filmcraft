//! Anti-aliased path rasteriser for glyph outlines and vector shapes.
//!
//! Paths are flattened to closed polylines (contours). [`fill`] samples each pixel row at [`SUB`]
//! sub-scanlines with an active-edge list and accumulates the non-zero winding spans of each
//! sub-scanline with exact horizontal coverage at the span ends. Coverage is linear (fractional
//! area), so compositing it in linear light gives correct anti-aliasing. Original implementation
//! for FilmCraft (it started as the caption rasteriser).

/// Vertical sub-samples per pixel row. Coverage of a near-horizontal edge is off by at most
/// 1 / (2 SUB); with fewer samples, curve tops and bottoms step visibly (#4791).
pub const SUB: usize = 16;

/// Most a flattened curve may stray from the true outline, in pixels (at `Path::detail` 1).
/// Coarser chords show as dents and bumps along curved edges (#4791).
const FLATTEN_TOL: f32 = 1.0 / 32.0;

/// A 2×3 affine transform `[a, b, c, d, e, f]`: (x, y) → (a x + c y + e, b x + d y + f).
pub type Xform = [f32; 6];

pub const IDENTITY: Xform = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[inline]
pub fn apply(m: &Xform, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// `a ∘ b`: apply `b` first, then `a`.
pub fn compose(a: &Xform, b: &Xform) -> Xform {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// Average linear scale of a transform (for flattening tolerances and stroke widths).
pub fn xform_scale(m: &Xform) -> f32 {
    (m[0] * m[3] - m[1] * m[2]).abs().sqrt()
}

/// A flattened path: closed contours of points in pixel space, y down.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub contours: Vec<Vec<(f32, f32)>>,
    cur: (f32, f32),
    open: bool,
    /// Flattening tolerance multiplier (curves are split more finely for larger values).
    pub detail: f32,
}

impl Path {
    pub fn new() -> Self {
        Self { detail: 1.0, ..Default::default() }
    }
    fn tol(&self) -> f32 {
        if self.detail > 0.0 { self.detail } else { 1.0 }
    }
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.contours.push(vec![(x, y)]);
        self.cur = (x, y);
        self.open = true;
    }
    pub fn line_to(&mut self, x: f32, y: f32) {
        if !self.open {
            self.move_to(self.cur.0, self.cur.1);
        }
        if (x, y) != self.cur
            && let Some(c) = self.contours.last_mut()
        {
            c.push((x, y));
        }
        self.cur = (x, y);
    }
    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let dd = ((x0 - 2.0 * cx + x).abs() + (y0 - 2.0 * cy + y).abs()).max(0.01) * self.tol();
        // n chords stay within |d| / (4 n²) of the curve (d: second difference).
        let n = ((dd / (4.0 * FLATTEN_TOL)).sqrt().ceil() as usize).clamp(1, 64);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.line_to(u * u * x0 + 2.0 * u * t * cx + t * t * x, u * u * y0 + 2.0 * u * t * cy + t * t * y);
        }
    }
    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let dd =
            ((x0 - 2.0 * c1x + c2x).abs() + (y0 - 2.0 * c1y + c2y).abs()).max((c1x - 2.0 * c2x + x).abs() + (c1y - 2.0 * c2y + y).abs()).max(0.01) * self.tol();
        // Wang's bound: n chords stay within 3 max|d| / (4 n²) of the curve.
        let n = ((3.0 * dd / (4.0 * FLATTEN_TOL)).sqrt().ceil() as usize).clamp(1, 96);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.line_to(a * x0 + b * c1x + c * c2x + d * x, a * y0 + b * c1y + c * c2y + d * y);
        }
    }
    /// Close the current contour (contours are always filled as closed polygons).
    pub fn close(&mut self) {
        if self.open {
            if let Some(c) = self.contours.last()
                && let Some(&p) = c.first()
            {
                self.cur = p;
            }
            if self.contours.last().is_some_and(|c| c.len() < 2) {
                self.contours.pop();
            }
        }
        self.open = false;
    }
    /// Append all contours of `o`.
    pub fn extend(&mut self, o: &Path) {
        self.close();
        self.contours.extend(o.contours.iter().cloned());
    }
    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }
    pub fn transform(&mut self, m: &Xform) {
        for c in &mut self.contours {
            for p in c.iter_mut() {
                *p = apply(m, p.0, p.1);
            }
        }
    }
    pub fn transformed(&self, m: &Xform) -> Path {
        let mut p = self.clone();
        p.transform(m);
        p
    }
    /// Bounding box `(x0, y0, x1, y1)` of all points.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut it = self.contours.iter().flatten();
        let f = it.next()?;
        let mut b = (f.0, f.1, f.0, f.1);
        for p in it {
            b = (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1));
        }
        Some(b)
    }
    /// Signed area (shoelace, y down: positive = clockwise on screen) of all contours.
    pub fn signed_area(&self) -> f32 {
        self.contours.iter().map(|c| contour_area(c)).sum()
    }
    /// Grow (d > 0) or shrink every contour by `d` pixels along its normals (faux bold). Holes
    /// shrink because they wind the other way.
    pub fn embolden(&mut self, d: f32) {
        if d == 0.0 {
            return;
        }
        let sign = if self.signed_area() >= 0.0 { 1.0 } else { -1.0 };
        for c in &mut self.contours {
            let n = c.len();
            if n < 3 {
                continue;
            }
            let src = c.clone();
            for i in 0..n {
                let p0 = src[(i + n - 1) % n];
                let p1 = src[i];
                let p2 = src[(i + 1) % n];
                let n1 = normal(p0, p1, sign);
                let n2 = normal(p1, p2, sign);
                let (mx, my) = (n1.0 + n2.0, n1.1 + n2.1);
                let ml = (mx * mx + my * my).sqrt();
                if ml < 1e-6 {
                    c[i] = (p1.0 + n1.0 * d, p1.1 + n1.1 * d);
                    continue;
                }
                let (mx, my) = (mx / ml, my / ml);
                let cos = (mx * n1.0 + my * n1.1).max(0.35);
                c[i] = (p1.0 + mx * d / cos, p1.1 + my * d / cos);
            }
        }
    }

    // ---- shapes ----

    pub fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Path {
        let mut p = Path::new();
        p.move_to(x0, y0);
        p.line_to(x1, y0);
        p.line_to(x1, y1);
        p.line_to(x0, y1);
        p.close();
        p
    }
    /// Rectangle with rounded corners of radius `r` (clamped to half the shorter side).
    pub fn round_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Path {
        let r = r.min((x1 - x0).abs() / 2.0).min((y1 - y0).abs() / 2.0).max(0.0);
        if r < 0.01 {
            return Path::rect(x0, y0, x1, y1);
        }
        let k = 0.552_284_8 * r;
        let mut p = Path::new();
        p.move_to(x0 + r, y0);
        p.line_to(x1 - r, y0);
        p.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
        p.line_to(x1, y1 - r);
        p.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
        p.line_to(x0 + r, y1);
        p.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
        p.line_to(x0, y0 + r);
        p.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
        p.close();
        p
    }
    /// Ellipse centred at (cx, cy) with radii (rx, ry), wound clockwise on screen.
    pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
        let (kx, ky) = (0.552_284_8 * rx, 0.552_284_8 * ry);
        let mut p = Path::new();
        p.move_to(cx + rx, cy);
        p.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
        p.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
        p.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
        p.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
        p.close();
        p
    }
    /// Closed polygon through `pts`.
    pub fn polygon(pts: &[(f32, f32)]) -> Path {
        let mut p = Path::new();
        if let Some(&(x, y)) = pts.first() {
            p.move_to(x, y);
            for &(x, y) in &pts[1..] {
                p.line_to(x, y);
            }
            p.close();
        }
        p
    }
}

fn contour_area(c: &[(f32, f32)]) -> f32 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let (x0, y0) = c[i];
        let (x1, y1) = c[(i + 1) % n];
        a += x0 * y1 - x1 * y0;
    }
    a / 2.0
}

/// Outward unit normal of edge p0→p1 for an outline of orientation `sign`.
fn normal(p0: (f32, f32), p1: (f32, f32), sign: f32) -> (f32, f32) {
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let l = (dx * dx + dy * dy).sqrt();
    if l < 1e-9 {
        return (0.0, 0.0);
    }
    (sign * dy / l, -sign * dx / l)
}

/// Coverage mask (0..1), row-major.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<f32>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, a: vec![0.0; w * h] }
    }
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.a[y * self.w + x]
    }
    /// Add `src` placed at (`ox`, `oy`) (saturating at 1).
    pub fn add(&mut self, src: &Mask, ox: i32, oy: i32) {
        for y in 0..src.h {
            let ty = oy + y as i32;
            if ty < 0 || ty >= self.h as i32 {
                continue;
            }
            let x0 = (-ox).max(0) as usize;
            let x1 = src.w.min((self.w as i32 - ox).max(0) as usize);
            if x0 >= x1 {
                continue;
            }
            let srow = &src.a[y * src.w + x0..y * src.w + x1];
            let d0 = ty as usize * self.w + (ox + x0 as i32) as usize;
            for (d, s) in self.a[d0..d0 + (x1 - x0)].iter_mut().zip(srow) {
                *d = (*d + s).min(1.0);
            }
        }
    }
    /// Per-pixel maximum with `o` (same size).
    pub fn max_with(&mut self, o: &Mask) {
        for (a, b) in self.a.iter_mut().zip(&o.a) {
            *a = a.max(*b);
        }
    }
}

/// Fill `path` (non-zero winding) into a `w`×`h` mask, offsetting the path by `(dx, dy)`.
pub fn fill(path: &Path, w: usize, h: usize, dx: f32, dy: f32) -> Mask {
    let mut m = Mask::new(w, h);
    fill_into(path, &mut m, dx, dy);
    m
}

struct Edge {
    ytop: f32,
    ybot: f32,
    x: f32,
    slope: f32,
    dir: i32,
}

/// Fill `path` into `m`, adding coverage (saturating at 1) — the union of what was there and the
/// path for disjoint content.
pub fn fill_into(path: &Path, m: &mut Mask, dx: f32, dy: f32) {
    let (w, h) = (m.w, m.h);
    if w == 0 || h == 0 {
        return;
    }
    let mut edges: Vec<Edge> = Vec::new();
    for c in &path.contours {
        let n = c.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let (x0, y0) = c[i];
            let (x1, y1) = c[(i + 1) % n];
            let (x0, y0, x1, y1) = (x0 + dx, y0 + dy, x1 + dx, y1 + dy);
            if y0 == y1 || !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
                continue;
            }
            let (dir, xa, ya, xb, yb) = if y0 < y1 { (1, x0, y0, x1, y1) } else { (-1, x1, y1, x0, y0) };
            if yb <= 0.0 || ya >= h as f32 {
                continue;
            }
            edges.push(Edge { ytop: ya, ybot: yb, x: xa, slope: (xb - xa) / (yb - ya), dir });
        }
    }
    if edges.is_empty() {
        return;
    }
    edges.sort_by(|a, b| a.ytop.total_cmp(&b.ytop));
    let ymin = edges[0].ytop.floor().max(0.0) as usize;
    let ymax = (edges.iter().map(|e| e.ybot).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(h);
    let mut next = 0usize;
    let mut active: Vec<usize> = Vec::new();
    let mut xs: Vec<(f32, i32)> = Vec::new();
    let mut row = vec![0.0f32; w + 1];
    let weight = 1.0 / SUB as f32;
    for y in ymin..ymax {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for k in 0..SUB {
            let sy = y as f32 + (k as f32 + 0.5) / SUB as f32;
            while next < edges.len() && edges[next].ytop <= sy {
                active.push(next);
                next += 1;
            }
            active.retain(|&i| edges[i].ybot > sy);
            xs.clear();
            for &i in &active {
                let e = &edges[i];
                if e.ytop <= sy {
                    xs.push((e.x + (sy - e.ytop) * e.slope, e.dir));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            let mut span_start = 0.0f32;
            for &(x, d) in &xs {
                let before = wind;
                wind += d;
                if before == 0 && wind != 0 {
                    span_start = x;
                } else if before != 0
                    && wind == 0
                    && let Some((a, b)) = add_span(&mut row, span_start, x, weight, w)
                {
                    lo = lo.min(a);
                    hi = hi.max(b);
                }
            }
        }
        if lo <= hi && lo < w {
            let hi = hi.min(w - 1);
            let out = &mut m.a[y * w..(y + 1) * w];
            for x in lo..=hi {
                out[x] = (out[x] + row[x]).min(1.0);
                row[x] = 0.0;
            }
            row[w] = 0.0;
        }
    }
}

/// Add coverage `weight` over `[x0, x1)` with fractional ends; returns the touched pixel range.
fn add_span(row: &mut [f32], x0: f32, x1: f32, weight: f32, w: usize) -> Option<(usize, usize)> {
    let x0 = x0.clamp(0.0, w as f32);
    let x1 = x1.clamp(0.0, w as f32);
    if x1 <= x0 {
        return None;
    }
    let i0 = x0.floor() as usize;
    let i1 = x1.floor() as usize;
    if i0 == i1 {
        row[i0] += (x1 - x0) * weight;
        return Some((i0, i0));
    }
    row[i0] += (i0 as f32 + 1.0 - x0) * weight;
    for v in &mut row[i0 + 1..i1] {
        *v += weight;
    }
    if i1 < w {
        row[i1] += (x1 - i1 as f32) * weight;
    }
    Some((i0, i1.min(w - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_coverage() {
        let m = fill(&Path::rect(1.5, 2.0, 4.0, 5.0), 6, 6, 0.0, 0.0);
        assert!((m.get(1, 3) - 0.5).abs() < 1e-5);
        assert!((m.get(2, 3) - 1.0).abs() < 1e-5);
        assert_eq!(m.get(4, 3), 0.0);
        assert_eq!(m.get(2, 1), 0.0);
        assert_eq!(m.get(2, 5), 0.0);
        let total: f32 = m.a.iter().sum();
        assert!((total - 2.5 * 3.0).abs() < 1e-3, "{total}");
    }

    #[test]
    fn hole_with_opposite_winding() {
        let mut p = Path::rect(0.0, 0.0, 10.0, 10.0);
        p.move_to(3.0, 3.0);
        p.line_to(3.0, 7.0);
        p.line_to(7.0, 7.0);
        p.line_to(7.0, 3.0);
        p.close();
        let m = fill(&p, 10, 10, 0.0, 0.0);
        assert_eq!(m.get(5, 5), 0.0);
        assert!((m.get(1, 5) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn circle_area() {
        let (cx, cy, r) = (10.0f32, 10.0f32, 6.0f32);
        let m = fill(&Path::ellipse(cx, cy, r, r), 20, 20, 0.0, 0.0);
        let total: f32 = m.a.iter().sum();
        let want = std::f32::consts::PI * r * r;
        assert!((total - want).abs() / want < 0.02, "{total} vs {want}");
    }

    /// #4791: large curved edges (title text) must match the true outline closely. Coarse curve
    /// flattening and five sub-scanlines left coverage errors up to ~0.2 along the edge, which
    /// show as dents and steps against a contrasting background.
    #[test]
    fn large_curve_edges_match_exact_coverage() {
        let (cx, cy, r) = (160.3f32, 160.7f32, 150.0f32);
        let m = fill(&Path::ellipse(cx, cy, r, r), 320, 320, 0.0, 0.0);
        const N: usize = 32;
        let mut worst = (0.0f32, 0, 0);
        for y in 0..320 {
            for x in 0..320 {
                let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (px * px + py * py).sqrt() - r;
                let want = if d < -1.5 {
                    1.0
                } else if d > 1.5 {
                    0.0
                } else {
                    let mut inside = 0;
                    for j in 0..N {
                        for i in 0..N {
                            let sx = x as f32 + (i as f32 + 0.5) / N as f32 - cx;
                            let sy = y as f32 + (j as f32 + 0.5) / N as f32 - cy;
                            if sx * sx + sy * sy < r * r {
                                inside += 1;
                            }
                        }
                    }
                    inside as f32 / (N * N) as f32
                };
                let e = (m.get(x, y) - want).abs();
                if e > worst.0 {
                    worst = (e, x, y);
                }
            }
        }
        assert!(worst.0 < 0.06, "coverage off by {} at {:?}", worst.0, (worst.1, worst.2));
    }

    #[test]
    fn embolden_grows_outline_and_shrinks_holes() {
        let mut p = Path::rect(4.0, 4.0, 14.0, 14.0);
        p.embolden(1.0);
        let b = p.bounds().unwrap();
        assert!((b.0 - 3.0).abs() < 1e-4 && (b.2 - 15.0).abs() < 1e-4, "{b:?}");
        // reversed winding: still grows (orientation is taken from the whole outline)
        let mut q = Path::polygon(&[(4.0, 4.0), (4.0, 14.0), (14.0, 14.0), (14.0, 4.0)]);
        q.embolden(1.0);
        assert!((q.bounds().unwrap().0 - 3.0).abs() < 1e-4);
    }

    #[test]
    fn transforms_compose() {
        let t = compose(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0], &[1.0, 0.0, 0.0, 1.0, 3.0, 4.0]);
        assert_eq!(apply(&t, 1.0, 1.0), (8.0, 10.0));
        let p = Path::rect(0.0, 0.0, 1.0, 1.0).transformed(&t);
        assert_eq!(p.bounds(), Some((6.0, 8.0, 8.0, 10.0)));
        assert!((xform_scale(&t) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn round_rect_area() {
        let m = fill(&Path::round_rect(0.0, 0.0, 20.0, 10.0, 3.0), 20, 10, 0.0, 0.0);
        let total: f32 = m.a.iter().sum();
        let want = 200.0 - (4.0 - std::f32::consts::PI) * 9.0;
        assert!((total - want).abs() < 1.0, "{total} {want}");
    }
}
