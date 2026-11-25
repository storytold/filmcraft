//! AMD AMF integration with FilmCraft export pipeline.

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result,
    VideoEncoder,
};
use filmcraft_isobmff::{Av1Config, AvcConfig, HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::{Codec, Config, Profile, Session};

pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    // AMF opens its device on the default adapter. Where an NVIDIA GPU with NVENC is present, it
    // takes H.264 and H.265 (an AMD iGPU as the default adapter must not get them).
    // The NVENC probe is cached, and warmed when the platform registers.
    if matches!(format, Format::H264 | Format::Hevc) && crate::nvenc::hevc_available() {
        return None;
    }
    factory_with(format, w, h, rate, s, Session::open)
}

/// AMF export factory without the dual-GPU handoff to NVENC (for testing AMF directly on AMD+NVIDIA hosts).
#[doc(hidden)]
pub fn direct_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    factory_with(format, w, h, rate, s, Session::open)
}

/// [`factory`] with the session open replaced, so what it declines is tested without the hardware.
fn factory_with(
    format: Format,
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
    open: impl FnOnce(&Config) -> std::result::Result<Session, String>,
) -> Option<Result<Box<dyn VideoEncoder>>> {
    let cfg = match config(format, w, h, rate, s) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return None,
        Err(why) => {
            log::info!("AMD AMF declined the {format:?} export: {why}");
            return None;
        }
    };
    let codec = cfg.profile.codec();
    let session = match open(&cfg) {
        Ok(s) => s,
        Err(why) => {
            log::info!("AMD AMF {:?} encoding declined: {why}", codec);
            return None;
        }
    };

    filmcraft_export::note_hw_encode_session();
    Some(Ok(Box::new(AmfEncoder {
        session,
        w,
        h,
        rate,
        signal: s.signal,
        sps: Vec::new(),
        pps: Vec::new(),
        vps: Vec::new(),
        hvcc: None,
        av1c: None,
        y: Vec::new(),
        u: Vec::new(),
        v: Vec::new(),
        y16: Vec::new(),
        u16: Vec::new(),
        v16: Vec::new(),
    })))
}

/// The session for this export: `Ok(None)` when it is not one AMF handles at all (another format,
/// or H.264 with Hardware encoding off), `Err` with the reason when AMF would encode something other
/// than what was asked. The same declines as NVENC's (`nvenc::export::config`), for AV1 too.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Option<Config>, String> {
    let codec = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto && !s.signal.is_hdr() => Codec::H264,
        Format::Hevc => Codec::Hevc,
        _ => return Ok(None),
    };
    if s.format.is_mxf() {
        return Err("MXF".into());
    }
    let hdr = s.signal.is_hdr();
    // BT.709 SDR, or BT.2020 NCL with PQ / HLG: the colour descriptions the session writes
    if hdr && (s.signal.primaries != 9 || s.signal.matrix != 9) || !hdr && s.signal != ColorSignal::default() {
        return Err(format!("the colour signal {:?} (AMF here writes BT.709, or BT.2020 with PQ / HLG)", s.signal));
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single) {
        return Err("two-pass VBR".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    if codec == Codec::H264 && s.h264_intra_only {
        return Err("all-intra H.264".into());
    }
    // the session writes no aspect ratio, for any codec
    if s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(format!("AMF needs nonzero even dimensions, not {w}x{h}"));
    }
    if w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
        return Err(format!("{w}x{h} does not fit an MP4 sample entry"));
    }
    let (Ok(timescale), Ok(frame_duration)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else {
        return Err("frame rate".into());
    };
    if timescale == 0 || frame_duration == 0 {
        return Err("frame rate".into());
    }
    let fps_val = f64::from(timescale) / f64::from(frame_duration);
    let keyint = s.keyframe_distance.filter(|&k| k > 0).unwrap_or_else(|| (fps_val * 2.0).round().max(1.0) as u32);
    let kbps = s.bitrate_kbps.max(100);
    let max_kbps = s.max_bitrate_kbps.filter(|&m| m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    let profile = match codec {
        Codec::H264 => match s.h264_profile {
            H264Profile::Baseline => Profile::H264Baseline,
            H264Profile::Main => Profile::H264Main,
            H264Profile::High => Profile::H264High,
            H264Profile::High10 => return Err("H.264 High 10".into()),
        },
        Codec::Hevc if hdr => Profile::HevcMain10,
        Codec::Hevc => Profile::HevcMain,
        Codec::Av1 if hdr => Profile::Av1Main10,
        Codec::Av1 => Profile::Av1Main,
    };
    Ok(Some(Config {
        width: w,
        height: h,
        fps: (timescale, frame_duration),
        bitrate_kbps: kbps,
        max_bitrate_kbps: max_kbps,
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        keyint,
        profile,
        signal: s.signal,
    }))
}

struct AmfEncoder {
    session: Session,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
    sps: Vec<u8>,
    pps: Vec<u8>,
    vps: Vec<u8>,
    hvcc: Option<HevcConfig>,
    av1c: Option<Av1Config>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y16: Vec<u16>,
    u16: Vec<u16>,
    v16: Vec<u16>,
}

impl AmfEncoder {
    fn process_raw_packets(&mut self, packets: Vec<super::Packet>) -> Result<Vec<EncodedPacket>> {
        let duration = u32::try_from(self.rate.den).unwrap_or(u32::MAX);
        let mut out = Vec::new();

        for p in packets {
            filmcraft_export::note_hw_encode_frame();
            let codec = self.session.codec();
            let is_av1 = codec == Codec::Av1;

            if !is_av1 {
                // Parse parameter sets from keyframe Annex B if not parsed yet
                if self.sps.is_empty() {
                    // the sample entry is built from the first keyframe's parameter sets, and they are
                    // stripped from the samples: without them the file would not decode
                    let sets = parameter_sets(codec, p.key, &p.data, (self.w, self.h), self.session.is_ten_bit()).map_err(ExportError::Encode)?;
                    self.vps = sets.vps;
                    self.sps = sets.sps;
                    self.pps = sets.pps;
                    self.hvcc = sets.hvcc;
                }
            } else if self.av1c.is_none() {
                // the `av1C` must match the stream's sequence header, which the first keyframe carries
                self.av1c = Some(av1_config(p.key, &p.data).map_err(ExportError::Encode)?);
            }

            let data = if is_av1 {
                p.data
            } else {
                let nvenc_codec = match codec {
                    Codec::H264 => crate::nvenc::Codec::H264,
                    Codec::Hevc => crate::nvenc::Codec::Hevc,
                    Codec::Av1 => return Err(ExportError::Encode("unexpected AV1 Annex B conversion request".into())),
                };
                crate::nvenc::annex_b_to_length_prefixed_for(nvenc_codec, &p.data)
                    .map_err(|e| ExportError::Encode(format!("AMD AMF Annex B conversion failed: {e}")))?
            };

            out.push(EncodedPacket { data, key: p.key, duration, composition_offset: 0 });
        }
        Ok(out)
    }
}

impl VideoEncoder for AmfEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.w).unwrap_or(u16::MAX), u16::try_from(self.h).unwrap_or(u16::MAX));
        match self.session.codec() {
            Codec::H264 => SampleEntry::avc(AvcConfig::new(vec![self.sps.clone()], vec![self.pps.clone()], 4), w, h),
            Codec::Hevc => {
                let mut e = SampleEntry::hevc(self.hvcc.clone().unwrap_or_default(), w, h);
                self.signal.apply_to(&mut e, false);
                e
            }
            Codec::Av1 => {
                let mut e = SampleEntry::av1(self.av1c.clone().unwrap_or_default(), w, h);
                self.signal.apply_to(&mut e, false);
                e
            }
        }
    }

    fn timescale(&self) -> u32 {
        u32::try_from(self.rate.num).unwrap_or(u32::MAX)
    }

    fn hardware(&self) -> Option<&'static str> {
        Some("AMD AMF")
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if f.width != self.w || f.height != self.h {
            return Err(ExportError::Encode(format!("AMD AMF: picture size {}x{} does not match encoder {}x{}", f.width, f.height, self.w, self.h)));
        }

        let pts = i64::try_from(f.index).unwrap_or(0).saturating_mul(self.rate.den);
        let duration = self.rate.den;

        let raw = match (self.session.is_ten_bit(), f.hdr) {
            (true, Some(rgb)) => {
                let (kr, kb) = self.signal.kr_kb();
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgbf_to_yuv420_10(
                        rgb,
                        self.w as usize,
                        self.h as usize,
                        filmcraft_export::Matrix::new(kr, kb),
                        &mut self.y16,
                        &mut self.u16,
                        &mut self.v16,
                    )
                })?;
                self.session.encode_10bit(&self.y16, &self.u16, &self.v16, pts, duration)
            }
            (true, None) => return Err(ExportError::Unsupported("AMD AMF Main 10 needs the HDR picture (EncoderFrame::hdr)".into())),
            (false, Some(_)) => return Err(ExportError::Unsupported("AMD AMF Main (8-bit) does not take HDR pictures".into())),
            (false, None) => {
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgba_to_yuv420_8(f.rgba, self.w as usize, self.h as usize, &mut self.y, &mut self.u, &mut self.v)
                });
                self.session.encode_8bit(&self.y, &self.u, &self.v, pts, duration)
            }
        };

        let packets = raw.map_err(|e| ExportError::Encode(format!("AMD AMF encode failed: {e}")))?;
        self.process_raw_packets(packets)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let packets = self.session.drain().map_err(|e| ExportError::Encode(format!("AMD AMF drain failed: {e}")))?;
        self.process_raw_packets(packets)
    }
}

/// The parameter sets of the first packet, from which the sample entry is written.
struct ParameterSets {
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    hvcc: Option<HevcConfig>,
}

fn parameter_sets(codec: Codec, key: bool, data: &[u8], size: (u32, u32), ten_bit: bool) -> std::result::Result<ParameterSets, String> {
    if !key {
        return Err("AMD AMF: the first packet is not a keyframe, so it carries no parameter sets".into());
    }
    match codec {
        Codec::H264 => {
            let (sps, pps) = crate::nvenc::split_parameter_sets(data).map_err(|e| format!("AMD AMF: the first keyframe has no usable SPS/PPS: {e}"))?;
            Ok(ParameterSets { vps: Vec::new(), sps, pps, hvcc: None })
        }
        Codec::Hevc => {
            let (vps, sps, pps) =
                crate::nvenc::split_hevc_parameter_sets(data).map_err(|e| format!("AMD AMF: the first keyframe has no usable VPS/SPS/PPS: {e}"))?;
            let hvcc = crate::nvenc::hevc::hevc_config(&vps, &sps, &pps, size, if ten_bit { 10 } else { 8 })
                .map_err(|e| format!("AMD AMF: the first keyframe's HEVC parameter sets are unusable: {e}"))?;
            Ok(ParameterSets { vps, sps, pps, hvcc: Some(hvcc) })
        }
        Codec::Av1 => Err("AMD AMF: AV1 has no Annex B parameter sets".into()),
    }
}

/// The OBUs of a temporal unit (each with a size field, as AMF writes them): `(type, whole OBU, payload)`.
fn av1_obus(data: &[u8]) -> Vec<(u8, &[u8], &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let start = pos;
        let b0 = data[pos];
        let (obu_type, extension, has_size) = ((b0 >> 3) & 0x0f, (b0 >> 2) & 1 == 1, (b0 >> 1) & 1 == 1);
        pos += 1 + usize::from(extension);
        if !has_size {
            break;
        }
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let Some(&b) = data.get(pos) else { return out };
            pos += 1;
            len |= usize::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
            if shift > 56 {
                return out;
            }
        }
        let Some(end) = pos.checked_add(len).filter(|&e| e <= data.len()) else { return out };
        out.push((obu_type, &data[start..end], &data[pos..end]));
        pos = end;
    }
    out
}

/// `seq_tier[0]` of a sequence header OBU's payload (AV1 §5.5.1): coded only for a level above 3.3.
fn av1_tier_0(payload: &[u8]) -> Option<bool> {
    let mut r = Bits { data: payload, pos: 0 };
    r.read(3)?; // seq_profile
    r.read(1)?; // still_picture
    if r.read(1)? == 1 {
        // reduced_still_picture_header: one level, Main tier
        return Some(false);
    }
    if r.read(1)? == 1 {
        // timing_info: num_units_in_display_tick, time_scale, equal_picture_interval
        r.read(32)?;
        r.read(32)?;
        if r.read(1)? == 1 {
            r.uvlc()?; // num_ticks_per_picture_minus_1
        }
        if r.read(1)? == 1 {
            // decoder_model_info: buffer_delay_length_minus_1, num_units_in_decoding_tick,
            // buffer_removal_time_length_minus_1, frame_presentation_time_length_minus_1
            r.read(5)?;
            r.read(32)?;
            r.read(5)?;
            r.read(5)?;
        }
    }
    r.read(1)?; // initial_display_delay_present_flag
    r.read(5)?; // operating_points_cnt_minus_1
    r.read(12)?; // operating_point_idc[0]
    let level = r.read(5)?;
    Some(level > 7 && r.read(1)? == 1)
}

/// An MSB-first bit reader over a byte slice.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    fn uvlc(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Some(u32::MAX);
            }
        }
        Some(self.read(zeros)?.saturating_add((1u32 << zeros) - 1))
    }
}

/// The `av1C` of the stream, from the sequence header OBU of the first packet (AV1-ISOBMFF §2.3: its
/// fields restate the sequence header's, and `configOBUs` carries the header itself).
fn av1_config(key: bool, data: &[u8]) -> std::result::Result<Av1Config, String> {
    if !key {
        return Err("AMD AMF: the first AV1 packet is not a keyframe, so it carries no sequence header".into());
    }
    let (obu, payload) =
        av1_obus(data).into_iter().find(|(t, ..)| *t == 1).map(|(_, o, p)| (o, p)).ok_or("AMD AMF: the first AV1 keyframe has no sequence header")?;
    let seq = craft_video::av1::decode::SequenceHeader::parse(payload).map_err(|e| format!("AMD AMF: the AV1 sequence header is unusable: {e}"))?;
    let c = &seq.color;
    Ok(Av1Config {
        seq_profile: seq.profile,
        seq_level_idx_0: seq.seq_level_idx[0],
        // the decoder's parser reads seq_tier but does not keep it
        seq_tier_0: av1_tier_0(payload).ok_or("AMD AMF: the AV1 sequence header ends before its first operating point")?,
        high_bitdepth: c.bit_depth > 8,
        twelve_bit: c.bit_depth == 12,
        monochrome: c.mono_chrome,
        chroma_subsampling_x: c.subsampling_x == 1,
        chroma_subsampling_y: c.subsampling_y == 1,
        chroma_sample_position: c.chroma_sample_position,
        initial_presentation_delay_minus_one: None,
        config_obus: obu.to_vec(),
    })
}

#[cfg(test)]
mod parameter_set_tests {
    use super::*;

    #[test]
    fn a_first_keyframe_without_parameter_sets_fails_the_export() {
        // an IDR slice alone: no SPS or PPS to build the sample entry from
        let idr_only = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];
        assert!(parameter_sets(Codec::H264, true, &idr_only, (64, 64), false).is_err());
        assert!(parameter_sets(Codec::Hevc, true, &[0, 0, 0, 1, 0x26, 0x01, 0xaf], (64, 64), false).is_err());
    }

    #[test]
    fn a_first_packet_that_is_not_a_keyframe_fails_the_export() {
        assert!(parameter_sets(Codec::H264, false, &[0, 0, 0, 1, 0x41, 0x9a], (64, 64), false).is_err());
    }

    #[test]
    fn the_first_keyframe_gives_its_sps_and_pps() {
        let data = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65, 0x88];
        let sets = parameter_sets(Codec::H264, true, &data, (64, 64), false).unwrap();
        assert_eq!(sets.sps, [0x67, 0x64, 0x00, 0x1f]);
        assert_eq!(sets.pps, [0x68, 0xee, 0x3c, 0x80]);
    }
}

#[cfg(test)]
mod factory_tests {
    use super::*;
    use std::cell::Cell;

    const RATE: FrameRate = FrameRate { num: 30, den: 1 };

    fn auto() -> ExportSettings {
        ExportSettings { hardware_encoding: HardwareEncoding::Auto, ..ExportSettings::default() }
    }

    /// Whether the factory got as far as opening a session (stubbed to fail, so nothing is encoded).
    fn opens(format: Format, w: u32, h: u32, s: &ExportSettings) -> bool {
        let opened = Cell::new(false);
        let r = factory_with(format, w, h, RATE, s, |_| {
            opened.set(true);
            Err("stub".into())
        });
        assert!(r.is_none(), "a failed open declines");
        opened.get()
    }

    #[test]
    fn exports_amf_would_encode_differently_are_declined_before_a_session_opens() {
        for (what, format, s) in [
            ("H.264 CRF", Format::H264, ExportSettings { bitrate_mode: BitrateMode::Crf, ..auto() }),
            ("H.265 CRF", Format::Hevc, ExportSettings { bitrate_mode: BitrateMode::Crf, ..auto() }),
            ("all-intra H.264", Format::H264, ExportSettings { h264_intra_only: true, ..auto() }),
            ("two-pass VBR", Format::Hevc, ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..auto() }),
            ("non-square pixels, H.265", Format::Hevc, ExportSettings { pixel_aspect: Some((4, 3)), ..auto() }),
            ("interlaced", Format::Hevc, ExportSettings { field_order: FieldOrder::UpperFirst, ..auto() }),
            ("BT.601 SDR", Format::Hevc, ExportSettings { signal: ColorSignal { primaries: 6, transfer: 6, matrix: 6 }, ..auto() }),
            ("PQ with BT.709 primaries", Format::Hevc, ExportSettings { signal: ColorSignal { primaries: 1, transfer: 16, matrix: 1 }, ..auto() }),
            ("H.264 with Hardware encoding off", Format::H264, ExportSettings::default()),
            ("AV1 deferred to follow-up", Format::Av1, auto()),
        ] {
            assert!(!opens(format, 1920, 1080, &s), "{what}: declined");
        }
        for (w, h) in [(0, 1080), (1920, 0), (1921, 1080), (1920, 1081), (70_000, 1080)] {
            assert!(!opens(Format::Hevc, w, h, &auto()), "{w}x{h}: declined");
        }
        assert!(!opens(Format::ProRes, 1920, 1080, &auto()), "not a format AMF encodes");
    }

    #[test]
    fn exports_amf_encodes_as_asked_open_a_session() {
        assert!(opens(Format::H264, 1920, 1080, &auto()));
        assert!(opens(Format::H264, 1920, 1080, &ExportSettings { bitrate_mode: BitrateMode::Cbr, pixel_aspect: Some((1, 1)), ..auto() }));
        assert!(opens(Format::Hevc, 1920, 1080, &ExportSettings::default()), "H.265 needs no opt-in");
        assert!(opens(Format::Hevc, 3840, 2160, &ExportSettings { signal: ColorSignal::PQ, ..auto() }));
        let cfg = config(Format::Hevc, 3840, 2160, RATE, &ExportSettings { signal: ColorSignal::PQ, ..auto() }).unwrap().unwrap();
        assert_eq!((cfg.profile, cfg.signal), (Profile::HevcMain10, ColorSignal::PQ), "Main 10 with the export's colour");
    }

    /// A 1080p 10-bit PQ sequence header OBU as libaom writes it: level 4.0 (8), Main tier.
    const SEQUENCE_HEADER: [u8; 16] = [0x0a, 0x0e, 0x00, 0x00, 0x00, 0x42, 0xab, 0xbf, 0xc3, 0x73, 0x2b, 0xe5, 0x40, 0x80, 0x82, 0x41];

    #[test]
    fn the_av1c_restates_the_first_keyframes_sequence_header() {
        // a temporal delimiter, the sequence header, then (a stand-in for) a frame OBU
        let mut tu = vec![0x12, 0x00];
        tu.extend_from_slice(&SEQUENCE_HEADER);
        tu.extend_from_slice(&[0x32, 0x01, 0x10]);
        let c = av1_config(true, &tu).unwrap();
        assert_eq!((c.seq_profile, c.seq_level_idx_0, c.seq_tier_0), (0, 8, false));
        assert!(c.high_bitdepth && !c.twelve_bit && !c.monochrome);
        assert!(c.chroma_subsampling_x && c.chroma_subsampling_y);
        assert_eq!(c.config_obus, SEQUENCE_HEADER, "configOBUs carries the sequence header OBU");
        assert!(av1_config(false, &tu).is_err(), "not a keyframe");
        assert!(av1_config(true, &[0x12, 0x00, 0x32, 0x01, 0x10]).is_err(), "no sequence header");
    }

    #[test]
    fn the_tier_is_read_where_the_level_codes_it() {
        // seq_profile 0, flags 0, operating_points_cnt_minus_1 0, idc 0, level 13 (5.1), tier 1
        let high = [0x00, 0x00, 0x00, 0b0110_1100];
        assert_eq!(av1_tier_0(&high), Some(true));
        // level 4 (3.0) codes no tier: the next bit is not read as one
        let low = [0x00, 0x00, 0x00, 0b0010_0100];
        assert_eq!(av1_tier_0(&low), Some(false));
        assert_eq!(av1_tier_0(&[0x00]), None, "truncated");
    }
}
