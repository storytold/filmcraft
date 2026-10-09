//! Audio decoding: PCM directly; compressed codecs through symphonia (bootstrap, MPL-2.0 unmodified).

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, SharedSource};
use filmcraft_time::Tick;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_ALAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CodecParameters, CodecType, Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, Packet};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{CodecError, Result};

enum Inner {
    /// Our own AAC-LC decoder. `up` interpolates HE-AAC's core to the output rate (see [`Upsample2x`]).
    #[cfg(feature = "aac")]
    Aac { dec: Box<filmcraft_aac::Decoder>, asc: Vec<u8>, up: Option<Box<Upsample2x>> },
    /// AAC whose configuration arrives with the first frame (ADTS / LATM).
    AacPending,
    /// Our own Opus decoder (always 48 kHz output; pre-skip is left to container timestamps).
    /// `order` maps output channel → decoded channel (Vorbis → WAV/SMPTE order for surround).
    Opus { dec: Box<filmcraft_opus::Decoder>, order: Option<&'static [usize]> },
    /// Bootstrap decoders (MP3, ALAC, FLAC, HE-AAC…) via symphonia.
    Symphonia(Box<dyn Decoder>),
    /// Our AC-3 decoder (ATSC A/52).
    Ac3(Box<filmcraft_ac3::Decoder>),
}

/// Opus always decodes at 48 kHz (RFC 7845 §5.1: the input rate in the header is informational).
pub const OPUS_RATE: u32 = 48_000;

/// Opus pre-roll decoded before a random-access target, in 48 kHz frames: 320 ms.
///
/// The containers' 80 ms (RFC 7845 §4.6, Matroska `SeekPreRoll`) is enough to sound right, but the
/// CELT energy predictor converges only ~24 dB per 80 ms: measured against a continuous decode, a
/// cold start 80 ms early is ~26 dB off, 320 ms early ~89 dB (float-exact). Seeking must give the
/// same samples as playback (render caches, scrubbing), so we pay the extra few packets.
pub const OPUS_PRE_ROLL: u32 = 4 * 3840;

/// Channel order for Vorbis-ordered surround (Opus mapping family 1, RFC 7845 §5.1.1.2): output
/// channel → decoded channel, giving the WAV/SMPTE order (L R C LFE back… side…) used elsewhere.
fn vorbis_to_wav_order(channels: usize) -> Option<&'static [usize]> {
    Some(match channels {
        3 => &[0, 2, 1],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 5, 3, 4],
        7 => &[0, 2, 1, 6, 5, 3, 4],
        8 => &[0, 2, 1, 7, 5, 6, 3, 4],
        _ => return None,
    })
}

/// Duration of an Opus packet in 48 kHz samples from its TOC and frame count (RFC 6716 §3.1),
/// without parsing frame lengths. For a multistream packet this is the first stream's TOC, which
/// every stream shares. `None` for an empty or invalid (> 120 ms) packet.
pub fn opus_packet_samples(data: &[u8]) -> Option<usize> {
    let toc = filmcraft_opus::Toc::parse(*data.first()?);
    let frames = match data[0] & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => (*data.get(1)? & 0x3F) as usize,
    };
    let n = toc.frame_samples_48k() * frames;
    (n > 0 && n <= 5760).then_some(n)
}

/// A packet decoder producing planar f32.
pub struct PacketDecoder {
    inner: Inner,
    pub channels: usize,
}

impl PacketDecoder {
    pub fn new(codec: CodecType, sample_rate: u32, extra: Option<Vec<u8>>) -> Result<Self> {
        let mut p = CodecParameters::new();
        p.for_codec(codec).with_sample_rate(sample_rate);
        if let Some(x) = extra {
            p.with_extra_data(x.into_boxed_slice());
        }
        let dec = symphonia::default::get_codecs().make(&p, &DecoderOptions::default()).map_err(|e| CodecError::Unsupported(e.to_string()))?;
        Ok(Self { inner: Inner::Symphonia(dec), channels: 0 })
    }
    /// AAC: our decoder for AAC-LC and the AAC-LC core of HE-AAC (v1/v2); symphonia for other
    /// object types. `sample_rate` is the stream's output rate: when it is twice the core rate
    /// (HE-AAC, see [`aac_output_rate`]) the decoded core is upsampled to it.
    #[cfg(feature = "aac")]
    pub fn aac(asc: &[u8], sample_rate: u32) -> Result<Self> {
        if let Ok(dec) = filmcraft_aac::Decoder::new(asc) {
            let up = (dec.sample_rate().checked_mul(2) == Some(sample_rate)).then(|| Box::new(Upsample2x::new(dec.channels())));
            return Ok(Self { inner: Inner::Aac { dec: Box::new(dec), asc: asc.to_vec(), up }, channels: 0 });
        }
        Self::new(symphonia::core::codecs::CODEC_TYPE_AAC, sample_rate, Some(asc.to_vec()))
    }
    /// AAC without the `aac` feature: an error naming it.
    #[cfg(not(feature = "aac"))]
    pub fn aac(_asc: &[u8], _sample_rate: u32) -> Result<Self> {
        Err(crate::missing_feature("AAC", "aac"))
    }
    /// Opus from an `OpusHead` (Matroska `CodecPrivate`, Ogg) or a `dOps` box payload. Output is
    /// 48 kHz; pre-skip is *not* trimmed here (Matroska `CodecDelay` / MP4 edit lists do that).
    pub fn opus(head: filmcraft_opus::OpusHead) -> Result<Self> {
        let dec = filmcraft_opus::Decoder::from_head(head, OPUS_RATE).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?;
        let channels = dec.channels();
        let order = if dec.head().mapping_family == 1 { vorbis_to_wav_order(channels) } else { None };
        Ok(Self { inner: Inner::Opus { dec: Box::new(dec), order }, channels })
    }
    /// MPEG-1/2 audio layer I, II or III (symphonia).
    pub fn mpeg_audio(layer: u8, sample_rate: u32) -> Result<Self> {
        let codec = match layer {
            1 => symphonia::core::codecs::CODEC_TYPE_MP1,
            2 => symphonia::core::codecs::CODEC_TYPE_MP2,
            _ => CODEC_TYPE_MP3,
        };
        Self::new(codec, sample_rate, None)
    }
    /// AAC configured by [`Self::ensure_aac`] from in-band headers (ADTS, LATM).
    pub fn lazy_aac() -> Self {
        Self { inner: Inner::AacPending, channels: 0 }
    }
    /// (Re)configure for the AudioSpecificConfig `asc` unless it is the current one.
    #[cfg(feature = "aac")]
    pub fn ensure_aac(&mut self, asc: &[u8]) -> Result<()> {
        if let Inner::Aac { asc: cur, .. } = &self.inner
            && cur == asc
        {
            return Ok(());
        }
        let dec = filmcraft_aac::Decoder::new(asc).map_err(|e| CodecError::Unsupported(format!("AAC: {e}")))?;
        self.inner = Inner::Aac { dec: Box::new(dec), asc: asc.to_vec(), up: None };
        Ok(())
    }
    /// AAC without the `aac` feature: an error naming it.
    #[cfg(not(feature = "aac"))]
    pub fn ensure_aac(&mut self, _asc: &[u8]) -> Result<()> {
        Err(crate::missing_feature("AAC", "aac"))
    }
    /// AC-3 (ATSC A/52).
    pub fn ac3() -> Result<Self> {
        Ok(Self { inner: Inner::Ac3(Box::new(filmcraft_ac3::Decoder::new())), channels: 0 })
    }
    /// Whether this decodes Opus (which needs [`OPUS_PRE_ROLL`] of pre-roll after a seek).
    pub fn is_opus(&self) -> bool {
        matches!(self.inner, Inner::Opus { .. })
    }
    pub fn for_isobmff(c: &filmcraft_isobmff::CodecConfig, rate: u32) -> Result<Self> {
        use filmcraft_isobmff::CodecConfig as C;
        match c {
            C::Aac(a) => Self::aac(&a.asc, if rate > 0 { rate } else { a.sample_rate }),
            C::Mp3 => Self::new(CODEC_TYPE_MP3, rate, None),
            C::Ac3 { .. } => Self::ac3(),
            C::Alac { cookie } => Self::new(CODEC_TYPE_ALAC, rate, Some(cookie.clone())),
            C::Flac(_) => Self::new(CODEC_TYPE_FLAC, rate, None),
            C::Opus(o) => Self::opus(filmcraft_opus::OpusHead::from_dops(&o.to_bytes()).map_err(|e| CodecError::Unsupported(format!("Opus: {e}")))?),
            other => Err(CodecError::Unsupported(format!("{} audio", other.name()))),
        }
    }
    /// Decode one packet into planar channels.
    pub fn decode(&mut self, data: &[u8], ts: u64) -> Result<Vec<Vec<f32>>> {
        let dec = match &mut self.inner {
            #[cfg(feature = "aac")]
            Inner::Aac { dec, up, .. } => {
                let mut out = dec.decode(data).map_err(|e| CodecError::Decode(e.to_string()))?;
                if let Some(up) = up {
                    out = up.process(out);
                }
                self.channels = out.len();
                return Ok(out);
            }
            Inner::Opus { dec, order } => {
                // A corrupt packet is concealed like a lost one (keeps timing and decoder state).
                let mut out = match dec.decode(Some(data)) {
                    Ok(o) => o,
                    Err(_) => dec.decode(None).map_err(|e| CodecError::Decode(e.to_string()))?,
                };
                if let Some(order) = order {
                    out = order.iter().map(|&c| std::mem::take(&mut out[c])).collect();
                }
                self.channels = out.len();
                return Ok(out);
            }
            Inner::Symphonia(d) => d,
            Inner::AacPending => return Err(CodecError::Decode("AAC: no configuration yet".into())),
            Inner::Ac3(d) => {
                // a packet may hold several syncframes (MP4 / Matroska)
                let mut out: Vec<Vec<f32>> = Vec::new();
                let mut p = 0;
                while p < data.len() {
                    let f = d.decode(&data[p..]).map_err(|e| CodecError::Decode(e.to_string()))?;
                    p += f.header.frame_bytes;
                    if out.is_empty() {
                        out = f.channels;
                    } else {
                        for (o, c) in out.iter_mut().zip(f.channels) {
                            o.extend(c);
                        }
                    }
                }
                self.channels = out.len();
                return Ok(out);
            }
        };
        let pkt = Packet::new_from_slice(0, ts, 0, data);
        let buf = dec.decode(&pkt).map_err(|e| CodecError::Decode(e.to_string()))?;
        let spec = *buf.spec();
        let ch = spec.channels.count().max(1);
        self.channels = ch;
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        let s = sb.samples();
        let n = s.len() / ch;
        let mut out = vec![Vec::with_capacity(n); ch];
        for i in 0..n {
            for (c, o) in out.iter_mut().enumerate() {
                o.push(s[i * ch + c]);
            }
        }
        Ok(out)
    }
    pub fn reset(&mut self) {
        match &mut self.inner {
            #[cfg(feature = "aac")]
            Inner::Aac { dec, asc, up } => {
                if let Ok(d) = filmcraft_aac::Decoder::new(asc) {
                    **dec = d;
                }
                if let Some(up) = up {
                    up.reset();
                }
            }
            Inner::Opus { dec, .. } => dec.reset(),
            Inner::Symphonia(d) => d.reset(),
            Inner::Ac3(d) => d.reset(),
            Inner::AacPending => {}
        }
    }
}

/// The output rate of an AAC stream: twice the core rate for HE-AAC (ISO/IEC 14496-3 §1.6.5).
/// Explicit signalling names the rate in the `AudioSpecificConfig`; with implicit signalling (an
/// AAC-LC config at 24 kHz or less) SBR is only visible as fill-element data in the access units,
/// so the first ones are decoded to look for it. `None` if `asc` is not one our decoder reads, or
/// without the `aac` feature.
#[cfg(feature = "aac")]
pub fn aac_output_rate<'a>(asc: &[u8], first_units: impl IntoIterator<Item = &'a [u8]>) -> Option<u32> {
    let mut dec = filmcraft_aac::Decoder::new(asc).ok()?;
    let core = dec.sample_rate();
    if let Some(ext) = dec.config().extension_sample_rate {
        return Some(ext);
    }
    if core <= 24_000 {
        for au in first_units {
            let _ = dec.decode(au);
            if dec.sbr() {
                return Some(core * 2);
            }
        }
    }
    Some(core)
}

#[cfg(not(feature = "aac"))]
pub fn aac_output_rate<'a>(_asc: &[u8], _first_units: impl IntoIterator<Item = &'a [u8]>) -> Option<u32> {
    None
}

/// Taps on each side of the interpolation point in [`Upsample2x`].
const UPSAMPLE_HALF: usize = 12;

/// 2× upsampler for HE-AAC. Our decoder reconstructs the AAC-LC core at half the output rate and
/// not yet the SBR high band, so the core is interpolated to the output rate: timing and pitch are
/// right and the band above the core's Nyquist stays empty. Even outputs are the input samples,
/// odd ones a Blackman-windowed sinc half-band interpolation over 2 × [`UPSAMPLE_HALF`] inputs.
/// The look-ahead delays the output by `UPSAMPLE_HALF` input samples (0.5 ms at 24 kHz); the
/// state carries across packets, so a seek primed with the preceding packet gives the same
/// samples as continuous decoding.
#[derive(Clone)]
pub struct Upsample2x {
    taps: [f32; 2 * UPSAMPLE_HALF],
    /// The last `2 × UPSAMPLE_HALF` input samples per channel.
    hist: Vec<Vec<f32>>,
}

impl Upsample2x {
    pub fn new(channels: usize) -> Self {
        let m = UPSAMPLE_HALF as f64;
        let mut taps = [0f32; 2 * UPSAMPLE_HALF];
        let mut sum = 0.0;
        for (k, t) in taps.iter_mut().enumerate() {
            // distance from the interpolation point (halfway between two inputs)
            let x = k as f64 - m + 0.5;
            let sinc = (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x);
            let w = 0.42 + 0.5 * (std::f64::consts::PI * x / m).cos() + 0.08 * (2.0 * std::f64::consts::PI * x / m).cos();
            *t = (sinc * w) as f32;
            sum += sinc * w;
        }
        for t in &mut taps {
            *t = (*t as f64 / sum) as f32;
        }
        Self { taps, hist: vec![vec![0.0; 2 * UPSAMPLE_HALF]; channels] }
    }

    pub fn reset(&mut self) {
        for h in &mut self.hist {
            h.fill(0.0);
        }
    }

    pub fn process(&mut self, input: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        if self.hist.len() < input.len() {
            self.hist.resize(input.len(), vec![0.0; 2 * UPSAMPLE_HALF]);
        }
        let m = UPSAMPLE_HALF;
        input
            .into_iter()
            .zip(&mut self.hist)
            .map(|(x, hist)| {
                let mut buf = std::mem::take(hist);
                buf.extend_from_slice(&x);
                let mut out = Vec::with_capacity(x.len() * 2);
                // output pair i is centred on buf[i + m]: the sample itself, then the point halfway
                // to the next one from buf[i + 1 ..= i + 2m]
                for w in buf.windows(2 * m + 1).take(x.len()) {
                    out.push(w[m]);
                    out.push(w[1..].iter().zip(&self.taps).map(|(a, b)| a * b).sum());
                }
                *hist = buf.split_off(buf.len() - 2 * m);
                out
            })
            .collect()
    }
}

/// Whether AC-3 audio decodes (our ATSC A/52 decoder).
pub const AC3_DECODER: bool = true;

/// An AudioSpecificConfig for AAC with these parameters (ISO/IEC 14496-3 §1.6.2.1, GA specific
/// config with no extension flags).
fn asc_bytes(object_type: u8, sf_index: u8, channel_config: u8) -> Vec<u8> {
    let v = ((object_type as u16) << 11) | ((sf_index as u16 & 15) << 7) | ((channel_config as u16 & 15) << 3);
    v.to_be_bytes().to_vec()
}

/// An ADTS frame (ISO/IEC 13818-7 §6.2): the equivalent AudioSpecificConfig and the raw data
/// block. Frames with several raw data blocks are not split (`None`).
pub fn adts_split(frame: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    if frame.len() < 7 || frame[0] != 0xFF || frame[1] & 0xF6 != 0xF0 {
        return None;
    }
    let protection_absent = frame[1] & 1 != 0;
    let profile = frame[2] >> 6;
    let sf = (frame[2] >> 2) & 15;
    let ch = ((frame[2] & 1) << 2) | (frame[3] >> 6);
    let len = (((frame[3] & 3) as usize) << 11) | (frame[4] as usize) << 3 | (frame[5] as usize) >> 5;
    if frame[6] & 3 != 0 {
        return None;
    }
    let hdr = if protection_absent { 7 } else { 9 };
    let end = len.min(frame.len());
    (end > hdr).then(|| (asc_bytes(profile + 1, sf, ch), &frame[hdr..end]))
}

/// The parts of a LATM StreamMuxConfig (ISO/IEC 14496-3 §1.7.3) we decode: one program, one
/// layer, AAC-LC, variable frame length (type 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatmConfig {
    pub asc: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
}

const AAC_RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];

fn latm_value(r: &mut filmcraft_bitstream::BitReader) -> Option<u32> {
    let n = r.read_bits(2).ok()? + 1;
    r.read_bits(8 * n).ok()
}

/// StreamMuxConfig after `audioMuxVersion`, leaving the reader after it.
fn stream_mux_config(r: &mut filmcraft_bitstream::BitReader) -> Option<LatmConfig> {
    let version = r.read_bits(1).ok()?;
    let version_a = if version == 1 { r.read_bits(1).ok()? } else { 0 };
    if version_a != 0 {
        return None;
    }
    if version == 1 {
        latm_value(r)?; // taraBufferFullness
    }
    let _all_same_framing = r.read_bits(1).ok()?;
    let sub_frames = r.read_bits(6).ok()?;
    let programs = r.read_bits(4).ok()?;
    let layers = r.read_bits(3).ok()?;
    if sub_frames != 0 || programs != 0 || layers != 0 {
        return None;
    }
    if version == 1 {
        latm_value(r)?; // ascLen
    }
    // AudioSpecificConfig
    let aot = r.read_bits(5).ok()?;
    let sf = r.read_bits(4).ok()?;
    if aot != 2 || sf == 15 {
        return None;
    }
    let ch = r.read_bits(4).ok()?;
    let frame_length_flag = r.read_bits(1).ok()?;
    let depends_on_core = r.read_bits(1).ok()?;
    let _extension = r.read_bits(1).ok()?;
    if frame_length_flag != 0 || depends_on_core != 0 {
        return None;
    }
    let frame_length_type = r.read_bits(3).ok()?;
    if frame_length_type != 0 {
        return None;
    }
    r.read_bits(8).ok()?; // latmBufferFullness
    if r.read_bits(1).ok()? == 1 {
        // otherData
        if version == 1 {
            latm_value(r)?;
        } else {
            loop {
                let esc = r.read_bits(1).ok()?;
                r.read_bits(8).ok()?;
                if esc == 0 {
                    break;
                }
            }
        }
    }
    if r.read_bits(1).ok()? == 1 {
        r.read_bits(8).ok()?; // crcCheckSum
    }
    Some(LatmConfig { asc: asc_bytes(2, sf as u8, ch as u8), sample_rate: *AAC_RATES.get(sf as usize)?, channels: if ch == 0 { 2 } else { ch } })
}

/// The configuration carried in a LOAS frame (`None` if it reuses an earlier one or is not
/// supported).
pub fn latm_config(frame: &[u8]) -> Option<LatmConfig> {
    if frame.len() < 4 || frame[0] != 0x56 || frame[1] & 0xE0 != 0xE0 {
        return None;
    }
    let mut r = filmcraft_bitstream::BitReader::new(&frame[3..]);
    if r.read_bits(1).ok()? == 1 {
        return None; // useSameStreamMux
    }
    stream_mux_config(&mut r)
}

/// A LOAS/LATM frame: its AudioSpecificConfig and the AAC payload of the single sub-frame.
/// `last` is the configuration of earlier frames (used by frames that set useSameStreamMux,
/// updated by frames that carry one).
pub fn latm_split(frame: &[u8], last: &mut Option<LatmConfig>) -> Option<(Vec<u8>, Vec<u8>)> {
    if frame.len() < 4 || frame[0] != 0x56 || frame[1] & 0xE0 != 0xE0 {
        return None;
    }
    let mut r = filmcraft_bitstream::BitReader::new(&frame[3..]);
    let same = r.read_bits(1).ok()? == 1;
    let cfg = if same {
        last.clone()?
    } else {
        let c = stream_mux_config(&mut r)?;
        *last = Some(c.clone());
        c
    };
    // PayloadLengthInfo (frameLengthType 0) then PayloadMux, bit-aligned
    let mut len = 0usize;
    loop {
        let b = r.read_bits(8).ok()? as usize;
        len += b;
        if b != 255 {
            break;
        }
    }
    let mut payload = Vec::with_capacity(len);
    for _ in 0..len {
        payload.push(r.read_bits(8).ok()? as u8);
    }
    Some((cfg.asc, payload))
}

/// Blu-ray / AVCHD LPCM packet header (4 bytes before the samples).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlurayLpcm {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u32,
}

/// Channels of each channel_assignment code (mono, stereo, 3/0, 2/1, 3/1, 2/2, 3/2, 3/2+LFE,
/// 3/4, 3/4+LFE).
const BD_LPCM_CHANNELS: [usize; 12] = [0, 1, 0, 2, 3, 3, 4, 4, 5, 6, 7, 8];

pub fn bluray_lpcm_header(p: &[u8]) -> Option<BlurayLpcm> {
    if p.len() < 4 {
        return None;
    }
    let channels = *BD_LPCM_CHANNELS.get((p[2] >> 4) as usize).filter(|&&c| c > 0)?;
    let sample_rate = match p[2] & 15 {
        1 => 48_000,
        4 => 96_000,
        5 => 192_000,
        _ => return None,
    };
    let bits = match p[3] >> 6 {
        1 => 16,
        2 => 20,
        3 => 24,
        _ => return None,
    };
    Some(BlurayLpcm { sample_rate, channels, bits })
}

/// Decode one Blu-ray LPCM packet (big-endian; channels padded to an even count; 20-bit
/// samples are stored in 24 bits).
pub fn decode_bluray_lpcm(p: &[u8]) -> Option<Vec<Vec<f32>>> {
    let h = bluray_lpcm_header(p)?;
    let coded = (h.channels + 1) & !1;
    let bps = if h.bits == 16 { 2 } else { 3 };
    let data = &p[4..];
    let n = data.len() / (coded * bps);
    let mut out = vec![Vec::with_capacity(n); h.channels];
    for k in 0..n {
        for (c, o) in out.iter_mut().enumerate() {
            let at = (k * coded + c) * bps;
            let v = if bps == 2 {
                i16::from_be_bytes([data[at], data[at + 1]]) as f32 / 32768.0
            } else {
                (i32::from_be_bytes([data[at], data[at + 1], data[at + 2], 0]) >> 8) as f32 / 8_388_608.0
            };
            o.push(v);
        }
    }
    Some(out)
}

/// Bytes per two sample frames of DVD LPCM.
pub fn dvd_lpcm_group_bytes(channels: usize, bits: u32) -> usize {
    match bits {
        16 => 4 * channels,
        20 => 5 * channels,
        _ => 6 * channels,
    }
}

/// Decode DVD-Video LPCM (big-endian). 16-bit samples are interleaved; 20- and 24-bit samples
/// come in groups of two frames: the upper 16 bits of all the group's samples, then their low
/// 4 / 8 bits.
pub fn decode_dvd_lpcm(data: &[u8], channels: usize, bits: u32) -> Vec<Vec<f32>> {
    let ch = channels.max(1);
    let group = dvd_lpcm_group_bytes(ch, bits);
    let groups = data.len() / group;
    let mut out = vec![Vec::with_capacity(groups * 2); ch];
    for g in 0..groups {
        let b = &data[g * group..(g + 1) * group];
        for f in 0..2 {
            for (c, o) in out.iter_mut().enumerate() {
                let k = f * ch + c;
                let hi = i16::from_be_bytes([b[2 * k], b[2 * k + 1]]) as i32;
                let v = match bits {
                    16 => hi as f32 / 32768.0,
                    20 => {
                        let nib = b[4 * ch + k / 2];
                        let lo = if k.is_multiple_of(2) { nib >> 4 } else { nib & 15 } as i32;
                        ((hi << 4) | lo) as f32 / 524_288.0
                    }
                    _ => ((hi << 8) | b[4 * ch + k] as i32) as f32 / 8_388_608.0,
                };
                o.push(v);
            }
        }
    }
    out
}

/// Decode interleaved PCM bytes into planar f32.
pub fn decode_pcm(data: &[u8], cfg: &filmcraft_isobmff::PcmConfig) -> Vec<Vec<f32>> {
    let ch = cfg.channels.max(1) as usize;
    let bps = (cfg.bits as usize).div_ceil(8);
    let n = data.len() / (bps * ch);
    let mut out = vec![Vec::with_capacity(n); ch];
    for i in 0..n {
        for (c, o) in out.iter_mut().enumerate() {
            let off = (i * ch + c) * bps;
            let b = &data[off..off + bps];
            let v = match (cfg.bits, cfg.float, cfg.big_endian) {
                (32, true, false) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                (32, true, true) => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                (64, true, false) => f64::from_le_bytes(b.try_into().unwrap_or([0; 8])) as f32,
                (64, true, true) => f64::from_be_bytes(b.try_into().unwrap_or([0; 8])) as f32,
                (8, _, _) => {
                    if cfg.signed {
                        b[0] as i8 as f32 / 128.0
                    } else {
                        (b[0] as f32 - 128.0) / 128.0
                    }
                }
                (16, _, false) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                (16, _, true) => i16::from_be_bytes([b[0], b[1]]) as f32 / 32768.0,
                (24, _, false) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                (24, _, true) => (i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8) as f32 / 8_388_608.0,
                (32, false, false) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
                (32, false, true) => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
                _ => 0.0,
            };
            o.push(v);
        }
    }
    out
}

/// Resample-read `frames` from planar source audio at `src_rate` into a buffer at `rate`,
/// starting at output sample `start` (linear interpolation; the audio crate adds sinc later).
pub fn read_resampled(src: &[Vec<f32>], src_rate: u32, start: i64, frames: usize, rate: u32) -> AudioBuffer {
    let ch = src.len().max(1);
    let mut out = AudioBuffer::silence(rate, ch, frames);
    let total = src.first().map_or(0, Vec::len);
    let ratio = src_rate as f64 / rate as f64;
    for i in 0..frames {
        let pos = (start + i as i64) as f64 * ratio;
        if pos < 0.0 {
            continue;
        }
        let i0 = pos.floor() as usize;
        if i0 >= total {
            break;
        }
        let f = (pos - i0 as f64) as f32;
        for (c, s) in src.iter().enumerate() {
            let a = s[i0];
            let b = s.get(i0 + 1).copied().unwrap_or(a);
            out.channels[c][i] = a + (b - a) * f;
        }
    }
    out
}

/// A standalone audio file decoded fully into memory (audio files are small next to video).
pub struct AudioFileSource {
    info: MediaInfo,
    rate: u32,
    samples: Vec<Vec<f32>>,
}

impl AudioFileSource {
    pub fn decode(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes.clone())), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(&ext);
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| CodecError::Unsupported(e.to_string()))?;
        let mut format = probed.format;
        let track = format.default_track().ok_or_else(|| CodecError::Unsupported("no audio track".into()))?.clone();
        let rate = track.codec_params.sample_rate.unwrap_or(48_000);
        let mut dec =
            symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| CodecError::Unsupported(e.to_string()))?;
        let mut samples: Vec<Vec<f32>> = Vec::new();
        while let Ok(pkt) = format.next_packet() {
            if pkt.track_id() != track.id {
                continue;
            }
            let Ok(buf) = dec.decode(&pkt) else { continue };
            let spec = *buf.spec();
            let ch = spec.channels.count().max(1);
            if samples.is_empty() {
                samples = vec![Vec::new(); ch];
            }
            let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
            sb.copy_interleaved_ref(buf);
            for (i, v) in sb.samples().iter().enumerate() {
                if let Some(c) = samples.get_mut(i % ch) {
                    c.push(*v);
                }
            }
        }
        if samples.is_empty() {
            return Err(CodecError::Decode("no audio decoded".into()));
        }
        let frames = samples[0].len();
        let codec_name =
            symphonia::default::get_codecs().get_codec(track.codec_params.codec).map(|d| d.short_name.to_uppercase()).unwrap_or_else(|| ext.to_uppercase());
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::AudioOnly,
            duration: Tick::from_units(frames as i64, rate as i64),
            video: None,
            audio: Some(AudioStreamInfo {
                sample_rate: rate,
                channels: samples.len() as u32,
                codec: codec_name,
                bits_per_sample: track.codec_params.bits_per_sample,
            }),
            container: ext.to_uppercase(),
            start_timecode: None,
            file_size: Some(bytes.len() as u64),
        };
        Ok(Self { info, rate, samples })
    }
}

impl MediaSource for AudioFileSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> std::result::Result<Arc<VideoFrame>, MediaError> {
        Err(MediaError::NoStream("video"))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> std::result::Result<AudioBuffer, MediaError> {
        Ok(read_resampled(&self.samples, self.rate, start, frames, sample_rate))
    }
}

pub fn opener(name: &str, bytes: Arc<[u8]>) -> Option<std::result::Result<SharedSource, MediaError>> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(ext.as_str(), "mp3" | "mp2" | "flac" | "ogg" | "oga" | "aif" | "aiff" | "aifc") {
        return None;
    }
    Some(AudioFileSource::decode(name, bytes).map(|s| Arc::new(s) as SharedSource).map_err(Into::into))
}

#[cfg(test)]
mod he_aac_tests {
    use super::*;

    /// AAC-LC config, 22.05 kHz stereo.
    const LC_22K: [u8; 2] = [0x13, 0x90];
    /// An access unit holding only a fill element with SBR data (ID_FIL, EXT_SBR_DATA, ID_END).
    const SBR_FILL: [u8; 3] = [0b1100_0011, 0b1010_0001, 0b1100_0000];

    #[test]
    fn output_rate_doubles_for_he_aac() {
        // explicit: the extension rate from the config
        assert_eq!(aac_output_rate(&[0x2B, 0x92, 0x08, 0x00], []), Some(44_100));
        // implicit: SBR data in the first access units of a ≤ 24 kHz AAC-LC stream
        assert_eq!(aac_output_rate(&LC_22K, [&SBR_FILL[..]]), Some(44_100));
        assert_eq!(aac_output_rate(&LC_22K, []), Some(22_050));
        // AAC-LC at 44.1 kHz stays as it is, even with SBR-looking fill data
        assert_eq!(aac_output_rate(&[0x12, 0x10], [&SBR_FILL[..]]), Some(44_100));
    }

    #[test]
    fn upsampler_interpolates_continuously_across_packets() {
        let f = 1000.0 / 22_050.0;
        let x: Vec<f32> = (0..4096).map(|i| (2.0 * std::f32::consts::PI * f * i as f32).sin()).collect();
        let mut up = Upsample2x::new(1);
        let mut y = Vec::new();
        for chunk in x.chunks(1024) {
            y.extend(up.process(vec![chunk.to_vec()]).remove(0));
        }
        assert_eq!(y.len(), 8192);
        // output 2k + 1 is input k − UPSAMPLE_HALF + ½ (the look-ahead delay), within 0.1 %
        let d = UPSAMPLE_HALF as f32;
        for k in 100..4000 {
            let want = (2.0 * std::f32::consts::PI * f * (k as f32 - d + 0.5)).sin();
            assert!((y[2 * k + 1] - want).abs() < 1e-3, "{k}: {} vs {want}", y[2 * k + 1]);
            assert_eq!(y[2 * k], x[k - UPSAMPLE_HALF]);
        }
    }
}
