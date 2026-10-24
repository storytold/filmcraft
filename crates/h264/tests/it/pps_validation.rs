//! #1441: malformed PPS fields must return `Error::Invalid`, not panic.

use filmcraft_h264::Decoder;

/// Valid, synthetic Baseline SPS (176x144, 8-bit).
const SPS: [u8; 12] = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0xda, 0x0b, 0x13, 0x90];

fn decode_pps(pps: &[u8]) -> Result<(), filmcraft_h264::Error> {
    let mut decoder = Decoder::with_threads(1);
    decoder.decode(&SPS, 0).expect("sps decodes");
    decoder.decode(pps, 0).map(|_| ())
}

/// The reporter's literal: PPS with `num_ref_idx_l0_default_active_minus1 = u32::MAX`.
#[test]
fn hostile_pps_returns_an_error_instead_of_panicking() {
    let pps = [0x00, 0x00, 0x00, 0x01, 0x68, 0xc8, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x02, 0x3c, 0x80];
    let outcome = std::panic::catch_unwind(|| {
        let mut decoder = Decoder::with_threads(1);
        decoder.decode(&SPS, 0).unwrap();
        decoder.decode(&pps, 0)
    });
    assert!(outcome.is_ok_and(|result| result.is_err()));
}

/// Write ue(v) as bits (helper for building synthetic PPS payloads).
fn push_ue(bits: &mut Vec<bool>, v: u32) {
    let x = v as u64 + 1;
    let len = 64 - x.leading_zeros();
    for _ in 1..len {
        bits.push(false);
    }
    for i in (0..len).rev() {
        bits.push(x & (1 << i) != 0);
    }
}

fn push_bits(bits: &mut Vec<bool>, v: u32, n: u32) {
    for i in (0..n).rev() {
        bits.push(v & (1 << i) != 0);
    }
}

fn to_bytes(bits: &[bool]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in bits.chunks(8) {
        let mut b = 0u8;
        for (i, v) in chunk.iter().enumerate() {
            b |= (*v as u8) << (7 - i);
        }
        out.push(b);
    }
    out
}

/// Build a PPS NAL (header byte 0x68 + payload) with `num_slice_groups_minus1` = u32::MAX.
#[test]
fn hostile_num_slice_groups_is_rejected() {
    let mut bits = vec![true, true, false, false];
    push_ue(&mut bits, u32::MAX); // num_slice_groups_minus1
    bits.extend(std::iter::repeat_n(false, 64));
    let payload = to_bytes(&bits);
    bits.extend(std::iter::repeat_n(false, 64));
    let mut nal = vec![0x00, 0x00, 0x00, 0x01, 0x68];
    nal.extend_from_slice(&payload);
    assert!(std::panic::catch_unwind(|| decode_pps(&nal)).is_ok_and(|r| r.is_err()));
}

/// `num_ref_idx_l1_default_active_minus1 = u32::MAX` (l0 valid) must error, not panic.
#[test]
fn hostile_num_ref_idx_l1_is_rejected() {
    let mut bits = vec![true, true, false, false];
    push_ue(&mut bits, 0); // num_slice_groups_minus1
    push_ue(&mut bits, 1); // num_ref_idx_l0_minus1
    push_ue(&mut bits, u32::MAX); // num_ref_idx_l1_minus1
    bits.extend(std::iter::repeat_n(false, 64));
    let mut nal = vec![0x00, 0x00, 0x00, 0x01, 0x68];
    nal.extend_from_slice(&to_bytes(&bits));
    assert!(std::panic::catch_unwind(|| decode_pps(&nal)).is_ok_and(|r| r.is_err()));
}

/// `pic_init_qp_minus26 = i32::MAX` must error, not overflow.
#[test]
fn hostile_pic_init_qp_is_rejected() {
    let mut bits = vec![true, true, false, false];
    push_ue(&mut bits, 0); // num_slice_groups_minus1
    push_ue(&mut bits, 0); // num_ref_idx_l0_minus1
    push_ue(&mut bits, 0); // num_ref_idx_l1_minus1
    bits.push(false); // weighted_pred
    push_bits(&mut bits, 0, 2); // weighted_bipred_idc
    // se(i32::MAX): codeNum = 2*MAX-1 = u32::MAX-2... encode via ue of huge codeNum
    push_ue(&mut bits, u32::MAX - 2); // codeNum for +2^31-1... any out-of-range codeNum works
    bits.extend(std::iter::repeat_n(false, 64));
    let mut nal = vec![0x00, 0x00, 0x00, 0x01, 0x68];
    nal.extend_from_slice(&to_bytes(&bits));
    assert!(std::panic::catch_unwind(|| decode_pps(&nal)).is_ok_and(|r| r.is_err()));
}

/// `weighted_bipred_idc = 3` is concealed to 2 per 7.4.2.2.1 (not rejected, not a panic).
#[test]
fn reserved_weighted_bipred_idc_is_concealed() {
    let mut bits = vec![true, true, false, false];
    push_ue(&mut bits, 0);
    push_ue(&mut bits, 0);
    push_ue(&mut bits, 0);
    bits.push(false); // weighted_pred
    push_bits(&mut bits, 3, 2); // weighted_bipred_idc = 3 (reserved)
    push_ue(&mut bits, 0); // pic_init_qp_minus26 = 0
    push_ue(&mut bits, 0); // pic_init_qs_minus26 = 0
    push_ue(&mut bits, 0); // chroma_qp_index_offset = 0
    bits.push(false); // deblocking_filter_control_present
    bits.push(false); // constrained_intra_pred
    bits.push(false); // redundant_pic_cnt_present
    bits.extend(std::iter::repeat_n(false, 64));
    let mut nal = vec![0x00, 0x00, 0x00, 0x01, 0x68];
    nal.extend_from_slice(&to_bytes(&bits));
    // parses fine; the decoder treats the field as 2
    decode_pps(&nal).expect("weighted_bipred_idc=3 concealed to 2");
}

/// A well-formed minimal PPS still parses (no regression).
#[test]
fn minimal_valid_pps_parses() {
    let mut bits = vec![true, true, false, false];
    push_ue(&mut bits, 0);
    push_ue(&mut bits, 0);
    push_ue(&mut bits, 0);
    bits.push(false); // weighted_pred
    push_bits(&mut bits, 0, 2); // weighted_bipred_idc
    push_ue(&mut bits, 0); // pic_init_qp_minus26
    push_ue(&mut bits, 0); // pic_init_qs_minus26
    push_ue(&mut bits, 0); // chroma_qp_index_offset
    bits.push(true); // deblocking_filter_control_present
    bits.push(false); // constrained_intra_pred
    bits.push(false); // redundant_pic_cnt_present
    bits.extend(std::iter::repeat_n(false, 64));
    let mut nal = vec![0x00, 0x00, 0x00, 0x01, 0x68];
    nal.extend_from_slice(&to_bytes(&bits));
    if let Err(e) = decode_pps(&nal) {
        panic!("minimal PPS rejected: {e}");
    }
}
