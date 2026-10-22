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
    large_superblock: bool,
}
impl Default for Sequence {
    fn default() -> Self {
        Self {
            profile: 0,
            size: (64, 64),
            depth: 10,
            mono: false,
            color: (1, 1, 1),
            range: false,
            location: 0,
            separate_uv: false,
            reduced: false,
            large_superblock: false,
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
    b.f(u32::from(s.large_superblock), 1); // use_128x128_superblock; 64x64 picture still has one tile.
    b.f(0, 2); // filter intra, intra edge
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

fn entry(s: &Sequence) -> filmcraft_isobmff::SampleEntry {
    use filmcraft_isobmff::{Av1Config, SampleEntry};
    SampleEntry::av1(
        Av1Config {
            high_bitdepth: s.depth > 8,
            chroma_subsampling_x: true,
            chroma_subsampling_y: true,
            config_obus: obu(1, &payload(s)),
            ..Default::default()
        },
        u16::try_from(s.size.0).unwrap(),
        u16::try_from(s.size.1).unwrap(),
    )
}

#[test]
fn mf_av1_container_matching_nclx_nclc_and_absent_override_allow_effective_color() {
    use super::super::av1_container::Av1ContainerPolicy;
    use filmcraft_isobmff::ColorInfo as Colr;
    let original = Sequence::default();
    for target in [Sequence { depth: 8, color: (9, 1, 9), ..original.clone() }, pq(), Sequence { color: (9, 18, 9), ..original.clone() }] {
        let original = Sequence { depth: target.depth, ..Default::default() };
        let candidate = stream(&original).initial_av1_color_candidate(&sample(&target)).unwrap().unwrap();
        for colr in [
            None,
            Some(Colr::Nclx { primaries: target.color.0.into(), transfer: target.color.1.into(), matrix: target.color.2.into(), full_range: false }),
            Some(Colr::Nclc { primaries: target.color.0.into(), transfer: target.color.1.into(), matrix: target.color.2.into() }),
        ] {
            let mut original_entry = entry(&original);
            original_entry.video.as_mut().unwrap().color = colr;
            let unchanged = original_entry.clone();
            assert!(Av1ContainerPolicy::from_entry(&original_entry, original.size).permits(&candidate));
            assert_eq!(original_entry, unchanged);
        }
    }
    let candidate = stream(&original).initial_av1_color_candidate(&sample(&pq())).unwrap().unwrap();
    assert!(!Av1ContainerPolicy::default().permits(&candidate), "unknown original context defaults to deny");
}

#[test]
fn mf_av1_container_conflicts_unspecified_fallback_and_unhandled_metadata_decline() {
    use super::super::av1_container::Av1ContainerPolicy;
    use filmcraft_isobmff::{CleanAperture, ColorInfo as Colr, FourCc, MasteringDisplay};
    let original = Sequence::default();
    let candidate = stream(&original).initial_av1_color_candidate(&sample(&pq())).unwrap().unwrap();
    for colr in [
        Colr::Nclx { primaries: 1, transfer: 16, matrix: 9, full_range: false },
        Colr::Nclx { primaries: 9, transfer: 1, matrix: 9, full_range: false },
        Colr::Nclx { primaries: 9, transfer: 16, matrix: 1, full_range: false },
        Colr::Nclx { primaries: 9, transfer: 16, matrix: 9, full_range: true },
        Colr::Nclx { primaries: 2, transfer: 2, matrix: 2, full_range: false },
        Colr::Nclc { primaries: 2, transfer: 2, matrix: 2 },
        Colr::Icc { kind: FourCc(*b"prof"), profile: vec![1, 2, 3] },
    ] {
        let mut original_entry = entry(&original);
        original_entry.video.as_mut().unwrap().color = Some(colr);
        let unchanged = original_entry.clone();
        assert!(!Av1ContainerPolicy::from_entry(&original_entry, original.size).permits(&candidate));
        assert_eq!(original_entry, unchanged);
    }
    for kind in 0..4 {
        let mut original_entry = entry(&original);
        let video = original_entry.video.as_mut().unwrap();
        match kind {
            0 => video.mastering_display = Some(MasteringDisplay::bt2020(1000.0, 0.01)),
            1 => video.content_light = Some((1000, 400)),
            2 => video.clean_aperture = Some(CleanAperture { width: (32, 1), height: (32, 1), horiz_offset: (0, 1), vert_offset: (0, 1) }),
            _ => video.gamma = Some(0x0002_3333),
        }
        assert!(!Av1ContainerPolicy::from_entry(&original_entry, original.size).permits(&candidate));
    }
}

#[test]
fn mf_av1_native_boundary_keeps_plan_spec_and_effective_header_consistent() {
    use super::super::av1_container::Av1ContainerPolicy;
    use super::super::gpu::SurfaceFormat;
    use super::super::stream::Stream;
    use filmcraft_color::Transfer;
    let original = Sequence::default();
    let policy = Av1ContainerPolicy::from_entry(&entry(&original), original.size);
    // Legacy direct Frame construction must enter the same guarded owner at decoder creation.
    let mut native = Stream::Frame(stream(&original)).into_guarded();
    let before_plan = native.plan().unwrap();
    let before_spec = native.spec(SurfaceFormat::P010);
    let accepted = sample(&pq());
    assert!(native.prepare_av1(&accepted, 0, SurfaceFormat::P010, &policy).unwrap());
    let after_plan = native.plan().unwrap();
    let after_spec = native.spec(SurfaceFormat::P010);
    assert_eq!((before_plan.size, before_plan.profile, before_plan.format), (after_plan.size, after_plan.profile, after_plan.format));
    assert_eq!((before_spec.size, before_spec.subtype, before_spec.format), (after_spec.size, after_spec.subtype, after_spec.format));
    assert!(before_spec.attrs.is_empty() && after_spec.attrs.is_empty());
    assert_eq!(native.first_geometry(SurfaceFormat::P010).color.transfer, Transfer::Pq);
    assert_eq!(native.frame_info().unwrap().color.unwrap().transfer, Transfer::Pq);
    assert_eq!(native.input(&accepted, true).unwrap(), accepted, "actual in-band header enters unchanged");
    native.close_av1_adoption();
    assert!(native.prepare_av1(&accepted, 2, SurfaceFormat::P010, &Av1ContainerPolicy::default()).unwrap(), "matching active header is idempotent");
    assert!(!native.prepare_av1(&sample(&original), 0, SurfaceFormat::P010, &policy).unwrap(), "active guard catches original-header reversion");
    let headerless = obu(6, &[0x19, 0, 0]);
    let mut injected = obu(1, &payload(&pq()));
    injected.extend_from_slice(&headerless);
    native.restart_av1_empty();
    assert_eq!(native.input(&headerless, true).unwrap(), injected, "reset reinjects effective header");
    let hlg = Sequence { color: (9, 18, 9), ..Default::default() };
    assert!(native.prepare_av1(&sample(&hlg), 0, SurfaceFormat::P010, &policy).unwrap());
    assert_eq!(native.first_geometry(SurfaceFormat::P010).color.transfer, Transfer::Hlg);
}

#[test]
fn mf_av1_native_boundary_unknown_context_closed_run_and_mismatched_spec_decline() {
    use super::super::av1_container::Av1ContainerPolicy;
    use super::super::gpu::SurfaceFormat;
    use super::super::mft::Attr;
    use super::super::stream::{Stream, av1_native_compatible};
    use windows::Win32::Media::MediaFoundation::{MF_MT_MPEG2_PROFILE, MFVideoFormat_VP90};
    let original = Sequence::default();
    let policy = Av1ContainerPolicy::from_entry(&entry(&original), original.size);
    let mut native = Stream::from(stream(&original));
    let changed = sample(&pq());
    assert!(!native.prepare_av1(&changed, 0, SurfaceFormat::P010, &Av1ContainerPolicy::default()).unwrap());
    assert!(!native.prepare_av1(&changed, 0, SurfaceFormat::Nv12, &policy).unwrap());
    native.close_av1_adoption();
    assert!(!native.prepare_av1(&changed, 0, SurfaceFormat::P010, &policy).unwrap());
    native.restart_av1_empty();
    assert!(native.prepare_av1(&changed, 0, SurfaceFormat::P010, &policy).unwrap());
    let candidate = stream(&original).initial_av1_color_candidate(&changed).unwrap().unwrap();
    let plan = native.plan().unwrap();
    assert!(av1_native_compatible(&plan, &native.spec(SurfaceFormat::P010), &candidate));
    for kind in 0..4 {
        let mut spec = native.spec(SurfaceFormat::P010);
        match kind {
            0 => spec.attrs.push((MF_MT_MPEG2_PROFILE, Attr::U32(1))),
            1 => spec.size = (65, 64),
            2 => spec.format = SurfaceFormat::Nv12,
            _ => spec.subtype = MFVideoFormat_VP90,
        }
        assert!(!av1_native_compatible(&plan, &spec, &candidate));
    }
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
fn av1_run_replay_initialization_tracks_accepted_header_across_empty_resets() {
    let original = Sequence::default();
    let mut run = Av1RunMetadata::new(stream(&original));
    assert!(run.software_replay_initialization().is_none(), "ordinary configuration keeps default replay");
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    assert_eq!(run.software_replay_initialization(), Some(obu(1, &payload(&pq())).as_slice()));
    run.restart_empty();
    assert_eq!(run.software_replay_initialization(), Some(obu(1, &payload(&pq())).as_slice()));
    assert!(run.prepare(&sample(&original), 0, |_| true).unwrap());
    assert_eq!(run.software_replay_initialization(), Some(obu(1, &payload(&original)).as_slice()), "new accepted reversion updates replay state");
    run.close_adoption();
    assert_eq!(run.software_replay_initialization(), Some(obu(1, &payload(&original)).as_slice()));
}

#[test]
fn av1_run_replay_initialization_refreshes_compatible_coding_tool_restart_after_adoption() {
    use filmcraft_codecs::hw::Av1ParameterDifference;
    let mut run = Av1RunMetadata::new(stream(&Sequence::default()));
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    let accepted_color = run.active().color;
    let tools = Sequence { large_superblock: true, ..pq() };
    let restart = sample(&tools);
    assert_eq!(
        run.active().av1_parameter_difference(&restart).unwrap(),
        Av1ParameterDifference::Unchanged,
        "coding-tool-only difference retains existing format acceptance"
    );
    assert!(run.prepare(&restart, 0, |_| panic!("same native format/color does not open new color adoption")).unwrap());
    assert_eq!(run.active().color, accepted_color);
    assert_eq!(
        run.software_replay_initialization(),
        Some(obu(1, &payload(&tools)).as_slice()),
        "replay must retain the latest accepted sequence, including coding-tool flags"
    );
    let hlg = Sequence { color: (9, 18, 9), ..tools };
    assert!(!run.prepare(&sample(&hlg), 0, |_| true).unwrap(), "metadata refresh must not reopen initial color window");
}

#[test]
fn av1_run_changed_coding_tools_after_adoption_require_supported_restart_proof() {
    let original = Sequence::default();
    let mut ordinary = Av1RunMetadata::new(stream(&original));
    let ordinary_tools = Sequence { large_superblock: true, ..original };
    assert!(ordinary.prepare(&sample(&ordinary_tools), 0, |_| panic!("ordinary coding-tool acceptance needs no color adoption")).unwrap());
    assert!(ordinary.software_replay_initialization().is_none());
    let mut run = Av1RunMetadata::new(stream(&Sequence::default()));
    assert!(run.prepare(&sample(&pq()), 0, |_| true).unwrap());
    let accepted = run.software_replay_initialization().unwrap().to_vec();
    let tools = Sequence { large_superblock: true, ..pq() };
    for frame in [vec![0x30], vec![0x80], vec![0]] {
        let mut unsupported = obu(1, &payload(&tools));
        unsupported.extend(obu(6, &frame));
        assert!(!run.prepare(&unsupported, 0, |_| panic!("unsupported refresh cannot reopen adoption")).unwrap());
        assert_eq!(run.software_replay_initialization(), Some(accepted.as_slice()));
    }
    let mut truncated = obu(1, &payload(&tools));
    truncated.extend(obu(6, &[0x19]));
    assert!(run.prepare(&truncated, 0, |_| true).is_err());
    assert_eq!(run.software_replay_initialization(), Some(accepted.as_slice()));
    let mut metadata = sample(&tools);
    metadata.extend(obu(5, &[1, 128]));
    assert!(!run.prepare(&metadata, 0, |_| true).unwrap());
    assert_eq!(run.software_replay_initialization(), Some(accepted.as_slice()));
    assert!(run.prepare(&sample(&pq()), 4, |_| panic!("identical accepted header remains idempotent")).unwrap());
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
