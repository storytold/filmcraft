//! Intel Quick Sync Video export encoder for FilmCraft.
//!
//! [`factory`] is registered with `filmcraft_export::register_encoder`.

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result,
    VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::session::{Codec, Config, Packet, Profile, QsvSession};

/// The Intel Quick Sync Video export encoder.
struct QsvEncoder {
    session: QsvSession,
    codec: Codec,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
    ten_bit: bool,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y16: Vec<u16>,
    u16: Vec<u16>,
    v16: Vec<u16>,
    sample_entry: Option<SampleEntry>,
}

impl QsvEncoder {
    fn new(session: QsvSession, codec: Codec, w: u32, h: u32, rate: FrameRate, signal: ColorSignal, ten_bit: bool) -> Self {
        let initial_entry =
            (!session.sequence_header().is_empty()).then(|| build_sample_entry(codec, session.sequence_header(), (w, h), ten_bit, signal).ok()).flatten();
        Self {
            session,
            codec,
            w,
            h,
            rate,
            signal,
            ten_bit,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
            y16: Vec::new(),
            u16: Vec::new(),
            v16: Vec::new(),
            sample_entry: initial_entry,
        }
    }

    fn make_encoded_packets(&mut self, ps: Vec<Packet>) -> Result<Vec<EncodedPacket>> {
        let den = self.rate.den;
        let duration = u32::try_from(den).unwrap_or(u32::MAX);

        let mut out = Vec::with_capacity(ps.len());
        for p in ps {
            if self.sample_entry.is_none() {
                let entry = build_sample_entry(self.codec, &p.data, (self.w, self.h), self.ten_bit, self.signal)
                    .map_err(|e| ExportError::Encode(format!("Intel Quick Sync: cannot build container sample entry: {e}")))?;
                self.sample_entry = Some(entry);
            }
            let comp_offset = i32::try_from((p.pts.saturating_sub(p.dts)).saturating_mul(den)).unwrap_or(0);
            let data = match self.codec {
                Codec::H264 => crate::nvenc::annex_b_to_length_prefixed_for(crate::nvenc::Codec::H264, &p.data)
                    .map_err(|e| ExportError::Encode(format!("Intel Quick Sync H.264 Annex B conversion failed: {e}")))?,
                Codec::Hevc => crate::nvenc::annex_b_to_length_prefixed_for(crate::nvenc::Codec::Hevc, &p.data)
                    .map_err(|e| ExportError::Encode(format!("Intel Quick Sync HEVC Annex B conversion failed: {e}")))?,
                Codec::Av1 => p.data,
            };
            out.push(EncodedPacket { data, key: p.key, duration, composition_offset: comp_offset });
        }
        Ok(out)
    }
}

/// Extract `(whole_obu, payload)` for the first AV1 `OBU_SEQUENCE_HEADER` (type 1), enforcing
/// the 8-byte LEB128 limit from AV1 Spec §4.10.5.
fn extract_av1_seq_header_obu(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0usize;
    while let Some(&b0) = data.get(pos) {
        if (b0 >> 7) & 1 != 0 {
            return None;
        }
        let obu_type = (b0 >> 3) & 0x0F;
        let extension_flag = (b0 >> 2) & 1;
        let has_size = (b0 >> 1) & 1;
        let start_pos = pos;
        pos += 1;
        if extension_flag != 0 {
            if pos >= data.len() {
                return None;
            }
            pos += 1;
        }
        if has_size != 0 {
            let mut payload_len = 0u64;
            let mut valid = false;
            for i in 0..8 {
                let &b = data.get(pos)?;
                pos += 1;
                payload_len |= u64::from(b & 0x7F) << (7 * i);
                if (b & 0x80) == 0 {
                    valid = true;
                    break;
                }
            }
            if !valid {
                return None;
            }
            let payload_len = usize::try_from(payload_len).ok()?;
            let end_pos = pos.checked_add(payload_len).filter(|&e| e <= data.len())?;
            if obu_type == 1 {
                return Some((&data[start_pos..end_pos], &data[pos..end_pos]));
            }
            pos = end_pos;
        } else {
            if obu_type == 1 {
                return Some((&data[start_pos..], &data[pos..]));
            }
            return None;
        }
    }
    None
}

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
        let mut zeros = 0u32;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Some(u32::MAX);
            }
        }
        Some(self.read(zeros)?.saturating_add((1u32 << zeros) - 1))
    }
}

/// `seq_tier[0]` of an AV1 sequence header OBU payload (AV1 §5.5.1): coded when `seq_level_idx[0] > 7`.
fn av1_tier_0(payload: &[u8]) -> Option<bool> {
    let mut r = Bits { data: payload, pos: 0 };
    r.read(3)?; // seq_profile
    r.read(1)?; // still_picture
    if r.read(1)? == 1 {
        return Some(false);
    }
    if r.read(1)? == 1 {
        r.read(32)?;
        r.read(32)?;
        if r.read(1)? == 1 {
            r.uvlc()?;
        }
        if r.read(1)? == 1 {
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

fn build_sample_entry(codec: Codec, data: &[u8], size: (u32, u32), ten_bit: bool, signal: ColorSignal) -> std::result::Result<SampleEntry, String> {
    let (w, h) = (u16::try_from(size.0).unwrap_or(u16::MAX), u16::try_from(size.1).unwrap_or(u16::MAX));
    match codec {
        Codec::H264 => {
            let (sps, pps) = crate::nvenc::split_parameter_sets(data)?;
            let mut e = SampleEntry::avc(AvcConfig::new(vec![sps], vec![pps], 4), w, h);
            signal.apply_to(&mut e, false);
            Ok(e)
        }
        Codec::Hevc => {
            let bit_depth = if ten_bit { 10 } else { 8 };
            let (vps, sps, pps) = crate::nvenc::split_hevc_parameter_sets(data)?;
            let hvcc = crate::nvenc::hevc::hevc_config(&vps, &sps, &pps, size, bit_depth)?;
            let mut e = SampleEntry::hevc(hvcc, w, h);
            signal.apply_to(&mut e, false);
            Ok(e)
        }
        Codec::Av1 => {
            let (whole_obu, payload) = extract_av1_seq_header_obu(data).ok_or_else(|| "missing or invalid AV1 sequence header OBU".to_string())?;
            let seq = craft_video::av1::decode::SequenceHeader::parse(payload).map_err(|e| format!("invalid AV1 sequence header: {e}"))?;
            let seq_tier_0 = av1_tier_0(payload).ok_or_else(|| "truncated AV1 sequence header operating point".to_string())?;
            let c = &seq.color;
            let av1_cfg = filmcraft_isobmff::Av1Config {
                seq_profile: seq.profile,
                seq_level_idx_0: seq.seq_level_idx[0],
                seq_tier_0,
                high_bitdepth: c.bit_depth > 8,
                twelve_bit: c.bit_depth == 12,
                monochrome: c.mono_chrome,
                chroma_subsampling_x: c.subsampling_x == 1,
                chroma_subsampling_y: c.subsampling_y == 1,
                chroma_sample_position: c.chroma_sample_position,
                initial_presentation_delay_minus_one: None,
                config_obus: whole_obu.to_vec(),
            };
            let mut e = SampleEntry::av1(av1_cfg, w, h);
            signal.apply_to(&mut e, false);
            Ok(e)
        }
    }
}

impl VideoEncoder for QsvEncoder {
    fn encode(&mut self, f: &EncoderFrame<'_>) -> Result<Vec<EncodedPacket>> {
        if f.width != self.w || f.height != self.h {
            return Err(ExportError::Encode(format!("Intel: a {}x{} picture for a {}x{} encoder", f.width, f.height, self.w, self.h)));
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let pts = f.index as i64;

        let packets = match (self.ten_bit, f.hdr) {
            (true, Some(rgb)) => {
                let (kr, kb) = self.signal.kr_kb();
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgbf_to_yuv420_10(rgb, w, h, filmcraft_export::Matrix::new(kr, kb), &mut self.y16, &mut self.u16, &mut self.v16)
                })?;
                self.session.encode_p010(&self.y16, &self.u16, &self.v16, pts).map_err(|e| ExportError::Encode(format!("Intel encode failed: {e}")))?
            }
            (true, None) => return Err(ExportError::Unsupported("Intel Quick Sync Main 10 needs the HDR picture (EncoderFrame::hdr)".into())),
            (false, Some(_)) => return Err(ExportError::Unsupported("Intel Quick Sync 8-bit encoder does not take HDR pictures".into())),
            (false, None) => {
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v)
                });
                self.session.encode_nv12(&self.y, &self.u, &self.v, pts).map_err(|e| ExportError::Encode(format!("Intel encode failed: {e}")))?
            }
        };

        filmcraft_export::note_hw_encode_frame();
        self.make_encoded_packets(packets)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let packets = self.session.flush().map_err(|e| ExportError::Encode(format!("Intel flush failed: {e}")))?;
        let out = self.make_encoded_packets(packets)?;
        if self.sample_entry.is_none() {
            return Err(ExportError::Encode("Intel Quick Sync encoder produced no codec sequence header before end of stream".into()));
        }
        Ok(out)
    }

    fn sample_entry(&self) -> SampleEntry {
        if let Some(ref entry) = self.sample_entry {
            return entry.clone();
        }
        // Only reachable if called directly before any frame was encoded and MF_MT_MPEG_SEQUENCE_HEADER
        // was empty; normal exports fail in make_encoded_packets/flush before opening the muxer.
        log::error!("Intel Quick Sync sample_entry() requested before codec headers were received");
        let (w, h) = (u16::try_from(self.w).unwrap_or(u16::MAX), u16::try_from(self.h).unwrap_or(u16::MAX));
        match self.codec {
            Codec::H264 => SampleEntry::avc(AvcConfig::new(Vec::new(), Vec::new(), 4), w, h),
            Codec::Hevc => SampleEntry::hevc(filmcraft_isobmff::HevcConfig::default(), w, h),
            Codec::Av1 => SampleEntry::av1(filmcraft_isobmff::Av1Config::default(), w, h),
        }
    }

    fn timescale(&self) -> u32 {
        u32::try_from(self.rate.num).unwrap_or(u32::MAX)
    }

    fn media_start(&self) -> Option<i64> {
        None
    }

    fn hardware(&self) -> Option<&'static str> {
        Some("Intel Quick Sync")
    }
}

/// Validate export settings and build a QSV [`Config`], or explain why QSV declines.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    if !matches!(format, Format::H264 | Format::Hevc | Format::Av1) || s.format.is_mxf() {
        return Err("only MP4 / MOV H.264, H.265 and AV1 exports".into());
    }
    let hdr = s.signal.is_hdr();
    if hdr && format == Format::H264 {
        return Err("HDR (8-bit H.264 here carries it with software signalling)".into());
    }
    if matches!(format, Format::Hevc | Format::Av1) && (hdr && (s.signal.primaries != 9 || s.signal.matrix != 9) || !hdr && s.signal != ColorSignal::default())
    {
        return Err(format!("the colour signal {:?} ({} here is BT.709, or BT.2020 with PQ / HLG)", s.signal, format.label()));
    }
    if !matches!(s.h264_pass, H264Pass::Single) || s.bitrate_mode == BitrateMode::Vbr2Pass {
        return Err("two-pass VBR".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    if format == Format::H264 && s.h264_intra_only {
        return Err("all-intra H.264".into());
    }
    if s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || w > 8192 || h > 8192 {
        return Err(format!("Intel Quick Sync needs nonzero even dimensions up to 8192x8192 (got {w}x{h})"));
    }
    if rate.num <= 0 || rate.den <= 0 || rate.num > i64::from(u32::MAX) || rate.den > i64::from(u32::MAX) {
        return Err("frame rate".into());
    }
    let (codec, profile) = match (format, s.h264_profile) {
        (Format::H264, H264Profile::Baseline) => (Codec::H264, Profile::H264Baseline),
        (Format::H264, H264Profile::Main) => (Codec::H264, Profile::H264Main),
        (Format::H264, H264Profile::High) => (Codec::H264, Profile::H264High),
        (Format::H264, H264Profile::High10) => return Err("High 10".into()),
        (Format::Hevc, _) if hdr => (Codec::Hevc, Profile::HevcMain10),
        (Format::Hevc, _) => (Codec::Hevc, Profile::HevcMain),
        (Format::Av1, _) if hdr => (Codec::Av1, Profile::Av1Main10),
        (Format::Av1, _) => (Codec::Av1, Profile::Av1Main),
        _ => return Err("unsupported format".into()),
    };
    let keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| ((rate.num as f64 / rate.den as f64) * 2.0).round().max(1.0) as u32);
    Ok(Config { codec, profile, size: (w, h), rate, bitrate_kbps: s.bitrate_kbps.max(100), keyint, ten_bit: hdr })
}

fn codec_for_format(format: Format) -> Option<Codec> {
    match format {
        Format::H264 => Some(Codec::H264),
        Format::Hevc => Some(Codec::Hevc),
        Format::Av1 => Some(Codec::Av1),
        _ => None,
    }
}

/// The export encoder factory registered with `filmcraft_export::register_encoder`.
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    factory_with(
        format,
        w,
        h,
        rate,
        s,
        super::has_device,
        |codec| match codec {
            Codec::Hevc => super::hevc_available(),
            Codec::Av1 => super::av1_available(),
            Codec::H264 => false,
        },
        QsvSession::new,
    )
}

#[allow(clippy::too_many_arguments)]
fn factory_with(
    format: Format,
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
    has_device: impl FnOnce() -> bool,
    codec_available: impl FnOnce(Codec) -> bool,
    open: impl FnOnce(Config) -> std::result::Result<QsvSession, String>,
) -> Option<Result<Box<dyn VideoEncoder>>> {
    let codec = codec_for_format(format)?;
    if format == Format::H264 && s.hardware_encoding != HardwareEncoding::Auto {
        return None;
    }

    if format == Format::H264 {
        let cfg = match config(format, w, h, rate, s) {
            Ok(c) => c,
            Err(why) => {
                log::info!("Intel Quick Sync H.264 declined: {why}");
                filmcraft_export::note_hw_encode_declined_by("Intel Quick Sync", &why);
                return None;
            }
        };
        if !has_device() {
            return None;
        }
        return match open(cfg) {
            Ok(session) => {
                filmcraft_export::note_hw_encode_session();
                Some(Ok(Box::new(QsvEncoder::new(session, codec, w, h, rate, s.signal, false))))
            }
            Err(why) => {
                log::info!("Intel Quick Sync H.264 encoder declined: {why}");
                filmcraft_export::note_hw_encode_declined_by("Intel Quick Sync", &why);
                None
            }
        };
    }

    if !has_device() {
        return None;
    }

    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => {
            if !codec_available(codec) {
                log::info!("no Intel Quick Sync {} encoder: {why}", codec.name());
                return None;
            }
            log::info!("Intel Quick Sync {} encoding declined: {why}", codec.name());
            filmcraft_export::note_hw_encode_declined_by("Intel Quick Sync", &why);
            return Some(Err(ExportError::Unsupported(format!("{} export with Intel Quick Sync: {why}", codec.name()))));
        }
    };

    let ten_bit = cfg.ten_bit;
    match open(cfg) {
        Ok(session) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(QsvEncoder::new(session, codec, w, h, rate, s.signal, ten_bit))))
        }
        Err(why) if !codec_available(codec) => {
            log::info!("no Intel Quick Sync {} encoder: {why}", codec.name());
            None
        }
        Err(why) => {
            log::info!("Intel Quick Sync {} encoding declined: {why}", codec.name());
            filmcraft_export::note_hw_encode_declined_by("Intel Quick Sync", &why);
            Some(Err(ExportError::Unsupported(format!("{} export with Intel Quick Sync: {why}", codec.name()))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_isobmff::CodecConfig;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn extract_av1_seq_header_rejects_overlong_leb128_without_panicking() {
        // 12 continuation bytes followed by payload: must stop at 8 LEB128 bytes without shift overflow or panic.
        let hostile = [0x0A, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x00];
        let res = std::panic::catch_unwind(|| extract_av1_seq_header_obu(&hostile));
        assert!(res.is_ok());
        assert!(res.unwrap().is_none());
    }

    #[test]
    fn build_sample_entry_rejects_missing_headers_without_placeholders() {
        let sdr = ColorSignal::default();
        let idr_only = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];
        assert!(build_sample_entry(Codec::H264, &[], (1920, 1080), false, sdr).is_err());
        assert!(build_sample_entry(Codec::H264, &idr_only, (1920, 1080), false, sdr).is_err());

        let hevc_slice_only = [0, 0, 0, 1, 0x26, 0x01, 0xaf];
        assert!(build_sample_entry(Codec::Hevc, &[], (1920, 1080), false, sdr).is_err());
        assert!(build_sample_entry(Codec::Hevc, &hevc_slice_only, (1920, 1080), true, ColorSignal::PQ).is_err());

        let av1_frame_only = [0x12, 0x00, 0x32, 0x01, 0x10];
        assert!(build_sample_entry(Codec::Av1, &[], (1920, 1080), false, sdr).is_err());
        assert!(build_sample_entry(Codec::Av1, &av1_frame_only, (1920, 1080), true, ColorSignal::PQ).is_err());
    }

    #[test]
    fn build_sample_entry_parses_real_h264_hevc_and_av1_headers() {
        let sdr = ColorSignal::default();
        let h264 = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65, 0x88];
        let e_h264 = build_sample_entry(Codec::H264, &h264, (1920, 1080), false, sdr).unwrap();
        let CodecConfig::Avc(avcc) = &e_h264.codec else { panic!("{:?}", e_h264.codec) };
        assert_eq!(avcc.sps, vec![vec![0x67, 0x64, 0x00, 0x28]]);
        assert_eq!(avcc.pps, vec![vec![0x68, 0xee, 0x3c, 0x80]]);

        let vps = unhex("40010c01ffff01600000030090000003000003005d9940c0000003004000000614");
        let sps = unhex("42010101600000030090000003000003005da00280802e1f1396654a421191bff0c05a8080808a0000030002000003003010");
        let pps = unhex("4401c1937c0cc9");
        let mut hevc = Vec::new();
        for nal in [&vps, &sps, &pps] {
            hevc.extend_from_slice(&[0, 0, 0, 1]);
            hevc.extend_from_slice(nal);
        }
        let e_hevc = build_sample_entry(Codec::Hevc, &hevc, (1280, 720), false, sdr).unwrap();
        let CodecConfig::Hevc(hvcc) = &e_hevc.codec else { panic!("{:?}", e_hevc.codec) };
        assert_eq!(hvcc.general_level_idc, 93);
        assert_eq!(hvcc.vps(), vec![vps.as_slice()]);
        assert_eq!(hvcc.sps(), vec![sps.as_slice()]);
        assert_eq!(hvcc.pps(), vec![pps.as_slice()]);

        // 1080p 10-bit PQ sequence header OBU: level 4.0 (idx 8), Main tier.
        let av1_seq_obu = [0x0au8, 0x0e, 0x00, 0x00, 0x00, 0x42, 0xab, 0xbf, 0xc3, 0x73, 0x2b, 0xe5, 0x40, 0x80, 0x82, 0x41];
        let mut av1_tu = vec![0x12, 0x00];
        av1_tu.extend_from_slice(&av1_seq_obu);
        av1_tu.extend_from_slice(&[0x32, 0x01, 0x10]);
        let e_av1 = build_sample_entry(Codec::Av1, &av1_tu, (1920, 1080), true, ColorSignal::PQ).unwrap();
        let CodecConfig::Av1(av1c) = &e_av1.codec else { panic!("{:?}", e_av1.codec) };
        assert_eq!((av1c.seq_profile, av1c.seq_level_idx_0, av1c.seq_tier_0), (0, 8, false));
        assert!(av1c.high_bitdepth && !av1c.twelve_bit && !av1c.monochrome);
        assert_eq!(av1c.config_obus, av1_seq_obu);
    }

    #[test]
    fn declined_h264_settings_never_open_qsv_session() {
        let declined = |format, w, h, rate, s: &ExportSettings| {
            let res = factory_with(format, w, h, rate, s, || true, |_| true, |_| panic!("must not open QSV session"));
            assert!(res.is_none(), "expected None for {format:?} {w}x{h} {rate:?}");
        };

        let mut s = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Off, ..Default::default() };
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &s);

        s.hardware_encoding = HardwareEncoding::Auto;
        declined(Format::ProRes, 1280, 720, FrameRate::FPS_24, &s);
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { format: Format::MxfOp1a, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { bitrate_mode: BitrateMode::Crf, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { h264_pass: H264Pass::First, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { h264_intra_only: true, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { h264_profile: H264Profile::High10, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { signal: ColorSignal::PQ, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { signal: ColorSignal::HLG, ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { pixel_aspect: Some((4, 3)), ..s.clone() });
        declined(Format::H264, 1280, 720, FrameRate::FPS_24, &ExportSettings { field_order: FieldOrder::UpperFirst, ..s.clone() });
        declined(Format::H264, 0, 720, FrameRate::FPS_24, &s);
        declined(Format::H264, 641, 360, FrameRate::FPS_24, &s);
        declined(Format::H264, 8194, 720, FrameRate::FPS_24, &s);
        declined(Format::H264, 1280, 720, FrameRate { num: 0, den: 1 }, &s);
        declined(Format::H264, 1280, 720, FrameRate { num: 24, den: 0 }, &s);
    }

    #[test]
    fn config_maps_profiles_and_declines_unwritable_hevc_and_av1_settings() {
        let h264_base = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, ..Default::default() };
        for (hp, qp) in [(H264Profile::Baseline, Profile::H264Baseline), (H264Profile::Main, Profile::H264Main), (H264Profile::High, Profile::H264High)] {
            let s = ExportSettings { h264_profile: hp, ..h264_base.clone() };
            let c = config(Format::H264, 1280, 720, FrameRate::FPS_24, &s).unwrap();
            assert_eq!((c.codec, c.profile, c.ten_bit), (Codec::H264, qp, false));
        }

        for (fmt, codec, sdr_p, hdr_p) in
            [(Format::Hevc, Codec::Hevc, Profile::HevcMain, Profile::HevcMain10), (Format::Av1, Codec::Av1, Profile::Av1Main, Profile::Av1Main10)]
        {
            let sdr = ExportSettings { format: fmt, ..Default::default() };
            let c_sdr = config(fmt, 1920, 1080, FrameRate::FPS_24, &sdr).unwrap();
            assert_eq!((c_sdr.codec, c_sdr.profile, c_sdr.ten_bit, c_sdr.keyint), (codec, sdr_p, false, 48));

            for sig in [ColorSignal::PQ, ColorSignal::HLG] {
                let c_hdr = config(fmt, 1920, 1080, FrameRate::FPS_24, &ExportSettings { signal: sig, ..sdr.clone() }).unwrap();
                assert_eq!((c_hdr.codec, c_hdr.profile, c_hdr.ten_bit), (codec, hdr_p, true));
            }

            for bad in [
                ExportSettings { signal: ColorSignal { primaries: 9, transfer: 1, matrix: 1 }, ..sdr.clone() },
                ExportSettings { signal: ColorSignal { primaries: 1, transfer: 16, matrix: 9 }, ..sdr.clone() },
                ExportSettings { bitrate_mode: BitrateMode::Crf, ..sdr.clone() },
                ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..sdr.clone() },
                ExportSettings { h264_pass: H264Pass::First, ..sdr.clone() },
                ExportSettings { pixel_aspect: Some((4, 3)), ..sdr.clone() },
                ExportSettings { field_order: FieldOrder::LowerFirst, ..sdr.clone() },
            ] {
                assert!(config(fmt, 1280, 720, FrameRate::FPS_24, &bad).is_err());
            }
        }
    }

    #[test]
    fn hevc_and_av1_declined_paths_respect_device_and_codec_availability() {
        for fmt in [Format::Hevc, Format::Av1] {
            let bad = ExportSettings { format: fmt, bitrate_mode: BitrateMode::Crf, ..Default::default() };
            // No Intel device -> silently returns None so another encoder can handle the request.
            assert!(factory_with(fmt, 1280, 720, FrameRate::FPS_24, &bad, || false, |_| true, |_| panic!("no device")).is_none());
            // Intel device present + codec unavailable -> also returns None.
            assert!(factory_with(fmt, 1280, 720, FrameRate::FPS_24, &bad, || true, |_| false, |_| panic!("must decline")).is_none());
            // Intel device present + codec available + invalid settings -> Unsupported error naming QSV.
            let err = match factory_with(fmt, 1280, 720, FrameRate::FPS_24, &bad, || true, |_| true, |_| panic!("must decline")) {
                Some(Err(e)) => e.to_string(),
                _ => panic!("expected Some(Err(_)) for {fmt:?}"),
            };
            assert!(err.contains("Intel Quick Sync"), "{err}");

            // Valid settings reach session open and map failure when codec_available == true.
            let ok = ExportSettings { format: fmt, ..Default::default() };
            let mut opened = None;
            let res = factory_with(
                fmt,
                1280,
                720,
                FrameRate::FPS_24,
                &ok,
                || true,
                |c| c != Codec::H264,
                |cfg| {
                    opened = Some(cfg.profile);
                    Err("simulated open failure".into())
                },
            );
            assert!(opened.is_some());
            assert!(matches!(res, Some(Err(ExportError::Unsupported(_)))));
        }
    }
}
