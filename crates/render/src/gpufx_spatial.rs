//! Typed spatial GPU effects. This module is intentionally not registered here; `gpufx` owns
//! dispatch and adds `FxOp::Spatial` once family integration is reviewed.

use filmcraft_project::EffectInstance;

use crate::{
    effects::{FxCtx, b, choice, f},
    image::Image,
};

#[derive(Clone, Debug, PartialEq)]
pub enum SpatialOp {
    Mosaic {
        bx: f32,
        by: f32,
        softness: f32,
        nx: u32,
        ny: u32,
        legacy: bool,
    },
    Replicate {
        count: u32,
    },
    Twirl {
        cx: f64,
        cy: f64,
        angle: f64,
        radius: f64,
    },
    Spherize {
        cx: f32,
        cy: f32,
        radius: f32,
    },
    Magnify {
        cx: f32,
        cy: f32,
        magnification: f32,
        radius: f32,
        feather: f32,
        opacity: f32,
        square: bool,
        mode: u32,
    },
    LensDistortion {
        cx: f64,
        cy: f64,
        k: f64,
        norm: f64,
    },
    WaveWarp {
        height: f64,
        width: f64,
        dir: f64,
        phase: f64,
    },
    TurbulentDisplace {
        kind: u32,
        amount: f32,
        size: f32,
        ox: f32,
        oy: f32,
        complexity: f32,
        seed: u64,
        ex: f32,
        ey: f32,
        ez: f32,
        pin: u32,
    },
    CornerPin {
        inv: [f32; 9],
        w: f32,
        h: f32,
    },
    /// A plane seen through a pinhole camera (3D Rotate, Basic 3D): `m` maps an output pixel to a source
    /// pixel as a homography; `nc` is the plane normal dotted with its centre, whose sign against the
    /// divisor tells whether the plane is in front of the camera.
    Projective3d {
        m: [f32; 9],
        nc: f32,
    },
    RoundedCrop {
        l: f32,
        t: f32,
        r: f32,
        b: f32,
        radius: f32,
        feather: f32,
        border: f32,
        color: [f32; 3],
    },
    RgbSplit {
        amount: f32,
        radial: bool,
        angle: f32,
        cx: f32,
        cy: f32,
        blend: f32,
        maxd: f32,
    },
    VrChromatic {
        aberration: [f32; 3],
        center: [f32; 2],
        falloff: f32,
        inverse: bool,
        layout: u32,
    },
    VrColorGradients {
        points: [[f32; 2]; 3],
        colors: [[f32; 3]; 3],
        blend: f32,
        opacity: f32,
        mode: u32,
        layout: u32,
    },
    VrProjection {
        matrix: [f64; 9],
        kx: f64,
        ky: f64,
        layout: u32,
        identity: bool,
        projection: bool,
    },
    VrPlaneToSphere {
        matrix: [f64; 9],
        tx: f64,
        feather: f32,
        layout: u32,
    },
}

impl SpatialOp {
    pub fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<Self> {
        if !e.enabled || w == 0 || h == 0 {
            return None;
        }
        let (wf, hf) = (w as f32, h as f32);
        Some(match e.effect.as_str() {
            "mosaic" | "mosaic_legacy" => {
                let bx = f(e, "horizontal", cx).max(1.0) as usize as f32;
                let by = f(e, "vertical", cx).max(1.0) as usize as f32;
                let bw = (wf / bx).max(1.0);
                let bh = (hf / by).max(1.0);
                Self::Mosaic {
                    bx,
                    by,
                    softness: if e.effect == "mosaic_legacy" && b(e, "sharp") { 0.0 } else { (f(e, "softness", cx) / 100.0).clamp(0.0, 1.0) },
                    nx: (wf / bw).ceil() as u32,
                    ny: (hf / bh).ceil() as u32,
                    legacy: e.effect == "mosaic_legacy",
                }
            }
            "replicate" => Self::Replicate { count: f(e, "count", cx).clamp(1.0, 16.0) as u32 },
            "twirl" | "twirl_legacy" => {
                let c = crate::vfx::pv(e, "center", cx, &Image { w, h, px: Vec::new() });
                let rad = f(e, "radius", cx) as f64 / 100.0 * w.min(h) as f64;
                Self::Twirl { cx: c.x, cy: c.y, angle: (f(e, "angle", cx) as f64).to_radians(), radius: rad }
            }
            "spherize" => {
                let c = crate::vfx::pv(e, "center", cx, &Image { w, h, px: Vec::new() });
                Self::Spherize { cx: c.x as f32, cy: c.y as f32, radius: f(e, "radius", cx) * cx.px_scale }
            }
            "magnify" | "magnify_legacy" => {
                let c = crate::vfx::pv(e, "center", cx, &Image { w, h, px: Vec::new() });
                Self::Magnify {
                    cx: c.x as f32,
                    cy: c.y as f32,
                    magnification: (f(e, "magnification", cx) / 100.0).max(0.01),
                    radius: f(e, "size", cx) * cx.px_scale,
                    feather: f(e, "feather", cx) * cx.px_scale,
                    opacity: f(e, "opacity", cx) / 100.0,
                    square: choice(e, "shape") == 1,
                    mode: choice(e, "mode"),
                }
            }
            "lens_distortion" => {
                let (wf, hf) = (w as f64, h as f64);
                let x = wf * 0.5 + f(e, "h_decentering", cx) as f64 / 100.0 * wf * 0.5;
                let y = hf * 0.5 + f(e, "v_decentering", cx) as f64 / 100.0 * hf * 0.5;
                Self::LensDistortion { cx: x, cy: y, k: f(e, "curvature", cx) as f64 / 100.0, norm: (wf * 0.5).hypot(hf * 0.5) }
            }
            "wave_warp" => Self::WaveWarp {
                height: f(e, "height", cx) as f64 * cx.px_scale as f64,
                width: (f(e, "width", cx) as f64 * cx.px_scale as f64).max(1.0),
                dir: (f(e, "direction", cx) as f64).to_radians(),
                phase: cx.seconds * f(e, "speed", cx) as f64 * std::f64::consts::TAU,
            },
            "turbulent_displace" => {
                let size = (f(e, "size", cx) * cx.px_scale).max(0.5);
                let o = crate::vfx::pv(e, "offset", cx, &Image { w, h, px: Vec::new() });
                let mut complexity = f(e, "complexity", cx).clamp(1.0, 10.0);
                let kind = choice(e, "displacement");
                if (3..=5).contains(&kind) {
                    complexity = (complexity * 0.5).max(1.0)
                }
                let evolution = f(e, "evolution", cx) / 360.0;
                let (ex, ey, ez) = if b(e, "cycle") {
                    let revs = f(e, "cycle_revs", cx).max(1.0);
                    let th = (evolution.rem_euclid(revs) / revs) * std::f32::consts::TAU;
                    (th.cos() * 0.8, th.sin() * 0.8, 0.0)
                } else {
                    (0.0, 0.0, evolution)
                };
                Self::TurbulentDisplace {
                    kind,
                    amount: f(e, "amount", cx) * cx.px_scale,
                    size,
                    ox: ((o.x - wf as f64 * 0.5) / size as f64) as f32,
                    oy: ((o.y - hf as f64 * 0.5) / size as f64) as f32,
                    complexity,
                    seed: f(e, "seed", cx).max(0.0) as u64,
                    ex,
                    ey,
                    ez,
                    pin: choice(e, "pinning"),
                }
            }
            "corner_pin" => {
                let pts = [
                    crate::vfx::pv(e, "upper_left", cx, &Image { w, h, px: Vec::new() }),
                    crate::vfx::pv(e, "upper_right", cx, &Image { w, h, px: Vec::new() }),
                    crate::vfx::pv(e, "lower_right", cx, &Image { w, h, px: Vec::new() }),
                    crate::vfx::pv(e, "lower_left", cx, &Image { w, h, px: Vec::new() }),
                ];
                let m = spatial_square_to_quad(pts);
                let inv = spatial_h_inverse(&m)?;
                Self::CornerPin { inv: inv.map(|x| x as f32), w: wf, h: hf }
            }
            "rotate_3d" | "basic_3d" => {
                let (wd, hd) = (w as f64, h as f64);
                let plane = if e.effect == "rotate_3d" {
                    match crate::vfx::rotate_3d_plane(e, cx, wd, hd) {
                        // untouched: the identity homography resamples every pixel at its own centre
                        crate::vfx::Rotate3d::Identity => {
                            crate::vfx::Plane3d { f: wd.max(hd), cz: wd.max(hd), n: [0.0, 0.0, 1.0], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0] }
                        }
                        // facing away with Hide Backside: every pixel is cleared (nc = 0 means "never in front")
                        crate::vfx::Rotate3d::Hidden => return Some(Self::Projective3d { m: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0], nc: 0.0 }),
                        crate::vfx::Rotate3d::Plane(plane) => plane,
                    }
                } else {
                    crate::effects::basic_3d_plane(e, cx, wd, hd)
                };
                // Perspective 0 puts the camera 1e7 pixels away: the coefficients would lose f32 precision.
                if plane.f > 100_000.0 {
                    return None;
                }
                Self::projective_3d(wd, hd, &plane)
            }
            "rounded_crop" => {
                let l = f(e, "left", cx) / 100.0 * wf;
                let t = f(e, "top", cx) / 100.0 * hf;
                let r = wf - f(e, "right", cx) / 100.0 * wf;
                let bot = hf - f(e, "bottom", cx) / 100.0 * hf;
                Self::RoundedCrop {
                    l,
                    t,
                    r,
                    b: bot,
                    radius: (f(e, "radius", cx) * cx.px_scale).min(((r - l).min(bot - t) / 2.0).max(0.0)),
                    feather: f(e, "feather", cx) * cx.px_scale,
                    border: f(e, "border", cx) * cx.px_scale,
                    color: crate::vfx::lin(crate::effects::color(e, "border_color", cx)),
                }
            }
            "rgb_split" => {
                let c = crate::vfx::pv(e, "center", cx, &Image { w, h, px: Vec::new() });
                Self::RgbSplit {
                    amount: f(e, "amount", cx) * cx.px_scale,
                    radial: choice(e, "mode") == 1,
                    angle: f(e, "angle", cx).to_radians(),
                    cx: c.x as f32,
                    cy: c.y as f32,
                    blend: f(e, "blend", cx) / 100.0,
                    maxd: (wf * wf + hf * hf).sqrt() * 0.5,
                }
            }
            "vr_chromatic_aberrations" => Self::VrChromatic {
                aberration: [f(e, "red", cx), f(e, "green", cx), f(e, "blue", cx)],
                center: [f(e, "center_x", cx), f(e, "center_y", cx)],
                falloff: f(e, "falloff", cx),
                inverse: b(e, "inverse"),
                layout: choice(e, "frame_layout"),
            },
            "vr_color_gradients" => Self::VrColorGradients {
                points: [[f(e, "p1_lon", cx), f(e, "p1_lat", cx)], [f(e, "p2_lon", cx), f(e, "p2_lat", cx)], [f(e, "p3_lon", cx), f(e, "p3_lat", cx)]],
                colors: ["c1", "c2", "c3"].map(|id| {
                    let c = crate::effects::color(e, id, cx);
                    crate::vfx::lin(c)
                }),
                blend: f(e, "blend", cx),
                opacity: f(e, "opacity", cx) / 100.0,
                mode: choice(e, "mode"),
                layout: choice(e, "frame_layout"),
            },
            "vr_projection" | "vr_rotate_sphere" => {
                let projection = e.effect == "vr_projection";
                let rotation =
                    spatial_vr_rotation(crate::vfx::fv(e, "tilt", cx) as f64, crate::vfx::fv(e, "pan", cx) as f64, crate::vfx::fv(e, "roll", cx) as f64);
                let fov = if projection { crate::vfx::fv(e, "fov", cx) as f64 } else { 360.0 };
                let stretch = projection && b(e, "stretch");
                let identity = (0..3).all(|i| (0..3).all(|j| (rotation[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-12)) && (fov - 360.0).abs() < 1e-9;
                let transposed = [
                    rotation[0][0],
                    rotation[1][0],
                    rotation[2][0],
                    rotation[0][1],
                    rotation[1][1],
                    rotation[2][1],
                    rotation[0][2],
                    rotation[1][2],
                    rotation[2][2],
                ];
                Self::VrProjection {
                    matrix: transposed,
                    kx: fov / 360.0,
                    ky: if stretch { (fov / 180.0).min(1.0) } else { 1.0 },
                    layout: choice(e, "frame_layout"),
                    identity,
                    projection,
                }
            }
            "vr_plane_to_sphere" => {
                let rotation =
                    spatial_vr_rotation(crate::vfx::fv(e, "tilt", cx) as f64, crate::vfx::fv(e, "pan", cx) as f64, crate::vfx::fv(e, "roll", cx) as f64);
                let fov = (crate::vfx::fv(e, "scale", cx) as f64).to_radians().clamp(1e-3, std::f64::consts::PI * 0.999);
                Self::VrPlaneToSphere {
                    matrix: [
                        rotation[0][0],
                        rotation[1][0],
                        rotation[2][0],
                        rotation[0][1],
                        rotation[1][1],
                        rotation[2][1],
                        rotation[0][2],
                        rotation[1][2],
                        rotation[2][2],
                    ],
                    tx: (fov / 2.0).tan(),
                    feather: crate::vfx::fv(e, "feather", cx) / 100.0,
                    layout: choice(e, "frame_layout"),
                }
            }
            _ => return None,
        })
    }

    /// Homography of `plane` for a `w` x `h` image, computed in f64: with d = (x - w/2, y - h/2, f), the
    /// source x is (nc (d.u) - cz u.z (n.d)) / (n.d) + w/2, and likewise for y with `v`.
    fn projective_3d(w: f64, h: f64, plane: &crate::vfx::Plane3d) -> Self {
        let crate::vfx::Plane3d { f, cz, n, u, v } = *plane;
        let nc = n[2] * cz;
        // a . d as coefficients of (x, y, 1)
        let row = |a: [f64; 3]| [a[0], a[1], -a[0] * w / 2.0 - a[1] * h / 2.0 + a[2] * f];
        let (den, du, dv) = (row(n), row(u), row(v));
        let m: [f64; 9] = std::array::from_fn(|i| match i / 3 {
            0 => nc * du[i % 3] - cz * u[2] * den[i % 3] + w / 2.0 * den[i % 3],
            1 => nc * dv[i % 3] - cz * v[2] * den[i % 3] + h / 2.0 * den[i % 3],
            _ => den[i % 3],
        });
        Self::Projective3d { m: m.map(|x| x as f32), nc: nc as f32 }
    }

    pub fn gpu_ok(&self) -> bool {
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
        let finite_f32 = |v: &[f64]| v.iter().all(|x| x.is_finite() && (*x as f32).is_finite());
        match self {
            Self::Mosaic { bx, by, softness, nx, ny, .. } => finite(&[*bx, *by, *softness]) && *bx >= 1.0 && *by >= 1.0 && *nx > 0 && *ny > 0,
            Self::Replicate { count } => *count >= 1 && *count <= 16,
            Self::Twirl { cx, cy, angle, radius } => finite_f32(&[*cx, *cy, *angle, *radius]),
            Self::Spherize { cx, cy, radius } => finite(&[*cx, *cy, *radius]),
            Self::Magnify { cx, cy, magnification, radius, feather, opacity, .. } => {
                finite(&[*cx, *cy, *magnification, *radius, *feather, *opacity]) && *magnification > 0.0
            }
            Self::CornerPin { inv, w, h } => finite(inv) && finite(&[*w, *h]) && *w > 0.0 && *h > 0.0,
            Self::Projective3d { m, nc } => finite(m) && nc.is_finite(),
            Self::TurbulentDisplace { amount, size, ox, oy, complexity, seed: _, ex, ey, ez, .. } => {
                finite(&[*amount, *size, *ox, *oy, *complexity, *ex, *ey, *ez])
                    && *size >= 0.5
                    && ox.abs() <= 1_000_000.0
                    && oy.abs() <= 1_000_000.0
                    && ex.abs() <= 1_000_000.0
                    && ey.abs() <= 1_000_000.0
                    && ez.abs() <= 1_000_000.0
            }
            Self::LensDistortion { cx, cy, k, norm } => finite_f32(&[*cx, *cy, *k, *norm]) && *norm > 0.0,
            Self::WaveWarp { height, width, dir, phase } => finite_f32(&[*height, *width, *dir, *phase]) && *width >= 1.0,
            Self::RoundedCrop { l, t, r, b, radius, feather, border, color } => finite(&[*l, *t, *r, *b, *radius, *feather, *border]) && finite(color),
            Self::RgbSplit { amount, angle, cx, cy, blend, maxd, .. } => finite(&[*amount, *angle, *cx, *cy, *blend, *maxd]) && *maxd > 0.0,
            Self::VrChromatic { aberration, center, falloff, .. } => {
                finite(aberration)
                    && aberration.iter().all(|v| (-100.0..=100.0).contains(v))
                    && finite(center)
                    && (-180.0..=180.0).contains(&center[0])
                    && (-90.0..=90.0).contains(&center[1])
                    && (0.0..=100.0).contains(falloff)
            }
            Self::VrColorGradients { points, colors, blend, opacity, .. } => {
                points.iter().flatten().all(|v| v.is_finite())
                    && points.iter().all(|p| (-180.0..=180.0).contains(&p[0]) && (-90.0..=90.0).contains(&p[1]))
                    && colors.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    && (0.0..=100.0).contains(blend)
                    && opacity.is_finite()
                    && (0.0..=1.0).contains(opacity)
            }
            Self::VrProjection { matrix, kx, ky, .. } => {
                matrix.iter().all(|v| v.is_finite() && v.abs() <= 1.000_001)
                    && kx.is_finite()
                    && *kx >= 10.0 / 360.0
                    && *kx <= 1.0
                    && ky.is_finite()
                    && (0.0..=1.0).contains(ky)
            }
            Self::VrPlaneToSphere { matrix, tx, feather, .. } => {
                matrix.iter().all(|v| v.is_finite() && v.abs() <= 1.000_001) && tx.is_finite() && *tx > 0.0 && *tx < 1000.0 && (0.0..=1.0).contains(feather)
            }
        }
    }

    pub fn matches_id(&self, id: &str) -> bool {
        match self {
            Self::Mosaic { legacy, .. } => id == if *legacy { "mosaic_legacy" } else { "mosaic" },
            Self::Replicate { .. } => id == "replicate",
            Self::Twirl { .. } => id == "twirl" || id == "twirl_legacy",
            Self::Spherize { .. } => id == "spherize",
            Self::Magnify { .. } => id == "magnify" || id == "magnify_legacy",
            Self::LensDistortion { .. } => id == "lens_distortion",
            Self::WaveWarp { .. } => id == "wave_warp",
            Self::TurbulentDisplace { .. } => id == "turbulent_displace",
            Self::CornerPin { .. } => id == "corner_pin",
            Self::Projective3d { .. } => id == "rotate_3d" || id == "basic_3d",
            Self::RoundedCrop { .. } => id == "rounded_crop",
            Self::RgbSplit { .. } => id == "rgb_split",
            Self::VrChromatic { .. } => id == "vr_chromatic_aberrations",
            Self::VrColorGradients { .. } => id == "vr_color_gradients",
            Self::VrProjection { projection, .. } => id == if *projection { "vr_projection" } else { "vr_rotate_sphere" },
            Self::VrPlaneToSphere { .. } => id == "vr_plane_to_sphere",
        }
    }

    pub fn packed(&self) -> (u32, [u32; 4], [f32; 24]) {
        let mut p = [0.0; 24];
        let mut i = [0; 4];
        let op = match self {
            Self::Mosaic { bx, by, softness, nx, ny, legacy } => {
                p[0] = *bx;
                p[1] = *by;
                p[2] = *softness;
                i[0] = u32::from(*legacy);
                i[1] = *nx;
                i[2] = *ny;
                64
            }
            Self::Replicate { count } => {
                i[0] = *count;
                65
            }
            Self::Twirl { cx, cy, angle, radius } => {
                p[..4].copy_from_slice(&[*cx as f32, *cy as f32, *angle as f32, *radius as f32]);
                66
            }
            Self::Spherize { cx, cy, radius } => {
                p[..3].copy_from_slice(&[*cx, *cy, *radius]);
                67
            }
            Self::Magnify { cx, cy, magnification, radius, feather, opacity, square, mode } => {
                p[..6].copy_from_slice(&[*cx, *cy, *magnification, *radius, *feather, *opacity]);
                i[0] = u32::from(*square);
                i[1] = *mode;
                68
            }
            Self::LensDistortion { cx, cy, k, norm } => {
                p[..4].copy_from_slice(&[*cx as f32, *cy as f32, *k as f32, *norm as f32]);
                69
            }
            Self::WaveWarp { height, width, dir, phase } => {
                p[..4].copy_from_slice(&[*height as f32, *width as f32, *dir as f32, *phase as f32]);
                70
            }
            Self::TurbulentDisplace { kind, amount, size, ox, oy, complexity, seed, ex, ey, ez, pin } => {
                i[..4].copy_from_slice(&[*kind, *seed as u32, (*seed >> 32) as u32, *pin]);
                p[..8].copy_from_slice(&[*amount, *size, *ox, *oy, *complexity, *ex, *ey, *ez]);
                71
            }
            Self::CornerPin { inv, w, h } => {
                p[..9].copy_from_slice(inv);
                p[9] = *w;
                p[10] = *h;
                72
            }
            Self::Projective3d { m, nc } => {
                p[..9].copy_from_slice(m);
                p[9] = *nc;
                75
            }
            Self::RoundedCrop { l, t, r, b, radius, feather, border, color } => {
                p[..10].copy_from_slice(&[*l, *t, *r, *b, *radius, *feather, *border, color[0], color[1], color[2]]);
                73
            }
            Self::RgbSplit { amount, radial, angle, cx, cy, blend, maxd } => {
                p[..6].copy_from_slice(&[*amount, *angle, *cx, *cy, *blend, *maxd]);
                i[0] = u32::from(*radial);
                74
            }
            Self::VrChromatic { aberration, center, falloff, inverse, layout } => {
                p[..3].copy_from_slice(aberration);
                p[4..7].copy_from_slice(&[center[0].to_radians(), center[1].to_radians(), 0.5 + falloff / 50.0]);
                i[0] = *layout;
                i[1] = u32::from(*inverse);
                i[3] = 1;
                74
            }
            Self::VrColorGradients { points, colors, blend, opacity, mode, layout } => {
                p[..3].copy_from_slice(&colors[0]);
                p[4..7].copy_from_slice(&colors[1]);
                p[8..11].copy_from_slice(&colors[2]);
                p[12..16].copy_from_slice(&[points[0][0].to_radians(), points[0][1].to_radians(), points[1][0].to_radians(), points[1][1].to_radians()]);
                p[16..20].copy_from_slice(&[points[2][0].to_radians(), points[2][1].to_radians(), 1.0 + blend / 25.0, *opacity]);
                i[0] = *layout;
                i[1] = *mode;
                i[3] = 2;
                74
            }
            Self::VrProjection { matrix, kx, ky, layout, identity, .. } => {
                p[..9].copy_from_slice(&(*matrix).map(|v| v as f32));
                p[9] = *kx as f32;
                p[10] = *ky as f32;
                i[0] = *layout;
                i[1] = u32::from(*identity);
                i[3] = 3;
                74
            }
            Self::VrPlaneToSphere { matrix, tx, feather, layout } => {
                p[..9].copy_from_slice(&(*matrix).map(|v| v as f32));
                p[11] = *tx as f32;
                p[12] = *feather;
                i[0] = *layout;
                i[3] = 4;
                74
            }
        };
        (op, i, p)
    }

    pub fn apply(&self, img: &mut Image) {
        if img.w == 0 || img.h == 0 {
            return;
        }
        if let Self::VrChromatic { aberration, center, falloff, inverse, layout } = self {
            *img = spatial_vr_chromatic(img, *aberration, *center, *falloff, *inverse, *layout);
            return;
        }
        if let Self::VrColorGradients { points, colors, blend, opacity, mode, layout } = self {
            *img = spatial_vr_color_gradients(img, *points, *colors, *blend, *opacity, *mode, *layout);
            return;
        }
        if let Self::VrProjection { matrix, kx, ky, layout, identity, .. } = self {
            if !identity {
                *img = spatial_vr_projection(img, *matrix, *kx, *ky, *layout);
            }
            return;
        }
        if let Self::VrPlaneToSphere { matrix, tx, feather, layout } = self {
            *img = spatial_vr_plane_to_sphere(img, *matrix, *tx, *feather, *layout);
            return;
        }
        let src = img.clone();
        let (w, h) = (img.w, img.h);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut out = src.get(x, y);
                match self {
                    Self::Mosaic { bx, by, softness, .. } => {
                        let bw = (w as f32 / bx).max(1.0);
                        let bh = (h as f32 / by).max(1.0);
                        let u = px / bw;
                        let v = py / bh;
                        let (ix, iy) = ((x as f32 / bw) as usize, (y as f32 / bh) as usize);
                        let sharp = spatial_mosaic_sample(&src, bw, bh, ix as f32 + 0.5, iy as f32 + 0.5);
                        let sm = spatial_mosaic_sample(&src, bw, bh, u, v);
                        for k in 0..4 {
                            out[k] = sharp[k] + (sm[k] - sharp[k]) * softness;
                        }
                    }
                    Self::Replicate { count } => {
                        let n = *count as usize;
                        let u = (x * n) % w;
                        let v = (y * n) % h;
                        out = src.sample_bilinear_clamped(u as f32 + 0.5 + (n as f32 - 1.0) * 0.5 / n as f32, v as f32 + 0.5);
                    }
                    Self::Twirl { cx, cy, angle, radius } => {
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let (dx, dy) = (px - cx, py - cy);
                        let d = dx.hypot(dy);
                        if d < *radius {
                            let k = 1.0 - d / radius;
                            let a = angle * k * k;
                            let (s, c) = a.sin_cos();
                            out = src.sample_bilinear((cx + dx * c - dy * s) as f32, (cy + dx * s + dy * c) as f32);
                        }
                    }
                    Self::Spherize { cx, cy, radius } => {
                        if *radius >= 0.5 {
                            let (dx, dy) = (px - cx, py - cy);
                            let d = dx.hypot(dy);
                            if d < *radius && d >= 1e-9 {
                                let n = d / radius;
                                let k = 1.0 - 0.5 * (1.0 - n * n).sqrt();
                                out = src.sample_bilinear(cx + dx * k, cy + dy * k);
                            }
                        }
                    }
                    Self::LensDistortion { cx, cy, k, norm } => {
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let (dx, dy) = ((px - cx) / norm, (py - cy) / norm);
                        let s = 1.0 - k * (dx * dx + dy * dy);
                        out = src.sample_bilinear((cx + dx * s * norm) as f32, (cy + dy * s * norm) as f32);
                    }
                    Self::WaveWarp { height, width, dir, phase } => {
                        let (ux, uy) = (dir.sin(), -dir.cos());
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let along = px * ux + py * uy;
                        let off = height * (along / width * std::f64::consts::TAU - phase).sin();
                        out = src.sample_bilinear((px - uy * off) as f32, (py + ux * off) as f32);
                    }
                    Self::Magnify { cx, cy, magnification, radius, feather, opacity, square, mode } => {
                        let (dx, dy) = (px - *cx, py - *cy);
                        let d = if *square { dx.abs().max(dy.abs()) } else { dx.hypot(dy) };
                        let k = if *feather > 0.0 { ((*radius - d) / *feather).clamp(0.0, 1.0) } else { (*radius - d + 0.5).clamp(0.0, 1.0) } * *opacity;
                        if k > 0.0 {
                            let s = src.sample_bilinear(*cx + dx / *magnification, *cy + dy / *magnification);
                            let base = Image::unpremul(out);
                            let sc = Image::unpremul(s);
                            let mixed = crate::vfx::blend_simple(*mode, base, sc);
                            let sample = [mixed[0] * s[3], mixed[1] * s[3], mixed[2] * s[3], s[3]];
                            for c in 0..4 {
                                out[c] += (sample[c] - out[c]) * k;
                            }
                        }
                    }
                    Self::CornerPin { inv, w: iw, h: ih } => {
                        let den = inv[6] * px + inv[7] * py + inv[8];
                        if den.abs() < 1e-12 {
                            out = [0.0; 4];
                        } else {
                            let u = (inv[0] * px + inv[1] * py + inv[2]) / den;
                            let v = (inv[3] * px + inv[4] * py + inv[5]) / den;
                            if u >= -1e-3 && v >= -1e-3 && u <= 1.0 + 1e-3 && v <= 1.0 + 1e-3 {
                                out = src.sample_bilinear(u * *iw, v * *ih);
                            } else {
                                out = [0.0; 4];
                            }
                        }
                    }
                    Self::Projective3d { m, nc } => {
                        let den = m[6] * px + m[7] * py + m[8];
                        if den.abs() < 1e-12 || nc / den <= 0.0 {
                            out = [0.0; 4];
                        } else {
                            out = src.sample_bilinear((m[0] * px + m[1] * py + m[2]) / den, (m[3] * px + m[4] * py + m[5]) / den);
                        }
                    }
                    Self::TurbulentDisplace { kind, amount, size, ox, oy, complexity, seed, ex, ey, ez, pin } => {
                        if amount.abs() >= 1e-4 {
                            let (wf, hf) = (w as f32, h as f32);
                            let n = |x: f32, y: f32, ch: f32| crate::vfx::fbm(x + *ex + ch * 31.7, y + *ey, *ez + ch * 3.1, *complexity, *seed);
                            let (u, v) = (px / *size + *ox, py / *size + *oy);
                            let (dx, dy) = match *kind {
                                0 | 3 => (n(u, v, 0.0), n(u, v, 1.0)),
                                1 | 4 | 2 | 5 => {
                                    let eps = 0.05;
                                    let gx = (n(u + eps, v, 0.0) - n(u - eps, v, 0.0)) / (2.0 * eps);
                                    let gy = (n(u, v + eps, 0.0) - n(u, v - eps, 0.0)) / (2.0 * eps);
                                    if *kind == 1 || *kind == 4 { (gx * 0.3, gy * 0.3) } else { (-gy * 0.3, gx * 0.3) }
                                }
                                6 => (0.0, n(u, v, 0.0)),
                                7 => (n(u, v, 0.0), 0.0),
                                _ => (n(u, v, 0.0), n(u, v, 0.0)),
                            };
                            let edge = |d: f32, len: f32| crate::vfx::smoothstep(1.0, (len * 0.1).max(2.0), d);
                            let (kx, ky) = match *pin {
                                1 => {
                                    let k = edge(px.min(wf - px), wf).min(edge(py.min(hf - py), hf));
                                    (k, k)
                                }
                                2 => {
                                    let k = edge(px.min(wf - px), wf);
                                    (k, k)
                                }
                                3 => {
                                    let k = edge(py.min(hf - py), hf);
                                    (k, k)
                                }
                                _ => (1.0, 1.0),
                            };
                            out = src.sample_bilinear(px - dx * *amount * kx, py - dy * *amount * ky);
                        }
                    }
                    Self::RoundedCrop { l, t, r, b, radius, feather, border, color } => {
                        let d = spatial_round_rect_sdf(px, py, *l, *t, *r, *b, *radius);
                        if *border > 0.0 {
                            let k = (0.5 - d).clamp(0.0, 1.0) - (0.5 - d - *border).clamp(0.0, 1.0);
                            if k > 0.0 {
                                for c in 0..3 {
                                    out[c] = color[c] * k + out[c] * (1.0 - k);
                                }
                                out[3] = k + out[3] * (1.0 - k);
                            }
                        }
                        let a = if *feather > 0.0 { (-d / feather).clamp(0.0, 1.0) } else { (0.5 - d).clamp(0.0, 1.0) };
                        for v in &mut out {
                            *v *= a;
                        }
                    }
                    Self::RgbSplit { amount, radial, angle, cx, cy, blend, maxd } => {
                        if amount.abs() >= 1e-3 {
                            let (ox, oy) =
                                if *radial { ((px - cx) / maxd * amount, (py - cy) / maxd * amount) } else { (angle.cos() * amount, angle.sin() * amount) };
                            let r = src.sample_bilinear(px - ox, py - oy);
                            let bq = src.sample_bilinear(px + ox, py + oy);
                            let g = src.get(x, y);
                            let base = [r[0], g[1], bq[2], r[3].max(g[3]).max(bq[3])];
                            for c in 0..4 {
                                out[c] = base[c] + (g[c] - base[c]) * blend;
                            }
                        }
                    }
                    Self::VrChromatic { .. } => {}
                    Self::VrColorGradients { .. } => {}
                    Self::VrProjection { .. } => {}
                    Self::VrPlaneToSphere { .. } => {}
                }
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&out);
            }
        }
    }
}

type SpatialV3 = [f64; 3];
type SpatialM3 = [[f64; 3]; 3];

fn spatial_vr_mmul(a: SpatialM3, b: SpatialM3) -> SpatialM3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn spatial_vr_rotation(tilt: f64, pan: f64, roll: f64) -> SpatialM3 {
    let (sx, cx) = tilt.to_radians().sin_cos();
    let (sy, cy) = pan.to_radians().sin_cos();
    let (sz, cz) = roll.to_radians().sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    spatial_vr_mmul(ry, spatial_vr_mmul(rx, rz))
}

fn spatial_vr_dir(lon: f64, lat: f64) -> SpatialV3 {
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}

fn spatial_vr_dot(a: SpatialV3, b: SpatialV3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn spatial_vr_mvec(matrix: &[f64; 9], v: SpatialV3) -> SpatialV3 {
    [
        matrix[0] * v[0] + matrix[1] * v[1] + matrix[2] * v[2],
        matrix[3] * v[0] + matrix[4] * v[1] + matrix[5] * v[2],
        matrix[6] * v[0] + matrix[7] * v[1] + matrix[8] * v[2],
    ]
}

fn spatial_vr_cross(a: SpatialV3, b: SpatialV3) -> SpatialV3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn spatial_vr_sample(img: &Image, d: SpatialV3) -> [f32; 4] {
    let lon = d[0].atan2(d[2]);
    let lat = d[1].clamp(-1.0, 1.0).asin();
    let (w, h) = (img.w as isize, img.h as isize);
    let fx = (lon / std::f64::consts::TAU + 0.5) * img.w as f64 - 0.5;
    let fy = ((0.5 - lat / std::f64::consts::PI) * img.h as f64 - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (fx.floor() as isize, fy.floor() as isize);
    let (tx, ty) = ((fx - fx.floor()) as f32, (fy - fy.floor()) as f32);
    let g = |x: isize, y: isize| img.get(x.rem_euclid(w) as usize, y.clamp(0, h - 1) as usize);
    let (a, b, c, d) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bot = c[i] + (d[i] - c[i]) * tx;
        out[i] = top + (bot - top) * ty;
    }
    out
}

fn spatial_vr_chromatic(img: &Image, aberration: [f32; 3], center: [f32; 2], falloff: f32, inverse: bool, layout: u32) -> Image {
    let (w, h) = (img.w, img.h);
    let views = match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    };
    let center = spatial_vr_dir((center[0] as f64).to_radians(), (center[1] as f64).to_radians());
    let exponent = 0.5 + falloff as f64 / 50.0;
    let mut out = img.clone();
    for (x0, y0, vw, vh) in views {
        let mut view = Image::new(vw, vh);
        for y in 0..vh {
            let from = ((y0 + y) * w + x0) * 4;
            view.px[y * vw * 4..(y + 1) * vw * 4].copy_from_slice(&img.px[from..from + vw * 4]);
        }
        let source = view.clone();
        for y in 0..vh {
            for x in 0..vw {
                let lon = (x as f64 + 0.5) / vw as f64 * std::f64::consts::TAU - std::f64::consts::PI;
                let lat = (0.5 - (y as f64 + 0.5) / vh as f64) * std::f64::consts::PI;
                let d = spatial_vr_dir(lon, lat);
                let angle = spatial_vr_dot(d, center).clamp(-1.0, 1.0).acos();
                let mut f = (angle / std::f64::consts::PI).powf(exponent);
                if inverse {
                    f = 1.0 - f;
                }
                let axis = spatial_vr_cross(center, d);
                let axis_len = spatial_vr_dot(axis, axis).sqrt();
                if axis_len > 1e-9 {
                    let k = axis.map(|v| v / axis_len);
                    let kd = spatial_vr_cross(k, d);
                    let kdot = spatial_vr_dot(k, d);
                    let idx = (y * vw + x) * 4;
                    for c in 0..3 {
                        let a = aberration[c] as f64 / 100.0 * 0.15 * f;
                        let (s, co) = (-a).sin_cos();
                        let r = [0, 1, 2].map(|i| d[i] * co + kd[i] * s + k[i] * kdot * (1.0 - co));
                        view.px[idx + c] = spatial_vr_sample(&source, r)[c];
                    }
                }
            }
        }
        for y in 0..vh {
            let to = ((y0 + y) * w + x0) * 4;
            out.px[to..to + vw * 4].copy_from_slice(&view.px[y * vw * 4..(y + 1) * vw * 4]);
        }
    }
    out
}

fn spatial_vr_color_gradients(img: &Image, points: [[f32; 2]; 3], colors: [[f32; 3]; 3], blend: f32, opacity: f32, mode: u32, layout: u32) -> Image {
    let (w, h) = (img.w, img.h);
    let views = match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    };
    let point_dirs = points.map(|p| spatial_vr_dir((p[0] as f64).to_radians(), (p[1] as f64).to_radians()));
    let exponent = 1.0 + blend as f64 / 25.0;
    let mut out = img.clone();
    for (x0, y0, vw, vh) in views {
        for y in 0..vh {
            for x in 0..vw {
                let lon = ((x as f64 + 0.5) / vw as f64 - 0.5) * std::f64::consts::TAU;
                let lat = (0.5 - (y as f64 + 0.5) / vh as f64) * std::f64::consts::PI;
                let d = spatial_vr_dir(lon, lat);
                let mut acc = [0.0f64; 3];
                let mut ws = 0.0;
                for n in 0..3 {
                    let angle = spatial_vr_dot(d, point_dirs[n]).clamp(-1.0, 1.0).acos().max(1e-3);
                    let weight = 1.0 / angle.powf(exponent);
                    for k in 0..3 {
                        acc[k] += colors[n][k] as f64 * weight;
                    }
                    ws += weight;
                }
                let gradient = acc.map(|v| (v / ws) as f32);
                let index = ((y0 + y) * w + x0 + x) * 4;
                let original = [out.px[index], out.px[index + 1], out.px[index + 2], out.px[index + 3]];
                let base = Image::unpremul(original);
                let blended = crate::vfx::blend_simple(mode, base, gradient);
                let rgb = crate::vfx::lerp3(base, blended, opacity);
                let alpha = original[3] + (1.0 - original[3]) * opacity;
                out.px[index..index + 4].copy_from_slice(&[rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha]);
            }
        }
    }
    out
}

fn spatial_vr_projection(img: &Image, matrix: [f64; 9], kx: f64, ky: f64, layout: u32) -> Image {
    let (w, h) = (img.w, img.h);
    let views = match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    };
    let mut out = img.clone();
    for (x0, y0, vw, vh) in views {
        let mut view = Image::new(vw, vh);
        for y in 0..vh {
            let from = ((y0 + y) * w + x0) * 4;
            view.px[y * vw * 4..(y + 1) * vw * 4].copy_from_slice(&img.px[from..from + vw * 4]);
        }
        let source = view.clone();
        for y in 0..vh {
            for x in 0..vw {
                let lon = ((x as f64 + 0.5) / vw as f64 - 0.5) * std::f64::consts::TAU;
                let lat = (0.5 - (y as f64 + 0.5) / vh as f64) * std::f64::consts::PI;
                let direction = spatial_vr_mvec(&matrix, spatial_vr_dir(lon * kx, lat * ky));
                let sample = spatial_vr_sample(&source, direction);
                let i = (y * vw + x) * 4;
                view.px[i..i + 4].copy_from_slice(&sample);
            }
        }
        for y in 0..vh {
            let to = ((y0 + y) * w + x0) * 4;
            out.px[to..to + vw * 4].copy_from_slice(&view.px[y * vw * 4..(y + 1) * vw * 4]);
        }
    }
    out
}

fn spatial_vr_plane_to_sphere(img: &Image, matrix: [f64; 9], tx: f64, feather: f32, layout: u32) -> Image {
    let (w, h) = (img.w, img.h);
    let views = match layout {
        1 if h >= 2 => vec![(0, 0, w, h / 2), (0, h / 2, w, h - h / 2)],
        2 if w >= 2 => vec![(0, 0, w / 2, h), (w / 2, 0, w - w / 2, h)],
        _ => vec![(0, 0, w, h)],
    };
    let mut out = img.clone();
    for (x0, y0, vw, vh) in views {
        let mut view = Image::new(vw, vh);
        for y in 0..vh {
            let from = ((y0 + y) * w + x0) * 4;
            view.px[y * vw * 4..(y + 1) * vw * 4].copy_from_slice(&img.px[from..from + vw * 4]);
        }
        let source = view.clone();
        let ty = tx * vh as f64 / vw as f64;
        for y in 0..vh {
            for x in 0..vw {
                let lon = ((x as f64 + 0.5) / vw as f64 - 0.5) * std::f64::consts::TAU;
                let lat = (0.5 - (y as f64 + 0.5) / vh as f64) * std::f64::consts::PI;
                let d = spatial_vr_mvec(&matrix, spatial_vr_dir(lon, lat));
                let mut sample = [0.0; 4];
                if d[2] > 1e-6 {
                    let (u, v) = (d[0] / d[2] / tx, -d[1] / d[2] / ty);
                    if u.abs() <= 1.0 && v.abs() <= 1.0 {
                        sample = source.sample_bilinear_clamped(((u + 1.0) / 2.0 * vw as f64) as f32, ((v + 1.0) / 2.0 * vh as f64) as f32);
                        if feather > 0.0 {
                            let edge = (1.0 - u.abs().max(v.abs())) as f32;
                            let k = crate::vfx::smoothstep(0.0, feather, edge);
                            sample = sample.map(|c| c * k);
                        }
                    }
                }
                let i = (y * vw + x) * 4;
                view.px[i..i + 4].copy_from_slice(&sample);
            }
        }
        for y in 0..vh {
            let to = ((y0 + y) * w + x0) * 4;
            out.px[to..to + vw * 4].copy_from_slice(&view.px[y * vw * 4..(y + 1) * vw * 4]);
        }
    }
    out
}

fn spatial_mosaic_sample(src: &Image, bw: f32, bh: f32, u: f32, v: f32) -> [f32; 4] {
    let (nx, ny) = (((src.w as f32 / bw).ceil() as usize).max(1), ((src.h as f32 / bh).ceil() as usize).max(1));
    let (fx, fy) = ((u - 0.5).clamp(0.0, nx as f32 - 1.0), (v - 0.5).clamp(0.0, ny as f32 - 1.0));
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(nx - 1), (y0 + 1).min(ny - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let fetch = |x: usize, y: usize| src.sample_bilinear_clamped((x as f32 + 0.5) * bw, (y as f32 + 0.5) * bh);
    let (a, b, c, d) = (fetch(x0, y0), fetch(x1, y0), fetch(x0, y1), fetch(x1, y1));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        top + (bot - top) * ty
    })
}

fn spatial_square_to_quad(q: [filmcraft_geom::Vec2; 4]) -> [f64; 9] {
    let [p0, p1, p2, p3] = q;
    let (sx, sy) = (p0.x - p1.x + p2.x - p3.x, p0.y - p1.y + p2.y - p3.y);
    if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
        return [p1.x - p0.x, p2.x - p1.x, p0.x, p1.y - p0.y, p2.y - p1.y, p0.y, 0.0, 0.0, 1.0];
    }
    let (dx1, dx2, dy1, dy2) = (p1.x - p2.x, p3.x - p2.x, p1.y - p2.y, p3.y - p2.y);
    let det = dx1 * dy2 - dx2 * dy1;
    let g = (sx * dy2 - dx2 * sy) / det;
    let h = (dx1 * sy - sx * dy1) / det;
    [p1.x - p0.x + g * p1.x, p3.x - p0.x + h * p3.x, p0.x, p1.y - p0.y + g * p1.y, p3.y - p0.y + h * p3.y, p0.y, g, h, 1.0]
}

fn spatial_h_inverse(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let co =
        [e * i - f * h, -(d * i - f * g), d * h - e * g, -(b * i - c * h), a * i - c * g, -(a * h - b * g), b * f - c * e, -(a * f - c * d), a * e - b * d];
    let det = a * co[0] + b * co[1] + c * co[2];
    if det.abs() < 1e-18 {
        return None;
    }
    Some([co[0] / det, co[3] / det, co[6] / det, co[1] / det, co[4] / det, co[7] / det, co[2] / det, co[5] / det, co[8] / det])
}

fn spatial_round_rect_sdf(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0 - r, (y1 - y0) / 2.0 - r);
    let (qx, qy) = ((px - cx).abs() - hx, (py - cy).abs() - hy);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    fn context() -> FxCtx<'static> {
        FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.375,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        }
    }

    fn source() -> Image {
        let mut image = Image::new(7, 5);
        for y in 0..5 {
            for x in 0..7 {
                let alpha = if (x + y) % 4 == 0 {
                    0.0
                } else if (x + y) % 3 == 0 {
                    0.35
                } else {
                    1.0
                };
                let i = (y * 7 + x) * 4;
                image.px[i..i + 4].copy_from_slice(&[x as f32 * 0.07 * alpha, y as f32 * 0.11 * alpha, (x + y) as f32 * 0.045 * alpha, alpha]);
            }
        }
        image
    }

    #[test]
    fn projective_3d_matches_the_cpu_effects() {
        for (id, params) in [
            ("rotate_3d", vec![("rot_x", 25.0), ("rot_y", -30.0), ("rot_z", 12.0), ("perspective", 60.0)]),
            ("rotate_3d", vec![("rot_x", 70.0), ("z", 3.0), ("perspective", 90.0)]),
            ("rotate_3d", vec![("rot_y", 160.0), ("perspective", 40.0)]),
            ("rotate_3d", vec![("rot_z", 45.0), ("perspective", 10.0)]),
            ("basic_3d", vec![("swivel", 40.0), ("tilt", -20.0), ("distance", 1.5)]),
            ("basic_3d", vec![("swivel", -120.0), ("tilt", 35.0), ("distance", -3.0)]),
        ] {
            let mut effect = find_effect(id).unwrap_or_else(|| panic!("missing effect {id}")).instance();
            for (key, value) in &params {
                effect.params.get_mut(*key).unwrap_or_else(|| panic!("{id} has no {key}")).value = ParamValue::Float(*value);
            }
            let ctx = context();
            let op = SpatialOp::eval(&effect, &ctx, 7, 5).unwrap_or_else(|| panic!("no spatial operation for {id}"));
            assert!(op.gpu_ok(), "{id} {params:?}");
            let mut expected = source();
            crate::effects::apply(&mut expected, &effect, &ctx);
            let mut actual = source();
            op.apply(&mut actual);
            for (index, (a, b)) in actual.px.iter().zip(&expected.px).enumerate() {
                assert!((a - b).abs() < 2e-4, "{id} {params:?} component {index}: {a} != {b}");
            }
        }
    }

    #[test]
    fn supported_spatial_operations_match_cpu_oracle_on_odd_alpha_image() {
        for id in [
            "mosaic",
            "mosaic_legacy",
            "replicate",
            "twirl",
            "twirl_legacy",
            "spherize",
            "magnify",
            "magnify_legacy",
            "lens_distortion",
            "wave_warp",
            "corner_pin",
            "rounded_crop",
            "rgb_split",
            "turbulent_displace",
        ] {
            let mut effect = find_effect(id).unwrap_or_else(|| panic!("missing effect {id}")).instance();
            for (key, value) in [
                ("horizontal", 3.0),
                ("vertical", 2.0),
                ("softness", 45.0),
                ("count", 3.0),
                ("angle", 42.0),
                ("radius", 60.0),
                ("height", 2.25),
                ("width", 8.0),
                ("curvature", -22.0),
                ("amount", 2.0),
                ("border", 1.5),
            ] {
                if let Some(param) = effect.params.get_mut(key) {
                    param.value = ParamValue::Float(value);
                }
            }
            let ctx = context();
            let op = SpatialOp::eval(&effect, &ctx, 7, 5).unwrap_or_else(|| panic!("no spatial operation for {id}"));
            assert!(op.gpu_ok(), "expected GPU support for bounded {id} case");
            let mut expected = source();
            if !crate::gpufx::legacy_cpu_oracle(&mut expected, &effect, &ctx) {
                assert!(crate::vfx::apply(&mut expected, &effect, &ctx), "missing independent CPU oracle for {id}");
            }
            let mut actual = source();
            op.apply(&mut actual);
            for (index, (a, b)) in actual.px.iter().zip(&expected.px).enumerate() {
                assert!((a - b).abs() < 2e-5, "{id} pixel component {index}: {a} != {b}");
            }
        }
    }

    #[test]
    fn turbulent_displace_matches_oracle_for_modes_cycle_seed_and_pinning() {
        let ctx = context();
        for kind in 0..9 {
            for pin in 0..4 {
                for cycle in [false, true] {
                    let mut effect = find_effect("turbulent_displace").unwrap_or_else(|| panic!("missing turbulent_displace")).instance();
                    for (key, value) in [
                        ("displacement", ParamValue::Choice(kind)),
                        ("pinning", ParamValue::Choice(pin)),
                        ("cycle", ParamValue::Bool(cycle)),
                        ("seed", ParamValue::Float(9999.0)),
                        ("complexity", ParamValue::Float(3.5)),
                        ("evolution", ParamValue::Float(725.0)),
                        ("amount", ParamValue::Float(8.0)),
                    ] {
                        effect.params.get_mut(key).unwrap_or_else(|| panic!("missing parameter {key}")).value = value;
                    }
                    let op = SpatialOp::eval(&effect, &ctx, 7, 5).unwrap_or_else(|| panic!("missing op"));
                    assert!(op.gpu_ok(), "mode {kind}, pin {pin}, cycle {cycle}");
                    let mut expected = source();
                    crate::vfx::apply(&mut expected, &effect, &ctx);
                    let mut actual = source();
                    op.apply(&mut actual);
                    for (index, (a, b)) in actual.px.iter().zip(&expected.px).enumerate() {
                        assert!((a - b).abs() < 2e-5, "mode {kind}, pin {pin}, cycle {cycle}, component {index}: {a} != {b}");
                    }
                }
            }
        }
    }
}
