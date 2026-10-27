//! CELT layer decoder (RFC 6716 §4.3).

pub(crate) mod bands;
pub(crate) mod energy;
pub(crate) mod mdct;
pub(crate) mod rate;
pub(crate) mod tables;

use std::sync::OnceLock;

use bands::{anti_collapse, denormalise_bands, quant_all_bands, renormalise_vector};
use energy::{unquant_coarse_energy, unquant_energy_finalise, unquant_fine_energy};
use mdct::ImdctScratch;
use rate::{Mode, compute_allocation};
use tables::*;

use crate::range::{BITRES, RangeDecoder};

const DECODE_BUFFER_SIZE: usize = 2048;
const COMBFILTER_MINPERIOD: i32 = 15;
const PLC_PITCH_LAG_MAX: usize = 720;
const PLC_PITCH_LAG_MIN: usize = 100;

pub(crate) fn mode() -> &'static Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    MODE.get_or_init(Mode::new)
}

/// CELT decoder state for one (mono or stereo) stream.
pub struct CeltDecoder {
    m: &'static Mode,
    /// Output channels.
    channels: usize,
    /// Channels coded in the stream.
    pub stream_channels: usize,
    downsample: usize,
    pub start: usize,
    pub end: usize,
    pub disable_inv: bool,
    pub rng: u32,
    loss_count: u32,
    skip_plc: bool,
    postfilter_period: i32,
    postfilter_period_old: i32,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    preemph_mem: [f32; 2],
    decode_mem: Vec<f32>,
    old_band_e: [f32; 2 * NB_EBANDS],
    old_log_e: [f32; 2 * NB_EBANDS],
    old_log_e2: [f32; 2 * NB_EBANDS],
    background_log_e: [f32; 2 * NB_EBANDS],
    // scratch
    x: Vec<f32>,
    freq: Vec<f32>,
    imdct: ImdctScratch,
    pcm_scratch: Vec<f32>,
}

impl CeltDecoder {
    /// `sample_rate` is the output rate (48000, 24000, 16000, 12000 or 8000).
    pub fn new(sample_rate: u32, channels: usize) -> CeltDecoder {
        let downsample = (48000 / sample_rate) as usize;
        let mut d = CeltDecoder {
            m: mode(),
            channels,
            stream_channels: channels,
            downsample,
            start: 0,
            end: NB_EBANDS,
            disable_inv: channels == 1,
            rng: 0,
            loss_count: 0,
            skip_plc: true,
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            preemph_mem: [0.0; 2],
            decode_mem: vec![0.0; channels * (DECODE_BUFFER_SIZE + OVERLAP)],
            old_band_e: [0.0; 2 * NB_EBANDS],
            old_log_e: [0.0; 2 * NB_EBANDS],
            old_log_e2: [0.0; 2 * NB_EBANDS],
            background_log_e: [0.0; 2 * NB_EBANDS],
            x: Vec::new(),
            freq: Vec::new(),
            imdct: ImdctScratch::default(),
            pcm_scratch: Vec::new(),
        };
        d.reset();
        d
    }

    /// `OPUS_RESET_STATE`.
    pub fn reset(&mut self) {
        self.rng = 0;
        self.loss_count = 0;
        self.skip_plc = true;
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.preemph_mem = [0.0; 2];
        self.decode_mem.fill(0.0);
        self.old_band_e = [0.0; 2 * NB_EBANDS];
        self.old_log_e = [-28.0; 2 * NB_EBANDS];
        self.old_log_e2 = [-28.0; 2 * NB_EBANDS];
        self.background_log_e = [0.0; 2 * NB_EBANDS];
    }

    fn mem_len(&self) -> usize {
        DECODE_BUFFER_SIZE + OVERLAP
    }

    /// Decodes one CELT frame of `frame_size` samples (at the output rate) into interleaved `pcm`
    /// (`channels` wide). With `accum`, the output is added to `pcm`. `data = None` (or fewer than
    /// two bytes) runs the packet-loss concealment.
    pub fn decode(
        &mut self,
        data: Option<&[u8]>,
        frame_size: usize,
        pcm: &mut [f32],
        accum: bool,
        dec: Option<&mut RangeDecoder>,
    ) -> Result<usize, crate::Error> {
        let n_full = frame_size * self.downsample;
        let lm = match n_full {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => return Err(crate::Error::InvalidArgument("CELT frame size")),
        };
        let data = match data {
            Some(d) if d.len() > 1 => d,
            _ => {
                self.decode_lost(n_full, lm);
                self.deemphasis(n_full, pcm, accum);
                return Ok(frame_size);
            }
        };
        if data.len() > 1275 {
            return Err(crate::Error::InvalidPacket("CELT frame too long"));
        }
        match dec {
            Some(d) => self.decode_frame(d, data.len(), n_full, lm),
            None => {
                let mut own = RangeDecoder::new(data);
                self.decode_frame(&mut own, data.len(), n_full, lm)
            }
        }
        self.deemphasis(n_full, pcm, accum);
        Ok(frame_size)
    }

    fn decode_frame(&mut self, dec: &mut RangeDecoder, len: usize, n: usize, lm: usize) {
        let cc = self.channels;
        let c = self.stream_channels;
        let m = self.m;
        let mm = 1usize << lm;
        let start = self.start;
        let end = self.end;
        let eff_end = end.min(NB_EBANDS);
        if self.loss_count == 0 {
            self.skip_plc = false;
        }
        if c == 1 {
            for i in 0..NB_EBANDS {
                self.old_band_e[i] = self.old_band_e[i].max(self.old_band_e[NB_EBANDS + i]);
            }
        }
        let total_bits_raw = len as i32 * 8;
        let mut tell = dec.tell();
        let silence = if tell >= total_bits_raw {
            true
        } else if tell == 1 {
            dec.bit_logp(15)
        } else {
            false
        };
        if silence {
            // Pretend we've read all the remaining bits.
            dec.skip_to_end(total_bits_raw);
            tell = total_bits_raw;
        }
        let mut postfilter_gain = 0.0f32;
        let mut postfilter_pitch = 0i32;
        let mut postfilter_tapset = 0usize;
        if start == 0 && tell + 16 <= total_bits_raw {
            if dec.bit_logp(1) {
                let octave = dec.uint(6);
                postfilter_pitch = ((16 << octave) + dec.bits(4 + octave)) as i32 - 1;
                let qg = dec.bits(3);
                if dec.tell() + 2 <= total_bits_raw {
                    postfilter_tapset = dec.icdf(&TAPSET_ICDF, 2);
                }
                postfilter_gain = 0.09375 * (qg + 1) as f32;
            }
            tell = dec.tell();
        }
        let is_transient = if lm > 0 && tell + 3 <= total_bits_raw {
            let t = dec.bit_logp(3);
            tell = dec.tell();
            t
        } else {
            false
        };
        let short_blocks = is_transient;
        let intra = if tell + 3 <= total_bits_raw { dec.bit_logp(3) } else { false };
        unquant_coarse_energy(start, end, &mut self.old_band_e, intra, dec, c, lm);
        let mut tf_res = [0i32; NB_EBANDS];
        tf_decode(start, end, is_transient, &mut tf_res, lm, dec);
        tell = dec.tell();
        let mut spread = 2usize;
        if tell + 4 <= total_bits_raw {
            spread = dec.icdf(&SPREAD_ICDF, 5);
        }
        let cap = m.init_caps(lm, c);
        let mut offsets = [0i32; NB_EBANDS];
        let mut dynalloc_logp = 6;
        let mut total_bits = total_bits_raw << BITRES;
        let mut tellf = dec.tell_frac() as i32;
        for i in start..end {
            let width = (c as i32 * (EBANDS[i + 1] - EBANDS[i])) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tellf + (loop_logp << BITRES) < total_bits && boost < cap[i] {
                let flag = dec.bit_logp(loop_logp as u32);
                tellf = dec.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits -= quanta;
                loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }
        let alloc_trim = if tellf + (6 << BITRES) <= total_bits { dec.icdf(&TRIM_ICDF, 7) as i32 } else { 5 };
        let mut bits = ((len as i32 * 8) << BITRES) - dec.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) { 1 << BITRES } else { 0 };
        bits -= anti_collapse_rsv;
        let alloc = compute_allocation(m, start, end, &offsets, &cap, alloc_trim, bits, c, lm, dec);
        unquant_fine_energy(start, end, &mut self.old_band_e, &alloc.fine_quant, dec, c);

        // Shift the decode memory by one frame.
        let ml = self.mem_len();
        for ch in 0..cc {
            let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
            mem.copy_within(n..DECODE_BUFFER_SIZE + OVERLAP / 2, 0);
        }

        let mut collapse_masks = vec![0u32; c * NB_EBANDS];
        let mut x = std::mem::take(&mut self.x);
        x.clear();
        x.resize(c * n, 0.0);
        {
            let (xa, xb) = x.split_at_mut(n);
            let yb = if c == 2 { Some(xb) } else { None };
            let mut seed = self.rng;
            quant_all_bands(
                m,
                start,
                end,
                xa,
                yb,
                &mut collapse_masks,
                &alloc.pulses,
                short_blocks,
                spread,
                alloc.dual_stereo,
                alloc.intensity,
                &tf_res,
                ((len as i32) * (8 << BITRES)) - anti_collapse_rsv,
                alloc.balance,
                dec,
                lm,
                alloc.coded_bands,
                &mut seed,
                self.disable_inv,
            );
            self.rng = seed;
        }
        let mut anti_collapse_on = false;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = dec.bits(1) != 0;
        }
        let bits_left = len as i32 * 8 - dec.tell();
        unquant_energy_finalise(start, end, &mut self.old_band_e, &alloc.fine_quant, &alloc.fine_priority, bits_left, dec, c);
        if anti_collapse_on {
            anti_collapse(&mut x, &collapse_masks, lm, c, n, start, end, &self.old_band_e, &self.old_log_e, &self.old_log_e2, &alloc.pulses, self.rng);
        }
        if silence {
            for v in self.old_band_e[..c * NB_EBANDS].iter_mut() {
                *v = -28.0;
            }
        }
        self.synthesis(&x, n, start, eff_end, c, is_transient, lm, silence);
        self.x = x;

        // Post-filter.
        for ch in 0..cc {
            self.postfilter_period = self.postfilter_period.max(COMBFILTER_MINPERIOD);
            self.postfilter_period_old = self.postfilter_period_old.max(COMBFILTER_MINPERIOD);
            let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
            let off = DECODE_BUFFER_SIZE - n;
            comb_filter(
                mem,
                off,
                self.postfilter_period_old,
                self.postfilter_period,
                SHORT_MDCT,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                &m.window,
            );
            if lm != 0 {
                comb_filter(
                    mem,
                    off + SHORT_MDCT,
                    self.postfilter_period,
                    postfilter_pitch,
                    n - SHORT_MDCT,
                    self.postfilter_gain,
                    postfilter_gain,
                    self.postfilter_tapset,
                    postfilter_tapset,
                    &m.window,
                );
            }
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        if c == 1 {
            let (a, b) = self.old_band_e.split_at_mut(NB_EBANDS);
            b.copy_from_slice(a);
        }
        if !is_transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
            for i in 0..2 * NB_EBANDS {
                self.background_log_e[i] = (self.background_log_e[i] + mm as f32 * 0.001).min(self.old_band_e[i]);
            }
        } else {
            for i in 0..2 * NB_EBANDS {
                self.old_log_e[i] = self.old_log_e[i].min(self.old_band_e[i]);
            }
        }
        for ch in 0..2 {
            for i in (0..start).chain(end..NB_EBANDS) {
                self.old_band_e[ch * NB_EBANDS + i] = 0.0;
                self.old_log_e[ch * NB_EBANDS + i] = -28.0;
                self.old_log_e2[ch * NB_EBANDS + i] = -28.0;
            }
        }
        self.rng = dec.rng();
        self.loss_count = 0;
    }

    /// Denormalisation + IMDCT into the decode memory (`celt_synthesis`).
    #[allow(clippy::too_many_arguments)]
    fn synthesis(&mut self, x: &[f32], n: usize, start: usize, eff_end: usize, c: usize, is_transient: bool, lm: usize, silence: bool) {
        let cc = self.channels;
        let m = self.m;
        let mm = 1usize << lm;
        let (bcount, nb, shift) = if is_transient { (mm, SHORT_MDCT, MAX_LM) } else { (1, SHORT_MDCT << lm, MAX_LM - lm) };
        let ml = self.mem_len();
        let mut freq = std::mem::take(&mut self.freq);
        freq.clear();
        freq.resize(2 * n, 0.0);
        let (f0, f1) = freq.split_at_mut(n);
        let out_off = DECODE_BUFFER_SIZE - n;
        if cc == 2 && c == 1 {
            denormalise_bands(x, f0, &self.old_band_e, start, eff_end, mm, self.downsample, silence);
            f1.copy_from_slice(f0);
            for (ch, f) in [(0usize, &*f1), (1usize, &*f0)] {
                let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
                for b in 0..bcount {
                    m.mdct[shift].backward(&f[b..], bcount, &mut mem[out_off + nb * b..], &m.window, OVERLAP, &mut self.imdct);
                }
            }
        } else if cc == 1 && c == 2 {
            denormalise_bands(x, f0, &self.old_band_e, start, eff_end, mm, self.downsample, silence);
            denormalise_bands(&x[n..], f1, &self.old_band_e[NB_EBANDS..], start, eff_end, mm, self.downsample, silence);
            for i in 0..n {
                f0[i] = 0.5 * (f0[i] + f1[i]);
            }
            let mem = &mut self.decode_mem[..ml];
            for b in 0..bcount {
                m.mdct[shift].backward(&f0[b..], bcount, &mut mem[out_off + nb * b..], &m.window, OVERLAP, &mut self.imdct);
            }
        } else {
            for ch in 0..cc {
                denormalise_bands(&x[ch * n..], f0, &self.old_band_e[ch * NB_EBANDS..], start, eff_end, mm, self.downsample, silence);
                let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
                for b in 0..bcount {
                    m.mdct[shift].backward(&f0[b..], bcount, &mut mem[out_off + nb * b..], &m.window, OVERLAP, &mut self.imdct);
                }
            }
        }
        self.freq = freq;
    }

    fn deemphasis(&mut self, n: usize, pcm: &mut [f32], accum: bool) {
        let cc = self.channels;
        let ml = self.mem_len();
        let ds = self.downsample;
        let out_off = DECODE_BUFFER_SIZE - n;
        let mut scratch = std::mem::take(&mut self.pcm_scratch);
        for ch in 0..cc {
            let mem = &self.decode_mem[ch * ml + out_off..ch * ml + out_off + n];
            let mut mstate = self.preemph_mem[ch];
            scratch.clear();
            for &v in mem {
                let tmp = v + 1e-30 + mstate;
                mstate = PREEMPH * tmp;
                scratch.push(tmp);
            }
            self.preemph_mem[ch] = mstate;
            for j in 0..n / ds {
                let v = scratch[j * ds] * (1.0 / 32768.0);
                let o = &mut pcm[j * cc + ch];
                if accum {
                    *o += v;
                } else {
                    *o = v;
                }
            }
        }
        self.pcm_scratch = scratch;
    }

    /// Packet-loss concealment: pitch-periodic extension for the first lost frames, then
    /// decaying shaped noise.
    fn decode_lost(&mut self, n: usize, lm: usize) {
        let cc = self.channels;
        let ml = self.mem_len();
        let m = self.m;
        let pitch_based = self.loss_count < 5 && self.start == 0 && !self.skip_plc;
        if !pitch_based {
            let decay = if self.loss_count == 0 { 1.5 } else { 0.5 };
            for ch in 0..cc {
                for i in self.start..self.end {
                    let k = ch * NB_EBANDS + i;
                    self.old_band_e[k] = self.background_log_e[k].max(self.old_band_e[k] - decay);
                }
            }
            let mut seed = self.rng;
            let mut x = std::mem::take(&mut self.x);
            x.clear();
            x.resize(cc * n, 0.0);
            let eff_end = self.end.min(NB_EBANDS);
            for ch in 0..cc {
                for i in self.start..eff_end {
                    let off = n * ch + ((EBANDS[i] as usize) << lm);
                    let blen = ((EBANDS[i + 1] - EBANDS[i]) as usize) << lm;
                    for v in x[off..off + blen].iter_mut() {
                        seed = bands::lcg_rand(seed);
                        *v = ((seed as i32) >> 20) as f32;
                    }
                    renormalise_vector(&mut x[off..off + blen], 1.0);
                }
            }
            self.rng = seed;
            for ch in 0..cc {
                let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
                mem.copy_within(n..DECODE_BUFFER_SIZE + OVERLAP / 2, 0);
            }
            let saved_sc = self.stream_channels;
            self.stream_channels = cc;
            self.synthesis(&x, n, self.start, eff_end, cc, false, lm, false);
            self.stream_channels = saved_sc;
            self.x = x;
        } else {
            // Pitch search on the (mono-mixed) history.
            let hist_len = 1024usize;
            let base = DECODE_BUFFER_SIZE - hist_len;
            let mut mono = vec![0f32; hist_len];
            for ch in 0..cc {
                for (i, v) in mono.iter_mut().enumerate() {
                    *v += self.decode_mem[ch * ml + base + i];
                }
            }
            let period = if self.loss_count == 0 { find_pitch(&mono) } else { self.postfilter_period_old.max(PLC_PITCH_LAG_MIN as i32) as usize };
            let period = period.clamp(PLC_PITCH_LAG_MIN, PLC_PITCH_LAG_MAX);
            // Remember the period for subsequent losses in the (otherwise unused) old period slot.
            self.postfilter_period_old = period as i32;
            let fade = 0.8f32.powi(self.loss_count as i32 + 1);
            let mut ext = vec![0f32; n + OVERLAP];
            for ch in 0..cc {
                let mem = &mut self.decode_mem[ch * ml..(ch + 1) * ml];
                // Energy decay across the last two periods.
                let e1: f32 = mem[DECODE_BUFFER_SIZE - period..DECODE_BUFFER_SIZE].iter().map(|v| v * v).sum();
                let e0: f32 = mem[DECODE_BUFFER_SIZE - 2 * period..DECODE_BUFFER_SIZE - period].iter().map(|v| v * v).sum();
                let decay = if e0 > 0.0 { (e1 / e0).sqrt().min(1.0) } else { 0.0 };
                let mut g = 1.0f32;
                for (i, e) in ext.iter_mut().enumerate() {
                    if i > 0 && i % period == 0 {
                        g *= decay;
                    }
                    let src = DECODE_BUFFER_SIZE - period + (i % period);
                    *e = mem[src] * g * fade;
                }
                mem.copy_within(n..DECODE_BUFFER_SIZE + OVERLAP / 2, 0);
                let out_off = DECODE_BUFFER_SIZE - n;
                // Cross-fade the start with the previous overlap so the transition is smooth.
                mem[out_off..out_off + n].copy_from_slice(&ext[..n]);
                let tail = &ext[n..n + OVERLAP];
                for i in 0..OVERLAP / 2 {
                    mem[DECODE_BUFFER_SIZE + i] = m.window[i] * tail[OVERLAP - 1 - i] + m.window[OVERLAP - 1 - i] * tail[i];
                }
            }
        }
        self.loss_count += 1;
    }
}

fn find_pitch(x: &[f32]) -> usize {
    let n = x.len();
    let win = 512.min(n - PLC_PITCH_LAG_MAX);
    let tgt = &x[n - win..];
    let mut best = PLC_PITCH_LAG_MIN;
    let mut best_score = f32::MIN;
    let e_t: f32 = tgt.iter().map(|v| v * v).sum::<f32>() + 1e-9;
    for lag in PLC_PITCH_LAG_MIN..=PLC_PITCH_LAG_MAX {
        let src = &x[n - win - lag..n - lag];
        let mut xy = 0f32;
        let mut yy = 1e-9f32;
        for (a, b) in tgt.iter().zip(src) {
            xy += a * b;
            yy += b * b;
        }
        let score = xy / (e_t * yy).sqrt();
        if score > best_score {
            best_score = score;
            best = lag;
        }
    }
    best
}

fn tf_decode(start: usize, end: usize, is_transient: bool, tf_res: &mut [i32; NB_EBANDS], lm: usize, dec: &mut RangeDecoder) {
    let mut budget = dec.storage() as i32 * 8;
    let mut tell = dec.tell();
    let mut logp: u32 = if is_transient { 2 } else { 4 };
    let tf_select_rsv = lm > 0 && tell + logp as i32 + 1 <= budget;
    budget -= tf_select_rsv as i32;
    let mut tf_changed = 0;
    let mut curr = 0;
    for r in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp as i32 <= budget {
            curr ^= dec.bit_logp(logp) as i32;
            tell = dec.tell();
            tf_changed |= curr;
        }
        *r = curr;
        logp = if is_transient { 4 } else { 5 };
    }
    let mut tf_select = 0;
    let ti = 4 * is_transient as usize;
    if tf_select_rsv && TF_SELECT_TABLE[lm][ti + tf_changed as usize] != TF_SELECT_TABLE[lm][ti + 2 + tf_changed as usize] {
        tf_select = dec.bit_logp(1) as usize;
    }
    for r in tf_res.iter_mut().take(end).skip(start) {
        *r = TF_SELECT_TABLE[lm][ti + 2 * tf_select + *r as usize] as i32;
    }
}

/// Pitch post-filter (in place on `buf[off .. off + n]`, reading history before `off`).
#[allow(clippy::too_many_arguments)]
fn comb_filter(buf: &mut [f32], off: usize, t0: i32, t1: i32, n: usize, g0: f32, g1: f32, tapset0: usize, tapset1: usize, window: &[f32; OVERLAP]) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    let t0 = t0.max(COMBFILTER_MINPERIOD) as usize;
    let t1 = t1.max(COMBFILTER_MINPERIOD) as usize;
    let g00 = g0 * COMB_GAINS[tapset0][0];
    let g01 = g0 * COMB_GAINS[tapset0][1];
    let g02 = g0 * COMB_GAINS[tapset0][2];
    let g10 = g1 * COMB_GAINS[tapset1][0];
    let g11 = g1 * COMB_GAINS[tapset1][1];
    let g12 = g1 * COMB_GAINS[tapset1][2];
    let mut x1 = buf[off + 1 - t1];
    let mut x2 = buf[off - t1];
    let mut x3 = buf[off - t1 - 1];
    let mut x4 = buf[off - t1 - 2];
    let overlap = if g0 == g1 && t0 == t1 && tapset0 == tapset1 { 0 } else { OVERLAP.min(n) };
    let mut i = 0;
    while i < overlap {
        let p = off + i;
        let x0 = buf[p + 2 - t1];
        let f = window[i] * window[i];
        let a = 1.0 - f;
        buf[p] = buf[p]
            + a * g00 * buf[p - t0]
            + a * g01 * (buf[p + 1 - t0] + buf[p - t0 - 1])
            + a * g02 * (buf[p + 2 - t0] + buf[p - t0 - 2])
            + f * g10 * x2
            + f * g11 * (x1 + x3)
            + f * g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
    if g1 == 0.0 {
        return;
    }
    // Constant filter for the rest of the block.
    let t = t1;
    for i in overlap..n {
        let p = off + i;
        buf[p] = buf[p] + g10 * buf[p - t] + g11 * (buf[p + 1 - t] + buf[p - t - 1]) + g12 * (buf[p + 2 - t] + buf[p - t - 2]);
    }
}
