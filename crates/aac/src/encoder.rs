//! AAC-LC encoder.
//!
//! Pipeline per 1024-sample frame and channel element:
//! 1. transient detection on high-passed 128-sample sub-blocks (one frame of look-ahead) →
//!    window sequence (`ONLY_LONG` / `LONG_START` / `EIGHT_SHORT` / `LONG_STOP`) and short-window grouping;
//! 2. windowed MDCT (sine or KBD);
//! 3. psychoacoustic model → per-band masking thresholds and perceptual entropy;
//! 4. TNS analysis (long windows; Levinson–Durbin, applied when the prediction gain is high);
//! 5. per-band M/S decision for channel pairs (common window);
//! 6. rate–distortion loop: a single multiplier λ scales every band's allowed noise; each band gets the
//!    coarsest scalefactor meeting its noise target, sections/codebooks are chosen by dynamic
//!    programming, and λ is bisected until the frame fits its bit budget (CBR) or fixed (VBR);
//! 7. bit reservoir (CBR): budget follows the frame's PE, bounded by the decoder buffer
//!    (6144 bits/channel); overflow is padded with fill elements so the rate stays constant.

mod psy;
mod quant;

use filmcraft_bitstream::BitWriter;

use crate::config::{AdtsHeader, AudioSpecificConfig, ElementType, ProgramConfig, config_layout};
use crate::ics::{IcsInfo, WindowSequence};
use crate::mdct::{WindowShape, mdct_long, mdct_short, window};
use crate::tables::{sample_rate_index, swb_offsets_long, swb_offsets_short, tns_max_bands};
use crate::tns::{self, TnsData, TnsFilter};
use crate::{Error, FRAME_LEN, Result};
use psy::Psy;
use quant::{ChannelInput, QuantChannel, quantise_channel, write_ics};

/// Rate control mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitrateMode {
    /// Constant bitrate in bits per second (bit reservoir + fill elements).
    Cbr(u32),
    /// Constant quality, 1.0 (small) ..= 5.0 (transparent-ish). 3.0 targets the masking threshold.
    Vbr(f32),
}

/// Encoder settings.
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    /// One of the 13 AAC sampling rates (8–96 kHz).
    pub sample_rate: u32,
    /// 1–8. Channel order is the AAC element order: C; L R; C L R; C L R Cs; C L R Ls Rs;
    /// C L R Ls Rs LFE; (7) C L R Ls Rs Cs LFE; (8) C Lc Rc L R Ls Rs LFE.
    pub channels: usize,
    pub mode: BitrateMode,
    /// Audio bandwidth in Hz (`None` = chosen from the bitrate / quality).
    pub bandwidth: Option<u32>,
    /// Temporal noise shaping on long windows.
    pub tns: bool,
    /// Per-band mid/side stereo for channel pairs.
    pub ms_stereo: bool,
    pub window_shape: WindowShape,
    /// Prefix every access unit with an ADTS header.
    pub adts: bool,
}

impl EncoderConfig {
    pub fn cbr(sample_rate: u32, channels: usize, bitrate_bps: u32) -> EncoderConfig {
        EncoderConfig {
            sample_rate,
            channels,
            mode: BitrateMode::Cbr(bitrate_bps),
            bandwidth: None,
            tns: true,
            ms_stereo: true,
            window_shape: WindowShape::Sine,
            adts: false,
        }
    }
    pub fn vbr(sample_rate: u32, channels: usize, quality: f32) -> EncoderConfig {
        EncoderConfig { mode: BitrateMode::Vbr(quality), ..Self::cbr(sample_rate, channels, 0) }
    }
}

/// Running statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncoderStats {
    pub frames: u64,
    /// Raw access-unit bytes (without ADTS headers).
    pub bytes: u64,
    /// Element-frames coded with eight short windows.
    pub short_blocks: u64,
    pub ms_bands: u64,
    pub stereo_bands: u64,
    pub tns_filters: u64,
    /// Bits spent on fill elements (CBR padding).
    pub fill_bits: u64,
}

impl EncoderStats {
    /// Average bitrate of the raw access units.
    pub fn bitrate(&self, sample_rate: u32) -> f64 {
        if self.frames == 0 { 0.0 } else { self.bytes as f64 * 8.0 * sample_rate as f64 / (self.frames as f64 * FRAME_LEN as f64) }
    }
}

struct ElementState {
    ty: ElementType,
    tag: u8,
    first_ch: usize,
    prev_seq: WindowSequence,
    short_next: Option<bool>,
    /// Previous long-window thresholds per channel (pre-echo control).
    prev_thr: Vec<Vec<f32>>,
}

/// Per-frame analysis of one element.
struct ElementFrame {
    info: IcsInfo,
    chans: Vec<ChannelInput>,
    ms_mask: Option<(u32, Vec<bool>)>,
}

const HISTORY: usize = 2048;

/// AAC-LC encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    sf_index: u8,
    asc: AudioSpecificConfig,
    psy: Psy,
    elements: Vec<ElementState>,
    /// Input history per channel; `hist[c][0]` is absolute sample `origin`.
    hist: Vec<Vec<f32>>,
    origin: i64,
    total_in: u64,
    frames_done: u64,
    flushed: bool,
    bandwidth: u32,
    // rate control
    mean_acc: u64,
    level: i64,
    res_max: i64,
    max_frame_bits: i64,
    pe_avg: f32,
    lambda: f32,
    stats: EncoderStats,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        let sf_index = sample_rate_index(cfg.sample_rate).ok_or(Error::InvalidConfig("sample rate must be one of the 13 AAC rates (7350–96000 Hz)"))?;
        if !(1..=8).contains(&cfg.channels) {
            return Err(Error::InvalidConfig("channels must be 1..=8"));
        }
        use ElementType::*;
        let (channel_config, layout) = match cfg.channels {
            7 => (0u8, vec![(Sce, 0u8), (Cpe, 0), (Cpe, 1), (Sce, 1), (Lfe, 0)]),
            8 => (7, config_layout(7).unwrap_or_default()),
            n => (n as u8, config_layout(n as u8).unwrap_or_default()),
        };
        let mut asc = AudioSpecificConfig::lc(sf_index, channel_config);
        if channel_config == 0 {
            asc.pce = Some(ProgramConfig {
                element_instance_tag: 0,
                object_type: 1,
                sf_index,
                front: vec![(false, 0), (true, 0)],
                side: vec![(true, 1)],
                back: vec![(false, 1)],
                lfe: vec![0],
                ..Default::default()
            });
        }
        let mut elements = Vec::new();
        let mut ch = 0;
        for (ty, tag) in layout {
            elements.push(ElementState {
                ty,
                tag,
                first_ch: ch,
                prev_seq: WindowSequence::OnlyLong,
                short_next: None,
                prev_thr: vec![Vec::new(); ty.channels()],
            });
            ch += ty.channels();
        }
        let nfull = cfg.channels - elements.iter().filter(|e| e.ty == Lfe).count();
        let nyq = cfg.sample_rate / 2;
        let auto_bw = match cfg.mode {
            BitrateMode::Cbr(b) => {
                let per = b as f32 / nfull.max(1) as f32 / 1000.0;
                let pts = [
                    (12.0f32, 4000.0f32),
                    (16.0, 5500.0),
                    (24.0, 8000.0),
                    (32.0, 11000.0),
                    (48.0, 14000.0),
                    (64.0, 16000.0),
                    (80.0, 17500.0),
                    (96.0, 19000.0),
                    (112.0, 20000.0),
                    (144.0, 22000.0),
                ];
                interp(&pts, per) as u32
            }
            BitrateMode::Vbr(q) => interp(&[(1.0, 13000.0), (2.0, 15000.0), (3.0, 17000.0), (4.0, 19000.0), (5.0, 20000.0)], q) as u32,
        };
        let bandwidth = cfg.bandwidth.unwrap_or(auto_bw).clamp(1000, nyq.saturating_sub(nyq / 50).max(1000));
        let max_frame_bits = 6144 * cfg.channels as i64;
        let mean = match cfg.mode {
            BitrateMode::Cbr(b) => b as i64 * FRAME_LEN as i64 / cfg.sample_rate as i64,
            BitrateMode::Vbr(_) => 0,
        };
        if let BitrateMode::Cbr(b) = cfg.mode
            && (b < 1000 * cfg.channels as u32 || mean > max_frame_bits)
        {
            return Err(Error::InvalidConfig("bitrate out of range for the channel count and sample rate"));
        }
        let res_max = (max_frame_bits - mean).max(0);
        let hist = vec![vec![0f32; HISTORY]; cfg.channels];
        Ok(Encoder {
            psy: Psy::new(cfg.sample_rate, sf_index),
            sf_index,
            asc,
            elements,
            hist,
            origin: -(HISTORY as i64),
            total_in: 0,
            frames_done: 0,
            flushed: false,
            bandwidth,
            mean_acc: 0,
            level: 0,
            res_max,
            max_frame_bits,
            pe_avg: 0.0,
            lambda: 1.0,
            stats: EncoderStats::default(),
            cfg,
        })
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// `AudioSpecificConfig` bytes for the MP4 `esds` decoder-specific info.
    pub fn audio_specific_config(&self) -> Vec<u8> {
        self.asc.to_bytes()
    }

    /// Encoder delay: the first 1024 decoded samples are priming and should be skipped (MP4 edit
    /// list `media_time`, or iTunSMPB).
    pub fn priming_samples(&self) -> u32 {
        FRAME_LEN as u32
    }

    /// Audio bandwidth in Hz.
    pub fn bandwidth(&self) -> u32 {
        self.bandwidth
    }

    pub fn stats(&self) -> EncoderStats {
        self.stats
    }

    /// Feed planar samples (±1.0 full scale); returns any complete access units. Missing channels are
    /// treated as silence; all channels are truncated to the shortest slice.
    pub fn encode(&mut self, planar: &[&[f32]]) -> Vec<Vec<u8>> {
        if self.flushed {
            return Vec::new();
        }
        let n = planar.iter().take(self.cfg.channels).map(|p| p.len()).min().unwrap_or(0);
        for c in 0..self.cfg.channels {
            match planar.get(c) {
                Some(p) => self.hist[c].extend(p[..n].iter().map(|v| if v.is_finite() { v.clamp(-8.0, 8.0) } else { 0.0 })),
                None => self.hist[c].extend(std::iter::repeat_n(0.0, n)),
            }
        }
        self.total_in += n as u64;
        let mut out = Vec::new();
        while self.available() >= self.needed_for(self.frames_done) {
            out.push(self.encode_frame(false));
        }
        out
    }

    /// Encode the remaining input (zero-padded). Afterwards the decoded stream, minus
    /// [`priming_samples`](Self::priming_samples), covers every input sample.
    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        if self.flushed {
            return Vec::new();
        }
        let frames = if self.total_in == 0 { 0 } else { self.total_in.div_ceil(FRAME_LEN as u64) + 1 };
        let mut out = Vec::new();
        while self.frames_done < frames {
            let need = self.needed_for(self.frames_done);
            let have = self.available();
            if have < need {
                for h in &mut self.hist {
                    h.extend(std::iter::repeat_n(0.0, (need - have) as usize));
                }
            }
            let last = self.frames_done + 1 == frames;
            out.push(self.encode_frame(last));
        }
        self.flushed = true;
        out
    }

    /// Absolute sample index one past the buffered input.
    fn available(&self) -> i64 {
        self.origin + self.hist[0].len() as i64
    }
    /// Input needed (absolute end) before frame `t` can be encoded (window + transient look-ahead).
    fn needed_for(&self, t: u64) -> i64 {
        (t as i64 + 1) * FRAME_LEN as i64 + 640
    }

    fn sample(&self, ch: usize, abs: i64) -> f32 {
        let i = abs - self.origin;
        if i < 0 { 0.0 } else { self.hist[ch].get(i as usize).copied().unwrap_or(0.0) }
    }

    fn subblock_energy(&self, ch: usize, j: i64) -> f32 {
        let s = j * 128;
        let mut e = 0f32;
        let mut prev = self.sample(ch, s - 1);
        for n in s..s + 128 {
            let x = self.sample(ch, n);
            let d = x - prev;
            e += d * d;
            prev = x;
        }
        e
    }

    /// Transient in the region covered by frame `t`'s short windows.
    fn transient(&self, first_ch: usize, nch: usize, t: i64) -> bool {
        for ch in first_ch..first_ch + nch {
            let base = 8 * t;
            let mut hist: Vec<f32> = (base - 13..base - 5).map(|j| self.subblock_energy(ch, j)).collect();
            for j in base - 5..=base + 4 {
                let e = self.subblock_energy(ch, j);
                let mean = hist.iter().sum::<f32>() / hist.len() as f32;
                if e > 1.5e-4 && e > 10.0 * mean {
                    return true;
                }
                hist.remove(0);
                hist.push(e);
            }
        }
        false
    }

    fn max_sfb(&self, short: bool, lfe: bool) -> usize {
        let bw = if lfe { 240.min(self.bandwidth) } else { self.bandwidth };
        let (swb, m) = if short { (swb_offsets_short(self.sf_index), 128f64) } else { (swb_offsets_long(self.sf_index), 1024f64) };
        let bin = (bw as f64 / (self.cfg.sample_rate as f64 / 2.0) * m).ceil() as u16;
        swb.iter().take(swb.len() - 1).filter(|&&o| o < bin).count().max(1)
    }

    fn mdct(&self, ch: usize, seq: WindowSequence) -> Vec<f32> {
        let shape = self.cfg.window_shape;
        let seg = &self.hist[ch][HISTORY - FRAME_LEN..HISTORY + FRAME_LEN];
        let mut spec = vec![0f32; 1024];
        let scale = 32768.0 * 2.0;
        if seq == WindowSequence::EightShort {
            let ws = window(shape, false);
            let mut buf = vec![0f32; 256];
            for w in 0..8 {
                let off = 448 + 128 * w;
                for n in 0..128 {
                    buf[n] = seg[off + n] * ws[n] * scale;
                    buf[128 + n] = seg[off + 128 + n] * ws[127 - n] * scale;
                }
                mdct_short().forward(&buf, &mut spec[w * 128..(w + 1) * 128]);
            }
            return spec;
        }
        let wl = window(shape, true);
        let ws = window(shape, false);
        let mut buf = vec![0f32; 2048];
        for n in 0..1024 {
            let w = match seq {
                WindowSequence::LongStop => {
                    if n < 448 {
                        0.0
                    } else if n < 576 {
                        ws[n - 448]
                    } else {
                        1.0
                    }
                }
                _ => wl[n],
            };
            buf[n] = seg[n] * w * scale;
        }
        for n in 1024..2048 {
            let w = match seq {
                WindowSequence::LongStart => {
                    if n < 1472 {
                        1.0
                    } else if n < 1600 {
                        ws[127 - (n - 1472)]
                    } else {
                        0.0
                    }
                }
                _ => wl[2047 - n],
            };
            buf[n] = seg[n] * w * scale;
        }
        mdct_long().forward(&buf, &mut spec);
        spec
    }

    fn tns_analyse(&self, spec: &mut [f32], info: &IcsInfo) -> Option<TnsData> {
        let swb = swb_offsets_long(self.sf_index);
        let lim = tns_max_bands(self.sf_index, false).min(info.max_sfb);
        let hz = self.cfg.sample_rate as f64 / 2048.0;
        let start = swb.iter().position(|&o| o as f64 * hz >= 1400.0).unwrap_or(swb.len() - 1);
        if lim < start + 4 {
            return None;
        }
        let (lo, hi) = (swb[start] as usize, swb[lim] as usize);
        let x = &spec[lo..hi];
        const ORDER: usize = 8;
        let mut r = [0f64; ORDER + 1];
        for (lag, rv) in r.iter_mut().enumerate() {
            let mut acc = 0f64;
            for n in lag..x.len() {
                acc += x[n] as f64 * x[n - lag] as f64;
            }
            // Gaussian lag window for robustness
            *rv = acc * (-0.5 * (0.02 * lag as f64).powi(2)).exp();
        }
        if r[0] <= 1e-3 {
            return None;
        }
        let (ks, gain) = tns::levinson(&r, ORDER);
        if gain < 1.4 {
            return None;
        }
        let mut coef = [0i8; 12];
        let mut order = 0;
        for (i, &k) in ks.iter().enumerate() {
            coef[i] = tns::quant_coef(k, true);
            if coef[i] != 0 {
                order = i + 1;
            }
        }
        if order == 0 {
            return None;
        }
        let mut t = TnsData::default();
        t.windows[0].n_filt = 1;
        t.windows[0].coef_res = true;
        t.windows[0].filt[0] = TnsFilter { length: (swb.len() - 1 - start) as u8, order: order as u8, direction: false, coef_compress: false, coef };
        tns::apply_encoder(spec, info, self.sf_index, &t);
        Some(t)
    }

    fn analyse_element(&mut self, ei: usize, t: i64) -> (ElementFrame, f32) {
        let (ty, first, nch, prev_seq, cached) = {
            let e = &self.elements[ei];
            (e.ty, e.first_ch, e.ty.channels(), e.prev_seq, e.short_next)
        };
        let lfe = ty == ElementType::Lfe;
        let (short_now, short_next) =
            if lfe { (false, false) } else { (cached.unwrap_or_else(|| self.transient(first, nch, t)), self.transient(first, nch, t + 1)) };
        use WindowSequence::*;
        let seq = if short_now {
            EightShort
        } else if prev_seq == EightShort || prev_seq == LongStart {
            if short_next { EightShort } else { LongStop }
        } else if short_next {
            LongStart
        } else {
            OnlyLong
        };
        let specs: Vec<Vec<f32>> = (first..first + nch).map(|c| self.mdct(c, seq)).collect();
        let short = seq == EightShort;
        let mut info = IcsInfo { window_sequence: seq, window_shape: self.cfg.window_shape, max_sfb: self.max_sfb(short, lfe), ..Default::default() };
        if short {
            // group windows of similar energy; an attack starts a new group
            let en: Vec<f32> = (0..8).map(|w| specs.iter().map(|s| s[w * 128..(w + 1) * 128].iter().map(|v| v * v).sum::<f32>()).sum::<f32>() + 1.0).collect();
            info.num_groups = 1;
            info.group_len = [1, 0, 0, 0, 0, 0, 0, 0];
            for w in 1..8 {
                let r = en[w] / en[w - 1];
                if !(1.0 / 6.0..=3.0).contains(&r) {
                    info.num_groups += 1;
                    info.group_len[info.num_groups - 1] = 1;
                } else {
                    info.group_len[info.num_groups - 1] += 1;
                }
            }
        }
        let mut pe = 0f32;
        let mut specs = specs;
        let mut thrs: Vec<Vec<Vec<f32>>> = Vec::with_capacity(nch);
        let mut tnss = Vec::with_capacity(nch);
        for (i, spec) in specs.iter_mut().enumerate() {
            let mut r = self.psy.analyse(spec, short);
            pe += r.pe;
            let e = &mut self.elements[ei];
            if !short {
                if prev_seq != EightShort && e.prev_thr[i].len() == r.thr[0].len() {
                    for (t, p) in r.thr[0].iter_mut().zip(&e.prev_thr[i]) {
                        *t = t.min(2.0 * p.max(1e-9));
                    }
                }
                e.prev_thr[i] = r.thr[0].clone();
            } else {
                e.prev_thr[i].clear();
            }
            // group thresholds: the most sensitive window times the group length
            let mut thr_g = vec![vec![0f32; 64]; info.num_groups];
            for g in 0..info.num_groups {
                let w0 = info.group_start(g);
                let gl = info.group_len[g] as usize;
                for sfb in 0..info.max_sfb {
                    let m = (w0..w0 + gl).map(|w| r.thr[w][sfb]).fold(f32::MAX, f32::min);
                    thr_g[g][sfb] = m * gl as f32;
                }
            }
            thrs.push(thr_g);
            let tns = if self.cfg.tns && !short && !lfe { self.tns_analyse(spec, &info) } else { None };
            if let Some(t) = &tns {
                self.stats.tns_filters += t.windows[0].n_filt as u64;
            }
            tnss.push(tns);
        }
        let swb = info.swb_offsets(self.sf_index);
        let mut ms_mask = None;
        if ty == ElementType::Cpe {
            let mut used = Vec::with_capacity(info.num_groups * info.max_sfb);
            let (l, r) = specs.split_at_mut(1);
            let (l, r) = (&mut l[0], &mut r[0]);
            for g in 0..info.num_groups {
                let w0 = info.group_start(g);
                let gl = info.group_len[g] as usize;
                for sfb in 0..info.max_sfb {
                    let (mut el, mut er, mut em, mut es) = (0f32, 0f32, 0f32, 0f32);
                    for w in w0..w0 + gl {
                        for k in w * 128 + swb[sfb] as usize..w * 128 + swb[sfb + 1] as usize {
                            let (a, b) = (l[k], r[k]);
                            el += a * a;
                            er += b * b;
                            em += 0.25 * (a + b) * (a + b);
                            es += 0.25 * (a - b) * (a - b);
                        }
                    }
                    let (tl, tr) = (thrs[0][g][sfb].max(1e-9), thrs[1][g][sfb].max(1e-9));
                    let tm = 0.5 * tl.min(tr);
                    let c = |e: f32, t: f32| (e / t).max(1.0).log2();
                    let use_ms = self.cfg.ms_stereo && c(em, tm) + c(es, tm) < c(el, tl) + c(er, tr);
                    used.push(use_ms);
                    self.stats.stereo_bands += 1;
                    if use_ms {
                        self.stats.ms_bands += 1;
                        thrs[0][g][sfb] = tm;
                        thrs[1][g][sfb] = tm;
                        for w in w0..w0 + gl {
                            for k in w * 128 + swb[sfb] as usize..w * 128 + swb[sfb + 1] as usize {
                                let (a, b) = (l[k], r[k]);
                                l[k] = 0.5 * (a + b);
                                r[k] = 0.5 * (a - b);
                            }
                        }
                    }
                }
            }
            let n_ms = used.iter().filter(|&&u| u).count();
            let present = if n_ms == 0 {
                0
            } else if n_ms == used.len() {
                2
            } else {
                1
            };
            ms_mask = Some((present, used));
        }
        let chans = specs.iter().zip(thrs.iter()).zip(tnss).map(|((s, th), tn)| ChannelInput::new(info, swb, s, th, tn)).collect();
        let e = &mut self.elements[ei];
        e.prev_seq = seq;
        e.short_next = Some(short_next);
        if short {
            self.stats.short_blocks += 1;
        }
        (ElementFrame { info, chans, ms_mask }, pe)
    }

    fn element_overhead(&self, ei: usize, f: &ElementFrame) -> u32 {
        let info_bits = if f.info.is_short() { 15 } else { 11 };
        match self.elements[ei].ty {
            ElementType::Cpe => {
                let ms = match &f.ms_mask {
                    Some((1, used)) => used.len() as u32,
                    _ => 0,
                };
                3 + 4 + 1 + info_bits + 2 + ms
            }
            _ => 3 + 4 + info_bits,
        }
    }

    fn quantise_all(frames: &[ElementFrame], lambda: f32) -> Vec<Vec<QuantChannel>> {
        #[cfg(feature = "rayon")]
        {
            use rayon::prelude::*;
            frames.par_iter().map(|f| f.chans.par_iter().map(|c| quantise_channel(c, lambda)).collect()).collect()
        }
        #[cfg(not(feature = "rayon"))]
        {
            frames.iter().map(|f| f.chans.iter().map(|c| quantise_channel(c, lambda)).collect()).collect()
        }
    }

    /// `last`: final frame of the stream — a CBR stream spends (or pads away) the whole reservoir so
    /// the total size is exactly `frames × bitrate × 1024 / rate`.
    fn encode_frame(&mut self, last: bool) -> Vec<u8> {
        let t = self.frames_done as i64;
        let mut frames = Vec::with_capacity(self.elements.len());
        let mut pe = 0f32;
        for ei in 0..self.elements.len() {
            let (f, p) = self.analyse_element(ei, t);
            pe += p;
            frames.push(f);
        }
        let pce_bits = if self.cfg.adts && self.asc.channel_config == 0 {
            let mut bw = BitWriter::new();
            if let Some(p) = &self.asc.pce {
                p.write(&mut bw);
            }
            3 + bw.bit_len() as u32 + 8
        } else {
            0
        };
        let overhead: u32 = (0..frames.len()).map(|i| self.element_overhead(i, &frames[i])).sum::<u32>() + 3 + 7 + pce_bits;
        let total_bits = |q: &Vec<Vec<QuantChannel>>| overhead + q.iter().flatten().map(|c| c.bits).sum::<u32>();

        let any_short = frames.iter().any(|f| f.info.is_short());
        let (mean, target) = match self.cfg.mode {
            BitrateMode::Cbr(b) => {
                self.mean_acc += b as u64 * FRAME_LEN as u64;
                let mean = (self.mean_acc / self.cfg.sample_rate as u64) as i64;
                self.mean_acc %= self.cfg.sample_rate as u64;
                self.pe_avg = if self.pe_avg == 0.0 { pe.max(1.0) } else { 0.95 * self.pe_avg + 0.05 * pe.max(1.0) };
                let mut ratio = (pe.max(1.0) / self.pe_avg).clamp(0.7, 1.6);
                if any_short {
                    ratio = (ratio * 1.3).min(2.0);
                }
                let mut desired = mean as f32 * ratio + (self.level as f32 - 0.5 * self.res_max as f32) * 0.15;
                desired = desired.min((mean + self.level) as f32).max((mean - (self.res_max - self.level)) as f32);
                desired = desired.min(self.max_frame_bits as f32);
                if last {
                    desired = (mean + self.level) as f32;
                }
                (mean, desired.max(0.0) as u32)
            }
            BitrateMode::Vbr(_) => (0, self.max_frame_bits as u32),
        };

        let quant = match self.cfg.mode {
            BitrateMode::Vbr(q) => {
                let d = q.clamp(0.5, 6.0) - 1.0;
                let mut lambda = 10f32.powf(-0.2 - 1.2 * d + 0.09 * d * d);
                let mut res = Self::quantise_all(&frames, lambda);
                while total_bits(&res) > target && lambda < 1e9 {
                    lambda *= 2.0;
                    res = Self::quantise_all(&frames, lambda);
                }
                res
            }
            BitrateMode::Cbr(_) => {
                let mut lam = self.lambda.clamp(1e-6, 1e9);
                let mut cur = Self::quantise_all(&frames, lam);
                let (mut lo, mut hi, mut best);
                if total_bits(&cur) <= target {
                    hi = lam;
                    best = cur;
                    lo = lam;
                    loop {
                        lo /= 4.0;
                        if lo < 1e-6 {
                            break;
                        }
                        cur = Self::quantise_all(&frames, lo);
                        if total_bits(&cur) > target {
                            break;
                        }
                        hi = lo;
                        best = cur;
                    }
                } else {
                    lo = lam;
                    loop {
                        lam *= 4.0;
                        cur = Self::quantise_all(&frames, lam);
                        if total_bits(&cur) <= target || lam > 1e9 {
                            break;
                        }
                        lo = lam;
                    }
                    hi = lam;
                    best = cur;
                }
                for _ in 0..6 {
                    if hi / lo < 1.03 {
                        break;
                    }
                    let mid = (lo * hi).sqrt();
                    let r = Self::quantise_all(&frames, mid);
                    if total_bits(&r) <= target {
                        hi = mid;
                        best = r;
                    } else {
                        lo = mid;
                    }
                }
                self.lambda = hi;
                best
            }
        };

        // assemble the raw_data_block
        let mut bw = BitWriter::new();
        if pce_bits > 0
            && let Some(p) = &self.asc.pce
        {
            bw.write_bits(5, 3);
            p.write(&mut bw);
        }
        for (ei, (f, q)) in frames.iter().zip(&quant).enumerate() {
            let e = &self.elements[ei];
            match e.ty {
                ElementType::Cpe => {
                    bw.write_bits(1, 3);
                    bw.write_bits(e.tag as u32, 4);
                    bw.write_bits(1, 1); // common_window
                    f.info.write(&mut bw);
                    let (present, used) = f.ms_mask.clone().unwrap_or((0, Vec::new()));
                    bw.write_bits(present, 2);
                    if present == 1 {
                        for &u in &used {
                            bw.write_bits(u as u32, 1);
                        }
                    }
                    write_ics(&mut bw, &f.chans[0], &q[0], false);
                    write_ics(&mut bw, &f.chans[1], &q[1], false);
                }
                ty => {
                    bw.write_bits(if ty == ElementType::Lfe { 3 } else { 0 }, 3);
                    bw.write_bits(e.tag as u32, 4);
                    write_ics(&mut bw, &f.chans[0], &q[0], true);
                }
            }
        }
        if let BitrateMode::Cbr(_) = self.cfg.mode {
            let projected = ((bw.bit_len() as i64 + 3 + 7) / 8) * 8;
            let excess = self.level + mean - projected - if last { 0 } else { self.res_max };
            if excess > 0 {
                self.stats.fill_bits += write_fill(&mut bw, excess as u64);
            }
        }
        bw.write_bits(7, 3); // ID_END
        let au = bw.finish();
        let used = au.len() as i64 * 8;
        if let BitrateMode::Cbr(_) = self.cfg.mode {
            self.level = (self.level + mean - used).clamp(0, self.res_max);
        }
        self.stats.frames += 1;
        self.stats.bytes += au.len() as u64;

        // slide the input window
        for h in &mut self.hist {
            h.drain(..FRAME_LEN);
        }
        self.origin += FRAME_LEN as i64;
        self.frames_done += 1;

        if self.cfg.adts {
            let fullness = match self.cfg.mode {
                BitrateMode::Cbr(_) => ((self.level / (32 * self.cfg.channels as i64)) as u16).min(0x7FE),
                BitrateMode::Vbr(_) => 0x7FF,
            };
            let mut out = AdtsHeader::write(2, self.sf_index, self.asc.channel_config, au.len(), fullness).to_vec();
            out.extend_from_slice(&au);
            out
        } else {
            au
        }
    }
}

/// Write fill elements totalling at least `bits - 7` bits (at most `bits + 8`); returns bits written.
fn write_fill(bw: &mut BitWriter, bits: u64) -> u64 {
    let start = bw.bit_len() as u64;
    let mut left = bits as i64;
    while left >= 7 {
        let n = (((left - 7) / 8) as usize).min(269);
        let n = if n >= 15 && (left - 15) / 8 < 15 { 14 } else { n };
        bw.write_bits(6, 3);
        if n >= 15 {
            bw.write_bits(15, 4);
            bw.write_bits((n - 14) as u32, 8);
        } else {
            bw.write_bits(n as u32, 4);
        }
        if n > 0 {
            bw.write_bits(0, 8); // extension_type EXT_FILL + fill_nibble
            for _ in 1..n {
                bw.write_bits(0xA5, 8);
            }
        }
        let el = 7 + if n >= 15 { 8 } else { 0 } + 8 * n as i64;
        left -= el;
        if n == 0 {
            break;
        }
    }
    bw.bit_len() as u64 - start
}

fn interp(pts: &[(f32, f32)], x: f32) -> f32 {
    if x <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        if x <= w[1].0 {
            let t = (x - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + t * (w[1].1 - w[0].1);
        }
    }
    pts[pts.len() - 1].1
}
