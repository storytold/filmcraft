//! The Premiere 26 video-effect catalogue beyond the original core set (M5.11): definitions for
//! every current folder (Adjust … Video), the Immersive Video (VR) set, the Legacy bin and the
//! obsolete effects old projects still reference, plus the Effects-panel badge table.
//!
//! Names, folders and parameter names follow the public effect list; parameter ranges/defaults are
//! our own where the reference does not pin them. Pixel implementations live in
//! `filmcraft_render::vfx`.

use super::*;

const IMMERSIVE: &[&str] = &["Video Effects", "Immersive Video"];
pub(super) const LIGHTS: &[&str] = &["Video Effects", "Lights & Glows"];
const TIME: &[&str] = &["Video Effects", "Time"];
pub(super) const UTILITY: &[&str] = &["Video Effects", "Utility"];
/// Hidden-in-Premiere effects that old projects still render (kept so they round-trip and render).
pub(super) const OBSOLETE: &[&str] = &["Video Effects", "Obsolete"];
/// The Effects panel's Legacy bin.
pub(super) const LEGACY: &[&str] = &["Legacy", "Video Effects"];

/// Track choices for effects that read another video track (Track Matte Key, Compound Blur…).
pub const TRACK_CHOICES: &[&str] = &["None", "Video 1", "Video 2", "Video 3", "Video 4", "Video 5", "Video 6", "Video 7", "Video 8"];
/// Equirectangular frame layouts for the Immersive Video effects.
pub const FRAME_LAYOUTS: &[&str] = &["Monoscopic", "Stereoscopic - Over/Under", "Stereoscopic - Side by Side"];
/// Easing for the animated Transform presets (Grow, Shrink, Move, Spin).
pub const EASINGS: &[&str] = &["Linear", "Ease In", "Ease Out", "Ease In and Out"];
pub const ECHO_OPERATORS: &[&str] = &["Add", "Maximum", "Minimum", "Screen", "Composite In Back", "Composite In Front", "Blend"];
pub const SIMPLE_BLEND: &[&str] = &["Normal", "Add", "Screen", "Multiply", "Overlay"];

const LIGHT_TYPES: &[&str] = &["None", "Directional", "Omni", "Spotlight"];
/// Per-light parameter ids of Lighting Effects: (type, color, center, major, minor, angle, intensity, focus).
pub const LIGHT_IDS: [[&str; 8]; 5] = [
    ["l1_type", "l1_color", "l1_center", "l1_major", "l1_minor", "l1_angle", "l1_intensity", "l1_focus"],
    ["l2_type", "l2_color", "l2_center", "l2_major", "l2_minor", "l2_angle", "l2_intensity", "l2_focus"],
    ["l3_type", "l3_color", "l3_center", "l3_major", "l3_minor", "l3_angle", "l3_intensity", "l3_focus"],
    ["l4_type", "l4_color", "l4_center", "l4_major", "l4_minor", "l4_angle", "l4_intensity", "l4_focus"],
    ["l5_type", "l5_color", "l5_center", "l5_major", "l5_minor", "l5_angle", "l5_intensity", "l5_focus"],
];
const LIGHT_GROUPS: [&str; 5] = ["Light 1", "Light 2", "Light 3", "Light 4", "Light 5"];

fn pct(id: &'static str, label: &'static str, def: f64) -> ParamDef {
    f(id, label, def, 0.0, 100.0, "%")
}
fn px(id: &'static str, label: &'static str, def: f64, max: f64, soft: f64) -> ParamDef {
    fs(id, label, def, (0.0, max), (0.0, soft), "", 1)
}
fn layout() -> ParamDef {
    ch("frame_layout", "Frame Layout", FRAME_LAYOUTS, 0)
}
fn seed() -> ParamDef {
    fs("seed", "Random Seed", 0.0, (0.0, 10000.0), (0.0, 1000.0), "", 0)
}
fn blend_orig() -> ParamDef {
    pct("blend", "Blend With Original", 0.0)
}

/// Default position of auto (NaN) point parameters, as fractions of the frame, where the frame
/// centre is not right (Corner Pin corners, gradient ends, Lightning endpoints…).
pub fn auto_point(effect: &str, param: &str) -> Option<(f64, f64)> {
    Some(match (effect, param) {
        ("corner_pin", "upper_left") => (0.0, 0.0),
        ("corner_pin", "upper_right") => (1.0, 0.0),
        ("corner_pin", "lower_left") => (0.0, 1.0),
        ("corner_pin", "lower_right") => (1.0, 1.0),
        ("gradient", "start") => (0.5, 0.0),
        ("gradient", "end") => (0.5, 1.0),
        ("lightning", "start") => (0.1, 0.1),
        ("lightning", "end") => (0.9, 0.9),
        ("checkerboard", "anchor") => (0.0, 0.0),
        ("write_on", "brush") => (0.5, 0.5),
        ("volumetric_rays", "center") => (0.5, 0.25),
        _ => return None,
    })
}

fn lighting_effects() -> EffectDef {
    let mut ps = Vec::new();
    for (i, ids) in LIGHT_IDS.iter().enumerate() {
        let g = LIGHT_GROUPS[i];
        ps.push(grp(ch(ids[0], "Light Type", LIGHT_TYPES, if i == 0 { 3 } else { 0 }), g));
        ps.push(grp(col(ids[1], "Light Color", [1.0, 1.0, 1.0, 1.0]), g));
        ps.push(grp(pt(ids[2], "Center", f64::NAN, f64::NAN), g));
        ps.push(grp(fs(ids[3], "Major Radius", 20.0, (0.0, 100.0), (0.0, 100.0), "", 1), g));
        ps.push(grp(fs(ids[4], "Minor Radius", 20.0, (0.0, 100.0), (0.0, 100.0), "", 1), g));
        ps.push(grp(ang(ids[5], "Angle", 0.0), g));
        ps.push(grp(fs(ids[6], "Intensity", 20.0, (-100.0, 100.0), (-100.0, 100.0), "", 1), g));
        ps.push(grp(f(ids[7], "Focus", 50.0, 0.0, 100.0, ""), g));
    }
    ps.push(col("ambient_color", "Ambience Light Color", [1.0, 1.0, 1.0, 1.0]));
    ps.push(fs("ambient", "Ambience Intensity", 20.0, (-100.0, 100.0), (-100.0, 100.0), "", 1));
    ps.push(fs("gloss", "Surface Gloss", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1));
    ps.push(fs("material", "Surface Material", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1));
    ps.push(fs("exposure", "Exposure", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1));
    ps.push(fs("bump_height", "Bump Height", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1));
    ps.push(b("white_high", "White Is High", true));
    video("lighting_effects", "Lighting Effects", ADJUST, ps)
}

fn immersive() -> Vec<EffectDef> {
    let vr = |id, name, mut ps: Vec<ParamDef>| {
        ps.insert(0, layout());
        video(id, name, IMMERSIVE, ps)
    };
    vec![
        vr("vr_blur", "VR Blur", vec![fs("blurriness", "Blurriness", 0.0, (0.0, 1000.0), (0.0, 100.0), "", 1)]),
        vr(
            "vr_chromatic_aberrations",
            "VR Chromatic Aberrations",
            vec![
                fs("red", "Chromatic Aberration (Red)", 0.0, (-100.0, 100.0), (-50.0, 50.0), "", 1),
                fs("green", "Chromatic Aberration (Green)", 0.0, (-100.0, 100.0), (-50.0, 50.0), "", 1),
                fs("blue", "Chromatic Aberration (Blue)", 0.0, (-100.0, 100.0), (-50.0, 50.0), "", 1),
                fs("center_x", "Center Point (Longitude)", 0.0, (-180.0, 180.0), (-180.0, 180.0), "°", 1),
                fs("center_y", "Center Point (Latitude)", 0.0, (-90.0, 90.0), (-90.0, 90.0), "°", 1),
                pct("falloff", "Falloff", 50.0),
                b("inverse", "Inverse Falloff", false),
            ],
        ),
        vr(
            "vr_color_gradients",
            "VR Color Gradients",
            vec![
                col("c1", "Point 1 Color", [1.0, 0.25, 0.25, 1.0]),
                fs("p1_lon", "Point 1 Longitude", -120.0, (-180.0, 180.0), (-180.0, 180.0), "°", 1),
                fs("p1_lat", "Point 1 Latitude", 20.0, (-90.0, 90.0), (-90.0, 90.0), "°", 1),
                col("c2", "Point 2 Color", [0.25, 1.0, 0.4, 1.0]),
                fs("p2_lon", "Point 2 Longitude", 0.0, (-180.0, 180.0), (-180.0, 180.0), "°", 1),
                fs("p2_lat", "Point 2 Latitude", -20.0, (-90.0, 90.0), (-90.0, 90.0), "°", 1),
                col("c3", "Point 3 Color", [0.3, 0.4, 1.0, 1.0]),
                fs("p3_lon", "Point 3 Longitude", 120.0, (-180.0, 180.0), (-180.0, 180.0), "°", 1),
                fs("p3_lat", "Point 3 Latitude", 20.0, (-90.0, 90.0), (-90.0, 90.0), "°", 1),
                f("blend", "Blend", 50.0, 1.0, 100.0, ""),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", SIMPLE_BLEND, 0),
            ],
        ),
        vr(
            "vr_denoise",
            "VR De-Noise",
            vec![pct("amount", "Noise Level", 0.0), ch("noise_type", "Noise Type", &["Fast", "Slow"], 0), b("show_noise", "Show Only Noise", false)],
        ),
        vr(
            "vr_digital_glitch",
            "VR Digital Glitch",
            vec![
                pct("amplitude", "Master Amplitude", 0.0),
                fs("distortion", "Distortion Complexity", 20.0, (1.0, 100.0), (1.0, 100.0), "", 0),
                f("rate", "Distortion Rate", 10.0, 0.0, 60.0, ""),
                pct("color", "Color Distortion", 50.0),
                pct("scanlines", "Scan Lines", 0.0),
                seed(),
            ],
        ),
        vr(
            "vr_fractal_noise",
            "VR Fractal Noise",
            vec![
                ch("fractal_type", "Fractal Type", &["Basic", "Turbulent Smooth", "Turbulent Sharp"], 0),
                b("invert", "Invert", false),
                fs("contrast", "Contrast", 100.0, (0.0, 1000.0), (0.0, 400.0), "", 1),
                fs("brightness", "Brightness", 0.0, (-200.0, 200.0), (-200.0, 200.0), "", 1),
                fs("scale", "Scale", 100.0, (10.0, 10000.0), (20.0, 600.0), "", 1),
                fs("complexity", "Complexity", 6.0, (1.0, 10.0), (1.0, 10.0), "", 1),
                ang("evolution", "Evolution", 0.0),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", SIMPLE_BLEND, 0),
            ],
        ),
        vr(
            "vr_glow",
            "VR Glow",
            vec![
                pct("threshold", "Luminance Threshold", 70.0),
                fs("radius", "Glow Radius", 30.0, (0.0, 1000.0), (0.0, 200.0), "", 1),
                fs("brightness", "Glow Brightness", 1.0, (0.0, 10.0), (0.0, 4.0), "", 2),
                fs("saturation", "Glow Saturation", 1.0, (0.0, 4.0), (0.0, 4.0), "", 2),
                b("use_tint", "Use Tint Color", false),
                col("tint", "Tint Color", [1.0, 1.0, 1.0, 1.0]),
            ],
        ),
        vr(
            "vr_plane_to_sphere",
            "VR Plane to Sphere",
            vec![
                fs("scale", "Scale (Degrees)", 60.0, (1.0, 180.0), (1.0, 180.0), "°", 1),
                fs("pan", "Pan", 0.0, (-180.0, 180.0), (-180.0, 180.0), "°", 1),
                fs("tilt", "Tilt", 0.0, (-90.0, 90.0), (-90.0, 90.0), "°", 1),
                ang("roll", "Roll", 0.0),
                pct("feather", "Edge Feather", 0.0),
            ],
        ),
        vr(
            "vr_projection",
            "VR Projection",
            vec![
                fs("fov", "Target Field of View", 360.0, (10.0, 360.0), (10.0, 360.0), "°", 1),
                ang("tilt", "Tilt (X Axis)", 0.0),
                ang("pan", "Pan (Y Axis)", 0.0),
                ang("roll", "Roll (Z Axis)", 0.0),
                b("stretch", "Stretch to Fit Frame", true),
            ],
        ),
        vr("vr_rotate_sphere", "VR Rotate Sphere", vec![ang("tilt", "Tilt (X Axis)", 0.0), ang("pan", "Pan (Y Axis)", 0.0), ang("roll", "Roll (Z Axis)", 0.0)]),
        vr("vr_sharpen", "VR Sharpen", vec![fs("amount", "Sharpen Amount", 0.0, (0.0, 500.0), (0.0, 100.0), "", 0)]),
    ]
}

/// The new effect definitions (appended after the core set).
pub(super) fn defs() -> Vec<EffectDef> {
    let mut v = vec![
        lighting_effects(),
        // ---- Blur & Sharpen ----
        video(
            "bokeh_blur",
            "Bokeh Blur",
            BLUR,
            vec![
                px("amount", "Blur Amount", 10.0, 500.0, 60.0),
                ch("shape", "Iris Shape", &["Circle", "Triangle", "Square", "Pentagon", "Hexagon", "Octagon"], 4),
                ang("rotation", "Iris Rotation", 0.0),
                pct("highlight", "Highlight Brightness", 50.0),
                pct("threshold", "Highlight Threshold", 80.0),
            ],
        ),
        video(
            "channel_blur",
            "Channel Blur",
            BLUR,
            vec![
                px("red", "Red Blurriness", 0.0, 1000.0, 100.0),
                px("green", "Green Blurriness", 0.0, 1000.0, 100.0),
                px("blue", "Blue Blurriness", 0.0, 1000.0, 100.0),
                px("alpha", "Alpha Blurriness", 0.0, 1000.0, 100.0),
                b("repeat_edge", "Repeat Edge Pixels", false),
                ch("dimensions", "Blur Dimensions", &["Horizontal and Vertical", "Horizontal", "Vertical"], 0),
            ],
        ),
        video(
            "compound_blur",
            "Compound Blur",
            BLUR,
            vec![
                ch("layer", "Blur Layer", TRACK_CHOICES, 0),
                px("max", "Maximum Blur", 20.0, 500.0, 100.0),
                b("stretch", "Stretch Map to Fit", true),
                b("invert", "Invert Blur", false),
            ],
        ),
        video(
            "focus_blur",
            "Focus Blur",
            BLUR,
            vec![
                ch("shape", "Focus Shape", &["Radial", "Linear"], 0),
                pt("center", "Focus Point", f64::NAN, f64::NAN),
                ang("angle", "Angle", 0.0),
                px("size", "Focus Size", 200.0, 4000.0, 1000.0),
                px("feather", "Feather", 200.0, 4000.0, 1000.0),
                px("amount", "Blur Amount", 20.0, 500.0, 100.0),
                b("show", "Show Focus Area", false),
            ],
        ),
        video("reduce_interlace_flicker", "Reduce Interlace Flicker", BLUR, vec![fs("softness", "Softness", 0.0, (0.0, 10.0), (0.0, 10.0), "", 2)]),
        // ---- Color Correction ----
        video(
            "asc_cdl",
            "ASC CDL",
            COLOR_CORR,
            vec![
                fs("r_slope", "Red Slope", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("r_offset", "Red Offset", 0.0, (-1.0, 1.0), (-1.0, 1.0), "", 3),
                fs("r_power", "Red Power", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("g_slope", "Green Slope", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("g_offset", "Green Offset", 0.0, (-1.0, 1.0), (-1.0, 1.0), "", 3),
                fs("g_power", "Green Power", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("b_slope", "Blue Slope", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("b_offset", "Blue Offset", 0.0, (-1.0, 1.0), (-1.0, 1.0), "", 3),
                fs("b_power", "Blue Power", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
                fs("saturation", "Saturation", 1.0, (0.0, 10.0), (0.0, 4.0), "", 3),
            ],
        ),
        video(
            "video_limiter",
            "Video Limiter",
            COLOR_CORR,
            vec![
                ch(
                    "clip_level",
                    "Clip Level",
                    &["100 IRE", "101 IRE", "102 IRE", "103 IRE", "104 IRE", "105 IRE", "106 IRE", "107 IRE", "108 IRE", "109 IRE"],
                    0,
                ),
                ch("compression", "Compression Before Clipping", &["None", "3%", "5%", "10%", "20%"], 1),
                ch("axis", "Reduce Axis", &["Luma", "Chroma", "Luma and Chroma", "Smart Limit"], 3),
                b("gamut_warning", "Gamut Warning", false),
                col("warning_color", "Gamut Warning Color", [1.0, 0.0, 0.0, 1.0]),
            ],
        ),
        video(
            "vignette",
            "Vignette",
            COLOR_CORR,
            vec![
                fs("amount", "Amount", -50.0, (-100.0, 100.0), (-100.0, 100.0), "", 1),
                pct("midpoint", "Midpoint", 50.0),
                fs("roundness", "Roundness", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1),
                pct("feather", "Feather", 50.0),
                col("color", "Color", [0.0, 0.0, 0.0, 1.0]),
            ],
        ),
        // ---- Distort ----
        video(
            "corner_pin",
            "Corner Pin",
            DISTORT,
            vec![
                pt("upper_left", "Upper Left", f64::NAN, f64::NAN),
                pt("upper_right", "Upper Right", f64::NAN, f64::NAN),
                pt("lower_left", "Lower Left", f64::NAN, f64::NAN),
                pt("lower_right", "Lower Right", f64::NAN, f64::NAN),
            ],
        ),
        video(
            "magnify",
            "Magnify",
            DISTORT,
            vec![
                ch("shape", "Shape", &["Circle", "Square"], 0),
                pt("center", "Center", f64::NAN, f64::NAN),
                fs("magnification", "Magnification", 200.0, (100.0, 600.0), (100.0, 600.0), "", 1),
                px("size", "Size", 100.0, 4000.0, 600.0),
                px("feather", "Feather", 0.0, 1000.0, 200.0),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", SIMPLE_BLEND, 0),
            ],
        ),
        video("spherize", "Spherize", DISTORT, vec![px("radius", "Radius", 0.0, 2500.0, 600.0), pt("center", "Center of Sphere", f64::NAN, f64::NAN)]),
        video(
            "turbulent_displace",
            "Turbulent Displace",
            DISTORT,
            vec![
                ch(
                    "displacement",
                    "Displacement",
                    &[
                        "Turbulent",
                        "Bulge",
                        "Twist",
                        "Turbulent Smoother",
                        "Bulge Smoother",
                        "Twist Smoother",
                        "Vertical Displacement",
                        "Horizontal Displacement",
                        "Cross Displacement",
                    ],
                    0,
                ),
                fs("amount", "Amount", 50.0, (-1000.0, 1000.0), (-200.0, 200.0), "", 1),
                fs("size", "Size", 100.0, (2.0, 1000.0), (2.0, 400.0), "", 1),
                pt("offset", "Offset (Turbulence)", f64::NAN, f64::NAN),
                fs("complexity", "Complexity", 1.0, (1.0, 10.0), (1.0, 10.0), "", 1),
                ang("evolution", "Evolution", 0.0),
                grp(b("cycle", "Cycle Evolution", false), "Evolution Options"),
                grp(fs("cycle_revs", "Cycle (in Revolutions)", 1.0, (1.0, 100.0), (1.0, 20.0), "", 0), "Evolution Options"),
                grp(seed(), "Evolution Options"),
                ch("pinning", "Pinning", &["None", "Pin All", "Pin Horizontal", "Pin Vertical"], 1),
                ch("antialias", "Antialiasing for Best Quality", &["Low", "High"], 0),
            ],
        ),
        video(
            "warp_stabilizer",
            "Warp Stabilizer",
            DISTORT,
            vec![
                grp(ch("result", "Result", &["Smooth Motion", "No Motion"], 0), "Stabilization"),
                grp(fs("smoothness", "Smoothness", 50.0, (0.0, 1000.0), (0.0, 200.0), "%", 0), "Stabilization"),
                grp(ch("method", "Method", &["Position", "Position, Scale, Rotation", "Perspective", "Subspace Warp"], 1), "Stabilization"),
                grp(b("preserve_scale", "Preserve Scale", false), "Stabilization"),
                grp(
                    ch("framing", "Framing", &["Stabilize Only", "Stabilize, Crop", "Stabilize, Crop, Auto-scale", "Stabilize, Synthesize Edges"], 2),
                    "Borders",
                ),
                grp(fs("max_scale", "Maximum Scale", 150.0, (100.0, 1000.0), (100.0, 300.0), "%", 0), "Borders"),
                grp(pct("action_safe", "Action-Safe Margin", 0.0), "Borders"),
                grp(fs("additional_scale", "Additional Scale", 100.0, (50.0, 400.0), (50.0, 200.0), "%", 0), "Borders"),
                grp(b("detailed", "Detailed Analysis", false), "Advanced"),
                grp(pct("crop_less", "Crop Less <-> Smooth More", 50.0), "Advanced"),
            ],
        ),
        // ---- Generate ----
        video(
            "gradient",
            "Gradient",
            GENERATE,
            vec![
                pt("start", "Gradient Start", f64::NAN, f64::NAN),
                col("start_color", "Start Color", [0.0, 0.0, 0.0, 1.0]),
                pt("end", "Gradient End", f64::NAN, f64::NAN),
                col("end_color", "End Color", [1.0, 1.0, 1.0, 1.0]),
                ch("shape", "Gradient Shape", &["Linear", "Radial", "Reflected", "Diamond"], 0),
                pct("scatter", "Gradient Scatter", 0.0),
                pct("midpoint", "Midpoint", 50.0),
                blend_orig(),
            ],
        ),
        // ---- Image Control ----
        video(
            "channel_mix",
            "Channel Mix",
            IMAGE_CONTROL,
            vec![
                f("rr", "Red-Red", 100.0, -200.0, 200.0, ""),
                f("rg", "Red-Green", 0.0, -200.0, 200.0, ""),
                f("rb", "Red-Blue", 0.0, -200.0, 200.0, ""),
                f("rc", "Red-Const", 0.0, -200.0, 200.0, ""),
                f("gr", "Green-Red", 0.0, -200.0, 200.0, ""),
                f("gg", "Green-Green", 100.0, -200.0, 200.0, ""),
                f("gb", "Green-Blue", 0.0, -200.0, 200.0, ""),
                f("gc", "Green-Const", 0.0, -200.0, 200.0, ""),
                f("br", "Blue-Red", 0.0, -200.0, 200.0, ""),
                f("bg", "Blue-Green", 0.0, -200.0, 200.0, ""),
                f("bb", "Blue-Blue", 100.0, -200.0, 200.0, ""),
                f("bc", "Blue-Const", 0.0, -200.0, 200.0, ""),
                b("monochrome", "Monochrome", false),
            ],
        ),
        video(
            "color_replace",
            "Color Replace",
            IMAGE_CONTROL,
            vec![
                pct("similarity", "Similarity", 10.0),
                b("solid", "Solid Colors", false),
                col("target", "Target Color", [1.0, 0.0, 0.0, 1.0]),
                col("replace", "Replace Color", [0.0, 0.0, 1.0, 1.0]),
            ],
        ),
        video(
            "rounded_crop",
            "Rounded Crop",
            IMAGE_CONTROL,
            vec![
                pct("left", "Left", 0.0),
                pct("top", "Top", 0.0),
                pct("right", "Right", 0.0),
                pct("bottom", "Bottom", 0.0),
                px("radius", "Roundness", 40.0, 2000.0, 400.0),
                px("feather", "Feather", 0.0, 500.0, 100.0),
                px("border", "Border Width", 0.0, 500.0, 100.0),
                col("border_color", "Border Color", [1.0, 1.0, 1.0, 1.0]),
            ],
        ),
        // ---- Keying ----
        video(
            "alpha_adjust",
            "Alpha Adjust",
            KEYING,
            vec![pct("opacity", "Opacity", 100.0), b("ignore", "Ignore Alpha", false), b("invert", "Invert Alpha", false), b("mask_only", "Mask Only", false)],
        ),
        video(
            "logo_cutout",
            "Logo Cutout",
            KEYING,
            vec![
                ch("background", "Background", &["White", "Black", "Custom Color"], 0),
                col("color", "Background Color", [1.0, 1.0, 1.0, 1.0]),
                pct("threshold", "Threshold", 5.0),
                pct("softness", "Softness", 10.0),
                b("unmultiply", "Remove Fringe", true),
                b("invert", "Invert", false),
            ],
        ),
        // ---- Lights & Glows ----
        video(
            "echo_glow",
            "Echo Glow",
            LIGHTS,
            vec![
                pct("threshold", "Threshold", 60.0),
                fs("echoes", "Echoes", 4.0, (1.0, 12.0), (1.0, 12.0), "", 0),
                px("radius", "Radius", 12.0, 500.0, 100.0),
                fs("spread", "Spread", 1.8, (1.0, 4.0), (1.0, 4.0), "", 2),
                fs("intensity", "Intensity", 100.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                pct("decay", "Decay", 50.0),
                col("color", "Glow Color", [1.0, 0.85, 0.6, 1.0]),
            ],
        ),
        video(
            "edge_glow",
            "Edge Glow",
            LIGHTS,
            vec![
                pct("threshold", "Edge Threshold", 20.0),
                px("width", "Edge Width", 2.0, 50.0, 10.0),
                px("radius", "Glow Radius", 10.0, 500.0, 100.0),
                fs("intensity", "Intensity", 150.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                col("color", "Glow Color", [0.3, 0.8, 1.0, 1.0]),
                b("only", "Glow Only", false),
            ],
        ),
        video(
            "glint",
            "Glint",
            LIGHTS,
            vec![
                pct("threshold", "Threshold", 75.0),
                ch("rays", "Rays", &["2", "4", "6", "8"], 1),
                px("length", "Length", 60.0, 2000.0, 400.0),
                ang("rotation", "Rotation", 45.0),
                fs("intensity", "Intensity", 100.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                pct("colorize", "Colorize", 0.0),
            ],
        ),
        video(
            "light_leaks",
            "Light Leaks",
            LIGHTS,
            vec![
                col("c1", "Color 1", [1.0, 0.45, 0.1, 1.0]),
                col("c2", "Color 2", [1.0, 0.15, 0.35, 1.0]),
                pct("intensity", "Intensity", 70.0),
                fs("scale", "Scale", 100.0, (10.0, 1000.0), (20.0, 400.0), "%", 0),
                ang("direction", "Direction", 30.0),
                fs("speed", "Speed", 1.0, (0.0, 20.0), (0.0, 5.0), "", 2),
                seed(),
                ch("mode", "Blending Mode", &["Screen", "Add", "Overlay"], 0),
            ],
        ),
        video(
            "rgb_split",
            "RGB Split",
            LIGHTS,
            vec![
                ch("mode", "Mode", &["Linear", "Radial"], 0),
                px("amount", "Amount", 10.0, 1000.0, 100.0),
                ang("angle", "Angle", 0.0),
                pt("center", "Center", f64::NAN, f64::NAN),
                blend_orig(),
            ],
        ),
        video(
            "volumetric_rays",
            "Volumetric Rays",
            LIGHTS,
            vec![
                pt("center", "Source Point", f64::NAN, f64::NAN),
                pct("threshold", "Threshold", 60.0),
                fs("length", "Ray Length", 50.0, (0.0, 100.0), (0.0, 100.0), "%", 0),
                fs("intensity", "Intensity", 100.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                col("color", "Ray Color", [1.0, 0.95, 0.8, 1.0]),
                b("only", "Rays Only", false),
            ],
        ),
        video(
            "wonder_glow",
            "Wonder Glow",
            LIGHTS,
            vec![
                pct("threshold", "Threshold", 60.0),
                px("radius", "Radius", 25.0, 1000.0, 200.0),
                fs("intensity", "Intensity", 100.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                fs("saturation", "Saturation", 100.0, (0.0, 400.0), (0.0, 200.0), "%", 0),
                b("use_color", "Use Glow Color", false),
                col("color", "Glow Color", [1.0, 0.8, 0.5, 1.0]),
                ch("mode", "Blending Mode", &["Add", "Screen"], 0),
            ],
        ),
        // ---- Perspective ----
        video(
            "long_shadow",
            "Long Shadow",
            PERSPECTIVE,
            vec![
                ang("angle", "Angle", 135.0),
                px("length", "Length", 100.0, 4000.0, 600.0),
                col("color", "Shadow Color", [0.0, 0.0, 0.0, 1.0]),
                pct("opacity", "Opacity", 50.0),
                b("fade", "Fade", true),
                b("only", "Shadow Only", false),
            ],
        ),
        // ---- Stylize ----
        video(
            "brush_strokes",
            "Brush Strokes",
            STYLIZE,
            vec![
                ang("angle", "Stroke Angle", 135.0),
                fs("size", "Brush Size", 2.0, (0.0, 5.0), (0.0, 5.0), "", 1),
                fs("length", "Stroke Length", 10.0, (0.0, 40.0), (0.0, 40.0), "", 0),
                fs("density", "Stroke Density", 1.0, (0.0, 2.0), (0.0, 2.0), "", 2),
                fs("randomness", "Stroke Randomness", 1.0, (0.0, 2.0), (0.0, 2.0), "", 2),
                ch("surface", "Paint Surface", &["Paint On Original Image", "Paint On Transparent", "Paint On White", "Paint On Black"], 0),
                blend_orig(),
            ],
        ),
        video(
            "color_emboss",
            "Color Emboss",
            STYLIZE,
            vec![
                ang("direction", "Direction", 45.0),
                fs("relief", "Relief", 1.0, (0.0, 10.0), (0.0, 10.0), "", 2),
                f("contrast", "Contrast", 100.0, 0.0, 500.0, ""),
                blend_orig(),
            ],
        ),
        video(
            "roughen_edges",
            "Roughen Edges",
            STYLIZE,
            vec![
                ch("edge_type", "Edge Type", &["Roughen", "Roughen Color", "Cut", "Spiky", "Rusty", "Rusty Color", "Photocopy", "Photocopy Color"], 0),
                col("edge_color", "Edge Color", [0.8, 0.5, 0.2, 1.0]),
                px("border", "Border", 8.0, 500.0, 100.0),
                fs("sharpness", "Edge Sharpness", 1.0, (0.0, 10.0), (0.0, 10.0), "", 2),
                fs("influence", "Fractal Influence", 1.0, (0.0, 1.0), (0.0, 1.0), "", 2),
                fs("scale", "Scale", 100.0, (10.0, 1000.0), (10.0, 400.0), "", 1),
                fs("stretch", "Stretch Width or Height", 0.0, (-10.0, 10.0), (-5.0, 5.0), "", 2),
                pt("offset", "Offset (Turbulence)", 0.0, 0.0),
                fs("complexity", "Complexity", 2.0, (1.0, 10.0), (1.0, 10.0), "", 0),
                ang("evolution", "Evolution", 0.0),
                grp(seed(), "Evolution Options"),
            ],
        ),
        // ---- Time ----
        video("posterize_time", "Posterize Time", TIME, vec![fs("rate", "Frame Rate", 12.0, (0.1, 99.0), (1.0, 60.0), "", 2)]),
        // ---- Transform ----
        video(
            "rotate_3d",
            "3D Rotate",
            TRANSFORM,
            vec![
                ang("rot_x", "X Rotation", 0.0),
                ang("rot_y", "Y Rotation", 0.0),
                ang("rot_z", "Z Rotation", 0.0),
                fs("perspective", "Perspective", 50.0, (0.0, 100.0), (0.0, 100.0), "", 0),
                fs("z", "Z Position", 0.0, (-10000.0, 10000.0), (-2000.0, 2000.0), "", 0),
                b("hide_back", "Hide Back Face", false),
            ],
        ),
        video(
            "auto_reframe",
            "Auto Reframe",
            TRANSFORM,
            vec![
                ch("preset", "Motion Preset", &["Slower Motion", "Default", "Faster Motion"], 1),
                pt("offset", "Reframe Offset", 0.0, 0.0),
                fs("zoom", "Reframe Zoom", 100.0, (100.0, 400.0), (100.0, 200.0), "%", 1),
                ch("aspect", "Target Aspect", &["Sequence", "Vertical 9:16", "Square 1:1", "Vertical 4:5", "Horizontal 16:9"], 0),
            ],
        ),
        video(
            "camera_shake",
            "Camera Shake",
            TRANSFORM,
            vec![
                px("amount", "Position Amount", 20.0, 1000.0, 200.0),
                fs("rotation", "Rotation Amount", 1.0, (0.0, 45.0), (0.0, 10.0), "°", 2),
                pct("zoom", "Zoom Amount", 5.0),
                fs("frequency", "Frequency", 4.0, (0.1, 60.0), (0.1, 20.0), "Hz", 2),
                fs("complexity", "Complexity", 3.0, (1.0, 6.0), (1.0, 6.0), "", 0),
                pct("motion_blur", "Motion Blur", 0.0),
                seed(),
            ],
        ),
        video(
            "grow",
            "Grow",
            TRANSFORM,
            vec![
                fs("from", "Scale From", 100.0, (1.0, 1000.0), (50.0, 200.0), "%", 1),
                fs("to", "Scale To", 120.0, (1.0, 1000.0), (50.0, 200.0), "%", 1),
                pt("center", "Center", f64::NAN, f64::NAN),
                ch("easing", "Easing", EASINGS, 0),
            ],
        ),
        video(
            "shrink",
            "Shrink",
            TRANSFORM,
            vec![
                fs("from", "Scale From", 120.0, (1.0, 1000.0), (50.0, 200.0), "%", 1),
                fs("to", "Scale To", 100.0, (1.0, 1000.0), (50.0, 200.0), "%", 1),
                pt("center", "Center", f64::NAN, f64::NAN),
                ch("easing", "Easing", EASINGS, 0),
            ],
        ),
        video(
            "move",
            "Move",
            TRANSFORM,
            vec![
                pt("from", "Offset From", -100.0, 0.0),
                pt("to", "Offset To", 0.0, 0.0),
                ch("easing", "Easing", EASINGS, 3),
                pct("motion_blur", "Motion Blur", 0.0),
            ],
        ),
        video(
            "spin",
            "Spin",
            TRANSFORM,
            vec![
                fs("amount", "Rotation Amount", 360.0, (-36000.0, 36000.0), (-720.0, 720.0), "°", 1),
                pt("center", "Center", f64::NAN, f64::NAN),
                ch("easing", "Easing", EASINGS, 3),
                fs("scale", "Scale", 100.0, (1.0, 1000.0), (50.0, 200.0), "%", 1),
            ],
        ),
        video(
            "spacer",
            "Spacer",
            TRANSFORM,
            vec![
                px("left", "Left", 40.0, 4000.0, 400.0),
                px("top", "Top", 40.0, 4000.0, 400.0),
                px("right", "Right", 40.0, 4000.0, 400.0),
                px("bottom", "Bottom", 40.0, 4000.0, 400.0),
                b("uniform", "Uniform Spacing", true),
                px("radius", "Corner Radius", 0.0, 2000.0, 200.0),
                b("fill", "Fill Background", false),
                col("color", "Background Color", [0.0, 0.0, 0.0, 1.0]),
            ],
        ),
        video(
            "wiggle",
            "Wiggle",
            TRANSFORM,
            vec![
                fs("frequency", "Frequency", 2.0, (0.0, 60.0), (0.0, 10.0), "Hz", 2),
                px("amount", "Position Amount", 20.0, 2000.0, 200.0),
                fs("rotation", "Rotation Amount", 0.0, (0.0, 180.0), (0.0, 45.0), "°", 1),
                pct("scale", "Scale Amount", 0.0),
                ch("dimensions", "Dimensions", &["Horizontal and Vertical", "Horizontal", "Vertical"], 0),
                seed(),
            ],
        ),
        // ---- Utility ----
        video(
            "auto_align",
            "Auto Align",
            UTILITY,
            vec![
                ch("horizontal", "Horizontal Alignment", &["None", "Left", "Center", "Right"], 2),
                ch("vertical", "Vertical Alignment", &["None", "Top", "Center", "Bottom"], 2),
                px("margin_x", "Horizontal Margin", 0.0, 4000.0, 400.0),
                px("margin_y", "Vertical Margin", 0.0, 4000.0, 400.0),
            ],
        ),
        video(
            "cineon_converter",
            "Cineon Converter",
            UTILITY,
            vec![
                ch("conversion", "Conversion Type", &["Log to Linear", "Linear to Log", "Log to Log"], 0),
                fs("black10", "10 Bit Black Point", 95.0, (0.0, 1023.0), (0.0, 1023.0), "", 0),
                fs("black_internal", "Internal Black Point", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("white10", "10 Bit White Point", 685.0, (0.0, 1023.0), (0.0, 1023.0), "", 0),
                fs("white_internal", "Internal White Point", 255.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("gamma", "Gamma", 1.7, (0.1, 5.0), (0.1, 5.0), "", 2),
                fs("rolloff", "Highlight Rolloff", 20.0, (0.0, 255.0), (0.0, 100.0), "", 0),
            ],
        ),
        video(
            "clone",
            "Clone",
            UTILITY,
            vec![
                fs("columns", "Columns", 2.0, (1.0, 16.0), (1.0, 16.0), "", 0),
                fs("rows", "Rows", 1.0, (1.0, 16.0), (1.0, 16.0), "", 0),
                px("gap", "Gap", 0.0, 1000.0, 100.0),
                b("mirror", "Mirror Alternate", false),
            ],
        ),
        video(
            "stroke",
            "Stroke",
            UTILITY,
            vec![
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                px("width", "Width", 4.0, 500.0, 100.0),
                ch("position", "Position", &["Outside", "Center", "Inside"], 0),
                pct("opacity", "Opacity", 100.0),
                b("only", "Stroke Only", false),
            ],
        ),
        // ---- Video ----
        video(
            "metadata_burnin",
            "Metadata & Timecode Burn-in",
            VIDEO,
            vec![
                ch("source", "Source", &["Sequence Timecode", "Media Timecode", "Clip Name", "File Name", "Frame Count", "Sequence Name"], 0),
                pt("position", "Position", f64::NAN, f64::NAN),
                ch("alignment", "Alignment", &["Bottom Center", "Bottom Left", "Bottom Right", "Top Center", "Top Left", "Top Right", "Custom"], 0),
                fs("size", "Size", 6.0, (1.0, 100.0), (1.0, 30.0), "%", 1),
                col("color", "Text Color", [1.0, 1.0, 1.0, 1.0]),
                pct("opacity", "Background Opacity", 60.0),
                txt("prefix", "Prefix"),
            ],
        ),
        // ---- Obsolete (still render in old projects) ----
        video(
            "echo",
            "Echo",
            OBSOLETE,
            vec![
                fs("time", "Echo Time (seconds)", -0.033, (-10.0, 10.0), (-2.0, 2.0), "", 3),
                fs("count", "Number Of Echoes", 1.0, (0.0, 30.0), (0.0, 30.0), "", 0),
                fs("start", "Starting Intensity", 1.0, (0.0, 1.0), (0.0, 1.0), "", 2),
                fs("decay", "Decay", 1.0, (0.0, 2.0), (0.0, 2.0), "", 2),
                ch("operator", "Echo Operator", ECHO_OPERATORS, 0),
            ],
        ),
        video(
            "lightning",
            "Lightning",
            OBSOLETE,
            vec![
                pt("start", "Start Point", f64::NAN, f64::NAN),
                pt("end", "End Point", f64::NAN, f64::NAN),
                fs("segments", "Segments", 7.0, (1.0, 100.0), (1.0, 40.0), "", 0),
                fs("amplitude", "Amplitude", 10.0, (0.0, 100.0), (0.0, 100.0), "", 1),
                fs("detail", "Detail Level", 2.0, (0.0, 10.0), (0.0, 10.0), "", 0),
                fs("detail_amplitude", "Detail Amplitude", 0.3, (0.0, 1.0), (0.0, 1.0), "", 2),
                fs("branching", "Branching", 0.3, (0.0, 1.0), (0.0, 1.0), "", 2),
                fs("speed", "Speed", 10.0, (0.0, 100.0), (0.0, 100.0), "", 0),
                px("width", "Width", 10.0, 200.0, 60.0),
                fs("core", "Core Width", 0.4, (0.0, 1.0), (0.0, 1.0), "", 2),
                col("outside", "Outside Color", [0.35, 0.3, 1.0, 1.0]),
                col("inside", "Inside Color", [1.0, 1.0, 1.0, 1.0]),
                seed(),
                ch("mode", "Blending Mode", SIMPLE_BLEND, 1),
            ],
        ),
        video(
            "cell_pattern",
            "Cell Pattern",
            OBSOLETE,
            vec![
                ch("pattern", "Cell Pattern", &["Bubbles", "Crystals", "Plates", "Static Plates", "Crystallize", "Pillow", "Mixed Crystals", "Tubular"], 0),
                b("invert", "Invert", false),
                fs("contrast", "Contrast", 100.0, (0.0, 10000.0), (0.0, 400.0), "", 1),
                ch("overflow", "Overflow", &["Clip", "Soft Clamp", "Wrap Back"], 0),
                fs("disperse", "Disperse", 1.0, (0.0, 1.5), (0.0, 1.5), "", 2),
                px("size", "Size", 60.0, 2000.0, 200.0),
                pt("offset", "Offset", 0.0, 0.0),
                ang("evolution", "Evolution", 0.0),
                seed(),
            ],
        ),
        video(
            "checkerboard",
            "Checkerboard",
            OBSOLETE,
            vec![
                pt("anchor", "Anchor", f64::NAN, f64::NAN),
                px("width", "Width", 64.0, 4000.0, 400.0),
                px("height", "Height", 64.0, 4000.0, 400.0),
                b("square", "Square (Width Only)", true),
                px("feather", "Feather", 0.0, 100.0, 20.0),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", &["None", "Normal", "Add", "Multiply", "Screen", "Overlay"], 0),
            ],
        ),
        video(
            "ellipse",
            "Ellipse",
            OBSOLETE,
            vec![
                pt("center", "Center", f64::NAN, f64::NAN),
                px("width", "Width", 200.0, 4000.0, 1000.0),
                px("height", "Height", 200.0, 4000.0, 1000.0),
                px("thickness", "Thickness", 8.0, 1000.0, 100.0),
                pct("softness", "Softness", 15.0),
                col("inside", "Inside Color", [1.0, 1.0, 1.0, 1.0]),
                col("outside", "Outside Color", [0.0, 0.5, 1.0, 1.0]),
                b("composite", "Composite on Original", false),
            ],
        ),
        video(
            "paint_bucket",
            "Paint Bucket",
            OBSOLETE,
            vec![
                pt("point", "Fill Point", f64::NAN, f64::NAN),
                ch("selector", "Fill Selector", &["Color & Alpha", "Straight Color", "Transparency", "Opacity"], 0),
                fs("tolerance", "Tolerance", 10.0, (0.0, 255.0), (0.0, 100.0), "", 1),
                b("invert", "Invert Fill", false),
                col("color", "Color", [1.0, 0.0, 0.0, 1.0]),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", &["Normal", "Behind", "Add", "Multiply", "Screen"], 0),
            ],
        ),
        video(
            "write_on",
            "Write-on",
            OBSOLETE,
            vec![
                pt("brush", "Brush Position", f64::NAN, f64::NAN),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                px("size", "Brush Size", 8.0, 500.0, 100.0),
                pct("hardness", "Brush Hardness", 75.0),
                pct("opacity", "Brush Opacity", 100.0),
                fs("stroke_length", "Stroke Length (secs)", 0.0, (0.0, 3600.0), (0.0, 30.0), "", 2),
                fs("spacing", "Brush Spacing (secs)", 0.01, (0.001, 1.0), (0.001, 0.2), "", 3),
                ch("style", "Paint Style", &["On Original Image", "On Transparent", "Reveal Original Image"], 0),
            ],
        ),
        // ---- Legacy bin ----
        video(
            "alpha_glow",
            "Alpha Glow",
            LEGACY,
            vec![
                fs("glow", "Glow", 30.0, (0.0, 100.0), (0.0, 100.0), "", 0),
                fs("brightness", "Brightness", 252.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                col("start_color", "Start Color", [1.0, 1.0, 1.0, 1.0]),
                col("end_color", "End Color", [1.0, 1.0, 1.0, 1.0]),
                b("use_end", "Use End Color", false),
                b("fade_out", "Fade Out", true),
            ],
        ),
        video(
            "block_dissolve",
            "Block Dissolve",
            LEGACY,
            vec![
                pct("completion", "Transition Completion", 0.0),
                px("block_w", "Block Width", 1.0, 4000.0, 200.0),
                px("block_h", "Block Height", 1.0, 4000.0, 200.0),
                px("feather", "Feather", 0.0, 100.0, 20.0),
                b("soft", "Soft Edges (Best Quality)", true),
            ],
        ),
        video(
            "directional_blur_legacy",
            "Directional Blur (Legacy)",
            LEGACY,
            vec![ang("direction", "Direction", 0.0), fs("length", "Blur Length", 0.0, (0.0, 1000.0), (0.0, 20.0), "", 1)],
        ),
        video(
            "gaussian_blur_legacy",
            "Gaussian Blur (Legacy)",
            LEGACY,
            vec![
                fs("blurriness", "Blurriness", 0.0, (0.0, 3000.0), (0.0, 100.0), "", 1),
                ch("dimensions", "Blur Dimensions", &["Horizontal and Vertical", "Horizontal", "Vertical"], 0),
            ],
        ),
        video(
            "gradient_wipe_legacy",
            "Gradient Wipe (Legacy)",
            LEGACY,
            vec![
                pct("completion", "Transition Completion", 0.0),
                pct("softness", "Transition Softness", 0.0),
                ch("layer", "Gradient Layer", TRACK_CHOICES, 0),
                ch("placement", "Gradient Placement", &["Tile Gradient", "Center Gradient", "Stretch Gradient to Fit"], 2),
                b("invert", "Invert Gradient", false),
            ],
        ),
        video(
            "linear_wipe_legacy",
            "Linear Wipe (Legacy)",
            LEGACY,
            vec![pct("completion", "Transition Completion", 0.0), ang("angle", "Wipe Angle", 90.0), px("feather", "Feather", 0.0, 4000.0, 400.0)],
        ),
        video(
            "magnify_legacy",
            "Magnify (Legacy)",
            LEGACY,
            vec![
                ch("shape", "Shape", &["Circle", "Square"], 0),
                pt("center", "Center", f64::NAN, f64::NAN),
                fs("magnification", "Magnification", 200.0, (100.0, 600.0), (100.0, 600.0), "", 1),
                px("size", "Size", 100.0, 4000.0, 600.0),
                px("feather", "Feather", 0.0, 1000.0, 200.0),
                pct("opacity", "Opacity", 100.0),
                ch("mode", "Blending Mode", SIMPLE_BLEND, 0),
            ],
        ),
        video(
            "mosaic_legacy",
            "Mosaic (Legacy)",
            LEGACY,
            vec![
                fs("horizontal", "Horizontal Blocks", 10.0, (1.0, 4000.0), (1.0, 200.0), "", 0),
                fs("vertical", "Vertical Blocks", 10.0, (1.0, 4000.0), (1.0, 200.0), "", 0),
                b("sharp", "Sharp Colors", false),
            ],
        ),
        video(
            "noise_legacy",
            "Noise (Legacy)",
            LEGACY,
            vec![pct("amount", "Amount of Noise", 0.0), b("color", "Use Color Noise", true), b("clip", "Clipping", true)],
        ),
        video(
            "twirl_legacy",
            "Twirl (Legacy)",
            LEGACY,
            vec![
                ang("angle", "Angle", 50.0),
                fs("radius", "Twirl Radius", 30.0, (0.0, 100.0), (0.0, 100.0), "", 1),
                pt("center", "Twirl Center", f64::NAN, f64::NAN),
            ],
        ),
    ];
    v.extend(immersive());
    v
}

/// Parameter values written when Ultra Key's Setting is Default, Relaxed or Aggressive.
/// Custom (index 3) leaves the current values. Aggressive matches the matte that cleans
/// light and dark clothing on a typical green screen.
pub fn ultra_key_setting(setting: u32) -> Option<&'static [(&'static str, f64)]> {
    Some(match setting {
        0 => &[
            ("transparency", 45.0),
            ("highlight", 10.0),
            ("shadow", 50.0),
            ("tolerance", 50.0),
            ("pedestal", 10.0),
            ("choke", 0.0),
            ("soften", 0.0),
            ("contrast", 0.0),
            ("mid_point", 50.0),
            ("desaturate", 25.0),
            ("range", 50.0),
            ("spill", 50.0),
            ("spill_luma", 50.0),
        ],
        1 => &[
            ("transparency", 30.0),
            ("highlight", 15.0),
            ("shadow", 35.0),
            ("tolerance", 70.0),
            ("pedestal", 5.0),
            ("choke", 0.0),
            ("soften", 8.0),
            ("contrast", 0.0),
            ("mid_point", 50.0),
            ("desaturate", 15.0),
            ("range", 50.0),
            ("spill", 40.0),
            ("spill_luma", 50.0),
        ],
        2 => &[
            ("transparency", 40.0),
            ("highlight", 10.0),
            ("shadow", 55.0),
            ("tolerance", 90.0),
            ("pedestal", 50.0),
            ("choke", 10.0),
            ("soften", 10.0),
            ("contrast", 10.0),
            ("mid_point", 50.0),
            ("desaturate", 50.0),
            ("range", 50.0),
            ("spill", 50.0),
            ("spill_luma", 50.0),
        ],
        _ => return None,
    })
}

/// Extra parameters added to core effects by the 26.x rebuilds (old instances fall back to the
/// defaults, so projects round-trip unchanged).
pub(super) fn extend_core(v: &mut [EffectDef]) {
    for e in v.iter_mut() {
        match e.id {
            "ultra_key" => e.params.extend([
                grp(f("contrast", "Contrast", 0.0, 0.0, 100.0, ""), "Matte Cleanup"),
                grp(f("mid_point", "Mid Point", 50.0, 0.0, 100.0, ""), "Matte Cleanup"),
                grp(f("desaturate", "Desaturate", 25.0, 0.0, 100.0, ""), "Spill Suppression"),
                grp(f("range", "Range", 50.0, 0.0, 100.0, ""), "Spill Suppression"),
                grp(f("spill_luma", "Luma", 50.0, 0.0, 100.0, ""), "Spill Suppression"),
                grp(fs("cc_saturation", "Saturation", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "Color Correction"),
                grp(ang("cc_hue", "Hue", 0.0), "Color Correction"),
                grp(fs("cc_luminance", "Luminance", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "Color Correction"),
            ]),
            "lens_flare" => e.params.extend([
                ch("lens_type", "Lens Type", &["50-300mm Zoom", "35mm Prime", "105mm Prime", "Anamorphic"], 0),
                col("color", "Flare Color", [1.0, 0.9, 0.75, 1.0]),
                fs("size", "Size", 100.0, (10.0, 400.0), (10.0, 300.0), "%", 0),
                fs("ghosts", "Ghosts", 3.0, (0.0, 8.0), (0.0, 8.0), "", 0),
            ]),
            "track_matte" => {
                if let Some(p) = e.params.iter_mut().find(|p| p.id == "matte") {
                    p.kind = ParamKind::Choice(TRACK_CHOICES);
                }
            }
            "noise" => e.params.push(fs("grain_size", "Grain Size", 1.0, (1.0, 10.0), (1.0, 6.0), "", 1)),
            "mosaic" => e.params.push(pct("softness", "Softness", 0.0)),
            "simple_text" => e.params.extend([
                ParamDef { default: ParamValue::Text("Simple Text".into()), ..txt("text", "Text") },
                pt("position", "Position", f64::NAN, f64::NAN),
                fs("size", "Size", 64.0, (1.0, 1000.0), (4.0, 300.0), "", 0),
                ch("font", "Font", &["Inter", "JetBrains Mono"], 0),
                ch("alignment", "Alignment", &["Left", "Center", "Right"], 1),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                pct("bg_opacity", "Background Opacity", 0.0),
            ]),
            _ => {}
        }
    }
}

/// Effects-panel badges (Accelerated, 32-bit, YUV) per the Premiere 26 list.
pub(super) fn apply_badges(v: &mut [EffectDef]) {
    for e in v.iter_mut().filter(|e| e.kind == EffectKind::Video && !e.intrinsic) {
        let id = e.id;
        let folder = e.category.get(1).copied().unwrap_or("");
        let a32 = matches!(
            id,
            "bokeh_blur"
                | "channel_blur"
                | "compound_blur"
                | "directional_blur"
                | "focus_blur"
                | "gaussian_blur"
                | "asc_cdl"
                | "lumetri"
                | "video_limiter"
                | "vignette"
                | "magnify"
                | "twirl"
                | "gradient"
                | "channel_mix"
                | "rounded_crop"
                | "logo_cutout"
                | "luma_key"
                | "track_matte"
                | "noise"
                | "long_shadow"
                | "mosaic"
                | "rotate_3d"
                | "camera_shake"
                | "grow"
                | "move"
                | "shrink"
                | "spacer"
                | "spin"
                | "wiggle"
                | "proc_amp"
        ) || folder == "Lights & Glows";
        let yuv = matches!(id, "proc_amp" | "lumetri" | "video_limiter" | "black_white" | "alpha_adjust" | "luma_key" | "track_matte");
        let accel = match e.category.first().copied() {
            Some("Legacy") => !matches!(id, "camera_blur"),
            _ if folder == "Obsolete" => false,
            _ => !matches!(id, "clone" | "stroke"),
        };
        e.accelerated = accel;
        e.float32 = a32;
        e.yuv = yuv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Premiere 26's Video Effects bin: every effect, in its folder (names are functional facts).
    const CATALOGUE: &[(&str, &[&str])] = &[
        ("Adjust", &["Extract", "Levels", "Lighting Effects", "ProcAmp"]),
        (
            "Blur & Sharpen",
            &[
                "Bokeh Blur",
                "Channel Blur",
                "Compound Blur",
                "Directional Blur",
                "Focus Blur",
                "Gaussian Blur",
                "Reduce Interlace Flicker",
                "Sharpen",
                "Unsharp Mask",
            ],
        ),
        ("Color Correction", &["ASC CDL", "Brightness & Contrast", "Lumetri Color", "Tint", "Video Limiter", "Vignette"]),
        ("Distort", &["Corner Pin", "Lens Distortion", "Magnify", "Mirror", "Spherize", "Turbulent Displace", "Twirl", "Warp Stabilizer", "Wave Warp"]),
        ("Generate", &["4-Color Gradient", "Gradient"]),
        ("Image Control", &["Black & White", "Channel Mix", "Color Pass", "Color Replace", "Gamma Correction", "Invert", "Rounded Crop"]),
        (
            "Immersive Video",
            &[
                "VR Blur",
                "VR Chromatic Aberrations",
                "VR Color Gradients",
                "VR De-Noise",
                "VR Digital Glitch",
                "VR Fractal Noise",
                "VR Glow",
                "VR Plane to Sphere",
                "VR Projection",
                "VR Rotate Sphere",
                "VR Sharpen",
            ],
        ),
        ("Keying", &["Alpha Adjust", "Color Key", "Logo Cutout", "Luma Key", "Track Matte Key", "Ultra Key"]),
        ("Lights & Glows", &["Echo Glow", "Edge Glow", "Glint", "Lens Flare", "Light Leaks", "RGB Split", "Volumetric Rays", "Wonder Glow"]),
        ("Noise & Grain", &["Noise"]),
        ("Perspective", &["Basic 3D", "Drop Shadow", "Long Shadow"]),
        ("Stylize", &["Brush Strokes", "Color Emboss", "Find Edges", "Mosaic", "Posterize", "Roughen Edges", "Strobe Light"]),
        ("Time", &["Posterize Time"]),
        (
            "Transform",
            &[
                "3D Rotate",
                "Auto Reframe",
                "Camera Shake",
                "Grow",
                "Horizontal Flip",
                "Move",
                "Offset",
                "Shrink",
                "Spacer",
                "Spin",
                "Transform",
                "Vertical Flip",
                "Wiggle",
            ],
        ),
        ("Utility", &["Auto Align", "Cineon Converter", "Clone", "Simple Text", "Stroke"]),
        ("Video", &["Metadata & Timecode Burn-in"]),
    ];

    const LEGACY_NAMES: &[&str] = &[
        "Alpha Glow",
        "Block Dissolve",
        "Camera Blur",
        "Crop",
        "Directional Blur (Legacy)",
        "Edge Feather",
        "Gaussian Blur (Legacy)",
        "Gradient Wipe (Legacy)",
        "Linear Wipe (Legacy)",
        "Magnify (Legacy)",
        "Mosaic (Legacy)",
        "Noise (Legacy)",
        "Ramp",
        "Replicate",
        "Twirl (Legacy)",
    ];

    #[test]
    fn premiere_26_video_effects_catalogue() {
        let mut n = 0;
        for (folder, names) in CATALOGUE {
            for name in *names {
                let d = find_effect_by_name(name).unwrap_or_else(|| panic!("missing {name}"));
                assert_eq!(d.kind, EffectKind::Video, "{name}");
                assert_eq!(d.category, &["Video Effects", *folder], "{name} folder");
                assert_eq!(d.accelerated, !matches!(d.id, "clone" | "stroke"), "{name} accelerated badge");
                n += 1;
            }
        }
        assert_eq!(n, 93);
        assert_eq!(CATALOGUE.len(), 16);
        // nothing else sits in the current folders
        for d in effect_defs().iter().filter(|d| d.kind == EffectKind::Video && d.category.first() == Some(&"Video Effects")) {
            let f = d.category[1];
            if f == "Obsolete" {
                continue;
            }
            let listed = CATALOGUE.iter().any(|(cf, names)| *cf == f && names.contains(&d.name));
            assert!(listed, "{} in {f} is not in Premiere's list", d.name);
        }
        for name in LEGACY_NAMES {
            let d = find_effect_by_name(name).unwrap_or_else(|| panic!("missing legacy {name}"));
            assert_eq!(d.category, LEGACY, "{name}");
        }
        // badge spot checks
        let b = |n: &str| find_effect_by_name(n).map(|d| (d.accelerated, d.float32, d.yuv)).unwrap();
        assert_eq!(b("ProcAmp"), (true, true, true));
        assert_eq!(b("Luma Key"), (true, true, true));
        assert_eq!(b("Black & White"), (true, false, true));
        assert_eq!(b("Glint"), (true, true, false));
        assert_eq!(b("Sharpen"), (true, false, false));
    }

    #[test]
    fn auto_points_resolve_to_frame_fractions() {
        let mut e = find_effect("corner_pin").unwrap().instance();
        crate::resolve_auto_points(&mut e, (1920, 1080), (1920, 1080));
        let v = |k: &str| e.param(k).unwrap().value.as_vec2().unwrap();
        assert_eq!((v("upper_left").x, v("upper_left").y), (0.0, 0.0));
        assert_eq!((v("lower_right").x, v("lower_right").y), (1920.0, 1080.0));
        assert_eq!((v("upper_right").x, v("upper_right").y), (1920.0, 0.0));
    }
}
