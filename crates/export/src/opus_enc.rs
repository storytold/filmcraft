//! Opus for the exports: craft-audio's encoder (`craft_audio::opus::encoder`) in CELT mode,
//! fullband, 20 ms packets at a constant bit rate, mono or stereo at 48 kHz.
//!
//! The shared encoder codes one interleaved frame at a time; [`OpusStream`] takes planar chunks
//! of any length, keeps the part of a frame not yet coded, and at the end pads with silence so
//! that a decoder, after dropping the pre-skip, returns every input sample.

use craft_audio::opus::encoder::{Bandwidth, Encoder, Mode};

use crate::{ExportError, Result};

/// Samples per channel in one packet (20 ms at 48 kHz).
pub(crate) const FRAME_SIZE: usize = Encoder::FRAME_SIZE;

fn encode_error(e: impl std::fmt::Display) -> ExportError {
    ExportError::Encode(format!("Opus: {e}"))
}

/// One Opus stream being encoded from planar chunks.
pub(crate) struct OpusStream {
    enc: Encoder,
    channels: usize,
    /// Interleaved input not yet coded (less than a frame between calls).
    pending: Vec<f32>,
    /// Input samples per channel taken so far.
    samples_in: u64,
    /// Packets produced so far.
    packets: u64,
    finished: bool,
}

impl OpusStream {
    /// An encoder for `channels` (1 or 2) at `bitrate` bit/s (total for all channels).
    pub(crate) fn new(channels: usize, bitrate: u32) -> Result<Self> {
        let ch = u8::try_from(channels).unwrap_or(u8::MAX);
        let enc = Encoder::with_mode(ch, bitrate, Mode::Celt, Bandwidth::Fullband).map_err(|e| ExportError::Unsupported(format!("Opus: {e}")))?;
        Ok(OpusStream { enc, channels, pending: Vec::with_capacity(2 * FRAME_SIZE * channels), samples_in: 0, packets: 0, finished: false })
    }

    /// Samples at 48 kHz a decoder drops from the start (the `OpusHead` pre-skip).
    pub(crate) fn pre_skip(&self) -> u16 {
        self.enc.pre_skip()
    }

    /// 1 or 2.
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// The 19-byte `OpusHead` packet that starts an Ogg Opus stream.
    pub(crate) fn head_packet(&self) -> Vec<u8> {
        self.enc.opus_head()
    }

    /// Input samples per channel taken so far.
    pub(crate) fn samples_in(&self) -> u64 {
        self.samples_in
    }

    /// Packets produced so far.
    pub(crate) fn packets(&self) -> u64 {
        self.packets
    }

    /// Takes planar 48 kHz samples (one slice per channel, all the same length); returns the
    /// packets completed. NaN or infinite samples, or a channel count or length mismatch, are an
    /// error.
    pub(crate) fn encode(&mut self, planar: &[&[f32]]) -> Result<Vec<Vec<u8>>> {
        if self.finished {
            return Err(encode_error("encode after finish"));
        }
        if planar.len() != self.channels {
            return Err(encode_error("one slice per channel"));
        }
        let len = planar.first().map_or(0, |p| p.len());
        if planar.iter().any(|p| p.len() != len) {
            return Err(encode_error("channels of different lengths"));
        }
        if planar.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(encode_error("NaN or infinite sample"));
        }
        self.pending.reserve(len * self.channels);
        for i in 0..len {
            self.pending.extend(planar.iter().filter_map(|p| p.get(i)));
        }
        self.samples_in += len as u64;
        self.drain()
    }

    /// Codes what remains, padded with silence so that the decoder (after dropping the
    /// pre-skip) returns every input sample; returns the last packets.
    pub(crate) fn finish(&mut self) -> Result<Vec<Vec<u8>>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let needed = (self.samples_in + u64::from(self.pre_skip())).div_ceil(FRAME_SIZE as u64);
        let missing = usize::try_from(needed.saturating_sub(self.packets)).unwrap_or(usize::MAX);
        let len = missing.saturating_mul(FRAME_SIZE * self.channels).max(self.pending.len());
        self.pending.resize(len, 0.0);
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<Vec<u8>>> {
        let step = FRAME_SIZE * self.channels;
        let mut out = Vec::with_capacity(self.pending.len() / step);
        for frame in self.pending.chunks_exact(step) {
            out.push(self.enc.encode_float(frame).map_err(encode_error)?);
        }
        self.packets += out.len() as u64;
        self.pending.drain(..out.len() * step);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn every_input_length_comes_back_after_the_pre_skip() {
        for channels in [1usize, 2] {
            for n in [0usize, 1, 959, 960, 961, 1_080, 48_000] {
                let mut s = OpusStream::new(channels, 128_000).unwrap();
                let x = vec![0.25f32; n];
                let planar: Vec<&[f32]> = (0..channels).map(|_| x.as_slice()).collect();
                let mut packets = s.encode(&planar).unwrap();
                packets.extend(s.finish().unwrap());
                let pre = u64::from(s.pre_skip());
                assert_eq!(packets.len() as u64, s.packets());
                assert_eq!(s.packets(), (n as u64 + pre).div_ceil(FRAME_SIZE as u64), "{channels} ch, {n} samples");
                assert!(s.finish().unwrap().is_empty());
                assert!(s.encode(&planar).is_err());
            }
        }
    }

    #[test]
    fn chunking_does_not_change_the_packets() {
        let x: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let whole = {
            let mut s = OpusStream::new(2, 160_000).unwrap();
            let mut p = s.encode(&[&x, &x]).unwrap();
            p.extend(s.finish().unwrap());
            p
        };
        let mut s = OpusStream::new(2, 160_000).unwrap();
        let mut parts = Vec::new();
        for c in x.chunks(777) {
            parts.extend(s.encode(&[c, c]).unwrap());
        }
        parts.extend(s.finish().unwrap());
        assert_eq!(parts, whole);
        assert!(whole.iter().all(|p| p.len() == 400 && p[0] >> 3 == 31), "CELT fullband, 400-byte packets at 160 kbps");
    }

    #[test]
    fn hostile_input_is_an_error() {
        assert!(OpusStream::new(0, 64_000).is_err());
        assert!(OpusStream::new(3, 64_000).is_err());
        assert!(OpusStream::new(300, 64_000).is_err());
        let mut s = OpusStream::new(2, 64_000).unwrap();
        assert!(s.encode(&[&[0.0; 4]]).is_err());
        assert!(s.encode(&[&[0.0; 4], &[0.0; 3]]).is_err());
        assert!(s.encode(&[&[f32::NAN], &[0.0]]).is_err());
        assert!(s.encode(&[&[f32::INFINITY], &[0.0]]).is_err());
        assert_eq!(s.samples_in(), 0);
    }

    #[test]
    fn head_matches_the_packet() {
        let s = OpusStream::new(2, 96_000).unwrap();
        let head = craft_audio::opus::OpusHead::parse(&s.head_packet()).unwrap();
        assert_eq!((head.channels, head.pre_skip, head.input_sample_rate, head.mapping_family), (s.channels(), s.pre_skip(), 48_000, 0));
        assert_eq!(s.pre_skip(), 120);
    }
}
