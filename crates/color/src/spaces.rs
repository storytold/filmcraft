//! Colour spaces: RGB primaries (and the 3×3 matrices between them), named source colour spaces
//! for "Interpret Footage ▸ Color Management", and the sequence working space.
//!
//! Every gamut here has a D65 white point, so conversions are a plain RGB→XYZ→RGB product with no
//! chromatic adaptation. Matrices follow SMPTE RP 177 (derivation of the normalised primary
//! matrix from chromaticities).

use serde::{Deserialize, Serialize};

use crate::log::LogCurve;
use crate::{ColorInfo, Primaries, Transfer};

pub type Mat3 = [[f64; 3]; 3];

pub const D65: [f64; 2] = [0.3127, 0.3290];

/// A set of RGB primaries (all D65).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Gamut {
    Bt709,
    Bt2020,
    P3D65,
    /// Sony S-Gamut3.Cine.
    SGamut3Cine,
    /// Sony S-Gamut3.
    SGamut3,
    /// Panasonic V-Gamut.
    VGamut,
    /// Canon Cinema Gamut.
    CinemaGamut,
    /// ARRI Wide Gamut 3.
    Awg3,
    /// ARRI Wide Gamut 4.
    Awg4,
    /// DJI D-Gamut.
    DGamut,
}

impl Gamut {
    /// (x, y) of R, G, B. Sources: ITU-R BT.709-6, BT.2020-2, SMPTE EG 432-1 (P3), and the camera
    /// vendors' documents listed in the crate README.
    pub fn primaries(self) -> [[f64; 2]; 3] {
        match self {
            Gamut::Bt709 => [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]],
            Gamut::Bt2020 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
            Gamut::P3D65 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
            Gamut::SGamut3Cine => [[0.766, 0.275], [0.225, 0.800], [0.089, -0.087]],
            Gamut::SGamut3 => [[0.730, 0.280], [0.140, 0.855], [0.100, -0.050]],
            Gamut::VGamut => [[0.730, 0.280], [0.165, 0.840], [0.100, -0.030]],
            Gamut::CinemaGamut => [[0.740, 0.270], [0.170, 1.140], [0.080, -0.100]],
            Gamut::Awg3 => [[0.6840, 0.3130], [0.2210, 0.8480], [0.0861, -0.1020]],
            Gamut::Awg4 => [[0.7347, 0.2653], [0.1424, 0.8576], [0.0991, -0.0308]],
            Gamut::DGamut => [[0.71, 0.31], [0.21, 0.88], [0.09, -0.08]],
        }
    }

    pub fn from_primaries(p: Primaries) -> Gamut {
        match p {
            Primaries::Bt2020 => Gamut::Bt2020,
            Primaries::P3D65 => Gamut::P3D65,
            // BT.601 primaries are close enough to BT.709 that NLEs treat them as such.
            _ => Gamut::Bt709,
        }
    }

    /// RGB → CIE XYZ (normalised so white Y = 1).
    pub fn to_xyz(self) -> Mat3 {
        npm(self.primaries(), D65)
    }

    /// Luminance weights (the Y row of [`Gamut::to_xyz`]).
    pub fn luma(self) -> [f64; 3] {
        self.to_xyz()[1]
    }
}

/// Normalised primary matrix (SMPTE RP 177).
pub fn npm(p: [[f64; 2]; 3], w: [f64; 2]) -> Mat3 {
    let xyz = |c: [f64; 2]| [c[0] / c[1], 1.0, (1.0 - c[0] - c[1]) / c[1]];
    let (r, g, b) = (xyz(p[0]), xyz(p[1]), xyz(p[2]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mul_vec(&inverse(&m), xyz(w));
    [[m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]], [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]], [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]]]
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

pub fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2], m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2], m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]]
}

pub fn inverse(m: &Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let k = 1.0 / det;
    [
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ]
}

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Linear RGB in `src` → linear RGB in `dst`.
pub fn gamut_matrix(src: Gamut, dst: Gamut) -> Mat3 {
    if src == dst {
        return IDENTITY;
    }
    mul(&inverse(&dst.to_xyz()), &src.to_xyz())
}

pub fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    m.map(|r| r.map(|v| v as f32))
}

#[inline]
pub fn apply3(m: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    [m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2], m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2], m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2]]
}

/// How a colour space's signal becomes light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Curve {
    /// Display-referred SDR (BT.709 / BT.1886 / sRGB) — decoded with the sRGB curve like the rest
    /// of the pipeline; 1.0 = SDR white.
    Sdr,
    Srgb,
    Linear,
    /// SMPTE ST 2084, absolute.
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG (scene light; the OOTF is applied separately).
    Hlg,
    Log(LogCurve),
}

/// Source colour spaces offered by Interpret Footage ▸ Color Management.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorSpace {
    Rec709,
    Srgb,
    /// Rec. 2020 SDR (BT.2020 primaries, BT.709-style transfer).
    Rec2020,
    /// Display P3 (D65) SDR.
    P3D65,
    /// Rec. 2100 PQ (BT.2020 primaries, ST 2084).
    Rec2100Pq,
    /// Rec. 2100 HLG.
    Rec2100Hlg,
    SLog3SGamut3Cine,
    SLog3SGamut3,
    VLogVGamut,
    CLog2CinemaGamut,
    CLog3CinemaGamut,
    LogC3Awg3,
    LogC4Awg4,
    /// Apple Log (BT.2020 primaries).
    AppleLog,
    DLogDGamut,
    /// Linear light with BT.709 primaries (EXR-style float sources).
    Linear709,
}

impl ColorSpace {
    pub const ALL: [ColorSpace; 16] = [
        ColorSpace::Rec709,
        ColorSpace::Srgb,
        ColorSpace::Rec2020,
        ColorSpace::P3D65,
        ColorSpace::Rec2100Pq,
        ColorSpace::Rec2100Hlg,
        ColorSpace::SLog3SGamut3Cine,
        ColorSpace::SLog3SGamut3,
        ColorSpace::VLogVGamut,
        ColorSpace::CLog2CinemaGamut,
        ColorSpace::CLog3CinemaGamut,
        ColorSpace::LogC3Awg3,
        ColorSpace::LogC4Awg4,
        ColorSpace::AppleLog,
        ColorSpace::DLogDGamut,
        ColorSpace::Linear709,
    ];

    /// Stable id used in commands and project files.
    pub fn id(self) -> &'static str {
        match self {
            ColorSpace::Rec709 => "rec709",
            ColorSpace::Srgb => "srgb",
            ColorSpace::Rec2020 => "rec2020",
            ColorSpace::P3D65 => "p3d65",
            ColorSpace::Rec2100Pq => "rec2100-pq",
            ColorSpace::Rec2100Hlg => "rec2100-hlg",
            ColorSpace::SLog3SGamut3Cine => "slog3-sgamut3cine",
            ColorSpace::SLog3SGamut3 => "slog3-sgamut3",
            ColorSpace::VLogVGamut => "vlog-vgamut",
            ColorSpace::CLog2CinemaGamut => "clog2-cinemagamut",
            ColorSpace::CLog3CinemaGamut => "clog3-cinemagamut",
            ColorSpace::LogC3Awg3 => "logc3-awg3",
            ColorSpace::LogC4Awg4 => "logc4-awg4",
            ColorSpace::AppleLog => "applelog",
            ColorSpace::DLogDGamut => "dlog-dgamut",
            ColorSpace::Linear709 => "linear709",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ColorSpace::Rec709 => "Rec. 709",
            ColorSpace::Srgb => "sRGB",
            ColorSpace::Rec2020 => "Rec. 2020",
            ColorSpace::P3D65 => "P3 D65",
            ColorSpace::Rec2100Pq => "Rec. 2100 PQ",
            ColorSpace::Rec2100Hlg => "Rec. 2100 HLG",
            ColorSpace::SLog3SGamut3Cine => "Sony S-Log3 / S-Gamut3.Cine",
            ColorSpace::SLog3SGamut3 => "Sony S-Log3 / S-Gamut3",
            ColorSpace::VLogVGamut => "Panasonic V-Log / V-Gamut",
            ColorSpace::CLog2CinemaGamut => "Canon Log 2 / Cinema Gamut",
            ColorSpace::CLog3CinemaGamut => "Canon Log 3 / Cinema Gamut",
            ColorSpace::LogC3Awg3 => "ARRI LogC3 / ARRI Wide Gamut 3",
            ColorSpace::LogC4Awg4 => "ARRI LogC4 / ARRI Wide Gamut 4",
            ColorSpace::AppleLog => "Apple Log / Rec. 2020",
            ColorSpace::DLogDGamut => "DJI D-Log / D-Gamut",
            ColorSpace::Linear709 => "Linear (Rec. 709 primaries)",
        }
    }

    /// Parse an id or a label (case-insensitive, punctuation ignored).
    pub fn parse(s: &str) -> Option<ColorSpace> {
        let norm = |x: &str| x.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let n = norm(s);
        ColorSpace::ALL.into_iter().find(|c| norm(c.id()) == n || norm(c.label()) == n)
    }

    pub fn gamut(self) -> Gamut {
        match self {
            ColorSpace::Rec709 | ColorSpace::Srgb | ColorSpace::Linear709 => Gamut::Bt709,
            ColorSpace::Rec2020 | ColorSpace::Rec2100Pq | ColorSpace::Rec2100Hlg | ColorSpace::AppleLog => Gamut::Bt2020,
            ColorSpace::P3D65 => Gamut::P3D65,
            ColorSpace::SLog3SGamut3Cine => Gamut::SGamut3Cine,
            ColorSpace::SLog3SGamut3 => Gamut::SGamut3,
            ColorSpace::VLogVGamut => Gamut::VGamut,
            ColorSpace::CLog2CinemaGamut | ColorSpace::CLog3CinemaGamut => Gamut::CinemaGamut,
            ColorSpace::LogC3Awg3 => Gamut::Awg3,
            ColorSpace::LogC4Awg4 => Gamut::Awg4,
            ColorSpace::DLogDGamut => Gamut::DGamut,
        }
    }

    pub fn curve(self) -> Curve {
        match self {
            ColorSpace::Rec709 | ColorSpace::Rec2020 | ColorSpace::P3D65 => Curve::Sdr,
            ColorSpace::Srgb => Curve::Srgb,
            ColorSpace::Linear709 => Curve::Linear,
            ColorSpace::Rec2100Pq => Curve::Pq,
            ColorSpace::Rec2100Hlg => Curve::Hlg,
            ColorSpace::SLog3SGamut3Cine | ColorSpace::SLog3SGamut3 => Curve::Log(LogCurve::SLog3),
            ColorSpace::VLogVGamut => Curve::Log(LogCurve::VLog),
            ColorSpace::CLog2CinemaGamut => Curve::Log(LogCurve::CLog2),
            ColorSpace::CLog3CinemaGamut => Curve::Log(LogCurve::CLog3),
            ColorSpace::LogC3Awg3 => Curve::Log(LogCurve::LogC3),
            ColorSpace::LogC4Awg4 => Curve::Log(LogCurve::LogC4),
            ColorSpace::AppleLog => Curve::Log(LogCurve::AppleLog),
            ColorSpace::DLogDGamut => Curve::Log(LogCurve::DLog),
        }
    }

    /// Carries more than SDR range (needs tone mapping into an SDR working space).
    pub fn is_hdr(self) -> bool {
        matches!(self.curve(), Curve::Pq | Curve::Hlg | Curve::Log(_))
    }
    pub fn is_log(self) -> bool {
        matches!(self.curve(), Curve::Log(_))
    }

    /// The colour space signalled by stream metadata (VUI / `colr` / MKV Colour).
    pub fn from_info(c: &ColorInfo) -> ColorSpace {
        match (c.transfer, c.primaries) {
            (Transfer::AppleLog, _) => ColorSpace::AppleLog,
            (Transfer::Pq, _) => ColorSpace::Rec2100Pq,
            (Transfer::Hlg, _) => ColorSpace::Rec2100Hlg,
            (Transfer::Linear, _) => ColorSpace::Linear709,
            (Transfer::Srgb, Primaries::P3D65) => ColorSpace::P3D65,
            (Transfer::Srgb, _) => ColorSpace::Srgb,
            (_, Primaries::Bt2020) => ColorSpace::Rec2020,
            (_, Primaries::P3D65) => ColorSpace::P3D65,
            _ => ColorSpace::Rec709,
        }
    }
}

/// Sequence working colour space (Sequence Settings ▸ Color).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkingSpace {
    #[default]
    Rec709,
    Rec2100Pq,
    Rec2100Hlg,
}

impl WorkingSpace {
    pub const ALL: [WorkingSpace; 3] = [WorkingSpace::Rec709, WorkingSpace::Rec2100Pq, WorkingSpace::Rec2100Hlg];

    pub fn id(self) -> &'static str {
        match self {
            WorkingSpace::Rec709 => "rec709",
            WorkingSpace::Rec2100Pq => "rec2100-pq",
            WorkingSpace::Rec2100Hlg => "rec2100-hlg",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            WorkingSpace::Rec709 => "Rec. 709",
            WorkingSpace::Rec2100Pq => "Rec. 2100 PQ",
            WorkingSpace::Rec2100Hlg => "Rec. 2100 HLG",
        }
    }
    pub fn parse(s: &str) -> Option<WorkingSpace> {
        let norm = |x: &str| x.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let n = norm(s);
        WorkingSpace::ALL.into_iter().find(|w| norm(w.id()) == n || norm(w.label()) == n)
    }
    pub fn is_hdr(self) -> bool {
        self != WorkingSpace::Rec709
    }
    /// The colour space exports of this sequence are encoded in.
    pub fn output_space(self) -> ColorSpace {
        match self {
            WorkingSpace::Rec709 => ColorSpace::Rec709,
            WorkingSpace::Rec2100Pq => ColorSpace::Rec2100Pq,
            WorkingSpace::Rec2100Hlg => ColorSpace::Rec2100Hlg,
        }
    }
}

/// Everything the renderer needs to know about a sequence's colour pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorPipeline {
    pub working: WorkingSpace,
    /// Composite in linear BT.2020 primaries even for a Rec. 709 sequence.
    pub wide_gamut: bool,
    /// Tone map HDR and log media into an SDR working space (Auto Tone Map Media).
    pub auto_tone_map: bool,
}

impl ColorPipeline {
    pub const REC709: ColorPipeline = ColorPipeline { working: WorkingSpace::Rec709, wide_gamut: false, auto_tone_map: true };

    /// Linear primaries of the compositing space.
    pub fn working_gamut(&self) -> Gamut {
        if self.working.is_hdr() || self.wide_gamut { Gamut::Bt2020 } else { Gamut::Bt709 }
    }
    /// The classic pipeline: linear BT.709, display-referred SDR.
    pub fn is_plain(&self) -> bool {
        !self.working.is_hdr() && !self.wide_gamut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt709_npm_matches_published_matrix() {
        // ITU-R BT.709 / sRGB RGB→XYZ (IEC 61966-2-1 rounds to 4 places).
        let m = Gamut::Bt709.to_xyz();
        let want = [[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]];
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - want[i][j]).abs() < 1e-4, "{i}{j}: {}", m[i][j]);
            }
        }
        // BT.2020 luma coefficients (BT.2020-2 table 4).
        let l = Gamut::Bt2020.luma();
        assert!((l[0] - 0.2627).abs() < 1e-4 && (l[1] - 0.6780).abs() < 1e-4 && (l[2] - 0.0593).abs() < 1e-4);
    }

    #[test]
    fn bt2020_to_709_matches_bt2087() {
        // ITU-R BT.2087-0, linear BT.2020 → BT.709 (inverse of the published 709→2020 matrix).
        let m = gamut_matrix(Gamut::Bt709, Gamut::Bt2020);
        let want = [[0.6274, 0.3293, 0.0433], [0.0691, 0.9195, 0.0114], [0.0164, 0.0880, 0.8956]];
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - want[i][j]).abs() < 1e-4, "{i}{j}: {}", m[i][j]);
            }
        }
        // white maps to white for every gamut pair
        for a in [Gamut::SGamut3Cine, Gamut::Awg4, Gamut::CinemaGamut, Gamut::DGamut, Gamut::VGamut] {
            let w = mul_vec(&gamut_matrix(a, Gamut::Bt709), [1.0, 1.0, 1.0]);
            assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-9), "{a:?}: {w:?}");
            let back = mul(&gamut_matrix(Gamut::Bt709, a), &gamut_matrix(a, Gamut::Bt709));
            for i in 0..3 {
                for j in 0..3 {
                    assert!((back[i][j] - IDENTITY[i][j]).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn ids_parse() {
        for c in ColorSpace::ALL {
            assert_eq!(ColorSpace::parse(c.id()), Some(c));
            assert_eq!(ColorSpace::parse(c.label()), Some(c));
        }
        assert_eq!(WorkingSpace::parse("Rec. 2100 PQ"), Some(WorkingSpace::Rec2100Pq));
        assert_eq!(WorkingSpace::parse("rec709"), Some(WorkingSpace::Rec709));
        let hdr = ColorInfo { transfer: Transfer::Hlg, primaries: Primaries::Bt2020, ..ColorInfo::REC709 };
        assert_eq!(ColorSpace::from_info(&hdr), ColorSpace::Rec2100Hlg);
        assert_eq!(ColorSpace::from_info(&ColorInfo::REC709), ColorSpace::Rec709);
    }
}
