//! Clean-room MPEG-1 / MPEG-2 Audio Layer I and Layer II decoder (MP1 / MP2), written from
//! ISO/IEC 11172-3 (§2.4 bit stream syntax and decoding, Annex B tables) and ISO/IEC 13818-3
//! (the lower sampling frequencies 16 / 22.05 / 24 kHz).
//!
//! - Layer I and Layer II; mono, stereo, dual channel and joint (intensity) stereo; every bit rate
//!   and sampling frequency of MPEG-1, MPEG-2 and the unofficial MPEG-2.5 (8 / 11.025 / 12 kHz).
//! - Lenient where encoders are: Layer II bit rate / mode combinations that ISO/IEC 11172-3 does not
//!   allow (mono above 192 kbit/s, which is ffmpeg's default for mono, and stereo below 64 kbit/s)
//!   decode with the allocation table their bit rate per channel selects (see [`Header`]).
//! - Output: planar f32 at full scale ±1, 384 (Layer I) or 1152 (Layer II) samples per channel.
//!
//! Not decoded: Layer III (use an MP3 decoder; [`parse_header`] still reads its headers), the free
//! format (bit rate index 0, whose frame length is only known from the next sync word) and the
//! CRC (read, not checked, like most decoders).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod tables;

use filmcraft_bitstream::BitReader;
use tables::AllocTable;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not an MPEG audio frame")]
    NoSync,
    #[error("unsupported MPEG audio: {0}")]
    Unsupported(&'static str),
    #[error("invalid MPEG audio frame: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// MPEG audio version (`ID` bit, and the MPEG-2.5 extension of the sync word).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    /// ISO/IEC 11172-3: 32 / 44.1 / 48 kHz.
    Mpeg1,
    /// ISO/IEC 13818-3 lower sampling frequencies: 16 / 22.05 / 24 kHz.
    Mpeg2,
    /// The unofficial extension to 8 / 11.025 / 12 kHz.
    Mpeg25,
}

/// Channel mode (`mode`, §2.4.2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stereo,
    /// Intensity stereo above the bound given by `mode_extension` (Layers I and II).
    JointStereo,
    /// Two independent mono channels.
    DualChannel,
    Mono,
}

/// The 32-bit frame header (§2.4.1.3, §2.4.2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: Version,
    /// 1, 2 or 3.
    pub layer: u8,
    /// A 16-bit CRC follows the header.
    pub crc: bool,
    /// 0 for the free format.
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub padding: bool,
    pub mode: Mode,
    pub mode_extension: u8,
    /// Bytes in the frame, header included (0 for the free format).
    pub frame_bytes: usize,
}

impl Header {
    /// Output channels: 1 for mono, else 2.
    pub fn channels(&self) -> usize {
        if self.mode == Mode::Mono { 1 } else { 2 }
    }
    /// Samples per channel the frame decodes to.
    pub fn samples(&self) -> usize {
        match (self.layer, self.version) {
            (1, _) => 384,
            (3, Version::Mpeg2 | Version::Mpeg25) => 576,
            _ => 1152,
        }
    }
    /// The first subband coded as intensity stereo (`bound`, §2.4.2.3): `mode_extension` 0-3 gives
    /// 4, 8, 12 or 16 in joint stereo; otherwise every subband is coded per channel.
    fn bound(&self) -> usize {
        if self.mode == Mode::JointStereo { 4 * (self.mode_extension as usize + 1) } else { 32 }
    }
}

/// Parse the frame header at the start of `b`.
pub fn parse_header(b: &[u8]) -> Result<Header> {
    let [0xFF, b1, b2, b3, ..] = *b else {
        return Err(if b.len() < 4 && b.first().is_none_or(|&x| x == 0xFF) { Error::Invalid("truncated header") } else { Error::NoSync });
    };
    if b1 & 0xE0 != 0xE0 {
        return Err(Error::NoSync);
    }
    let version = match (b1 >> 3) & 3 {
        3 => Version::Mpeg1,
        2 => Version::Mpeg2,
        0 => Version::Mpeg25,
        _ => return Err(Error::Invalid("reserved version")),
    };
    let layer = match (b1 >> 1) & 3 {
        3 => 1,
        2 => 2,
        1 => 3,
        _ => return Err(Error::Invalid("reserved layer")),
    };
    let crc = b1 & 1 == 0;
    let bitrate_index = (b2 >> 4) as usize;
    let rate_index = ((b2 >> 2) & 3) as usize;
    let padding = (b2 >> 1) & 1 == 1;
    let mode = match b3 >> 6 {
        0 => Mode::Stereo,
        1 => Mode::JointStereo,
        2 => Mode::DualChannel,
        _ => Mode::Mono,
    };
    let mode_extension = (b3 >> 4) & 3;
    let base = *tables::SAMPLE_RATES.get(rate_index).ok_or(Error::Invalid("reserved sampling frequency"))?;
    let sample_rate = match version {
        Version::Mpeg1 => base,
        Version::Mpeg2 => base / 2,
        Version::Mpeg25 => base / 4,
    };
    let rates = match (version, layer) {
        (Version::Mpeg1, 1) => &tables::BITRATES_MPEG1_L1,
        (Version::Mpeg1, 2) => &tables::BITRATES_MPEG1_L2,
        (Version::Mpeg1, _) => &tables::BITRATES_MPEG1_L3,
        (_, 1) => &tables::BITRATES_LSF_L1,
        _ => &tables::BITRATES_LSF_L23,
    };
    let bitrate_kbps = *rates.get(bitrate_index).ok_or(Error::Invalid("forbidden bit rate index"))?;
    // §2.4.3.1: Layer I slots are 4 bytes, 12 slots per 384-sample frame per bit/s over the
    // sampling frequency; Layers II / III have 1-byte slots, 144 per 1152 samples (72 for the
    // 576-sample Layer III frames of MPEG-2).
    let bits_per_second = u64::from(bitrate_kbps) * 1000;
    let rate = u64::from(sample_rate);
    let pad = u64::from(padding);
    let frame_bytes = if bitrate_kbps == 0 {
        0
    } else if layer == 1 {
        (12 * bits_per_second / rate + pad) * 4
    } else {
        let slots = if layer == 3 && version != Version::Mpeg1 { 72 } else { 144 };
        slots * bits_per_second / rate + pad
    };
    Ok(Header { version, layer, crc, bitrate_kbps, sample_rate, padding, mode, mode_extension, frame_bytes: frame_bytes as usize })
}

/// One decoded frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub header: Header,
    /// Planar samples, [`Header::channels`] × [`Header::samples`].
    pub channels: Vec<Vec<f32>>,
}

/// Subband samples of one frame: `[channel][time slot][subband]`, up to 36 time slots.
type Subbands = [[[f32; tables::SUBBANDS]; 36]; 2];

/// A Layer I / Layer II decoder. It keeps the synthesis filter state between frames, so frames
/// must be fed in order; call [`Decoder::reset`] before decoding from another position.
pub struct Decoder {
    /// The synthesis filter's `V` vector per channel (§2.4.3.2, Figure A.2).
    v: [[f32; 1024]; 2],
    sb: Box<Subbands>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self { v: [[0.0; 1024]; 2], sb: Box::new([[[0.0; 32]; 36]; 2]) }
    }

    /// Forget the filter history.
    pub fn reset(&mut self) {
        self.v = [[0.0; 1024]; 2];
    }

    /// Decode the frame at the start of `frame` (which may run on into the next frames; only
    /// [`Header::frame_bytes`] are read).
    pub fn decode(&mut self, frame: &[u8]) -> Result<Frame> {
        let header = parse_header(frame)?;
        if header.layer == 3 {
            return Err(Error::Unsupported("Layer III"));
        }
        if header.frame_bytes == 0 {
            return Err(Error::Unsupported("free-format bit rate"));
        }
        let data = frame.get(..header.frame_bytes).ok_or(Error::Invalid("truncated frame"))?;
        let mut r = BitReader::new(data);
        r.skip(32 + if header.crc { 16 } else { 0 }).map_err(|_| Error::Invalid("truncated frame"))?;
        let nch = header.channels();
        for ch in self.sb.iter_mut() {
            for slot in ch.iter_mut() {
                *slot = [0.0; 32];
            }
        }
        let slots = if header.layer == 1 {
            layer1(&mut r, &header, &mut self.sb)?;
            12
        } else {
            layer2(&mut r, &header, &mut self.sb)?;
            36
        };
        let window = tables::window();
        let matrix = tables::matrix();
        let mut channels = vec![vec![0f32; slots * 32]; nch];
        for (ch, out) in channels.iter_mut().enumerate() {
            let (Some(v), Some(sb)) = (self.v.get_mut(ch), self.sb.get(ch)) else { continue };
            for (slot, o) in sb.iter().take(slots).zip(out.as_chunks_mut::<32>().0) {
                synthesize(v, matrix, window, slot, o);
            }
        }
        Ok(Frame { header, channels })
    }
}

fn read(r: &mut BitReader, n: u32) -> Result<u32> {
    r.read_bits(n).map_err(|_| Error::Invalid("frame data runs past the frame"))
}

/// Requantize an `steps`-level code `v` (§2.4.3.3.4 / §2.4.3.4.4): the fractional value with
/// the MSB inverted, `C · (s''' + D)`, which for every class is `(2v + 1 - steps) / steps`.
fn requantize(v: u32, steps: u32) -> f32 {
    (2.0 * v as f32 + 1.0 - steps as f32) / steps as f32
}

/// Layer I audio data (§2.4.1.5): allocation, scalefactors and 12 samples per subband.
fn layer1(r: &mut BitReader, h: &Header, out: &mut Subbands) -> Result<()> {
    let nch = h.channels();
    let bound = if nch == 1 { 32 } else { h.bound() };
    let mut alloc = [[0u32; 32]; 2];
    for sb in 0..32 {
        if sb < bound {
            for ch in 0..nch {
                alloc[ch][sb] = read(r, 4)?;
            }
        } else {
            let a = read(r, 4)?;
            alloc[0][sb] = a;
            alloc[1][sb] = a;
        }
    }
    if alloc.iter().flatten().any(|&a| a == 15) {
        return Err(Error::Invalid("forbidden Layer I allocation"));
    }
    let mut scf = [[0f32; 32]; 2];
    for sb in 0..32 {
        for ch in 0..nch {
            if alloc[ch][sb] != 0 {
                scf[ch][sb] = tables::scalefactor(read(r, 6)?);
            }
        }
    }
    for s in 0..12 {
        for sb in 0..32 {
            if sb < bound {
                for ch in 0..nch {
                    let a = alloc[ch][sb];
                    if a != 0 {
                        let nb = a + 1;
                        out[ch][s][sb] = requantize(read(r, nb)?, (1 << nb) - 1) * scf[ch][sb];
                    }
                }
            } else if alloc[0][sb] != 0 {
                let nb = alloc[0][sb] + 1;
                let q = requantize(read(r, nb)?, (1 << nb) - 1);
                for ch in 0..nch {
                    out[ch][s][sb] = q * scf[ch][sb];
                }
            }
        }
    }
    Ok(())
}

/// Read the three samples of one granule in a subband with `steps` quantization steps.
fn granule(r: &mut BitReader, steps: u16) -> Result<[f32; 3]> {
    let steps = u32::from(steps);
    let mut q = [0u32; 3];
    if let Some(bits) = tables::grouping(steps as u16) {
        // three samples in one codeword: c = s0 + steps·s1 + steps²·s2 (§2.4.3.3.4); what is left
        // of an invalid codeword beyond steps³ - 1 is ignored
        let mut c = read(r, bits)?;
        for s in q.iter_mut() {
            *s = c % steps;
            c /= steps;
        }
    } else {
        let nb = (steps + 1).trailing_zeros();
        for s in q.iter_mut() {
            *s = read(r, nb)?.min(steps - 1);
        }
    }
    Ok(q.map(|v| requantize(v, steps)))
}

/// Layer II audio data (§2.4.1.6): allocation, scalefactor selection, scalefactors and 12
/// granules of three samples per subband.
fn layer2(r: &mut BitReader, h: &Header, out: &mut Subbands) -> Result<()> {
    let nch = h.channels();
    let per_channel = h.bitrate_kbps / nch as u32;
    let table = AllocTable::select(h.version != Version::Mpeg1, h.sample_rate, per_channel);
    // at most 32 for every table (a compile-time check in `tables`); the clamp keeps every
    // `[sb]` below in bounds even so
    let sblimit = table.sblimit().min(tables::SUBBANDS);
    let bound = if nch == 1 { sblimit } else { h.bound().min(sblimit) };
    let mut steps = [[0u16; tables::SUBBANDS]; 2];
    for sb in 0..sblimit {
        let row = table.row(sb);
        let nbal = row.len().trailing_zeros();
        let pick = |r: &mut BitReader| -> Result<u16> { Ok(row.get(read(r, nbal)? as usize).copied().unwrap_or(0)) };
        if sb < bound {
            for ch in 0..nch {
                steps[ch][sb] = pick(r)?;
            }
        } else {
            let s = pick(r)?;
            steps[0][sb] = s;
            steps[1][sb] = s;
        }
    }
    let mut scfsi = [[0u32; tables::SUBBANDS]; 2];
    for sb in 0..sblimit {
        for ch in 0..nch {
            if steps[ch][sb] != 0 {
                scfsi[ch][sb] = read(r, 2)?;
            }
        }
    }
    // the scalefactor for each third of the frame (4 granules each)
    let mut scf = [[[0f32; 3]; tables::SUBBANDS]; 2];
    for sb in 0..sblimit {
        for ch in 0..nch {
            if steps[ch][sb] == 0 {
                continue;
            }
            let mut s = || -> Result<f32> { Ok(tables::scalefactor(read(r, 6)?)) };
            scf[ch][sb] = match scfsi[ch][sb] {
                0 => [s()?, s()?, s()?],
                1 => {
                    let (a, b) = (s()?, s()?);
                    [a, a, b]
                }
                2 => {
                    let a = s()?;
                    [a, a, a]
                }
                _ => {
                    let (a, b) = (s()?, s()?);
                    [a, b, b]
                }
            };
        }
    }
    for gr in 0..12 {
        let part = gr / 4;
        for sb in 0..sblimit {
            if sb < bound {
                for ch in 0..nch {
                    if steps[ch][sb] != 0 {
                        let q = granule(r, steps[ch][sb])?;
                        for (k, v) in q.iter().enumerate() {
                            out[ch][3 * gr + k][sb] = v * scf[ch][sb][part];
                        }
                    }
                }
            } else if steps[0][sb] != 0 {
                let q = granule(r, steps[0][sb])?;
                for ch in 0..nch {
                    for (k, v) in q.iter().enumerate() {
                        out[ch][3 * gr + k][sb] = v * scf[ch][sb][part];
                    }
                }
            }
        }
    }
    Ok(())
}

/// The synthesis subband filter (§2.4.3.2, Figure A.2) for one time slot: 32 subband samples in,
/// 32 PCM samples out.
fn synthesize(v: &mut [f32; 1024], n: &[[f32; 32]; 64], d: &[f32; 512], s: &[f32; 32], out: &mut [f32; 32]) {
    v.copy_within(0..960, 64);
    for (vi, row) in v.iter_mut().zip(n.iter()) {
        *vi = row.iter().zip(s.iter()).map(|(a, b)| a * b).sum();
    }
    for (j, o) in out.iter_mut().enumerate() {
        let mut acc = 0f32;
        for i in 0..8 {
            acc += v[i * 128 + j] * d[i * 64 + j] + v[i * 128 + 96 + j] * d[i * 64 + 32 + j];
        }
        *o = acc;
    }
}

#[cfg(test)]
mod tests;
