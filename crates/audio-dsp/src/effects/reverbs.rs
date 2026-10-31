//! Convolution Reverb (non-uniformly partitioned overlap-save convolution with impulse responses
//! we generate ourselves) and Surround Reverb (early reflections + the FDN late field).

use super::time::Reverb;
use super::util::{DelayLine, OnePole, Prng};
use crate::fft::Fft;
use crate::{AudioEffect, ParamSpec, ParamValues, Smoothed, Unit, block_len, db_to_gain, param_plumbing};

/// Partition (block) size of the head of the impulse; the effect's latency.
const PART: usize = 256;
/// Partitions of [`PART`] samples in the head stage.
const HEAD_PARTS: usize = 8;
/// Impulse samples handled by the head stage; the tail stage starts here.
const HEAD_LEN: usize = HEAD_PARTS * PART;
/// The tail stages after the head, each a `(partition size, partitions)` stretch of the impulse
/// (the last takes the rest). A stage's output for a block is ready one block after the block's
/// input, which is in time because its stretch starts at least a partition in.
const TAILS: [(usize, usize); 2] = [(2048, 3), (8192, usize::MAX)];
/// Blocks of input spectra a stage keeps beyond its partitions (see [`Stage::convolve`]).
const HISTORY: usize = 3;
/// Ring of tail output waiting for its sample time (a power of two above the largest start offset
/// plus latency plus partition: 8192 + 256 + 8192).
const TAIL_RING: usize = 32768;

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

/// One uniformly partitioned overlap-save convolution stage: `part`-sample input blocks against
/// the stereo partition spectra of one stretch of the impulse, via a frequency-domain delay line.
struct Stage {
    part: usize,
    fft: Fft,
    /// Partition spectra (`part + 1` bins, re/im interleaved per partition): `h[ch][p * 2(part+1) ..]`.
    h: [Vec<f32>; 2],
    parts: usize,
    /// Frequency-domain delay line of input spectra (same layout), ring of `cap`: `max_parts`
    /// plus [`HISTORY`] blocks, so the output of the last few blocks can be recomputed.
    fdl: Vec<f32>,
    head: usize,
    max_parts: usize,
    cap: usize,
    /// Time-domain input history (2 × part).
    hist: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    yl: Vec<f32>,
    yr: Vec<f32>,
    /// The latest block of output (`part` samples per channel).
    out: [Vec<f32>; 2],
}

impl Stage {
    fn new(part: usize, max_parts: usize) -> Self {
        let max_parts = max_parts.max(1);
        let bins2 = 2 * (part + 1);
        Stage {
            part,
            fft: Fft::new(2 * part),
            h: [vec![0.0; max_parts * bins2], vec![0.0; max_parts * bins2]],
            parts: 0,
            fdl: vec![0.0; (max_parts + HISTORY) * bins2],
            head: 0,
            max_parts,
            cap: max_parts + HISTORY,
            hist: vec![0.0; 2 * part],
            re: vec![0.0; 2 * part],
            im: vec![0.0; 2 * part],
            yl: vec![0.0; bins2],
            yr: vec![0.0; bins2],
            out: [vec![0.0; part], vec![0.0; part]],
        }
    }

    fn reset(&mut self) {
        self.fdl.iter_mut().for_each(|v| *v = 0.0);
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.out.iter_mut().for_each(|o| o.iter_mut().for_each(|v| *v = 0.0));
        self.head = 0;
    }

    /// Load the stretch of the impulse that starts at `ir[0]` (both channels), at most
    /// `max_parts` partitions of it.
    fn set_impulse(&mut self, ir: [&[f32]; 2]) {
        let bins2 = 2 * (self.part + 1);
        self.parts = ir[0].len().div_ceil(self.part).min(self.max_parts);
        for (c, ir) in ir.into_iter().enumerate() {
            for p in 0..self.parts {
                self.re.iter_mut().for_each(|v| *v = 0.0);
                self.im.iter_mut().for_each(|v| *v = 0.0);
                let chunk = ir.get(p * self.part..).unwrap_or_default();
                for (d, v) in self.re.iter_mut().zip(chunk.iter().take(self.part)) {
                    *d = *v;
                }
                self.fft.forward(&mut self.re, &mut self.im);
                let dst = &mut self.h[c][p * bins2..(p + 1) * bins2];
                for k in 0..=self.part {
                    dst[2 * k] = self.re[k];
                    dst[2 * k + 1] = self.im[k];
                }
            }
        }
    }

    /// Convolve one block of `part` input samples and fill `out`.
    fn run(&mut self, input: &[f32]) {
        let part = self.part;
        let bins2 = 2 * (part + 1);
        self.hist.copy_within(part.., 0);
        self.hist[part..].copy_from_slice(input);
        self.re.copy_from_slice(&self.hist);
        self.im.iter_mut().for_each(|v| *v = 0.0);
        self.fft.forward(&mut self.re, &mut self.im);
        self.head = (self.head + self.cap - 1) % self.cap;
        {
            let slot = &mut self.fdl[self.head * bins2..(self.head + 1) * bins2];
            for k in 0..=part {
                slot[2 * k] = self.re[k];
                slot[2 * k + 1] = self.im[k];
            }
        }
        self.convolve(0);
    }

    /// Fill `out` with the output of the block `back` blocks before the latest one, from the
    /// input spectra kept in the delay line and the current impulse.
    fn convolve(&mut self, back: usize) {
        let part = self.part;
        let bins2 = 2 * (part + 1);
        if self.parts == 0 {
            self.out.iter_mut().for_each(|o| o.iter_mut().for_each(|v| *v = 0.0));
            return;
        }
        // Accumulate Y_L and Y_R (half spectra) then pack Z = Y_L + j·Y_R for one inverse FFT.
        self.yl.iter_mut().for_each(|v| *v = 0.0);
        self.yr.iter_mut().for_each(|v| *v = 0.0);
        for p in 0..self.parts {
            let xi = ((self.head + back + p) % self.cap) * bins2;
            let x = &self.fdl[xi..xi + bins2];
            let hl = &self.h[0][p * bins2..(p + 1) * bins2];
            let hr = &self.h[1][p * bins2..(p + 1) * bins2];
            for k in 0..=part {
                let (xr, xim) = (x[2 * k], x[2 * k + 1]);
                self.yl[2 * k] += xr * hl[2 * k] - xim * hl[2 * k + 1];
                self.yl[2 * k + 1] += xr * hl[2 * k + 1] + xim * hl[2 * k];
                self.yr[2 * k] += xr * hr[2 * k] - xim * hr[2 * k + 1];
                self.yr[2 * k + 1] += xr * hr[2 * k + 1] + xim * hr[2 * k];
            }
        }
        let n = 2 * part;
        for k in 0..n {
            let (lr, li, rr, ri) = if k <= part {
                (self.yl[2 * k], self.yl[2 * k + 1], self.yr[2 * k], self.yr[2 * k + 1])
            } else {
                let m = n - k;
                (self.yl[2 * m], -self.yl[2 * m + 1], self.yr[2 * m], -self.yr[2 * m + 1])
            };
            // Z = Y_L + j·Y_R
            self.re[k] = lr - ri;
            self.im[k] = li + rr;
        }
        self.fft.inverse(&mut self.re, &mut self.im);
        let s = 1.0 / n as f32;
        for k in 0..part {
            self.out[0][k] = self.re[part + k] * s;
            self.out[1][k] = self.im[part + k] * s;
        }
    }
}

/// A stage past the head, with its block in buffer and where its stretch starts in the impulse.
struct TailStage {
    stage: Stage,
    /// Impulse sample the stretch starts at.
    offset: usize,
    /// Impulse samples the stage can hold.
    capacity: usize,
    buf: Vec<f32>,
    pos: usize,
    blocks: u64,
}

/// Convolution reverb with generated impulses (mono-summed input, decorrelated stereo IR).
///
/// The impulse is split in stages at the same latency: its first [`HEAD_LEN`] samples in
/// [`PART`]-sample partitions, then stretches in partitions of 2048 and 8192 samples ([`TAILS`]).
/// A long tail then costs a fraction of the multiply-accumulates that short partitions need
/// (they grow with the number of partitions), and the output is the same convolution.
pub struct ConvolutionReverb {
    pv: ParamValues,
    sr: f32,
    /// Base impulse (current preset) and the processed one (size, damping) per output channel.
    base: [Vec<f32>; 2],
    base_len: usize,
    ir: [Vec<f32>; 2],
    head: Stage,
    tails: Vec<TailStage>,
    inbuf: Vec<f32>,
    pos: usize,
    /// Output samples produced so far; the tails' output for a block waits in `tail_ring`.
    clock: u64,
    tail_ring: [Vec<f32>; 2],
    pre: DelayLine,
    dry_delay: Vec<DelayLine>,
    key: (usize, f32, f32, f32),
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
        let mut tails = Vec::new();
        let mut offset = HEAD_LEN;
        for (part, parts) in TAILS {
            let parts = parts.min(max_len.saturating_sub(offset).div_ceil(part));
            tails.push(TailStage { stage: Stage::new(part, parts), offset, capacity: parts * part, buf: vec![0.0; part], pos: 0, blocks: 0 });
            offset += parts * part;
        }
        let mut s = ConvolutionReverb {
            pv: ParamValues::new(Self::PARAMS),
            sr: sample_rate,
            base: [vec![0.0; max_len], vec![0.0; max_len]],
            base_len: 0,
            ir: [vec![0.0; max_len], vec![0.0; max_len]],
            head: Stage::new(PART, HEAD_PARTS),
            tails,
            inbuf: vec![0.0; PART],
            pos: 0,
            clock: 0,
            tail_ring: [vec![0.0; TAIL_RING], vec![0.0; TAIL_RING]],
            pre: DelayLine::new((0.5 * sample_rate) as usize + 2),
            dry_delay: vec![DelayLine::new(PART + 2); channels.max(1)],
            key: (usize::MAX, 0.0, 0.0, 0.0),
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
        let len = (((self.base_len as f32 * scale) as usize).max(1)).min(self.ir[0].len());
        let (alf, ahf) = (OnePole::alpha(key.2, self.sr), OnePole::alpha(key.3, self.sr));
        for c in 0..2 {
            let mut hp = OnePole::default();
            let mut lp = OnePole::default();
            let b = &self.base[c];
            for (i, out) in self.ir[c][..len].iter_mut().enumerate() {
                let src = i as f32 / scale;
                let i0 = src as usize;
                let fr = src - i0 as f32;
                let v0 = b.get(i0).copied().unwrap_or(0.0);
                let v1 = b.get(i0 + 1).copied().unwrap_or(0.0);
                let mut v = (v0 + (v1 - v0) * fr) * scale.sqrt().recip();
                if key.2 > 10.5 {
                    v = hp.hp(alf, v);
                }
                if key.3 < 19999.0 {
                    v = lp.lp(ahf, v);
                }
                *out = v;
            }
        }
        let [l, r] = &self.ir;
        let split = len.min(HEAD_LEN);
        self.head.set_impulse([&l[..split], &r[..split]]);
        // the head's block in progress was computed with the old impulse: its unplayed samples
        // are recomputed from the input spectra, as the queued tails are
        self.head.convolve(0);
        for t in &mut self.tails {
            let (from, to) = (t.offset.min(len), (t.offset + t.capacity).min(len));
            t.stage.set_impulse([&l[from..to], &r[from..to]]);
        }
        self.requeue_tails();
    }

    fn apply_params(&mut self, snap: bool) {
        self.rebuild();
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

    /// Replace the tail output queued with the old impulse by the same blocks recomputed with the
    /// new one, from the input spectra the stages kept: a change of size or damping while sound
    /// plays leaves no hole and no old response in the tail.
    fn requeue_tails(&mut self) {
        self.tail_ring.iter_mut().for_each(|c| c.iter_mut().for_each(|v| *v = 0.0));
        for t in &mut self.tails {
            let part = t.stage.part as u64;
            for back in 0..HISTORY {
                let Some(block) = t.blocks.checked_sub(1 + back as u64) else { break };
                let start = block * part + (t.offset + PART) as u64;
                if start + part <= self.clock {
                    break;
                }
                t.stage.convolve(back);
                for c in 0..2 {
                    for (k, v) in t.stage.out[c].iter().enumerate() {
                        let at = start + k as u64;
                        if at >= self.clock {
                            self.tail_ring[c][(at % TAIL_RING as u64) as usize] += *v;
                        }
                    }
                }
            }
        }
    }

    /// Tail stage `i` finished a block: its output belongs `offset + PART` samples after the
    /// block's start (where its stretch of the impulse starts plus the effect's latency).
    fn tail_block(&mut self, i: usize) {
        let Some(t) = self.tails.get_mut(i) else { return };
        t.stage.run(&t.buf);
        let start = t.blocks * t.stage.part as u64 + (t.offset + PART) as u64;
        for c in 0..2 {
            for (k, v) in t.stage.out[c].iter().enumerate() {
                let slot = ((start + k as u64) % TAIL_RING as u64) as usize;
                self.tail_ring[c][slot] += *v;
            }
        }
        t.blocks += 1;
    }
}

impl AudioEffect for ConvolutionReverb {
    param_plumbing!("convolution_reverb");
    fn latency(&self) -> usize {
        PART
    }
    fn reset(&mut self) {
        self.head.reset();
        for t in &mut self.tails {
            t.stage.reset();
            t.buf.iter_mut().for_each(|v| *v = 0.0);
            t.pos = 0;
            t.blocks = 0;
        }
        self.inbuf.iter_mut().for_each(|v| *v = 0.0);
        self.tail_ring.iter_mut().for_each(|o| o.iter_mut().for_each(|v| *v = 0.0));
        self.pos = 0;
        self.clock = 0;
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
        for i in 0..n {
            let (mix, gain, width, pd) = (self.mix.tick(), self.gain.tick(), self.width.tick(), self.predelay.tick());
            let mono = channels[..nch].iter().map(|c| c[i]).sum::<f32>() / nch as f32;
            self.pre.push(mono);
            let input = self.pre.read(pd);
            self.inbuf[self.pos] = input;
            for t in &mut self.tails {
                t.buf[t.pos] = input;
            }
            let slot = (self.clock % TAIL_RING as u64) as usize;
            let (mut wl, mut wr) = (self.head.out[0][self.pos], self.head.out[1][self.pos]);
            wl += std::mem::take(&mut self.tail_ring[0][slot]);
            wr += std::mem::take(&mut self.tail_ring[1][slot]);
            self.clock += 1;
            self.pos += 1;
            if self.pos == PART {
                self.pos = 0;
                self.head.run(&self.inbuf);
            }
            for i in 0..self.tails.len() {
                let full = self.tails.get_mut(i).is_some_and(|t| {
                    t.pos += 1;
                    t.pos == t.buf.len()
                });
                if full {
                    if let Some(t) = self.tails.get_mut(i) {
                        t.pos = 0;
                    }
                    self.tail_block(i);
                }
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

    const SR: f32 = 48_000.0;

    fn reverb(impulse: usize) -> ConvolutionReverb {
        let mut e = ConvolutionReverb::new(SR, 2);
        for (p, v) in [("impulse", impulse as f32), ("mix", 100.0), ("width", 100.0)] {
            assert!(e.set_param(p, v), "{p}");
        }
        e.reset();
        e
    }

    fn process(e: &mut ConvolutionReverb, input: &[f32]) -> [Vec<f32>; 2] {
        let (mut l, mut r) = (input.to_vec(), input.to_vec());
        // odd block sizes: the stages' block edges fall anywhere in a call
        for (a, b) in l.chunks_mut(509).zip(r.chunks_mut(509)) {
            e.process(&mut [a, b]);
        }
        [l, r]
    }

    /// The impulse response of the staged convolution is the generated impulse, delayed by the
    /// latency, however the impulse sits against the stages' block edges (#3134).
    #[test]
    fn staged_convolution_reproduces_the_impulse_response() {
        for (index, at) in [(1, 0), (3, 0), (3, 777), (3, 2048 + 255), (6, 5)] {
            let mut e = reverb(index);
            let len = ir_len(&IR_SPECS[index], SR);
            let (mut ir_l, mut ir_r) = (vec![0.0; len], vec![0.0; len]);
            generate_ir(index, SR, [&mut ir_l, &mut ir_r]);
            let mut input = vec![0.0f32; at + len + 2 * PART + TAIL_RING];
            input[at] = 1.0;
            let [l, r] = process(&mut e, &input);
            let lat = e.latency() + at;
            let worst = |out: &[f32], ir: &[f32]| ir.iter().enumerate().map(|(i, v)| (out[lat + i] - v).abs()).fold(0.0f32, f32::max);
            assert!(worst(&l, &ir_l) < 1e-5, "impulse {index} at {at}: left {}", worst(&l, &ir_l));
            assert!(worst(&r, &ir_r) < 1e-5, "impulse {index} at {at}: right {}", worst(&r, &ir_r));
            // and nothing after the impulse response ends
            assert!(l[lat + len..].iter().all(|v| v.abs() < 1e-5), "impulse {index} at {at}: tail past the end");
        }
    }

    /// A new impulse (or size, or damping) takes over completely and without a break: once the
    /// head has flushed, the output equals that of the new settings from the start, tail included.
    /// The tail output queued with the old impulse is replaced, not dropped (#3134).
    #[test]
    fn changing_size_during_sound_continues_as_if_it_had_always_been_so() {
        let mut rng = Prng::new(9);
        let noise: Vec<f32> = (0..70_000).map(|_| rng.uniform() * 0.5).collect();
        let mut fixed = reverb(3);
        assert!(fixed.set_param("room_size", 60.0));
        fixed.reset();
        let [want, _] = process(&mut fixed, &noise);
        for change_at in [20_000usize, 20_100, 33_333] {
            let mut e = reverb(3);
            process(&mut e, &noise[..0]);
            let [mut got, _] = process(&mut e, &noise[..change_at]);
            assert!(e.set_param("room_size", 60.0));
            got.extend(&process(&mut e, &noise[change_at..])[0]);
            // after the head's block and the old head's tail ring out, nothing of the old setting is left
            let from = change_at + 2 * PART + 1;
            let worst = (from..noise.len()).map(|i| (got[i] - want[i]).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "change at {change_at}: output differs from the always-60% run by {worst}");
            // and no dropout in the tail region
            let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
            let (a, b) = (rms(&got[change_at + 1_000..change_at + 9_000]), rms(&want[change_at + 1_000..change_at + 9_000]));
            assert!((a / b - 1.0).abs() < 0.01, "change at {change_at}: rms {a} vs {b}");
        }
    }

    /// The same through the resampled and damped path (room size, low and high damping), on both
    /// channels, from the very sample of the change: no sample of the old impulse is left in the
    /// head's block in progress.
    #[test]
    fn changing_damping_and_size_takes_effect_on_the_next_sample_on_both_channels() {
        let mut rng = Prng::new(11);
        let noise: Vec<f32> = (0..40_000).map(|_| rng.uniform() * 0.5).collect();
        let settings = [("room_size", 45.0), ("damping_lf", 300.0), ("damping_hf", 3_000.0)];
        let mut fixed = reverb(3);
        for (p, v) in settings {
            assert!(fixed.set_param(p, v));
        }
        fixed.reset();
        let want = process(&mut fixed, &noise);
        // 256-sample blocks start at multiples of 256 of the effect's own clock: change mid-block
        for change_at in [10_000usize, 10_100, 10_255] {
            let mut e = reverb(3);
            let mut got = process(&mut e, &noise[..change_at]);
            for (p, v) in settings {
                assert!(e.set_param(p, v));
            }
            let tail = process(&mut e, &noise[change_at..]);
            for c in 0..2 {
                got[c].extend(&tail[c]);
                let worst = (change_at..noise.len()).map(|i| (got[c][i] - want[c][i]).abs()).fold(0.0f32, f32::max);
                assert!(worst < 1e-4, "change at {change_at}, channel {c}: differs from the always-new run by {worst}");
            }
        }
    }

    /// Changing the impulse itself drops nothing and keeps nothing of the old tail.
    #[test]
    fn changing_the_impulse_replaces_the_old_tail() {
        let mut rng = Prng::new(9);
        let noise: Vec<f32> = (0..30_000).map(|_| rng.uniform() * 0.5).collect();
        let mut e = reverb(3);
        process(&mut e, &noise);
        // Vocal Booth at 10% room size: about 1000 samples long, all in the head stage
        assert!(e.set_param("impulse", 6.0) && e.set_param("room_size", 10.0));
        let [l, r] = process(&mut e, &vec![0.0; 20_000]);
        let late = |x: &[f32]| x[1_700..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(late(&l) < 1e-6 && late(&r) < 1e-6, "old tail still audible: {} {}", late(&l), late(&r));
    }

    /// Noise through the staged convolution equals the direct convolution with the impulse, at
    /// samples either side of each stage boundary.
    #[test]
    fn staged_convolution_equals_the_direct_convolution() {
        let index = 3;
        let mut e = reverb(index);
        let len = ir_len(&IR_SPECS[index], SR);
        let (mut ir_l, mut ir_r) = (vec![0.0; len], vec![0.0; len]);
        generate_ir(index, SR, [&mut ir_l, &mut ir_r]);
        let mut rng = Prng::new(5);
        let input: Vec<f32> = (0..30_000).map(|_| rng.uniform() * 0.5).collect();
        let [l, _] = process(&mut e, &input);
        let lat = e.latency();
        for n in [100, 2047, 2048, 2049, 4000, 8191, 8192, 8193, 12_345, 24_000, 29_000] {
            let direct: f32 = (0..=n).filter(|k| n - k < len).map(|k| input[k] * ir_l[n - k]).sum();
            assert!((l[lat + n] - direct).abs() < 1e-4, "sample {n}: {} vs {direct}", l[lat + n]);
        }
    }
}
