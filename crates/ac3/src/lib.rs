//! Clean-room AC-3 (Dolby Digital) and E-AC-3 (Dolby Digital Plus) audio decoder, written from
//! ATSC A/52:2012 ("Digital Audio Compression Standard", §5 bit stream syntax, §6-7 decoding,
//! Annex E for E-AC-3).
//!
//! - All audio coding modes (1+1, 1/0 … 3/2) with or without LFE, 32 / 44.1 / 48 kHz, every
//!   frame size (Table 5.18).
//! - Exponents (D15 / D25 / D45, reuse across blocks), the parametric bit allocation exactly as
//!   specified (§7.2, fixed-point integer steps and Tables 7.6-7.16) including delta bit allocation,
//!   grouped and asymmetric mantissas, dither for zero-bit mantissas (§7.3.4), channel coupling
//!   with phase flags, rematrixing, dynamic range control (dynrng / dynrng2), block switching
//!   (512- and 256-sample IMDCTs, §7.9.4) with the Kaiser-Bessel derived window.
//! - Output: planar f32 at full scale ±1, channels in WAV / SMPTE order (L R C LFE Ls Rs); no
//!   downmix.
//!
//! E-AC-3 (bsid 11-16, Annex E): independent substream 0 (dependent substreams and further
//! programs are skipped), 1 / 2 / 3 / 6 blocks per syncframe, the reduced sample rates, frame-based
//! exponent strategies and the other frame-level syntax, and spectral extension (§E3.6). The
//! adaptive hybrid transform and enhanced coupling are refused with [`Error::Unsupported`];
//! transient pre-noise processing data is read and not applied.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod eac3;
mod tables;

use std::sync::OnceLock;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not an AC-3 frame")]
    NoSync,
    #[error("unsupported AC-3: {0}")]
    Unsupported(String),
    #[error("invalid AC-3 frame: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Data rate in kb/s by frmsizecod / 2 (Table 5.18).
const BITRATES: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];

/// Full-bandwidth channels per acmod (Table 5.8).
const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// syncinfo and the start of bsi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub sample_rate: u32,
    pub bitrate_kbps: u32,
    /// Bytes in the syncframe.
    pub frame_bytes: usize,
    pub bsid: u8,
    pub acmod: u8,
    pub lfeon: bool,
    /// Audio blocks (of 256 samples per channel) in the syncframe: 6 for AC-3; 1, 2, 3 or 6 for
    /// E-AC-3.
    pub blocks: u8,
    /// E-AC-3 stream type (Table E2.1): 0 independent, 1 dependent, 2 independent converted from
    /// AC-3. 0 for AC-3.
    pub strmtyp: u8,
    /// E-AC-3 substream id (0 for AC-3).
    pub substreamid: u8,
}

impl Header {
    /// Output channels (full bandwidth + LFE).
    pub fn channels(&self) -> usize {
        NFCHANS[self.acmod as usize & 7] + self.lfeon as usize
    }
    /// Samples per channel the syncframe decodes to.
    pub fn samples(&self) -> usize {
        256 * self.blocks as usize
    }
    /// Whether this is an E-AC-3 syncframe (bsid 11-16).
    pub fn is_eac3(&self) -> bool {
        self.bsid > 10
    }
    /// Whether the syncframe carries the program [`Decoder::decode`] outputs: AC-3, or E-AC-3
    /// independent substream 0 (§E2.3.1.2). [`Decoder::decode`] returns no channels for the
    /// others: dependent substreams (the channels beyond 5.1 of a 7.1 program) and further
    /// independent programs.
    pub fn is_primary(&self) -> bool {
        self.strmtyp != 1 && self.substreamid == 0
    }
}

/// Parse the frame header at the start of `b`.
pub fn parse_header(b: &[u8]) -> Result<Header> {
    if b.len() < 7 || b[0] != 0x0B || b[1] != 0x77 {
        return Err(Error::NoSync);
    }
    let fscod = b[4] >> 6;
    let frmsizecod = b[4] & 0x3F;
    let bsid = b[5] >> 3;
    if (11..=16).contains(&bsid) {
        return eac3::parse_header(b, bsid);
    }
    if bsid > 16 {
        return Err(Error::Unsupported(format!("bsid {bsid}")));
    }
    if fscod == 3 || frmsizecod >= 38 {
        return Err(Error::Invalid("reserved fscod / frmsizecod"));
    }
    let kbps = BITRATES[(frmsizecod / 2) as usize];
    let words = match fscod {
        0 => kbps * 2,
        1 => kbps * 96_000 / 44_100 + (frmsizecod & 1) as u32,
        _ => kbps * 3,
    };
    // bsid 9 / 10: half / quarter sample rate (A/52 Annex note on "bsid 9 and 10")
    let shift = bsid.saturating_sub(8) as u32;
    let sample_rate = [48_000, 44_100, 32_000][fscod as usize] >> shift;
    let mut r = Bits::new(&b[6..]);
    let acmod = r.read(3) as u8;
    if acmod & 1 != 0 && acmod != 1 {
        r.skip(2);
    }
    if acmod & 4 != 0 {
        r.skip(2);
    }
    if acmod == 2 {
        r.skip(2);
    }
    let lfeon = r.read(1) == 1;
    Ok(Header { sample_rate, bitrate_kbps: kbps >> shift, frame_bytes: words as usize * 2, bsid, acmod, lfeon, blocks: 6, strmtyp: 0, substreamid: 0 })
}

/// MSB-first bit reader; reads past the end give zeros.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }
    fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        v
    }
    fn bit(&mut self) -> bool {
        self.read(1) == 1
    }
    fn skip(&mut self, n: usize) {
        self.pos += n;
    }
    fn overrun(&self) -> bool {
        self.pos > self.d.len() * 8
    }
}

/// Index of the LFE and coupling channels in the per-channel arrays.
const LFE: usize = 5;
const CPL: usize = 6;

/// Bit allocation parameter tables (Tables 7.6-7.11).
const SLOWDEC: [i32; 4] = [0x0f, 0x11, 0x13, 0x15];
const FASTDEC: [i32; 4] = [0x3f, 0x53, 0x67, 0x7b];
const SLOWGAIN: [i32; 4] = [0x540, 0x4d8, 0x478, 0x410];
const DBPBTAB: [i32; 4] = [0x000, 0x700, 0x900, 0xb00];
const FLOORTAB: [i32; 8] = [0x2f0, 0x2b0, 0x270, 0x230, 0x1f0, 0x170, 0x0f0, -0x800];
const FASTGAIN: [i32; 8] = [0x080, 0x100, 0x180, 0x200, 0x280, 0x300, 0x380, 0x400];
/// Mantissa bits of the asymmetric quantizers by bap (Table 7.18).
const QNTZTAB: [u32; 16] = [0, 0, 0, 3, 0, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// The Kaiser-Bessel derived window of §7.9.4 (α = 5, 512 points), whose first half Table 7.33
/// lists to five decimals.
fn window() -> &'static [f32; 256] {
    static W: OnceLock<[f32; 256]> = OnceLock::new();
    W.get_or_init(|| {
        let i0 = |x: f64| {
            let (mut s, mut t) = (1.0f64, 1.0f64);
            for k in 1..50 {
                t *= (x / 2.0) / k as f64;
                s += t * t;
            }
            s
        };
        let alpha = 5.0 * std::f64::consts::PI;
        let k: Vec<f64> = (0..=256).map(|j| i0(alpha * (1.0 - ((j as f64 - 128.0) / 128.0).powi(2)).max(0.0).sqrt())).collect();
        let total: f64 = k.iter().sum();
        let mut acc = 0.0;
        let mut w = [0f32; 256];
        for n in 0..256 {
            acc += k[n];
            w[n] = (acc / total).sqrt() as f32;
        }
        w
    })
}

/// Twiddles for the IMDCTs: (xcos1, xsin1) for N = 512 and (xcos2, xsin2) for the 256-sample
/// transforms, and the IFFT kernels.
struct Twiddles {
    c1: [f32; 128],
    s1: [f32; 128],
    c2: [f32; 64],
    s2: [f32; 64],
    /// e^{j 2π m / 128}, m = 0..128.
    e: [(f32, f32); 128],
}

fn twiddles() -> &'static Twiddles {
    static T: OnceLock<Twiddles> = OnceLock::new();
    T.get_or_init(|| {
        use std::f64::consts::PI;
        let n = 512.0;
        let mut t = Twiddles { c1: [0.0; 128], s1: [0.0; 128], c2: [0.0; 64], s2: [0.0; 64], e: [(0.0, 0.0); 128] };
        for k in 0..128 {
            let a = 2.0 * PI * (8 * k + 1) as f64 / (8.0 * n);
            t.c1[k] = -a.cos() as f32;
            t.s1[k] = -a.sin() as f32;
            let m = 2.0 * PI * k as f64 / 128.0;
            t.e[k] = (m.cos() as f32, m.sin() as f32);
        }
        for k in 0..64 {
            let a = 2.0 * PI * (8 * k + 1) as f64 / (4.0 * n);
            t.c2[k] = -a.cos() as f32;
            t.s2[k] = -a.sin() as f32;
        }
        t
    })
}

/// In-place complex inverse DFT of size `n` (128 or 64) by radix-2 decimation in time:
/// z[n] = Σ Z[k] e^{+j 2π k n / size}.
fn ifft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let e = &twiddles().e;
    // bit reversal
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = 128 / len;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = e[k * step];
                let (a, b) = (s + k, s + k + len / 2);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len *= 2;
    }
}

/// Inverse transform of one block of 256 coefficients into 512 windowed samples (§7.9.4).
fn imdct(x: &[f32; 256], short: bool, out: &mut [f32; 512]) {
    let w = window();
    let t = twiddles();
    const N: usize = 512;
    if !short {
        let (mut zr, mut zi) = ([0f32; 128], [0f32; 128]);
        for k in 0..N / 4 {
            let (a, b) = (x[N / 2 - 2 * k - 1], x[2 * k]);
            zr[k] = a * t.c1[k] - b * t.s1[k];
            zi[k] = b * t.c1[k] + a * t.s1[k];
        }
        ifft(&mut zr, &mut zi);
        let (mut yr, mut yi) = ([0f32; 128], [0f32; 128]);
        for n in 0..N / 4 {
            yr[n] = zr[n] * t.c1[n] - zi[n] * t.s1[n];
            yi[n] = zi[n] * t.c1[n] + zr[n] * t.s1[n];
        }
        for n in 0..N / 8 {
            out[2 * n] = -yi[N / 8 + n] * w[2 * n];
            out[2 * n + 1] = yr[N / 8 - n - 1] * w[2 * n + 1];
            out[N / 4 + 2 * n] = -yr[n] * w[N / 4 + 2 * n];
            out[N / 4 + 2 * n + 1] = yi[N / 4 - n - 1] * w[N / 4 + 2 * n + 1];
            out[N / 2 + 2 * n] = -yr[N / 8 + n] * w[N / 2 - 2 * n - 1];
            out[N / 2 + 2 * n + 1] = yi[N / 8 - n - 1] * w[N / 2 - 2 * n - 2];
            out[3 * N / 4 + 2 * n] = yi[n] * w[N / 4 - 2 * n - 1];
            out[3 * N / 4 + 2 * n + 1] = -yr[N / 4 - n - 1] * w[N / 4 - 2 * n - 2];
        }
        return;
    }
    let mut y = [([0f32; 64], [0f32; 64]); 2];
    for (h, yh) in y.iter_mut().enumerate() {
        let (mut zr, mut zi) = ([0f32; 64], [0f32; 64]);
        for k in 0..N / 8 {
            let (a, b) = (x[2 * (N / 4 - 2 * k - 1) + h], x[2 * (2 * k) + h]);
            zr[k] = a * t.c2[k] - b * t.s2[k];
            zi[k] = b * t.c2[k] + a * t.s2[k];
        }
        ifft(&mut zr, &mut zi);
        for n in 0..N / 8 {
            yh.0[n] = zr[n] * t.c2[n] - zi[n] * t.s2[n];
            yh.1[n] = zi[n] * t.c2[n] + zr[n] * t.s2[n];
        }
    }
    let ((yr1, yi1), (yr2, yi2)) = (y[0], y[1]);
    for n in 0..N / 8 {
        out[2 * n] = -yi1[n] * w[2 * n];
        out[2 * n + 1] = yr1[N / 8 - n - 1] * w[2 * n + 1];
        out[N / 4 + 2 * n] = -yr1[n] * w[N / 4 + 2 * n];
        out[N / 4 + 2 * n + 1] = yi1[N / 8 - n - 1] * w[N / 4 + 2 * n + 1];
        out[N / 2 + 2 * n] = -yr2[n] * w[N / 2 - 2 * n - 1];
        out[N / 2 + 2 * n + 1] = yi2[N / 8 - n - 1] * w[N / 2 - 2 * n - 2];
        out[3 * N / 4 + 2 * n] = yi2[n] * w[N / 4 - 2 * n - 1];
        out[3 * N / 4 + 2 * n + 1] = -yr2[N / 8 - n - 1] * w[N / 4 - 2 * n - 2];
    }
}

fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let addr = ((c.abs() >> 1) as usize).min(255);
    if c >= 0 { a + tables::LATAB[addr] as i32 } else { b + tables::LATAB[addr] as i32 }
}

fn calc_lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            320
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else {
        (a - 128).max(0)
    }
}

/// Inputs to the bit allocation of one channel.
struct AllocParams {
    start: usize,
    end: usize,
    fgain: i32,
    snroffset: i32,
    /// Coupling channel leak initialisation.
    leak: Option<(i32, i32)>,

    /// Delta bit allocation segments (offset, length, ba) when in use.
    dba: Option<Vec<(u8, u8, u8)>>,
}

/// The parametric bit allocation of §7.2.2 for one exponent set.
fn bit_allocation(exp: &[u8; 256], p: &AllocParams, g: &Globals, bap: &mut [u8; 256]) {
    let (start, end) = (p.start, p.end);
    if start >= end {
        return;
    }
    let (sdecay, fdecay, sgain, dbknee, floor) = (SLOWDEC[g.sdcycod], FASTDEC[g.fdcycod], SLOWGAIN[g.sgaincod], DBPBTAB[g.dbpbcod], FLOORTAB[g.floorcod]);
    let mut psd = [0i32; 256];
    for bin in start..end {
        psd[bin] = 3072 - ((exp[bin] as i32) << 7);
    }
    // PSD integration
    let mut bndpsd = [0i32; 50];
    let mut j = start;
    let mut k = tables::MASKTAB[start] as usize;
    loop {
        let lastbin = (tables::BNDTAB[k] as usize + tables::BNDSZ[k] as usize).min(end);
        bndpsd[k] = psd[j];
        j += 1;
        while j < lastbin {
            bndpsd[k] = logadd(bndpsd[k], psd[j]);
            j += 1;
        }
        k += 1;
        if end <= lastbin || k >= 50 {
            break;
        }
    }
    // excitation
    let bndstrt = tables::MASKTAB[start] as usize;
    let bndend = tables::MASKTAB[end - 1] as usize + 1;
    let mut excite = [0i32; 50];
    let (mut fastleak, mut slowleak) = p.leak.unwrap_or((0, 0));
    let begin = if bndstrt == 0 {
        // the last band of the LFE channel (bndend 7) has no band above it
        let lfe_last = |bin: usize| !(bndend == 7 && bin == 6);
        let mut lowcomp = 0;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0);
        excite[0] = bndpsd[0] - p.fgain - lowcomp;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1);
        excite[1] = bndpsd[1] - p.fgain - lowcomp;
        let mut b = 7;
        for bin in 2..7 {
            if lfe_last(bin) {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak = bndpsd[bin] - p.fgain;
            slowleak = bndpsd[bin] - sgain;
            excite[bin] = fastleak - lowcomp;
            if lfe_last(bin) && bndpsd[bin] <= bndpsd[bin + 1] {
                b = bin + 1;
                break;
            }
        }
        for bin in b..bndend.min(22) {
            if lfe_last(bin) {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak -= fdecay;
            fastleak = fastleak.max(bndpsd[bin] - p.fgain);
            slowleak -= sdecay;
            slowleak = slowleak.max(bndpsd[bin] - sgain);
            excite[bin] = (fastleak - lowcomp).max(slowleak);
        }
        22
    } else {
        bndstrt
    };
    for bin in begin..bndend {
        fastleak -= fdecay;
        fastleak = fastleak.max(bndpsd[bin] - p.fgain);
        slowleak -= sdecay;
        slowleak = slowleak.max(bndpsd[bin] - sgain);
        excite[bin] = fastleak.max(slowleak);
    }
    // masking curve
    let mut mask = [0i32; 50];
    for bin in bndstrt..bndend {
        if bndpsd[bin] < dbknee {
            excite[bin] += (dbknee - bndpsd[bin]) >> 2;
        }
        mask[bin] = excite[bin].max(tables::HTH[g.fscod][bin] as i32);
    }
    // delta bit allocation
    if let Some(segs) = &p.dba {
        let mut band = 0usize;
        for &(off, len, ba) in segs {
            band += off as usize;
            let delta = if ba >= 4 { (ba as i32 - 3) << 7 } else { (ba as i32 - 4) << 7 };
            for _ in 0..len {
                if band < 50 {
                    mask[band] += delta;
                }
                band += 1;
            }
        }
    }
    // bit allocation pointers
    let mut i = start;
    let mut j = tables::MASKTAB[start] as usize;
    loop {
        let lastbin = (tables::BNDTAB[j] as usize + tables::BNDSZ[j] as usize).min(end);
        let mut m = mask[j] - p.snroffset - floor;
        if m < 0 {
            m = 0;
        }
        m &= 0x1fe0;
        m += floor;
        while i < lastbin {
            let addr = ((psd[i] - m) >> 5).clamp(0, 63) as usize;
            bap[i] = tables::BAPTAB[addr];
            i += 1;
        }
        j += 1;
        if end <= lastbin || j >= 50 {
            break;
        }
    }
}

/// Bit allocation parameters common to all channels of a block.
#[derive(Clone, Copy, Default)]
struct Globals {
    fscod: usize,
    sdcycod: usize,
    fdcycod: usize,
    sgaincod: usize,
    dbpbcod: usize,
    floorcod: usize,
}

/// State carried from block to block within a syncframe.
struct Frame {
    nfchans: usize,
    blksw: [bool; 5],
    dithflag: [bool; 5],
    dynrng: [f32; 2],
    cplinu: bool,
    chincpl: [bool; 5],
    phsflginu: bool,
    cplbegf: usize,
    /// cplendf + 3: one past the last coupling sub-band (with spectral extension cplendf may be
    /// negative, §E3.3.1).
    cplend: usize,
    /// Coupling band of each coupling sub-band (relative to cplbegf).
    sub_to_band: [usize; 18],
    ncplbnd: usize,
    cplco: [[f32; 18]; 5],
    phsflg: [bool; 18],
    rematflg: [bool; 4],
    expstr: [u8; 7],
    endmant: [usize; 5],
    exps: [[u8; 256]; 7],
    g: Globals,
    csnroffst: i32,
    fsnroffst: [i32; 7],
    fgaincod: [usize; 7],
    cplleak: (i32, i32),
    deltbae: [u8; 7],
    deltsegs: [Vec<(u8, u8, u8)>; 7],
    bap: [[u8; 256]; 7],
}

impl Frame {
    fn new(nfchans: usize, fscod: usize) -> Frame {
        Frame {
            nfchans,
            blksw: [false; 5],
            dithflag: [false; 5],
            dynrng: [1.0; 2],
            cplinu: false,
            chincpl: [false; 5],
            phsflginu: false,
            cplbegf: 0,
            cplend: 0,
            sub_to_band: [0; 18],
            ncplbnd: 0,
            cplco: [[0.0; 18]; 5],
            phsflg: [false; 18],
            rematflg: [false; 4],
            expstr: [0; 7],
            endmant: [0; 5],
            exps: [[0; 256]; 7],
            g: Globals { fscod, ..Default::default() },
            csnroffst: 0,
            fsnroffst: [0; 7],
            fgaincod: [0; 7],
            cplleak: (0, 0),
            deltbae: [2; 7],
            deltsegs: Default::default(),
            bap: [[0; 256]; 7],
        }
    }
    fn cplstrtmant(&self) -> usize {
        37 + 12 * self.cplbegf
    }
    fn cplendmant(&self) -> usize {
        37 + 12 * self.cplend
    }
}

/// dynrng gain (§7.7.1.2): X (3-bit signed) gives 6.02 dB steps, Y a linear factor in [1/2, 1).
fn dynrng_gain(v: u32) -> f32 {
    let x = ((v as i32) << 24) >> 29;
    let y = (v & 0x1F) as f32;
    2f32.powi(x + 1) * (32.0 + y) / 64.0
}

/// Decode exponents of one set (§7.1.3) into `exp[first..]`; `ngrps` groups of 3 mapped values,
/// each expanded to `grpsize` bins after the absolute exponent.
fn decode_exponents(r: &mut Bits, absexp: i32, ngrps: usize, grpsize: usize, exp: &mut [u8; 256], first: usize, skip_abs: bool) -> Result<()> {
    let mut prev = absexp;
    let mut at = first;
    if !skip_abs {
        exp[at] = absexp.clamp(0, 24) as u8;
        at += 1;
    }
    for _ in 0..ngrps {
        let g = r.read(7) as i32;
        if g > 124 {
            return Err(Error::Invalid("exponent group out of range"));
        }
        for d in [g / 25, (g % 25) / 5, g % 5] {
            prev += d - 2;
            if !(0..=24).contains(&prev) {
                return Err(Error::Invalid("exponent out of range"));
            }
            for _ in 0..grpsize {
                if at < 256 {
                    exp[at] = prev as u8;
                }
                at += 1;
            }
        }
    }
    Ok(())
}

/// Grouped-mantissa buffers shared by all exponent sets of a block (§7.3.5).
#[derive(Default)]
struct Groups {
    b1: ([f32; 3], usize),
    b2: ([f32; 3], usize),
    b4: ([f32; 2], usize),
}

const fn sym(levels: i32, code: i32) -> f32 {
    (2 * code - (levels - 1)) as f32 / levels as f32
}

/// The mantissa value (before the exponent shift) of a bin with allocation `bap`.
fn mantissa(r: &mut Bits, bap: u8, gr: &mut Groups) -> f32 {
    match bap {
        1 => {
            if gr.b1.1 == 0 {
                let g = r.read(5) as i32;
                gr.b1.0 = [sym(3, g / 9), sym(3, (g % 9) / 3), sym(3, g % 3)];
                gr.b1.1 = 3;
            }
            gr.b1.1 -= 1;
            gr.b1.0[2 - gr.b1.1]
        }
        2 => {
            if gr.b2.1 == 0 {
                let g = r.read(7) as i32;
                gr.b2.0 = [sym(5, g / 25), sym(5, (g % 25) / 5), sym(5, g % 5)];
                gr.b2.1 = 3;
            }
            gr.b2.1 -= 1;
            gr.b2.0[2 - gr.b2.1]
        }
        3 => sym(7, r.read(3) as i32),
        4 => {
            if gr.b4.1 == 0 {
                let g = r.read(7) as i32;
                gr.b4.0 = [sym(11, g / 11), sym(11, g % 11)];
                gr.b4.1 = 2;
            }
            gr.b4.1 -= 1;
            gr.b4.0[1 - gr.b4.1]
        }
        5 => sym(15, r.read(4) as i32),
        _ => {
            let bits = QNTZTAB[bap as usize & 15];
            let v = r.read(bits) as i32;
            let v = (v << (32 - bits)) >> (32 - bits);
            v as f32 / (1u32 << (bits - 1)) as f32
        }
    }
}

/// A decoder: keeps the overlap of each channel between frames.
pub struct Decoder {
    delay: [[f32; 256]; 6],
    dither: u32,
    seed: u32,
    /// E-AC-3: the block state of the last syncframe (acmod, lfeon, sample rate code), whose
    /// exponents a syncframe of fewer than six blocks may reuse.
    carry: Option<(usize, bool, usize, Box<Frame>)>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// A decoded syncframe: [`Header::samples`] samples per channel (1536 for AC-3), WAV channel
/// order. No channels for an E-AC-3 syncframe that is not [`Header::is_primary`].
#[derive(Clone, Debug)]
pub struct Decoded {
    pub header: Header,
    pub channels: Vec<Vec<f32>>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder { delay: [[0.0; 256]; 6], dither: 0, seed: 0x1234_5678, carry: None }
    }

    /// Seed the dither generator (dither is decoder-specific noise; tests compare two seeds).
    pub fn set_dither_seed(&mut self, seed: u32) {
        self.seed = seed;
    }

    /// Forget the overlap (before decoding from another position).
    pub fn reset(&mut self) {
        self.delay = [[0.0; 256]; 6];
        self.carry = None;
    }

    fn dither(&mut self) -> f32 {
        // uniform in [-0.707, 0.707] (§7.3.4; any reasonably random sequence)
        self.dither = self.dither.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((self.dither >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.707
    }

    /// Zero-mean, unit-variance noise for spectral extension (§E3.6.4.2.4): uniform in ±√3.
    fn noise(&mut self) -> f32 {
        self.dither() * (3f32.sqrt() / 0.707)
    }

    /// Decode one syncframe.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Decoded> {
        let h = parse_header(frame)?;
        if frame.len() < h.frame_bytes {
            return Err(Error::Invalid("truncated syncframe"));
        }
        if h.is_eac3() {
            return self.decode_eac3(&frame[..h.frame_bytes], h);
        }
        let fscod = (frame[4] >> 6) as usize;
        // Dither is reseeded per syncframe from its CRC words (plus the configured seed), so a
        // frame decodes to the same samples however it was reached (seeking, render caches).
        let crcs = u32::from_be_bytes([frame[2], frame[3], frame[h.frame_bytes - 2], frame[h.frame_bytes - 1]]);
        self.dither = self.seed ^ crcs.wrapping_mul(0x9E37_79B9);
        let mut r = Bits::new(&frame[..h.frame_bytes]);
        r.skip(40); // syncinfo
        // bsi
        r.skip(8); // bsid, bsmod
        let acmod = r.read(3) as usize;
        if acmod & 1 != 0 && acmod != 1 {
            r.skip(2);
        }
        if acmod & 4 != 0 {
            r.skip(2);
        }
        if acmod == 2 {
            r.skip(2);
        }
        let lfeon = r.bit();
        for k in 0..if acmod == 0 { 2 } else { 1 } {
            let _ = k;
            r.skip(5); // dialnorm
            if r.bit() {
                r.skip(8); // compr
            }
            if r.bit() {
                r.skip(8); // langcod
            }
            if r.bit() {
                r.skip(7); // mixlevel, roomtyp
            }
        }
        r.skip(2); // copyrightb, origbs
        if r.bit() {
            r.skip(14);
        }
        if r.bit() {
            r.skip(14);
        }
        if r.bit() {
            let l = r.read(6) as usize;
            r.skip((l + 1) * 8);
        }
        let mut f = Frame::new(NFCHANS[acmod], fscod);
        let pcm = self.blocks(&mut r, &mut f, acmod, lfeon, 6, None)?;
        Ok(Decoded { header: h, channels: wav_order(pcm, acmod, lfeon) })
    }

    /// Decode one E-AC-3 syncframe (`frame` is exactly the syncframe).
    fn decode_eac3(&mut self, frame: &[u8], h: Header) -> Result<Decoded> {
        if !h.is_primary() {
            return Ok(Decoded { header: h, channels: Vec::new() });
        }
        // E-AC-3 has a single CRC, at the end (§E2.2.6): reseed the dither from the last words
        let n = frame.len();
        let tail = frame.get(n.saturating_sub(4)..).and_then(|t| <[u8; 4]>::try_from(t).ok()).unwrap_or_default();
        self.dither = self.seed ^ u32::from_be_bytes(tail).wrapping_mul(0x9E37_79B9);
        let mut r = Bits::new(frame);
        let bsi = eac3::read_bsi(&mut r)?;
        let (acmod, lfeon) = (h.acmod as usize & 7, h.lfeon);
        let nblocks = h.blocks as usize;
        let mut e = eac3::FrameInfo::read(&mut r, &bsi, acmod, lfeon, nblocks, n / 2)?;
        let mut f = match self.carry.take() {
            Some((a, l, fs, f)) if (a, l, fs) == (acmod, lfeon, bsi.fscod) => f,
            _ => Box::new(Frame::new(NFCHANS[acmod], bsi.fscod)),
        };
        let pcm = self.blocks(&mut r, &mut f, acmod, lfeon, nblocks, Some(&mut e))?;
        self.carry = Some((acmod, lfeon, bsi.fscod, f));
        Ok(Decoded { header: h, channels: wav_order(pcm, acmod, lfeon) })
    }

    /// Decode `nblocks` audio blocks and synthesize them (inverse transform, overlap-add), in coded
    /// channel order with the LFE last.
    fn blocks(&mut self, r: &mut Bits, f: &mut Frame, acmod: usize, lfeon: bool, nblocks: usize, mut x: Option<&mut eac3::FrameInfo>) -> Result<Vec<Vec<f32>>> {
        let nfchans = f.nfchans;
        let nout = nfchans + lfeon as usize;
        let mut pcm: Vec<Vec<f32>> = vec![Vec::with_capacity(256 * nblocks); nout];
        for blk in 0..nblocks {
            let mut coefs = [[0f32; 256]; 6];
            self.audio_block(r, f, acmod, lfeon, blk, &mut coefs, x.as_deref_mut())?;
            if r.overrun() {
                return Err(Error::Invalid("audio block runs past the frame"));
            }
            // inverse transform, overlap-add
            let mut buf = [0f32; 512];
            for (ch, out) in pcm.iter_mut().enumerate() {
                let src = if ch < nfchans { ch } else { LFE };
                let short = ch < nfchans && f.blksw[ch];
                imdct(&coefs[src], short, &mut buf);
                let d = &mut self.delay[src];
                for n in 0..256 {
                    out.push(2.0 * (buf[n] + d[n]));
                    d[n] = buf[256 + n];
                }
            }
        }
        Ok(pcm)
    }

    #[allow(clippy::too_many_arguments)]
    fn audio_block(
        &mut self,
        r: &mut Bits,
        f: &mut Frame,
        acmod: usize,
        lfeon: bool,
        blk: usize,
        coefs: &mut [[f32; 256]; 6],
        mut x: Option<&mut eac3::FrameInfo>,
    ) -> Result<()> {
        let nf = f.nfchans;
        let eac3 = x.is_some();
        match x.as_deref() {
            Some(e) if !e.blkswe => f.blksw = [false; 5],
            _ => (0..nf).for_each(|ch| f.blksw[ch] = r.bit()),
        }
        match x.as_deref() {
            // dither on when the flags are not sent (§E2.2.4)
            Some(e) if !e.dithflage => f.dithflag = [true; 5],
            _ => (0..nf).for_each(|ch| f.dithflag[ch] = r.bit()),
        }
        for k in 0..if acmod == 0 { 2 } else { 1 } {
            if r.bit() {
                f.dynrng[k] = dynrng_gain(r.read(8));
            } else if blk == 0 {
                f.dynrng[k] = 1.0;
            }
        }
        // E-AC-3 spectral extension strategy and coordinates
        if let Some(e) = x.as_deref_mut() {
            e.spx.read(r, blk, acmod, nf)?;
        }
        let spx = x.as_deref().map(|e| &e.spx).filter(|s| s.inu);
        let spxbegf = spx.map(|s| s.begf);
        let spx_begin = spx.map(|s| s.begin_bin());
        let spx_chans = spx.map(|s| s.chinspx).unwrap_or([false; 5]);
        // coupling strategy
        let strategy = match x.as_deref() {
            Some(e) => e.cplstre[blk].then_some(e.cplinu[blk]),
            None => r.bit().then(|| r.bit()),
        };
        if let Some(inu) = strategy {
            f.cplinu = inu;
            if f.cplinu {
                if eac3 && r.bit() {
                    return Err(Error::Unsupported("E-AC-3 enhanced coupling".into()));
                }
                if eac3 && acmod == 2 {
                    f.chincpl = [true, true, false, false, false];
                } else {
                    for ch in 0..nf {
                        f.chincpl[ch] = r.bit();
                    }
                }
                if acmod == 2 {
                    f.phsflginu = r.bit();
                }
                f.cplbegf = r.read(4) as usize;
                f.cplend = match spxbegf {
                    // the coupling region ends where spectral extension begins (§E2.2.4)
                    Some(b) if b < 6 => b + 1,
                    Some(b) => 2 * b - 4,
                    None => r.read(4) as usize + 3,
                };
                if f.cplend <= f.cplbegf {
                    return Err(Error::Invalid("coupling end below start"));
                }
                // coupling band structure, by absolute sub-band
                let mut strc = [false; 18];
                match x.as_deref_mut() {
                    Some(e) => {
                        if r.bit() {
                            for s in f.cplbegf + 1..f.cplend {
                                e.cplbndstrc[s] = r.bit();
                            }
                        }
                        strc = e.cplbndstrc;
                    }
                    None => (f.cplbegf + 1..f.cplend).for_each(|s| strc[s] = r.bit()),
                }
                let mut band = 0;
                f.sub_to_band[0] = 0;
                for s in 1..f.cplend - f.cplbegf {
                    if !strc[f.cplbegf + s] {
                        band += 1;
                    }
                    f.sub_to_band[s] = band;
                }
                f.ncplbnd = band + 1;
            } else {
                f.chincpl = [false; 5];
                if let Some(e) = x.as_deref_mut() {
                    e.firstcplcos = [true; 5];
                    e.firstcplleak = true;
                    f.phsflginu = false;
                }
            }
        } else if blk == 0 && !eac3 {
            return Err(Error::Invalid("no coupling strategy in block 0"));
        }
        // coupling coordinates
        if f.cplinu {
            let mut any = false;
            for ch in 0..nf {
                if !f.chincpl[ch] {
                    if let Some(e) = x.as_deref_mut() {
                        e.firstcplcos[ch] = true;
                    }
                    continue;
                }
                let coe = match x.as_deref_mut() {
                    Some(e) if e.firstcplcos[ch] => {
                        e.firstcplcos[ch] = false;
                        true
                    }
                    _ => r.bit(),
                };
                if coe {
                    any = true;
                    let mstr = r.read(2) as i32;
                    for bnd in 0..f.ncplbnd {
                        let e = r.read(4) as i32;
                        let m = r.read(4) as f32;
                        let v = if e == 15 { m / 16.0 } else { (m + 16.0) / 32.0 };
                        let co = v * 2f32.powi(-(e + 3 * mstr));
                        for s in 0..(f.cplend - f.cplbegf) {
                            if f.sub_to_band[s] == bnd {
                                f.cplco[ch][s] = co;
                            }
                        }
                    }
                }
            }
            if acmod == 2 && f.phsflginu && any {
                for bnd in 0..f.ncplbnd {
                    let flag = r.bit();
                    for s in 0..(f.cplend - f.cplbegf) {
                        if f.sub_to_band[s] == bnd {
                            f.phsflg[s] = flag;
                        }
                    }
                }
            }
        }
        // rematrixing (E-AC-3 always sends the flags in block 0)
        if acmod == 2 && ((eac3 && blk == 0) || r.bit()) {
            let n = if f.cplinu {
                match f.cplbegf {
                    0 => 2,
                    1 | 2 => 3,
                    _ => 4,
                }
            } else {
                match spxbegf {
                    Some(b) if b < 2 => 3,
                    _ => 4,
                }
            };
            f.rematflg = [false; 4];
            for k in 0..n {
                f.rematflg[k] = r.bit();
            }
        }
        // exponent strategies (E-AC-3: from the audio frame)
        match x.as_deref() {
            Some(e) => {
                if f.cplinu {
                    f.expstr[CPL] = e.cplexpstr[blk];
                }
                f.expstr[..nf].copy_from_slice(&e.chexpstr[blk][..nf]);
                if lfeon {
                    f.expstr[LFE] = e.lfeexpstr[blk];
                }
            }
            None => {
                if f.cplinu {
                    f.expstr[CPL] = r.read(2) as u8;
                }
                for ch in 0..nf {
                    f.expstr[ch] = r.read(2) as u8;
                }
                if lfeon {
                    f.expstr[LFE] = r.read(1) as u8;
                }
            }
        }
        for ch in 0..nf {
            if f.expstr[ch] != 0 {
                if f.chincpl[ch] && f.cplinu {
                    f.endmant[ch] = f.cplstrtmant();
                } else if let (true, Some(begin)) = (spx_chans[ch], spx_begin) {
                    // §E3.3.3: the channel ends where spectral extension begins
                    f.endmant[ch] = begin;
                } else {
                    let bw = r.read(6) as usize;
                    if bw > 60 {
                        return Err(Error::Invalid("chbwcod > 60"));
                    }
                    f.endmant[ch] = (bw + 12) * 3 + 37;
                }
            }
        }
        // exponents
        if f.cplinu && f.expstr[CPL] != 0 {
            let abs = (r.read(4) as i32) << 1;
            let grpsize = 1 << (f.expstr[CPL] - 1);
            let ngrps = (f.cplendmant() - f.cplstrtmant()) / (3 * grpsize);
            let first = f.cplstrtmant();
            decode_exponents(r, abs, ngrps, grpsize, &mut f.exps[CPL], first, true)?;
        }
        for ch in 0..nf {
            if f.expstr[ch] != 0 {
                let abs = r.read(4) as i32;
                let grpsize = 1usize << (f.expstr[ch] - 1);
                let ngrps = (f.endmant[ch] - 1 + 3 * grpsize - 3) / (3 * grpsize);
                decode_exponents(r, abs, ngrps, grpsize, &mut f.exps[ch], 0, false)?;
                r.skip(2); // gainrng
            }
        }
        if lfeon && f.expstr[LFE] != 0 {
            let abs = r.read(4) as i32;
            decode_exponents(r, abs, 2, 1, &mut f.exps[LFE], 0, false)?;
        }
        // bit allocation parametric information
        match x.as_deref() {
            Some(e) if !e.bamode => {
                // §E2.2.4 defaults
                (f.g.sdcycod, f.g.fdcycod, f.g.sgaincod, f.g.dbpbcod, f.g.floorcod) = (2, 1, 1, 2, 7);
            }
            _ => {
                if r.bit() {
                    f.g.sdcycod = r.read(2) as usize;
                    f.g.fdcycod = r.read(2) as usize;
                    f.g.sgaincod = r.read(2) as usize;
                    f.g.dbpbcod = r.read(2) as usize;
                    f.g.floorcod = r.read(3) as usize;
                } else if blk == 0 && !eac3 {
                    return Err(Error::Invalid("no bit allocation information in block 0"));
                }
            }
        }
        match x.as_deref() {
            Some(e) => {
                // SNR offset strategies (Table E2.9), fast gain codes
                let sets = [CPL, 0, 1, 2, 3, 4, LFE];
                match e.snroffststr {
                    0 => {
                        f.csnroffst = e.frmcsnroffst;
                        sets.iter().for_each(|&c| f.fsnroffst[c] = e.frmfsnroffst);
                    }
                    s if blk == 0 || r.bit() => {
                        f.csnroffst = r.read(6) as i32;
                        if s == 1 {
                            let v = r.read(4) as i32;
                            sets.iter().for_each(|&c| f.fsnroffst[c] = v);
                        } else {
                            if f.cplinu {
                                f.fsnroffst[CPL] = r.read(4) as i32;
                            }
                            for ch in 0..nf {
                                f.fsnroffst[ch] = r.read(4) as i32;
                            }
                            if lfeon {
                                f.fsnroffst[LFE] = r.read(4) as i32;
                            }
                        }
                    }
                    _ => {}
                }
                if e.frmfgaincode && r.bit() {
                    if f.cplinu {
                        f.fgaincod[CPL] = r.read(3) as usize;
                    }
                    for ch in 0..nf {
                        f.fgaincod[ch] = r.read(3) as usize;
                    }
                    if lfeon {
                        f.fgaincod[LFE] = r.read(3) as usize;
                    }
                } else {
                    f.fgaincod = [4; 7];
                }
                if e.strmtyp == 0 && r.bit() {
                    r.skip(10); // convsnroffst
                }
            }
            None => {
                if r.bit() {
                    f.csnroffst = r.read(6) as i32;
                    if f.cplinu {
                        f.fsnroffst[CPL] = r.read(4) as i32;
                        f.fgaincod[CPL] = r.read(3) as usize;
                    }
                    for ch in 0..nf {
                        f.fsnroffst[ch] = r.read(4) as i32;
                        f.fgaincod[ch] = r.read(3) as usize;
                    }
                    if lfeon {
                        f.fsnroffst[LFE] = r.read(4) as i32;
                        f.fgaincod[LFE] = r.read(3) as usize;
                    }
                } else if blk == 0 {
                    return Err(Error::Invalid("no SNR offsets in block 0"));
                }
            }
        }
        if f.cplinu {
            let leak = match x.as_deref_mut() {
                Some(e) if e.firstcplleak => {
                    e.firstcplleak = false;
                    true
                }
                _ => r.bit(),
            };
            if leak {
                f.cplleak = (((r.read(3) as i32) << 8) + 768, ((r.read(3) as i32) << 8) + 768);
            }
        }
        // delta bit allocation
        if blk == 0 {
            f.deltbae = [2; 7];
        }
        if x.as_deref().is_none_or(|e| e.dbaflde) && r.bit() {
            if f.cplinu {
                f.deltbae[CPL] = r.read(2) as u8;
            }
            for ch in 0..nf {
                f.deltbae[ch] = r.read(2) as u8;
            }
            let order: Vec<usize> = f.cplinu.then_some(CPL).into_iter().chain(0..nf).collect();
            for ch in order {
                if f.deltbae[ch] == 1 {
                    let n = r.read(3) as usize + 1;
                    f.deltsegs[ch] = (0..n).map(|_| (r.read(5) as u8, r.read(4) as u8, r.read(3) as u8)).collect();
                } else if f.deltbae[ch] == 3 {
                    return Err(Error::Invalid("reserved delta bit allocation state"));
                }
            }
        }
        // skip field
        if x.as_deref().is_none_or(|e| e.skipflde) && r.bit() {
            let l = r.read(9) as usize;
            r.skip(l * 8);
        }
        // bit allocation for every exponent set
        let all_zero = f.csnroffst == 0 && (0..7).all(|c| f.fsnroffst[c] == 0);
        let snr = |fine: i32| (((f.csnroffst - 15) << 4) + fine) << 2;
        let mut sets: Vec<(usize, AllocParams)> = Vec::new();
        for ch in 0..nf {
            let dba = (f.deltbae[ch] <= 1).then(|| f.deltsegs[ch].clone());
            sets.push((ch, AllocParams { start: 0, end: f.endmant[ch], fgain: FASTGAIN[f.fgaincod[ch]], snroffset: snr(f.fsnroffst[ch]), leak: None, dba }));
        }
        if f.cplinu {
            let dba = (f.deltbae[CPL] <= 1).then(|| f.deltsegs[CPL].clone());
            sets.push((
                CPL,
                AllocParams {
                    start: f.cplstrtmant(),
                    end: f.cplendmant(),
                    fgain: FASTGAIN[f.fgaincod[CPL]],
                    snroffset: snr(f.fsnroffst[CPL]),
                    leak: Some(f.cplleak),
                    dba,
                },
            ));
        }
        if lfeon {
            sets.push((LFE, AllocParams { start: 0, end: 7, fgain: FASTGAIN[f.fgaincod[LFE]], snroffset: snr(f.fsnroffst[LFE]), leak: None, dba: None }));
        }
        for (ch, p) in &sets {
            f.bap[*ch] = [0; 256];
            if !all_zero {
                let exps = f.exps[*ch];
                bit_allocation(&exps, p, &f.g, &mut f.bap[*ch]);
            }
        }
        // mantissas
        let mut gr = Groups::default();
        let mut cpl = [0f32; 256];
        let mut got_cpl = false;
        for ch in 0..nf {
            for bin in 0..f.endmant[ch] {
                let b = f.bap[ch][bin];
                let m = if b == 0 { if f.dithflag[ch] { self.dither() } else { 0.0 } } else { mantissa(r, b, &mut gr) };
                coefs[ch][bin] = m * 2f32.powi(-(f.exps[ch][bin] as i32));
            }
            if f.cplinu && f.chincpl[ch] && !got_cpl {
                got_cpl = true;
                for bin in f.cplstrtmant()..f.cplendmant() {
                    let b = f.bap[CPL][bin];
                    // zero-bit coupling mantissas are dithered per channel when decoupling
                    cpl[bin] = if b == 0 { f32::NAN } else { mantissa(r, b, &mut gr) };
                }
            }
        }
        if lfeon {
            for bin in 0..7 {
                let b = f.bap[LFE][bin];
                let m = if b == 0 { 0.0 } else { mantissa(r, b, &mut gr) };
                coefs[LFE][bin] = m * 2f32.powi(-(f.exps[LFE][bin] as i32));
            }
        }
        // decoupling
        if f.cplinu {
            for ch in 0..nf {
                if !f.chincpl[ch] {
                    continue;
                }
                for bin in f.cplstrtmant()..f.cplendmant() {
                    let s = (bin - 37) / 12 - f.cplbegf;
                    let m = if cpl[bin].is_nan() { if f.dithflag[ch] { self.dither() } else { 0.0 } } else { cpl[bin] };
                    let mut v = m * 2f32.powi(-(f.exps[CPL][bin] as i32)) * f.cplco[ch][s] * 8.0;
                    if acmod == 2 && ch == 1 && f.phsflginu && f.phsflg[s] {
                        v = -v;
                    }
                    coefs[ch][bin] = v;
                }
            }
        }
        // rematrixing
        if acmod == 2 {
            let end = if f.cplinu { f.cplstrtmant() } else { f.endmant[0].min(f.endmant[1]) };
            const B: [usize; 5] = [13, 25, 37, 61, 253];
            for k in 0..4 {
                if f.rematflg[k] {
                    for bin in B[k]..B[k + 1].min(end) {
                        let (l, rr) = (coefs[0][bin], coefs[1][bin]);
                        coefs[0][bin] = l + rr;
                        coefs[1][bin] = l - rr;
                    }
                }
            }
        }
        // spectral extension: synthesize the high band of each channel from its low band
        if let Some(e) = x.as_deref()
            && e.spx.inu
        {
            for ch in 0..nf {
                if e.spx.chinspx[ch] {
                    e.spx.synthesize(ch, e.spxatten[ch], &mut coefs[ch], || self.noise());
                }
            }
        }
        // dynamic range
        for ch in 0..nf {
            let g = if acmod == 0 && ch == 1 { f.dynrng[1] } else { f.dynrng[0] };
            if g != 1.0 {
                coefs[ch].iter_mut().for_each(|c| *c *= g);
            }
        }
        if lfeon && f.dynrng[0] != 1.0 {
            coefs[LFE].iter_mut().for_each(|c| *c *= f.dynrng[0]);
        }
        Ok(())
    }
}

/// Coded channel order (Table 5.8) → WAV / SMPTE order: fronts L R C, LFE, surrounds.
fn wav_order(mut pcm: Vec<Vec<f32>>, acmod: usize, lfeon: bool) -> Vec<Vec<f32>> {
    let nf = NFCHANS[acmod];
    let lfe = if lfeon { pcm.pop() } else { None };
    let mut fbw = pcm;
    // acmod 3, 5, 7 code L C R …: move C after R
    if acmod & 1 == 1 && acmod != 1 {
        let c = fbw.remove(1);
        fbw.insert(2, c);
    }
    let fronts = match acmod {
        1 => 1,
        3 | 5 | 7 => 3,
        _ => 2.min(nf),
    };
    let mut out: Vec<Vec<f32>> = fbw.drain(..fronts).collect();
    if let Some(l) = lfe {
        out.push(l);
    }
    out.extend(fbw);
    out
}

#[cfg(test)]
mod tests;
