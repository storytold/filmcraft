//! Evaluated filter operations whose GPU implementation needs only the current source texture.

use filmcraft_project::EffectInstance;

use crate::effects::{FxCtx, f};
use crate::image::Image;

/// Median uses a fixed 3×3 neighborhood. Larger radii stay on CPU until a bounded multi-pass
/// selection algorithm can reproduce the reference exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterOp {
    pub radius: u32,
}

impl FilterOp {
    pub fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<Self> {
        if !e.enabled || e.effect != "median" {
            return None;
        }
        let scale = if cx.px_scale.is_finite() { cx.px_scale.max(0.0) } else { 1.0 };
        let raw = f(e, "radius", cx) * scale;
        let radius = if !raw.is_finite() || raw <= 0.0 { 0 } else { raw.round().clamp(0.0, (100.0 * scale).min(w.max(h).max(1) as f32)) as u32 };
        Some(Self { radius })
    }

    /// Opcode 160; zero radius is a no-op, radius one is exact single-pass GPU support.
    pub fn pack(&self) -> (u32, [u32; 4], [f32; 24]) {
        (160, [self.radius, 0, 0, 0], [0.0; 24])
    }

    /// No second texture, mask, or history input required.
    pub const fn requires_current_source(&self) -> bool {
        true
    }

    pub fn gpu_ok(&self) -> bool {
        self.radius <= 1
    }

    /// CPU reference for this evaluated operation.
    pub fn apply(&self, img: &mut Image) {
        if self.radius > 0 && img.w > 0 && img.h > 0 {
            crate::effects::median(img, self.radius as isize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oracle(src: &Image, radius: isize) -> Image {
        let mut out = Image::new(src.w, src.h);
        for y in 0..src.h {
            for x in 0..src.w {
                for c in 0..4 {
                    let width = (radius * 2 + 1) as usize;
                    let mut taps = Vec::with_capacity(width * width);
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            taps.push(src.get_clamped(x as isize + dx, y as isize + dy)[c]);
                        }
                    }
                    taps.sort_by(f32::total_cmp);
                    out.px[(y * src.w + x) * 4 + c] = taps[taps.len() / 2];
                }
            }
        }
        out
    }

    #[test]
    fn median_three_by_three_matches_independent_sorted_oracle_with_edges_and_alpha() {
        let mut src = Image::new(5, 3);
        for y in 0..src.h {
            for x in 0..src.w {
                let a = if (x + y) % 3 == 0 {
                    0.0
                } else if x % 2 == 0 {
                    0.35
                } else {
                    1.0
                };
                let i = (y * src.w + x) * 4;
                src.px[i..i + 4].copy_from_slice(&[x as f32 * 0.2 * a, y as f32 * 0.3 * a, (x + y) as f32 * 0.1 * a, a]);
            }
        }
        let expected = oracle(&src, 1);
        let mut actual = src;
        FilterOp { radius: 1 }.apply(&mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn median_radius_two_preserves_cpu_semantics_with_independent_sorted_oracle() {
        let mut src = Image::new(5, 3);
        for y in 0..src.h {
            for x in 0..src.w {
                let alpha = if (x + y) % 2 == 0 { 0.25 } else { 1.0 };
                let i = (y * src.w + x) * 4;
                src.px[i..i + 4].copy_from_slice(&[x as f32 * alpha, y as f32 * alpha, (x + 2 * y) as f32 * alpha, alpha]);
            }
        }
        let expected = oracle(&src, 2);
        let mut actual = src;
        let op = FilterOp { radius: 2 };
        assert!(!op.gpu_ok(), "larger radii remain CPU-only");
        op.apply(&mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn median_rejects_non_three_by_three_radius_for_strict_gpu() {
        assert!(FilterOp { radius: 1 }.gpu_ok());
        assert!(!FilterOp { radius: 2 }.gpu_ok());
    }
}
