//! Original synthetic fixtures, AV1 v1.0.0 + Errata 1 sections 5.5/5.9.
use super::*;
use filmcraft_codecs::hw::FrameStreamInfo;

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
    let entry = filmcraft_isobmff::SampleEntry::av1(config, u16::try_from(s.size.0).unwrap(), u16::try_from(s.size.1).unwrap());
    FrameStreamInfo::from_entry(&entry).unwrap().unwrap()
}
fn sample(s: &Sequence) -> Vec<u8> {
    let mut out = obu(1, &payload(s));
    out.extend(obu(6, if s.reduced { &[0x90, 0, 0] } else { &[0x19, 0, 0] }));
    out
}

fn pq() -> Sequence {
    Sequence { color: (9, 16, 9), ..Default::default() }
}

#[test]
fn av1_run_adopts_initial_color_once_and_repeats_without_rechecking_eligibility() {
    use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
    let original = stream(&Sequence::default());
    let original_config = original.config_obus.clone();
    let mut run = Av1RunMetadata::new(original);
    let bytes = sample(&pq());
    let mut checked = false;
    assert!(
        run.prepare(&bytes, 0, |candidate| {
            checked = true;
            assert_eq!((candidate.size, candidate.bit_depth), ((64, 64), 10));
            candidate.color == Some(ColorInfo { primaries: Primaries::Bt2020, transfer: Transfer::Pq, matrix: Matrix::Bt2020Ncl, range: Range::Limited })
        })
        .unwrap()
    );
    assert!(checked);
    assert_ne!(run.active().config_obus, original_config);
    assert_eq!(run.active().config_obus, obu(1, &payload(&pq())));
    assert!(run.prepare(&bytes, 5, |_| panic!("matching active metadata must be idempotent")).unwrap());
    let next = Sequence { color: (9, 18, 9), ..Default::default() };
    assert!(!run.prepare(&sample(&next), 0, |_| true).unwrap());
    assert_eq!(run.active().color.unwrap().transfer, Transfer::Pq);
}

#[test]
fn av1_run_declined_candidate_leaves_active_header_and_window_unchanged() {
    let original = stream(&Sequence::default());
    let original_config = original.config_obus.clone();
    let mut run = Av1RunMetadata::new(original);
    assert!(!run.prepare(&sample(&pq()), 0, |_| false).unwrap());
    assert_eq!(run.active().config_obus, original_config);
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
}

#[test]
fn av1_run_pending_output_blocks_adoption_without_calling_eligibility() {
    let original = stream(&Sequence::default());
    let original_config = original.config_obus.clone();
    let mut run = Av1RunMetadata::new(original);
    assert!(!run.prepare(&sample(&pq()), 1, |_| panic!("pending output blocks before native eligibility")).unwrap());
    assert_eq!(run.active().config_obus, original_config);
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
}

#[test]
fn av1_run_headerless_submission_and_drain_cannot_reopen_adoption() {
    let mut run = Av1RunMetadata::new(stream(&Sequence::default()));
    // A frame without a sequence header is allowed to continue, never to authorize a change.
    assert!(run.prepare(&obu(6, &[0x19, 0, 0]), 0, |_| panic!("no candidate header")).unwrap());
    run.close_adoption(); // C closes before the first native submission, including headerless input.
    assert!(!run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    run.close_adoption(); // Drain closes even if its output queue becomes empty.
    assert!(!run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    run.restart_empty(); // Only a confirmed reset or fresh MFT permits this call.
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    run.close_adoption(); // Failed flush/reset without recreation stays closed.
    let hlg = Sequence { color: (9, 18, 9), ..Default::default() };
    assert!(!run.prepare(&sample(&hlg), 0, |_| true).unwrap());
    run.restart_empty();
    assert!(run.prepare(&sample(&hlg), 0, |_| true).unwrap());
}

#[test]
fn av1_run_active_guard_rejects_reversion_that_original_hybrid_info_calls_unchanged() {
    let original = stream(&Sequence::default());
    let old_sample = sample(&Sequence::default());
    assert!(!original.parameters_changed(&old_sample));
    let mut run = Av1RunMetadata::new(original);
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    assert!(!run.prepare(&old_sample, 0, |_| true).unwrap());
    assert_eq!(run.active().config_obus, obu(1, &payload(&pq())));
}

#[test]
fn av1_run_malformed_structural_and_non_restart_samples_never_authorize_adoption() {
    let original = stream(&Sequence::default());
    let original_config = original.config_obus.clone();
    let mut run = Av1RunMetadata::new(original);
    for seq in [Sequence { size: (65, 64), ..pq() }, Sequence { depth: 8, ..pq() }, Sequence { range: true, ..pq() }] {
        assert!(!run.prepare(&sample(&seq), 0, |_| panic!("structural format cannot be eligible")).unwrap());
    }
    let mut non_restart = obu(1, &payload(&pq()));
    non_restart.extend(obu(6, &[0x30])); // An inter-frame prefix cannot restart.
    assert!(!run.prepare(&non_restart, 0, |_| panic!("non-restart cannot be eligible")).unwrap());
    for invalid in [
        vec![0x0a, 0x80],
        obu(1, &[0xff]),
        {
            let mut bytes = sample(&pq());
            bytes.extend([0x0a, 0x80]);
            bytes
        },
        {
            let mut bytes = obu(1, &payload(&pq()));
            bytes.extend(obu(6, &[0x19])); // Truncated uncompressed key-frame header.
            bytes
        },
    ] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.prepare(&invalid, 0, |_| panic!("invalid input cannot be eligible"))));
        assert!(result.is_ok(), "hostile parameter inspection must not panic");
        assert!(!matches!(result.unwrap(), Ok(true)));
        assert_eq!(run.active().config_obus, original_config);
    }
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
}
