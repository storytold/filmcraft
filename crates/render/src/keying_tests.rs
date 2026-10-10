//! Regression checks against the original CPU keying functions, independent of the GPU op.
use super::*;
use filmcraft_project::{ParamValue, find_effect};
use filmcraft_time::Tick;

fn cx(t: Tick) -> FxCtx<'static> {
    FxCtx { t, px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project: None, env: None, working: filmcraft_color::WorkingSpace::Rec709 }
}

fn picture() -> Image {
    let mut px = Vec::new();
    for i in 0..512 {
        let a = [0.0, 1e-8, 0.25, 0.75, 1.0][i % 5];
        let c = [(i % 17) as f32 / 16.0, (i % 29) as f32 / 20.0, (i % 31) as f32 / 30.0];
        px.extend_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a]);
    }
    Image { w: 32, h: 16, px }
}

#[test]
fn keying_ops_preserve_original_cpu_results_and_animation() {
    for id in ["color_key", "ultra_key", "luma_key"] {
        for key in [[0.0, 0.8, 0.2, 1.0], [0.9, 0.1, 0.1, 1.0], [0.1, 0.1, 0.9, 1.0]] {
            for output in 0..3 {
                let mut e = find_effect(id).unwrap().instance();
                let (param, from, to) = if id == "luma_key" { ("threshold", 10.0, 90.0) } else { ("tolerance", 0.0, 100.0) };
                e.params.get_mut(param).unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(from));
                e.params.get_mut(param).unwrap().put_keyframe(Tick::from_seconds_f64(1.0), ParamValue::Float(to));
                if id != "luma_key" {
                    e.params.get_mut(if id == "ultra_key" { "key_color" } else { "color" }).unwrap().value = ParamValue::Color(key);
                }
                if id == "ultra_key" {
                    e.params.get_mut("output").unwrap().value = ParamValue::Choice(output);
                    e.params.get_mut("spill").unwrap().value = ParamValue::Float(100.0);
                }
                for seconds in [0.0, 0.25, 0.75, 1.0] {
                    let cx = cx(Tick::from_seconds_f64(seconds));
                    let mut reference = picture();
                    let mut actual = reference.clone();
                    if id == "ultra_key" {
                        assert!(crate::vfx::apply(&mut reference, &e, &cx));
                    } else if id == "luma_key" {
                        crate::effects::luma_key(&mut reference, &e, &cx);
                    } else {
                        crate::effects::key(&mut reference, &e, &cx);
                    }
                    let op = FxOp::eval(&e, &cx, actual.w, actual.h).unwrap();
                    assert!(op.gpu_ok());
                    op.apply(&mut actual);
                    assert_eq!(actual, reference, "{id}, {key:?}, output {output}, t={seconds}");
                }
            }
        }
    }
}

#[test]
fn reversed_and_equal_luma_thresholds_preserve_original_results() {
    for (threshold, cutoff) in [(20.0, 80.0), (50.0, 50.0), (0.0, 0.0), (100.0, 100.0)] {
        let mut e = find_effect("luma_key").unwrap().instance();
        e.params.get_mut("threshold").unwrap().value = ParamValue::Float(threshold);
        e.params.get_mut("cutoff").unwrap().value = ParamValue::Float(cutoff);
        let mut reference = picture();
        let mut actual = reference.clone();
        crate::effects::luma_key(&mut reference, &e, &cx(Tick::ZERO));
        FxOp::eval(&e, &cx(Tick::ZERO), actual.w, actual.h).unwrap().apply(&mut actual);
        assert_eq!(actual, reference);
    }
}

#[test]
fn ultra_key_cleanup_spill_and_correction_match_current_cpu() {
    for setting in 0..4 {
        for output in 0..3 {
            let mut e = find_effect("ultra_key").unwrap().instance();
            e.params.get_mut("setting").unwrap().value = ParamValue::Choice(setting);
            e.params.get_mut("output").unwrap().value = ParamValue::Choice(output);
            for (id, value) in [
                ("contrast", 40.0),
                ("mid_point", 35.0),
                ("cc_hue", -45.0),
                ("cc_saturation", 140.0),
                ("cc_luminance", 80.0),
                ("range", 75.0),
                ("desaturate", 80.0),
                ("spill_luma", 20.0),
            ] {
                e.params.get_mut(id).unwrap().value = ParamValue::Float(value);
            }
            let mut reference = picture();
            let mut actual = reference.clone();
            assert!(crate::vfx::apply(&mut reference, &e, &cx(Tick::ZERO)));
            FxOp::eval(&e, &cx(Tick::ZERO), actual.w, actual.h).unwrap().apply(&mut actual);
            assert_eq!(actual, reference, "setting={setting}, output={output}");
        }
    }
}

#[test]
fn invalid_key_channel_is_rejected_without_panicking() {
    let op = FxOp::ChromaKey { key_ycc: [0.0; 3], tolerance: 0.1, softness: 0.1, spill: 1.0, dominant: u32::MAX, output: 0 };
    assert!(!op.gpu_ok());
    let mut img = picture();
    let original = img.clone();
    op.apply(&mut img);
    assert_eq!(img, original);
}
