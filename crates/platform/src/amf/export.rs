//! AMD AMF integration with FilmCraft export pipeline.

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{Av1Config, AvcConfig, HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::{Codec, Config, Profile, Session};

pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    let codec = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto && !s.signal.is_hdr() => Codec::H264,
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
            H264Profile::High10 => return None,
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
