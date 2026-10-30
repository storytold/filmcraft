use super::*;
use proptest::prelude::*;

#[test]
fn empty_time_ranges_never_overlap() {
    let clip = TimeRange::new(Tick(0), Tick(10));
    for duration in [0, -1, -5] {
        let empty = TimeRange::new(Tick(5), Tick(duration));
        assert!(empty.is_empty());
        assert_eq!(empty.intersect(&clip), None);
        assert!(!empty.overlaps(&clip), "duration {duration}");
        assert!(!clip.overlaps(&empty), "duration {duration}");
    }
}

#[test]
fn time_range_overlap_respects_half_open_boundaries() {
    let clip = TimeRange::new(Tick(0), Tick(10));
    for (start, duration, expected) in [(-5, 5, false), (10, 5, false), (-5, 6, true), (9, 5, true), (2, 3, true)] {
        let other = TimeRange::new(Tick(start), Tick(duration));
        assert_eq!(clip.overlaps(&other), expected);
        assert_eq!(other.overlaps(&clip), expected);
    }
}

#[test]
fn seconds_display_has_no_negative_zero() {
    let f = |t: i64| format_time(Tick(t), FrameRate::default(), false, TimeDisplay::Seconds, 48000);
    assert_eq!(f(-1), "0.000");
    assert_eq!(f(-2), "0.000");
    assert_eq!(f(-(TICKS_PER_SECOND / 2500)), "0.000");
    assert_eq!(f(-TICKS_PER_SECOND), "-1.000");
    assert_eq!(f(-TICKS_PER_SECOND / 1000), "-0.001");
}

#[test]
fn audio_samples_negative_time_has_leading_minus() {
    let r = FrameRate::new(30000, 1001);
    let f = |t| format_time(Tick(t), r, false, TimeDisplay::AudioSamples, 48000);
    assert_eq!(f(-TICKS_PER_SECOND), "-00:00:01:00000");
    assert_eq!(f(-(TICKS_PER_SECOND + TICKS_PER_SECOND / 2)), "-00:00:01:24000");
}

/// Damaged files and projects can carry zero / negative rates and timebases, and users or
/// agents can type huge timecodes: these used to panic (division by zero, overflow,
/// crossed `clamp` bounds).
#[test]
fn hostile_rates_and_timecodes_never_panic() {
    let zero = FrameRate { num: 0, den: 0 };
    for r in [
        zero,
        FrameRate { num: 0, den: 1 },
        FrameRate { num: 25, den: 0 },
        FrameRate { num: -30, den: 1 },
        FrameRate::from_f64(0.0),
        FrameRate::from_f64(f64::NAN),
    ] {
        assert_eq!(r.tick_of(10), FrameRate::default().tick_of(10));
        assert_eq!(r.frame_at(Tick(TICKS_PER_SECOND)), FrameRate::default().frame_at(Tick(TICKS_PER_SECOND)));
        assert!(r.frame_duration() > Tick::ZERO);
        assert!(r.timecode_base() >= 1);
        let _ = r.snap_nearest(Tick(12345));
        let _ = format_time(Tick(TICKS_PER_SECOND), r, true, TimeDisplay::Timecode, 48_000);
    }
    assert_eq!(Tick::from_rational(100, 1, 0), Tick::ZERO);
    assert_eq!(Tick::from_units(100, 0), Tick::ZERO);
    assert_eq!(Tick(5).to_rational_floor(0, 1), 0);
    assert_eq!(Tick(5).to_rational_round(0, 1), 0);
    for t in [i64::MIN, -1, 0, 1, i64::MAX] {
        for (n, d) in [(i64::MIN, 1), (-1, 1000), (1, i64::MAX), (i64::MAX, i64::MIN), (1, 0)] {
            let _ = Tick(t).to_rational_round(n, d);
        }
    }
    assert_eq!(Tick(5).mul_ratio(1, 0), Tick::ZERO);
    assert_eq!(Tick(5).clamp(Tick(10), Tick(0)), Tick(0));
    let _ = format_timecode_frames(i64::MIN, FrameRate::FPS_29_97, true);
    for tc in ["99999999999:99:99:99", "9223372036854775807", "+9223372036854775807", "99999999999;59;59;29"] {
        let _ = parse_timecode(tc, FrameRate::FPS_29_97, true, i64::MAX);
        let _ = parse_timecode(tc, FrameRate::FPS_25, false, i64::MIN);
    }
}

#[test]
fn boundary_rates_saturate_without_wrapping_and_keep_representable_values_exact() {
    let slow = FrameRate { num: 1, den: i64::MAX };
    assert_eq!(slow.frame_duration(), Tick(i64::MAX));
    assert_eq!(slow.tick_of(i64::MAX), Tick(i64::MAX));
    assert_eq!(slow.tick_of(i64::MIN), Tick(i64::MIN));
    assert_eq!(slow.snap_nearest(Tick(i64::MAX)), Tick::ZERO, "compare the real boundary before saturating");
    assert_eq!(slow.snap_nearest(Tick(i64::MIN)), Tick::ZERO);
    let fast = FrameRate { num: i64::MAX, den: 1 };
    assert_eq!(fast.tick_of(i64::MAX), Tick(TICKS_PER_SECOND), "large factors can cancel exactly");
    assert_eq!(fast.tick_of(-i64::MAX), Tick(-TICKS_PER_SECOND));
    assert_eq!(fast.frame_at(Tick(TICKS_PER_SECOND)), i64::MAX);
    assert_eq!(fast.frame_at(Tick(i64::MAX)), i64::MAX);
    assert_eq!(fast.frame_at(Tick(i64::MIN)), i64::MIN);
    assert_eq!(fast.timecode_base(), i64::MAX);
    for t in [i64::MIN, -TICKS_PER_SECOND, -1, 0, 1, TICKS_PER_SECOND, i64::MAX] {
        assert_eq!(fast.snap(Tick(t)), Tick(t), "sub-tick frames include every integer tick");
        assert_eq!(fast.snap_nearest(Tick(t)), Tick(t));
    }
    let balanced = FrameRate { num: i64::MAX, den: i64::MAX };
    assert_eq!(balanced.frame_duration(), Tick(TICKS_PER_SECOND));
    assert_eq!(balanced.tick_of(2), Tick(2 * TICKS_PER_SECOND));
    assert_eq!(balanced.frame_at(Tick(2 * TICKS_PER_SECOND)), 2);
    assert_eq!(balanced.timecode_base(), 1);
    assert_eq!(FrameRate::new(i64::MIN, i64::MIN), FrameRate { num: -1, den: -1 });
    assert_eq!(FrameRate::new(i64::MIN, 0), FrameRate { num: -1, den: 0 });
    assert_eq!(FrameRate::new(i64::MIN, 1), FrameRate { num: i64::MIN, den: 1 });
}

#[test]
fn scalar_time_conversions_saturate_at_storage_limits() {
    assert_eq!(Tick::from_units(i64::MAX, 1), Tick(i64::MAX));
    assert_eq!(Tick::from_units(i64::MIN, 1), Tick(i64::MIN));
    assert_eq!(Tick(i64::MAX).to_units_floor(i64::MAX), i64::MAX);
    assert_eq!(Tick(i64::MIN).to_units_floor(i64::MAX), i64::MIN);
    assert_eq!(Tick::from_rational(i64::MAX, i64::MAX, 1), Tick(i64::MAX));
    assert_eq!(Tick::from_rational(i64::MIN, i64::MAX, 1), Tick(i64::MIN));
    assert_eq!(Tick::from_rational(i64::MIN, i64::MAX, -1), Tick(i64::MAX));
    assert_eq!(Tick::from_rational(i64::MAX, i64::MAX, 0), Tick::ZERO);
    assert_eq!(Tick(i64::MAX).to_rational_floor(1, i64::MAX), i64::MAX);
    assert_eq!(Tick(i64::MIN).to_rational_round(1, i64::MAX), i64::MIN);
    assert_eq!(Tick(i64::MAX).mul_ratio(i64::MAX, 1), Tick(i64::MAX));
    assert_eq!(Tick(i64::MIN).abs(), Tick(i64::MAX));
}

#[test]
fn drop_frame_fields_keep_cancelling_terms_wide_at_storage_limits() {
    for rate in [FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_119_88] {
        for frame in [0, 1, 999_999, i64::MAX - 1, i64::MAX] {
            let (negative, h, m, s, f) = frames_to_fields(frame, rate, true);
            assert!(!negative);
            assert_eq!(fields_to_frames(h, m, s, f, rate, true), frame, "{rate}: {frame}");
            assert_eq!(fields_to_frames(-h, -m, -s, -f, rate, true), -frame);
        }
        let (negative, h, m, s, f) = frames_to_fields(i64::MIN, rate, true);
        assert!(negative);
        assert_eq!(fields_to_frames(-h, -m, -s, -f, rate, true), i64::MIN);
    }
}

#[test]
fn tick_arithmetic_saturates_only_outside_storage_limits() {
    let values = [i64::MIN, i64::MIN + 1, Tick::MIN.0, -1, 0, 1, Tick::MAX.0, i64::MAX - 1, i64::MAX];
    for a in values {
        assert_eq!(-Tick(a), Tick(clamp_i64(-i128::from(a))));
        for b in values {
            let sum = Tick(clamp_i64(i128::from(a) + i128::from(b)));
            let difference = Tick(clamp_i64(i128::from(a) - i128::from(b)));
            assert_eq!(Tick(a) + Tick(b), sum);
            assert_eq!(Tick(a) - Tick(b), difference);
            let mut assigned = Tick(a);
            assigned += Tick(b);
            assert_eq!(assigned, sum);
            assigned = Tick(a);
            assigned -= Tick(b);
            assert_eq!(assigned, difference);
        }
    }
    assert_eq!(Tick::MAX + Tick(1), Tick(Tick::MAX.0 + 1), "project bounds are not storage bounds");
    assert_eq!(Tick::MIN - Tick(1), Tick(Tick::MIN.0 - 1));
    assert_eq!(TimeRange::new(Tick(i64::MAX), Tick(1)).end(), Tick(i64::MAX));
    assert_eq!(TimeRange::new(Tick(i64::MIN), Tick(-1)).end(), Tick(i64::MIN));
    assert_eq!(TimeRange::from_bounds(Tick(i64::MIN), Tick(i64::MAX)).duration, Tick(i64::MAX));
    assert_eq!(TimeRange::from_bounds(Tick(i64::MAX), Tick(i64::MIN)).duration, Tick(i64::MIN));
    let ordinary = TimeRange::from_bounds(Tick::MIN, Tick::MAX);
    assert_eq!(ordinary.end(), Tick::MAX);
}

#[test]
fn numeric_limits_are_total_across_frame_conversion_and_display() {
    let values = [i64::MIN, -i64::MAX, -1, 0, 1, TICKS_PER_SECOND, i64::MAX];
    for num in values {
        for den in values {
            let r = FrameRate { num, den };
            let _ = FrameRate::new(num, den);
            let _ = r.frame_duration();
            assert!(r.timecode_base() >= 1);
            for value in values {
                let tick = Tick(value);
                let _ = r.tick_of(value);
                let _ = r.frame_at(tick);
                let _ = r.snap(tick);
                let _ = r.snap_nearest(tick);
                for display in TimeDisplay::ALL {
                    let _ = format_time(tick, r, true, display, 48_000);
                }
                let (_, h, m, sec, f) = frames_to_fields(value, r, true);
                let _ = fields_to_frames(h, m, sec, f, r, true);
            }
        }
    }
}

#[test]
fn every_common_rate_is_exact() {
    for r in FrameRate::COMMON {
        let d = r.frame_duration();
        assert_eq!(d.0 as i128 * r.num as i128, TICKS_PER_SECOND as i128 * r.den as i128, "{r}");
    }
    for sr in SAMPLE_RATES.iter().chain(&[8000, 11025, 16000, 22050, 176_400]) {
        assert_eq!(TICKS_PER_SECOND % sr, 0, "{sr}");
    }
}

#[test]
fn drop_frame_known_values() {
    let r = FrameRate::FPS_29_97;
    assert_eq!(format_timecode_frames(0, r, true), "00;00;00;00");
    assert_eq!(format_timecode_frames(1799, r, true), "00;00;59;29");
    assert_eq!(format_timecode_frames(1800, r, true), "00;01;00;02");
    assert_eq!(format_timecode_frames(17982, r, true), "00;10;00;00");
    assert_eq!(format_timecode_frames(107892, r, true), "01;00;00;00");
    assert_eq!(format_timecode_frames(1800, r, false), "00:01:00:00");
    let r60 = FrameRate::FPS_59_94;
    assert_eq!(format_timecode_frames(3600, r60, true), "00;01;00;04");
}

#[test]
fn parse_forms() {
    let r = FrameRate::FPS_25;
    assert_eq!(parse_timecode("00:00:10:00", r, false, 0).unwrap(), 250);
    assert_eq!(parse_timecode("1000", r, false, 0).unwrap(), 250);
    assert_eq!(parse_timecode("1.00", r, false, 0).unwrap(), 25);
    assert_eq!(parse_timecode("+15", r, false, 100).unwrap(), 115);
    assert_eq!(parse_timecode("-1.00", r, false, 100).unwrap(), 75);
    assert_eq!(parse_timecode("00;01;00;02", FrameRate::FPS_29_97, true, 0).unwrap(), 1800);
    assert!(parse_timecode("abc", r, false, 0).is_err());
}

#[test]
fn signed_field_rejected() {
    let r = FrameRate::FPS_25;
    assert!(parse_timecode("00:00:-5:00", r, false, 0).is_err());
    assert!(parse_timecode("00:+1:00:00", r, false, 0).is_err());
    assert_eq!(parse_timecode("00:00:10:00", r, false, 0).unwrap(), 250);
}

#[test]
fn display_modes() {
    let r = FrameRate::FPS_24;
    let t = r.tick_of(40);
    assert_eq!(format_time(t, r, false, TimeDisplay::Feet35, 48000), "2+08");
    assert_eq!(format_time(t, r, false, TimeDisplay::Feet16, 48000), "1+00");
    assert_eq!(format_time(t, r, false, TimeDisplay::Frames, 48000), "40");
    assert_eq!(format_time(Tick::from_units(48_001, 48_000), r, false, TimeDisplay::AudioSamples, 48000), "00:00:01:00001");
}

#[test]
fn rational_conversions() {
    // 90 kHz MPEG timebase
    let t = Tick::from_rational(90_000, 1, 90_000);
    assert_eq!(t.0, TICKS_PER_SECOND);
    assert_eq!(t.to_rational_floor(1, 90_000), 90_000);
    assert_eq!(t.to_rational_round(1, 90_000), 90_000);
    // 1 ms timebase (Matroska): to the nearest millisecond, halves up
    let ms = |t: Tick| t.to_rational_round(1, 1000);
    assert_eq!(ms(FrameRate::FPS_30.tick_of(1)), 33);
    assert_eq!(ms(FrameRate::FPS_30.tick_of(2)), 67);
    assert_eq!(FrameRate::FPS_30.tick_of(2).to_rational_floor(1, 1000), 66);
    assert_eq!(ms(FrameRate::FPS_29_97.tick_of(15)), 501, "500.5 ms");
    assert_eq!(ms(Tick::from_rational(-4, 1, 10_000)), 0, "-0.4 ms");
    assert_eq!(ms(Tick::from_rational(-5, 1, 10_000)), 0, "-0.5 ms");
    assert_eq!(ms(Tick::from_rational(-6, 1, 10_000)), -1, "-0.6 ms");
    // a negative timebase is nonsense, but rounds the same way
    assert_eq!(FrameRate::FPS_30.tick_of(2).to_rational_round(-1, -1000), 67);
    assert_eq!(FrameRate::FPS_29_97.tick_of(15).to_rational_round(-1, -1000), 501);
    assert_eq!(FrameRate::from_f64(29.97), FrameRate::FPS_29_97);
    assert_eq!(FrameRate::FPS_23_976.label(), "23.976");
}

#[test]
fn exotic_frame_rate_roundtrip() {
    // Issue #301: 37.516 fps footage.  The frame duration is
    // 254_016_000_000 * 250 / 9379 = 6_770_871_094.999… ticks — not an
    // integer.  Floor division in `tick_of` truncated to 6_770_871_094,
    // losing ~1 tick/frame so `frame_at(tick_of(f))` returned `f - 1`.
    // Ceiling division makes the round-trip exact for all integer frames.
    let rates: &[(FrameRate, &str)] = &[
        (FrameRate::from_f64(37.516), "37.516"),
        (FrameRate::from_f64(37.5161), "37.5161"),
        (FrameRate::from_f64(12.345), "12.345"),
        (FrameRate::from_f64(59.9401), "59.9401"),
        (FrameRate::from_f64(23.9761), "23.9761"),
    ];
    for &(r, name) in rates {
        // Round-trip over a wide range including negatives.
        let bad: Vec<i64> = (-20_000..20_000).filter(|&f| r.frame_at(r.tick_of(f)) != f).collect();
        assert!(bad.is_empty(), "{name}: {} round-trip failures, first {bad:?}", r);

        // snap(tick_of(f)) == tick_of(f): snapping a frame boundary is a no-op.
        for f in -100i64..100 {
            assert_eq!(r.snap(r.tick_of(f)), r.tick_of(f), "{name}: snap(tick_of({f}))");
        }

        // Step-back: the last tick of frame f is one tick before tick_of(f+1),
        // and frame_at of that is still f.  Also check the frame_duration path:
        // frame_at(tick_of(f) - frame_duration()) == f - 1.
        for f in 1..2000i64 {
            let last_tick = r.tick_of(f + 1) - Tick(1);
            assert_eq!(r.frame_at(last_tick), f, "{name}: last tick of frame {f}");
            let prev = r.tick_of(f) - r.frame_duration();
            assert_eq!(r.frame_at(prev), f - 1, "{name}: tick_of({f}) - frame_duration");
        }
    }

    // Stepping simulation: frame_at(playhead) + 1, tick_of, snap.
    // Before the fix this stalled at frame 0 (step forward did nothing).
    let r = FrameRate::from_f64(37.516);
    let mut tick = Tick::ZERO;
    for expected in 0..50 {
        let frame = r.frame_at(tick);
        assert_eq!(frame, expected, "stepping forward at frame {expected}");
        tick = r.tick_of(frame + 1);
        tick = r.snap(tick); // set_playhead calls snap
    }

    // Stepping backward from frame 50 should land on 49, 48, … 0
    tick = r.tick_of(50);
    let mut last = 50;
    while last > 0 {
        let frame = r.frame_at(tick);
        let target = frame - 1;
        if target < 0 {
            break;
        }
        tick = r.snap(r.tick_of(target));
        let landed = r.frame_at(tick);
        assert_eq!(landed, target, "stepping backward to frame {target}");
        last = landed;
    }
}

#[test]
fn exotic_frame_rate_durations() {
    // frame_duration uses floor (not round) so that `end - frame_duration()`
    // lands on the previous frame, not two frames back, for exotic rates.
    let r = FrameRate::from_f64(37.516);
    // 254_016_000_000 * 250 / 9379 = 6_770_871_094.999… → floor = 6_770_871_094
    assert_eq!(r.frame_duration().0, 6_770_871_094);

    // Standard rates must remain exact (unchanged).
    assert_eq!(FrameRate::FPS_23_976.frame_duration().0, 10_594_584_000);
    assert_eq!(FrameRate::FPS_24.frame_duration().0, 10_584_000_000);
    assert_eq!(FrameRate::FPS_25.frame_duration().0, 10_160_640_000);
    assert_eq!(FrameRate::FPS_29_97.frame_duration().0, 8_475_667_200);
    assert_eq!(FrameRate::FPS_30.frame_duration().0, 8_467_200_000);
    assert_eq!(FrameRate::FPS_60.frame_duration().0, 4_233_600_000);
}

proptest! {
    #[test]
    fn extreme_frame_math_stays_monotonic(num in 1i64..=i64::MAX, den in 1i64..=i64::MAX, a in any::<i64>(), b in any::<i64>()) {
        let r = FrameRate { num, den };
        let (lo, hi) = (a.min(b), a.max(b));
        prop_assert!(r.tick_of(lo) <= r.tick_of(hi));
        prop_assert!(r.frame_at(Tick(lo)) <= r.frame_at(Tick(hi)));
        prop_assert!(r.snap(Tick(lo)) <= r.snap(Tick(hi)));
        prop_assert!(r.snap_nearest(Tick(lo)) <= r.snap_nearest(Tick(hi)));
        let snapped = r.snap_nearest(Tick(a));
        prop_assert_eq!(r.snap_nearest(snapped), snapped);
    }

    #[test]
    fn frame_tick_roundtrip(f in -1_000_000i64..10_000_000, ri in 0usize..11) {
        let r = FrameRate::COMMON[ri];
        prop_assert_eq!(r.frame_at(r.tick_of(f)), f);
        prop_assert_eq!(r.frame_at(r.tick_of(f) + r.frame_duration() - Tick(1)), f);
    }

    #[test]
    fn exotic_frame_tick_roundtrip(f in -100_000i64..100_000, num in 1u64..200_000, den in 1u64..2_000) {
        // Skip rates that divide evenly (those are covered by COMMON above).
        let r = FrameRate::new(num as i64, den as i64);
        if (TICKS_PER_SECOND as i128 * r.den as i128) % r.num as i128 == 0 {
            return Ok(());
        }
        prop_assert_eq!(r.frame_at(r.tick_of(f)), f, "rate={}/{}, f={}", r.num, r.den, f);
    }

    #[test]
    fn drop_frame_bijection(f in 0i64..(24 * 107_892)) {
        let r = FrameRate::FPS_29_97;
        let (_, h, m, s, fr) = frames_to_fields(f, r, true);
        // dropped labels never appear
        prop_assert!(!(s == 0 && fr < 2 && m % 10 != 0));
        prop_assert_eq!(fields_to_frames(h, m, s, fr, r, true), f);
        let txt = format_timecode_frames(f, r, true);
        prop_assert_eq!(parse_timecode(&txt, r, true, 0).unwrap(), f);
    }

    #[test]
    fn ndf_parse_roundtrip(f in 0i64..10_000_000, ri in 0usize..11) {
        let r = FrameRate::COMMON[ri];
        let txt = format_timecode_frames(f, r, false);
        prop_assert_eq!(parse_timecode(&txt, r, false, 0).unwrap(), f);
    }
}
