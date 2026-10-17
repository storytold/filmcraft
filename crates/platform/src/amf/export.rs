//! AMD AMF integration with FilmCraft export pipeline.

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{Av1Config, AvcConfig, HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::{Codec, Config, Profile, Session};

pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    let codec = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto => Codec::H264,
        Format::Hevc => Codec::Hevc,
        Format::Av1 => Codec::Av1,
        _ => return None,
    };

    if s.format.is_mxf() {
        return None;
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single) {
        return None;
    }
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return None;
    }

    let timescale = u32::try_from(rate.num).ok().filter(|&n| n > 0)?;
    let frame_duration = u32::try_from(rate.den).ok().filter(|&d| d > 0)?;
    let fps_val = f64::from(timescale) / f64::from(frame_duration);
    let keyint = s.keyframe_distance.filter(|&k| k > 0).unwrap_or_else(|| (fps_val * 2.0).round().max(1.0) as u32);

    let kbps = s.bitrate_kbps.max(100);
    let max_kbps = s.max_bitrate_kbps.filter(|&m| m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    let cbr = s.bitrate_mode == BitrateMode::Cbr;
    let hdr = s.signal.is_hdr();

    let profile = match codec {
        Codec::H264 => match s.h264_profile {
            H264Profile::Baseline => Profile::H264Baseline,
            H264Profile::Main => Profile::H264Main,
            H264Profile::High => Profile::H264High,
        },
        Codec::Hevc => {
            if hdr {
                Profile::HevcMain10
            } else {
                Profile::HevcMain
            }
        }
        Codec::Av1 => {
            if hdr {
                Profile::Av1Main10
            } else {
                Profile::Av1Main
            }
        }
    };

    let cfg = Config { width: w, height: h, fps: (timescale, frame_duration), bitrate_kbps: kbps, max_bitrate_kbps: max_kbps, cbr, keyint, profile };

    let session = match Session::open(&cfg) {
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
                if p.key && (self.sps.is_empty() || (codec == Codec::Hevc && self.hvcc.is_none())) {
                    if codec == Codec::H264 {
                        if let Ok((s, pp)) = crate::nvenc::split_parameter_sets(&p.data) {
                            self.sps = s;
                            self.pps = pp;
                        }
                    } else if codec == Codec::Hevc
                        && let Ok((v, s, pp)) = crate::nvenc::split_hevc_parameter_sets(&p.data)
                    {
                        let ten_bit = self.session.is_ten_bit();
                        if let Ok(rec) = crate::nvenc::hevc::hevc_config(&v, &s, &pp, (self.w, self.h), if ten_bit { 10 } else { 8 }) {
                            self.vps = v;
                            self.sps = s;
                            self.pps = pp;
                            self.hvcc = Some(rec);
                        }
                    }
                }
            } else if self.av1c.is_none() {
                self.av1c = Some(filmcraft_isobmff::Av1Config {
                    seq_profile: 0,
                    seq_level_idx_0: 0,
                    seq_tier_0: false,
                    high_bitdepth: self.session.is_ten_bit(),
                    twelve_bit: false,
                    monochrome: false,
                    chroma_subsampling_x: true,
                    chroma_subsampling_y: true,
                    chroma_sample_position: 0,
                    initial_presentation_delay_minus_one: None,
                    config_obus: Vec::new(),
                });
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
                    filmcraft_export::rgbf_to_yuv420_10(rgb, self.w as usize, self.h as usize, kr, kb, &mut self.y16, &mut self.u16, &mut self.v16)
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
