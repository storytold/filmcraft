//! Clean-room FLAC encoder (RFC 9639, Free Lossless Audio Codec).
//!
//! - [`Encoder`]: planar integer samples (8–24 bit, 1–8 channels) → a `.flac` stream: the
//!   [`Encoder::header`] (`fLaC`, STREAMINFO, a VORBIS_COMMENT naming the encoder), then frames of
//!   a fixed block size. Each channel of each frame is coded as constant, verbatim, a fixed
//!   polynomial predictor or linear prediction, whichever is smallest, with partitioned Rice codes;
//!   stereo frames also try left/side, side/right and mid/side.
//! - The STREAMINFO sizes and MD5 are only known at the end: write the header first, then overwrite
//!   [`STREAMINFO_OFFSET`] with [`Encoder::streaminfo`] once [`Encoder::finish`] has run (or keep
//!   the placeholder, whose zeros mean "unknown" and decode fine).
//!
//! Lossless at every compression level; the level (0–8) only trades encoding time for size.
//!
//! Layer L0: depends only on `filmcraft-bitstream` (+ `thiserror`); no `unsafe`; builds for
//! `wasm32-unknown-unknown`.
//!
//! ```
//! use filmcraft_flac::{Encoder, EncoderConfig, STREAMINFO_OFFSET};
//!
//! let left: Vec<i32> = (0..48_000).map(|i| ((i as f64 * 0.05).sin() * 20_000.0) as i32).collect();
//! let right = left.clone();
//! let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 16))?;
//! let mut file = enc.header();
//! file.extend(enc.encode(&[&left, &right])?);
//! file.extend(enc.finish());
//! let at = STREAMINFO_OFFSET as usize;
//! file[at..at + 34].copy_from_slice(&enc.streaminfo());
//! assert_eq!(&file[..4], b"fLaC");
//! assert!(file.len() < 48_000 * 2 * 2 / 2); // well under half of 16-bit PCM
//! # Ok::<(), filmcraft_flac::Error>(())
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod crc;
mod frame;
mod md5;
mod predict;

use frame::{FrameSpec, Stereo};
use predict::{Effort, Subframe};

/// Byte offset of the 34-byte STREAMINFO body in [`Encoder::header`].
pub const STREAMINFO_OFFSET: u64 = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("expected {expected} channels, got {got}")]
    Channels { expected: usize, got: usize },
    #[error("channels of unequal length")]
    UnequalChannels,
    #[error("sample {0} does not fit the configured sample size")]
    SampleRange(i32),
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    /// 1 to 1 048 575 Hz.
    pub sample_rate: u32,
    /// 1–8 (FLAC channel order: L R C LFE Ls Rs … for 5.1).
    pub channels: u8,
    /// 8–24.
    pub bits_per_sample: u8,
    /// Samples per channel per frame, 16–65535 (4096 by default).
    pub block_size: u16,
    /// 0 (fastest) to 8 (smallest); 5 by default.
    pub level: u8,
}

impl EncoderConfig {
    pub fn new(sample_rate: u32, channels: u8, bits_per_sample: u8) -> Self {
        EncoderConfig { sample_rate, channels, bits_per_sample, block_size: 4096, level: 5 }
    }
}

pub struct Encoder {
    cfg: EncoderConfig,
    effort: Effort,
    /// Samples waiting for a full block, per channel.
    pending: Vec<Vec<i32>>,
    frame_number: u64,
    total_samples: u64,
    min_frame: u32,
    max_frame: u32,
    md5: md5::Md5,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Self, Error> {
        if !(1..=8).contains(&cfg.channels) {
            return Err(Error::InvalidConfig("1 to 8 channels"));
        }
        if !(8..=24).contains(&cfg.bits_per_sample) {
            return Err(Error::InvalidConfig("8 to 24 bits per sample"));
        }
        if !(1..=1_048_575).contains(&cfg.sample_rate) {
            return Err(Error::InvalidConfig("sample rate 1 to 1048575 Hz"));
        }
        if cfg.block_size < 16 {
            return Err(Error::InvalidConfig("block size 16 to 65535"));
        }
        let effort = Effort::level(cfg.level);
        Ok(Encoder {
            pending: vec![Vec::new(); usize::from(cfg.channels)],
            cfg,
            effort,
            frame_number: 0,
            total_samples: 0,
            min_frame: 0,
            max_frame: 0,
            md5: md5::Md5::default(),
        })
    }

    /// `fLaC`, the STREAMINFO block (sizes and MD5 unknown yet: see [`Encoder::streaminfo`]) and a
    /// VORBIS_COMMENT block with the vendor string.
    pub fn header(&self) -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend_from_slice(&[0, 0, 0, 34]);
        v.extend_from_slice(&self.streaminfo_with(0, 0, 0, [0; 16]));
        let vendor = concat!("filmcraft-flac ", env!("CARGO_PKG_VERSION"));
        let len = 4 + vendor.len() + 4;
        // last metadata block, type 4 (VORBIS_COMMENT), little-endian lengths inside
        v.push(0x84);
        v.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
        v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        v.extend_from_slice(vendor.as_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    /// The final STREAMINFO body: frame sizes, total samples and MD5 of what has been encoded.
    pub fn streaminfo(&self) -> [u8; 34] {
        self.streaminfo_with(self.min_frame, self.max_frame, self.total_samples, self.md5.digest())
    }

    fn streaminfo_with(&self, min_frame: u32, max_frame: u32, total: u64, md5: [u8; 16]) -> [u8; 34] {
        let mut w = filmcraft_bitstream::BitWriter::new();
        let bs = u32::from(self.cfg.block_size);
        w.write_bits(bs, 16);
        w.write_bits(bs, 16);
        w.write_bits(min_frame.min(0xFF_FFFF), 24);
        w.write_bits(max_frame.min(0xFF_FFFF), 24);
        w.write_bits(self.cfg.sample_rate, 20);
        w.write_bits(u32::from(self.cfg.channels) - 1, 3);
        w.write_bits(u32::from(self.cfg.bits_per_sample) - 1, 5);
        // 36 bits; 0 means unknown, and a count that does not fit is left unknown
        let total = if total >> 36 == 0 { total } else { 0 };
        w.write_bits((total >> 32) as u32, 4);
        w.write_bits(total as u32, 32);
        w.write_bytes(&md5);
        let mut out = [0u8; 34];
        out.copy_from_slice(w.bytes().get(..34).unwrap_or(&[0; 34]));
        out
    }

    /// Add samples (one slice per channel, equal lengths); returns the frames completed.
    pub fn encode(&mut self, planar: &[&[i32]]) -> Result<Vec<u8>, Error> {
        Ok(self.encode_frames(planar)?.concat())
    }

    /// [`Encoder::encode`] with each completed frame on its own (one MP4 / QuickTime sample each).
    pub fn encode_frames(&mut self, planar: &[&[i32]]) -> Result<Vec<Vec<u8>>, Error> {
        if planar.len() != self.pending.len() {
            return Err(Error::Channels { expected: self.pending.len(), got: planar.len() });
        }
        let n = planar.first().map_or(0, |c| c.len());
        if planar.iter().any(|c| c.len() != n) {
            return Err(Error::UnequalChannels);
        }
        let max = (1i32 << (self.cfg.bits_per_sample - 1)) - 1;
        if let Some(&bad) = planar.iter().flat_map(|c| c.iter()).find(|&&s| s > max || s < -max - 1) {
            return Err(Error::SampleRange(bad));
        }
        for (p, c) in self.pending.iter_mut().zip(planar) {
            p.extend_from_slice(c);
        }
        let bs = usize::from(self.cfg.block_size);
        let mut out = Vec::new();
        while self.pending.first().is_some_and(|p| p.len() >= bs) {
            let block: Vec<Vec<i32>> = self.pending.iter_mut().map(|p| p.drain(..bs).collect()).collect();
            out.push(self.frame(&block));
        }
        Ok(out)
    }

    /// Samples per channel waiting for a full block: the length of the frame [`Encoder::finish`]
    /// would write.
    pub fn pending_samples(&self) -> usize {
        self.pending.first().map_or(0, Vec::len)
    }

    /// Encode what is left as a final, shorter frame.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.pending.first().is_none_or(Vec::is_empty) {
            return Vec::new();
        }
        let block: Vec<Vec<i32>> = self.pending.iter_mut().map(std::mem::take).collect();
        self.frame(&block)
    }

    fn frame(&mut self, block: &[Vec<i32>]) -> Vec<u8> {
        let n = block.first().map_or(0, Vec::len);
        let bps = self.cfg.bits_per_sample;
        // the MD5 of the unencoded audio: interleaved, little-endian, whole bytes per sample
        let bytes = usize::from(bps.div_ceil(8));
        let mut raw = Vec::with_capacity(n * block.len() * bytes);
        for i in 0..n {
            for c in block {
                raw.extend_from_slice(c.get(i).copied().unwrap_or(0).to_le_bytes().get(..bytes).unwrap_or_default());
            }
        }
        self.md5.update(&raw);

        let effort = self.effort;
        let code = |x: &[i32], b: u8| predict::choose(x, b, effort);
        let data = match block {
            [l, r] if effort.stereo => {
                let side: Vec<i32> = l.iter().zip(r).map(|(&a, &b)| a - b).collect();
                let mid: Vec<i32> = l.iter().zip(r).map(|(&a, &b)| (a + b) >> 1).collect();
                let (cl, cr, cs, cm) = (code(l, bps), code(r, bps), code(&side, bps + 1), code(&mid, bps));
                let options =
                    [(Stereo::Independent, cl.1 + cr.1), (Stereo::LeftSide, cl.1 + cs.1), (Stereo::SideRight, cs.1 + cr.1), (Stereo::MidSide, cm.1 + cs.1)];
                let best = options.iter().min_by_key(|o| o.1).map_or(Stereo::Independent, |o| o.0);
                let channels: Vec<(&[i32], Subframe, u8)> = match best {
                    Stereo::Independent => vec![(l, cl.0, bps), (r, cr.0, bps)],
                    Stereo::LeftSide => vec![(l, cl.0, bps), (&side, cs.0, bps + 1)],
                    Stereo::SideRight => vec![(&side, cs.0, bps + 1), (r, cr.0, bps)],
                    Stereo::MidSide => vec![(&mid, cm.0, bps), (&side, cs.0, bps + 1)],
                };
                frame::write(&FrameSpec { number: self.frame_number, sample_rate: self.cfg.sample_rate, bps, stereo: Some(best), channels: &channels })
            }
            _ => {
                let channels: Vec<(&[i32], Subframe, u8)> = block.iter().map(|c| (c.as_slice(), code(c, bps).0, bps)).collect();
                frame::write(&FrameSpec { number: self.frame_number, sample_rate: self.cfg.sample_rate, bps, stereo: None, channels: &channels })
            }
        };
        self.frame_number += 1;
        self.total_samples += n as u64;
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        self.min_frame = if self.min_frame == 0 { size } else { self.min_frame.min(size) };
        self.max_frame = self.max_frame.max(size);
        data
    }
}
