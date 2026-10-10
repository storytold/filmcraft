//! Evaluated SDR Lumetri advanced stage. The original effect remains the independent oracle.
use crate::Image;
use crate::effects::{FxCtx, choice, curve_lut, curve_param, dec, enc, f, hue_lut, is_identity_curve, on, text, wheel_rgb};
use filmcraft_color::{Lut, hsl_to_rgb, luma709, rgb_to_hsl};
use filmcraft_project::EffectInstance;
use std::sync::Arc;

pub const CURVE_SIZE: usize = 1024;
#[derive(Clone, Debug, PartialEq)]
pub struct Grade {
    pub look_lut: Option<Arc<Lut>>,
    pub look: u32,
    pub intensity: f32,
    /// Already weighted lift, midtone offset, gain.
    pub wheels: Option<[[f32; 3]; 3]>,
    /// Luma, R, G, B, hue/sat, hue/hue, hue/luma, luma/sat, sat/sat.
    pub curves: [Option<Vec<f32>>; 9],
}

impl Grade {
    pub fn eval(e: &EffectInstance, cx: &FxCtx) -> Option<Self> {
        let creative = on(e, "creative_on");
        let curves_on = on(e, "curves_on");
        let look_lut = if creative { crate::luts::resolve(cx.project, text(e, "look_lut")) } else { None };
        let look = if creative && look_lut.is_none() { choice(e, "look") } else { 0 };
        let ids = ["curve_luma", "curve_red", "curve_green", "curve_blue", "hue_vs_sat", "hue_vs_hue", "hue_vs_luma", "luma_vs_sat", "sat_vs_sat"];
        let curves = std::array::from_fn(|i| {
            let points = curve_param(e, ids[i]).filter(|_| curves_on)?;
            if i < 4 { (!is_identity_curve(&points)).then(|| curve_lut(&points, CURVE_SIZE)) } else { hue_lut(&points, CURVE_SIZE) }
        });
        let wheels = if on(e, "wheels_on") {
            let ids = [("wheel_shadows", "wheel_shadows_l", 0.3), ("wheel_midtones", "wheel_midtones_l", 0.3), ("wheel_highlights", "wheel_highlights_l", 0.5)];
            let values = ids.map(|(id, light, k)| {
                let rgb = wheel_rgb(e.param(id).map(|p| p.vec2_at(cx.t)).unwrap_or_default());
                let l = f(e, light, cx) / 100.0;
                rgb.map(|v| (v + l) * k)
            });
            // Preserve the original threshold (before applying weights).
            let active = ids.iter().any(|(id, _, _)| wheel_rgb(e.param(id).map(|p| p.vec2_at(cx.t)).unwrap_or_default()).iter().any(|v| v.abs() > 1e-5))
                || ids.iter().map(|(_, light, _)| (f(e, light, cx) / 100.0).abs()).sum::<f32>() > 1e-5;
            active.then_some(values)
        } else {
            None
        };
        (look_lut.is_some() || look > 0 || wheels.is_some() || curves.iter().any(Option::is_some)).then_some(Self {
            look_lut,
            look,
            intensity: f(e, "look_intensity", cx) / 100.0,
            wheels,
            curves,
        })
    }

    pub fn gpu_ok(&self) -> bool {
        self.intensity.is_finite()
            && self.look <= 8
            && self.wheels.iter().flatten().flatten().all(|v| v.is_finite())
            && self.curves.iter().flatten().all(|t| t.len() == CURVE_SIZE && t.iter().all(|v| v.is_finite()))
            && self.look_lut.as_ref().is_none_or(|l| lut_ok(l))
    }

    pub fn apply(&self, img: &mut Image) {
        img.map_rgb(|c, _, _| {
            let mut v = enc(c);
            if let Some(lut) = &self.look_lut {
                let lk = lut.apply(v.map(|q| q.clamp(0.0, 1.0)));
                for k in 0..3 {
                    v[k] += (lk[k] - v[k]) * self.intensity;
                }
            } else if self.look > 0 {
                let lk = crate::effects::apply_look(self.look, v);
                for k in 0..3 {
                    v[k] += (lk[k] - v[k]) * self.intensity;
                }
            }
            if let Some([s, m, h]) = self.wheels {
                let l = luma709(v[0], v[1], v[2]).clamp(0.0, 1.0);
                let ws = (1.0 - l).powi(2);
                let wh = l * l;
                let wm = (1.0 - ws - wh).max(0.0);
                for k in 0..3 {
                    v[k] += s[k] * ws;
                    v[k] += m[k] * wm;
                    v[k] *= 1.0 + h[k] * wh;
                }
            }
            let sample = |index: usize, x: f32| -> Option<f32> {
                let t = self.curves.get(index)?.as_ref()?;
                let p = x.clamp(0.0, 1.0) * (CURVE_SIZE - 1) as f32;
                let i = p as usize;
                let j = (i + 1).min(CURVE_SIZE - 1);
                Some(t[i] + (t[j] - t[i]) * (p - i as f32))
            };
            for k in 0..3 {
                if let Some(q) = sample(0, v[k]) {
                    v[k] = q;
                }
            }
            for k in 0..3 {
                if let Some(q) = sample(k + 1, v[k]) {
                    v[k] = q;
                }
            }
            if self.curves[4..].iter().any(Option::is_some) {
                let u = v.map(|q| q.clamp(0.0, 1.0));
                let mut h = rgb_to_hsl(u[0], u[1], u[2]);
                let [h0, s0, l0] = h;
                if let Some(q) = sample(5, h0) {
                    h[0] = (h[0] + q - 0.5).rem_euclid(1.0);
                }
                let mut sm = 1.0;
                for (index, x) in [(4, h0), (7, l0), (8, s0)] {
                    if let Some(q) = sample(index, x) {
                        sm *= q * 2.0;
                    }
                }
                h[1] = (h[1] * sm).clamp(0.0, 1.0);
                if let Some(q) = sample(6, h0) {
                    h[2] = (h[2] + (q - 0.5) * 0.5).clamp(0.0, 1.0);
                }
                v = hsl_to_rgb(h[0], h[1], h[2]);
            }
            dec(v)
        });
    }
}

pub fn lut_ok(lut: &Lut) -> bool {
    let domain = |min: &[f32; 3], max: &[f32; 3]| min.iter().zip(max).all(|(a, b)| a.is_finite() && b.is_finite() && b > a);
    lut.shaper
        .as_ref()
        .is_none_or(|l| (2..=65536).contains(&l.data.len()) && domain(&l.domain_min, &l.domain_max) && l.data.iter().flatten().all(|v| v.is_finite()))
        && lut.cube.as_ref().is_none_or(|l| {
            (2..=64).contains(&l.size)
                && l.data.len() == l.size.saturating_pow(3)
                && domain(&l.domain_min, &l.domain_max)
                && l.data.iter().flatten().all(|v| v.is_finite())
        })
}

pub(crate) fn hsl_key(v: [f32; 3], q: &[f32; 12]) -> f32 {
    let h = rgb_to_hsl(v[0], v[1], v[2]);
    let dh = (h[0] - q[0]).abs().min(1.0 - (h[0] - q[0]).abs());
    (1.0 - ((dh - q[1] / 2.0) / q[5]).clamp(0.0, 1.0))
        * ((h[1] - q[2]) / q[5]).clamp(0.0, 1.0)
        * ((h[2] - q[3]) / q[5]).clamp(0.0, 1.0).min(((q[4] - h[2]) / q[5]).clamp(0.0, 1.0))
}
