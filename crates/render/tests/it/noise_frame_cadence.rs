use filmcraft_project::{ParamValue, find_effect};
use filmcraft_render::{
    Image,
    effects::{FxCtx, apply},
};
use filmcraft_time::FrameRate;

fn render_noise(effect_id: &str, rate: FrameRate, frame: i64, amount: f64) -> Vec<f32> {
    let mut image = Image::filled(16, 16, [0.5, 0.5, 0.5, 1.0]);
    let mut effect = find_effect(effect_id).unwrap().instance();
    effect.param_mut("amount").unwrap().value = ParamValue::Float(amount);
    let tick = rate.tick_of(frame);
    let context = FxCtx {
        t: tick,
        px_scale: 1.0,
        seconds: tick.seconds(),
        timecode: "00:00:00:00",
        clip_name: "generated matte",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    apply(&mut image, &effect, &context).unwrap();
    image.px
}

#[test]
fn both_noise_variants_change_on_every_sequence_frame() {
    for effect_id in ["noise", "noise_legacy"] {
        for rate in [FrameRate::FPS_24, FrameRate::FPS_30, FrameRate::FPS_60, FrameRate::FPS_120] {
            for frame in 0..5 {
                let this = render_noise(effect_id, rate, frame, 100.0);
                let next = render_noise(effect_id, rate, frame + 1, 100.0);
                assert_ne!(this, next, "{effect_id} {rate:?} repeated frames {frame} and {}", frame + 1);
                assert_eq!(this, render_noise(effect_id, rate, frame, 100.0), "same timestamp must be deterministic");
            }
        }
        // Floating-point second-to-30Hz conversion also repeated frames 122/123 at 30 fps.
        assert_ne!(render_noise(effect_id, FrameRate::FPS_30, 122, 100.0), render_noise(effect_id, FrameRate::FPS_30, 123, 100.0));
        assert_eq!(render_noise(effect_id, FrameRate::FPS_60, 0, 0.0), [0.5, 0.5, 0.5, 1.0].repeat(16 * 16));
    }
}
