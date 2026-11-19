//! Intel Quick Sync Video export encoder for FilmCraft.
//!
//! [`factory`] is registered with `filmcraft_export::register_encoder`.

use filmcraft_export::{ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, HardwareEncoding, Result, VideoEncoder};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::device::IntelGpu;
use super::session::{Codec, Config, Packet, Profile, QsvSession};

/// The Intel Quick Sync Video export encoder.
pub struct QsvEncoder {
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

        let packets = match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
                    filmcraft_export::rgbf_to_yuv420_10(rgb, w, h, filmcraft_export::Matrix::new(kr, kb), &mut self.y16, &mut self.u16, &mut self.v16)
                })?;
                self.session.encode_p010(&self.y16, &self.u16, &self.v16, pts).map_err(|e| ExportError::Encode(format!("Intel encode failed: {e}")))?
            }
            None => {
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

/// The export encoder factory registered with `filmcraft_export::register_encoder`.
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    let codec = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto && !s.signal.is_hdr() => Codec::H264,
        Format::Hevc => Codec::Hevc,
        Format::Av1 => Codec::Av1,
        _ => return None,
    };

    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || w > 8192 || h > 8192 {
        return None;
    }

    // Check if an Intel GPU is physically present
    IntelGpu::probe().ok().flatten()?;

    let is_hdr = s.signal.is_hdr();
    let profile = match (codec, is_hdr) {
        (Codec::H264, _) => Profile::H264High,
        (Codec::Hevc, true) => Profile::HevcMain10,
        (Codec::Hevc, false) => Profile::HevcMain,
        (Codec::Av1, true) => Profile::Av1Main10,
        (Codec::Av1, false) => Profile::Av1Main,
    };

    let cfg = Config { codec, profile, size: (w, h), rate, bitrate_kbps: s.bitrate_kbps.max(100), keyint: s.keyframe_distance.unwrap_or(60), ten_bit: is_hdr };

    let session = match QsvSession::new(cfg) {
        Ok(sess) => sess,
        Err(e) => {
            log::info!("Intel Quick Sync encoder declined: {e}");
            filmcraft_export::note_hw_encode_declined();
            return None;
        }
    };

    filmcraft_export::note_hw_encode_session();
    Some(Ok(Box::new(QsvEncoder::new(session, codec, w, h, rate, s.signal, is_hdr))))
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
}
