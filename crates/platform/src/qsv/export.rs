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
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y16: Vec<u16>,
    u16: Vec<u16>,
    v16: Vec<u16>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    vps: Vec<u8>,
}

impl QsvEncoder {
    fn new(session: QsvSession, codec: Codec, w: u32, h: u32, rate: FrameRate, signal: ColorSignal) -> Self {
        Self {
            session,
            codec,
            w,
            h,
            rate,
            signal,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
            y16: Vec::new(),
            u16: Vec::new(),
            v16: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
            vps: Vec::new(),
        }
    }

    fn make_encoded_packets(&mut self, ps: Vec<Packet>) -> Vec<EncodedPacket> {
        let den = self.rate.den;
        let duration = u32::try_from(den).unwrap_or(u32::MAX);

        let mut out = Vec::with_capacity(ps.len());
        for p in ps {
            self.extract_headers(&p.data);
            let comp_offset = i32::try_from((p.pts.saturating_sub(p.dts)).saturating_mul(den)).unwrap_or(0);
            out.push(EncodedPacket { data: p.data, key: p.key, duration, composition_offset: comp_offset });
        }
        out
    }

    fn extract_headers(&mut self, data: &[u8]) {
        // Parse Annex B NALs to extract parameter sets for SampleEntry
        let mut i = 0usize;
        let mut nal_starts = Vec::new();
        while let Some(w) = data.get(i..i.saturating_add(4)) {
            if w == [0, 0, 0, 1] {
                nal_starts.push(i + 4);
                i += 4;
            } else {
                i += 1;
            }
        }
        for (idx, &start) in nal_starts.iter().enumerate() {
            let end = nal_starts.get(idx + 1).copied().unwrap_or(data.len());
            let mut end_clean = end;
            while end_clean > start && data.get(end_clean.saturating_sub(1)) == Some(&0) {
                end_clean -= 1;
            }
            if let Some(nal) = data.get(start..end_clean)
                && let Some(&first) = nal.first()
            {
                match self.codec {
                    Codec::H264 => {
                        let nal_type = first & 0x1F;
                        if nal_type == 7 && self.sps.is_empty() {
                            self.sps = nal.to_vec();
                        } else if nal_type == 8 && self.pps.is_empty() {
                            self.pps = nal.to_vec();
                        }
                    }
                    Codec::Hevc => {
                        let nal_type = (first >> 1) & 0x3F;
                        if nal_type == 32 && self.vps.is_empty() {
                            self.vps = nal.to_vec();
                        } else if nal_type == 33 && self.sps.is_empty() {
                            self.sps = nal.to_vec();
                        } else if nal_type == 34 && self.pps.is_empty() {
                            self.pps = nal.to_vec();
                        }
                    }
                    Codec::Av1 => {}
                }
            }
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
                    filmcraft_export::rgbf_to_yuv420_10(rgb, w, h, kr, kb, &mut self.y16, &mut self.u16, &mut self.v16)
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
        Ok(self.make_encoded_packets(packets))
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let packets = self.session.flush().map_err(|e| ExportError::Encode(format!("Intel flush failed: {e}")))?;
        Ok(self.make_encoded_packets(packets))
    }

    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.w).unwrap_or(u16::MAX), u16::try_from(self.h).unwrap_or(u16::MAX));
        match self.codec {
            Codec::H264 => {
                let sps = if self.sps.is_empty() { vec![0x67, 0x64, 0x00, 0x1f] } else { self.sps.clone() };
                let pps = if self.pps.is_empty() { vec![0x68, 0xe8, 0x43, 0x8f] } else { self.pps.clone() };
                SampleEntry::avc(AvcConfig::new(vec![sps], vec![pps], 4), w, h)
            }
            Codec::Hevc => {
                let mut e = SampleEntry::hevc(filmcraft_isobmff::HevcConfig::default(), w, h);
                self.signal.apply_to(&mut e, false);
                e
            }
            Codec::Av1 => {
                let mut e = SampleEntry::av1(filmcraft_isobmff::Av1Config::default(), w, h);
                self.signal.apply_to(&mut e, false);
                e
            }
        }
    }

    fn timescale(&self) -> u32 {
        u32::try_from(self.rate.num).unwrap_or(u32::MAX)
    }

    fn media_start(&self) -> Option<i64> {
        None
    }
}

/// The export encoder factory registered with `filmcraft_export::register_encoder`.
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    let codec = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto => Codec::H264,
        Format::Hevc => Codec::Hevc,
        Format::Av1 => Codec::Av1,
        _ => return None,
    };

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
    Some(Ok(Box::new(QsvEncoder::new(session, codec, w, h, rate, s.signal))))
}
