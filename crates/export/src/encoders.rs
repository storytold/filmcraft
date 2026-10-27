use super::convert::{rgba_to_yuv420_8, rgba_to_yuv422_10, rgbf_to_yuv420_8, rgbf_to_yuv422_10, to_yuv444_10};
use super::format::{ColorSignal, DEFAULT_CRF, ExportSettings, Format, H264Pass};
use super::registry::{AudioEncoder, EncodedPacket, EncoderFrame, PrepareAhead, VideoEncoder};
use super::stats::{Stage, timed};
use super::*;

struct MjpegEncoder {
    w: u16,
    h: u16,
    quality: u8,
    rate: FrameRate,
}

impl VideoEncoder for MjpegEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::jpeg(self.w, self.h)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let rgb: Vec<u8> = f.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, self.quality);
        enc.encode(&rgb, f.width, f.height, image::ExtendedColorType::Rgb8).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data: out, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

pub(crate) fn mjpeg_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Mjpeg).then(|| Ok(Box::new(MjpegEncoder { w: w as u16, h: h as u16, quality: s.quality.clamp(1, 100), rate }) as Box<dyn VideoEncoder>))
}

/// AAC-LC (our encoder), 320 kbps stereo by default.
struct AacEncoder {
    enc: filmcraft_aac::Encoder,
    rate: u32,
    channels: u32,
}

impl AudioEncoder for AacEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::aac(self.enc.audio_specific_config(), self.channels, self.rate)
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let refs: Vec<&[f32]> = if planar.len() == 6 {
            // ours: L, R, C, LFE, Ls, Rs → AAC channel configuration 6: C, L, R, Ls, Rs, LFE
            [2usize, 0, 1, 4, 5, 3].iter().map(|&c| planar[c].as_slice()).collect()
        } else {
            planar.iter().map(Vec::as_slice).collect()
        };
        Ok(self.enc.encode(&refs))
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        Ok(self.enc.flush())
    }
    fn priming(&self) -> u32 {
        self.enc.priming_samples()
    }
    fn frame_size(&self) -> u32 {
        1024
    }
}

/// FLAC (our encoder) in MP4 / QuickTime: one frame of 4096 samples per access unit.
pub(crate) struct FlacAudio {
    enc: filmcraft_flac::Encoder,
    format: SampleFormat,
    /// Samples of the frame `flush` returns (shorter than a full block).
    tail: u32,
}

impl FlacAudio {
    pub(crate) fn new(sample_rate: u32, channels: u32, s: &ExportSettings) -> Result<Self> {
        let format = s.audio.sample_format();
        let ch = u8::try_from(channels).map_err(|_| ExportError::Unsupported("FLAC: too many audio channels".into()))?;
        let mut cfg = filmcraft_flac::EncoderConfig::new(sample_rate, ch, format.bits() as u8);
        cfg.level = s.audio.flac_level.min(8);
        let enc = filmcraft_flac::Encoder::new(cfg).map_err(|e| ExportError::Unsupported(format!("FLAC: {e}")))?;
        Ok(FlacAudio { enc, format, tail: 0 })
    }
}

impl AudioEncoder for FlacAudio {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::flac(&self.enc.streaminfo())
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let ints: Vec<Vec<i32>> = planar.iter().map(|c| c.iter().map(|&s| self.format.quantise(s)).collect()).collect();
        let refs: Vec<&[i32]> = ints.iter().map(Vec::as_slice).collect();
        self.enc.encode_frames(&refs).map_err(|e| ExportError::Encode(format!("FLAC: {e}")))
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        Ok(self.flush_timed()?.into_iter().map(|(au, _)| au).collect())
    }
    fn flush_timed(&mut self) -> Result<Vec<(Vec<u8>, u32)>> {
        self.tail = self.enc.pending_samples() as u32;
        let last = self.enc.finish();
        Ok(if last.is_empty() { Vec::new() } else { vec![(last, self.tail)] })
    }
    fn priming(&self) -> u32 {
        0
    }
    fn frame_size(&self) -> u32 {
        4096
    }
    fn final_sample_entry(&self) -> Option<SampleEntry> {
        Some(SampleEntry::flac(&self.enc.streaminfo()))
    }
}

pub(crate) fn aac_factory(_format: Format, sample_rate: u32, channels: u32, s: &ExportSettings) -> Option<Result<Box<dyn AudioEncoder>>> {
    // AAC caps a frame at 6144 bits per channel (ISO/IEC 14496-3 §4.5.3.2): 6 bits per sample, so
    // 264.6 kbps for stereo at 22.05 kHz; a higher setting is capped instead of refused
    let max = (6 * sample_rate as u64 * channels.max(1) as u64).min(u32::MAX as u64) as u32;
    let bps = (s.audio.bitrate_kbps.clamp(32, 512) * 1000).min(max);
    Some(
        filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(sample_rate, channels as usize, bps))
            .map(|enc| Box::new(AacEncoder { enc, rate: sample_rate, channels }) as Box<dyn AudioEncoder>)
            .map_err(|e| ExportError::Encode(e.to_string())),
    )
}

/// ProRes 422 encoder (HQ unless the settings pick another flavour): sRGB/709 RGBA8 → 10-bit limited-range BT.709 4:2:2.
struct ProResEncoder {
    enc: filmcraft_prores::Encoder,
    profile: filmcraft_prores::Profile,
    /// Code the frames' alpha channel (4444 profiles with Include Alpha Channel).
    alpha: bool,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for ProResEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let mut e = SampleEntry::prores(filmcraft_isobmff::FourCc(self.profile.fourcc()), self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let chroma = self.profile.chroma();
        let mut fr = filmcraft_prores::Frame::new(f.width, f.height, chroma, 10, self.alpha);
        let (w, h) = (f.width as usize, f.height as usize);
        timed(Stage::Convert, || match (chroma, f.hdr) {
            (filmcraft_prores::ChromaFormat::Yuv444, hdr) => {
                let (kr, kb) = if hdr.is_some() { self.signal.kr_kb() } else { (0.2126, 0.0722) };
                to_yuv444_10(f.rgba, hdr, w, h, kr, kb, &mut fr.y, &mut fr.cb, &mut fr.cr, fr.alpha.as_deref_mut())
            }
            (_, Some(rgb)) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, w, h, kr, kb, &mut fr.y, &mut fr.cb, &mut fr.cr)
            }
            (_, None) => rgba_to_yuv422_10(f.rgba, w, h, &mut fr.y, &mut fr.cb, &mut fr.cr),
        });
        let data = self.enc.encode(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// The ProRes profile named by [`ExportSettings::prores_profile`].
pub fn prores_profile(name: &str) -> filmcraft_prores::Profile {
    use filmcraft_prores::Profile;
    match name.to_ascii_lowercase().as_str() {
        "proxy" => Profile::Proxy,
        "lt" => Profile::Lt,
        "standard" | "422" => Profile::Standard,
        "4444" => Profile::P4444,
        "4444xq" | "4444 xq" | "4444-xq" | "xq" => Profile::P4444Xq,
        _ => Profile::Hq,
    }
}

pub(crate) fn prores_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::ProRes).then(|| {
        let profile = prores_profile(&s.prores_profile);
        let mut cfg = filmcraft_prores::EncoderConfig::new(profile, w, h);
        if s.signal.is_hdr() {
            cfg.color = filmcraft_prores::ColorInfo { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix };
        }
        let alpha = s.keeps_alpha();
        cfg.encode_alpha = alpha;
        let enc = filmcraft_prores::Encoder::with_config(cfg);
        Ok(Box::new(ProResEncoder { enc, profile, alpha, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// The DNxHR profile named by [`ExportSettings::dnx_profile`].
pub fn dnx_profile(name: &str) -> filmcraft_dnx::Profile {
    use filmcraft_dnx::Profile;
    match name.to_ascii_lowercase().as_str() {
        "lb" => Profile::Lb,
        "sq" => Profile::Sq,
        "hqx" => Profile::Hqx,
        _ => Profile::Hq,
    }
}

/// DNxHR encoder (RGBA8 or HDR floats → BT.709 / BT.2020 limited-range 4:2:2; 8-bit for
/// LB/SQ/HQ, 10-bit for HQX).
struct DnxEncoder {
    enc: filmcraft_dnx::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for DnxEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let mut e = SampleEntry::dnx(filmcraft_isobmff::FourCc(*b"AVdh"), self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let mut fr = filmcraft_dnx::Frame::new(f.width, f.height, filmcraft_dnx::ChromaFormat::Yuv422, 10, false);
        match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, f.width as usize, f.height as usize, kr, kb, &mut fr.y, &mut fr.cb, &mut fr.cr)
            }
            None => rgba_to_yuv422_10(f.rgba, f.width as usize, f.height as usize, &mut fr.y, &mut fr.cb, &mut fr.cr),
        }
        // the encoder rescales 10-bit input to its coded depth
        let data = self.enc.encode(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

pub(crate) fn dnx_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::DnxHr).then(|| {
        let mut cfg = filmcraft_dnx::EncoderConfig::new(dnx_profile(&s.dnx_profile), w, h);
        if s.signal.primaries == 9 {
            cfg.color_volume = filmcraft_dnx::ColorVolume::Bt2020Ncl;
        }
        let enc = filmcraft_dnx::Encoder::with_config(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(Box::new(DnxEncoder { enc, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// The APV profile named by [`ExportSettings::apv_profile`].
pub fn apv_profile(name: &str) -> filmcraft_apv::Profile {
    use filmcraft_apv::Profile;
    match name.to_ascii_lowercase().replace([' ', '_'], "-").trim_start_matches("apv-").trim_start_matches("apv") {
        "422-12" | "42212" => Profile::P422_12,
        "444-10" | "44410" => Profile::P444_10,
        "444-12" | "44412" => Profile::P444_12,
        "4444-10" | "444410" => Profile::P4444_10,
        "4444-12" | "444412" => Profile::P4444_12,
        "400-10" | "40010" => Profile::P400_10,
        _ => Profile::P422_10,
    }
}

/// APV encoder (RFC 9924: 10/12-bit 4:2:2 or 4:4:4 intra).
struct ApvEncoder {
    enc: filmcraft_apv::Encoder,
    chroma: filmcraft_apv::ChromaFormat,
    w: u32,
    h: u32,
    rate: FrameRate,
    signal: ColorSignal,
}

impl VideoEncoder for ApvEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let apvc = filmcraft_isobmff::ApvConfig::parse(&self.enc.decoder_config_record()).unwrap_or_default();
        let mut e = SampleEntry::apv(apvc, self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, true);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let (w, h) = (f.width as usize, f.height as usize);
        let mut fr = filmcraft_apv::Frame::new(f.width, f.height, self.chroma, 10, self.chroma == filmcraft_apv::ChromaFormat::Yuv4444);
        let mut y422 = vec![0u16; w * h];
        let cw = w.div_ceil(2);
        let mut cb422 = vec![0u16; cw * h];
        let mut cr422 = vec![0u16; cw * h];
        match f.hdr {
            Some(rgb) => {
                let (kr, kb) = self.signal.kr_kb();
                rgbf_to_yuv422_10(rgb, w, h, kr, kb, &mut y422, &mut cb422, &mut cr422);
            }
            None => rgba_to_yuv422_10(f.rgba, w, h, &mut y422, &mut cb422, &mut cr422),
        }
        fr.y = y422;
        match self.chroma {
            filmcraft_apv::ChromaFormat::Monochrome => {
                fr.cb.clear();
                fr.cr.clear();
            }
            filmcraft_apv::ChromaFormat::Yuv422 => {
                fr.cb = cb422;
                fr.cr = cr422;
            }
            filmcraft_apv::ChromaFormat::Yuv444 | filmcraft_apv::ChromaFormat::Yuv4444 => {
                for row in 0..h {
                    for x in 0..w {
                        fr.cb[row * w + x] = cb422[row * cw + x / 2];
                        fr.cr[row * w + x] = cr422[row * cw + x / 2];
                    }
                }
                if let Some(a) = fr.alpha.as_mut() {
                    for (i, dst) in a.iter_mut().enumerate() {
                        *dst = f.rgba.get(i * 4 + 3).map_or(1023, |&v| ((v as u32 * 1023 + 127) / 255) as u16);
                    }
                }
            }
        }
        let data = self.enc.encode_raw_au(&fr).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(vec![EncodedPacket { data, key: true, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

pub(crate) fn apv_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Apv).then(|| {
        let profile = apv_profile(&s.apv_profile);
        let mut cfg = filmcraft_apv::EncoderConfig::new(profile, w, h);
        if s.signal.is_hdr() {
            cfg.color = filmcraft_apv::ColorInfo { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix, full_range: false };
        }
        let chroma = cfg.chroma;
        let enc = filmcraft_apv::Encoder::with_config(cfg).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(Box::new(ApvEncoder { enc, chroma, w, h, rate, signal: s.signal }) as Box<dyn VideoEncoder>)
    })
}

/// H.264 High (our encoder): sRGB/709 RGBA8 → 8-bit limited-range BT.709 4:2:0, VBR at the
/// requested bitrate, length-prefixed samples with the `avcC` in the sample entry.
struct H264Encoder {
    enc: filmcraft_h264enc::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    signal: ColorSignal,
    /// 4:2:0 planes the render threads converted ahead ([`VideoEncoder::prepare_ahead`]), by
    /// picture index.
    ready: Arc<Mutex<std::collections::HashMap<u64, Yuv420>>>,
}

type Yuv420 = (Vec<u8>, Vec<u8>, Vec<u8>);

/// The encoder input of `f`: 8-bit limited-range 4:2:0 in the export's colour signal.
fn h264_input(f: &EncoderFrame, signal: ColorSignal, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    match f.hdr {
        Some(rgb) => {
            let (kr, kb) = signal.kr_kb();
            rgbf_to_yuv420_8(rgb, f.width as usize, f.height as usize, kr, kb, y, u, v)
        }
        None => rgba_to_yuv420_8(f.rgba, f.width as usize, f.height as usize, y, u, v),
    }
}

impl H264Encoder {
    fn packets(&self, ps: Vec<filmcraft_h264enc::Packet>) -> Vec<EncodedPacket> {
        ps.into_iter()
            .map(|p| EncodedPacket { data: p.data, key: p.keyframe, duration: self.rate.den as u32, composition_offset: (p.pts - p.dts) as i32 })
            .collect()
    }
}

impl VideoEncoder for H264Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let cfg = filmcraft_isobmff::AvcConfig::parse(&self.enc.avcc()).unwrap_or_else(|_| {
            let (sps, pps) = self.enc.sps_pps();
            filmcraft_isobmff::AvcConfig::new(vec![sps], vec![pps], 4)
        });
        let mut e = SampleEntry::avc(cfg, self.w as u16, self.h as u16);
        self.signal.apply_to(&mut e, false);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let ready = {
            let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
            let mine = ready.remove(&f.index);
            // anything older belongs to a batch that was rendered and then not encoded
            ready.retain(|&i, _| i > f.index);
            mine
        };
        let (w, h) = (f.width as usize, f.height as usize);
        let chroma = w.div_ceil(2) * h.div_ceil(2);
        match ready.filter(|(y, u, v)| y.len() == w * h && u.len() == chroma && v.len() == chroma) {
            Some((y, u, v)) => (self.y, self.u, self.v) = (y, u, v),
            None => {
                let (enc, y, u, v, signal) = (&self.enc, &mut self.y, &mut self.u, &mut self.v, self.signal);
                // on the encoder's own threads: the global pool is busy rendering the next batch
                timed(Stage::Convert, || enc.install(|| h264_input(f, signal, y, u, v)));
            }
        }
        let cw = (f.width as usize).div_ceil(2);
        let frame = filmcraft_h264enc::YuvFrame { y: &self.y, u: &self.u, v: &self.v, y_stride: f.width as usize, uv_stride: cw };
        let ps = self.enc.try_encode(&frame, f.index as i64 * self.rate.den).map_err(|e| ExportError::Encode(e.to_string()))?;
        Ok(self.packets(ps))
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let ps = self.enc.flush();
        Ok(self.packets(ps))
    }
    fn media_start(&self) -> Option<i64> {
        // With B-frames the first DTS is one frame before the first PTS.
        (self.enc.delay() > 0).then_some(self.rate.den)
    }
    fn pass_stats(&self) -> Option<filmcraft_h264enc::PassStats> {
        self.enc.pass_stats()
    }
    fn prepare_ahead(&self) -> Option<PrepareAhead> {
        let (ready, signal) = (self.ready.clone(), self.signal);
        Some(Arc::new(move |f: &EncoderFrame| {
            let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
            timed(Stage::Convert, || h264_input(f, signal, &mut y, &mut u, &mut v));
            ready.lock().unwrap_or_else(PoisonError::into_inner).insert(f.index, (y, u, v));
        }))
    }
}

/// Slices per H.264 picture: one per four macroblock rows. The encoder's default follows the core
/// count, which would make the stream (and every decoded picture) depend on the machine; this is
/// what it picks on a machine with enough cores.
pub(crate) fn h264_slices(height: u32) -> usize {
    (height.div_ceil(16) as usize).div_ceil(4).max(1)
}

pub(crate) fn h264_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 {
        return None;
    }
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(w, h, rate.num as u32, rate.den as u32);
    // MXF carries the Annex B byte stream (ST 381-3) with in-band parameter sets
    cfg.format = if s.format.is_mxf() { filmcraft_h264enc::PacketFormat::AnnexB } else { filmcraft_h264enc::PacketFormat::LengthPrefixed };
    cfg.aud = s.format.is_mxf();
    cfg.keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (rate.num as f64 / rate.den as f64 * 2.0).round().max(1.0) as u32);
    cfg.slices = h264_slices(h);
    let kbps = s.bitrate_kbps.max(100);
    let max = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or_else(|| (u64::from(kbps) * 3 / 2).min(u64::from(u32::MAX)) as u32);
    cfg.rate = match s.bitrate_mode {
        BitrateMode::Cbr => filmcraft_h264enc::RateControl::Cbr { kbps },
        // the encoder clamps to 0..=51; a NaN from a hand-edited preset becomes the default
        BitrateMode::Crf => filmcraft_h264enc::RateControl::Crf(if s.crf.is_finite() { s.crf } else { DEFAULT_CRF }),
        _ => filmcraft_h264enc::RateControl::Vbr { target_kbps: kbps, max_kbps: max },
    };
    cfg.pass = match &s.h264_pass {
        H264Pass::Single => filmcraft_h264enc::Pass::Single,
        H264Pass::First => filmcraft_h264enc::Pass::First,
        H264Pass::Second(st) => filmcraft_h264enc::Pass::Second(st.clone()),
    };
    cfg.profile = match s.h264_profile {
        H264Profile::Baseline => filmcraft_h264enc::Profile::Baseline,
        H264Profile::Main => filmcraft_h264enc::Profile::Main,
        H264Profile::High => filmcraft_h264enc::Profile::High,
    };
    cfg.level = s.h264_level;
    if let Some((n, d)) = s.pixel_aspect {
        cfg.sar = (n.clamp(1, 65535) as u16, d.clamp(1, 65535) as u16);
    }
    if s.signal.is_hdr() {
        cfg.color = filmcraft_h264enc::ColorConfig { primaries: s.signal.primaries, transfer: s.signal.transfer, matrix: s.signal.matrix, full_range: false };
        if let Some((md, cll)) = s.signal.static_metadata() {
            let mut b = [0u8; 24];
            b.copy_from_slice(&md.to_bytes());
            cfg.mastering_display = Some(b);
            cfg.content_light = Some(cll);
        }
    }
    Some(
        filmcraft_h264enc::Encoder::new(cfg)
            .map(|enc| {
                Box::new(H264Encoder { enc, w, h, rate, y: Vec::new(), u: Vec::new(), v: Vec::new(), signal: s.signal, ready: Default::default() })
                    as Box<dyn VideoEncoder>
            })
            .map_err(|e| ExportError::Encode(e.to_string())),
    )
}
