//! Camera colour to linear Rec. 709, following the DNG specification's "Mapping Camera Color Space
//! to CIE XYZ Space" (DNG 1.4–1.7 chapter 6):
//!
//! - The two calibrations (ColorMatrix, CameraCalibration, ForwardMatrix 1 and 2) are blended
//!   linearly in inverse correlated colour temperature between their CalibrationIlluminants.
//! - The white balance is AsShotNeutral (found as a white xy by the specification's iteration) or
//!   AsShotWhiteXY; without either, D65.
//! - With ForwardMatrix: CameraToXYZ_D50 = FM · D · (AB · CC)⁻¹, D = diag((AB · CC)⁻¹ · neutral)⁻¹.
//!   Without: CameraToXYZ_D50 = CA · (AB · CC · CM)⁻¹, CA a linear Bradford adaptation from the
//!   white to D50.
//! - XYZ (D50) goes to linear Rec. 709 / sRGB primaries (D65 white) through Bradford again.
//!
//! The correlated colour temperature of an xy white comes from McCamy's cubic approximation
//! (C. S. McCamy, "Correlated color temperature as an explicit function of chromaticity
//! coordinates", Color Research & Application 17(2), 1992); the illuminant temperatures are the
//! CIE / EXIF 2.3 LightSource values.

use crate::{Error, Result};

pub type M3 = [[f64; 3]; 3];
pub type V3 = [f64; 3];

pub const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// CIE D50 and D65 white chromaticities (the DNG specification's D50).
pub const D50: (f64, f64) = (0.3457, 0.3585);
pub const D65: (f64, f64) = (0.3127, 0.3290);

/// Linear Bradford cone response matrix.
const BRADFORD: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];

pub fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn mulv(a: &M3, v: &V3) -> V3 {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

pub fn diag(v: &V3) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

pub fn scale(a: &M3, s: f64) -> M3 {
    a.map(|r| r.map(|v| v * s))
}

fn blend(a: &M3, b: &M3, w: f64) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = w * a[i][j] + (1.0 - w) * b[i][j];
        }
    }
    m
}

/// The inverse of `a`, or `None` when it is (nearly) singular.
pub fn inverse(a: &M3) -> Option<M3> {
    let c = |r: usize, s: usize| a[(r + 1) % 3][(s + 1) % 3] * a[(r + 2) % 3][(s + 2) % 3] - a[(r + 1) % 3][(s + 2) % 3] * a[(r + 2) % 3][(s + 1) % 3];
    let det = a[0][0] * c(0, 0) + a[0][1] * c(0, 1) + a[0][2] * c(0, 2);
    let norm: f64 = a.iter().flatten().map(|v| v.abs()).fold(0.0, f64::max);
    if !det.is_finite() || det.abs() <= 1e-12 * norm.powi(3).max(1e-300) {
        return None;
    }
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c(j, i) / det;
        }
    }
    Some(m)
}

/// XYZ (Y = 1) of a chromaticity.
pub fn xy_to_xyz((x, y): (f64, f64)) -> V3 {
    let y = y.max(1e-6);
    [x / y, 1.0, (1.0 - x - y) / y]
}

pub fn xyz_to_xy(v: &V3) -> Option<(f64, f64)> {
    let s = v[0] + v[1] + v[2];
    (s.is_finite() && s > 1e-12).then(|| (v[0] / s, v[1] / s))
}

/// Correlated colour temperature (kelvin) of a chromaticity, McCamy (1992), limited to
/// 1 000–50 000 K.
pub fn cct((x, y): (f64, f64)) -> f64 {
    let d = 0.1858 - y;
    if d.abs() < 1e-9 {
        return 50_000.0;
    }
    let n = (x - 0.3320) / d;
    let t = 449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33;
    if t.is_finite() { t.clamp(1_000.0, 50_000.0) } else { 50_000.0 }
}

/// Colour temperature of an EXIF LightSource code used as a DNG CalibrationIlluminant.
pub fn illuminant_kelvin(code: u64) -> Option<f64> {
    Some(match code {
        1 | 4 | 9 => 5500.0, // daylight, flash, fine weather
        2 | 14 => 4150.0,    // fluorescent, cool white fluorescent (W 3800–4500 K)
        3 => 2850.0,         // tungsten
        10 => 6500.0,        // cloudy
        11 => 7500.0,        // shade
        12 => 6430.0,        // daylight fluorescent (D 5700–7100 K)
        13 => 5050.0,        // day white fluorescent (N 4600–5500 K)
        15 => 3525.0,        // white fluorescent (WW 3250–3800 K)
        16 => 2925.0,        // warm white fluorescent (L 2600–3250 K)
        17 => 2856.0,        // standard light A
        18 => 4874.0,        // standard light B
        19 => 6774.0,        // standard light C
        20 => 5503.0,        // D55
        21 => 6504.0,        // D65
        22 => 7504.0,        // D75
        23 => 5003.0,        // D50
        24 => 3200.0,        // ISO studio tungsten
        _ => return None,
    })
}

/// Linear Bradford adaptation from one white (XYZ) to another.
pub fn bradford(src: &V3, dst: &V3) -> Option<M3> {
    let s = mulv(&BRADFORD, src);
    let d = mulv(&BRADFORD, dst);
    if s.iter().any(|v| v.abs() < 1e-12) {
        return None;
    }
    let g = [d[0] / s[0], d[1] / s[1], d[2] / s[2]];
    Some(mul(&inverse(&BRADFORD)?, &mul(&diag(&g), &BRADFORD)))
}

/// Linear RGB (Rec. 709 / sRGB primaries, D65 white) → XYZ.
pub fn rec709_to_xyz() -> Option<M3> {
    let p = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)].map(xy_to_xyz);
    let pm: M3 = [[p[0][0], p[1][0], p[2][0]], [p[0][1], p[1][1], p[2][1]], [p[0][2], p[1][2], p[2][2]]];
    let s = mulv(&inverse(&pm)?, &xy_to_xyz(D65));
    Some(mul(&pm, &diag(&s)))
}

/// XYZ relative to D50 → linear Rec. 709.
pub fn xyz_d50_to_rec709() -> Option<M3> {
    let to_d65 = bradford(&xy_to_xyz(D50), &xy_to_xyz(D65))?;
    Some(mul(&inverse(&rec709_to_xyz()?)?, &to_d65))
}

/// The colour tags of a three-colour DNG.
#[derive(Clone, Debug, Default)]
pub struct Calibration {
    pub color_matrix: [Option<M3>; 2],
    pub camera_calibration: [Option<M3>; 2],
    pub forward_matrix: [Option<M3>; 2],
    /// EXIF LightSource codes of the two calibrations.
    pub illuminant: [Option<u64>; 2],
    pub analog_balance: Option<V3>,
    pub as_shot_neutral: Option<V3>,
    pub as_shot_white_xy: Option<(f64, f64)>,
    pub baseline_exposure: f64,
}

/// The result of [`Calibration::solve`].
#[derive(Clone, Debug, PartialEq)]
pub struct Solved {
    /// Camera neutral: raw (normalised) camera values of a white surface. Dividing by it white
    /// balances the camera values.
    pub neutral: V3,
    /// White-balanced camera values → linear Rec. 709, including the baseline exposure. The white
    /// balanced white (1, 1, 1) goes to Y = 2^BaselineExposure.
    pub matrix: M3,
    /// The white point found, for information.
    pub white_xy: (f64, f64),
}

impl Calibration {
    /// Weight of calibration 1 for a white of temperature `t` (1 when only one is usable).
    fn weight(&self, t: f64) -> f64 {
        let (Some(_), Some(_)) = (self.color_matrix[0], self.color_matrix[1]) else { return 1.0 };
        let (Some(t1), Some(t2)) = (self.illuminant[0].and_then(illuminant_kelvin), self.illuminant[1].and_then(illuminant_kelvin)) else {
            return 1.0;
        };
        if (t1 - t2).abs() < 1.0 {
            return 1.0;
        }
        // the formula needs t1 < t2; swap the roles otherwise
        let (lo, hi, lo_is_1) = if t1 < t2 { (t1, t2, true) } else { (t2, t1, false) };
        let w_lo = if t <= lo {
            1.0
        } else if t >= hi {
            0.0
        } else {
            (1.0 / t - 1.0 / hi) / (1.0 / lo - 1.0 / hi)
        };
        if lo_is_1 { w_lo } else { 1.0 - w_lo }
    }

    fn pick(&self, m: &[Option<M3>; 2], w: f64, default: M3) -> M3 {
        match (m[0], m[1]) {
            (Some(a), Some(b)) => blend(&a, &b, w),
            (Some(a), None) => a,
            (None, Some(b)) if self.color_matrix[0].is_none() => b,
            _ => default,
        }
    }

    /// XYZ → camera at weight `w`: AB · CC · CM.
    fn xyz_to_camera(&self, w: f64) -> Option<M3> {
        let cm = match self.color_matrix {
            [None, None] => return None,
            _ => self.pick(&self.color_matrix, w, IDENTITY),
        };
        let cc = self.pick(&self.camera_calibration, w, IDENTITY);
        let ab = diag(&self.analog_balance.unwrap_or([1.0; 3]));
        Some(mul(&ab, &mul(&cc, &cm)))
    }

    /// Work out the white balance and the camera → linear Rec. 709 matrix.
    pub fn solve(&self) -> Result<Solved> {
        let out_matrix = xyz_d50_to_rec709().ok_or(Error::Malformed("colour maths"))?;
        let exposure = 2f64.powf(self.baseline_exposure.clamp(-16.0, 16.0));
        if self.color_matrix.iter().all(Option::is_none) {
            // no colour calibration: the camera values are taken as Rec. 709, white balanced
            let neutral = self.as_shot_neutral.unwrap_or([1.0; 3]);
            if neutral.iter().any(|v| !v.is_finite() || *v <= 1e-9) {
                return Err(Error::Malformed("camera neutral must be positive"));
            }
            return Ok(Solved { neutral, matrix: scale(&IDENTITY, exposure), white_xy: D65 });
        }
        let bad = || Error::Malformed("singular colour matrix");
        // the white point and the camera neutral
        let (white_xy, neutral) = if let Some(n) = self.as_shot_neutral {
            let mut xy = D50;
            for _ in 0..50 {
                let w = self.weight(cct(xy));
                let to_xyz = inverse(&self.xyz_to_camera(w).ok_or_else(bad)?).ok_or_else(bad)?;
                let next = xyz_to_xy(&mulv(&to_xyz, &n)).ok_or(Error::Malformed("AsShotNeutral is not a colour"))?;
                let done = (next.0 - xy.0).abs() + (next.1 - xy.1).abs() < 1e-9;
                xy = next;
                if done {
                    break;
                }
            }
            (xy, n)
        } else {
            let xy = self.as_shot_white_xy.unwrap_or(D65);
            let w = self.weight(cct(xy));
            let n = mulv(&self.xyz_to_camera(w).ok_or_else(bad)?, &xy_to_xyz(xy));
            (xy, n)
        };
        if neutral.iter().any(|v| !v.is_finite() || *v <= 1e-9) {
            return Err(Error::Malformed("camera neutral must be positive"));
        }
        let w = self.weight(cct(white_xy));
        let ab_cc = mul(&diag(&self.analog_balance.unwrap_or([1.0; 3])), &self.pick(&self.camera_calibration, w, IDENTITY));
        let camera_to_xyz_d50 = if self.forward_matrix.iter().any(Option::is_some) {
            let fm = self.pick(&self.forward_matrix, w, IDENTITY);
            let inv_ab_cc = inverse(&ab_cc).ok_or_else(bad)?;
            let reference = mulv(&inv_ab_cc, &neutral);
            if reference.iter().any(|v| v.abs() < 1e-12) {
                return Err(bad());
            }
            mul(&fm, &mul(&diag(&reference.map(|v| 1.0 / v)), &inv_ab_cc))
        } else {
            let to_xyz = inverse(&self.xyz_to_camera(w).ok_or_else(bad)?).ok_or_else(bad)?;
            let ca = bradford(&xy_to_xyz(white_xy), &xy_to_xyz(D50)).ok_or_else(bad)?;
            mul(&ca, &to_xyz)
        };
        // white-balanced camera values: camera = neutral · wb
        let wb_to_xyz = mul(&camera_to_xyz_d50, &diag(&neutral));
        let y = mulv(&wb_to_xyz, &[1.0; 3])[1];
        if !y.is_finite() || y <= 1e-9 {
            return Err(bad());
        }
        let matrix = scale(&mul(&out_matrix, &wb_to_xyz), exposure / y);
        if matrix.iter().flatten().any(|v| !v.is_finite()) {
            return Err(bad());
        }
        Ok(Solved { neutral, matrix, white_xy })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &M3, b: &M3, tol: f64) -> bool {
        a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn rec709_matrix_matches_the_published_one() {
        // IEC 61966-2-1 (sRGB) linear RGB → XYZ, four decimals
        let m = rec709_to_xyz().unwrap();
        let want = [[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]];
        assert!(close(&m, &want, 1e-3), "{m:?}");
    }

    #[test]
    fn inverse_round_trips_and_refuses_singular() {
        let a = [[2.0, 1.0, 0.5], [0.1, 3.0, 0.2], [0.3, 0.4, 1.5]];
        assert!(close(&mul(&a, &inverse(&a).unwrap()), &IDENTITY, 1e-12));
        assert!(inverse(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]).is_none());
        assert!(inverse(&[[f64::NAN; 3]; 3]).is_none());
    }

    #[test]
    fn mccamy_temperatures() {
        assert!((cct(D65) - 6504.0).abs() < 15.0, "{}", cct(D65));
        assert!((cct(D50) - 5003.0).abs() < 15.0, "{}", cct(D50));
        assert!((cct((0.44757, 0.40745)) - 2856.0).abs() < 15.0, "illuminant A");
        assert_eq!(cct((0.3320, 0.1858)), 50_000.0, "no division by zero");
    }

    #[test]
    fn interpolation_weight_follows_inverse_temperature() {
        let c = Calibration { color_matrix: [Some(IDENTITY), Some(IDENTITY)], illuminant: [Some(17), Some(21)], ..Default::default() };
        assert_eq!(c.weight(2000.0), 1.0);
        assert_eq!(c.weight(9000.0), 0.0);
        let mid = c.weight(4000.0);
        let want = (1.0 / 4000.0 - 1.0 / 6504.0) / (1.0 / 2856.0 - 1.0 / 6504.0);
        assert!((mid - want).abs() < 1e-12);
        // swapped order: weight of calibration 1 (now D65)
        let s = Calibration { illuminant: [Some(21), Some(17)], ..c.clone() };
        assert!((s.weight(4000.0) - (1.0 - want)).abs() < 1e-12);
        // unknown illuminant: calibration 1 only
        let u = Calibration { illuminant: [Some(0), Some(21)], ..c };
        assert_eq!(u.weight(4000.0), 1.0);
    }

    #[test]
    fn an_srgb_camera_maps_to_itself() {
        // ColorMatrix = XYZ → linear sRGB, white D65: the camera is Rec. 709 already
        let cm = inverse(&rec709_to_xyz().unwrap()).unwrap();
        let c = Calibration { color_matrix: [Some(cm), None], illuminant: [Some(21), None], as_shot_neutral: Some([1.0; 3]), ..Default::default() };
        let s = c.solve().unwrap();
        assert!(close(&s.matrix, &IDENTITY, 1e-6), "{:?}", s.matrix);
        assert!((s.white_xy.0 - D65.0).abs() < 1e-4 && (s.white_xy.1 - D65.1).abs() < 1e-4, "{:?}", s.white_xy);
        // a baseline exposure of +1 doubles everything
        let e = Calibration { baseline_exposure: 1.0, ..c }.solve().unwrap();
        assert!(close(&e.matrix, &scale(&IDENTITY, 2.0), 1e-6));
    }

    #[test]
    fn forward_matrix_white_balances_by_the_neutral() {
        // FM = linear Rec. 709 → XYZ D50: the white-balanced camera values come out unchanged
        let fm = mul(&bradford(&xy_to_xyz(D65), &xy_to_xyz(D50)).unwrap(), &rec709_to_xyz().unwrap());
        let cm = inverse(&rec709_to_xyz().unwrap()).unwrap();
        let c = Calibration {
            color_matrix: [Some(cm), None],
            forward_matrix: [Some(fm), None],
            illuminant: [Some(21), None],
            as_shot_neutral: Some([0.5, 1.0, 0.8]),
            ..Default::default()
        };
        let s = c.solve().unwrap();
        assert_eq!(s.neutral, [0.5, 1.0, 0.8]);
        assert!(close(&s.matrix, &IDENTITY, 1e-6), "{:?}", s.matrix);
    }

    #[test]
    fn white_xy_gives_the_neutral() {
        let cm = inverse(&rec709_to_xyz().unwrap()).unwrap();
        let c = Calibration { color_matrix: [Some(cm), None], illuminant: [Some(21), None], as_shot_white_xy: Some(D65), ..Default::default() };
        let s = c.solve().unwrap();
        for v in s.neutral {
            assert!((v - s.neutral[1]).abs() < 1e-3, "D65 is neutral for an sRGB camera: {:?}", s.neutral);
        }
    }

    #[test]
    fn hostile_calibrations_are_errors() {
        let zero = Calibration { color_matrix: [Some([[0.0; 3]; 3]), None], as_shot_neutral: Some([1.0; 3]), ..Default::default() };
        assert!(zero.solve().is_err());
        let cm = inverse(&rec709_to_xyz().unwrap()).unwrap();
        let neg = Calibration { color_matrix: [Some(cm), None], as_shot_neutral: Some([-1.0, 1.0, 1.0]), ..Default::default() };
        assert!(neg.solve().is_err());
        let none = Calibration { as_shot_neutral: Some([0.5, 1.0, 0.5]), ..Default::default() }.solve().unwrap();
        assert_eq!(none.matrix, IDENTITY);
    }
}
