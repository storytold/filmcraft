//! Synthetic uncompressed headers from AV1 v1.0.0 + Errata 1, sections 5.5/5.9.
use filmcraft_av1::{ColorConfig, SequenceHeader, independent_frame_header_info};

fn sequence(reduced: bool) -> SequenceHeader {
    SequenceHeader {
        profile: 0,
        still_picture: reduced,
        reduced_still_picture_header: reduced,
        frame_width_bits: 6,
        frame_height_bits: 6,
        max_frame_width: 64,
        max_frame_height: 64,
        seq_force_screen_content_tools: if reduced { 2 } else { 0 },
        seq_force_integer_mv: 2,
        color: ColorConfig { bit_depth: 8, num_planes: 3, subsampling_x: 1, subsampling_y: 1, ..Default::default() },
        ..Default::default()
    }
}

#[test]
fn checked_independent_header_regular_and_reduced_still() {
    for (reduced, payload) in [(false, [0x19, 0, 0]), (true, [0x90, 0, 0])] {
        let h = independent_frame_header_info(&sequence(reduced), &payload, 0, 0).unwrap().unwrap();
        assert_eq!(h.size, (64, 64));
    }
}

#[test]
fn checked_independent_header_declines_dependent_or_nonbase_frames() {
    let s = sequence(false);
    for payload in [[0x80], [0x30], [0x00]] {
        assert!(independent_frame_header_info(&s, &payload, 0, 0).unwrap().is_none());
    }
    assert!(independent_frame_header_info(&s, &[0x19, 0, 0], 1, 0).unwrap().is_none());
    let mut scalable = s;
    scalable.operating_point_idc[0] = 0x101;
    assert!(independent_frame_header_info(&scalable, &[0x19, 0, 0], 0, 0).unwrap().is_none());
}

#[test]
fn checked_independent_header_requires_the_complete_header() {
    let s = sequence(false);
    for payload in [&[][..], &[0x19][..], &[0x19, 0][..]] {
        assert!(independent_frame_header_info(&s, payload, 0, 0).is_err());
    }
}

#[test]
fn checked_independent_header_hostile_public_sequence_and_mutations_do_not_panic() {
    let mut s = sequence(false);
    s.max_frame_width = u32::MAX;
    s.frame_width_bits = u32::MAX;
    assert!(independent_frame_header_info(&s, &[0x19, 0, 0], 0, 0).is_err());
    for bit in 0..24 {
        let mut payload = [0x19, 0, 0];
        payload[bit / 8] ^= 1 << (bit % 8);
        assert!(std::panic::catch_unwind(|| independent_frame_header_info(&sequence(false), &payload, 0, 0)).is_ok());
    }
}
