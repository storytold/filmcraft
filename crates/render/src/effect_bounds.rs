//! Extra source-space canvas for effects that paint outside the decoded frame.
//!
//! Keep the source origin separate from the allocation: Motion, point controls and masks still
//! refer to the original clip. Padding is symmetric so image-centred operations keep their centre.

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::EffectInstance;

use crate::{Image, effects::FxCtx};

/// Pad just before each Drop Shadow, after upstream effects have produced its alpha. Downstream
/// effects receive the expanded image in stack order. Bound allocations even for hostile projects
/// or very long effect stacks; when the budget is exceeded, retain the old clipped rendering.
pub(crate) fn expand(img: &mut Image, origin: &mut Vec2, e: &EffectInstance, cx: &FxCtx) {
    if !e.enabled || e.effect != "drop_shadow" || img.w == 0 || img.h == 0 {
        return;
    }
    let opacity = crate::effects::f(e, "opacity", cx);
    if !opacity.is_finite() || opacity <= 0.0 {
        return;
    }
    let (dx, dy, soft) = crate::effects::shadow_geometry(e, cx);
    // Three box passes have finite support. Reserve all of it, plus a pixel for interpolation.
    let blur = crate::effects::shadow_support(soft);
    let x = (dx.abs().ceil() as usize).saturating_add(blur);
    let y = (dy.abs().ceil() as usize).saturating_add(blur);
    if x == 0 && y == 0 {
        return;
    }
    if let Some(padded) = pad(img, x, y) {
        *img = padded;
        origin.x += x as f64;
        origin.y += y as f64;
    } else {
        log::warn!("Drop Shadow canvas exceeds the render allocation budget; rendering with clipped bounds");
    }
}

fn pad(img: &Image, x: usize, y: usize) -> Option<Image> {
    const MAX_PIXELS: usize = 64 * 1024 * 1024;
    let w = img.w.checked_add(x.checked_mul(2)?)?;
    let h = img.h.checked_add(y.checked_mul(2)?)?;
    let pixels = w.checked_mul(h)?;
    if pixels > MAX_PIXELS {
        return None;
    }
    let len = pixels.checked_mul(4)?;
    let row = img.w.checked_mul(4)?;
    if row == 0 || img.px.len() != row.checked_mul(img.h)? {
        return None;
    }
    let mut px = Vec::new();
    px.try_reserve_exact(len).ok()?;
    px.resize(len, 0.0);
    for (yy, src) in img.px.chunks_exact(row).enumerate() {
        let start = ((yy + y) * w + x) * 4;
        px[start..start + row].copy_from_slice(src);
    }
    Some(Image { w, h, px })
}

pub(crate) fn placement(m: Affine, origin: Vec2) -> Affine {
    m.then_apply(&Affine::translate(-origin.x, -origin.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_padding_is_declined_without_allocating() {
        let img = Image::filled(2, 2, [1.0; 4]);
        assert!(pad(&img, usize::MAX, 0).is_none());
        assert!(pad(&img, 100_000, 100_000).is_none());
        assert_eq!(img.get(0, 0), [1.0; 4]);
    }
}
