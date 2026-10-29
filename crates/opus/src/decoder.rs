//! Single-stream Opus decoder: mode switching, hybrid summation, redundancy and transitions
//! (RFC 6716 §4, §4.5).

use crate::celt::CeltDecoder;
use crate::celt::tables::OVERLAP;
use crate::packet::{Bandwidth, Mode, Packet};
use crate::range::RangeDecoder;
use crate::silk::{LostFlag, SilkDecoder};
use crate::{Error, Result};

/// Output sample rates supported by the decoder.
pub const SAMPLE_RATES: [u32; 5] = [8000, 12000, 16000, 24000, 48000];

/// Decoder for one elementary Opus stream (mono or stereo), like `OpusDecoder`.
pub struct StreamDecoder {
    rate: u32,
    channels: usize,
    celt: CeltDecoder,
    silk: SilkDecoder,
    stream_channels: usize,
    bandwidth: Bandwidth,
    mode: Option<Mode>,
    prev_mode: Option<Mode>,
    frame_size: usize,
    prev_redundancy: bool,
    last_packet_duration: usize,
    range_final: u32,
    /// Output gain in Q8 dB (OpusHead output gain).
    gain_q8: i16,
}

struct SilkFrame {
    mode: Mode,
    bandwidth: Option<Bandwidth>,
    has_packet: bool,
    fec: bool,
    frame_size: usize,
    audio_size: usize,
    rate: usize,
    channels: usize,
}

struct Redundancy {
    present: bool,
    celt_to_silk: bool,
    bytes: usize,
    packet_len: usize,
}

fn apply_output_gain(pcm: &mut [f32], samples: usize, channels: usize, gain_q8: i16) {
    if gain_q8 == 0 {
        return;
    }
    let gain = (6.488_141e-4f32 * gain_q8 as f32).exp2();
    for sample in pcm.iter_mut().take(samples * channels) {
        *sample *= gain;
    }
}

impl StreamDecoder {
    /// Creates a decoder producing `channels` (1 or 2) at `rate` (8/12/16/24/48 kHz).
    pub fn new(rate: u32, channels: usize) -> Result<StreamDecoder> {
        if !SAMPLE_RATES.contains(&rate) {
            return Err(Error::InvalidArgument("sample rate must be 8000, 12000, 16000, 24000 or 48000"));
        }
        if !(1..=2).contains(&channels) {
            return Err(Error::InvalidArgument("channels must be 1 or 2"));
        }
        Ok(StreamDecoder {
            rate,
            channels,
            celt: CeltDecoder::new(rate, channels),
            silk: SilkDecoder::new(rate, channels),
            stream_channels: channels,
            bandwidth: Bandwidth::Fullband,
            mode: None,
            prev_mode: None,
            frame_size: rate as usize / 400,
            prev_redundancy: false,
            last_packet_duration: 0,
            range_final: 0,
            gain_q8: 0,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Final range coder state of the last decoded frame (for conformance checks).
    pub fn final_range(&self) -> u32 {
        self.range_final
    }

    /// Duration (samples per channel) of the last decoded packet.
    pub fn last_packet_duration(&self) -> usize {
        self.last_packet_duration
    }

    /// Sets the output gain in Q8 dB (as in the OpusHead header).
    pub fn set_gain_q8(&mut self, gain: i16) {
        self.gain_q8 = gain;
    }

    /// Disables the 180-degree intensity-stereo phase inversion (RFC 8251 §10).
    pub fn set_phase_inversion_disabled(&mut self, disabled: bool) {
        self.celt.disable_inv = disabled;
    }

    /// `OPUS_RESET_STATE`.
    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.stream_channels = self.channels;
        self.bandwidth = Bandwidth::Fullband;
        self.mode = None;
        self.prev_mode = None;
        self.frame_size = self.rate as usize / 400;
        self.prev_redundancy = false;
        self.last_packet_duration = 0;
        self.range_final = 0;
    }

    /// Decodes a packet into interleaved samples appended to `out`. `None` conceals one lost
    /// packet of the last packet's duration. Returns samples per channel.
    pub fn decode_interleaved(&mut self, packet: Option<&[u8]>, out: &mut Vec<f32>) -> Result<usize> {
        match packet {
            Some(p) if !p.is_empty() => self.decode_packet(p, out),
            _ => {
                let n = if self.last_packet_duration > 0 { self.last_packet_duration } else { self.rate as usize / 50 };
                self.conceal(n, out)
            }
        }
    }

    /// Conceals `n` samples (a multiple of 2.5 ms).
    pub fn conceal(&mut self, n: usize, out: &mut Vec<f32>) -> Result<usize> {
        let f2_5 = self.rate as usize / 400;
        if n == 0 || !n.is_multiple_of(f2_5) {
            return Err(Error::InvalidArgument("concealment length must be a multiple of 2.5 ms"));
        }
        let start = out.len();
        out.resize(start + n * self.channels, 0.0);
        let mut done = 0;
        while done < n {
            let r = self.decode_frame(None, &mut out[start + done * self.channels..], n - done, false)?;
            done += r;
        }
        self.last_packet_duration = n;
        Ok(n)
    }

    /// Decodes the in-band FEC (LBRR) of `packet` for the previous (lost) packet of `n` samples.
    pub fn decode_fec(&mut self, packet: &[u8], n: usize, out: &mut Vec<f32>) -> Result<usize> {
        let p = Packet::parse(packet)?;
        let pfs = p.toc.frame_samples(self.rate);
        if n < pfs || p.toc.mode == Mode::CeltOnly || self.mode == Some(Mode::CeltOnly) {
            return self.conceal(n, out);
        }
        let start = out.len();
        if n > pfs {
            self.conceal(n - pfs, out)?;
        }
        self.mode = Some(p.toc.mode);
        self.bandwidth = p.toc.bandwidth;
        self.frame_size = pfs;
        self.stream_channels = p.toc.channels();
        let off = out.len();
        out.resize(off + pfs * self.channels, 0.0);
        self.decode_frame(Some(p.frames[0]), &mut out[off..], pfs, true)?;
        self.last_packet_duration = n;
        debug_assert_eq!(out.len() - start, n * self.channels);
        Ok(n)
    }

    fn decode_packet(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<usize> {
        let p = Packet::parse(data)?;
        let fs = p.toc.frame_samples(self.rate);
        self.mode = Some(p.toc.mode);
        self.bandwidth = p.toc.bandwidth;
        self.frame_size = fs;
        self.stream_channels = p.toc.channels();
        let start = out.len();
        out.resize(start + fs * p.frames.len() * self.channels, 0.0);
        let mut nb = 0;
        for f in &p.frames {
            let r = self.decode_frame(Some(f), &mut out[start + nb * self.channels..], fs, false)?;
            nb += r;
        }
        out.truncate(start + nb * self.channels);
        self.last_packet_duration = nb;
        Ok(nb)
    }

    fn decode_silk_frame(&mut self, frame: SilkFrame, dec: &mut Option<&mut RangeDecoder>) -> Result<Vec<f32>> {
        let SilkFrame { mode, bandwidth, has_packet, fec, frame_size, audio_size, rate, channels } = frame;
        let mut pcm = Vec::new();
        if mode == Mode::CeltOnly {
            return Ok(pcm);
        }
        pcm.resize(frame_size.max(rate / 100) * channels, 0.0);
        if self.prev_mode == Some(Mode::CeltOnly) {
            self.silk.reset();
        }
        let internal_rate = if mode == Mode::SilkOnly {
            match bandwidth {
                Some(Bandwidth::Narrowband) => 8000,
                Some(Bandwidth::Mediumband) => 12000,
                _ => 16000,
            }
        } else {
            16000
        };
        if has_packet {
            self.silk.set_stream(self.stream_channels, internal_rate);
        }
        let lost = if !has_packet {
            LostFlag::Lost
        } else if fec {
            LostFlag::Lbrr
        } else {
            LostFlag::Normal
        };
        let payload_ms = (1000 * audio_size / rate).max(10);
        let mut decoded = 0;
        while decoded < frame_size {
            let first = decoded == 0;
            let count = match self.silk.decode(dec.as_deref_mut(), lost, first, payload_ms, &mut pcm[decoded * channels..]) {
                Ok(count) => count,
                Err(_error) if lost != LostFlag::Normal => {
                    let remaining = frame_size - decoded;
                    pcm[decoded * channels..(decoded + remaining) * channels].fill(0.0);
                    remaining
                }
                Err(error) => return Err(error),
            };
            if count == 0 {
                break;
            }
            decoded += count;
        }
        Ok(pcm)
    }

    fn read_redundancy(&self, mode: Mode, fec: bool, packet_len: usize, dec: &mut Option<&mut RangeDecoder>) -> Redundancy {
        let mut state = Redundancy { present: false, celt_to_silk: false, bytes: 0, packet_len };
        let Some(decoder) = dec.as_deref_mut() else { return state };
        let hybrid = self.mode == Some(Mode::Hybrid);
        if fec || mode == Mode::CeltOnly || decoder.tell() + 17 + 20 * i32::from(hybrid) > 8 * state.packet_len as i32 {
            return state;
        }
        state.present = if mode == Mode::Hybrid { decoder.bit_logp(12) } else { true };
        if !state.present {
            return state;
        }
        state.celt_to_silk = decoder.bit_logp(1);
        state.bytes = if mode == Mode::Hybrid { decoder.uint(256) as usize + 2 } else { state.packet_len - ((decoder.tell() as usize + 7) >> 3) };
        if state.bytes > state.packet_len || (state.packet_len - state.bytes) * 8 < decoder.tell() as usize {
            state.packet_len = 0;
            state.bytes = 0;
            state.present = false;
        } else {
            state.packet_len -= state.bytes;
        }
        decoder.shrink_storage(state.bytes);
        state
    }

    fn is_mode_transition(&self, has_packet: bool, mode: Mode) -> bool {
        has_packet
            && self.prev_mode.is_some()
            && ((mode == Mode::CeltOnly && self.prev_mode != Some(Mode::CeltOnly) && !self.prev_redundancy)
                || (mode != Mode::CeltOnly && self.prev_mode == Some(Mode::CeltOnly)))
    }

    /// `opus_decode_frame`: decodes one frame (or conceals when `data` is `None`/≤1 byte).
    fn decode_frame(&mut self, data: Option<&[u8]>, pcm: &mut [f32], frame_size: usize, fec: bool) -> Result<usize> {
        let ch = self.channels;
        let fs = self.rate as usize;
        let f20 = fs / 50;
        let f10 = f20 >> 1;
        let f5 = f10 >> 1;
        let f2_5 = f5 >> 1;
        let mut frame_size = frame_size.min(fs / 25 * 3);
        let data = match data {
            Some(d) if d.len() > 1 => Some(d),
            _ => {
                frame_size = frame_size.min(self.frame_size);
                None
            }
        };
        let (mut audiosize, mode, bandwidth);
        match data {
            Some(_) => {
                audiosize = self.frame_size;
                mode = self.mode.unwrap_or(Mode::CeltOnly);
                bandwidth = Some(self.bandwidth);
            }
            None => {
                audiosize = frame_size;
                bandwidth = None;
                mode = match self.prev_mode {
                    None => {
                        pcm[..audiosize * ch].fill(0.0);
                        return Ok(audiosize);
                    }
                    Some(m) => m,
                };
                if audiosize > f20 {
                    let mut done = 0;
                    while done < audiosize {
                        let r = self.decode_frame(None, &mut pcm[done * ch..], (audiosize - done).min(f20), false)?;
                        done += r;
                    }
                    return Ok(frame_size);
                } else if audiosize < f20 {
                    if audiosize > f10 {
                        audiosize = f10;
                    } else if mode != Mode::SilkOnly && audiosize > f5 && audiosize < f10 {
                        audiosize = f5;
                    }
                }
            }
        }
        let data_len = data.map_or(0, |d| d.len());
        let mut dec_storage;
        let mut dec: Option<&mut RangeDecoder> = match data {
            Some(d) => {
                dec_storage = RangeDecoder::new(d);
                Some(&mut dec_storage)
            }
            None => None,
        };
        let mut transition = self.is_mode_transition(data.is_some(), mode);
        let mut pcm_transition = Vec::new();
        if transition && mode == Mode::CeltOnly {
            pcm_transition.resize(f5 * ch, 0.0);
            let n = f5.min(audiosize);
            self.decode_frame(None, &mut pcm_transition, n, false)?;
        }
        if audiosize > frame_size {
            return Err(Error::InvalidArgument("frame larger than output buffer"));
        }
        frame_size = audiosize;

        // SILK.
        let pcm_silk = self.decode_silk_frame(
            SilkFrame { mode, bandwidth, has_packet: data.is_some(), fec, frame_size, audio_size: audiosize, rate: fs, channels: ch },
            &mut dec,
        )?;

        let Redundancy { present: redundancy, celt_to_silk, bytes: redundancy_bytes, packet_len: len } = self.read_redundancy(mode, fec, data_len, &mut dec);
        let mut start_band = 0;
        if mode != Mode::CeltOnly {
            start_band = 17;
        }
        if redundancy {
            transition = false;
        }
        if transition && mode != Mode::CeltOnly {
            pcm_transition.resize(f5 * ch, 0.0);
            let n = f5.min(audiosize);
            self.decode_frame(None, &mut pcm_transition, n, false)?;
        }
        if let Some(bw) = bandwidth {
            self.celt.end = bw.celt_end_band();
        }
        self.celt.stream_channels = self.stream_channels;
        let mut redundant_audio = vec![0f32; if redundancy { f5 * ch } else { 0 }];
        let mut redundant_rng = 0u32;
        let red_data = data.map(|d| &d[len..len + redundancy_bytes]);
        if redundancy && celt_to_silk {
            self.celt.start = 0;
            self.celt.decode(red_data, f5, &mut redundant_audio, false, None)?;
            redundant_rng = self.celt.rng;
        }
        self.celt.start = start_band;

        if mode != Mode::SilkOnly {
            let celt_frame = f20.min(frame_size);
            if self.prev_mode.is_some() && Some(mode) != self.prev_mode && !self.prev_redundancy {
                self.celt.reset();
            }
            let cdata = if fec { None } else { data.map(|d| &d[..len]) };
            let cdec = if fec { None } else { dec.as_deref_mut() };
            self.celt.decode(cdata, celt_frame, pcm, false, cdec)?;
        } else {
            pcm[..frame_size * ch].fill(0.0);
            if self.prev_mode == Some(Mode::Hybrid) && !(redundancy && celt_to_silk && self.prev_redundancy) {
                self.celt.start = 0;
                let silence = [0xFFu8, 0xFF];
                self.celt.decode(Some(&silence), f2_5, pcm, true, None)?;
            }
        }
        if mode != Mode::CeltOnly {
            for (o, s) in pcm[..frame_size * ch].iter_mut().zip(&pcm_silk) {
                *o += *s;
            }
        }
        let window = &crate::celt::mode().window;
        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt.start = 0;
            self.celt.decode(red_data, f5, &mut redundant_audio, false, None)?;
            redundant_rng = self.celt.rng;
            let o = ch * (frame_size - f2_5);
            let tail: Vec<f32> = pcm[o..o + f2_5 * ch].to_vec();
            smooth_fade(&tail, &redundant_audio[ch * f2_5..], &mut pcm[o..], f2_5, ch, window, self.rate);
        }
        if redundancy && celt_to_silk {
            pcm[..f2_5 * ch].copy_from_slice(&redundant_audio[..f2_5 * ch]);
            let cur: Vec<f32> = pcm[ch * f2_5..ch * 2 * f2_5].to_vec();
            smooth_fade(&redundant_audio[ch * f2_5..], &cur, &mut pcm[ch * f2_5..], f2_5, ch, window, self.rate);
        }
        if transition {
            if audiosize >= f5 {
                pcm[..f2_5 * ch].copy_from_slice(&pcm_transition[..f2_5 * ch]);
                let cur: Vec<f32> = pcm[ch * f2_5..ch * 2 * f2_5].to_vec();
                smooth_fade(&pcm_transition[ch * f2_5..], &cur, &mut pcm[ch * f2_5..], f2_5, ch, window, self.rate);
            } else {
                let cur: Vec<f32> = pcm[..ch * f2_5].to_vec();
                smooth_fade(&pcm_transition, &cur, pcm, f2_5, ch, window, self.rate);
            }
        }
        apply_output_gain(pcm, frame_size, ch, self.gain_q8);
        self.range_final = match dec.as_deref() {
            Some(d) if data_len > 1 => d.rng() ^ redundant_rng,
            _ => 0,
        };
        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        Ok(audiosize)
    }
}

fn smooth_fade(in1: &[f32], in2: &[f32], out: &mut [f32], overlap: usize, ch: usize, window: &[f32; OVERLAP], rate: u32) {
    let inc = (48000 / rate) as usize;
    for c in 0..ch {
        for i in 0..overlap {
            let w = window[i * inc] * window[i * inc];
            out[i * ch + c] = w * in2[i * ch + c] + (1.0 - w) * in1[i * ch + c];
        }
    }
}
