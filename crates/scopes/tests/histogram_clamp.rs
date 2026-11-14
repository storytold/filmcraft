use filmcraft_scopes::{Params, Signal, histogram};

#[test]
fn histogram_y_uses_clamped_rgb_when_clamp_is_on() {
    let s = Signal::from_fn(1, 1, |_, _| [2.0, 0.0, 0.0]);
    let h = histogram(&s, &Params::default());
    // Clamped to (1,0,0): Y' = 0.2126 -> bin 54.
    assert_eq!(h.y[54], 1);
    assert_eq!(h.y.iter().sum::<u32>(), 1);
}

#[test]
fn histogram_over_range_channel_is_counted_above_when_clamp_is_off() {
    let s = Signal::from_fn(1, 1, |_, _| [2.0, 0.0, 0.0]);
    let h = histogram(&s, &Params { clamp: false, ..Params::default() });
    assert_eq!(h.above[0], 1);
    assert_eq!(h.r.iter().sum::<u32>(), 0);
}
