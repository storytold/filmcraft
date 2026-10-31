//! 2D geometry for `FilmCraft`: vectors, rectangles and affine transforms (Motion effect math).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use serde::{Deserialize, Serialize};
use std::ops::{Add, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    #[must_use]
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }
    #[must_use]
    pub fn lerp(self, o: Vec2, t: f64) -> Vec2 {
        Vec2::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
}
impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f64> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f64) -> Vec2 {
        Vec2::new(self.x * s, self.y * s)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    #[must_use]
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }
    #[must_use]
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }
    #[must_use]
    pub fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        Rect::new(x0, y0, self.right().max(o.right()) - x0, self.bottom().max(o.bottom()) - y0)
    }
    /// Fit `inner` (w,h) inside this rect preserving aspect, centered.
    #[must_use]
    pub fn fit(&self, w: f64, h: f64) -> Rect {
        let s = (self.w / w).min(self.h / h);
        let (fw, fh) = (w * s, h * s);
        Rect::new(self.x + (self.w - fw) / 2.0, self.y + (self.h - fh) / 2.0, fw, fh)
    }
}

/// Affine transform `[a c e; b d f; 0 0 1]` mapping (x, y) → (a x + c y + e, b x + d y + f).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };
    #[must_use]
    pub fn translate(x: f64, y: f64) -> Self {
        Affine { e: x, f: y, ..Self::IDENTITY }
    }
    #[must_use]
    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine { a: sx, d: sy, ..Self::IDENTITY }
    }
    /// Rotation by `deg` degrees, clockwise on screen (y down), matching Premiere's Rotation.
    #[must_use]
    pub fn rotate_deg(deg: f64) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Affine { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }
    /// `self * o`: apply `o` first, then `self`.
    #[must_use]
    pub fn then_apply(&self, o: &Affine) -> Affine {
        // result = self ∘ o
        Affine {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    #[must_use]
    pub fn apply(&self, p: Vec2) -> Vec2 {
        Vec2::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f)
    }
    #[must_use]
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    #[must_use]
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.determinant();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let a = self.d * inv;
        let b = -self.b * inv;
        let c = -self.c * inv;
        let d = self.a * inv;
        Some(Affine { a, b, c, d, e: -(a * self.e + c * self.f), f: -(b * self.e + d * self.f) })
    }
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
    /// Bounding box of a transformed rect.
    pub fn bounds(&self, r: &Rect) -> Rect {
        let pts = [Vec2::new(r.x, r.y), Vec2::new(r.right(), r.y), Vec2::new(r.x, r.bottom()), Vec2::new(r.right(), r.bottom())].map(|p| self.apply(p));
        let x0 = pts.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let x1 = pts.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        let y1 = pts.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Premiere's Motion effect: the source's anchor point is placed at `position` (sequence pixels),
    /// scaled by `scale` (1.0 = 100%) around the anchor and rotated by `rotation_deg`.
    #[must_use]
    pub fn motion(position: Vec2, scale: Vec2, rotation_deg: f64, anchor: Vec2) -> Affine {
        Affine::translate(position.x, position.y)
            .then_apply(&Affine::rotate_deg(rotation_deg))
            .then_apply(&Affine::scale(scale.x, scale.y))
            .then_apply(&Affine::translate(-anchor.x, -anchor.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_roundtrip() {
        let m = Affine::motion(Vec2::new(960.0, 540.0), Vec2::new(1.5, 0.75), 33.0, Vec2::new(100.0, 50.0));
        let inv = m.inverse().unwrap();
        let p = Vec2::new(12.0, -7.0);
        let q = inv.apply(m.apply(p));
        assert!((p - q).length() < 1e-9);
    }

    #[test]
    fn motion_anchor_lands_on_position() {
        let m = Affine::motion(Vec2::new(960.0, 540.0), Vec2::new(2.0, 2.0), 90.0, Vec2::new(10.0, 20.0));
        let p = m.apply(Vec2::new(10.0, 20.0));
        assert!((p - Vec2::new(960.0, 540.0)).length() < 1e-9);
    }

    #[test]
    fn fit_rect() {
        let r = Rect::new(0.0, 0.0, 200.0, 100.0).fit(16.0, 9.0);
        assert!((r.h - 100.0).abs() < 1e-9 && (r.w - 177.777).abs() < 0.01);
    }
}
