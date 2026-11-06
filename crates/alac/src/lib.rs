//! Clean-room ALAC (Apple Lossless Audio Codec) encoder, written from the published format.
//!
//! - [`Encoder`]: planar integer samples (16, 20, 24 or 32 bit, 1–8 channels in ALAC channel
//!   order) → frames of [`EncoderConfig::frame_length`] samples, one MP4 / QuickTime sample each,
//!   and the 24-byte magic cookie ([`Encoder::magic_cookie`]) that goes in the `alac` box.
//! - Every channel is predicted by ALAC's adaptive filter (8 taps, denominator shift 9) whose
//!   coefficients carry over from frame to frame, and its residual is coded with ALAC's adaptive
//!   Golomb code. A channel pair also tries left/right, mid/side and left/side mixing and keeps
//!   the smallest. 24- and 32-bit samples send their low 8 or 16 bits verbatim (`bytesShifted`),
//!   as Apple's encoder does. A frame that would not shrink is stored uncompressed (32-bit
//!   channel pairs excepted: ffmpeg cannot decode those verbatim).
//! - Lossless: the decoder gets back exactly the samples that went in.
//!
//! Layer L0: depends only on `filmcraft-bitstream` (+ `thiserror`); no `unsafe`; builds for
//! `wasm32-unknown-unknown`.
//!
//! ```
//! use filmcraft_alac::{Encoder, EncoderConfig};
//!
//! let left: Vec<i32> = (0..48_000).map(|i| ((i as f64 * 0.05).sin() * 20_000.0) as i32).collect();
//! let right = left.clone();
//! let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 16))?;
//! let mut frames = enc.encode_frames(&[&left, &right])?;
//! frames.extend(enc.finish());
//! assert_eq!(frames.len(), 12); // 11 full frames of 4096 and a last one of 2944
//! let cookie = enc.magic_cookie(); // for the `alac` box
//! assert_eq!(cookie.len(), 24);
//! assert!(frames.iter().map(Vec::len).sum::<usize>() < 48_000 * 2 * 2 / 3);
//! # Ok::<(), filmcraft_alac::Error>(())
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod ag;
mod predict;

use filmcraft_bitstream::BitWriter;

use ag::AgParams;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid encoder configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("expected {expected} channels, got {got}")]
    Channels { expected: usize, got: usize },
    #[error("channels have different lengths")]
    UnequalChannels,
    #[error("sample {0} does not fit the configured bit depth")]
    SampleRange(i64),
}

/// The cookie's adaptation parameters (Apple's defaults, which every encoder uses).
const PB: u8 = 40;
const MB: u8 = 10;
const KB: u8 = 14;
const MAX_RUN: u16 = 255;
/// Predictor taps and denominator shift.
const ORDER: usize = 8;
const DEN_SHIFT: u32 = 9;
/// Mixing precision for channel pairs; the mixes tried are mixRes 0 (left/right), 2 (mid/side)
/// and 4 (left/side).
const MIX_BITS: u32 = 2;
const MIX_RES: [i32; 3] = [0, 2, 4];

/// Element types (the frame's 3-bit tags).
const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_END: u32 = 7;

#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    /// 1 to 384000 Hz.
    pub sample_rate: u32,
    /// 1–8, in ALAC order: mono; L R; C L R; C L R Cs; C L R Ls Rs; C L R Ls Rs LFE (5.1);
    /// C L R Ls Rs Cs LFE; C Lc Rc L R Ls Rs LFE (7.1).
    pub channels: u8,
    /// 16, 20, 24 or 32.
    pub bits_per_sample: u8,
    /// Samples per channel per frame, 16–16384 (4096 by default).
    pub frame_length: u32,
}

impl EncoderConfig {
    pub fn new(sample_rate: u32, channels: u8, bits_per_sample: u8) -> Self {
        EncoderConfig { sample_rate, channels, bits_per_sample, frame_length: 4096 }
    }
}

/// The elements a frame of `channels` channels is made of (ALAC's channel maps): single channels
/// and channel pairs, in channel order.
fn elements(channels: u8) -> &'static [u32] {
    match channels {
        1 => &[ID_SCE],
        2 => &[ID_CPE],
        3 => &[ID_SCE, ID_CPE],
        4 => &[ID_SCE, ID_CPE, ID_SCE],
        5 => &[ID_SCE, ID_CPE, ID_CPE],
        6 => &[ID_SCE, ID_CPE, ID_CPE, ID_SCE],
        7 => &[ID_SCE, ID_CPE, ID_CPE, ID_SCE, ID_SCE],
        _ => &[ID_SCE, ID_CPE, ID_CPE, ID_CPE, ID_SCE],
    }
}

/// Apple's starting coefficients (before the filter adapts to the audio).
fn initial_coefs() -> [i16; ORDER] {
    let den = 1i32 << DEN_SHIFT;
    let mut c = [0i16; ORDER];
    c[0] = ((38 * den) >> 4) as i16;
    c[1] = ((-29 * den) >> 4) as i16;
    c[2] = ((-2 * den) >> 4) as i16;
    c
}

pub struct Encoder {
    cfg: EncoderConfig,
    /// Samples waiting for a full frame, per channel.
    pending: Vec<Vec<i32>>,
    /// Predictor state per coded channel: two per element (the second unused by single channels).
    coefs: Vec<[[i16; ORDER]; 2]>,
    max_frame_bytes: u32,
    total_bytes: u64,
    total_samples: u64,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Self, Error> {
        if !(1..=8).contains(&cfg.channels) {
            return Err(Error::InvalidConfig("1 to 8 channels"));
        }
        if !matches!(cfg.bits_per_sample, 16 | 20 | 24 | 32) {
            return Err(Error::InvalidConfig("16, 20, 24 or 32 bits per sample"));
        }
        if !(1..=384_000).contains(&cfg.sample_rate) {
            return Err(Error::InvalidConfig("sample rate 1 to 384000 Hz"));
        }
        // a zero run can't reach 65535 samples within a frame: decoders differ past that
        if !(16..=16_384).contains(&cfg.frame_length) {
            return Err(Error::InvalidConfig("frame length 16 to 16384"));
        }
        Ok(Encoder {
            pending: vec![Vec::new(); usize::from(cfg.channels)],
            coefs: vec![[initial_coefs(); 2]; elements(cfg.channels).len()],
            cfg,
            max_frame_bytes: 0,
            total_bytes: 0,
            total_samples: 0,
        })
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// The ALACSpecificConfig (24 bytes, big-endian): frame length, compatible version 0, bit
    /// depth, pb, mb, kb, channels, maxRun, then the largest frame and the average bitrate so far
    /// (0 = unknown until frames are encoded) and the sample rate.
    pub fn magic_cookie(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(24);
        v.extend_from_slice(&self.cfg.frame_length.to_be_bytes());
        v.extend_from_slice(&[0, self.cfg.bits_per_sample, PB, MB, KB, self.cfg.channels]);
        v.extend_from_slice(&MAX_RUN.to_be_bytes());
        v.extend_from_slice(&self.max_frame_bytes.to_be_bytes());
        // bits per second; 0 (unknown) before the first frame
        let avg = self
            .total_bytes
            .saturating_mul(8)
            .saturating_mul(u64::from(self.cfg.sample_rate))
            .checked_div(self.total_samples)
            .map_or(0, |b| b.min(u64::from(u32::MAX)) as u32);
        v.extend_from_slice(&avg.to_be_bytes());
        v.extend_from_slice(&self.cfg.sample_rate.to_be_bytes());
        v
    }

    /// Add samples (one slice per channel, equal lengths, ALAC channel order); returns the frames
    /// completed, one MP4 / QuickTime sample each.
    pub fn encode_frames(&mut self, planar: &[&[i32]]) -> Result<Vec<Vec<u8>>, Error> {
        if planar.len() != self.pending.len() {
            return Err(Error::Channels { expected: self.pending.len(), got: planar.len() });
        }
        let n = planar.first().map_or(0, |c| c.len());
        if planar.iter().any(|c| c.len() != n) {
            return Err(Error::UnequalChannels);
        }
        let max = (1i64 << (self.cfg.bits_per_sample - 1)) - 1;
        if let Some(&bad) = planar.iter().flat_map(|c| c.iter()).find(|&&s| i64::from(s) > max || i64::from(s) < -max - 1) {
            return Err(Error::SampleRange(i64::from(bad)));
        }
        for (p, c) in self.pending.iter_mut().zip(planar) {
            p.extend_from_slice(c);
        }
        let fl = self.cfg.frame_length as usize;
        let mut out = Vec::new();
        while self.pending.first().is_some_and(|p| p.len() >= fl) {
            let block: Vec<Vec<i32>> = self.pending.iter_mut().map(|p| p.drain(..fl).collect()).collect();
            out.push(self.frame(&block));
        }
        Ok(out)
    }

    /// Samples per channel waiting for a full frame: the length of the frame [`Encoder::finish`]
    /// would write.
    pub fn pending_samples(&self) -> usize {
        self.pending.first().map_or(0, Vec::len)
    }

    /// Encode what is left as a final, shorter frame (none when nothing is left).
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if self.pending.first().is_none_or(Vec::is_empty) {
            return None;
        }
        let block: Vec<Vec<i32>> = self.pending.iter_mut().map(std::mem::take).collect();
        Some(self.frame(&block))
    }

    fn frame(&mut self, block: &[Vec<i32>]) -> Vec<u8> {
        let n = block.first().map_or(0, Vec::len);
        let mut w = BitWriter::new();
        let mut ch = 0usize;
        let (mut sce_tag, mut cpe_tag) = (0u32, 0u32);
        for (e, &id) in elements(self.cfg.channels).iter().enumerate() {
            let state = &mut self.coefs[e];
            if id == ID_CPE {
                let (Some(l), Some(r)) = (block.get(ch), block.get(ch + 1)) else { break };
                write_element(&mut w, ID_CPE, cpe_tag, &[l, r], self.cfg, n, state);
                cpe_tag += 1;
                ch += 2;
            } else {
                let Some(c) = block.get(ch) else { break };
                write_element(&mut w, ID_SCE, sce_tag, &[c], self.cfg, n, state);
                sce_tag += 1;
                ch += 1;
            }
        }
        w.write_bits(ID_END, 3);
        let data = w.finish();
        self.max_frame_bytes = self.max_frame_bytes.max(u32::try_from(data.len()).unwrap_or(u32::MAX));
        self.total_bytes += data.len() as u64;
        self.total_samples += n as u64;
        data
    }
}

/// The low bytes ALAC sends verbatim for a bit depth: 24-bit keeps 16 bits predicted, 32-bit too.
fn bytes_shifted(bits: u8) -> u32 {
    match bits {
        32 => 2,
        24 => 1,
        _ => 0,
    }
}

/// One coded channel of an element: its residuals, the coefficients they start from and the ones
/// they end with, and their coded size.
struct Coded {
    coefs: [i16; ORDER],
    adapted: [i16; ORDER],
    residuals: Vec<i32>,
    bits: usize,
}

const AG: AgParams = AgParams { mb: MB as u32, pb: PB as u32, kb: KB as u32 };

/// Predict one channel (`x` within `chan_bits` bits) from coefficients `start` and measure its
/// coding; `None` when the residual coding can't represent it.
fn code_channel(x: &[i32], start: [i16; ORDER], chan_bits: u32) -> Option<Coded> {
    let mut adapted = start;
    let mut residuals = Vec::new();
    predict::residuals(x, &mut adapted, chan_bits, DEN_SHIFT, &mut residuals);
    let mut count = ag::Counter::default();
    ag::encode(&mut count, &residuals, AG, chan_bits).then_some(Coded { coefs: start, adapted, residuals, bits: count.0 })
}

/// Write one element (a single channel or a pair) of `n` samples: compressed with the smallest
/// mix, or verbatim when that is no smaller (or the residual coding can't represent the frame).
fn write_element(w: &mut BitWriter, id: u32, tag: u32, chans: &[&Vec<i32>], cfg: EncoderConfig, n: usize, state: &mut [[i16; ORDER]; 2]) {
    let pair = chans.len() == 2;
    let shift = bytes_shifted(cfg.bits_per_sample);
    let sh = shift * 8;
    let chan_bits = u32::from(cfg.bits_per_sample) - sh + u32::from(pair);
    // the predicted high parts
    let hi: Vec<Vec<i32>> = chans.iter().map(|c| c.iter().map(|&s| s >> sh).collect()).collect();
    // (bits, mixRes, coded channels)
    let mut best: Option<(usize, i32, Vec<Coded>)> = None;
    let mixes: &[i32] = if pair { &MIX_RES } else { &[0] };
    for &mix_res in mixes {
        let mixed: Vec<Vec<i32>> = match (pair, hi.as_slice()) {
            (true, [l, r]) if mix_res != 0 => {
                let m2 = (1 << MIX_BITS) - mix_res;
                let u = l.iter().zip(r).map(|(&a, &b)| (mix_res * a + m2 * b) >> MIX_BITS).collect();
                let v = l.iter().zip(r).map(|(&a, &b)| a - b).collect();
                vec![u, v]
            }
            _ => hi.clone(),
        };
        let coded: Option<Vec<Coded>> =
            mixed.iter().enumerate().map(|(i, x)| code_channel(x, state.get(i).copied().unwrap_or_else(initial_coefs), chan_bits)).collect();
        let Some(coded) = coded else { continue };
        let total = coded.iter().map(|c| c.bits).sum::<usize>();
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((total, mix_res, coded));
        }
    }
    let partial = n != cfg.frame_length as usize;
    let header = |w: &mut BitWriter, shifted: u32, escape: bool| {
        w.write_bits(id, 3);
        w.write_bits(tag & 15, 4);
        w.write_bits(0, 12);
        w.write_bits(u32::from(partial), 1);
        w.write_bits(shifted, 2);
        w.write_bits(u32::from(escape), 1);
        if partial {
            w.write_bits(n as u32, 32);
        }
    };
    // per channel: mode/denShift, pbFactor/order and the 16-bit coefficients
    let side_bits = 16 + chans.len() * (16 + ORDER * 16);
    let shift_bits = n * chans.len() * sh as usize;
    let raw_bits = n * chans.len() * usize::from(cfg.bits_per_sample);
    // A verbatim channel pair at 32 bits is a 33-bit element to ffmpeg, which refuses it: 32-bit
    // pairs stay compressed whenever they can be (full-scale white noise, the worst case, comes
    // out about 16 % over raw)
    let keep = pair && cfg.bits_per_sample == 32;
    match best {
        Some((bits, mix_res, coded)) if keep || side_bits + shift_bits + bits < raw_bits => {
            header(w, shift, false);
            w.write_bits(if pair { MIX_BITS } else { 0 }, 8);
            w.write_bits(u32::from(mix_res as i8 as u8), 8);
            for c in &coded {
                w.write_bits(DEN_SHIFT, 8); // mode 0, denShift
                w.write_bits((4 << 5) | ORDER as u32, 8); // pbFactor 4, order
                for &k in &c.coefs {
                    w.write_bits(u32::from(k as u16), 16);
                }
            }
            if sh > 0 {
                let mask = (1i32 << sh) - 1;
                for j in 0..n {
                    for c in chans {
                        w.write_bits(c.get(j).map_or(0, |&v| (v & mask) as u32), sh);
                    }
                }
            }
            for (i, c) in coded.iter().enumerate() {
                // measured above: the same residuals code the same way
                ag::encode(w, &c.residuals, AG, chan_bits);
                if let Some(s) = state.get_mut(i) {
                    *s = c.adapted;
                }
            }
        }
        _ => {
            // verbatim: every sample at the full bit depth, channels interleaved; the predictors
            // start over from Apple's coefficients in the next frame
            header(w, 0, true);
            let b = u32::from(cfg.bits_per_sample);
            for j in 0..n {
                for c in chans {
                    let v = c.get(j).copied().unwrap_or(0) as u32;
                    if b > 16 {
                        w.write_bits(v >> 16, b - 16);
                        w.write_bits(v & 0xffff, 16);
                    } else {
                        w.write_bits(v, b);
                    }
                }
            }
            *state = [initial_coefs(); 2];
        }
    }
}

#[cfg(test)]
mod tests;
