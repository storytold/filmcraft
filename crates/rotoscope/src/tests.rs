use super::*;

fn input() -> Input<'static> {
    Input {
        width: 1,
        height: 1,
        end: Tick(2),
        frames: vec![Frame { time: Tick(1), rgba: &[0, 0, 0, 255] }],
        points: vec![Point { frame: 0, x: 0.0, y: 0.0, kind: Kind::Include }],
        quality: false,
        band: 8,
        base_alpha: None,
    }
}

#[test]
fn hostile_frame_point_and_budget_inputs_return_errors() {
    assert_eq!(input().validate().unwrap(), 1);
    for case in 0..14 {
        let outcome = std::panic::catch_unwind(|| {
            let mut input = input();
            match case {
                0 => input.width = usize::MAX,
                1 => input.height = 0,
                2 => input.width = 721,
                3 => input.frames.clear(),
                4 => input.band = 0,
                5 => input.band = usize::MAX,
                6 => input.points[0].x = f32::NAN,
                7 => input.points[0].y = -1.0,
                8 => input.points[0].frame = usize::MAX,
                9 => input.points[0].kind = Kind::Exclude,
                10 => input.frames[0].rgba = &[1],
                11 => input.end = Tick(1),
                12 => input.frames[0].time = Tick(-1),
                _ => input.frames.push(Frame { time: Tick(1), rgba: &[0, 0, 0, 255] }),
            }
            input.validate()
        });
        assert!(outcome.is_ok(), "case {case} panicked");
        assert!(outcome.unwrap().is_err(), "case {case} was accepted");
    }
}

#[test]
fn existing_alpha_accepts_only_one_matching_quality_frame() {
    let mut input = input();
    input.points.clear();
    input.quality = true;
    input.base_alpha = Some(&[32768]);
    assert_eq!(input.validate().unwrap(), 1);
    input.quality = false;
    assert!(input.validate().is_err());
    input.quality = true;
    input.base_alpha = Some(&[]);
    assert!(input.validate().is_err());
}

#[test]
fn trimap_matches_square_min_max_with_zero_padding() {
    let mask = [
        false, false, false, false, false, false, true, true, true, false, false, true, true, true, false, false, true, true, true, false, false, false, false,
        false, false,
    ];
    let trimap = pixels::trimap(&mask, 5, 5, 1).unwrap();
    assert_eq!(trimap[12], 255, "definite foreground stays locked");
    assert!(trimap.iter().enumerate().all(|(index, value)| index == 12 || *value == 128));
    assert_eq!(pixels::trimap(&[false; 4], 2, 2, 1).unwrap(), [0; 4]);
    assert_eq!(pixels::trimap(&[true; 4], 2, 2, 1).unwrap(), [128; 4], "out-of-image pixels are background");
}

#[test]
fn resampling_rejects_bad_sizes_and_preserves_constant_planes() {
    assert!(pixels::bilinear(&[], 0, 1, 2, 2).is_err());
    assert!(pixels::bilinear(&[1.0], usize::MAX, usize::MAX, 2, 2).is_err());
    assert_eq!(pixels::bilinear(&[3.0; 6], 3, 2, 7, 9).unwrap(), [3.0; 63]);
    let base = [0.7; 197 * 384];
    for (height, width) in [(1, 1), (14, 14), (28, 32), (32, 26)] {
        let positions = pixels::absolute_position(&base, height, width).unwrap();
        assert!(positions.iter().all(|p| (*p - 0.7).abs() < 1e-5));
    }
    assert!(pixels::absolute_position(&base, usize::MAX, 1).is_err());
    assert!(pixels::trimap(&[true], usize::MAX, 2, 8).is_err());
}
