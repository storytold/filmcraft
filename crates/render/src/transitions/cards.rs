//! Textured 3D cards under a pinhole camera, for flip / spin / cube / fold transitions.
//!
//! World space is pixel units: the screen is the plane z = 0 spanning (0..w, 0..h), +z points
//! away from the viewer and the eye sits at (w/2, h/2, −focal). A card at z = 0 with the identity
//! transform therefore covers the frame pixel-for-pixel.

/// 3-vector.
pub type V3 = [f32; 3];

#[inline]
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
#[inline]
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// An affine placement of a card: world = origin + lx·ex + ly·ey for image pixel (lx, ly).
#[derive(Clone, Copy, Debug)]
pub struct Card {
    pub origin: V3,
    pub ex: V3,
    pub ey: V3,
}

impl Card {
    /// The identity card (covers the frame at z = 0).
    pub fn flat() -> Self {
        Card { origin: [0.0; 3], ex: [1.0, 0.0, 0.0], ey: [0.0, 1.0, 0.0] }
    }
    fn map(&self, f: impl Fn(V3) -> V3, lin: impl Fn(V3) -> V3) -> Self {
        Card { origin: f(self.origin), ex: lin(self.ex), ey: lin(self.ey) }
    }
    pub fn translate(self, d: V3) -> Self {
        self.map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]], |v| v)
    }
    /// Uniform scale about `c`.
    pub fn scale_about(self, c: V3, s: f32) -> Self {
        self.map(|p| [c[0] + (p[0] - c[0]) * s, c[1] + (p[1] - c[1]) * s, c[2] + (p[2] - c[2]) * s], |v| [v[0] * s, v[1] * s, v[2] * s])
    }
    /// Rotate about the vertical (y) axis through `c` by `a` radians (positive turns the right edge away).
    pub fn rot_y(self, c: V3, a: f32) -> Self {
        let (s, co) = a.sin_cos();
        let co = if co.abs() < 1e-5 { 0.0 } else { co }; // edge-on: match the GPU snap, so front/back cannot flip on f32 noise
        let r = move |v: V3| [v[0] * co - v[2] * s, v[1], v[0] * s + v[2] * co];
        self.map(move |p| add(c, r(sub(p, c))), r)
    }
    /// Rotate about the horizontal (x) axis through `c` (positive turns the bottom edge away).
    pub fn rot_x(self, c: V3, a: f32) -> Self {
        let (s, co) = a.sin_cos();
        let co = if co.abs() < 1e-5 { 0.0 } else { co }; // edge-on: match the GPU snap, so front/back cannot flip on f32 noise
        let r = move |v: V3| [v[0], v[1] * co - v[2] * s, v[1] * s + v[2] * co];
        self.map(move |p| add(c, r(sub(p, c))), r)
    }
    /// Rotate in the screen plane about `c` (positive = clockwise on screen).
    pub fn rot_z(self, c: V3, a: f32) -> Self {
        let (s, co) = a.sin_cos();
        let r = move |v: V3| [v[0] * co - v[1] * s, v[0] * s + v[1] * co, v[2]];
        self.map(move |p| add(c, r(sub(p, c))), r)
    }

    /// Intersect the eye ray through screen point (x, y): local image coords, depth along the
    /// ray, and whether the front face (the side with the normal towards the eye) is seen.
    #[inline]
    pub fn hit(&self, eye: V3, x: f32, y: f32) -> Option<(f32, f32, f32, bool)> {
        let d = sub([x, y, 0.0], eye);
        // eye + t·d = origin + lx·ex + ly·ey  →  lx·ex + ly·ey − t·d = eye − origin
        let rhs = sub(eye, self.origin);
        let nd = [-d[0], -d[1], -d[2]];
        let det = dot(self.ex, cross(self.ey, nd));
        if det.abs() < 1e-9 {
            return None;
        }
        let lx = dot(rhs, cross(self.ey, nd)) / det;
        let ly = dot(self.ex, cross(rhs, nd)) / det;
        let t = dot(self.ex, cross(self.ey, rhs)) / det;
        if t <= 1e-4 {
            return None;
        }
        let n = cross(self.ex, self.ey); // +z for the flat card (faces away from the eye at −z)
        let front = dot(n, d) > 0.0;
        Some((lx, ly, t, front))
    }
}

#[inline]
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// The camera eye for a frame of `w`×`h` with a focal distance of `focal` pixels.
pub fn eye(w: f32, h: f32, focal: f32) -> V3 {
    [w / 2.0, h / 2.0, -focal.max(1.0)]
}

/// Focal distance for a "Perspective" percentage (0 % = nearly orthographic, 100 % = wide angle).
pub fn focal_for(w: f32, h: f32, perspective: f32) -> f32 {
    let d = w.max(h);
    d * (6.0 - 5.0 * perspective.clamp(0.0, 1.0)).max(0.6)
}
