//! SILK layer decoder (RFC 6716 §4.2).
//!
//! Parameter decoding and reconstruction follow the normative fixed-point arithmetic so that the
//! output matches the reference decoder sample for sample at the internal rate. Resampling to the
//! output rate uses our own linear-phase interpolator with the delays of RFC 6716 Table 54.

pub(crate) mod lpc;
mod resampler;
#[allow(dead_code)]
pub(crate) mod tables;

use lpc::*;
use resampler::Resampler;
use tables::*;

use crate::range::RangeDecoder;
use crate::{Error, Result};

const MAX_FRAME: usize = 320;
const MAX_SUBFR: usize = 80;
const LTP_ORDER: usize = 5;
const OUT_BUF: usize = MAX_FRAME + 2 * MAX_SUBFR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostFlag {
    Normal,
    Lost,
    Lbrr,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cond {
    Independently,
    IndependentlyNoLtpScaling,
    Conditionally,
}

#[derive(Clone, Default)]
struct Indices {
    gains: [i32; 4],
    ltp_index: [usize; 4],
    nlsf: [i32; 17],
    lag_index: i32,
    contour_index: usize,
    signal_type: usize,
    quant_offset_type: usize,
    nlsf_interp_q2: i32,
    per_index: usize,
    ltp_scale_index: usize,
    seed: i32,
}

#[derive(Clone)]
struct Channel {
    fs_khz: usize,
    nb_subfr: usize,
    frame_length: usize,
    subfr_length: usize,
    ltp_mem_length: usize,
    lpc_order: usize,
    prev_gain_q16: i32,
    s_lpc_q14: [i32; MAX_LPC_ORDER],
    out_buf: [i16; OUT_BUF],
    lag_prev: i32,
    last_gain_index: i32,
    prev_signal_type: usize,
    first_frame_after_reset: bool,
    prev_nlsf_q15: [i32; MAX_LPC_ORDER],
    ec_prev_signal_type: usize,
    ec_prev_lag_index: i32,
    vad_flags: [bool; 3],
    lbrr_flag: bool,
    lbrr_flags: [bool; 3],
    n_frames_decoded: usize,
    n_frames_per_packet: usize,
    loss_cnt: u32,
    indices: Indices,
    resampler: Resampler,
    // PLC memory.
    plc_lpc_q12: [i32; MAX_LPC_ORDER],
    plc_gain_q16: i32,
    plc_pitch: i32,
    plc_rand: u32,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            fs_khz: 0,
            nb_subfr: 4,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_gain_q16: 65536,
            s_lpc_q14: [0; MAX_LPC_ORDER],
            out_buf: [0; OUT_BUF],
            lag_prev: 100,
            last_gain_index: 10,
            prev_signal_type: 0,
            first_frame_after_reset: true,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            vad_flags: [false; 3],
            lbrr_flag: false,
            lbrr_flags: [false; 3],
            n_frames_decoded: 0,
            n_frames_per_packet: 1,
            loss_cnt: 0,
            indices: Indices::default(),
            resampler: Resampler::new(16000, 16000),
            plc_lpc_q12: [0; MAX_LPC_ORDER],
            plc_gain_q16: 0,
            plc_pitch: 0,
            plc_rand: 22222,
        }
    }

    fn set_fs(&mut self, fs_khz: usize, api_rate: u32) {
        self.subfr_length = 5 * fs_khz;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz || self.resampler.rates() != (fs_khz as u32 * 1000, api_rate) {
            self.resampler = Resampler::new(fs_khz as u32 * 1000, api_rate);
        }
        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = 20 * fs_khz;
                self.lpc_order = if fs_khz == 16 { 16 } else { 10 };
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = 0;
                self.out_buf = [0; OUT_BUF];
                self.s_lpc_q14 = [0; MAX_LPC_ORDER];
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }

    fn decode_indices(&mut self, dec: &mut RangeDecoder, frame_index: usize, decode_lbrr: bool, cond: Cond) {
        let ix = if decode_lbrr || self.vad_flags[frame_index] { dec.icdf(&TYPE_ACTIVE, 8) + 2 } else { dec.icdf(&TYPE_INACTIVE, 8) };
        let ind = &mut self.indices;
        ind.signal_type = ix >> 1;
        ind.quant_offset_type = ix & 1;
        if cond == Cond::Conditionally {
            ind.gains[0] = dec.icdf(&DELTA_GAIN, 8) as i32;
        } else {
            ind.gains[0] = (dec.icdf(GAIN_MSB[ind.signal_type], 8) as i32) << 3;
            ind.gains[0] += dec.icdf(&UNIFORM8, 8) as i32;
        }
        for i in 1..self.nb_subfr {
            ind.gains[i] = dec.icdf(&DELTA_GAIN, 8) as i32;
        }
        let wb = self.lpc_order == 16;
        let s1 = (ind.signal_type >> 1) + if wb { 2 } else { 0 };
        let i1 = dec.icdf(NLSF_STAGE1[s1], 8);
        ind.nlsf[0] = i1 as i32;
        for i in 0..self.lpc_order {
            let cb = if wb { 8 + NLSF_SEL_WB[i1][i] as usize } else { NLSF_SEL_NBMB[i1][i] as usize };
            let mut v = dec.icdf(NLSF_STAGE2[cb], 8) as i32;
            if v == 0 {
                v -= dec.icdf(&NLSF_EXT, 8) as i32;
            } else if v == 8 {
                v += dec.icdf(&NLSF_EXT, 8) as i32;
            }
            ind.nlsf[i + 1] = v - 4;
        }
        ind.nlsf_interp_q2 = if self.nb_subfr == 4 { dec.icdf(&NLSF_INTERP, 8) as i32 } else { 4 };
        if ind.signal_type == 2 {
            let mut absolute = true;
            if cond == Cond::Conditionally && self.ec_prev_signal_type == 2 {
                let d = dec.icdf(&PITCH_DELTA, 8) as i32;
                if d > 0 {
                    ind.lag_index = self.ec_prev_lag_index + d - 9;
                    absolute = false;
                }
            }
            if absolute {
                ind.lag_index = dec.icdf(&PITCH_HIGH, 8) as i32 * (self.fs_khz as i32 >> 1);
                let low = match self.fs_khz {
                    8 => PITCH_LOW[0],
                    12 => PITCH_LOW[1],
                    _ => PITCH_LOW[2],
                };
                ind.lag_index += dec.icdf(low, 8) as i32;
            }
            self.ec_prev_lag_index = ind.lag_index;
            let contour = match (self.fs_khz, self.nb_subfr) {
                (8, 2) => PITCH_CONTOUR[0],
                (8, _) => PITCH_CONTOUR[1],
                (_, 2) => PITCH_CONTOUR[2],
                _ => PITCH_CONTOUR[3],
            };
            ind.contour_index = dec.icdf(contour, 8);
            ind.per_index = dec.icdf(&LTP_PERIODICITY, 8);
            for k in 0..self.nb_subfr {
                ind.ltp_index[k] = dec.icdf(LTP_FILTER[ind.per_index], 8);
            }
            ind.ltp_scale_index = if cond == Cond::Independently { dec.icdf(&LTP_SCALE, 8) } else { 0 };
        }
        self.ec_prev_signal_type = ind.signal_type;
        ind.seed = dec.icdf(&UNIFORM4, 8) as i32;
    }

    fn decode_frame(&mut self, dec: Option<&mut RangeDecoder>, out: &mut [i16], lost: LostFlag, cond: Cond) {
        let l = self.frame_length;
        let decode = match (lost, &dec) {
            (LostFlag::Normal, Some(_)) => true,
            (LostFlag::Lbrr, Some(_)) => self.lbrr_flags[self.n_frames_decoded],
            _ => false,
        };
        if decode && let Some(dec) = dec {
            self.decode_indices(dec, self.n_frames_decoded, lost == LostFlag::Lbrr, cond);
            let mut pulses = [0i32; MAX_FRAME + 16];
            decode_pulses(dec, &mut pulses, self.indices.signal_type, self.indices.quant_offset_type, l);
            let ctrl = self.decode_parameters(cond);
            self.decode_core(&ctrl, &pulses, out);
            self.plc_lpc_q12 = ctrl.pred_coef_q12[1];
            self.plc_gain_q16 = ctrl.gains_q16[self.nb_subfr - 1];
            self.plc_pitch = if self.indices.signal_type == 2 { ctrl.pitch_l[self.nb_subfr - 1] } else { 0 };
            self.loss_cnt = 0;
            self.prev_signal_type = self.indices.signal_type;
            self.first_frame_after_reset = false;
            self.lag_prev = ctrl.pitch_l[self.nb_subfr - 1];
        } else {
            self.conceal(out);
        }
        // Update the output buffer.
        let mv = self.ltp_mem_length - l;
        self.out_buf.copy_within(l..l + mv, 0);
        self.out_buf[mv..mv + l].copy_from_slice(&out[..l]);
    }

    fn decode_parameters(&mut self, cond: Cond) -> Ctrl {
        let mut ctrl = Ctrl::default();
        let nb = self.nb_subfr;
        // Gains.
        let mut prev = self.last_gain_index;
        for k in 0..nb {
            let ind = self.indices.gains[k];
            if k == 0 && cond != Cond::Conditionally {
                prev = ind.max(prev - 16);
            } else {
                let tmp = ind - 4;
                let thr = 2 * 36 - 64 + prev;
                if tmp > thr {
                    prev += (tmp << 1) - thr;
                } else {
                    prev += tmp;
                }
            }
            prev = prev.clamp(0, 63);
            ctrl.gains_q16[k] = log2lin((smulwb(0x1D1C71, prev) + 2090).min(3967));
        }
        self.last_gain_index = prev;
        // LPC.
        let order = self.lpc_order;
        let nlsf = nlsf_decode(&self.indices.nlsf, order == 16);
        ctrl.pred_coef_q12[1] = nlsf2a(&nlsf[..order]);
        if self.first_frame_after_reset {
            self.indices.nlsf_interp_q2 = 4;
        }
        if self.indices.nlsf_interp_q2 < 4 {
            let mut n0 = [0i32; MAX_LPC_ORDER];
            for i in 0..order {
                n0[i] = self.prev_nlsf_q15[i] + ((self.indices.nlsf_interp_q2 * (nlsf[i] - self.prev_nlsf_q15[i])) >> 2);
            }
            ctrl.pred_coef_q12[0] = nlsf2a(&n0[..order]);
        } else {
            ctrl.pred_coef_q12[0] = ctrl.pred_coef_q12[1];
        }
        self.prev_nlsf_q15 = nlsf;
        if self.loss_cnt > 0 {
            bwexpander(&mut ctrl.pred_coef_q12[0][..order], 63570);
            bwexpander(&mut ctrl.pred_coef_q12[1][..order], 63570);
        }
        if self.indices.signal_type == 2 {
            let fs = self.fs_khz as i32;
            let min_lag = 2 * fs;
            let max_lag = 18 * fs;
            let lag = min_lag + self.indices.lag_index;
            for k in 0..nb {
                let off = match (self.fs_khz, nb) {
                    (8, 2) => CB_NB_10[self.indices.contour_index][k],
                    (8, _) => CB_NB_20[self.indices.contour_index][k],
                    (_, 2) => CB_WB_10[self.indices.contour_index][k],
                    _ => CB_WB_20[self.indices.contour_index][k],
                };
                ctrl.pitch_l[k] = (lag + off).clamp(min_lag, max_lag);
            }
            for k in 0..nb {
                let ix = self.indices.ltp_index[k];
                let taps = match self.indices.per_index {
                    0 => LTP_TAPS_0[ix.min(7)],
                    1 => LTP_TAPS_1[ix.min(15)],
                    _ => LTP_TAPS_2[ix.min(31)],
                };
                for i in 0..LTP_ORDER {
                    ctrl.ltp_coef_q14[k * LTP_ORDER + i] = taps[i] << 7;
                }
            }
            ctrl.ltp_scale_q14 = [15565, 12288, 8192][self.indices.ltp_scale_index];
        }
        ctrl
    }

    fn decode_core(&mut self, ctrl: &Ctrl, pulses: &[i32], xq: &mut [i16]) {
        let ind = &self.indices;
        let l = self.frame_length;
        let sub = self.subfr_length;
        let order = self.lpc_order;
        let ltp_mem = self.ltp_mem_length;
        let mut s_ltp = [0i16; MAX_FRAME];
        let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME];
        let mut res_q14 = [0i32; MAX_SUBFR];
        let mut s_lpc = [0i32; MAX_SUBFR + MAX_LPC_ORDER];
        let offset_q10 = [[100, 240], [32, 100]][ind.signal_type >> 1][ind.quant_offset_type];
        let interp = ind.nlsf_interp_q2 < 4;
        let mut exc = [0i32; MAX_FRAME];
        let mut seed = ind.seed;
        for i in 0..l {
            seed = seed.wrapping_mul(196_314_165).wrapping_add(907_633_515);
            let mut e = pulses[i] << 14;
            if e > 0 {
                e -= 80 << 4;
            } else if e < 0 {
                e += 80 << 4;
            }
            e += offset_q10 << 4;
            if seed < 0 {
                e = -e;
            }
            exc[i] = e;
            seed = seed.wrapping_add(pulses[i]);
        }
        s_lpc[..MAX_LPC_ORDER].copy_from_slice(&self.s_lpc_q14);
        let mut ltp_buf_idx = ltp_mem;
        for k in 0..self.nb_subfr {
            let a_q12 = &ctrl.pred_coef_q12[k >> 1];
            let mut b_q14 = [0i32; LTP_ORDER];
            b_q14.copy_from_slice(&ctrl.ltp_coef_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER]);
            let mut signal_type = ind.signal_type;
            let gain_q10 = ctrl.gains_q16[k] >> 6;
            let mut inv_gain_q31 = inverse32_varq(ctrl.gains_q16[k], 47);
            let gain_adj_q16 = if ctrl.gains_q16[k] != self.prev_gain_q16 {
                let g = div32_varq(self.prev_gain_q16, ctrl.gains_q16[k], 16);
                for v in s_lpc[..MAX_LPC_ORDER].iter_mut() {
                    *v = smulww(g, *v);
                }
                g
            } else {
                1 << 16
            };
            self.prev_gain_q16 = ctrl.gains_q16[k];
            let mut lag = ctrl.pitch_l[k];
            if self.loss_cnt > 0 && self.prev_signal_type == 2 && signal_type != 2 && k < 2 {
                b_q14 = [0; LTP_ORDER];
                b_q14[LTP_ORDER / 2] = 1 << 12;
                signal_type = 2;
                lag = self.lag_prev;
            }
            if signal_type == 2 {
                let lag_u = lag as usize;
                if k == 0 || (k == 2 && interp) {
                    let start_idx = ltp_mem as isize - lag as isize - order as isize - (LTP_ORDER / 2) as isize;
                    let start_idx = start_idx.max(0) as usize;
                    if k == 2 {
                        self.out_buf[ltp_mem..ltp_mem + 2 * sub].copy_from_slice(&xq[..2 * sub]);
                    }
                    let src_off = start_idx + k * sub;
                    lpc_analysis_filter(&mut s_ltp[start_idx..ltp_mem], &self.out_buf[src_off..src_off + ltp_mem - start_idx], a_q12, order);
                    if k == 0 {
                        inv_gain_q31 = smulwb(inv_gain_q31, ctrl.ltp_scale_q14) << 2;
                    }
                    for i in 0..(lag_u + LTP_ORDER / 2).min(ltp_mem) {
                        s_ltp_q15[ltp_buf_idx - i - 1] = smulwb(inv_gain_q31, s_ltp[ltp_mem - i - 1] as i32);
                    }
                } else if gain_adj_q16 != 1 << 16 {
                    for i in 0..(lag_u + LTP_ORDER / 2).min(ltp_buf_idx) {
                        let p = ltp_buf_idx - i - 1;
                        s_ltp_q15[p] = smulww(gain_adj_q16, s_ltp_q15[p]);
                    }
                }
                let base = ltp_buf_idx as isize - lag as isize + (LTP_ORDER / 2) as isize;
                for i in 0..sub {
                    let p = base + i as isize;
                    let g = |o: isize| -> i32 { if p - o >= 0 { s_ltp_q15[(p - o) as usize] } else { 0 } };
                    let mut pred = 2i32;
                    pred = smlawb(pred, g(0), b_q14[0]);
                    pred = smlawb(pred, g(1), b_q14[1]);
                    pred = smlawb(pred, g(2), b_q14[2]);
                    pred = smlawb(pred, g(3), b_q14[3]);
                    pred = smlawb(pred, g(4), b_q14[4]);
                    res_q14[i] = exc[k * sub + i].wrapping_add(pred << 1);
                    s_ltp_q15[ltp_buf_idx] = res_q14[i] << 1;
                    ltp_buf_idx += 1;
                }
            } else {
                res_q14[..sub].copy_from_slice(&exc[k * sub..(k + 1) * sub]);
            }
            for i in 0..sub {
                let mut pred = (order >> 1) as i32;
                for j in 0..order {
                    pred = smlawb(pred, s_lpc[MAX_LPC_ORDER + i - 1 - j], a_q12[j]);
                }
                let v = res_q14[i].saturating_add(pred.saturating_mul(16));
                s_lpc[MAX_LPC_ORDER + i] = v;
                xq[k * sub + i] = sat16(rshift_round(smulww(v, gain_q10), 8)) as i16;
            }
            s_lpc.copy_within(sub..sub + MAX_LPC_ORDER, 0);
        }
        self.s_lpc_q14.copy_from_slice(&s_lpc[..MAX_LPC_ORDER]);
    }

    /// Basic concealment: pitch-periodic repetition for voiced history, otherwise LPC-shaped
    /// noise at the residual level of the history; both fade out over successive losses.
    fn conceal(&mut self, out: &mut [i16]) {
        let l = self.frame_length;
        if l == 0 {
            return;
        }
        self.loss_cnt += 1;
        let order = self.lpc_order;
        let mem = self.ltp_mem_length;
        let fade = 0.85f32.powi(self.loss_cnt as i32);
        let hist: Vec<f32> = self.out_buf[..mem].iter().map(|&v| v as f32).collect();
        let pitch = self.plc_pitch as usize;
        let mut y = hist.clone();
        if pitch > 0 && pitch < mem {
            for _ in 0..l {
                let v = y[y.len() - pitch] * fade;
                y.push(v);
            }
        } else {
            let mut a12 = self.plc_lpc_q12;
            bwexpander(&mut a12[..order], 64880);
            self.plc_lpc_q12 = a12;
            let a: Vec<f32> = a12[..order].iter().map(|&v| v as f32 / 4096.0).collect();
            let mut e = 0f32;
            for i in order..mem {
                let mut r = hist[i];
                for j in 0..order {
                    r -= a[j] * hist[i - 1 - j];
                }
                e += r * r;
            }
            let rms = (e / (mem - order).max(1) as f32).sqrt() * fade;
            for _ in 0..l {
                self.plc_rand = self.plc_rand.wrapping_mul(196_314_165).wrapping_add(907_633_515);
                let mut v = ((self.plc_rand as i32) >> 16) as f32 / 32768.0 * rms * 1.7;
                for j in 0..order {
                    v += a[j] * y[y.len() - 1 - j];
                }
                y.push(v.clamp(-32768.0, 32767.0));
            }
        }
        for (o, v) in out.iter_mut().zip(&y[mem..mem + l]) {
            *o = v.clamp(-32768.0, 32767.0) as i16;
        }
        // Keep the LPC synthesis state consistent for the next decoded frame.
        let g = self.prev_gain_q16.max(1) as i64;
        for j in 0..MAX_LPC_ORDER {
            let v = out[l - MAX_LPC_ORDER + j] as i64;
            self.s_lpc_q14[j] = ((v << 30) / g).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
        self.lag_prev = self.plc_pitch.max(1);
        self.first_frame_after_reset = false;
    }
}

#[derive(Default)]
struct Ctrl {
    gains_q16: [i32; 4],
    pred_coef_q12: [[i32; MAX_LPC_ORDER]; 2],
    pitch_l: [i32; 4],
    ltp_coef_q14: [i32; 4 * LTP_ORDER],
    ltp_scale_q14: i32,
}

fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= 3967 {
        return i32::MAX;
    }
    let out = 1i32 << (in_log_q7 >> 7);
    let frac = in_log_q7 & 0x7F;
    if in_log_q7 < 2048 { out + ((out * smlawb(frac, frac * (128 - frac), -174)) >> 7) } else { out + (out >> 7) * smlawb(frac, frac * (128 - frac), -174) }
}

fn lpc_analysis_filter(out: &mut [i16], input: &[i16], b: &[i32], d: usize) {
    let len = out.len();
    for ix in d..len {
        let mut acc: i32 = 0;
        for j in 0..d {
            acc = acc.wrapping_add((input[ix - 1 - j] as i32).wrapping_mul(b[j]));
        }
        let v = ((input[ix] as i32) << 12).wrapping_sub(acc);
        out[ix] = sat16(rshift_round(v, 12)) as i16;
    }
    for v in out.iter_mut().take(d.min(len)) {
        *v = 0;
    }
}

fn decode_pulses(dec: &mut RangeDecoder, pulses: &mut [i32], signal_type: usize, quant_offset_type: usize, frame_length: usize) {
    let rate_level = dec.icdf(RATE_LEVEL[signal_type >> 1], 8);
    let mut iter = frame_length >> 4;
    if iter * 16 < frame_length {
        iter += 1;
    }
    let mut sum_pulses = [0i32; 20];
    let mut n_lshifts = [0i32; 20];
    for i in 0..iter {
        let mut s = dec.icdf(PULSE_COUNT[rate_level], 8) as i32;
        while s == 17 {
            n_lshifts[i] += 1;
            let tab = if n_lshifts[i] == 10 { PULSE_COUNT[10] } else { PULSE_COUNT[9] };
            s = dec.icdf(tab, 8) as i32;
        }
        sum_pulses[i] = s;
    }
    for i in 0..iter {
        let blk = &mut pulses[i * 16..(i + 1) * 16];
        if sum_pulses[i] > 0 {
            shell_decode(dec, blk, sum_pulses[i]);
        } else {
            blk.fill(0);
        }
    }
    for i in 0..iter {
        let nls = n_lshifts[i];
        if nls > 0 {
            for p in pulses[i * 16..(i + 1) * 16].iter_mut() {
                let mut a = *p;
                for _ in 0..nls {
                    a = (a << 1) + dec.icdf(&EXC_LSB, 8) as i32;
                }
                *p = a;
            }
            sum_pulses[i] |= nls << 5;
        }
    }
    // Signs.
    let base = 7 * (quant_offset_type + (signal_type << 1));
    let len = (frame_length + 8) >> 4;
    for i in 0..len {
        let p = sum_pulses[i];
        if p > 0 {
            let tab = EXC_SIGN[base + ((p & 0x1F) as usize).min(6)];
            for q in pulses[i * 16..(i + 1) * 16].iter_mut() {
                if *q > 0 && dec.icdf(tab, 8) == 0 {
                    *q = -*q;
                }
            }
        }
    }
}

fn shell_decode(dec: &mut RangeDecoder, out: &mut [i32], total: i32) {
    fn split(dec: &mut RangeDecoder, out: &mut [i32], n: usize, count: i32) {
        if n == 1 {
            out[0] = count;
            return;
        }
        if count == 0 {
            out[..n].fill(0);
            return;
        }
        let tab = match n {
            16 => SHELL_16[(count - 1) as usize],
            8 => SHELL_8[(count - 1) as usize],
            4 => SHELL_4[(count - 1) as usize],
            _ => SHELL_2[(count - 1) as usize],
        };
        let left = dec.icdf(tab, 8) as i32;
        let h = n / 2;
        let (a, b) = out.split_at_mut(h);
        split(dec, a, h, left);
        split(dec, b, h, count - left);
    }
    split(dec, out, 16, total.min(16));
}

#[derive(Default, Clone)]
struct Stereo {
    pred_prev_q13: [i32; 2],
    s_mid: [i16; 2],
    s_side: [i16; 2],
}

fn stereo_decode_pred(dec: &mut RangeDecoder) -> [i32; 2] {
    let n = dec.icdf(&STEREO_PRED_JOINT, 8);
    let mut ix = [[0usize; 3]; 2];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for v in ix.iter_mut() {
        v[0] = dec.icdf(&UNIFORM3, 8);
        v[1] = dec.icdf(&UNIFORM5, 8);
    }
    let mut pred = [0i32; 2];
    for (p, v) in pred.iter_mut().zip(ix.iter_mut()) {
        v[0] += 3 * v[2];
        let low = STEREO_WEIGHTS_Q13[v[0]];
        let step = smulwb(STEREO_WEIGHTS_Q13[v[0] + 1] - low, 6554);
        *p = low + step * (2 * v[1] as i32 + 1);
    }
    pred[0] -= pred[1];
    pred
}

fn stereo_ms_to_lr(st: &mut Stereo, x1: &mut [i16], x2: &mut [i16], pred_q13: [i32; 2], fs_khz: usize, len: usize) {
    x1[..2].copy_from_slice(&st.s_mid);
    x2[..2].copy_from_slice(&st.s_side);
    st.s_mid.copy_from_slice(&x1[len..len + 2]);
    st.s_side.copy_from_slice(&x2[len..len + 2]);
    let mut p0 = st.pred_prev_q13[0];
    let mut p1 = st.pred_prev_q13[1];
    let denom_q16 = (1 << 16) / (8 * fs_khz as i32);
    let d0 = rshift_round((pred_q13[0] - st.pred_prev_q13[0]) as i16 as i32 * denom_q16 as i16 as i32, 16);
    let d1 = rshift_round((pred_q13[1] - st.pred_prev_q13[1]) as i16 as i32 * denom_q16 as i16 as i32, 16);
    let interp = 8 * fs_khz;
    for n in 0..len {
        if n < interp {
            p0 += d0;
            p1 += d1;
        } else if n == interp {
            p0 = pred_q13[0];
            p1 = pred_q13[1];
        }
        let sum = ((x1[n] as i32 + x1[n + 2] as i32) + ((x1[n + 1] as i32) << 1)) << 9;
        let mut s = smlawb((x2[n + 1] as i32) << 8, sum, p0);
        s = smlawb(s, (x1[n + 1] as i32) << 11, p1);
        x2[n + 1] = sat16(rshift_round(s, 8)) as i16;
    }
    st.pred_prev_q13 = pred_q13;
    for n in 0..len {
        let a = x1[n + 1] as i32;
        let b = x2[n + 1] as i32;
        x1[n + 1] = sat16(a + b) as i16;
        x2[n + 1] = sat16(a - b) as i16;
    }
}

/// SILK decoder for up to two internal channels (`silk_decoder`).
pub struct SilkDecoder {
    api_rate: u32,
    api_channels: usize,
    ch: [Channel; 2],
    stereo: Stereo,
    channels_internal: usize,
    channels_internal_prev: usize,
    internal_rate: u32,
    prev_decode_only_middle: bool,
    ms_pred: [i32; 2],
    decode_only_middle: bool,
}

impl SilkDecoder {
    pub fn new(api_rate: u32, api_channels: usize) -> SilkDecoder {
        SilkDecoder {
            api_rate,
            api_channels,
            ch: [Channel::new(), Channel::new()],
            stereo: Stereo::default(),
            channels_internal: api_channels,
            channels_internal_prev: 0,
            internal_rate: 16000,
            prev_decode_only_middle: false,
            ms_pred: [0; 2],
            decode_only_middle: false,
        }
    }

    pub fn reset(&mut self) {
        self.ch = [Channel::new(), Channel::new()];
        self.stereo = Stereo::default();
        self.prev_decode_only_middle = false;
        self.channels_internal_prev = 0;
    }

    pub fn set_stream(&mut self, channels: usize, internal_rate: u32) {
        self.channels_internal = channels;
        self.internal_rate = internal_rate;
    }

    /// Decodes one SILK frame (10 or 20 ms) into interleaved `out` at the API rate.
    pub fn decode(&mut self, mut dec: Option<&mut RangeDecoder>, lost: LostFlag, new_packet: bool, payload_ms: usize, out: &mut [f32]) -> Result<usize> {
        let nci = self.channels_internal;
        if new_packet {
            for c in self.ch.iter_mut() {
                c.n_frames_decoded = 0;
            }
        }
        if nci > self.channels_internal_prev && self.channels_internal_prev != 0 {
            self.ch[1] = Channel::new();
        }
        let stereo_to_mono = nci == 1 && self.channels_internal_prev == 2 && self.internal_rate == 1000 * self.ch[0].fs_khz as u32;
        if self.ch[0].n_frames_decoded == 0 {
            for c in self.ch.iter_mut().take(nci) {
                let (nfpp, nb) = match payload_ms {
                    10 => (1, 2),
                    20 => (1, 4),
                    40 => (2, 4),
                    60 => (3, 4),
                    _ => return Err(Error::InvalidPacket("bad SILK frame size")),
                };
                c.n_frames_per_packet = nfpp;
                c.nb_subfr = nb;
                let fs_khz = ((self.internal_rate >> 10) + 1) as usize;
                c.set_fs(fs_khz, self.api_rate);
            }
        }
        if self.api_channels == 2 && nci == 2 && self.channels_internal_prev == 1 {
            self.stereo.pred_prev_q13 = [0; 2];
            self.stereo.s_side = [0; 2];
            self.ch[1].resampler = self.ch[0].resampler.clone();
        }
        self.channels_internal_prev = nci;

        if lost != LostFlag::Lost && self.ch[0].n_frames_decoded == 0 {
            let d = dec.as_deref_mut().ok_or(Error::InvalidPacket("missing SILK data"))?;
            for c in self.ch.iter_mut().take(nci) {
                for i in 0..c.n_frames_per_packet {
                    c.vad_flags[i] = d.bit_logp(1);
                }
                c.lbrr_flag = d.bit_logp(1);
            }
            for c in self.ch.iter_mut().take(nci) {
                c.lbrr_flags = [false; 3];
                if c.lbrr_flag {
                    if c.n_frames_per_packet == 1 {
                        c.lbrr_flags[0] = true;
                    } else {
                        let sym = d.icdf(LBRR_FLAGS[c.n_frames_per_packet - 2], 8) + 1;
                        for i in 0..c.n_frames_per_packet {
                            c.lbrr_flags[i] = (sym >> i) & 1 != 0;
                        }
                    }
                }
            }
            if lost == LostFlag::Normal {
                // Skip LBRR data.
                for i in 0..self.ch[0].n_frames_per_packet {
                    for n in 0..nci {
                        if self.ch[n].lbrr_flags[i] {
                            if nci == 2 && n == 0 {
                                stereo_decode_pred(d);
                                if !self.ch[1].lbrr_flags[i] {
                                    d.icdf(&MID_ONLY, 8);
                                }
                            }
                            let cond = if i > 0 && self.ch[n].lbrr_flags[i - 1] { Cond::Conditionally } else { Cond::Independently };
                            let c = &mut self.ch[n];
                            c.decode_indices(d, i, true, cond);
                            let mut pulses = [0i32; MAX_FRAME + 16];
                            decode_pulses(d, &mut pulses, c.indices.signal_type, c.indices.quant_offset_type, c.frame_length);
                        }
                    }
                }
            }
        }
        // Stereo prediction.
        if nci == 2 {
            let fi = self.ch[0].n_frames_decoded;
            let coded = lost == LostFlag::Normal || (lost == LostFlag::Lbrr && self.ch[0].lbrr_flags[fi]);
            match dec.as_deref_mut() {
                Some(d) if coded => {
                    self.ms_pred = stereo_decode_pred(d);
                    let side_inactive = (lost == LostFlag::Normal && !self.ch[1].vad_flags[fi]) || (lost == LostFlag::Lbrr && !self.ch[1].lbrr_flags[fi]);
                    self.decode_only_middle = if side_inactive { d.icdf(&MID_ONLY, 8) == 1 } else { false };
                }
                _ => {
                    self.ms_pred = self.stereo.pred_prev_q13;
                }
            }
        }
        if nci == 2 && !self.decode_only_middle && self.prev_decode_only_middle {
            let c = &mut self.ch[1];
            c.out_buf = [0; OUT_BUF];
            c.s_lpc_q14 = [0; MAX_LPC_ORDER];
            c.lag_prev = 100;
            c.last_gain_index = 10;
            c.prev_signal_type = 0;
            c.first_frame_after_reset = true;
        }
        let l = self.ch[0].frame_length;
        let mut buf = [[0i16; MAX_FRAME + 2]; 2];
        let has_side = if lost == LostFlag::Normal {
            !self.decode_only_middle
        } else {
            !self.prev_decode_only_middle || (nci == 2 && lost == LostFlag::Lbrr && self.ch[1].lbrr_flags[self.ch[1].n_frames_decoded])
        };
        for n in 0..nci {
            if n == 0 || has_side {
                let frame_index = self.ch[0].n_frames_decoded as isize - n as isize;
                let cond = if frame_index <= 0 {
                    Cond::Independently
                } else if lost == LostFlag::Lbrr {
                    if self.ch[n].lbrr_flags[frame_index as usize - 1] { Cond::Conditionally } else { Cond::Independently }
                } else if n > 0 && self.prev_decode_only_middle {
                    Cond::IndependentlyNoLtpScaling
                } else {
                    Cond::Conditionally
                };
                let c = &mut self.ch[n];
                c.decode_frame(dec.as_deref_mut(), &mut buf[n][2..], lost, cond);
            } else {
                buf[n][2..2 + l].fill(0);
            }
            self.ch[n].n_frames_decoded += 1;
        }
        if self.api_channels == 2 && nci == 2 {
            let (a, b) = buf.split_at_mut(1);
            stereo_ms_to_lr(&mut self.stereo, &mut a[0], &mut b[0], self.ms_pred, self.ch[0].fs_khz, l);
        } else {
            buf[0][..2].copy_from_slice(&self.stereo.s_mid);
            self.stereo.s_mid.copy_from_slice(&buf[0][l..l + 2]);
        }
        let n_out = l * self.api_rate as usize / (self.ch[0].fs_khz * 1000);
        let ach = self.api_channels;
        let mut tmp = Vec::with_capacity(n_out);
        for n in 0..ach.min(nci) {
            tmp.clear();
            self.ch[n].resampler.process(&buf[n][1..1 + l], &mut tmp);
            for (i, &v) in tmp.iter().enumerate().take(n_out) {
                out[i * ach + n] = v / 32768.0;
            }
        }
        if ach == 2 && nci == 1 {
            if stereo_to_mono {
                tmp.clear();
                self.ch[1].resampler.process(&buf[0][1..1 + l], &mut tmp);
                for (i, &v) in tmp.iter().enumerate().take(n_out) {
                    out[i * 2 + 1] = v / 32768.0;
                }
            } else {
                for i in 0..n_out {
                    out[i * 2 + 1] = out[i * 2];
                }
            }
        }
        if lost == LostFlag::Lost {
            for c in self.ch.iter_mut().take(nci) {
                c.last_gain_index = 10;
            }
        } else {
            self.prev_decode_only_middle = self.decode_only_middle;
        }
        Ok(n_out)
    }
}
