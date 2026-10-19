//! Video transition definitions: the Effects-panel folders (Premiere 26 layout: 84 transitions in
//! 10 folders, plus the Legacy folder) and each transition's Effect Controls parameters.
//!
//! Transition parameters are not animatable (as in Premiere). Pixel implementations live in
//! `filmcraft-render` (`transitions`), looked up by id. Ids are stable: projects reference
//! transitions by id, so an id is never renamed (a few modern transitions reuse the id of the
//! transition they replaced, e.g. `push`, `clock_wipe`, `page_peel`; the old behaviour lives on
//! under a `*_legacy` id).

use crate::effect::{EffectDef, EffectKind, ParamDef, ang, b, ch, col, fs, pt};

/// Top-level Effects panel folders, in Premiere's order, with FilmCraft's own effects before Legacy.
pub const EFFECT_TOP_FOLDERS: &[&str] =
    &["Presets", "Lumetri Presets", "Audio Effects", "Audio Transitions", "Video Effects", "Video Transitions", "FilmCraft Effects", "Legacy"];

/// The ten `Video Transitions` sub-folders, in panel order.
pub const VIDEO_TRANSITION_FOLDERS: &[&str] =
    &["Animation", "Dissolve", "Grunge & Distort", "Immersive Video", "Lights & Blurs", "Slide", "Smart Tools", "Text", "Transformers", "Wipe"];

/// Category of the legacy transitions (`Legacy > Video Transitions`).
pub const LEGACY_VIDEO_TRANSITIONS: &[&str] = &["Legacy", "Video Transitions"];
/// Category of transitions Premiere no longer lists (kept so old projects still render; not shown
/// in the Effects panel).
pub const OBSOLETE_VIDEO_TRANSITIONS: &[&str] = &["Obsolete", "Video Transitions"];

const ANIMATION: &[&str] = &["Video Transitions", "Animation"];
const DISSOLVE: &[&str] = &["Video Transitions", "Dissolve"];
const GRUNGE: &[&str] = &["Video Transitions", "Grunge & Distort"];
const IMMERSIVE: &[&str] = &["Video Transitions", "Immersive Video"];
const LIGHTS: &[&str] = &["Video Transitions", "Lights & Blurs"];
const SLIDE: &[&str] = &["Video Transitions", "Slide"];
const SMART: &[&str] = &["Video Transitions", "Smart Tools"];
const TEXT: &[&str] = &["Video Transitions", "Text"];
const TRANSFORMERS: &[&str] = &["Video Transitions", "Transformers"];
const WIPE: &[&str] = &["Video Transitions", "Wipe"];

/// Direction choices (index 3, "From West", moves left → right).
pub const DIR_OPTS: &[&str] = &["From North", "From East", "From South", "From West"];
pub const AA_OPTS: &[&str] = &["Off", "Low", "Medium", "High"];
pub const AXIS_OPTS: &[&str] = &["Horizontal", "Vertical"];
pub const CORNER_OPTS: &[&str] = &["Top Left", "Top Right", "Bottom Right", "Bottom Left"];
pub const SHAPE_OPTS: &[&str] = &["Circle", "Square", "Diamond", "Star", "Heart", "Triangle", "Hexagon"];
pub const SPIN_OPTS: &[&str] = &["Clockwise", "Counter-clockwise"];
pub const ORDER_OPTS: &[&str] = &["Random", "Left to Right", "Center Out"];

/// Not animatable (transition parameters have no stopwatch in Premiere).
fn fixed(mut p: ParamDef) -> ParamDef {
    p.animatable = false;
    p
}
fn num(id: &'static str, label: &'static str, def: f64, max: f64, unit: &'static str) -> ParamDef {
    fixed(fs(id, label, def, (0.0, max), (0.0, max), unit, 1))
}
fn range(id: &'static str, label: &'static str, def: f64, min: f64, max: f64, unit: &'static str) -> ParamDef {
    fixed(fs(id, label, def, (min, max), (min, max), unit, 0))
}
fn colour(id: &'static str, label: &'static str, c: [f32; 4]) -> ParamDef {
    fixed(col(id, label, c))
}
fn angle(id: &'static str, label: &'static str, def: f64) -> ParamDef {
    fixed(ang(id, label, def))
}
fn centre() -> ParamDef {
    fixed(pt("center", "Center", f64::NAN, f64::NAN))
}
fn direction() -> ParamDef {
    ch("direction", "Direction", DIR_OPTS, 3)
}
fn motion_blur() -> ParamDef {
    num("motion_blur", "Motion Blur", 50.0, 100.0, "%")
}
fn seed() -> ParamDef {
    range("seed", "Random Seed", 1.0, 0.0, 9999.0, "")
}
fn amount(def: f64) -> ParamDef {
    num("amount", "Amount", def, 100.0, "%")
}
fn feather(def: f64) -> ParamDef {
    fixed(fs("feather", "Feather", def, (0.0, 2000.0), (0.0, 500.0), "px", 0))
}
/// Border Width / Border Color / Anti-aliasing Quality (Premiere's wipe and iris controls).
fn border() -> [ParamDef; 3] {
    [
        num("border_width", "Border Width", 0.0, 200.0, "px"),
        colour("border_color", "Border Color", [0.0, 0.0, 0.0, 1.0]),
        ch("antialias", "Anti-aliasing Quality", AA_OPTS, 1),
    ]
}

#[derive(Clone, Copy)]
enum Badges {
    /// Accelerated + 32-bit (every modern transition).
    A32,
    /// Accelerated only.
    A,
    /// Accelerated + YUV.
    AYuv,
    /// Accelerated + 32-bit + YUV.
    A32Yuv,
}

fn tr(id: &'static str, name: &'static str, cat: &'static [&'static str], badges: Badges, params: Vec<ParamDef>) -> EffectDef {
    let (float32, yuv) = match badges {
        Badges::A32 => (true, false),
        Badges::A => (false, false),
        Badges::AYuv => (false, true),
        Badges::A32Yuv => (true, true),
    };
    EffectDef { id, name, kind: EffectKind::VideoTransition, category: cat, params, intrinsic: false, accelerated: true, float32, yuv }
}

fn with(mut v: Vec<ParamDef>, extra: impl IntoIterator<Item = ParamDef>) -> Vec<ParamDef> {
    v.extend(extra);
    v
}

pub(crate) fn video_transition_defs() -> Vec<EffectDef> {
    use Badges::*;
    let m = A32;
    vec![
        // ---- Animation ----
        tr("block_motion", "Block Motion", ANIMATION, m, vec![direction(), range("blocks", "Blocks", 6.0, 2.0, 32.0, ""), motion_blur()]),
        tr("flip_motion", "Flip Motion", ANIMATION, m, vec![direction(), num("perspective", "Perspective", 50.0, 100.0, "%"), motion_blur()]),
        tr("fold_motion", "Fold Motion", ANIMATION, m, vec![direction(), num("perspective", "Perspective", 50.0, 100.0, "%")]),
        tr("pop_motion", "Pop Motion", ANIMATION, m, vec![range("overshoot", "Overshoot", 20.0, 0.0, 100.0, "%"), motion_blur()]),
        tr("pull_motion", "Pull Motion", ANIMATION, m, vec![direction(), motion_blur()]),
        tr(
            "spin_motion",
            "Spin Motion",
            ANIMATION,
            m,
            vec![range("rotation", "Rotation", 360.0, 0.0, 1440.0, "°"), ch("spin", "Spin Direction", SPIN_OPTS, 0), motion_blur()],
        ),
        tr("spring_motion", "Spring Motion", ANIMATION, m, vec![direction(), range("bounce", "Bounce", 40.0, 0.0, 100.0, "%"), motion_blur()]),
        tr("travel_motion", "Travel Motion", ANIMATION, m, vec![direction(), range("zoom", "Zoom Out", 40.0, 0.0, 90.0, "%"), motion_blur()]),
        // ---- Dissolve ----
        tr("additive_dissolve", "Additive Dissolve", DISSOLVE, m, vec![]),
        tr("blur_dissolve", "Blur Dissolve", DISSOLVE, m, vec![num("blur", "Blur", 40.0, 400.0, "px")]),
        tr(
            "burn_alpha",
            "Burn Alpha",
            DISSOLVE,
            m,
            vec![colour("burn_color", "Burn Color", [1.0, 0.45, 0.1, 1.0]), num("softness", "Softness", 25.0, 100.0, "%"), seed()],
        ),
        tr("cross_dissolve", "Cross Dissolve", DISSOLVE, m, vec![]),
        tr("dip_to_black", "Dip to Black", DISSOLVE, m, vec![]),
        tr("dip_to_color", "Dip to Color", DISSOLVE, m, vec![colour("color", "Color", [0.85, 0.2, 0.2, 1.0]), num("hold", "Hold", 0.0, 90.0, "%")]),
        tr("dip_to_white", "Dip to White", DISSOLVE, m, vec![]),
        tr("film_dissolve", "Film Dissolve", DISSOLVE, m, vec![]),
        tr(
            "luma_fade",
            "Luma Fade",
            DISSOLVE,
            m,
            vec![ch("source", "Luma Source", &["Outgoing", "Incoming"], 0), num("softness", "Softness", 20.0, 100.0, "%"), b("invert", "Invert", false)],
        ),
        tr("morph_cut", "Morph Cut", DISSOLVE, m, vec![]),
        tr(
            "mosaic_transition",
            "Mosaic",
            DISSOLVE,
            m,
            vec![num("block_size", "Maximum Block Size", 64.0, 400.0, "px"), b("dissolve", "Dissolve Blocks", true)],
        ),
        // ---- Grunge & Distort ----
        tr("chaos", "Chaos", GRUNGE, m, vec![amount(60.0), seed()]),
        tr("earthquake", "Earthquake", GRUNGE, m, vec![num("shake", "Shake", 40.0, 400.0, "px"), seed(), motion_blur()]),
        tr("flicker", "Flicker", GRUNGE, m, vec![range("flickers", "Flickers", 8.0, 1.0, 40.0, ""), seed()]),
        tr("glass", "Glass", GRUNGE, m, vec![num("cell_size", "Shard Size", 90.0, 1000.0, "px"), num("refraction", "Refraction", 40.0, 400.0, "px"), seed()]),
        tr("glitch", "Glitch", GRUNGE, m, vec![amount(60.0), seed()]),
        tr(
            "grunge",
            "Grunge",
            GRUNGE,
            m,
            vec![
                num("scale", "Scale", 120.0, 1000.0, "px"),
                colour("edge_color", "Edge Color", [0.22, 0.14, 0.08, 1.0]),
                num("edge_width", "Edge Width", 12.0, 200.0, "px"),
                seed(),
            ],
        ),
        tr("kaleidoscope", "Kaleidoscope", GRUNGE, m, vec![range("segments", "Segments", 6.0, 2.0, 32.0, ""), num("zoom", "Zoom", 30.0, 100.0, "%")]),
        tr(
            "liquid_distortion",
            "Liquid Distortion",
            GRUNGE,
            m,
            vec![num("amount", "Amount", 60.0, 500.0, "px"), num("scale", "Scale", 200.0, 2000.0, "px"), seed()],
        ),
        tr("tv_power", "TV Power", GRUNGE, m, vec![colour("glow_color", "Glow Color", [0.85, 0.95, 1.0, 1.0]), num("line", "Line Thickness", 4.0, 50.0, "px")]),
        tr("vhs_damage", "VHS Damage", GRUNGE, m, vec![amount(60.0), seed()]),
        // ---- Immersive Video (flat equirectangular approximations; see the render crate) ----
        tr("vr_chroma_leaks", "VR Chroma Leaks", IMMERSIVE, m, vec![amount(70.0), seed()]),
        tr("vr_gradient_wipe", "VR Gradient Wipe", IMMERSIVE, m, vec![num("softness", "Softness", 20.0, 100.0, "%"), b("invert", "Invert Gradient", false)]),
        tr("vr_iris_wipe", "VR Iris Wipe", IMMERSIVE, m, with(vec![centre(), feather(8.0)], border())),
        tr("vr_light_leaks", "VR Light Leaks", IMMERSIVE, m, vec![amount(70.0), seed()]),
        tr("vr_light_rays", "VR Light Rays", IMMERSIVE, m, vec![centre(), amount(70.0)]),
        tr("vr_mobius_zoom", "VR Mobius Zoom", IMMERSIVE, m, vec![num("zoom", "Zoom", 60.0, 100.0, "%"), angle("twist", "Twist", 180.0)]),
        tr(
            "vr_random_blocks",
            "VR Random Blocks",
            IMMERSIVE,
            m,
            vec![num("block_size", "Block Size", 80.0, 1000.0, "px"), num("softness", "Softness", 10.0, 100.0, "%"), seed()],
        ),
        tr("vr_spherical_blur", "VR Spherical Blur", IMMERSIVE, m, vec![num("blur", "Blur", 60.0, 400.0, "px")]),
        // ---- Lights & Blurs ----
        tr("burn_chroma", "Burn Chroma", LIGHTS, m, vec![amount(80.0)]),
        tr("chroma_leak", "Chroma Leak", LIGHTS, m, vec![amount(70.0), seed()]),
        tr("cross_zoom", "Cross Zoom", LIGHTS, m, vec![centre(), num("strength", "Zoom Strength", 60.0, 100.0, "%")]),
        tr("directional_blur_transition", "Directional Blur", LIGHTS, m, vec![angle("angle", "Angle", 90.0), num("blur", "Blur Length", 160.0, 1000.0, "px")]),
        tr("flare", "Flare", LIGHTS, m, vec![colour("color", "Color", [1.0, 0.8, 0.55, 1.0]), amount(80.0), angle("angle", "Angle", 20.0)]),
        tr("flash", "Flash", LIGHTS, m, vec![colour("color", "Color", [1.0, 1.0, 1.0, 1.0]), num("intensity", "Intensity", 100.0, 100.0, "%")]),
        tr("glow", "Glow", LIGHTS, m, vec![num("radius", "Radius", 40.0, 400.0, "px"), num("intensity", "Intensity", 80.0, 100.0, "%")]),
        tr("lens_blur", "Lens Blur", LIGHTS, m, vec![num("radius", "Iris Radius", 30.0, 200.0, "px")]),
        tr("light_leak", "Light Leak", LIGHTS, m, vec![colour("color", "Color", [1.0, 0.55, 0.2, 1.0]), amount(80.0), seed()]),
        tr(
            "light_sweep",
            "Light Sweep",
            LIGHTS,
            m,
            vec![angle("angle", "Angle", 70.0), num("width", "Width", 160.0, 1000.0, "px"), colour("color", "Color", [1.0, 0.97, 0.9, 1.0]), amount(80.0)],
        ),
        tr("phosphor", "Phosphor", LIGHTS, m, vec![colour("color", "Phosphor Color", [0.55, 1.0, 0.6, 1.0]), num("decay", "Decay", 50.0, 100.0, "%")]),
        tr("radial_blur", "Radial Blur", LIGHTS, m, vec![centre(), range("angle", "Blur Angle", 40.0, 0.0, 180.0, "°")]),
        tr("ray", "Ray", LIGHTS, m, vec![centre(), amount(80.0), num("length", "Ray Length", 50.0, 100.0, "%")]),
        tr("solarize", "Solarize", LIGHTS, m, vec![amount(100.0)]),
        tr(
            "stripe",
            "Stripe",
            LIGHTS,
            m,
            vec![range("stripes", "Stripes", 8.0, 1.0, 64.0, ""), angle("angle", "Angle", 30.0), colour("color", "Color", [1.0, 1.0, 1.0, 1.0])],
        ),
        tr("zoom_blur", "Zoom Blur", LIGHTS, m, vec![centre(), num("strength", "Strength", 50.0, 100.0, "%")]),
        // ---- Slide ----
        tr("roll_3d", "3D Roll", SLIDE, m, vec![direction(), num("perspective", "Perspective", 50.0, 100.0, "%")]),
        tr(
            "film_roll",
            "Film Roll",
            SLIDE,
            m,
            vec![direction(), num("gap", "Gap", 40.0, 400.0, "px"), colour("gap_color", "Gap Color", [0.03, 0.03, 0.03, 1.0]), motion_blur()],
        ),
        tr("push", "Push", SLIDE, m, vec![direction(), motion_blur()]),
        tr("roll", "Roll", SLIDE, m, vec![direction(), motion_blur()]),
        tr("slide", "Slide", SLIDE, m, vec![direction(), motion_blur()]),
        tr("split", "Split", SLIDE, m, vec![ch("orientation", "Orientation", AXIS_OPTS, 0), motion_blur()]),
        tr("stretch", "Stretch", SLIDE, m, vec![direction()]),
        tr("whip", "Whip", SLIDE, m, vec![direction(), num("blur", "Blur", 80.0, 100.0, "%")]),
        // ---- Smart Tools ----
        tr("motion_camera", "Motion Camera", SMART, m, vec![range("zoom", "Zoom", 40.0, 0.0, 200.0, "%"), angle("rotation", "Rotation", 8.0), motion_blur()]),
        tr("motion_tween", "Motion Tween", SMART, m, vec![range("scale", "Scale Match", 15.0, 0.0, 100.0, "%")]),
        tr(
            "shape_dissolve",
            "Shape Dissolve",
            SMART,
            m,
            vec![
                ch("shape", "Shape", SHAPE_OPTS, 0),
                num("size", "Shape Size", 120.0, 1000.0, "px"),
                num("softness", "Softness", 2.0, 100.0, "px"),
                angle("rotation", "Rotation", 0.0),
                ch("order", "Order", ORDER_OPTS, 0),
                seed(),
            ],
        ),
        tr("shape_flow", "Shape Flow", SMART, m, vec![ch("shape", "Shape", SHAPE_OPTS, 0), direction(), num("softness", "Softness", 4.0, 200.0, "px")]),
        // ---- Text ----
        tr("text_animator", "Text Animator", TEXT, m, vec![range("columns", "Columns", 24.0, 2.0, 128.0, ""), range("rows", "Rows", 10.0, 1.0, 64.0, "")]),
        tr(
            "typewriter",
            "Typewriter",
            TEXT,
            m,
            vec![range("columns", "Columns", 32.0, 2.0, 128.0, ""), range("rows", "Rows", 12.0, 1.0, 64.0, ""), b("cursor", "Cursor", true)],
        ),
        // ---- Transformers ----
        tr("spin_3d", "3D Spin", TRANSFORMERS, m, vec![direction(), num("perspective", "Perspective", 50.0, 100.0, "%")]),
        tr("spinback_3d", "3D Spinback", TRANSFORMERS, m, vec![direction(), num("depth", "Depth", 60.0, 100.0, "%")]),
        tr(
            "frame",
            "Frame",
            TRANSFORMERS,
            m,
            vec![
                num("frame_width", "Frame Width", 24.0, 200.0, "px"),
                colour("frame_color", "Frame Color", [1.0, 1.0, 1.0, 1.0]),
                range("scale", "Scale", 60.0, 10.0, 95.0, "%"),
            ],
        ),
        tr("louver", "Louver", TRANSFORMERS, m, vec![range("slats", "Slats", 8.0, 1.0, 64.0, ""), ch("orientation", "Orientation", AXIS_OPTS, 0)]),
        tr("mirror_transition", "Mirror", TRANSFORMERS, m, vec![direction()]),
        tr(
            "page_peel",
            "Page Peel",
            TRANSFORMERS,
            m,
            vec![ch("corner", "Corner", CORNER_OPTS, 2), num("radius", "Curl Radius", 60.0, 500.0, "px"), b("shadow", "Shadow", true)],
        ),
        tr("slice", "Slice", TRANSFORMERS, m, vec![range("slices", "Slices", 8.0, 1.0, 64.0, ""), angle("angle", "Angle", 20.0), motion_blur()]),
        tr(
            "wave",
            "Wave",
            TRANSFORMERS,
            m,
            vec![direction(), num("amplitude", "Amplitude", 60.0, 500.0, "px"), num("wavelength", "Wavelength", 300.0, 3000.0, "px")],
        ),
        // ---- Wipe ----
        tr("clock_wipe", "Clock Wipe", WIPE, m, with(vec![centre(), angle("start", "Start Angle", 0.0), feather(0.0)], border())),
        tr("linear_wipe", "Linear Wipe", WIPE, m, with(vec![angle("angle", "Wipe Angle", 90.0), feather(0.0)], border())),
        tr(
            "neon_wipe",
            "Neon Wipe",
            WIPE,
            m,
            vec![
                angle("angle", "Wipe Angle", 90.0),
                colour("color", "Neon Color", [0.2, 0.9, 1.0, 1.0]),
                num("glow", "Glow Width", 40.0, 400.0, "px"),
                amount(100.0),
            ],
        ),
        tr("panel_wipe", "Panel Wipe", WIPE, m, with(vec![direction(), range("panels", "Panels", 6.0, 1.0, 64.0, ""), feather(0.0)], border())),
        tr(
            "plateau_wipe",
            "Plateau Wipe",
            WIPE,
            m,
            with(vec![angle("angle", "Wipe Angle", 90.0), range("plateaus", "Plateaus", 5.0, 1.0, 64.0, ""), feather(4.0)], border()),
        ),
        tr("radial_wipe", "Radial Wipe", WIPE, m, with(vec![ch("corner", "Corner", CORNER_OPTS, 0), feather(0.0)], border())),
        tr("soft_wipe", "Soft Wipe", WIPE, m, vec![angle("angle", "Wipe Angle", 90.0), feather(300.0)]),
        tr(
            "star_wipe",
            "Star Wipe",
            WIPE,
            m,
            with(
                vec![
                    centre(),
                    range("points", "Points", 5.0, 3.0, 24.0, ""),
                    range("inner", "Inner Radius", 45.0, 5.0, 100.0, "%"),
                    angle("rotation", "Rotation", 0.0),
                    feather(0.0),
                ],
                border(),
            ),
        ),
        tr("stretch_wipe", "Stretch Wipe", WIPE, m, vec![direction(), num("stretch", "Stretch", 200.0, 2000.0, "px")]),
        // ---- Legacy ----
        tr("additive_dissolve_legacy", "Additive Dissolve (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("barn_doors", "Barn Doors", LEGACY_VIDEO_TRANSITIONS, A, with(vec![ch("orientation", "Orientation", AXIS_OPTS, 1)], border())),
        tr("center_split", "Center Split", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("clock_wipe_legacy", "Clock Wipe (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("cross_dissolve_legacy", "Cross Dissolve (Legacy)", LEGACY_VIDEO_TRANSITIONS, A32Yuv, vec![]),
        tr("cross_zoom_legacy", "Cross Zoom (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("dip_to_black_legacy", "Dip to Black (Legacy)", LEGACY_VIDEO_TRANSITIONS, A32Yuv, vec![]),
        tr("dip_to_white_legacy", "Dip to White (Legacy)", LEGACY_VIDEO_TRANSITIONS, A32Yuv, vec![]),
        tr("film_dissolve_legacy", "Film Dissolve (Legacy)", LEGACY_VIDEO_TRANSITIONS, A32Yuv, vec![]),
        tr("inset", "Inset", LEGACY_VIDEO_TRANSITIONS, A, with(vec![ch("corner", "Corner", CORNER_OPTS, 0)], border())),
        tr("iris_box", "Iris Box", LEGACY_VIDEO_TRANSITIONS, AYuv, with(vec![centre()], border())),
        tr("iris_cross", "Iris Cross", LEGACY_VIDEO_TRANSITIONS, AYuv, with(vec![centre()], border())),
        tr("iris_diamond", "Iris Diamond", LEGACY_VIDEO_TRANSITIONS, AYuv, with(vec![centre()], border())),
        tr("iris_round", "Iris Round", LEGACY_VIDEO_TRANSITIONS, AYuv, with(vec![centre()], border())),
        tr("non_additive_dissolve", "Non-Additive Dissolve", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("push_legacy", "Push (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![direction()]),
        tr("radial_wipe_legacy", "Radial Wipe (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("slide_legacy", "Slide (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![direction()]),
        tr("split_legacy", "Split (Legacy)", LEGACY_VIDEO_TRANSITIONS, A, vec![]),
        tr("whip_legacy", "Whip (Legacy)", LEGACY_VIDEO_TRANSITIONS, A32Yuv, vec![direction()]),
        tr("wipe", "Wipe", LEGACY_VIDEO_TRANSITIONS, AYuv, vec![direction()]),
        // ---- Obsolete (hidden; still render in old projects) ----
        tr("band_slide", "Band Slide", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
        tr("checker_wipe", "Checker Wipe", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
        tr("cube_spin", "Cube Spin", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
        tr("flip_over", "Flip Over", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
        tr("gradient_wipe", "Gradient Wipe", OBSOLETE_VIDEO_TRANSITIONS, A, vec![fixed(crate::effect::f("softness", "Softness", 10.0, 0.0, 100.0, ""))]),
        tr("page_turn", "Page Turn", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
        tr("venetian_blinds", "Venetian Blinds", OBSOLETE_VIDEO_TRANSITIONS, A, vec![]),
    ]
}

/// Case-insensitive lookup of a transition of `kind` by id or display name (names such as
/// "Mosaic" or "Mirror" also name video effects, so plain name lookup is ambiguous).
pub fn find_transition(name: &str, kind: EffectKind) -> Option<&'static EffectDef> {
    let n = name.trim().to_ascii_lowercase();
    crate::effect::effect_defs().iter().filter(|d| d.kind == kind).find(|d| d.id == n || d.name.to_ascii_lowercase() == n)
}

/// The Effects panel folder tree for video transitions: `(folder path, transition ids)` in panel
/// order (sub-folders as [`VIDEO_TRANSITION_FOLDERS`], then `Legacy/Video Transitions`); items are
/// sorted by name as in the panel.
pub fn video_transition_tree() -> Vec<(String, Vec<&'static str>)> {
    let defs = crate::effect::effect_defs();
    let folder = |cat: &[&str]| -> Vec<&'static str> {
        let mut v: Vec<&EffectDef> = defs.iter().filter(|d| d.kind == EffectKind::VideoTransition && d.category == cat).collect();
        v.sort_by_key(|d| d.name.to_ascii_lowercase());
        v.into_iter().map(|d| d.id).collect()
    };
    let mut out: Vec<(String, Vec<&'static str>)> =
        VIDEO_TRANSITION_FOLDERS.iter().map(|f| (format!("Video Transitions/{f}"), folder(&["Video Transitions", f]))).collect();
    out.push(("Legacy/Video Transitions".into(), folder(LEGACY_VIDEO_TRANSITIONS)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premiere_26_transition_tree() {
        let tree = video_transition_tree();
        let counts: Vec<usize> = tree.iter().map(|(_, v)| v.len()).collect();
        assert_eq!(counts, vec![8, 11, 10, 8, 16, 8, 4, 2, 8, 9, 21], "{tree:?}");
        assert_eq!(counts[..10].iter().sum::<usize>(), 84);
        // every transition is in a known folder, ids and names are unique per kind
        let mut names = std::collections::HashSet::new();
        for d in crate::effect::effect_defs().iter().filter(|d| d.kind == EffectKind::VideoTransition) {
            assert!(names.insert(d.name), "duplicate transition name {}", d.name);
            let ok = (d.category.len() == 2 && d.category[0] == "Video Transitions" && VIDEO_TRANSITION_FOLDERS.contains(&d.category[1]))
                || d.category == LEGACY_VIDEO_TRANSITIONS
                || d.category == OBSOLETE_VIDEO_TRANSITIONS;
            assert!(ok, "{} in {:?}", d.id, d.category);
            assert!(d.params.iter().all(|p| !p.animatable), "{}: transition params are not animatable", d.id);
        }
        // spot checks: names in the right folders
        let at = |id: &str| crate::effect::find_effect(id).unwrap().category;
        assert_eq!(at("cross_dissolve"), &["Video Transitions", "Dissolve"]);
        assert_eq!(at("page_peel"), &["Video Transitions", "Transformers"]);
        assert_eq!(at("cross_zoom"), &["Video Transitions", "Lights & Blurs"]);
        assert_eq!(at("shape_dissolve"), &["Video Transitions", "Smart Tools"]);
        assert_eq!(at("iris_round"), LEGACY_VIDEO_TRANSITIONS);
        assert_eq!(at("venetian_blinds"), OBSOLETE_VIDEO_TRANSITIONS);
    }

    #[test]
    fn transition_lookup_by_kind() {
        assert_eq!(find_transition("Mosaic", EffectKind::VideoTransition).unwrap().id, "mosaic_transition");
        assert_eq!(crate::effect::find_effect_by_name("Mosaic").unwrap().kind, EffectKind::Video);
        assert_eq!(find_transition("push (legacy)", EffectKind::VideoTransition).unwrap().id, "push_legacy");
        assert_eq!(find_transition("3D Spinback", EffectKind::VideoTransition).unwrap().id, "spinback_3d");
        assert!(find_transition("Constant Power", EffectKind::VideoTransition).is_none());
    }
}
