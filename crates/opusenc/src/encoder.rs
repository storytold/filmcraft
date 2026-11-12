//! Opus encoder (RFC 6716): CELT-only, fullband, 20 ms frames, constant bit rate, mono or stereo
//! at 48 kHz.
//!
//! Each packet is a TOC byte (configuration 31, CELT fullband 20 ms; one frame, code 0) and one
//! CELT frame of a fixed size, so the bit rate is exact. The decoder's output is the input delayed
//! by [`PRE_SKIP`] samples, which `OpusHead` / `dOps` tell players to drop; [`Encoder::finish`]
//! pads with silence so that the last input sample is still decoded.

use crate::celt::encode::{CeltEncoder, FRAME};
use crate::header::OpusHead;
use crate::{Error, Result};

/// Samples per channel in one packet (20 ms at 48 kHz).
pub const FRAME_SIZE: usize = FRAME;
/// The encoder's delay in samples at 48 kHz (the CELT window overlap): the `OpusHead` pre-skip.
pub const PRE_SKIP: u16 = 120;
/// Lowest and highest bit rates (bit/s), from the smallest (3-byte) and largest (1276-byte)
/// 20 ms packet.
pub const MIN_BITRATE: u32 = 1200;
pub const MAX_BITRATE: u32 = 510_400;

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    /// 1 or 2.
    pub channels: usize,
    /// Bit rate in bit/s; rounded to whole bytes per 20 ms packet and clamped to
    /// [`MIN_BITRATE`] ..= [`MAX_BITRATE`].
    pub bitrate: u32,
}

impl EncoderConfig {
    pub fn new(channels: usize, bitrate: u32) -> EncoderConfig {
        EncoderConfig { channels, bitrate }
    }
}

/// A CELT-only Opus encoder for one mono or stereo stream.
pub struct Encoder {
    celt: CeltEncoder,
    channels: usize,
    packet_bytes: usize,
    intensity: usize,
    spread: usize,
    /// Input not yet encoded, one buffer per channel (shorter than a frame between calls).
    pending: Vec<Vec<f32>>,
    /// Input samples per channel taken so far.
    samples_in: u64,
    /// Packets produced so far.
    packets: u64,
    finished: bool,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if !(1..=2).contains(&cfg.channels) {
            return Err(Error::InvalidArgument("Opus encoder: 1 or 2 channels"));
        }
        let bitrate = cfg.bitrate.clamp(MIN_BITRATE, MAX_BITRATE);
        // bytes per 20 ms packet = bitrate * 0.02 / 8
        let packet_bytes = ((u64::from(bitrate) + 200) / 400).clamp(3, 1276) as usize;
        Ok(Encoder {
            celt: CeltEncoder::new(cfg.channels)?,
            channels: cfg.channels,
            packet_bytes,
            intensity: intensity_band(cfg.channels, bitrate),
            spread: spreading(cfg.channels, bitrate),
            pending: vec![Vec::with_capacity(2 * FRAME); cfg.channels],
            samples_in: 0,
            packets: 0,
            finished: false,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Bytes in every packet (TOC included).
    pub fn packet_bytes(&self) -> usize {
        self.packet_bytes
    }

    /// The exact bit rate in bit/s.
    pub fn bitrate(&self) -> u32 {
        (self.packet_bytes as u32) * 400
    }

    /// Input samples per channel taken so far.
    pub fn samples_in(&self) -> u64 {
        self.samples_in
    }

    /// The `OpusHead` for this stream (mapping family 0).
    pub fn head(&self) -> OpusHead {
        OpusHead {
            channels: self.channels,
            pre_skip: PRE_SKIP,
            input_sample_rate: 48_000,
            output_gain: 0,
            mapping_family: 0,
            streams: 1,
            coupled_streams: self.channels - 1,
            mapping: (0..self.channels as u8).collect(),
        }
    }

    /// Encodes planar 48 kHz samples (one slice per channel, all the same length, nominal range
    /// ±1); returns the packets completed, each of [`FRAME_SIZE`] samples. NaN or infinite
    /// samples, or a channel count or length mismatch, are an error.
    pub fn encode(&mut self, planar: &[&[f32]]) -> Result<Vec<Vec<u8>>> {
        if self.finished {
            return Err(Error::InvalidArgument("Opus encoder: encode after finish"));
        }
        if planar.len() != self.channels {
            return Err(Error::InvalidArgument("Opus encoder: one slice per channel"));
        }
        let len = planar.first().map_or(0, |p| p.len());
        if planar.iter().any(|p| p.len() != len) {
            return Err(Error::InvalidArgument("Opus encoder: channels of different lengths"));
        }
        if planar.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(Error::InvalidArgument("Opus encoder: NaN or infinite sample"));
        }
        for (buf, p) in self.pending.iter_mut().zip(planar) {
            buf.extend_from_slice(p);
        }
        self.samples_in += len as u64;
        self.drain(false)
    }

    /// Encodes what remains, padded with silence so that the decoder (after dropping the
    /// pre-skip) returns every input sample; returns the last packets.
    pub fn finish(&mut self) -> Result<Vec<Vec<u8>>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let needed = (self.samples_in + u64::from(PRE_SKIP)).div_ceil(FRAME as u64);
        let missing = needed.saturating_sub(self.packets) as usize * FRAME;
        let have = self.pending.first().map_or(0, Vec::len);
        for buf in self.pending.iter_mut() {
            buf.resize(missing.max(have), 0.0);
        }
        self.drain(true)
    }

    /// Packets that `finish` will have produced in total for the input so far (the stream's
    /// length is `packets × FRAME_SIZE − PRE_SKIP` samples, trimmed to [`Self::samples_in`]).
    pub fn packets(&self) -> u64 {
        self.packets
    }

    fn drain(&mut self, all: bool) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let have = self.pending.first().map_or(0, Vec::len);
        let frames = if all { have.div_ceil(FRAME) } else { have / FRAME };
        if all {
            for buf in self.pending.iter_mut() {
                buf.resize(frames * FRAME, 0.0);
            }
        }
        for f in 0..frames {
            let at = f * FRAME;
            let slices: Vec<&[f32]> = self.pending.iter().map(|b| &b[at..at + FRAME]).collect();
            let frame = self.celt.encode_frame(&slices, self.packet_bytes - 1, self.intensity, self.spread)?;
            let mut packet = Vec::with_capacity(self.packet_bytes);
            packet.push(0xF8 | (((self.channels == 2) as u8) << 2));
            packet.extend_from_slice(&frame);
            out.push(packet);
            self.packets += 1;
        }
        for buf in self.pending.iter_mut() {
            buf.drain(..frames * FRAME);
        }
        Ok(out)
    }
}

/// First band coded with intensity stereo: none at high rates, more of the top bands as the
/// stereo bit rate falls.
fn intensity_band(channels: usize, bitrate: u32) -> usize {
    use crate::celt::tables::NB_EBANDS;
    if channels < 2 {
        return NB_EBANDS;
    }
    match bitrate {
        0..40_000 => 12,
        40_000..64_000 => 16,
        64_000..96_000 => 19,
        _ => NB_EBANDS,
    }
}

/// Spreading: aggressive at low rates, where bands get few pulses and the rotation keeps their
/// energy from collapsing into a few spectral lines; normal otherwise.
fn spreading(channels: usize, bitrate: u32) -> usize {
    if bitrate / (channels.max(1) as u32) < 32_000 { 3 } else { 2 }
}
