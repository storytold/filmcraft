//! Original synthetic fixtures, AV1 v1.0.0 + Errata 1 sections 5.5/5.9.
use super::*;

#[derive(Clone)]
struct DecoderModel {
    buffer_delay_length_minus_1: u8,
    decoding_tick: u32,
    decoder_delay: u32,
    encoder_delay: u32,
    low_delay: bool,
}

#[derive(Clone)]
struct Timing {
    display_tick: u32,
    time_scale: u32,
    equal_ticks: Option<u8>,
    decoder: Option<DecoderModel>,
}

#[derive(Clone)]
struct Sequence {
    profile: u8,
    size: (u32, u32),
    depth: u8,
    mono: bool,
    color: (u8, u8, u8),
    color_present: bool,
    range: bool,
    location: u8,
    separate_uv: bool,
    reduced: bool,
    level: u8,
    tier: bool,
    initial_delay: Option<u8>,
    timing: Option<Timing>,
}
impl Default for Sequence {
    fn default() -> Self {
        Self {
            profile: 0,
            size: (64, 64),
            depth: 10,
            mono: false,
            color: (1, 1, 1),
            color_present: true,
            range: false,
            location: 0,
            separate_uv: false,
            reduced: false,
            level: 0,
            tier: false,
            initial_delay: None,
            timing: None,
        }
    }
}
#[derive(Default)]
struct Bits(Vec<bool>);
impl Bits {
    fn f(&mut self, value: u32, n: u32) {
        for shift in (0..n).rev() {
            self.0.push(value & (1 << shift) != 0);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        self.0.push(true); // trailing_bits()
        let mut out = vec![0; self.0.len().div_ceil(8)];
        for (i, bit) in self.0.iter().enumerate() {
            if *bit {
                out[i / 8] |= 1 << (7 - i % 8);
            }
        }
        out
    }
}
fn payload(s: &Sequence) -> Vec<u8> {
    let mut b = Bits::default();
    b.f(u32::from(s.profile), 3);
    b.f(u32::from(s.reduced), 1);
    b.f(u32::from(s.reduced), 1);
    if s.reduced {
        b.f(0, 5);
    } else {
        b.f(u32::from(s.timing.is_some()), 1);
        if let Some(timing) = &s.timing {
            b.f(timing.display_tick, 32);
            b.f(timing.time_scale, 32);
            b.f(u32::from(timing.equal_ticks.is_some()), 1);
            if let Some(ticks) = timing.equal_ticks {
                // uvlc(0) is 1 and uvlc(1) is 010; fixtures use only these values.
                if ticks == 0 {
                    b.f(1, 1);
                } else {
                    b.f(0, 1);
                    b.f(2, 2);
                }
            }
            b.f(u32::from(timing.decoder.is_some()), 1);
            if let Some(decoder) = &timing.decoder {
                b.f(u32::from(decoder.buffer_delay_length_minus_1), 5);
                b.f(decoder.decoding_tick, 32);
                b.f(0, 5); // buffer_removal_time_length_minus_1
                b.f(0, 5); // frame_presentation_time_length_minus_1
            }
        }
        b.f(u32::from(s.initial_delay.is_some()), 1);
        b.f(0, 5);
        b.f(0, 12);
        b.f(u32::from(s.level), 5);
        if s.level > 7 {
            b.f(u32::from(s.tier), 1);
        }
        if let Some(decoder) = s.timing.as_ref().and_then(|timing| timing.decoder.as_ref()) {
            b.f(1, 1); // decoder_model_present_for_this_op
            let n = u32::from(decoder.buffer_delay_length_minus_1) + 1;
            b.f(decoder.decoder_delay, n);
            b.f(decoder.encoder_delay, n);
            b.f(u32::from(decoder.low_delay), 1);
        }
        if let Some(delay) = s.initial_delay {
            b.f(1, 1);
            b.f(u32::from(delay), 4);
        }
    }
    b.f(7, 4);
    b.f(7, 4);
    b.f(s.size.0 - 1, 8);
    b.f(s.size.1 - 1, 8);
    if !s.reduced {
        b.f(0, 1);
    }
    b.f(0, 3); // superblock, filter intra, intra edge
    if !s.reduced {
        b.f(0, 7);
    } // five tools, choose screen tools, force screen tools
    b.f(0, 3); // superres, CDEF, restoration
    b.f(u32::from(s.depth > 8), 1);
    if s.profile == 2 && s.depth > 8 {
        b.f(u32::from(s.depth == 12), 1);
    }
    if s.profile != 1 {
        b.f(u32::from(s.mono), 1);
    }
    b.f(u32::from(s.color_present), 1);
    if s.color_present {
        b.f(u32::from(s.color.0), 8);
        b.f(u32::from(s.color.1), 8);
        b.f(u32::from(s.color.2), 8);
    }
    b.f(u32::from(s.range), 1);
    if !s.mono {
        if s.profile == 2 && s.depth == 12 {
            b.f(1, 1);
            b.f(1, 1);
        }
        if s.profile == 0 || (s.profile == 2 && s.depth == 12) {
            b.f(u32::from(s.location), 2);
        }
        b.f(u32::from(s.separate_uv), 1);
    }
    b.f(0, 1); // film grain
    b.finish()
}
fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(kind << 3) | 2];
    let mut n = payload.len();
    loop {
        let v = (n & 127) as u8;
        n >>= 7;
        out.push(v | if n > 0 { 128 } else { 0 });
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(payload);
    out
}
fn stream(s: &Sequence) -> FrameStreamInfo {
    let config = filmcraft_isobmff::Av1Config {
        seq_profile: s.profile,
        seq_level_idx_0: s.level,
        seq_tier_0: s.tier,
        high_bitdepth: s.depth > 8,
        twelve_bit: s.depth == 12,
        monochrome: s.mono,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        chroma_sample_position: s.location,
        initial_presentation_delay_minus_one: s.initial_delay,
        config_obus: obu(1, &payload(s)),
    };
    FrameStreamInfo::from_av1(&config, s.size).unwrap()
}
fn sample(s: &Sequence) -> Vec<u8> {
    let mut out = obu(1, &payload(s));
    out.extend(obu(6, if s.reduced { &[0x90, 0, 0] } else { &[0x19, 0, 0] }));
    out
}

#[test]
fn av1_color_candidate_allows_absent_color_description() {
    let original = Sequence::default();
    let info = stream(&original);
    let changed = Sequence { color_present: false, color: (2, 2, 2), ..original };
    let bytes = sample(&changed);
    assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::ColorDescription { before: (1, 1, 1), after: (2, 2, 2) });
    assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_some());
}

#[test]
fn av1_explicit_unspecified_color_matches_absent_description() {
    let original = Sequence { color_present: false, color: (2, 2, 2), ..Default::default() };
    let info = stream(&original);
    let changed = Sequence { color_present: true, ..original };
    assert_eq!(info.av1_parameter_difference(&sample(&changed)).unwrap(), Av1ParameterDifference::Unchanged);
}

#[test]
fn av1_color_candidate_rejects_removing_initial_display_delay() {
    let original = Sequence { initial_delay: Some(2), ..Default::default() };
    let info = stream(&original);
    let changed = Sequence { initial_delay: None, color: (9, 16, 9), ..original };
    assert_eq!(info.av1_parameter_difference(&sample(&changed)).unwrap(), Av1ParameterDifference::Structural);
}

#[test]
fn av1_comparison_detects_tier_change_without_color_change() {
    let original = Sequence { level: 8, ..Default::default() };
    let info = stream(&original);
    let changed = Sequence { tier: true, ..original };
    assert_eq!(info.av1_parameter_difference(&sample(&changed)).unwrap(), Av1ParameterDifference::Structural);
}

#[test]
fn av1_comparison_detects_timing_change_without_color_change() {
    let original = timing_sequence();
    let info = stream(&original);
    let mut changed = original;
    changed.timing.as_mut().unwrap().display_tick = 1000;
    assert_eq!(info.av1_parameter_difference(&sample(&changed)).unwrap(), Av1ParameterDifference::Structural);
}

#[test]
fn av1_repeated_sequence_headers_exhaust_bounded_work() {
    let original = Sequence::default();
    let mut info = stream(&original);
    let mut large = payload(&original);
    large.resize(65_536, 0);
    info.sequence_header = Some(large.clone());
    let short_obu = obu(1, &payload(&original));
    let mut many = Vec::new();
    for _ in 0..80 {
        many.extend_from_slice(&short_obu);
    }
    assert!(info.av1_parameter_difference(&many).is_err());

    let long_obu = obu(1, &large);
    let mut many_large = Vec::new();
    for _ in 0..80 {
        many_large.extend_from_slice(&long_obu);
    }
    assert!(info.initial_av1_color_candidate(&many_large).is_err());
}

#[test]
fn av1_color_candidate_rejects_a_tier_change() {
    let original = Sequence { level: 8, ..Default::default() };
    let info = stream(&original);
    let changed = Sequence { tier: true, color: (9, 16, 9), ..original };
    let bytes = sample(&changed);
    assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::Structural);
    assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none());
}

#[test]
fn av1_color_candidate_rejects_an_initial_display_delay_change() {
    let original = Sequence { initial_delay: Some(2), ..Default::default() };
    let info = stream(&original);
    let changed = Sequence { initial_delay: Some(3), color: (9, 16, 9), ..original };
    let bytes = sample(&changed);
    assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::Structural);
    assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none());
}

fn timing_sequence() -> Sequence {
    Sequence {
        timing: Some(Timing {
            display_tick: 1001,
            time_scale: 60_000,
            equal_ticks: Some(0),
            decoder: Some(DecoderModel { buffer_delay_length_minus_1: 0, decoding_tick: 1000, decoder_delay: 0, encoder_delay: 0, low_delay: false }),
        }),
        ..Default::default()
    }
}

fn assert_color_and_timing_change_is_structural(mut changed: Sequence) {
    let info = stream(&timing_sequence());
    changed.color = (9, 16, 9);
    let bytes = sample(&changed);
    assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::Structural);
    assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none());
}

#[test]
fn av1_color_comparison_detects_display_tick_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().display_tick = 1000;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_time_scale_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().time_scale = 50_000;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_equal_picture_interval_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().equal_ticks = Some(1);
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_buffer_delay_length_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().decoder.as_mut().unwrap().buffer_delay_length_minus_1 = 1;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_decoding_tick_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().decoder.as_mut().unwrap().decoding_tick = 1001;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_decoder_buffer_delay_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().decoder.as_mut().unwrap().decoder_delay = 1;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_encoder_buffer_delay_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().decoder.as_mut().unwrap().encoder_delay = 1;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_color_comparison_detects_low_delay_mode_change() {
    let mut changed = timing_sequence();
    changed.timing.as_mut().unwrap().decoder.as_mut().unwrap().low_delay = true;
    assert_color_and_timing_change_is_structural(changed);
}

#[test]
fn av1_checked_color_candidate_declines_tile_groups_before_the_first_frame() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    for before_sequence in [true, false] {
        let mut bytes = Vec::new();
        if before_sequence {
            bytes.extend(obu(4, &[0]));
        }
        bytes.extend(obu(1, &payload(&changed)));
        if !before_sequence {
            bytes.extend(obu(4, &[0]));
        }
        bytes.extend(obu(6, &[0x19, 0, 0]));
        assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none(), "orphan tile group before_sequence={before_sequence}");
    }
}

#[test]
fn av1_checked_color_candidate_preserves_padding_and_postframe_tile_handling() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    for (kind, frame) in [(3, [0x19, 0, 2]), (6, [0x19, 0, 0])] {
        let mut bytes = obu(15, &[0]);
        bytes.extend(obu(1, &payload(&changed)));
        bytes.extend(obu(15, &[0]));
        bytes.extend(obu(kind, &frame));
        bytes.extend(obu(4, &[0]));
        bytes.extend(obu(15, &[0]));
        let candidate = info.initial_av1_color_candidate(&bytes).unwrap().unwrap();
        assert_eq!(candidate.config_obus, obu(1, &payload(&changed)));
        assert_eq!(candidate.color, Some(av1_color(64, 64, 9, 16, 9, false)));
    }
}

#[test]
fn av1_checked_color_candidate_classifies_each_description_field_and_preserves_original() {
    let original = Sequence::default();
    let info = stream(&original);
    for color in [(9, 1, 1), (1, 16, 1), (1, 1, 9), (9, 16, 9), (9, 18, 9), (2, 2, 2)] {
        let changed = Sequence { color, ..original.clone() };
        let bytes = sample(&changed);
        assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::ColorDescription { before: original.color, after: color });
        let accepted = info.initial_av1_color_candidate(&bytes).unwrap().unwrap();
        assert_eq!(accepted.color, Some(av1_color(64, 64, color.2, color.1, color.0, false)));
        assert_eq!((accepted.size, accepted.bit_depth, accepted.subsampling), ((64, 64), 10, (1, 1)));
        assert_eq!(info.config_obus, obu(1, &payload(&original)));
        assert_eq!(accepted.config_obus, obu(1, &payload(&changed)));
        assert!(info.parameters_changed(&bytes), "existing policy stays conservative");
    }
}
#[test]
fn av1_checked_color_candidate_rejects_every_structural_difference() {
    let original = Sequence::default();
    let info = stream(&original);
    let changes = [
        Sequence { profile: 1, ..original.clone() },
        Sequence { size: (65, 64), ..original.clone() },
        Sequence { size: (64, 65), ..original.clone() },
        Sequence { depth: 8, ..original.clone() },
        Sequence { mono: true, ..original.clone() },
        Sequence { range: true, ..original.clone() },
        Sequence { location: 1, ..original.clone() },
        Sequence { separate_uv: true, ..original.clone() },
        Sequence { profile: 2, ..original.clone() },
    ];
    for changed in changes {
        let bytes = sample(&changed);
        assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::Structural);
        assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none());
    }
}
#[test]
fn av1_checked_color_candidate_requires_a_sequence_before_a_complete_independent_frame() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    for frame in [vec![0x80], vec![0x30], vec![0x00]] {
        let mut bytes = obu(1, &payload(&changed));
        bytes.extend(obu(6, &frame));
        assert!(info.initial_av1_color_candidate(&bytes).unwrap().is_none());
    }
    let mut reversed = obu(6, &[0x19, 0, 0]);
    reversed.extend(obu(1, &payload(&changed)));
    assert!(info.initial_av1_color_candidate(&reversed).unwrap().is_none());
    assert!(info.initial_av1_color_candidate(&obu(6, &[0x19, 0, 0])).unwrap().is_none());
    let mut truncated = obu(1, &payload(&changed));
    truncated.extend(obu(6, &[0x19]));
    assert!(info.initial_av1_color_candidate(&truncated).is_err());
    let reduced = Sequence { reduced: true, ..changed };
    let reduced_info = stream(&Sequence { reduced: true, ..Default::default() });
    assert!(reduced_info.initial_av1_color_candidate(&sample(&reduced)).unwrap().is_some());
}
#[test]
fn av1_checked_color_candidate_checks_framing_tail_and_resource_caps() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    for tail in
        [vec![0x8a, 0], vec![0x0b, 0], vec![0x0e], vec![0x0e, 1, 0], vec![0x0a, 128], vec![0x0a, 128, 128, 128, 128, 128, 128, 128, 128], vec![0x32, 4, 0]]
    {
        let mut bytes = sample(&changed);
        bytes.extend(tail);
        assert!(info.av1_parameter_difference(&bytes).is_err());
        assert!(info.initial_av1_color_candidate(&bytes).is_err());
    }
    let mut bytes = sample(&changed);
    bytes.extend(obu(1, &[0xff]));
    assert_eq!(info.av1_parameter_difference(&bytes).unwrap(), Av1ParameterDifference::InvalidHeader);
    assert!(info.initial_av1_color_candidate(&bytes).is_err());
    let huge = obu(1, &vec![0; 65_537]);
    assert!(info.av1_parameter_difference(&huge).is_err());
    let too_many = [0x12, 0].repeat(4097);
    assert!(info.av1_parameter_difference(&too_many).is_err());
    let mut oversized = info.clone();
    oversized.config_obus = vec![0; 65_537];
    assert!(oversized.initial_av1_color_candidate(&sample(&changed)).is_err());
}
#[test]
fn av1_checked_color_candidate_declines_metadata_unknown_color_and_conflicting_sequences() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    let mut metadata = sample(&changed);
    metadata.extend(obu(5, &[1, 128]));
    assert!(info.initial_av1_color_candidate(&metadata).unwrap().is_none());
    let mut with_metadata = info.clone();
    with_metadata.config_obus.extend(obu(5, &[1, 128]));
    assert!(with_metadata.initial_av1_color_candidate(&sample(&changed)).unwrap().is_none());
    for color in [(255, 16, 9), (9, 255, 9), (9, 16, 255)] {
        assert!(info.initial_av1_color_candidate(&sample(&Sequence { color, ..changed.clone() })).unwrap().is_none());
    }
    let mut conflict = obu(1, &payload(&changed));
    conflict.extend(sample(&Sequence::default()));
    assert!(info.initial_av1_color_candidate(&conflict).unwrap().is_none());
    assert_eq!(info.av1_parameter_difference(&sample(&Sequence::default())).unwrap(), Av1ParameterDifference::Unchanged);
    assert!(info.initial_av1_color_candidate(&sample(&Sequence::default())).unwrap().is_none());
}
#[test]
fn av1_checked_color_candidate_mutations_and_truncations_do_not_panic() {
    let info = stream(&Sequence::default());
    let bytes = sample(&Sequence { color: (9, 16, 9), ..Default::default() });
    for end in 0..bytes.len() {
        assert!(std::panic::catch_unwind(|| info.initial_av1_color_candidate(&bytes[..end])).is_ok());
    }
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut corrupted = bytes.clone();
            corrupted[index] ^= 1 << bit;
            assert!(std::panic::catch_unwind(|| info.initial_av1_color_candidate(&corrupted)).is_ok());
        }
    }
}

#[test]
fn av1_checked_color_candidate_checks_sequence_trailing_and_frame_alignment_bits() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    let mut wrong_sequence = payload(&changed);
    let last = wrong_sequence.last_mut().unwrap();
    *last ^= 1;
    assert_eq!(info.av1_parameter_difference(&obu(1, &wrong_sequence)).unwrap(), Av1ParameterDifference::InvalidHeader);
    let mut header = obu(1, &payload(&changed));
    header.extend(obu(3, &[0x19, 0, 2]));
    assert!(info.initial_av1_color_candidate(&header).unwrap().is_some());
    for frame in [(3, [0x19, 0, 0]), (6, [0x19, 0, 1])] {
        let mut bytes = obu(1, &payload(&changed));
        bytes.extend(obu(frame.0, &frame.1));
        assert!(info.initial_av1_color_candidate(&bytes).is_err());
    }
    assert_eq!(info.av1_parameter_difference(&[0x12, 0].repeat(4096)).unwrap(), Av1ParameterDifference::Unchanged);
    // ISOBMFF permits the final OBU to omit its size; it consumes exactly the remaining sample.
    let mut sizeless = obu(1, &payload(&changed));
    sizeless.extend([0x30, 0x19, 0, 0]);
    assert!(info.initial_av1_color_candidate(&sizeless).unwrap().is_some());
    let mut layered = obu(1, &payload(&changed));
    layered.extend([0x36, 0x20, 3, 0x19, 0, 0]);
    assert!(info.initial_av1_color_candidate(&layered).unwrap().is_none());
}
