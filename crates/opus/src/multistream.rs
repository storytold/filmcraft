//! Public decoder: OpusHead-configured (multistream) decoding to planar f32.

use crate::decoder::StreamDecoder;
use crate::header::OpusHead;
use crate::packet::Packet;
use crate::{Error, Result};

/// Opus decoder for an `OpusHead` configuration (mono, stereo, or multistream surround).
///
/// Produces planar f32 at the chosen output rate. Pre-skip is *not* removed unless
/// [`Decoder::set_trim_pre_skip`] is enabled (containers usually handle it via edit lists /
/// granule positions). The header's output gain is applied.
pub struct Decoder {
    head: OpusHead,
    rate: u32,
    streams: Vec<StreamDecoder>,
    trim_pre_skip: bool,
    skip_remaining: usize,
    scratch: Vec<Vec<f32>>,
}

impl Decoder {
    /// Creates a 48 kHz decoder from an `OpusHead` packet (Ogg / Matroska `CodecPrivate`).
    pub fn new(opus_head: &[u8]) -> Result<Decoder> {
        Self::from_head(OpusHead::parse(opus_head)?, 48000)
    }

    /// Creates a decoder for plain mono/stereo packets without a header.
    pub fn simple(sample_rate: u32, channels: usize) -> Result<Decoder> {
        Self::from_head(OpusHead::simple(channels)?, sample_rate)
    }

    /// Creates a decoder for a parsed header at `sample_rate` (8/12/16/24/48 kHz).
    pub fn from_head(head: OpusHead, sample_rate: u32) -> Result<Decoder> {
        head.validate()?;
        let mut streams = Vec::with_capacity(head.streams);
        for s in 0..head.streams {
            let ch = if s < head.coupled_streams { 2 } else { 1 };
            let mut d = StreamDecoder::new(sample_rate, ch)?;
            d.set_gain_q8(head.output_gain);
            streams.push(d);
        }
        Ok(Decoder { rate: sample_rate, streams, trim_pre_skip: false, skip_remaining: 0, scratch: vec![Vec::new(); head.streams], head })
    }

    pub fn head(&self) -> &OpusHead {
        &self.head
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.head.channels
    }

    /// Pre-skip in samples at the output rate.
    pub fn pre_skip(&self) -> usize {
        self.head.pre_skip as usize * self.rate as usize / 48000
    }

    /// When enabled, the first [`Self::pre_skip`] output samples are dropped.
    pub fn set_trim_pre_skip(&mut self, enable: bool) {
        self.trim_pre_skip = enable;
        self.skip_remaining = if enable { self.pre_skip() } else { 0 };
    }

    /// Disables intensity-stereo phase inversion (useful when downmixing; RFC 8251 section 10).
    pub fn set_phase_inversion_disabled(&mut self, disabled: bool) {
        for s in &mut self.streams {
            s.set_phase_inversion_disabled(disabled);
        }
    }

    /// Final range coder state of the first stream (conformance testing).
    pub fn final_range(&self) -> u32 {
        self.streams.first().map_or(0, |s| s.final_range())
    }

    pub fn reset(&mut self) {
        for s in &mut self.streams {
            s.reset();
        }
        self.skip_remaining = if self.trim_pre_skip { self.pre_skip() } else { 0 };
    }

    /// Decodes one packet (`None` = lost packet, concealed) into planar channels.
    pub fn decode(&mut self, packet: Option<&[u8]>) -> Result<Vec<Vec<f32>>> {
        let n = self.streams.len();
        for b in &mut self.scratch {
            b.clear();
        }
        let mut samples = None;
        match packet {
            Some(data) if !data.is_empty() => {
                let mut off = 0;
                for s in 0..n {
                    let rest = &data[off..];
                    if rest.is_empty() {
                        return Err(Error::InvalidPacket("truncated multistream packet"));
                    }
                    let used = if s + 1 < n { Packet::parse_self_delimited(rest)?.1 } else { rest.len() };
                    let pkt = &rest[..used];
                    let r = if s + 1 < n {
                        // Re-frame the self-delimited packet as a regular one for the stream decoder.
                        let (p, _) = Packet::parse_self_delimited(pkt)?;
                        let regular = reframe(&p);
                        self.streams[s].decode_interleaved(Some(&regular), &mut self.scratch[s])?
                    } else {
                        self.streams[s].decode_interleaved(Some(pkt), &mut self.scratch[s])?
                    };
                    if *samples.get_or_insert(r) != r {
                        return Err(Error::InvalidPacket("multistream frame durations differ"));
                    }
                    off += used;
                }
            }
            _ => {
                let dur = match self.streams[0].last_packet_duration() {
                    0 => self.rate as usize / 50,
                    d => d,
                };
                for s in 0..n {
                    self.streams[s].conceal(dur, &mut self.scratch[s])?;
                }
                samples = Some(dur);
            }
        }
        let samples = samples.unwrap_or(0);
        let mut out = vec![vec![0f32; samples]; self.head.channels];
        for (c, o) in out.iter_mut().enumerate() {
            let m = self.head.mapping[c] as usize;
            if m == 255 {
                continue;
            }
            let (s, ch, nch) = if m < 2 * self.head.coupled_streams { (m / 2, m % 2, 2) } else { (m - self.head.coupled_streams, 0, 1) };
            let src = &self.scratch[s];
            for (i, v) in o.iter_mut().enumerate() {
                *v = src[i * nch + ch];
            }
        }
        if self.skip_remaining > 0 {
            let k = self.skip_remaining.min(samples);
            for o in &mut out {
                o.drain(..k);
            }
            self.skip_remaining -= k;
        }
        Ok(out)
    }
}

/// Rebuilds a regular (code 3 VBR) packet from a parsed self-delimited one.
fn reframe(p: &Packet) -> Vec<u8> {
    let mut v = vec![(p.toc.config << 3) | if p.toc.stereo { 4 } else { 0 } | 3];
    v.push(0x80 | p.frames.len() as u8);
    for f in &p.frames[..p.frames.len() - 1] {
        let s = f.len();
        if s < 252 {
            v.push(s as u8);
        } else {
            let b0 = 252 + (s & 3);
            v.push(b0 as u8);
            v.push(((s - b0) >> 2) as u8);
        }
    }
    for f in &p.frames {
        v.extend_from_slice(f);
    }
    v
}
