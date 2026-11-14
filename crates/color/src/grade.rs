//! The grading signal colour correctors (Color Grading) work on.
//!
//! Grading controls (whites / blacks, highlights / shadows, contrast, curves, wheels, keys) are
//! defined on a perceptual signal from 0 (black) to 1 (white), not on linear light. In an SDR
//! working space that signal is the sRGB-encoded display value, and 1 is SDR white.
//!
//! In an HDR working space (Rec. 2100 PQ or HLG) the signal follows the working space's own
//! transfer function and is **normalised to HDR White** (cd/m², Color Grading ▸ Basic Correction ▸ HDR
//! White; the curves have their own HDR Range): signal 1 is HDR White, so the sliders and curves
//! span 0 … HDR White nits the way they span black … white in SDR, and highlights above HDR
//! White (speculars) are values above 1 that pass through instead of clipping:
//!
//! - **PQ** (SMPTE ST 2084): `signal = PQ⁻¹(nits / 10000) / PQ⁻¹(white / 10000)`.
//! - **HLG** (ITU-R BT.2100): the inverse OOTF of a display with peak `white` (system gamma
//!   γ = 1.2 + 0.42·log10(white / 1000), BT.2100 note 5f), then the HLG OETF; so 203 cd/m²
//!   (BT.2408 HDR reference white) is signal 0.75 with HDR White = 1000 cd/m².
//!
//! Working units are linear light with 1.0 = 203 cd/m² ([`crate::REFERENCE_WHITE_NITS`]) in HDR
//! working spaces, as in [`crate::transform`].

use crate::spaces::{Gamut, WorkingSpace};
use crate::{REFERENCE_WHITE_NITS, hlg_inverse_oetf, hlg_oetf, linear_to_srgb, pq_eotf, pq_inverse_eotf, srgb_to_linear};

/// Default HDR White / curve HDR Range (cd/m²): the nominal HLG display peak and the usual PQ
/// mastering peak.
pub const DEFAULT_HDR_WHITE_NITS: f32 = 1000.0;

/// The signal a grading operation works on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradeSpace {
    /// sRGB-encoded display values, clamped to 0…1 on the way back (the classic SDR behaviour).
    Sdr,
    /// PQ signal normalised to `white` cd/m².
    Pq { white: f32, norm: f32 },
    /// HLG signal of a display with peak `white` cd/m² (`gamma` = its system gamma).
    Hlg { white: f32, gamma: f32 },
}

impl GradeSpace {
    /// The grading space of a working space, with HDR White `white_nits` (ignored for SDR).
    pub fn new(working: WorkingSpace, white_nits: f32) -> Self {
        let white = white_nits.clamp(100.0, 10_000.0);
        match working {
            WorkingSpace::Rec709 => GradeSpace::Sdr,
            WorkingSpace::Rec2100Pq => GradeSpace::Pq { white, norm: pq_inverse_eotf(white / 10_000.0) },
            WorkingSpace::Rec2100Hlg => GradeSpace::Hlg { white, gamma: 1.2 + 0.42 * (white / 1000.0).log10() },
        }
    }

    pub fn is_hdr(&self) -> bool {
        !matches!(self, GradeSpace::Sdr)
    }

    /// HDR White in cd/m² (None for SDR).
    pub fn white_nits(&self) -> Option<f32> {
        match *self {
            GradeSpace::Sdr => None,
            GradeSpace::Pq { white, .. } | GradeSpace::Hlg { white, .. } => Some(white),
        }
    }

    /// Working linear → grading signal (1 = white; HDR values above HDR White are > 1).
    #[inline]
    pub fn encode(&self, c: [f32; 3]) -> [f32; 3] {
        match *self {
            GradeSpace::Sdr => c.map(|v| linear_to_srgb(v.max(0.0))),
            GradeSpace::Pq { norm, .. } => c.map(|v| pq_inverse_eotf((v.max(0.0) * REFERENCE_WHITE_NITS as f32 / 10_000.0).min(1.0)) / norm),
            GradeSpace::Hlg { white, gamma } => {
                // display light normalised to the peak, inverse OOTF (scene light), OETF
                let fd = c.map(|v| v.max(0.0) * REFERENCE_WHITE_NITS as f32 / white);
                let l = luma2020();
                let yd = l[0] * fd[0] + l[1] * fd[1] + l[2] * fd[2];
                if yd <= 0.0 {
                    return [0.0; 3];
                }
                let k = yd.powf((1.0 - gamma) / gamma);
                fd.map(|v| hlg_oetf(v * k))
            }
        }
    }

    /// Grading signal → working linear. SDR clamps to 0…1 (white); HDR keeps values above HDR
    /// White up to 10 000 cd/m² (PQ) or the HLG signal's range.
    #[inline]
    pub fn decode(&self, v: [f32; 3]) -> [f32; 3] {
        match *self {
            GradeSpace::Sdr => v.map(|q| srgb_to_linear(q.clamp(0.0, 1.0))),
            GradeSpace::Pq { norm, .. } => v.map(|q| pq_eotf((q * norm).clamp(0.0, 1.0)) * 10_000.0 / REFERENCE_WHITE_NITS as f32),
            GradeSpace::Hlg { white, gamma } => {
                // graded highlights may go past signal 1.0 (the display peak); keep them up to
                // 1.5 (≈ 28× the peak) instead of clipping
                let es = v.map(|q| hlg_inverse_oetf(q.clamp(0.0, 1.5)));
                let l = luma2020();
                let ys = l[0] * es[0] + l[1] * es[1] + l[2] * es[2];
                if ys <= 0.0 {
                    return [0.0; 3];
                }
                let k = ys.powf(gamma - 1.0);
                es.map(|e| e * k * white / REFERENCE_WHITE_NITS as f32)
            }
        }
    }

    /// The signal of a grey of `nits` cd/m² (HDR) — what a nit-scaled slider position means.
    pub fn signal_of_nits(&self, nits: f32) -> f32 {
        let w = nits / REFERENCE_WHITE_NITS as f32;
        self.encode([w; 3])[1]
    }

    /// The cd/m² of a grey of signal `s` (HDR).
    pub fn nits_of_signal(&self, s: f32) -> f32 {
        self.decode([s; 3])[1] * REFERENCE_WHITE_NITS as f32
    }
}

fn luma2020() -> [f32; 3] {
    let l = Gamut::Bt2020.luma();
    [l[0] as f32, l[1] as f32, l[2] as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(nits: f32) -> [f32; 3] {
        [nits / REFERENCE_WHITE_NITS as f32; 3]
    }

    #[test]
    fn sdr_is_the_srgb_signal() {
        let g = GradeSpace::new(WorkingSpace::Rec709, 1000.0);
        assert!(!g.is_hdr());
        let e = g.encode([0.18, 0.5, 1.0]);
        assert!((e[0] - linear_to_srgb(0.18)).abs() < 1e-6 && (e[2] - 1.0).abs() < 1e-6);
        // values above white clip on the way back (SDR)
        assert_eq!(g.decode([1.4, 1.4, 1.4]), [1.0; 3]);
    }

    #[test]
    fn pq_known_nit_values() {
        // HDR White 10 000: the plain PQ signal; 203 cd/m² ≈ 58 % (BT.2408), 1000 cd/m² ≈ 75.2 %
        let g = GradeSpace::new(WorkingSpace::Rec2100Pq, 10_000.0);
        assert!((g.signal_of_nits(203.0) - 0.5807).abs() < 1e-3);
        assert!((g.signal_of_nits(1000.0) - 0.7518).abs() < 1e-3);
        assert!((g.signal_of_nits(100.0) - 0.5081).abs() < 1e-3);
        // HDR White 1000: 1000 cd/m² is signal 1, reference white 0.772
        let g = GradeSpace::new(WorkingSpace::Rec2100Pq, 1000.0);
        assert!((g.signal_of_nits(1000.0) - 1.0).abs() < 1e-4);
        assert!((g.signal_of_nits(203.0) - 0.5807 / 0.7518).abs() < 1e-3);
        // above HDR White: > 1, not clipped, round trip to 4000 cd/m²
        let s = g.signal_of_nits(4000.0);
        assert!(s > 1.1);
        assert!((g.nits_of_signal(s) - 4000.0).abs() < 4.0);
        for n in [0.05f32, 1.0, 26.0, 100.0, 203.0, 600.0, 1000.0, 2500.0, 10_000.0] {
            let back = g.decode(g.encode(grey(n)))[0] * REFERENCE_WHITE_NITS as f32;
            assert!((back - n).abs() <= n * 2e-3 + 1e-3, "{n} → {back}");
        }
    }

    #[test]
    fn hlg_known_nit_values() {
        // a 1000 cd/m² HLG display: reference white 203 cd/m² = 75 % (BT.2408), the peak = 100 %
        let g = GradeSpace::new(WorkingSpace::Rec2100Hlg, 1000.0);
        assert!((g.signal_of_nits(203.0) - 0.75).abs() < 2e-3, "{}", g.signal_of_nits(203.0));
        assert!((g.signal_of_nits(1000.0) - 1.0).abs() < 1e-3);
        // 18 % grey card (≈ 26 cd/m² on that display, BT.2408 table) ≈ 38 %
        assert!((g.signal_of_nits(26.0) - 0.38).abs() < 0.01, "{}", g.signal_of_nits(26.0));
        // colour round trip (the inverse OOTF depends on luminance)
        let c = [2.0f32, 0.5, 0.25];
        let back = g.decode(g.encode(c));
        assert!(c.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-3 * a.max(1.0)), "{back:?}");
        // a brighter display has a higher system gamma (BT.2100: 1.2 + 0.42 log10(Lw / 1000))
        let GradeSpace::Hlg { gamma, .. } = GradeSpace::new(WorkingSpace::Rec2100Hlg, 2000.0) else { panic!() };
        assert!((gamma - 1.2 - 0.42 * 2f32.log10()).abs() < 1e-5);
    }
}
