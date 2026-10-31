use super::*;
use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_media::FrameRequest;

/// A one-frame 64×32 ProRes MOV (bright left half, dark right half) whose `tkhd` matrix is
/// replaced by `matrix` — built here, no media files.
fn rotated_mov(matrix: [i32; 9]) -> Arc<[u8]> {
    let (w, h) = (64u32, 32u32);
    let mut fr = filmcraft_prores::Frame::new(w, h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
    for (i, y) in fr.y.iter_mut().enumerate() {
        *y = if (i as u32 % w) < w / 2 { 800 } else { 100 };
    }
    let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h).encode(&fr).expect("encode");
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let t = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), w as u16, h as u16), 25)).expect("track");
    mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    let mut b = mux.finish().expect("finish").into_inner();
    // tkhd (ISO/IEC 14496-12 §8.3.2): version/flags, then 20 (v0) or 32 (v1) bytes of
    // times/id/duration, 8 reserved, layer, alternate_group, volume, reserved, matrix
    let k = b.windows(4).position(|x| x == b"tkhd").expect("tkhd") + 4;
    let m = k + 4 + if b[k] == 1 { 32 } else { 20 } + 16;
    for (j, v) in matrix.iter().enumerate() {
        b[m + j * 4..m + j * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    b.into()
}

/// A four-frame 25 fps ProRes MOV (64×32) whose video track carries `edits`.
fn mov_with_edits(edits: Vec<filmcraft_isobmff::Edit>) -> Arc<[u8]> {
    let fr = filmcraft_prores::Frame::new(64, 32, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
    let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, 64, 32).encode(&fr).expect("encode");
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let mut cfg = TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), 64, 32), 25);
    cfg.edits = edits;
    let t = mux.add_track(cfg).expect("track");
    for _ in 0..4 {
        mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    }
    mux.finish().expect("finish").into_inner().into()
}

/// A clip lasts as long as its edit list presents it, not as long as its media (`mdhd`) runs.
/// Every ffmpeg / x264 MP4 with B-frames skips its reorder delay with an edit and imported one
/// frame too long; a video track delayed by an empty edit lost its last frame.
#[test]
fn duration_follows_the_edit_list() {
    use filmcraft_isobmff::Edit;
    let frames = |s: &Mp4Source| s.info().duration.to_rational_floor(1, 25);
    // no edit list: the media duration
    assert_eq!(frames(&Mp4Source::open("plain.mov", mov_with_edits(Vec::new())).expect("open")), 4);
    // skip the first frame (B-frame delay style): three frames play (movie timescale 1000 ms)
    let skip = vec![Edit { segment_duration: 120, media_time: 1, media_rate: 0x10000 }];
    assert_eq!(frames(&Mp4Source::open("skip.mov", mov_with_edits(skip)).expect("open")), 3);
    // a one-frame empty edit, then all four frames: five frames, and the last one is there
    let delayed = vec![Edit { segment_duration: 40, media_time: -1, media_rate: 0x10000 }, Edit { segment_duration: 160, media_time: 0, media_rate: 0x10000 }];
    let s = Mp4Source::open("delayed.mov", mov_with_edits(delayed)).expect("open");
    assert_eq!(frames(&s), 5);
    assert!(s.video_frame(FrameRequest::full(Tick::from_rational(4, 1, 25))).is_ok());
    // hostile segment durations fall back to the media duration instead of wrapping
    let huge = vec![Edit { segment_duration: u64::MAX, media_time: 0, media_rate: 0x10000 }];
    assert_eq!(frames(&Mp4Source::open("huge.mov", mov_with_edits(huge)).expect("open")), 4);
    let zero = vec![Edit { segment_duration: 0, media_time: 0, media_rate: 0x10000 }];
    assert_eq!(frames(&Mp4Source::open("zero.mov", mov_with_edits(zero)).expect("open")), 4);
}

/// #714: a video track without an edit list whose pictures carry a composition delay (B-frame
/// style, as x264 writes them with no `elst`) starts at its first picture. Media time zero
/// repeated picture 0 for the length of the delay and the last pictures never showed.
#[test]
fn unedited_composition_delay_starts_at_the_first_picture() {
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let t = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apch"), 64, 32), 25)).expect("track");
    for k in 0..4u16 {
        let mut fr = filmcraft_prores::Frame::new(64, 32, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
        fr.y.fill(100 + 200 * k);
        let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, 64, 32).encode(&fr).expect("encode");
        mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 2, is_sync: true }).expect("sample");
    }
    let src = Mp4Source::open("delay.mov", mux.finish().expect("finish").into_inner().into()).expect("open");
    assert_eq!(src.info().duration, Tick::from_rational(4, 1, 25));
    let luma = |k: i64| {
        let f = src.video_frame(FrameRequest::full(Tick::from_rational(k, 1, 25))).expect("frame");
        let l = f.luma8();
        l.iter().map(|&v| u32::from(v)).sum::<u32>() / l.len().max(1) as u32
    };
    let seen: Vec<u32> = (0..4).map(luma).collect();
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "one picture per frame, in order: {seen:?}");
}

/// Overwrite the big-endian u32 `skip` bytes after the first `fourcc` box type.
fn patch_u32(b: &mut [u8], fourcc: &[u8; 4], skip: usize, v: u32) {
    let k = b.windows(4).position(|x| x == fourcc).expect("box") + 4 + skip;
    b[k..k + 4].copy_from_slice(&v.to_be_bytes());
}

/// Damaged MOVs found by mutation fuzzing panicked on import: a track with timescale 0
/// ("attempt to divide by zero" in the duration) and one whose sample description list is
/// empty ("index out of bounds" on `entries[0]`). Both must open as an error, or as media
/// that answers requests without panicking.
#[test]
fn damaged_track_headers_never_panic() {
    let identity = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];
    // mdhd v0: version/flags, creation, modification, then timescale
    let mut zero_timescale = rotated_mov(identity).to_vec();
    patch_u32(&mut zero_timescale, b"mdhd", 12, 0);
    // stsd: version/flags, then entry_count
    let mut no_entries = rotated_mov(identity).to_vec();
    patch_u32(&mut no_entries, b"stsd", 4, 0);
    for (name, b) in [("ts0.mov", zero_timescale), ("stsd0.mov", no_entries)] {
        if let Ok(s) = Mp4Source::open(name, b.into()) {
            let _ = s.video_frame(FrameRequest::full(Tick::ZERO));
            let _ = s.audio(0, 1024, 48_000);
        }
    }
}

/// Mean luma of the top and bottom halves of the decoded frame, and its size.
fn halves(src: &Mp4Source) -> (u32, u32, u8, u8) {
    let f = src.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
    let l = f.luma8();
    let half = l.len() / 2;
    let mean = |s: &[u8]| (s.iter().map(|&v| v as u32).sum::<u32>() / s.len() as u32) as u8;
    (f.width, f.height, mean(&l[..half]), mean(&l[half..]))
}

#[test]
fn display_matrix_rotates_frames_and_reported_size() {
    const ONE: i32 = 0x10000;
    const W: i32 = 0x4000_0000;
    // identity: landscape 64x32, top and bottom alike
    let s = Mp4Source::open("id.mov", rotated_mov([ONE, 0, 0, 0, ONE, 0, 0, 0, W])).expect("open");
    let v = s.info().video.as_ref().expect("video");
    assert_eq!((v.width, v.height), (64, 32));
    let (w, h, top, bottom) = halves(&s);
    assert_eq!((w, h), (64, 32));
    assert!(top.abs_diff(bottom) < 4);

    // 90° clockwise (what an iPhone writes for portrait): the left half ends up on top
    let s = Mp4Source::open("cw.mov", rotated_mov([0, ONE, 0, -ONE, 0, 0, 32 * ONE, 0, W])).expect("open");
    let v = s.info().video.as_ref().expect("video");
    assert_eq!((v.width, v.height), (32, 64));
    let (w, h, top, bottom) = halves(&s);
    assert_eq!((w, h), (32, 64));
    assert!(top > bottom + 100, "top {top} bottom {bottom}");

    // 270° clockwise: the left half ends up at the bottom
    let s = Mp4Source::open("ccw.mov", rotated_mov([0, -ONE, 0, ONE, 0, 0, 0, 64 * ONE, W])).expect("open");
    let (w, h, top, bottom) = halves(&s);
    assert_eq!((w, h), (32, 64));
    assert!(bottom > top + 100, "top {top} bottom {bottom}");

    // 180°: still landscape
    let s = Mp4Source::open("180.mov", rotated_mov([-ONE, 0, 0, 0, -ONE, 0, 64 * ONE, 32 * ONE, W])).expect("open");
    assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)));
}

/// A one-frame 25 fps ProRes MOV (64×32, left half bright) with clean aperture `clap`.
fn clap_mov(clap: filmcraft_isobmff::CleanAperture) -> Arc<[u8]> {
    let (w, h) = (64u32, 32u32);
    let mut fr = filmcraft_prores::Frame::new(w, h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
    for (i, y) in fr.y.iter_mut().enumerate() {
        *y = if (i as u32 % w) < w / 2 { 800 } else { 100 };
    }
    let data = filmcraft_prores::Encoder::new(filmcraft_prores::Profile::Hq, w, h).encode(&fr).expect("encode");
    let mut entry = SampleEntry::prores(FourCc(*b"apch"), w as u16, h as u16);
    entry.video.as_mut().expect("video").clean_aperture = Some(clap);
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let t = mux.add_track(TrackConfig::new(entry, 25)).expect("track");
    mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    mux.finish().expect("finish").into_inner().into()
}

#[test]
fn clean_aperture_crops_frames_and_reported_size() {
    use filmcraft_isobmff::CleanAperture;
    let clap = |w: (u32, u32), h: (u32, u32), dx: (i32, u32), dy: (i32, u32)| CleanAperture { width: w, height: h, horiz_offset: dx, vert_offset: dy };
    // 32x16 moved 8 left of centre: columns 8..40 (24 bright, then 8 dark), rows 8..24
    let s = Mp4Source::open("clap.mov", clap_mov(clap((32, 1), (16, 1), (-8, 1), (0, 1)))).expect("open");
    assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((32, 16)));
    let f = s.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
    assert_eq!((f.width, f.height), (32, 16));
    let l = f.luma8();
    let col = |x: usize| (0..16).map(|y| u32::from(l[y * 32 + x])).sum::<u32>() / 16;
    assert!(col(0) > col(31) + 100 && col(23) > col(24) + 100, "bright up to column 23: {} {} {} {}", col(0), col(23), col(24), col(31));

    // rational sizes and offsets (64/2 = 32): the same rectangle
    let s = Mp4Source::open("clap2.mov", clap_mov(clap((64, 2), (48, 3), (-16, 2), (0, 7)))).expect("open");
    assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((32, 16)));

    // a clean aperture that makes no sense is ignored (the whole picture), never a panic
    for c in [
        clap((0, 1), (16, 1), (0, 1), (0, 1)),
        clap((32, 0), (16, 1), (0, 1), (0, 1)),
        clap((32, 1), (16, 1), (0, 0), (0, 1)),
        clap((65, 1), (16, 1), (0, 1), (0, 1)),
        clap((32, 1), (33, 1), (0, 1), (0, 1)),
        clap((32, 1), (16, 1), (17, 1), (0, 1)),
        clap((32, 1), (16, 1), (0, 1), (-9, 1)),
        clap((32, 1), (16, 1), (i32::MIN, 1), (i32::MAX, 1)),
        clap((u32::MAX, 1), (u32::MAX, 1), (0, 1), (0, 1)),
        clap((1, u32::MAX), (16, 1), (0, u32::MAX), (0, 1)),
        clap((64, 1), (32, 1), (0, 1), (0, 1)),
    ] {
        let s = Mp4Source::open("bad.mov", clap_mov(c)).expect("open");
        assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)), "{c:?}");
        let f = s.video_frame(FrameRequest::full(Tick::ZERO)).expect("frame");
        assert_eq!((f.width, f.height), (64, 32), "{c:?}");
    }
}

#[test]
fn mirrored_or_scaled_matrices_are_not_mistaken_for_rotations() {
    const ONE: i32 = 0x10000;
    const W: i32 = 0x4000_0000;
    // horizontal mirror: not a pure rotation, so the frame is left as stored
    let s = Mp4Source::open("mirror.mov", rotated_mov([-ONE, 0, 0, 0, ONE, 0, 64 * ONE, 0, W])).expect("open");
    let (w, h, top, bottom) = halves(&s);
    assert_eq!((w, h), (64, 32));
    assert!(top.abs_diff(bottom) < 4);
    // a 2× scaled identity is still upright (only the signs decide the rotation)
    let s = Mp4Source::open("scaled.mov", rotated_mov([2 * ONE, 0, 0, 0, 2 * ONE, 0, 0, 0, W])).expect("open");
    assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)));
    // a scaled quarter turn is still a quarter turn
    let s = Mp4Source::open("cw2.mov", rotated_mov([0, 2 * ONE, 0, -2 * ONE, 0, 0, 64 * ONE, 0, W])).expect("open");
    assert_eq!(s.info().video.as_ref().map(|v| (v.width, v.height)), Some((32, 64)));
}

#[test]
fn hevc_pixel_format_comes_from_hvcc() {
    for (chroma, bits, want) in [(1, 8, "YUV 4:2:0 8-bit"), (1, 10, "YUV 4:2:0 10-bit"), (2, 8, "YUV 4:2:2 8-bit"), (2, 10, "YUV 4:2:2 10-bit")] {
        let c = filmcraft_isobmff::HevcConfig { general_profile_idc: 1, chroma_format_idc: chroma, bit_depth_luma: bits, ..Default::default() };
        assert_eq!(pixfmt_label(&CodecConfig::Hevc(c)), want);
    }
}

/// A High-profile SPS NAL unit (ITU-T H.264 §7.3.2.1.1) with the given chroma format and bit depth, 16×16.
fn high_sps(profile: u8, chroma: u32, bits: u32) -> Vec<u8> {
    let mut w = filmcraft_bitstream::BitWriter::new();
    w.write_bits(0x67, 8);
    w.write_bits(profile as u32, 8);
    w.write_bits(0, 8); // constraint flags
    w.write_bits(30, 8); // level
    w.write_ue(0); // seq_parameter_set_id
    w.write_ue(chroma);
    if chroma == 3 {
        w.write_bit(false); // separate_colour_plane_flag
    }
    w.write_ue(bits - 8); // luma
    w.write_ue(bits - 8); // chroma
    w.write_bits(0, 2); // no transform bypass, no scaling matrices
    w.write_ue(0); // log2_max_frame_num_minus4
    w.write_ue(2); // pic_order_cnt_type
    w.write_ue(1); // max_num_ref_frames
    w.write_bit(false); // gaps_in_frame_num_allowed
    w.write_ue(0); // pic_width_in_mbs_minus1
    w.write_ue(0); // pic_height_in_map_units_minus1
    w.write_bits(0b110, 3); // frame_mbs_only, direct_8x8_inference, no cropping
    w.write_bit(false); // no VUI
    w.rbsp_trailing();
    w.finish()
}

#[test]
fn avc_pixel_format_comes_from_avcc_not_the_profile() {
    let avc = |profile: u8, sps: Vec<u8>, ext: Vec<u8>| CodecConfig::Avc(filmcraft_isobmff::AvcConfig { profile, sps: vec![sps], ext, ..Default::default() });
    // High 4:2:2 at 10 and 8 bits, from the avcC extension (chroma_format 2, bit_depth_luma_minus8)
    assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 10), vec![0xFE, 0xFA, 0xFA, 0])), "YUV 4:2:2 10-bit");
    assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 8), vec![0xFE, 0xF8, 0xF8, 0])), "YUV 4:2:2 8-bit");
    // no extension (or not a valid one): the SPS says
    assert_eq!(pixfmt_label(&avc(122, high_sps(122, 2, 10), Vec::new())), "YUV 4:2:2 10-bit");
    assert_eq!(pixfmt_label(&avc(110, high_sps(110, 1, 10), vec![0, 0, 0, 0])), "YUV 4:2:0 10-bit");
    assert_eq!(pixfmt_label(&avc(244, high_sps(244, 3, 10), Vec::new())), "YUV 4:4:4 10-bit");
    // an unreadable SPS and no extension: unknown, not a guess
    assert_eq!(pixfmt_label(&avc(122, vec![0x67, 122], Vec::new())), "");
    // the SPS outranks the extension: an avcC claiming 4:0:0 for a 4:2:0 stream labels 4:2:0 (#1959)
    assert_eq!(pixfmt_label(&avc(100, high_sps(100, 1, 8), vec![0xFC, 0xF8, 0xF8, 0])), "YUV 4:2:0 8-bit");
    // only without a readable SPS does the extension speak for itself
    assert_eq!(pixfmt_label(&avc(100, vec![0x67, 100], vec![0xFC, 0xF8, 0xF8, 0])), "YUV 4:0:0 8-bit");
}

/// An HEVC Main SPS NAL unit (ITU-T H.265 §7.3.2.2) with the given chroma format and bit depth, 64×64.
fn hevc_sps(chroma: u32, bits: u32) -> Vec<u8> {
    let mut w = filmcraft_bitstream::BitWriter::new();
    w.write_bits(0, 4); // vps id
    w.write_bits(0, 3); // max_sub_layers_minus1
    w.write_bits(1, 1); // temporal_id_nesting
    // profile_tier_level: Main, level 3.1
    w.write_bits(0, 2);
    w.write_bits(0, 1);
    w.write_bits(1, 5);
    w.write_bits(0x6000_0000, 32);
    w.write_bits(0b1001, 4);
    w.write_bits(0, 32);
    w.write_bits(0, 11);
    w.write_bits(0, 1);
    w.write_bits(93, 8);
    w.write_ue(0); // sps id
    w.write_ue(chroma);
    if chroma == 3 {
        w.write_bit(false); // separate_colour_plane
    }
    w.write_ue(64);
    w.write_ue(64);
    w.write_bits(0, 1); // conformance window
    w.write_ue(bits - 8); // luma
    w.write_ue(bits - 8); // chroma
    w.write_ue(4); // log2_max_poc_lsb_minus4
    w.write_bits(1, 1); // sub_layer_ordering_info_present
    w.write_ue(4);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(0); // min CB 8
    w.write_ue(1); // CTB 16
    w.write_ue(0); // min TB 4
    w.write_ue(2); // max TB 16
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 1); // scaling lists
    w.write_bits(0, 1); // amp
    w.write_bits(0, 1); // sao
    w.write_bits(0, 1); // pcm
    w.write_ue(0); // num_short_term_ref_pic_sets
    w.write_bits(0, 1); // long_term_ref_pics_present
    w.write_bits(0, 1); // temporal mvp
    w.write_bits(0, 1); // strong intra smoothing
    w.write_bits(0, 1); // vui
    w.write_bits(0, 1); // extensions
    w.rbsp_trailing();
    let mut nal = vec![33 << 1, 1];
    nal.extend(filmcraft_bitstream::escape_rbsp(&w.finish()));
    nal
}

/// The hvcC's own chroma/bit-depth fields can be left at 0 by a muxer: the SPS it carries says.
#[test]
fn hevc_pixel_format_prefers_the_sps_over_the_hvcc_fields() {
    let with_sps = |chroma: u32, bits: u32, field_chroma: u8, field_bits: u8| {
        let arrays = vec![filmcraft_isobmff::HevcNalArray { completeness: true, nal_type: 33, nalus: vec![hevc_sps(chroma, bits)] }];
        CodecConfig::Hevc(filmcraft_isobmff::HevcConfig {
            general_profile_idc: 1,
            chroma_format_idc: field_chroma,
            bit_depth_luma: field_bits,
            arrays,
            ..Default::default()
        })
    };
    assert_eq!(pixfmt_label(&with_sps(1, 8, 0, 0)), "YUV 4:2:0 8-bit", "hvcC fields at 0, SPS 4:2:0");
    assert_eq!(pixfmt_label(&with_sps(2, 10, 1, 8)), "YUV 4:2:2 10-bit", "the SPS outranks the fields");
    assert_eq!(pixfmt_label(&with_sps(3, 12, 3, 12)), "YUV 4:4:4 12-bit");
    // no SPS in the record: its fields
    let bare = CodecConfig::Hevc(filmcraft_isobmff::HevcConfig { general_profile_idc: 1, chroma_format_idc: 1, bit_depth_luma: 10, ..Default::default() });
    assert_eq!(pixfmt_label(&bare), "YUV 4:2:0 10-bit");
}

/// A one-frame 32×16 ProRes MOV with the given profile, chroma format and fourcc.
fn prores_mov(profile: filmcraft_prores::Profile, fourcc: &[u8; 4], chroma: filmcraft_prores::ChromaFormat, alpha: bool) -> Arc<[u8]> {
    let (w, h) = (32u32, 16u32);
    let fr = filmcraft_prores::Frame::new(w, h, chroma, if chroma == filmcraft_prores::ChromaFormat::Yuv444 { 12 } else { 10 }, alpha);
    let data = filmcraft_prores::Encoder::new(profile, w, h).encode(&fr).expect("encode");
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let t = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*fourcc), w as u16, h as u16), 25)).expect("track");
    mux.write_sample(t, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    mux.finish().expect("finish").into_inner().into()
}

/// `ap4h` / `ap4x` don't imply alpha: the frame header's alpha_channel_type does.
#[test]
fn prores_alpha_comes_from_the_frame_header() {
    use filmcraft_prores::{ChromaFormat, Profile};
    for (profile, fourcc, chroma, alpha, want) in [
        (Profile::P4444, b"ap4h", ChromaFormat::Yuv444, false, "YUV 4:4:4 12-bit"),
        (Profile::P4444, b"ap4h", ChromaFormat::Yuv444, true, "YUVA 4:4:4 12-bit"),
        (Profile::P4444Xq, b"ap4x", ChromaFormat::Yuv444, false, "YUV 4:4:4 12-bit"),
        (Profile::Hq, b"apch", ChromaFormat::Yuv422, false, "YUV 4:2:2 10-bit"),
    ] {
        let s = Mp4Source::open("p.mov", prores_mov(profile, fourcc, chroma, alpha)).expect("open");
        let v = s.info().video.as_ref().expect("video");
        assert_eq!((v.pixel_format.as_str(), v.has_alpha), (want, alpha), "{}", String::from_utf8_lossy(fourcc));
    }
}

#[test]
fn rtmd_fields_parse_and_reject_damaged_metadata() {
    let rate = FrameRate::FPS_59_94;
    // 03:37:12:34, non-drop (a Sony FX3/a6400 XAVC header).
    let mut s = [0u8; 24];
    s[0x0d..0x12].copy_from_slice(&[3, 37, 12, 0, 34]);
    assert_eq!(rtmd_fields(&s, rate), Some((3, 37, 12, 34, false)));
    s[0x10] = 1;
    assert_eq!(rtmd_fields(&s, rate), Some((3, 37, 12, 34, true)));
    // Too short to hold the block: no fields, no panic.
    assert_eq!(rtmd_fields(&[0u8; 6], rate), None);
    // Damaged fields are rejected rather than normalized into a plausible time.
    for bad in [[3, 37, 12, 0, 60], [3, 60, 0, 0, 0], [24, 0, 0, 0, 0], [0, 0, 60, 0, 0]] {
        let mut b = [0u8; 24];
        b[0x0d..0x12].copy_from_slice(&bad);
        assert_eq!(rtmd_fields(&b, rate), None, "{bad:?}");
    }
}

#[test]
fn rtmd_fields_reject_drop_frame_skipped_labels() {
    let df = |h, m, s, f| {
        let mut b = [0u8; 24];
        b[0x0d..0x12].copy_from_slice(&[h, m, s, 1, f]);
        rtmd_fields(&b, FrameRate::FPS_59_94)
    };
    // At 59.94 DF, frames 00-03 of a non-tenth minute are skipped, so they never occur.
    assert_eq!(df(0, 1, 0, 0), None);
    assert_eq!(df(0, 1, 0, 3), None);
    assert_eq!(df(0, 1, 0, 4), Some((0, 1, 0, 4, true)));
    assert_eq!(df(0, 10, 0, 0), Some((0, 10, 0, 0, true))); // tenth minute: label exists
    assert_eq!(df(0, 1, 1, 0), Some((0, 1, 1, 0, true))); // seconds > 0: label exists
    // At 29.97 DF only frames 00-01 are skipped.
    let df30 = |m, s, f| {
        let mut b = [0u8; 24];
        b[0x0d..0x12].copy_from_slice(&[0, m, s, 1, f]);
        rtmd_fields(&b, FrameRate::FPS_29_97)
    };
    assert_eq!(df30(1, 0, 0), None);
    assert_eq!(df30(1, 0, 2), Some((0, 1, 0, 2, true)));
}
