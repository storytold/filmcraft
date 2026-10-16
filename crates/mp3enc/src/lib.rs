//! Clean-room MPEG-1 / MPEG-2 Audio Layer III (MP3) encoder, written from ISO/IEC 11172-3:1993
//! (MPEG-1 Audio: §2.4.1 bit stream syntax, §2.4.2 semantics, Annex B tables, Annex C encoding
//! process) and ISO/IEC 13818-3:1998 (the lower sampling frequencies).
//!
//! - Constant bit rate: 32-320 kbit/s at 32 / 44.1 / 48 kHz (MPEG-1, two granules per frame) and
//!   8-160 kbit/s at 16 / 22.05 / 24 kHz (MPEG-2 LSF, one granule per frame).
//! - Mono, stereo, or joint stereo with frame-by-frame mid/side coding.
//! - The polyphase analysis filterbank of Annex C.1.3, the normal-block MDCT and alias reduction;
//!   long blocks only (no block switching yet).
//! - A simple psychoacoustic model (spread band energies against the threshold in quiet), the
//!   inner / outer quantization loops with scalefactors, Huffman table and region selection, and
//!   the bit reservoir (`main_data_begin`).
//! - Output: a plain stream of frames, no ID3 or Xing / Info tag, no CRC.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod filterbank;
mod huffman;
mod huffman_tables;
mod quantize;
mod tables;

use std::collections::VecDeque;

use filmcraft_bitstream::BitWriter;

use filterbank::{Analysis, GRANULE};
use quantize::{Granule, Model};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("MP3: unsupported sample rate {0} Hz (32, 44.1, 48 kHz, or 16, 22.05, 24 kHz)")]
    SampleRate(u32),
    #[error("MP3: unsupported bit rate {kbps} kbit/s at {rate} Hz")]
    Bitrate { kbps: u32, rate: u32 },
    #[error("MP3: unsupported channel count {0} (1 or 2)")]
    Channels(usize),
}

pub type Result<T> = std::result::Result<T, Error>;

/// How two channels are coded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StereoMode {
    /// Left and right coded independently (`mode` 00).
    Stereo,
    /// Joint stereo (`mode` 01): each frame chooses left / right or mid / side.
    #[default]
    JointStereo,
}

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub sample_rate: u32,
    pub channels: usize,
    pub bitrate_kbps: u32,
    pub stereo: StereoMode,
}

impl Config {
    /// `channels` at `sample_rate`, constant `bitrate_kbps`, joint stereo.
    pub fn cbr(sample_rate: u32, channels: usize, bitrate_kbps: u32) -> Config {
        Config { sample_rate, channels, bitrate_kbps, stereo: StereoMode::JointStereo }
    }
}

/// Whether the sample rate needs MPEG-2's lower sampling frequencies.
fn is_lsf(rate: u32) -> Option<bool> {
    if tables::RATES_MPEG1.contains(&rate) {
        Some(false)
    } else if tables::RATES_LSF.contains(&rate) {
        Some(true)
    } else {
        None
    }
}

/// The sample rates the encoder accepts.
pub const SAMPLE_RATES: [u32; 6] = [32_000, 44_100, 48_000, 16_000, 22_050, 24_000];

/// The bit rates (kbit/s) a stream at `sample_rate` can use; empty for an unsupported rate.
pub fn bitrates(sample_rate: u32) -> &'static [u32] {
    match is_lsf(sample_rate) {
        Some(false) => &tables::BITRATES_MPEG1,
        Some(true) => &tables::BITRATES_LSF,
        None => &[],
    }
}

/// The highest bit rate at `sample_rate` not above `kbps` (the lowest one when `kbps` is below
/// all of them); `None` for an unsupported rate.
pub fn nearest_bitrate(sample_rate: u32, kbps: u32) -> Option<u32> {
    let rates = bitrates(sample_rate);
    rates.iter().rev().find(|&&r| r <= kbps).or(rates.first()).copied()
}

/// The supported sample rate an encoder should use for audio at `rate`: `rate` itself when
/// supported, else the closest supported one at or above it (48 kHz above 48 kHz).
pub fn nearest_sample_rate(rate: u32) -> u32 {
    let mut sorted = SAMPLE_RATES;
    sorted.sort_unstable();
    sorted.iter().copied().find(|&r| r >= rate).unwrap_or(48_000)
}

/// Energy of the frequency lines of a full-scale sine (amplitude 1) over one granule.
const FULL_SCALE_TONE_ENERGY: f32 = 1.0;
/// Playback level of a full-scale sine assumed by the threshold in quiet, dB SPL.
const FULL_SCALE_SPL: f64 = 96.0;
/// Samples by which a standard decoder's output lags the input: the polyphase analysis and
/// synthesis filters (481) and the MDCT overlap of one granule (576).
pub const DELAY: u32 = 1057;

/// A Layer III encoder. Feed planar `f32` samples (full scale ±1) to [`Encoder::encode`]; it
/// returns whole frames as they are completed. [`Encoder::flush`] pads the end with silence and
/// returns the rest.
pub struct Encoder {
    cfg: Config,
    lsf: bool,
    sr_index: u32,
    br_index: u32,
    granules: usize,
    analysis: Vec<Analysis>,
    model: Model,
    /// Buffered input per channel, and how much of it is consumed.
    input: Vec<Vec<f32>>,
    consumed: usize,
    /// Frames whose main-data slot is not yet filled: header + side information, slot bytes.
    pending: VecDeque<(Vec<u8>, usize)>,
    /// Main data from the first pending frame's slot on.
    main: Vec<u8>,
    /// Slot bytes of the pending frames.
    slot_total: usize,
    /// Largest `main_data_begin`, bytes.
    max_reservoir: usize,
    /// Remainder accumulator for the padding slot (44.1 / 22.05 kHz).
    pad_acc: u64,
    samples_in: u64,
    frames: u64,
}

impl Encoder {
    pub fn new(cfg: Config) -> Result<Encoder> {
        let lsf = is_lsf(cfg.sample_rate).ok_or(Error::SampleRate(cfg.sample_rate))?;
        if !(1..=2).contains(&cfg.channels) {
            return Err(Error::Channels(cfg.channels));
        }
        let rates = if lsf { &tables::RATES_LSF } else { &tables::RATES_MPEG1 };
        let sr_index = rates.iter().position(|&r| r == cfg.sample_rate).ok_or(Error::SampleRate(cfg.sample_rate))?;
        let br_index =
            bitrates(cfg.sample_rate).iter().position(|&b| b == cfg.bitrate_kbps).ok_or(Error::Bitrate { kbps: cfg.bitrate_kbps, rate: cfg.sample_rate })?;
        let bands = if lsf { tables::SFB_LSF[sr_index] } else { tables::SFB_MPEG1[sr_index] };
        let line_hz = cfg.sample_rate as f64 / 2.0 / GRANULE as f64;
        let mut ath = [0f32; 22];
        for (b, a) in ath.iter_mut().enumerate() {
            let (lo, hi) = (usize::from(bands[b]), usize::from(bands[b + 1]));
            let db = (lo..hi).map(|i| tables::ath_db((i as f64 + 0.5) * line_hz)).fold(f64::MAX, f64::min);
            *a = (f64::from(FULL_SCALE_TONE_ENERGY) * 10f64.powf((db - FULL_SCALE_SPL) / 10.0)) as f32;
        }
        let cutoff_hz = cutoff_hz(cfg.bitrate_kbps, cfg.channels).min(cfg.sample_rate as f64 * 0.5);
        let cutoff = ((cutoff_hz / line_hz).ceil() as usize).min(GRANULE);
        let frame_bits = u64::from(cfg.bitrate_kbps) * 1000 * if lsf { 576 } else { 1152 } / u64::from(cfg.sample_rate);
        // reservoir and frame must fit the decoder's 7680-bit input buffer
        let max_reservoir = (7680u64.saturating_sub(frame_bits) / 8).min(if lsf { 255 } else { 511 }) as usize;
        Ok(Encoder {
            cfg,
            lsf,
            sr_index: sr_index as u32,
            br_index: br_index as u32 + 1,
            granules: if lsf { 1 } else { 2 },
            analysis: vec![Analysis::new(); cfg.channels],
            model: Model::new(lsf, bands, ath, cutoff),
            input: vec![Vec::new(); cfg.channels],
            consumed: 0,
            pending: VecDeque::new(),
            main: Vec::new(),
            slot_total: 0,
            max_reservoir,
            pad_acc: 0,
            samples_in: 0,
            frames: 0,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Samples per channel in a frame: 1152 (MPEG-1) or 576 (MPEG-2 LSF).
    pub fn frame_samples(&self) -> usize {
        GRANULE * self.granules
    }

    /// Samples by which a standard decoder's output lags the input.
    pub fn delay(&self) -> u32 {
        DELAY
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Encode planar samples (one slice per channel; a missing channel is silence, extra ones
    /// are ignored, every channel takes the first channel's length). Returns the completed frames.
    pub fn encode(&mut self, planar: &[&[f32]]) -> Vec<u8> {
        let n = planar.first().map_or(0, |p| p.len());
        for (c, buf) in self.input.iter_mut().enumerate() {
            let src = planar.get(c).copied().unwrap_or(&[]);
            buf.extend((0..n).map(|i| src.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-4.0, 4.0)));
        }
        self.samples_in = self.samples_in.saturating_add(n as u64);
        let mut out = Vec::new();
        self.drain(&mut out);
        out
    }

    /// Encode the buffered rest padded with silence, plus enough silence that a decoder outputs
    /// every input sample, and return the remaining frames.
    pub fn flush(&mut self) -> Vec<u8> {
        let fs = self.frame_samples() as u64;
        let needed = if self.samples_in == 0 { self.frames } else { self.samples_in.saturating_add(u64::from(DELAY)).div_ceil(fs) };
        let have = self.input.first().map_or(0, |b| b.len().saturating_sub(self.consumed));
        let want = (needed.saturating_sub(self.frames) as usize).saturating_mul(fs as usize);
        if want > have {
            for buf in &mut self.input {
                buf.resize(self.consumed + want, 0.0);
            }
        }
        let mut out = Vec::new();
        self.drain(&mut out);
        // fill the last frames' slots and write them out
        self.main.resize(self.slot_total.max(self.main.len()), 0);
        self.emit(&mut out);
        self.input.iter_mut().for_each(Vec::clear);
        self.consumed = 0;
        out
    }

    /// Encode every whole frame of buffered input.
    fn drain(&mut self, out: &mut Vec<u8>) {
        let fs = self.frame_samples();
        let mut pcm = vec![[0f32; GRANULE]; self.cfg.channels * self.granules];
        while self.input.first().is_some_and(|b| b.len() - self.consumed >= fs) {
            for (c, buf) in self.input.iter().enumerate() {
                for gr in 0..self.granules {
                    let start = self.consumed + gr * GRANULE;
                    if let (Some(dst), Some(src)) = (pcm.get_mut(gr * self.cfg.channels + c), buf.get(start..start + GRANULE)) {
                        dst.copy_from_slice(src);
                    }
                }
            }
            self.consumed += fs;
            self.encode_frame(&pcm);
            self.emit(out);
        }
        if self.consumed > 0 {
            let k = self.consumed;
            self.input.iter_mut().for_each(|b| {
                b.drain(..k.min(b.len()));
            });
            self.consumed = 0;
        }
    }

    /// Bytes of the next frame (with its padding slot).
    fn next_frame_len(&mut self) -> usize {
        let per = if self.lsf { 72_000 } else { 144_000 };
        let num = per * u64::from(self.cfg.bitrate_kbps);
        let sr = u64::from(self.cfg.sample_rate);
        self.pad_acc += num % sr;
        let pad = self.pad_acc >= sr;
        if pad {
            self.pad_acc -= sr;
        }
        (num / sr) as usize + usize::from(pad)
    }

    fn side_info_len(&self) -> usize {
        match (self.lsf, self.cfg.channels) {
            (false, 1) => 17,
            (false, _) => 32,
            (true, 1) => 9,
            (true, _) => 17,
        }
    }

    /// Analyse, quantize and assemble one frame (`pcm[gr * channels + ch]`).
    fn encode_frame(&mut self, pcm: &[[f32; GRANULE]]) {
        let nch = self.cfg.channels;
        let ngr = self.granules;
        let mut xr = vec![[0f32; GRANULE]; nch * ngr];
        for gr in 0..ngr {
            for ch in 0..nch {
                let k = gr * nch + ch;
                if let (Some(a), Some(p), Some(x)) = (self.analysis.get_mut(ch), pcm.get(k), xr.get_mut(k)) {
                    a.granule(p, x);
                    x.iter_mut().skip(self.model.cutoff).for_each(|v| *v = 0.0);
                }
            }
        }
        let mut xmin: Vec<[f32; 22]> = xr.iter().map(|x| self.model.thresholds(x)).collect();
        let ms = nch == 2 && self.cfg.stereo == StereoMode::JointStereo && use_mid_side(&xr);
        if ms {
            for gr in 0..ngr {
                let (l, r) = xr.split_at_mut(gr * 2 + 1);
                if let (Some(l), Some(r)) = (l.last_mut(), r.first_mut()) {
                    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                        let (m, s) = ((*a + *b) * std::f32::consts::FRAC_1_SQRT_2, (*a - *b) * std::f32::consts::FRAC_1_SQRT_2);
                        *a = m;
                        *b = s;
                    }
                }
                let t: [f32; 22] = std::array::from_fn(|b| xmin[gr * 2][b].min(xmin[gr * 2 + 1][b]));
                xmin[gr * 2] = t;
                xmin[gr * 2 + 1] = t;
            }
        }
        // the reservoir: free bytes in the pending frames' slots, stuffed down to the maximum
        let free = self.slot_total.saturating_sub(self.main.len());
        if free > self.max_reservoir {
            self.main.resize(self.slot_total - self.max_reservoir, 0);
        }
        let main_data_begin = free.min(self.max_reservoir);
        let frame_len = self.next_frame_len();
        let slot = frame_len.saturating_sub(4 + self.side_info_len());
        let budget = 8 * (main_data_begin + slot);
        let n = nch * ngr;
        let mean = 8 * slot / n;
        let max_reservoir_bits = 8 * self.max_reservoir;
        let mut used = 0usize;
        let mut coded: Vec<Granule> = Vec::with_capacity(n);
        for (k, (x, t)) in xr.iter().zip(&xmin).enumerate() {
            let left = n - k;
            let remaining = budget.saturating_sub(used);
            let surplus = remaining.saturating_sub(mean * left);
            let max_bits = (mean + surplus * 3 / 5 / left).min(remaining).min(4095);
            let room = max_reservoir_bits.saturating_sub(surplus);
            let min_bits = mean.saturating_sub(room / left);
            let g = self.model.encode(x, t, max_bits, min_bits);
            used += g.bits();
            coded.push(g);
        }
        let mut md = BitWriter::new();
        for g in &coded {
            g.write(&mut md, &self.model);
        }
        let mut main = md.finish();
        if main.len() > main_data_begin + slot || coded.iter().any(|g| g.bits() > 4095) {
            // never expected: fall back to silence rather than write a broken frame
            coded = (0..n).map(|_| self.model.encode(&[0.0; GRANULE], &[f32::MAX; 22], 0, 0)).collect();
            main = Vec::new();
        }
        let head = self.header_and_side_info(frame_len, ms, main_data_begin, &coded);
        self.pending.push_back((head, slot));
        self.slot_total += slot;
        self.main.extend_from_slice(&main);
        self.frames += 1;
    }

    /// Write out the pending frames whose slots are full.
    fn emit(&mut self, out: &mut Vec<u8>) {
        while let Some((_, slot)) = self.pending.front() {
            let slot = *slot;
            if self.main.len() < slot {
                break;
            }
            if let Some((head, _)) = self.pending.pop_front() {
                out.extend_from_slice(&head);
                out.extend(self.main.drain(..slot));
                self.slot_total -= slot;
            }
        }
    }

    fn header_and_side_info(&self, frame_len: usize, ms: bool, main_data_begin: usize, coded: &[Granule]) -> Vec<u8> {
        let nch = self.cfg.channels;
        let padding = frame_len > (if self.lsf { 72_000 } else { 144_000 } * self.cfg.bitrate_kbps / self.cfg.sample_rate) as usize;
        let mode = match (nch, self.cfg.stereo) {
            (1, _) => 3,
            (_, StereoMode::Stereo) => 0,
            (_, StereoMode::JointStereo) => 1,
        };
        let mut w = BitWriter::new();
        w.write_bits(0x7FF, 11);
        w.write_bits(if self.lsf { 2 } else { 3 }, 2); // ID: MPEG-1 = 11, MPEG-2 = 10
        w.write_bits(1, 2); // Layer III
        w.write_bit(true); // protection_bit: no CRC
        w.write_bits(self.br_index, 4);
        w.write_bits(self.sr_index, 2);
        w.write_bit(padding);
        w.write_bit(false); // private_bit
        w.write_bits(mode, 2);
        w.write_bits(if ms { 2 } else { 0 }, 2); // mode_extension: mid/side, no intensity
        w.write_bit(false); // copyright
        w.write_bit(true); // original
        w.write_bits(0, 2); // emphasis
        if self.lsf {
            w.write_bits(main_data_begin as u32, 8);
            w.write_bits(0, if nch == 1 { 1 } else { 2 });
        } else {
            w.write_bits(main_data_begin as u32, 9);
            w.write_bits(0, if nch == 1 { 5 } else { 3 });
            w.write_bits(0, 4 * nch as u32); // scfsi
        }
        for g in coded {
            let c = &g.coding;
            w.write_bits(g.bits() as u32, 12);
            w.write_bits(c.big_values as u32, 9);
            w.write_bits(g.global_gain, 8);
            w.write_bits(g.scalefac_compress, if self.lsf { 9 } else { 4 });
            w.write_bit(false); // window_switching_flag
            for t in c.table_select {
                w.write_bits(t as u32, 5);
            }
            w.write_bits(c.region0_count as u32, 4);
            w.write_bits(c.region1_count as u32, 3);
            if !self.lsf {
                w.write_bit(false); // preflag
            }
            w.write_bit(g.scalefac_scale);
            w.write_bits(c.count1_table as u32, 1);
        }
        w.finish()
    }
}

/// Mid/side for a frame when the side signal is much weaker than the mid signal.
fn use_mid_side(xr: &[[f32; GRANULE]]) -> bool {
    let (mut m, mut s) = (0f64, 0f64);
    for pair in xr.as_chunks::<2>().0 {
        for (l, r) in pair[0].iter().zip(pair[1].iter()) {
            let (a, b) = (f64::from(*l), f64::from(*r));
            m += (a + b) * (a + b);
            s += (a - b) * (a - b);
        }
    }
    s < 0.25 * m
}

/// The low-pass for a bit rate: the band the bits can carry (joint stereo counted as 1.6
/// channels' worth).
fn cutoff_hz(kbps: u32, channels: usize) -> f64 {
    let per = f64::from(kbps) / if channels > 1 { 1.6 } else { 1.0 };
    const STEPS: [(f64, f64); 11] = [
        (140.0, 20_000.0),
        (120.0, 19_000.0),
        (100.0, 18_000.0),
        (80.0, 16_500.0),
        (64.0, 15_000.0),
        (56.0, 13_500.0),
        (48.0, 12_000.0),
        (40.0, 10_000.0),
        (32.0, 8_000.0),
        (24.0, 6_500.0),
        (16.0, 5_000.0),
    ];
    STEPS.iter().find(|(k, _)| per >= *k).map_or(3_500.0, |s| s.1)
}

#[cfg(test)]
mod tests;
