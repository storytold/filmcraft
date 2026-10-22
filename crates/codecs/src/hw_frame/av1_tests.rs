//! Original synthetic fixtures, AV1 v1.0.0 + Errata 1 sections 5.5/5.9.
use super::*;

#[derive(Clone)]
struct Sequence {
    profile: u8,
    size: (u32, u32),
    depth: u8,
    mono: bool,
    color: (u8, u8, u8),
    range: bool,
    location: u8,
    separate_uv: bool,
    reduced: bool,
}
impl Default for Sequence {
    fn default() -> Self {
        Self { profile: 0, size: (64, 64), depth: 10, mono: false, color: (1, 1, 1), range: false, location: 0, separate_uv: false, reduced: false }
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
        b.f(0, 1);
        b.f(0, 1);
        b.f(0, 5);
        b.f(0, 12);
        b.f(0, 5);
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
    b.f(1, 1);
    b.f(u32::from(s.color.0), 8);
    b.f(u32::from(s.color.1), 8);
    b.f(u32::from(s.color.2), 8);
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
        seq_level_idx_0: 0,
        seq_tier_0: false,
        high_bitdepth: s.depth > 8,
        twelve_bit: s.depth == 12,
        monochrome: s.mono,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        chroma_sample_position: s.location,
        initial_presentation_delay_minus_one: None,
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

#[test]
fn av1_checked_color_candidate_declines_zero_layer_obu_extensions() {
    let info = stream(&Sequence::default());
    let changed = Sequence { color: (9, 16, 9), ..Default::default() };
    assert!(info.initial_av1_color_candidate(&sample(&changed)).unwrap().is_some());
    for kind in [1, 2, 6] {
        let extended = |kind, bytes: &[u8]| {
            let mut unit = obu(kind, bytes);
            unit[0] |= 4;
            unit.insert(1, 0); // Present extension, zero temporal/spatial IDs and reserved bits.
            unit
        };
        let mut bytes = if kind == 2 { extended(2, &[]) } else { Vec::new() };
        bytes.extend(if kind == 1 { extended(1, &payload(&changed)) } else { obu(1, &payload(&changed)) });
        bytes.extend(if kind == 6 { extended(6, &[0x19, 0, 0]) } else { obu(6, &[0x19, 0, 0]) });
        assert!(!matches!(info.initial_av1_color_candidate(&bytes), Ok(Some(_))), "zero-valued extension on OBU {kind} must not authorize op_idc=0 adoption");
        let active = stream(&changed);
        assert!(active.av1_parameter_difference(&bytes).is_err(), "op_idc=0 active guard must reject zero-valued extension on OBU {kind}");
    }
}
