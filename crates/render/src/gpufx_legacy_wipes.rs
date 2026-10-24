//! Exact single-input GPU routes for two legacy wipe effects.
use filmcraft_project::EffectInstance;

use crate::{effects::FxCtx, image::Image};

#[derive(Clone, Debug, PartialEq)]
pub enum LegacyWipeOp {
    BlockDissolve { completion: f32, block_w: f32, block_h: f32, feather: f32, soft: bool },
    LinearWipe { completion: f32, dx: f32, dy: f32, lo: f32, extent: f32, feather: f32 },
}

impl LegacyWipeOp {
    pub fn eval(e: &EffectInstance, cx: &FxCtx<'_>, w: usize, h: usize) -> Option<Self> {
        if !e.enabled || w == 0 || h == 0 || w > 16_384 || h > 16_384 || !cx.px_scale.is_finite() || cx.px_scale <= 0.0 {
            return None;
        }
        use crate::vfx::{bv, fv};
        let completion = fv(e, "completion", cx) / 100.0;
        Some(match e.effect.as_str() {
            "block_dissolve" => Self::BlockDissolve {
                completion,
                block_w: (fv(e, "block_w", cx) * cx.px_scale).max(1.0),
                block_h: (fv(e, "block_h", cx) * cx.px_scale).max(1.0),
                feather: fv(e, "feather", cx) / 100.0 * 0.5,
                soft: bv(e, "soft"),
            },
            "linear_wipe_legacy" => {
                let a = fv(e, "angle", cx).to_radians();
                let (dx, dy) = (a.sin(), -a.cos());
                let wf = w as f32;
                let hf = h as f32;
                let corners = [(0.0, 0.0), (wf, 0.0), (0.0, hf), (wf, hf)].map(|(x, y)| x * dx + y * dy);
                let lo = corners.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = corners.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let extent = (hi - lo).max(1.0);
                Self::LinearWipe { completion, dx, dy, lo, extent, feather: fv(e, "feather", cx) * cx.px_scale / extent }
            }
            _ => return None,
        })
    }

    pub fn matches_id(&self, id: &str) -> bool {
        matches!((self, id), (Self::BlockDissolve { .. }, "block_dissolve") | (Self::LinearWipe { .. }, "linear_wipe_legacy"))
    }

    pub fn gpu_ok(&self) -> bool {
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite() && x.abs() <= 1.0e9);
        match self {
            Self::BlockDissolve { completion, block_w, block_h, feather, .. } => {
                finite(&[*completion, *block_w, *block_h, *feather]) && *block_w >= 1.0 && *block_h >= 1.0
            }
            Self::LinearWipe { completion, dx, dy, lo, extent, feather } => finite(&[*completion, *dx, *dy, *lo, *extent, *feather]) && *extent >= 1.0,
        }
    }

    /// Returns opcode, integer flags, and six vec4 float payloads.
    pub fn pack(&self) -> (u32, [u32; 4], [f32; 24]) {
        let mut p = [0.0; 24];
        let (opcode, flags) = match self {
            Self::BlockDissolve { completion, block_w, block_h, feather, soft } => {
                p[..4].copy_from_slice(&[*completion, *block_w, *block_h, *feather]);
                // Reciprocals, so GPU and CPU both multiply by the same f32 value.
                p[4..6].copy_from_slice(&[1.0 / *block_w, 1.0 / *block_h]);
                (108, [u32::from(*soft), 0, 0, 0])
            }
            Self::LinearWipe { completion, dx, dy, lo, extent, feather } => {
                p[..6].copy_from_slice(&[*completion, *dx, *dy, *lo, *extent, *feather]);
                (107, [0; 4])
            }
        };
        (opcode, flags, p)
    }

    /// CPU oracle for matching source operation.
    pub fn apply(&self, img: &mut Image) {
        match self {
            Self::BlockDissolve { completion: c, block_w: bw, block_h: bh, feather: fe, soft } => {
                if *c <= 0.0 {
                    return;
                }
                let (iw, ih) = (1.0f32 / bw, 1.0f32 / bh);
                for y in 0..img.h {
                    for x in 0..img.w {
                        let r = crate::vfx::hash01((x as f32 * iw) as i64, (y as f32 * ih) as i64, 0, 41);
                        let a = if *soft && *fe > 0.0 {
                            crate::vfx::smoothstep(c - fe, c + fe, r)
                        } else if r >= *c {
                            1.0
                        } else {
                            0.0
                        };
                        let a = if *c >= 1.0 { 0.0 } else { a };
                        let i = (y * img.w + x) * 4;
                        for v in &mut img.px[i..i + 4] {
                            *v *= a;
                        }
                    }
                }
            }
            Self::LinearWipe { completion: c, dx, dy, lo, extent, feather } => {
                if *c <= 0.0 {
                    return;
                }
                for y in 0..img.h {
                    for x in 0..img.w {
                        let g = ((x as f32 + 0.5) * dx + (y as f32 + 0.5) * dy - lo) / extent;
                        let a = wipe_alpha(g, *c, *feather);
                        let i = (y * img.w + x) * 4;
                        for v in &mut img.px[i..i + 4] {
                            *v *= a;
                        }
                    }
                }
            }
        }
    }
}

fn wipe_alpha(g: f32, c: f32, soft: f32) -> f32 {
    if soft <= 1e-4 {
        return if c <= 0.0 || g >= c { 1.0 } else { 0.0 };
    }
    ((g - c * (1.0 + soft) + soft) / soft).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::LegacyWipeOp;
    use crate::{effects::FxCtx, image::Image};
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    fn cx() -> FxCtx<'static> {
        FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        }
    }
    fn image() -> Image {
        let mut im = Image::new(9, 7);
        for y in 0..im.h {
            for x in 0..im.w {
                let a = if (x + y * 2) % 5 == 0 { 0.0 } else { 0.37 + (x % 3) as f32 * 0.2 };
                let i = (y * im.w + x) * 4;
                im.px[i..i + 4].copy_from_slice(&[0.8 * a, 0.3 * a, 0.1 * a, a]);
            }
        }
        im
    }
    fn set(e: &mut filmcraft_project::EffectInstance, key: &str, v: ParamValue) {
        if let Some(p) = e.params.get_mut(key) {
            p.value = v;
        }
    }

    #[test]
    fn implemented_legacy_wipes_match_cpu_oracles() {
        for id in ["block_dissolve", "linear_wipe_legacy"] {
            let Some(def) = find_effect(id) else { panic!("missing registered effect {id}") };
            let mut e = def.instance();
            set(&mut e, "completion", ParamValue::Float(47.0));
            if id == "block_dissolve" {
                set(&mut e, "block_w", ParamValue::Float(2.3));
                set(&mut e, "block_h", ParamValue::Float(1.7));
                set(&mut e, "feather", ParamValue::Float(60.0));
                set(&mut e, "soft", ParamValue::Bool(true));
            } else {
                set(&mut e, "angle", ParamValue::Float(38.0));
                set(&mut e, "feather", ParamValue::Float(2.2));
            }
            let c = cx();
            let Some(op) = LegacyWipeOp::eval(&e, &c, 9, 7) else { panic!("{id} not evaluated") };
            assert!(op.matches_id(id) && op.gpu_ok(), "{id}");
            let mut expected = image();
            let mut actual = expected.clone();
            crate::vfx::apply(&mut expected, &e, &c);
            op.apply(&mut actual);
            for (a, b) in actual.px.iter().zip(expected.px) {
                assert!((a - b).abs() < 2e-6, "{id}: {a} != {b}");
            }
            let (opcode, _, _) = op.pack();
            assert_eq!(opcode, if id == "linear_wipe_legacy" { 107 } else { 108 });
        }
    }
}
