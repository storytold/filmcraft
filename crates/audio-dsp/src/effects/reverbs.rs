//! Convolution Reverb (uniformly partitioned overlap-save convolution with impulse responses
//! we generate ourselves) and Surround Reverb (early reflections + the FDN late field).

use super::time::Reverb;
use super::util::{DelayLine, OnePole, Prng};
use crate::fft::Fft;
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

/// Partition (block) size; the effect's latency.
const PART: usize = 256;

/// Built-in impulse responses (generated, see [`generate_ir`]).
pub const IMPULSES: &[&str] = &["Small Room", "Medium Room", "Large Hall", "Cathedral", "Plate", "Ambience", "Vocal Booth"];

/// Character of a generated impulse.
#[derive(Clone, Copy, Debug)]
struct IrSpec {
    rt60: f32,
    /// Early-reflection span (ms) and count.
    er_ms: f32,
    er_count: usize,
    /// How fast the high frequencies die relative to the low ones (1/s).
    hf_decay: f32,
    /// Late-field onset (ms).
    onset_ms: f32,
    seed: u64,
}

const IR_SPECS: [IrSpec; 7] = [
    IrSpec { rt60: 0.45, er_ms: 25.0, er_count: 10, hf_decay: 6.0, onset_ms: 8.0, seed: 11 },
    IrSpec { rt60: 0.8, er_ms: 45.0, er_count: 14, hf_decay: 4.0, onset_ms: 15.0, seed: 12 },
    IrSpec { rt60: 1.9, er_ms: 80.0, er_count: 18, hf_decay: 2.2, onset_ms: 30.0, seed: 13 },
    IrSpec { rt60: 3.2, er_ms: 120.0, er_count: 22, hf_decay: 1.4, onset_ms: 45.0, seed: 14 },
    IrSpec { rt60: 1.6, er_ms: 0.0, er_count: 0, hf_decay: 1.0, onset_ms: 1.0, seed: 15 },
    IrSpec { rt60: 0.3, er_ms: 15.0, er_count: 8, hf_decay: 8.0, onset_ms: 4.0, seed: 16 },
    IrSpec { rt60: 0.2, er_ms: 8.0, er_count: 6, hf_decay: 12.0, onset_ms: 2.0, seed: 17 },
];

fn ir_len(spec: &IrSpec, sr: f32) -> usize {
    ((spec.rt60 * 1.1 + spec.er_ms * 0.001) * sr) as usize + 1
}

/// Fill `out` (stereo, equal lengths) with a synthetic impulse response: sparse early
/// reflections plus exponentially decaying noise whose bandwidth narrows over time (high
/// frequencies decay faster), fading in over the onset, each channel normalised to unit energy.
/// Deterministic for a given index and sample rate.
fn generate_ir(index: usize, sr: f32, out: [&mut [f32]; 2]) {
    let spec = IR_SPECS[index.min(IR_SPECS.len() - 1)];
    for (c, buf) in out.into_iter().enumerate() {
        let len = buf.len();
        let mut rng = Prng::new(spec.seed * 2 + c as u64);
        let mut lp = OnePole::default();
        let onset = (spec.onset_ms * 0.001 * sr).max(1.0);
        for (i, v) in buf.iter_mut().enumerate() {
            let t = i as f32 / sr;
            let env = (-6.907_755 * t / spec.rt60).exp();
            let fade = (i as f32 / onset).min(1.0);
            let fc = (16000.0 * (-t * spec.hf_decay).exp()).max(800.0);
            let a = OnePole::alpha(fc, sr);
            *v = lp.lp(a, rng.uniform()) * env * fade;
        }
        for _ in 0..spec.er_count {
            let t = 0.002 + (rng.uniform() * 0.5 + 0.5) * spec.er_ms * 0.001;
            let i = ((t * sr) as usize).min(len - 1);
            let amp = 0.6 * (-3.0 * t / (spec.er_ms * 0.001 + 1e-3)).exp() * if rng.uniform() < 0.0 { -1.0 } else { 1.0 };
            buf[i] += amp;
        }
        let e: f64 = buf.iter().map(|&v| (v as f64) * (v as f64)).sum();
        let g = if e > 0.0 { (1.0 / e.sqrt()) as f32 } else { 0.0 };
        buf.iter_mut().for_each(|v| *v *= g);
    }
}

/// Convolution reverb with generated impulses (mono-summed input, decorrelated stereo IR).
pub struct ConvolutionReverb {
    pv: ParamValues,
    sr: f32,
    fft: Fft,
    /// Base impulse (current preset) and the processed one (size, damping) per output channel.
    base: [Vec<f32>; 2],
    base_len: usize,
    /// Partition spectra (B+1 bins, re/im interleaved per partition): `h[ch][p * 2(B+1) ..]`.
    h: [Vec<f32>; 2],
    parts: usize,
    /// Frequency-domain delay line of input spectra (same layout), ring of `max_parts`.
    fdl: Vec<f32>,
    head: usize,
    max_parts: usize,
    /// Time-domain input history (2B) and block in/out buffers.
    hist: Vec<f32>,
    inbuf: Vec<f32>,
    out: [Vec<f32>; 2],
    pos: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    pre: DelayLine,
    dry_delay: Vec<DelayLine>,
    key: (usize, f32, f32, f32),
    /// Impulse, size or damping changed since the last rebuild; rebuilt at the next `process`.
    dirty: bool,
    mix: Smoothed,
    gain: Smoothed,
    width: Smoothed,
    predelay: Smoothed,
}

impl ConvolutionReverb {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::choice("impulse", "Impulse", IMPULSES, 1),
        ParamSpec::new("mix", "Mix", 0.0, 100.0, 30.0, Unit::Percent),
        ParamSpec::new("room_size", "Room Size", 10.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::log("damping_lf", "Damping LF", 10.0, 1000.0, 10.0, Unit::Hertz),
        ParamSpec::log("damping_hf", "Damping HF", 1000.0, 20000.0, 20000.0, Unit::Hertz),
        ParamSpec::new("predelay", "Pre-Delay", 0.0, 500.0, 0.0, Unit::Milliseconds),
        ParamSpec::new("width", "Width", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("gain", "Gain", -24.0, 12.0, 0.0, Unit::Decibels),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let max_len = IR_SPECS.iter().map(|s| ir_len(s, sample_rate)).max().unwrap_or(1);
        let max_parts = max_len.div_ceil(PART);
        let bins2 = 2 * (PART + 1);
        let mut s = ConvolutionReverb {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            fft: Fft::new(2 * PART),
            base: [vec![0.0; max_len], vec![0.0; max_len]],
            base_len: 0,
            h: [vec![0.0; max_parts * bins2], vec![0.0; max_parts * bins2]],
            parts: 0,
            fdl: vec![0.0; max_parts * bins2],
            head: 0,
            max_parts,
            hist: vec![0.0; 2 * PART],
            inbuf: vec![0.0; PART],
            out: [vec![0.0; PART], vec![0.0; PART]],
            pos: 0,
            re: vec![0.0; 2 * PART],
            im: vec![0.0; 2 * PART],
            pre: DelayLine::new((0.5 * sample_rate) as usize + 2),
            dry_delay: vec![DelayLine::new(PART + 2); channels.max(1)],
            key: (usize::MAX, 0.0, 0.0, 0.0),
            dirty: false,
            mix: Smoothed::with_ms(0.0, sample_rate, 20.0),
            gain: Smoothed::with_ms(1.0, sample_rate, 20.0),
            width: Smoothed::with_ms(1.0, sample_rate, 20.0),
            predelay: Smoothed::with_ms(0.0, sample_rate, 50.0),
        };
        s.apply_params(true);
        s
    }

    /// Rebuild the partition spectra (only when the impulse, size or damping changed).
    fn rebuild(&mut self) {
        let key = (self.pv.idx("impulse"), self.pv.v("room_size"), self.pv.v("damping_lf"), self.pv.v("damping_hf"));
        if key == self.key {
            return;
        }
        if key.0 != self.key.0 {
            let len = ir_len(&IR_SPECS[key.0.min(IR_SPECS.len() - 1)], self.sr).min(self.base[0].len());
            let [a, b] = &mut self.base;
            generate_ir(key.0, self.sr, [&mut a[..len], &mut b[..len]]);
            self.base_len = len;
        }
        self.key = key;
        // Room size shortens the impulse by resampling it (linear interpolation).
        let scale = (key.1 / 100.0).clamp(0.1, 1.0);
        let len = ((self.base_len as f32 * scale) as usize).max(1);
        self.parts = len.div_ceil(PART).min(self.max_parts);
        let (alf, ahf) = (OnePole::alpha(key.2, self.sr), OnePole::alpha(key.3, self.sr));
        let bins2 = 2 * (PART + 1);
        for c in 0..2 {
            let mut hp = OnePole::default();
            let mut lp = OnePole::default();
            for p in 0..self.parts {
                for k in 0..2 * PART {
                    self.re[k] = 0.0;
                    self.im[k] = 0.0;
                }
                for k in 0..PART {
                    let i = p * PART + k;
                    if i >= len {
                        break;
                    }
                    let src = i as f32 / scale;
                    let i0 = src as usize;
                    let fr = src - i0 as f32;
                    let b = &self.base[c];
                    let v0 = b.get(i0).copied().unwrap_or(0.0);
                    let v1 = b.get(i0 + 1).copied().unwrap_or(0.0);
                    let mut v = (v0 + (v1 - v0) * fr) * scale.sqrt().recip();
                    if key.2 > 10.5 {
                        v = hp.hp(alf, v);
                    }
                    if key.3 < 19999.0 {
                        v = lp.lp(ahf, v);
                    }
                    self.re[k] = v;
                }
                self.fft.forward(&mut self.re, &mut self.im);
                let dst = &mut self.h[c][p * bins2..(p + 1) * bins2];
                for k in 0..=PART {
                    dst[2 * k] = self.re[k];
                    dst[2 * k + 1] = self.im[k];
                }
            }
        }
    }

    fn apply_params(&mut self, snap: bool) {
        if snap {
            self.rebuild();
        } else {
            self.dirty = true;
        }
        self.mix.set(self.pv.v("mix") / 100.0);
        self.gain.set(db_to_gain(self.pv.v("gain")));
        self.width.set(self.pv.v("width") / 100.0);
        self.predelay.set(self.pv.v("predelay") * 0.001 * self.sr);
        if snap {
            self.mix.snap();
            self.gain.snap();
            self.width.snap();
            self.predelay.snap();
        }
    }

    /// Convolve the collected block (`inbuf`) and fill `out`.
    fn block(&mut self) {
        let bins2 = 2 * (PART + 1);
        self.hist.copy_within(PART.., 0);
        self.hist[PART..].copy_from_slice(&self.inbuf);
        self.re.copy_from_slice(&self.hist);
        self.im.iter_mut().for_each(|v| *v = 0.0);
        self.fft.forward(&mut self.re, &mut self.im);
        self.head = (self.head + self.max_parts - 1) % self.max_parts;
        {
            let slot = &mut self.fdl[self.head * bins2..(self.head + 1) * bins2];
            for k in 0..=PART {
                slot[2 * k] = self.re[k];
                slot[2 * k + 1] = self.im[k];
            }
        }
        // Accumulate Y_L and Y_R (half spectra) then pack Z = Y_L + j·Y_R for one inverse FFT.
        let mut yl = [0.0f32; 2 * (PART + 1)];
        let mut yr = [0.0f32; 2 * (PART + 1)];
        for p in 0..self.parts {
            let xi = ((self.head + p) % self.max_parts) * bins2;
            let x = &self.fdl[xi..xi + bins2];
            let hl = &self.h[0][p * bins2..(p + 1) * bins2];
            let hr = &self.h[1][p * bins2..(p + 1) * bins2];
            for k in 0..=PART {
                let (xr, xim) = (x[2 * k], x[2 * k + 1]);
                yl[2 * k] += xr * hl[2 * k] - xim * hl[2 * k + 1];
                yl[2 * k + 1] += xr * hl[2 * k + 1] + xim * hl[2 * k];
                yr[2 * k] += xr * hr[2 * k] - xim * hr[2 * k + 1];
                yr[2 * k + 1] += xr * hr[2 * k + 1] + xim * hr[2 * k];
            }
        }
        let n = 2 * PART;
        for k in 0..n {
            let (lr, li, rr, ri) = if k <= PART {
                (yl[2 * k], yl[2 * k + 1], yr[2 * k], yr[2 * k + 1])
            } else {
                let m = n - k;
                (yl[2 * m], -yl[2 * m + 1], yr[2 * m], -yr[2 * m + 1])
            };
            // Z = Y_L + j·Y_R
            self.re[k] = lr - ri;
            self.im[k] = li + rr;
        }
        self.fft.inverse(&mut self.re, &mut self.im);
        let s = 1.0 / n as f32;
        for k in 0..PART {
            self.out[0][k] = self.re[PART + k] * s;
            self.out[1][k] = self.im[PART + k] * s;
        }
    }
}

impl AudioEffect for ConvolutionReverb {
    param_plumbing!("convolution_reverb");
    fn latency(&self) -> usize {
        PART
    }
    fn reset(&mut self) {
        self.fdl.iter_mut().for_each(|v| *v = 0.0);
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.inbuf.iter_mut().for_each(|v| *v = 0.0);
        self.out.iter_mut().for_each(|o| o.iter_mut().for_each(|v| *v = 0.0));
        self.head = 0;
        self.pos = 0;
        self.pre.reset();
        self.dry_delay.iter_mut().for_each(DelayLine::reset);
        self.mix.snap();
        self.gain.snap();
        self.width.snap();
        self.predelay.snap();
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.dry_delay.len());
        if nch == 0 {
            return;
        }
        if self.dirty {
            self.dirty = false;
            self.rebuild();
        }
        for i in 0..n {
            let (mix, gain, width, pd) = (self.mix.tick(), self.gain.tick(), self.width.tick(), self.predelay.tick());
            let mono = channels[..nch].iter().map(|c| c[i]).sum::<f32>() / nch as f32;
            self.pre.push(mono);
            self.inbuf[self.pos] = self.pre.read(pd);
            let (wl, wr) = (self.out[0][self.pos], self.out[1][self.pos]);
            self.pos += 1;
            if self.pos == PART {
                self.pos = 0;
                self.block();
            }
            // Width: 0 = mono wet, 1 = full decorrelated stereo.
            let m = 0.5 * (wl + wr);
            let (wl, wr) = (m + width * (wl - m), m + width * (wr - m));
            for ch in 0..nch {
                self.dry_delay[ch].push(channels[ch][i]);
                let dry = self.dry_delay[ch].tap(PART);
                let wet = if nch == 1 {
                    m
                } else if ch == 0 {
                    wl
                } else {
                    wr
                };
                channels[ch][i] = dry + mix * (wet * gain - dry);
            }
        }
    }
}

// ------------------------------------------------------------------------- surround reverb

const ER_TAPS: usize = 8;
const ER_MS: [f32; ER_TAPS] = [7.1, 11.3, 15.7, 19.9, 24.2, 31.7, 38.3, 47.9];
const ER_GAIN: [f32; ER_TAPS] = [0.8, -0.7, 0.62, -0.55, 0.47, -0.4, 0.33, -0.27];
const CHUNK: usize = 256;

/// Early reflections (8 taps, alternating sides) plus the 8-line FDN late field, with
/// low/high cut on the wet signal, wet width, and separate dry/wet levels. The centre input
/// level controls how much of the mid (L+R) signal feeds the reverb.
pub struct SurroundReverb {
    pv: ParamValues,
    sr: f32,
    late: Reverb,
    er: DelayLine,
    lc: [OnePole; 2],
    hc: [OnePole; 2],
    scratch: [[f32; CHUNK]; 2],
    dry: Smoothed,
    wet: Smoothed,
    early: Smoothed,
    width: Smoothed,
    centre: Smoothed,
    size: Smoothed,
}

impl SurroundReverb {
    pub const PARAMS: &'static [ParamSpec] = &[
        ParamSpec::new("center_input", "Center Input", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("room_size", "Room Size", 0.0, 100.0, 50.0, Unit::Percent),
        ParamSpec::log("decay", "Decay", 0.1, 20.0, 1.5, Unit::Seconds),
        ParamSpec::new("predelay", "Pre-Delay", 0.0, 200.0, 20.0, Unit::Milliseconds),
        ParamSpec::new("damping", "Damping", 0.0, 100.0, 40.0, Unit::Percent),
        ParamSpec::new("early", "Early Reflections", 0.0, 100.0, 40.0, Unit::Percent),
        ParamSpec::log("low_cut", "Low Frequency Cut", 20.0, 1000.0, 20.0, Unit::Hertz),
        ParamSpec::log("high_cut", "High Frequency Cut", 1000.0, 20000.0, 12000.0, Unit::Hertz),
        ParamSpec::new("width", "Width", 0.0, 100.0, 100.0, Unit::Percent),
        ParamSpec::new("dry", "Dry", 0.0, 100.0, 80.0, Unit::Percent),
        ParamSpec::new("wet", "Wet", 0.0, 100.0, 30.0, Unit::Percent),
    ];

    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut late = Reverb::new(sample_rate, channels);
        late.set_param("mix", 100.0);
        let sm = |v| Smoothed::with_ms(v, sample_rate, 20.0);
        let mut s = SurroundReverb {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            late,
            er: DelayLine::new((0.1 * sample_rate) as usize),
            lc: [OnePole::default(); 2],
            hc: [OnePole::default(); 2],
            scratch: [[0.0; CHUNK]; 2],
            dry: sm(1.0),
            wet: sm(0.0),
            early: sm(0.0),
            width: sm(1.0),
            centre: sm(1.0),
            size: sm(1.0),
        };
        s.apply_params(true);
        s
    }

    fn apply_params(&mut self, snap: bool) {
        self.late.set_param("size", self.pv.v("room_size"));
        self.late.set_param("decay", self.pv.v("decay"));
        self.late.set_param("predelay", self.pv.v("predelay"));
        self.late.set_param("damping", self.pv.v("damping"));
        self.dry.set(self.pv.v("dry") / 100.0);
        self.wet.set(self.pv.v("wet") / 100.0);
        self.early.set(self.pv.v("early") / 100.0);
        self.width.set(self.pv.v("width") / 100.0);
        self.centre.set(self.pv.v("center_input") / 100.0);
        self.size.set(0.5 + self.pv.v("room_size") / 100.0);
        if snap {
            self.late.reset();
            for s in [&mut self.dry, &mut self.wet, &mut self.early, &mut self.width, &mut self.centre, &mut self.size] {
                s.snap();
            }
        }
    }
}

impl AudioEffect for SurroundReverb {
    param_plumbing!("surround_reverb");
    fn reset(&mut self) {
        self.late.reset();
        self.er.reset();
        self.lc = [OnePole::default(); 2];
        self.hc = [OnePole::default(); 2];
        for s in [&mut self.dry, &mut self.wet, &mut self.early, &mut self.width, &mut self.centre, &mut self.size] {
            s.snap();
        }
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(2);
        if nch == 0 {
            return;
        }
        let (alc, ahc) = (OnePole::alpha(self.pv.v("low_cut"), self.sr), OnePole::alpha(self.pv.v("high_cut"), self.sr));
        let ms = self.sr * 0.001;
        let mut start = 0;
        while start < n {
            let len = (n - start).min(CHUNK);
            // Reverb input: side signal plus the centre-weighted mid.
            for i in 0..len {
                let (l, r) = if nch == 2 { (channels[0][start + i], channels[1][start + i]) } else { (channels[0][start + i], channels[0][start + i]) };
                let c = self.centre.tick();
                let (mid, side) = (0.5 * (l + r), 0.5 * (l - r));
                self.scratch[0][i] = c * mid + side;
                self.scratch[1][i] = c * mid - side;
            }
            {
                let [a, b] = &mut self.scratch;
                self.late.process(&mut [&mut a[..len], &mut b[..len]]);
            }
            for i in 0..len {
                let x = if nch == 2 { 0.5 * (channels[0][start + i] + channels[1][start + i]) } else { channels[0][start + i] };
                let size = self.size.tick();
                self.er.push(x);
                let (mut el, mut er) = (0.0, 0.0);
                for k in 0..ER_TAPS {
                    let v = ER_GAIN[k] * self.er.read(ER_MS[k] * size * ms);
                    if k % 2 == 0 { el += v } else { er += v }
                }
                let early = self.early.tick();
                let (dry, wet, width) = (self.dry.tick(), self.wet.tick(), self.width.tick());
                let mut w = [self.scratch[0][i] + early * el, self.scratch[1][i] + early * er];
                for c in 0..2 {
                    w[c] = self.hc[c].lp(ahc, self.lc[c].hp(alc, w[c]));
                }
                let m = 0.5 * (w[0] + w[1]);
                let (wl, wr) = (m + width * (w[0] - m), m + width * (w[1] - m));
                if nch == 2 {
                    channels[0][start + i] = dry * channels[0][start + i] + wet * wl;
                    channels[1][start + i] = dry * channels[1][start + i] + wet * wr;
                } else {
                    channels[0][start + i] = dry * channels[0][start + i] + wet * m;
                }
            }
            start += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn set_param_defers_the_impulse_rebuild_to_process() {
        let mut fx = ConvolutionReverb::new(48_000.0, 2);
        let key = fx.key;
        fx.set_param("damping_hf", 8000.0);
        fx.set_param("room_size", 60.0);
        assert_eq!(fx.key, key, "set_param rebuilt the impulse");
        fx.process(&mut [&mut [0.0; 16][..], &mut [0.0; 16][..]]);
        assert_eq!(fx.key, (key.0, 60.0, key.2, 8000.0));
        assert!(!fx.dirty);
    }

    #[test]
    fn deferred_rebuild_output_is_identical() {
        let changes: &[&[(&str, f32)]] = &[
            &[("room_size", 70.0)],
            &[],
            &[("damping_lf", 200.0), ("damping_hf", 6000.0)],
            &[("impulse", 2.0), ("mix", 80.0)],
            &[("room_size", 30.0), ("room_size", 90.0)],
            &[("impulse", 4.0), ("damping_hf", 20_000.0), ("damping_lf", 10.0)],
        ];
        let mut eager = ConvolutionReverb::new(48_000.0, 2);
        let mut deferred = ConvolutionReverb::new(48_000.0, 2);
        let mut rng = Rng::new(5);
        for (block, set) in changes.iter().cycle().take(24).enumerate() {
            for &(id, v) in *set {
                eager.set_param(id, v);
                eager.rebuild();
                deferred.set_param(id, v);
            }
            let n = 300 + 37 * block;
            let l: Vec<f32> = (0..n).map(|_| rng.uniform()).collect();
            let r: Vec<f32> = (0..n).map(|_| rng.uniform()).collect();
            let (mut l1, mut r1, mut l2, mut r2) = (l.clone(), r.clone(), l, r);
            eager.process(&mut [&mut l1, &mut r1]);
            deferred.process(&mut [&mut l2, &mut r2]);
            for (a, b) in l1.iter().chain(&r1).zip(l2.iter().chain(&r2)) {
                assert_eq!(a.to_bits(), b.to_bits(), "block {block}");
            }
        }
    }
}
