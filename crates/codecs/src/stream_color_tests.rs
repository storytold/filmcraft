//! Colour from the bitstream when the container has none (`crate::stream_color`): ffmpeg writes MP4
//! without a `colr` box by default, and an HEVC PQ file imported as Rec. 709 SDR, a full-range
//! H.264 file as limited range, and a BT.601 SD file with BT.709 primaries. The parameter sets
//! are built here bit by bit; no media files.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_bitstream::{BitWriter, escape_rbsp};
use filmcraft_color::{ColorInfo, ColorSpace, Matrix, Primaries, Range, Transfer};
use filmcraft_isobmff::{Av1Config, AvcConfig, Brand, CodecConfig, HevcConfig, HevcNalArray, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_media::MediaSource;

use crate::stream_color::{ColorCodes, from_codec_config, resolve};

/// The video signal description of a VUI: (colour_primaries, transfer_characteristics,
/// matrix_coefficients) when `colour_description_present_flag` is set, and the range flag.
#[derive(Clone, Copy)]
struct Signal {
    codes: Option<(u8, u8, u8)>,
    full_range: bool,
}

const PQ_2020: Signal = Signal { codes: Some((9, 16, 9)), full_range: false };
const SMPTE_170M: Signal = Signal { codes: Some((6, 6, 6)), full_range: false };
const FULL_ONLY: Signal = Signal { codes: None, full_range: true };

fn signal(w: &mut BitWriter, s: Signal) {
    w.write_bit(true); // video_signal_type_present_flag
    w.write_bits(5, 3); // video_format: unspecified
    w.write_bit(s.full_range);
    w.write_bit(s.codes.is_some()); // colour_description_present_flag
    if let Some((p, t, m)) = s.codes {
        w.write_bits(p.into(), 8);
        w.write_bits(t.into(), 8);
        w.write_bits(m.into(), 8);
    }
}

/// A Baseline H.264 SPS NAL unit (ITU-T H.264 §7.3.2.1.1, VUI per Annex E.1.1) of
/// `mbs_w`×`mbs_h` macroblocks, with a VUI carrying `vui` (no VUI when `None`).
fn h264_sps(mbs_w: u32, mbs_h: u32, vui: Option<Signal>) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(66, 8); // profile_idc: Baseline
    w.write_bits(0, 8); // constraint flags
    w.write_bits(30, 8); // level_idc
    w.write_ue(0); // seq_parameter_set_id
    w.write_ue(0); // log2_max_frame_num_minus4
    w.write_ue(2); // pic_order_cnt_type
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(mbs_w - 1);
    w.write_ue(mbs_h - 1);
    w.write_bits(0b110, 3); // frame_mbs_only, direct_8x8_inference, no cropping
    w.write_bit(vui.is_some()); // vui_parameters_present_flag
    if let Some(s) = vui {
        w.write_bit(false); // aspect_ratio_info_present_flag
        w.write_bit(false); // overscan_info_present_flag
        signal(&mut w, s);
        w.write_bit(false); // chroma_loc_info_present_flag
        w.write_bit(false); // timing_info_present_flag
        w.write_bit(false); // nal_hrd_parameters_present_flag
        w.write_bit(false); // vcl_hrd_parameters_present_flag
        w.write_bit(false); // pic_struct_present_flag
        w.write_bit(false); // bitstream_restriction_flag
    }
    w.rbsp_trailing();
    let mut nal = vec![0x67];
    nal.extend(escape_rbsp(&w.finish()));
    nal
}

/// A Main 10 HEVC SPS NAL unit (ITU-T H.265 §7.3.2.2, VUI per Annex E.2.1) of `width`×`height`
/// (multiples of 8), with a VUI carrying `vui` (no VUI when `None`).
fn hevc_sps(width: u32, height: u32, vui: Option<Signal>) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 4); // sps_video_parameter_set_id
    w.write_bits(0, 3); // sps_max_sub_layers_minus1
    w.write_bit(true); // sps_temporal_id_nesting_flag
    // profile_tier_level: Main 10, level 4.1
    w.write_bits(0, 2);
    w.write_bit(false);
    w.write_bits(2, 5);
    w.write_bits(0x2000_0000, 32);
    w.write_bits(0b1001, 4);
    w.write_bits(0, 32);
    w.write_bits(0, 12);
    w.write_bits(123, 8);
    w.write_ue(0); // sps_seq_parameter_set_id
    w.write_ue(1); // chroma_format_idc 4:2:0
    w.write_ue(width);
    w.write_ue(height);
    w.write_bit(false); // conformance_window_flag
    w.write_ue(2); // bit_depth_luma_minus8
    w.write_ue(2); // bit_depth_chroma_minus8
    w.write_ue(4); // log2_max_pic_order_cnt_lsb_minus4
    w.write_bit(true); // sps_sub_layer_ordering_info_present_flag
    w.write_ue(4); // sps_max_dec_pic_buffering_minus1
    w.write_ue(0); // sps_max_num_reorder_pics
    w.write_ue(0); // sps_max_latency_increase_plus1
    w.write_ue(0); // log2_min_luma_coding_block_size_minus3 (8)
    w.write_ue(1); // log2_diff_max_min_luma_coding_block_size (CTB 16)
    w.write_ue(0); // log2_min_luma_transform_block_size_minus2 (4)
    w.write_ue(2); // log2_diff_max_min_luma_transform_block_size (16)
    w.write_ue(0); // max_transform_hierarchy_depth_inter
    w.write_ue(0); // max_transform_hierarchy_depth_intra
    w.write_bit(false); // scaling_list_enabled_flag
    w.write_bit(false); // amp_enabled_flag
    w.write_bit(false); // sample_adaptive_offset_enabled_flag
    w.write_bit(false); // pcm_enabled_flag
    w.write_ue(0); // num_short_term_ref_pic_sets
    w.write_bit(false); // long_term_ref_pics_present_flag
    w.write_bit(false); // sps_temporal_mvp_enabled_flag
    w.write_bit(false); // strong_intra_smoothing_enabled_flag
    w.write_bit(vui.is_some()); // vui_parameters_present_flag
    if let Some(s) = vui {
        w.write_bit(false); // aspect_ratio_info_present_flag
        w.write_bit(false); // overscan_info_present_flag
        signal(&mut w, s);
        w.write_bit(false); // chroma_loc_info_present_flag
        w.write_bit(false); // neutral_chroma_indication_flag
        w.write_bit(false); // field_seq_flag
        w.write_bit(false); // frame_field_info_present_flag
        w.write_bit(false); // default_display_window_flag
        w.write_bit(false); // vui_timing_info_present_flag
        w.write_bit(false); // bitstream_restriction_flag
    }
    w.write_bit(false); // sps_extension_present_flag
    w.rbsp_trailing();
    let mut nal = vec![33 << 1, 1];
    nal.extend(escape_rbsp(&w.finish()));
    nal
}

/// An AV1 sequence header OBU (AV1 bitstream spec §5.5; reduced still-picture header, profile 0,
/// 10-bit 4:2:0) of `width`×`height` with `color_config` signalling `codes` and `full_range`.
fn av1_sequence_header(width: u32, height: u32, codes: Option<(u8, u8, u8)>, full_range: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 3); // seq_profile
    w.write_bit(true); // still_picture
    w.write_bit(true); // reduced_still_picture_header
    w.write_bits(0, 5); // seq_level_idx[0]
    w.write_bits(15, 4); // frame_width_bits_minus_1
    w.write_bits(15, 4); // frame_height_bits_minus_1
    w.write_bits(width - 1, 16);
    w.write_bits(height - 1, 16);
    w.write_bits(0, 3); // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
    w.write_bits(0, 3); // enable_superres, enable_cdef, enable_restoration
    w.write_bit(true); // high_bitdepth
    w.write_bit(false); // mono_chrome
    w.write_bit(codes.is_some()); // color_description_present_flag
    if let Some((p, t, m)) = codes {
        w.write_bits(p.into(), 8);
        w.write_bits(t.into(), 8);
        w.write_bits(m.into(), 8);
    }
    w.write_bit(full_range); // color_range
    w.write_bits(0, 2); // chroma_sample_position
    w.write_bit(false); // separate_uv_delta_q
    w.write_bit(false); // film_grain_params_present
    w.rbsp_trailing();
    let payload = w.finish();
    // OBU header: type 1 (OBU_SEQUENCE_HEADER), obu_has_size_field; one-byte leb128 size
    let mut obu = vec![(1 << 3) | 2, u8::try_from(payload.len()).expect("short payload")];
    obu.extend(payload);
    obu
}

fn avc(sps: Vec<u8>) -> CodecConfig {
    CodecConfig::Avc(AvcConfig::new(vec![sps], vec![vec![0x68, 0xCE, 0x38, 0x80]], 4))
}

fn hevc(sps: Vec<u8>) -> CodecConfig {
    CodecConfig::Hevc(HevcConfig {
        general_profile_idc: 2,
        chroma_format_idc: 1,
        bit_depth_luma: 10,
        bit_depth_chroma: 10,
        length_size: 4,
        arrays: vec![HevcNalArray { completeness: true, nal_type: 33, nalus: vec![sps] }],
        ..Default::default()
    })
}

fn av1(obus: Vec<u8>) -> CodecConfig {
    CodecConfig::Av1(Av1Config { high_bitdepth: true, chroma_subsampling_x: true, chroma_subsampling_y: true, config_obus: obus, ..Default::default() })
}

/// A one-sample MP4 with `codec` (the sample is never decoded) and, when given, a `colr` box.
fn mp4(codec: CodecConfig, w: u16, h: u16, colr: Option<filmcraft_isobmff::ColorInfo>) -> Arc<[u8]> {
    let mut entry = match codec {
        CodecConfig::Avc(c) => SampleEntry::avc(c, w, h),
        CodecConfig::Hevc(c) => SampleEntry::hevc(c, w, h),
        other => SampleEntry::video(filmcraft_isobmff::FourCc(*b"av01"), other, w, h),
    };
    if let Some(v) = entry.video.as_mut() {
        v.color = colr;
    }
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).expect("writer");
    let t = mux.add_track(TrackConfig::new(entry, 30)).expect("track");
    mux.write_sample(t, WriteSample { data: &[0, 0, 0, 1, 0x65], duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    mux.finish().expect("finish").into_inner().into()
}

fn mp4_color(codec: CodecConfig, w: u16, h: u16, colr: Option<filmcraft_isobmff::ColorInfo>) -> ColorInfo {
    let src = crate::Mp4Source::open("clip.mp4", mp4(codec, w, h, colr)).expect("open");
    src.info().video.as_ref().expect("video").color
}

#[test]
fn hevc_pq_without_colr_is_rec2100_pq() {
    let c = mp4_color(hevc(hevc_sps(1920, 1080, Some(PQ_2020))), 1920, 1080, None);
    assert_eq!(c, ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Pq, primaries: Primaries::Bt2020, range: Range::Limited });
    assert_eq!(ColorSpace::from_info(&c), ColorSpace::Rec2100Pq);
    assert!(ColorSpace::from_info(&c).is_hdr());
}

#[test]
fn full_range_h264_without_colr_is_full_range() {
    // video_full_range_flag with no colour description: full range, the rest by size
    let c = mp4_color(avc(h264_sps(80, 45, Some(FULL_ONLY))), 1280, 720, None);
    assert_eq!(c, ColorInfo { range: Range::Full, ..ColorInfo::REC709 });
}

#[test]
fn bt601_sd_h264_without_colr_has_bt601_primaries() {
    let c = mp4_color(avc(h264_sps(45, 30, Some(SMPTE_170M))), 720, 480, None);
    assert_eq!(c, ColorInfo { matrix: Matrix::Bt601, transfer: Transfer::Bt709, primaries: Primaries::Bt601_525, range: Range::Limited });
    // PAL (BT.470 BG primaries, BT.601 matrix)
    let pal = Signal { codes: Some((5, 6, 5)), full_range: false };
    let c = mp4_color(avc(h264_sps(45, 36, Some(pal))), 720, 576, None);
    assert_eq!((c.primaries, c.matrix), (Primaries::Bt601_625, Matrix::Bt601));
}

#[test]
fn unspecified_or_missing_vui_keeps_the_size_defaults() {
    let unspecified = Signal { codes: Some((2, 2, 2)), full_range: false };
    for vui in [None, Some(unspecified)] {
        assert_eq!(mp4_color(avc(h264_sps(80, 45, vui)), 1280, 720, None), ColorInfo::REC709);
        assert_eq!(mp4_color(hevc(hevc_sps(1920, 1080, vui)), 1920, 1080, None), ColorInfo::REC709);
        // SD: the BT.601 matrix, as before
        assert_eq!(mp4_color(avc(h264_sps(45, 30, vui)), 720, 480, None), ColorInfo { matrix: Matrix::Bt601, ..ColorInfo::REC709 });
    }
}

#[test]
fn colr_wins_over_the_bitstream() {
    use filmcraft_isobmff::ColorInfo::{Nclc, Nclx};
    let sps = || hevc(hevc_sps(1920, 1080, Some(PQ_2020)));
    // a BT.709 nclx box beats a PQ VUI, and is what the decoder's frames are tagged with
    let colr = Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false };
    assert_eq!(mp4_color(sps(), 1920, 1080, Some(colr.clone())), ColorInfo::REC709);
    // HLG in colr, PQ in the VUI: HLG
    let hlg = Nclx { primaries: 9, transfer: 18, matrix: 9, full_range: false };
    assert_eq!(mp4_color(sps(), 1920, 1080, Some(hlg)).transfer, Transfer::Hlg);
    // limited range in colr beats a full-range VUI
    assert_eq!(mp4_color(avc(h264_sps(80, 45, Some(FULL_ONLY))), 1280, 720, Some(colr)).range, Range::Limited);
    // what colr leaves unspecified comes from the VUI
    let partial = Nclx { primaries: 2, transfer: 16, matrix: 2, full_range: false };
    assert_eq!(mp4_color(sps(), 1920, 1080, Some(partial)), mp4_color(sps(), 1920, 1080, None));
    // QuickTime nclc has no range flag: the VUI's range stands
    let nclc = Nclc { primaries: 1, transfer: 1, matrix: 1 };
    assert_eq!(mp4_color(avc(h264_sps(80, 45, Some(FULL_ONLY))), 1280, 720, Some(nclc)).range, Range::Full);
    // an ICC profile has no code points: the bitstream decides
    let icc = filmcraft_isobmff::ColorInfo::Icc { kind: filmcraft_isobmff::FourCc(*b"prof"), profile: vec![0; 8] };
    assert_eq!(mp4_color(sps(), 1920, 1080, Some(icc)).transfer, Transfer::Pq);
}

#[test]
fn av1_sequence_header_colour() {
    let obu = av1_sequence_header(1920, 1080, Some((9, 16, 9)), false);
    assert_eq!(from_codec_config(&av1(obu.clone())), Some(ColorCodes { primaries: 9, transfer: 16, matrix: 9, full_range: Some(false) }));
    let c = mp4_color(av1(obu), 1920, 1080, None);
    assert_eq!(ColorSpace::from_info(&c), ColorSpace::Rec2100Pq);
    // no colour description: unspecified code points, the range flag still counts
    let obu = av1_sequence_header(640, 360, None, true);
    assert_eq!(from_codec_config(&av1(obu)), Some(ColorCodes { primaries: 2, transfer: 2, matrix: 2, full_range: Some(true) }));
}

#[test]
fn matroska_without_colour_uses_the_bitstream() {
    let CodecConfig::Hevc(hvcc) = hevc(hevc_sps(1920, 1080, Some(PQ_2020))) else { panic!("an hvcC") };
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MPEGH/ISO/HEVC");
    spec.codec_private = hvcc.to_bytes();
    spec.video_size = Some((1920, 1080));
    spec.default_duration_ns = Some(33_333_333);
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], MuxOptions::default()).expect("writer");
    w.write_frame(0, 0, true, &[0, 0, 0, 1, 0x26], None).expect("frame");
    let b: Arc<[u8]> = w.finish().expect("finish").into_inner().into();
    let src = crate::MkvSource::open("clip.mkv", b).expect("open");
    let c = src.info().video.as_ref().expect("video").color;
    assert_eq!(ColorSpace::from_info(&c), ColorSpace::Rec2100Pq);
    assert_eq!((c.primaries, c.matrix), (Primaries::Bt2020, Matrix::Bt2020Ncl));
}

#[test]
fn resolve_takes_each_field_from_the_first_source_that_knows_it() {
    let colr = ColorCodes { primaries: 1, transfer: 2, matrix: 0, full_range: None };
    let vui = ColorCodes { primaries: 9, transfer: 18, matrix: 9, full_range: Some(true) };
    // matrix 0 (identity) is not one we know: the VUI's BT.2020 is used
    let c = resolve(1920, 1080, &[colr, vui]);
    assert_eq!(c, ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Hlg, primaries: Primaries::Bt709, range: Range::Full });
    assert_eq!(resolve(720, 480, &[]), ColorInfo { matrix: Matrix::Bt601, ..ColorInfo::REC709 });
    // code points that don't fit a byte are unspecified, not truncated (0x109 is not 9)
    assert_eq!(ColorCodes::from_wide(0x109, 0x110, 0x109, None), ColorCodes { primaries: 2, transfer: 2, matrix: 2, full_range: None });
}

/// Every truncation and many bit flips of each configuration: never a panic, and the result is
/// either some colour or none (the size defaults).
#[test]
fn hostile_parameter_sets_never_panic() {
    let configs: Vec<(Vec<u8>, fn(Vec<u8>) -> CodecConfig)> = vec![
        (h264_sps(45, 30, Some(SMPTE_170M)), avc),
        (hevc_sps(1920, 1080, Some(PQ_2020)), hevc),
        (av1_sequence_header(1920, 1080, Some((9, 16, 9)), false), av1),
    ];
    for (good, make) in configs {
        assert!(from_codec_config(&make(good.clone())).is_some());
        let mut variants: Vec<Vec<u8>> = (0..good.len()).map(|n| good[..n].to_vec()).collect();
        for bit in 0..good.len() * 8 {
            let mut b = good.clone();
            b[bit / 8] ^= 0x80 >> (bit % 8);
            variants.push(b);
        }
        variants.push(vec![0xff; 64]);
        variants.push(Vec::new());
        for v in variants {
            let r = std::panic::catch_unwind(|| from_codec_config(&make(v.clone())));
            assert!(r.is_ok(), "panicked on {v:02x?}");
        }
    }
    // configurations with no parameter sets at all
    assert_eq!(from_codec_config(&CodecConfig::Avc(AvcConfig::default())), None);
    assert_eq!(from_codec_config(&CodecConfig::Hevc(HevcConfig::default())), None);
    assert_eq!(from_codec_config(&CodecConfig::Av1(Av1Config::default())), None);
    // a garbage SPS imports with the size defaults
    assert_eq!(mp4_color(avc(vec![0x67, 0xff, 0xff]), 1280, 720, None), ColorInfo::REC709);
    assert_eq!(mp4_color(hevc(vec![33 << 1, 1, 0xff]), 1920, 1080, None), ColorInfo::REC709);
}
