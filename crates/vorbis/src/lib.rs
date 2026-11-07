//! Clean-room Vorbis I encoder, written from the Vorbis I specification (Xiph.Org, 2020-07-04).
//!
//! Layer L0: no dependencies, no `unsafe`, builds for `wasm32-unknown-unknown`. No libvorbis,
//! FFmpeg or other Vorbis implementation was read; ffmpeg is used only as an external test oracle.
//!
//! The stream: one mode of 2048-sample blocks; per channel a floor 1 envelope of 34 points; one
//! residue type 2 over the interleaved channels, partitions of 32 values in seven classes (silent,
//! three single-pass lattice books, and two- and three-pass cascades for large values); no channel
//! coupling. Quality (-1 to 10) sets how far the floor sits from the spectral envelope (below it
//! for tonal regions, above it for noise-like ones, from the spectral flatness), and with it the
//! residue's precision: variable bit rate, like other Vorbis encoders.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod book;
mod floor;
mod mdct;

use book::{BitWriter, Codebook};
use floor::Floor1;
use mdct::Mdct;

/// Errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidArgument(&'static str),
    /// An encoder invariant failed (a bug, reported instead of panicking).
    Internal(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidArgument(s) => write!(f, "invalid argument: {s}"),
            Error::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Block size (samples); the short block size of the header is never used.
pub const BLOCK: usize = 2048;
const HOP: usize = BLOCK / 2;
const PARTITION: usize = 32;
const CLASSES: usize = 8;
/// Largest residue magnitude the cascade codes: 7·225 + 7·15 + 7.
const MAX_RESIDUE: i32 = 1687;

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncoderConfig {
    pub sample_rate: u32,
    /// 1 to 8, in Vorbis channel order (§4.3.9).
    pub channels: usize,
    /// -1 (smallest) to 10 (best); clamped.
    pub quality: f32,
}

impl EncoderConfig {
    pub fn new(sample_rate: u32, channels: usize, quality: f32) -> EncoderConfig {
        EncoderConfig { sample_rate, channels, quality }
    }
}

/// Book indices in the setup header.
const BOOK_CLASS: usize = 0;
const BOOK_FLOOR: usize = 1;
const BOOK_A1_SPARSE: usize = 2;
const BOOK_A1: usize = 3;
const BOOK_A2: usize = 4;
const BOOK_A4: usize = 5;
const BOOK_F: usize = 6;
const BOOK_M: usize = 7;
const BOOK_C: usize = 8;

/// Books per class and pass (`None`: nothing coded in that pass).
const CASCADE: [[Option<usize>; 3]; CLASSES] = [
    [None, None, None],
    [Some(BOOK_A1_SPARSE), None, None],
    [Some(BOOK_A1), None, None],
    [Some(BOOK_A2), None, None],
    [Some(BOOK_A4), None, None],
    [Some(BOOK_F), None, None],
    [Some(BOOK_M), Some(BOOK_F), None],
    [Some(BOOK_C), Some(BOOK_M), Some(BOOK_F)],
];

/// A Vorbis encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    books: Vec<Codebook>,
    floor: Floor1,
    mdct: Mdct,
    window: Vec<f32>,
    /// Input not yet past a block, per channel, starting at the next block's first sample.
    pending: Vec<Vec<f32>>,
    samples_in: u64,
    blocks: u64,
    finished: bool,
    /// Floor offset below the envelope (dB) for tonal and for noise-like regions.
    tonal_db: f32,
    noise_db: f32,
    /// Residue magnitudes below this quantise to 0.
    dead_zone: f32,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if !(1..=8).contains(&cfg.channels) {
            return Err(Error::InvalidArgument("Vorbis encoder: 1 to 8 channels"));
        }
        if !(1..=768_000).contains(&cfg.sample_rate) {
            return Err(Error::InvalidArgument("Vorbis encoder: sample rate"));
        }
        let quality = if cfg.quality.is_finite() { cfg.quality.clamp(-1.0, 10.0) } else { 3.0 };
        let cfg = EncoderConfig { quality, ..cfg };
        let books = vec![
            Codebook::scalar(2, &(0..CLASSES * CLASSES).map(|e| class_weight(e / CLASSES) * class_weight(e % CLASSES)).collect::<Vec<_>>())?,
            Codebook::scalar(1, &(0..floor::RANGE).map(|v| 1.0 / (1.0 + f64::from(v)).powf(1.6)).collect::<Vec<_>>())?,
            Codebook::lattice(4, 3, -1, 1, |v| if v == 0 { 0.92 } else { 0.04 })?,
            Codebook::lattice(4, 3, -1, 1, |v| if v == 0 { 0.5 } else { 0.25 })?,
            Codebook::lattice(2, 5, -2, 1, |v| 0.6f64.powi(v.abs()))?,
            Codebook::lattice(2, 9, -4, 1, |v| 0.7f64.powi(v.abs()))?,
            Codebook::lattice(2, 15, -7, 1, |v| 0.8f64.powi(v.abs()))?,
            Codebook::lattice(2, 15, -105, 15, |v| 0.55f64.powi((v / 15).abs()))?,
            Codebook::lattice(2, 15, -1575, 225, |v| 0.4f64.powi((v / 225).abs()))?,
        ];
        Ok(Encoder {
            cfg,
            books,
            floor: Floor1::new(BOOK_FLOOR),
            mdct: Mdct::new(BLOCK),
            window: mdct::window(BLOCK),
            // the first block is centred on sample 0: the decoder's first output is input sample 0
            pending: vec![vec![0.0; HOP]; cfg.channels],
            samples_in: 0,
            blocks: 0,
            finished: false,
            // tones 4 dB (-1) to 30.4 dB (10) under their envelope; noise 14 dB above it (-1)
            // to 4.1 dB (10)
            tonal_db: 4.0 + 2.4 * (quality + 1.0),
            noise_db: -14.0 + 0.9 * (quality + 1.0),
            dead_zone: 0.8 - 0.025 * (quality + 1.0),
        })
    }

    pub fn channels(&self) -> usize {
        self.cfg.channels
    }

    /// Input samples per channel taken so far.
    pub fn samples_in(&self) -> u64 {
        self.samples_in
    }

    /// The identification header (§4.2.2).
    pub fn identification(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write(1, 8);
        w.bytes(b"vorbis");
        w.write(0, 32);
        w.write(self.cfg.channels as u32, 8);
        w.write(self.cfg.sample_rate, 32);
        w.write(0, 32); // bit rates: unset (variable)
        w.write(0, 32);
        w.write(0, 32);
        w.write(8, 4); // 256-sample short blocks (unused)
        w.write(11, 4); // 2048-sample long blocks
        w.flag(true);
        w.finish()
    }

    /// The setup header (§4.2.4): codebooks, floor, residue, mapping and mode.
    pub fn setup(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write(5, 8);
        w.bytes(b"vorbis");
        w.write(self.books.len() as u32 - 1, 8);
        for b in &self.books {
            b.write_header(&mut w);
        }
        w.write(0, 6); // one time-domain transform placeholder
        w.write(0, 16);
        w.write(0, 6); // one floor
        w.write(1, 16);
        self.floor.write_header(&mut w);
        w.write(0, 6); // one residue, type 2
        w.write(2, 16);
        let end = (self.cfg.channels * HOP) as u32;
        w.write(0, 24);
        w.write(end, 24);
        w.write(PARTITION as u32 - 1, 24);
        w.write(CLASSES as u32 - 1, 6);
        w.write(BOOK_CLASS as u32, 8);
        for c in &CASCADE {
            let bits: u32 = c.iter().enumerate().map(|(j, b)| u32::from(b.is_some()) << j).sum();
            w.write(bits & 7, 3);
            w.flag(bits > 7);
            if bits > 7 {
                w.write(bits >> 3, 5);
            }
        }
        for c in &CASCADE {
            for b in c.iter().flatten() {
                w.write(*b as u32, 8);
            }
        }
        w.write(0, 6); // one mapping, type 0
        w.write(0, 16);
        w.flag(false); // one submap
        w.flag(false); // no coupling
        w.write(0, 2);
        w.write(0, 8); // submap 0: time, floor 0, residue 0
        w.write(0, 8);
        w.write(0, 8);
        w.write(0, 6); // one mode: long blocks, mapping 0
        w.flag(true);
        w.write(0, 16);
        w.write(0, 16);
        w.write(0, 8);
        w.flag(true);
        w.finish()
    }

    /// Encodes planar samples (one slice per channel, all the same length, nominal range ±1);
    /// returns the packets completed with their granule positions. NaN or infinite samples, or a
    /// channel count or length mismatch, are an error.
    pub fn encode(&mut self, planar: &[&[f32]]) -> Result<Vec<(Vec<u8>, i64)>> {
        if self.finished {
            return Err(Error::InvalidArgument("Vorbis encoder: encode after finish"));
        }
        if planar.len() != self.cfg.channels {
            return Err(Error::InvalidArgument("Vorbis encoder: one slice per channel"));
        }
        let len = planar.first().map_or(0, |p| p.len());
        if planar.iter().any(|p| p.len() != len) {
            return Err(Error::InvalidArgument("Vorbis encoder: channels of different lengths"));
        }
        if planar.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(Error::InvalidArgument("Vorbis encoder: NaN or infinite sample"));
        }
        for (b, p) in self.pending.iter_mut().zip(planar) {
            b.extend_from_slice(p);
        }
        self.samples_in += len as u64;
        self.drain()
    }

    /// Encodes what remains, padded with silence; the last packet's granule position is the
    /// input's length, so the decoder trims the padding (§A.2).
    pub fn finish(&mut self) -> Result<Vec<(Vec<u8>, i64)>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        // block k ends output at k·HOP: blocks 0..=ceil(len / HOP)
        let blocks = self.samples_in.div_ceil(HOP as u64) + 1;
        let missing = blocks.saturating_sub(self.blocks) as usize;
        let need = (missing + 1) * HOP;
        for b in self.pending.iter_mut() {
            if b.len() < need {
                b.resize(need, 0.0);
            }
        }
        let mut out = Vec::new();
        for _ in 0..missing {
            out.push(self.block()?);
        }
        Ok(out)
    }

    fn drain(&mut self) -> Result<Vec<(Vec<u8>, i64)>> {
        let mut out = Vec::new();
        while self.pending.first().is_some_and(|b| b.len() >= BLOCK) {
            out.push(self.block()?);
        }
        Ok(out)
    }

    /// Encodes the block at the front of `pending` and advances by a hop.
    fn block(&mut self) -> Result<(Vec<u8>, i64)> {
        let ch = self.cfg.channels;
        let mut w = BitWriter::new();
        w.write(0, 1); // audio packet; one mode, so no mode number
        w.flag(true); // previous and next windows long
        w.flag(true);
        let mut residue = vec![0i32; ch * HOP];
        let mut any = false;
        let mut x = vec![0f32; BLOCK];
        let mut coeffs = vec![0f32; HOP];
        for c in 0..ch {
            let src = self.pending.get(c).ok_or(Error::Internal("channel buffer"))?;
            for (i, v) in x.iter_mut().enumerate() {
                *v = src.get(i).copied().unwrap_or(0.0).clamp(-8.0, 8.0) * self.window[i];
            }
            self.mdct.forward(&x, &mut coeffs, 2.0 / HOP as f64);
            let peak = coeffs.iter().fold(0f32, |m, v| m.max(v.abs()));
            // (a NaN peak cannot happen: samples are finite and clamped)
            if peak <= 1e-7 {
                w.flag(false); // floor unused: the channel is silent
                continue;
            }
            any = true;
            w.flag(true);
            let want = self.floor_targets(&coeffs);
            let curve = self.floor.encode(&mut w, &self.books[BOOK_FLOOR], &want, HOP);
            for (k, (&v, &f)) in coeffs.iter().zip(&curve).enumerate() {
                let r = if f > 0.0 { v / f } else { 0.0 };
                // a dead zone: values just past half a step are noise peaks, not worth a ±1
                let q = if r.abs() < self.dead_zone { 0.0 } else { r.round() };
                residue[k * ch + c] = if q.is_finite() { (q as i32).clamp(-MAX_RESIDUE, MAX_RESIDUE) } else { 0 };
            }
        }
        if any {
            self.write_residue(&mut w, &residue);
        }
        let granule = if self.blocks == 0 {
            0
        } else {
            let natural = self.blocks.saturating_mul(HOP as u64);
            if self.finished { natural.min(self.samples_in) } else { natural }
        };
        for b in self.pending.iter_mut() {
            b.drain(..HOP.min(b.len()));
        }
        self.blocks += 1;
        Ok((w.finish(), i64::try_from(granule).unwrap_or(i64::MAX)))
    }

    /// Floor values at the floor's points: the spectral envelope (power smoothed over a quarter of
    /// the frequency, at least 8 bins, on each side) less an offset that grows with the region's tonality, from its
    /// spectral flatness: noise needs a few dB of precision, tones much more.
    fn floor_targets(&self, coeffs: &[f32]) -> Vec<i32> {
        let n = coeffs.len();
        let mut cum = vec![0f64; n + 1];
        let mut cum_ln = vec![0f64; n + 1];
        for (i, &v) in coeffs.iter().enumerate() {
            let p = f64::from(v) * f64::from(v);
            cum[i + 1] = cum[i] + p;
            cum_ln[i + 1] = cum_ln[i] + (p + 1e-30).ln();
        }
        self.floor
            .xs
            .iter()
            .map(|&x| {
                let k = (x as usize).min(n - 1);
                let half = (k / 4).max(8);
                let (lo, hi) = (k.saturating_sub(half), (k + half + 1).min(n));
                let len = (hi - lo) as f64;
                let am = (cum[hi] - cum[lo]) / len;
                let gm = ((cum_ln[hi] - cum_ln[lo]) / len).exp();
                // spectral flatness: about -2.5 dB for noise, far below for a tone
                let sfm_db = 10.0 * (gm / am.max(1e-30)).max(1e-30).log10();
                let tonality = ((-sfm_db - 2.5) / 20.0).clamp(0.0, 1.0);
                let offset = tonality * f64::from(self.tonal_db) + (1.0 - tonality) * f64::from(self.noise_db);
                let amp = (am.sqrt() * 10f64.powf(-offset / 20.0)).max(1e-7) as f32;
                (floor::db_index(amp) + 1) / floor::MULTIPLIER as i32
            })
            .collect()
    }

    /// Residue type 2 (§8.6.2 with the interleaved vector): classes per partition, then the
    /// cascade's passes in the decoder's order.
    fn write_residue(&self, w: &mut BitWriter, r: &[i32]) {
        let parts = r.len() / PARTITION;
        let classes: Vec<usize> = r.chunks(PARTITION).map(class_of).collect();
        let per_word = self.books[BOOK_CLASS].dimensions as usize;
        for pass in 0..3 {
            let mut pc = 0;
            while pc < parts {
                if pass == 0 {
                    let mut e = 0usize;
                    for i in 0..per_word {
                        e = e * CLASSES + classes.get(pc + i).copied().unwrap_or(0);
                    }
                    self.books[BOOK_CLASS].put(w, e);
                }
                for _ in 0..per_word {
                    if pc >= parts {
                        break;
                    }
                    let class = classes[pc];
                    if let Some(b) = CASCADE[class][pass] {
                        let book = &self.books[b];
                        let dims = book.dimensions as usize;
                        let part = &r[pc * PARTITION..(pc + 1) * PARTITION];
                        for chunk in part.chunks(dims) {
                            let v: Vec<i32> = chunk.iter().map(|&x| pass_value(x, class, pass)).collect();
                            book.put(w, book.entry_of(&v));
                        }
                    }
                    pc += 1;
                }
            }
        }
    }
}

/// Relative frequency of each residue class (for the class book's code lengths).
fn class_weight(c: usize) -> f64 {
    [0.3, 0.2, 0.12, 0.12, 0.1, 0.08, 0.06, 0.02].get(c).copied().unwrap_or(0.01)
}

/// The part of residue value `v` coded in `pass` for `class`: the passes add up to `v`.
fn pass_value(v: i32, class: usize, pass: usize) -> i32 {
    let split = |v: i32, step: i32| -> (i32, i32) {
        let a = ((f64::from(v) / f64::from(step)).round() as i32).clamp(-7, 7);
        (a * step, v - a * step)
    };
    match (class, pass) {
        (6, 0) => split(v, 15).0,
        (6, _) => split(v, 15).1,
        (7, 0) => split(v, 225).0,
        (7, 1) => split(split(v, 225).1, 15).0,
        (7, _) => split(split(v, 225).1, 15).1,
        _ => v,
    }
}

/// The residue class of a partition: silent; ±1 with at most four nonzero values, or more; then
/// by the largest magnitude.
fn class_of(p: &[i32]) -> usize {
    let m = p.iter().map(|v| v.abs()).max().unwrap_or(0);
    match m {
        0 => 0,
        1 if p.iter().filter(|&&v| v != 0).count() <= 4 => 1,
        1 => 2,
        2 => 3,
        3..=4 => 4,
        5..=7 => 5,
        8..=112 => 6,
        _ => 7,
    }
}

#[cfg(test)]
mod tests;
