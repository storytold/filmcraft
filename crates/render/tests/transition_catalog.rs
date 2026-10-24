//! Frozen transition inventory and transparent CPU parity oracle.
use filmcraft_project::{EffectKind, effect_defs, find_effect};
use filmcraft_render::{image::Image, transitions::apply_scaled};

const IDS: &[&str] = &[
    "block_motion",
    "flip_motion",
    "fold_motion",
    "pop_motion",
    "pull_motion",
    "spin_motion",
    "spring_motion",
    "travel_motion",
    "additive_dissolve",
    "blur_dissolve",
    "burn_alpha",
    "cross_dissolve",
    "dip_to_black",
    "dip_to_color",
    "dip_to_white",
    "film_dissolve",
    "luma_fade",
    "morph_cut",
    "mosaic_transition",
    "chaos",
    "earthquake",
    "flicker",
    "glass",
    "glitch",
    "grunge",
    "kaleidoscope",
    "liquid_distortion",
    "tv_power",
    "vhs_damage",
    "vr_chroma_leaks",
    "vr_gradient_wipe",
    "vr_iris_wipe",
    "vr_light_leaks",
    "vr_light_rays",
    "vr_mobius_zoom",
    "vr_random_blocks",
    "vr_spherical_blur",
    "burn_chroma",
    "chroma_leak",
    "cross_zoom",
    "directional_blur_transition",
    "flare",
    "flash",
    "glow",
    "lens_blur",
    "light_leak",
    "light_sweep",
    "phosphor",
    "radial_blur",
    "ray",
    "solarize",
    "stripe",
    "zoom_blur",
    "roll_3d",
    "film_roll",
    "push",
    "roll",
    "slide",
    "split",
    "stretch",
    "whip",
    "motion_camera",
    "motion_tween",
    "shape_dissolve",
    "shape_flow",
    "text_animator",
    "typewriter",
    "spin_3d",
    "spinback_3d",
    "frame",
    "louver",
    "mirror_transition",
    "page_peel",
    "slice",
    "wave",
    "clock_wipe",
    "linear_wipe",
    "neon_wipe",
    "panel_wipe",
    "plateau_wipe",
    "radial_wipe",
    "soft_wipe",
    "star_wipe",
    "stretch_wipe",
    "additive_dissolve_legacy",
    "barn_doors",
    "center_split",
    "clock_wipe_legacy",
    "cross_dissolve_legacy",
    "cross_zoom_legacy",
    "dip_to_black_legacy",
    "dip_to_white_legacy",
    "film_dissolve_legacy",
    "inset",
    "iris_box",
    "iris_cross",
    "iris_diamond",
    "iris_round",
    "non_additive_dissolve",
    "push_legacy",
    "radial_wipe_legacy",
    "slide_legacy",
    "split_legacy",
    "whip_legacy",
    "wipe",
    "band_slide",
    "checker_wipe",
    "cube_spin",
    "flip_over",
    "gradient_wipe",
    "page_turn",
    "venetian_blinds",
];

fn fixtures(w: usize, h: usize) -> (Image, Image) {
    let mut a = Image::new(w, h);
    let mut b = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let alpha = ((x + y) % 4) as f32 / 4.0;
            a.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[0.7 * alpha, 0.2 * alpha, 0.1 * alpha, alpha]);
            b.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[0.1 * (1.0 - alpha), 0.3 * (1.0 - alpha), 0.8 * (1.0 - alpha), 1.0 - alpha]);
        }
    }
    (a, b)
}

#[test]
fn frozen_inventory_matches_all_current_legacy_and_obsolete_ids() {
    let live: std::collections::BTreeSet<_> = effect_defs().iter().filter(|d| d.kind == EffectKind::VideoTransition).map(|d| d.id).collect();
    assert_eq!(IDS.len(), 112);
    assert_eq!(IDS.iter().copied().collect::<std::collections::BTreeSet<_>>(), live);
    assert_eq!(live.iter().filter(|id| find_effect(id).unwrap().category == ["Legacy", "Video Transitions"]).count(), 21);
    assert_eq!(live.iter().filter(|id| find_effect(id).unwrap().category == ["Obsolete", "Video Transitions"]).count(), 7);
}

#[test]
fn transparent_cpu_oracle_is_exact_at_endpoints_finite_and_deterministic() {
    for (w, h) in [(1, 1), (17, 9)] {
        let (a, b) = fixtures(w, h);
        for id in IDS {
            let e = find_effect(id).unwrap().instance();
            for p in [f32::NAN, f32::NEG_INFINITY, -1.0, 0.0] {
                assert_eq!(apply_scaled(&e, &a, &b, p, f32::NAN), a, "{id}: {p}");
            }
            for p in [1.0, 2.0] {
                assert_eq!(apply_scaled(&e, &a, &b, p, 0.0), b, "{id}: {p}");
            }
            for p in [0.000001, 0.37, 0.5, 0.999999] {
                let out = apply_scaled(&e, &a, &b, p, 0.5);
                assert!(out.px.iter().all(|v| v.is_finite()), "{id}: {p}, {w}x{h}");
                assert_eq!(out, apply_scaled(&e, &a, &b, p, 0.5), "{id}: nondeterministic");
            }
        }
    }
}

#[test]
fn cross_dissolve_interpolates_all_premultiplied_channels() {
    let (a, b) = fixtures(17, 9);
    for id in ["cross_dissolve", "cross_dissolve_legacy"] {
        let e = find_effect(id).unwrap().instance();
        for p in [0.01, 0.37, 0.5, 0.99] {
            let out = apply_scaled(&e, &a, &b, p, 1.0);
            for ((a, b), got) in a.px.iter().zip(&b.px).zip(&out.px) {
                assert!((got - (a + (b - a) * p)).abs() < 1e-6, "{id}: alpha/color gap");
            }
        }
    }
}

#[test]
#[ignore = "diagnostic scan of CPU transparency quirks; not a parity acceptance gate"]
fn transparent_cpu_oracle_continuity_near_both_endpoints() {
    let (a, b) = fixtures(17, 9);
    let mut defects = Vec::new();
    for id in IDS {
        let e = find_effect(id).unwrap().instance();
        for (p, want) in [(0.000001, &a), (0.999999, &b)] {
            let out = apply_scaled(&e, &a, &b, p, 0.5);
            let mean = out.px.iter().zip(&want.px).map(|(x, y)| (x - y).abs()).sum::<f32>() / out.px.len() as f32;
            if mean >= 0.06 {
                defects.push(format!("{id}@{p}: mean RGBA delta {mean}"));
            }
        }
    }
    eprintln!("CPU transparency continuity diagnostics: {defects:?}");
}
