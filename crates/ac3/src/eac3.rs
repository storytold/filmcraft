//! E-AC-3 (ATSC A/52:2012 Annex E): the syncframe header and bit stream information (§E2.2.2),
//! the audio frame (§E2.2.3) and spectral extension (§E2.2.4, §E3.6). The audio blocks share the
//! AC-3 decoder (`Decoder::audio_block`), which reads the E-AC-3 differences from [`FrameInfo`].

use crate::{Bits, Error, Header, NFCHANS, Result};

/// Sample rates by fscod, and by fscod2 when fscod is 3 (Tables E2.2, E2.3).
const RATES: [u32; 3] = [48_000, 44_100, 32_000];
const REDUCED_RATES: [u32; 3] = [24_000, 22_050, 16_000];
/// Audio blocks per syncframe by numblkscod (Table E2.4).
const BLOCKS: [u8; 4] = [1, 2, 3, 6];

/// Exponent strategies of the six blocks for each frame-based strategy code (Table E2.10):
/// 0 reuse, 1 D15, 2 D25, 3 D45.
const FRAME_EXPSTR: [[u8; 6]; 32] = [
    [1, 0, 0, 0, 0, 0],
    [1, 0, 0, 0, 0, 3],
    [1, 0, 0, 0, 2, 0],
    [1, 0, 0, 0, 3, 3],
    [2, 0, 0, 2, 0, 0],
    [2, 0, 0, 2, 0, 3],
    [2, 0, 0, 3, 2, 0],
    [2, 0, 0, 3, 3, 3],
    [2, 0, 1, 0, 0, 0],
    [2, 0, 2, 0, 0, 3],
    [2, 0, 2, 0, 2, 0],
    [2, 0, 2, 0, 3, 3],
    [2, 0, 3, 2, 0, 0],
    [2, 0, 3, 2, 0, 3],
    [2, 0, 3, 3, 2, 0],
    [2, 0, 3, 3, 3, 3],
    [3, 1, 0, 0, 0, 0],
    [3, 1, 0, 0, 0, 3],
    [3, 2, 0, 0, 2, 0],
    [3, 2, 0, 0, 3, 3],
    [3, 2, 0, 2, 0, 0],
    [3, 2, 0, 2, 0, 3],
    [3, 2, 0, 3, 2, 0],
    [3, 2, 0, 3, 3, 3],
    [3, 3, 1, 0, 0, 0],
    [3, 3, 2, 0, 0, 3],
    [3, 3, 2, 0, 2, 0],
    [3, 3, 2, 0, 3, 3],
    [3, 3, 3, 2, 0, 0],
    [3, 3, 3, 2, 0, 3],
    [3, 3, 3, 3, 2, 0],
    [3, 3, 3, 3, 3, 3],
];

/// Default coupling band structure by coupling sub-band (Table E2.12, "Default Coupling Banding
/// Structure"; sub-band 0 never starts a combined band).
const DEFAULT_CPLBNDSTRC: [bool; 18] = {
    let mut t = [false; 18];
    let ones = [8, 10, 11, 13, 14, 15, 16, 17];
    let mut i = 0;
    while i < ones.len() {
        t[ones[i]] = true;
        i += 1;
    }
    t
};

/// Default spectral extension band structure by sub-band (Table E2.11).
const DEFAULT_SPXBNDSTRC: [bool; 17] = {
    let mut t = [false; 17];
    let mut i = 8;
    while i < 17 {
        t[i] = true;
        i += 2;
    }
    t
};

/// First transform coefficient of spectral extension sub-band `k` (Table E3.13, k ≤ 17).
const fn spx_band_bin(k: usize) -> usize {
    25 + 12 * k
}

/// Spectral extension attenuation (Table E3.14): spxattentab[code][binindex] = 2^(-(code + 1) ·
/// (binindex + 1) / 15), which the table lists to nine decimals.
fn spx_atten(code: usize, binindex: usize) -> f32 {
    let e = (code as f64 + 1.0) * (binindex as f64 + 1.0) / 15.0;
    (-e).exp2() as f32
}

/// Parse the header of an E-AC-3 syncframe (bsid 11-16) at the start of `b` (at least 7 bytes).
pub(crate) fn parse_header(b: &[u8], bsid: u8) -> Result<Header> {
    let mut r = Bits::new(b.get(2..).unwrap_or_default());
    let strmtyp = r.read(2) as u8;
    let substreamid = r.read(3) as u8;
    let words = r.read(11) as usize + 1;
    let fscod = r.read(2) as usize;
    let (sample_rate, blocks) = match RATES.get(fscod) {
        Some(&rate) => (rate, BLOCKS[r.read(2) as usize & 3]),
        None => (*REDUCED_RATES.get(r.read(2) as usize).ok_or(Error::Invalid("reserved fscod2"))?, 6),
    };
    let acmod = r.read(3) as u8;
    let lfeon = r.bit();
    if strmtyp == 3 {
        return Err(Error::Unsupported("reserved E-AC-3 stream type 3".into()));
    }
    let frame_bytes = words * 2;
    if frame_bytes < 8 {
        return Err(Error::Invalid("E-AC-3 syncframe too short"));
    }
    // frame bits per second of audio
    let bits = frame_bytes as u64 * 8 * u64::from(sample_rate) / (256 * u64::from(blocks));
    let bitrate_kbps = u32::try_from(bits / 1000).unwrap_or(u32::MAX);
    Ok(Header { sample_rate, bitrate_kbps, frame_bytes, bsid, acmod, lfeon, blocks, strmtyp, substreamid })
}

/// What the audio frame and blocks need from the bit stream information.
pub(crate) struct Bsi {
    pub strmtyp: u8,
    /// Sample rate code for the bit allocation's hearing threshold: fscod, or fscod2 at the
    /// reduced rates.
    pub fscod: usize,
    pub numblkscod: usize,
}

/// Read syncinfo and bsi (§E2.2.1-2), leaving `r` at the audio frame.
pub(crate) fn read_bsi(r: &mut Bits) -> Result<Bsi> {
    r.skip(16); // syncword
    let strmtyp = r.read(2) as u8;
    r.skip(3 + 11); // substreamid, frmsiz
    let fscod = r.read(2) as usize;
    let (rate_code, numblkscod) = if fscod == 3 {
        let fscod2 = r.read(2) as usize;
        if fscod2 == 3 {
            return Err(Error::Invalid("reserved fscod2"));
        }
        (fscod2, 3)
    } else {
        (fscod, r.read(2) as usize)
    };
    let nblocks = BLOCKS[numblkscod & 3] as usize;
    let acmod = r.read(3) as usize;
    let lfeon = r.bit();
    r.skip(5 + 5); // bsid, dialnorm
    if r.bit() {
        r.skip(8); // compr
    }
    if acmod == 0 {
        r.skip(5); // dialnorm2
        if r.bit() {
            r.skip(8); // compr2
        }
    }
    if strmtyp == 1 && r.bit() {
        r.skip(16); // chanmap
    }
    if r.bit() {
        // mixing metadata
        if acmod > 2 {
            r.skip(2); // dmixmod
        }
        if acmod & 1 != 0 && acmod > 2 {
            r.skip(6); // ltrtcmixlev, lorocmixlev
        }
        if acmod & 4 != 0 {
            r.skip(6); // ltrtsurmixlev, lorosurmixlev
        }
        if lfeon && r.bit() {
            r.skip(5); // lfemixlevcod
        }
        if strmtyp == 0 {
            if r.bit() {
                r.skip(6); // pgmscl
            }
            if acmod == 0 && r.bit() {
                r.skip(6); // pgmscl2
            }
            if r.bit() {
                r.skip(6); // extpgmscl
            }
            match r.read(2) {
                1 => r.skip(5), // premixcmpsel, drcsrc, premixcmpscl
                2 => r.skip(12),
                // mixdeflen: the mixdata field (through mixdatafill) is mixdeflen + 2 bytes
                3 => {
                    let len = r.read(5) as usize;
                    r.skip(8 * (len + 2));
                }
                _ => {}
            }
            if acmod < 2 {
                if r.bit() {
                    r.skip(14); // panmean, paninfo
                }
                if acmod == 0 && r.bit() {
                    r.skip(14); // panmean2, paninfo2
                }
            }
            if r.bit() {
                // frame mixing configuration information
                if numblkscod == 0 {
                    r.skip(5);
                } else {
                    for _ in 0..nblocks {
                        if r.bit() {
                            r.skip(5);
                        }
                    }
                }
            }
        }
    }
    if r.bit() {
        // informational metadata
        r.skip(5); // bsmod, copyrightb, origbs
        if acmod == 2 {
            r.skip(4); // dsurmod, dheadphonmod
        }
        if acmod >= 6 {
            r.skip(2); // dsurexmod
        }
        if r.bit() {
            r.skip(8); // mixlevel, roomtyp, adconvtyp
        }
        if acmod == 0 && r.bit() {
            r.skip(8); // mixlevel2, roomtyp2, adconvtyp2
        }
        if fscod < 3 {
            r.skip(1); // sourcefscod
        }
    }
    if strmtyp == 0 && numblkscod != 3 {
        r.skip(1); // convsync
    }
    if strmtyp == 2 && (numblkscod == 3 || r.bit()) {
        r.skip(6); // blkid → frmsizecod
    }
    if r.bit() {
        let l = r.read(6) as usize;
        r.skip((l + 1) * 8); // addbsi
    }
    Ok(Bsi { strmtyp, fscod: rate_code, numblkscod })
}

/// The audio frame (§E2.2.3) and the state that runs from block to block within a syncframe.
pub(crate) struct FrameInfo {
    pub strmtyp: u8,
    pub blkswe: bool,
    pub dithflage: bool,
    pub bamode: bool,
    pub frmfgaincode: bool,
    pub dbaflde: bool,
    pub skipflde: bool,
    pub snroffststr: u8,
    pub frmcsnroffst: i32,
    pub frmfsnroffst: i32,
    pub cplstre: [bool; 6],
    pub cplinu: [bool; 6],
    pub cplexpstr: [u8; 6],
    pub chexpstr: [[u8; 5]; 6],
    pub lfeexpstr: [u8; 6],
    /// Spectral extension attenuation code per channel (spxattencod), when in use.
    pub spxatten: [Option<usize>; 5],
    pub firstcplcos: [bool; 5],
    pub firstcplleak: bool,
    /// Coupling band structure by absolute coupling sub-band.
    pub cplbndstrc: [bool; 18],
    pub spx: Spx,
}

impl FrameInfo {
    /// Read the audio frame. `words` is the syncframe size in 16-bit words.
    pub(crate) fn read(r: &mut Bits, bsi: &Bsi, acmod: usize, lfeon: bool, nblocks: usize, words: usize) -> Result<FrameInfo> {
        let nf = NFCHANS[acmod & 7];
        let six = bsi.numblkscod == 3;
        let (expstre, ahte) = if six { (r.bit(), r.bit()) } else { (true, false) };
        let snroffststr = r.read(2) as u8;
        if snroffststr == 3 {
            return Err(Error::Invalid("reserved SNR offset strategy"));
        }
        let transproce = r.bit();
        let mut e = FrameInfo {
            strmtyp: bsi.strmtyp,
            blkswe: r.bit(),
            dithflage: r.bit(),
            bamode: r.bit(),
            frmfgaincode: r.bit(),
            dbaflde: r.bit(),
            skipflde: r.bit(),
            snroffststr,
            frmcsnroffst: 0,
            frmfsnroffst: 0,
            cplstre: [false; 6],
            cplinu: [false; 6],
            cplexpstr: [0; 6],
            chexpstr: [[0; 5]; 6],
            lfeexpstr: [0; 6],
            spxatten: [None; 5],
            firstcplcos: [true; 5],
            firstcplleak: true,
            cplbndstrc: DEFAULT_CPLBNDSTRC,
            spx: Spx::new(),
        };
        let spxattene = r.bit();
        if acmod > 1 {
            e.cplstre[0] = true;
            e.cplinu[0] = r.bit();
            for blk in 1..nblocks {
                e.cplstre[blk] = r.bit();
                e.cplinu[blk] = if e.cplstre[blk] { r.bit() } else { e.cplinu[blk - 1] };
            }
        }
        if expstre {
            for blk in 0..nblocks {
                if e.cplinu[blk] {
                    e.cplexpstr[blk] = r.read(2) as u8;
                }
                for ch in 0..nf {
                    e.chexpstr[blk][ch] = r.read(2) as u8;
                }
            }
        } else {
            // frame-based strategies (six blocks only)
            if acmod > 1 && e.cplinu.iter().any(|&c| c) {
                e.cplexpstr = FRAME_EXPSTR[r.read(5) as usize & 31];
            }
            for ch in 0..nf {
                let s = FRAME_EXPSTR[r.read(5) as usize & 31];
                for (blk, &v) in s.iter().enumerate() {
                    e.chexpstr[blk][ch] = v;
                }
            }
        }
        if lfeon {
            for blk in 0..nblocks {
                e.lfeexpstr[blk] = r.read(1) as u8;
            }
        }
        if bsi.strmtyp == 0 && (six || r.bit()) {
            r.skip(5 * nf); // convexpstr
        }
        if ahte {
            // AHT flags exist for channels whose exponents are sent once in the frame (§E3.4.2)
            let unsupported = || Err(Error::Unsupported("E-AC-3 adaptive hybrid transform".into()));
            let ncplblks = e.cplinu.iter().filter(|&&c| c).count();
            let ncplregs = (0..6).filter(|&b| e.cplstre[b] || e.cplexpstr[b] != 0).count();
            if ncplblks == 6 && ncplregs == 1 && r.bit() {
                return unsupported();
            }
            for ch in 0..nf {
                if (0..6).filter(|&b| e.chexpstr[b][ch] != 0).count() == 1 && r.bit() {
                    return unsupported();
                }
            }
            if lfeon && e.lfeexpstr.iter().filter(|&&s| s != 0).count() == 1 && r.bit() {
                return unsupported();
            }
        }
        if snroffststr == 0 {
            e.frmcsnroffst = r.read(6) as i32;
            e.frmfsnroffst = r.read(4) as i32;
        }
        if transproce {
            // transient pre-noise processing (§E3.7) is not applied
            for _ in 0..nf {
                if r.bit() {
                    r.skip(10 + 8); // transprocloc, transproclen
                }
            }
        }
        if spxattene {
            for ch in 0..nf {
                if r.bit() {
                    e.spxatten[ch] = Some(r.read(5) as usize);
                }
            }
        }
        if bsi.numblkscod != 0 && r.bit() {
            // blkstrtinfo: (blocks - 1) × (4 + ceil(log2(words per frame))) bits (§E2.3.2.27)
            let log2 = (usize::BITS - words.saturating_sub(1).leading_zeros()) as usize;
            r.skip((nblocks - 1) * (4 + log2));
        }
        Ok(e)
    }
}

/// Spectral extension parameters (§E2.2.4, §E3.6), carried from block to block of a syncframe.
pub(crate) struct Spx {
    pub inu: bool,
    pub chinspx: [bool; 5],
    pub begf: usize,
    strtf: usize,
    /// First sub-band and one past the last (spx_begin_subbnd, spx_end_subbnd).
    begin: usize,
    end: usize,
    bndstrc: [bool; 17],
    /// Band sizes in coefficients (spxbndsztab) and their count (nspxbnds).
    sizes: [usize; 17],
    nbands: usize,
    first: [bool; 5],
    co: [[f32; 17]; 5],
    nblend: [[f32; 17]; 5],
    sblend: [[f32; 17]; 5],
}

impl Spx {
    fn new() -> Spx {
        Spx {
            inu: false,
            chinspx: [false; 5],
            begf: 0,
            strtf: 0,
            begin: 0,
            end: 0,
            bndstrc: DEFAULT_SPXBNDSTRC,
            sizes: [0; 17],
            nbands: 0,
            first: [true; 5],
            co: [[0.0; 17]; 5],
            nblend: [[0.0; 17]; 5],
            sblend: [[1.0; 17]; 5],
        }
    }

    /// First coefficient of the extension region.
    pub fn begin_bin(&self) -> usize {
        spx_band_bin(self.begin)
    }

    /// Read the strategy and coordinates of block `blk`.
    pub(crate) fn read(&mut self, r: &mut Bits, blk: usize, acmod: usize, nf: usize) -> Result<()> {
        if blk == 0 || r.bit() {
            self.inu = r.bit();
            if self.inu {
                if acmod == 1 {
                    self.chinspx = [true, false, false, false, false];
                } else {
                    for ch in 0..nf {
                        self.chinspx[ch] = r.bit();
                    }
                }
                self.strtf = r.read(2) as usize;
                self.begf = r.read(3) as usize;
                let endf = r.read(3) as usize;
                self.begin = if self.begf < 6 { self.begf + 2 } else { self.begf * 2 - 3 };
                self.end = if endf < 3 { endf + 5 } else { endf * 2 + 3 };
                if r.bit() {
                    for b in self.begin + 1..self.end {
                        self.bndstrc[b] = r.bit();
                    }
                }
                if self.begin >= self.end {
                    return Err(Error::Invalid("spectral extension ends before it begins"));
                }
                if self.strtf >= self.begin {
                    return Err(Error::Invalid("spectral extension copies from above its start"));
                }
                self.nbands = 1;
                self.sizes = [0; 17];
                self.sizes[0] = 12;
                for b in self.begin + 1..self.end {
                    if self.bndstrc[b] {
                        self.sizes[self.nbands - 1] += 12;
                    } else {
                        self.sizes[self.nbands] = 12;
                        self.nbands += 1;
                    }
                }
            } else {
                self.chinspx = [false; 5];
                self.first = [true; 5];
            }
        }
        if !self.inu {
            return Ok(());
        }
        for ch in 0..nf {
            if !self.chinspx[ch] {
                self.first[ch] = true;
                continue;
            }
            let coe = std::mem::replace(&mut self.first[ch], false) || r.bit();
            if !coe {
                continue;
            }
            let blend = r.read(5) as f32 / 32.0;
            let mstr = r.read(2) as i32;
            // coordinates (§E3.6.3) and blending factors (§E3.6.4.2.1)
            let mut mant = self.begin_bin() as f32;
            let end = spx_band_bin(self.end) as f32;
            for b in 0..self.nbands {
                let e = r.read(4) as i32;
                let m = r.read(2) as f32;
                let v = if e == 15 { m / 4.0 } else { (m + 4.0) / 8.0 };
                self.co[ch][b] = v * 2f32.powi(-(e + 3 * mstr));
                let size = self.sizes[b] as f32;
                let nratio = ((mant + 0.5 * size) / end - blend).clamp(0.0, 1.0);
                self.nblend[ch][b] = nratio.sqrt();
                self.sblend[ch][b] = (1.0 - nratio).sqrt();
                mant += size;
            }
        }
        Ok(())
    }

    /// Synthesize the extension region of one channel's coefficients (§E3.6.4): translate the
    /// low band up, notch-filter the borders when attenuation is on, blend with `noise` (zero
    /// mean, unit variance) scaled to the band's energy, and scale by the coordinates.
    pub(crate) fn synthesize(&self, ch: usize, atten: Option<usize>, tc: &mut [f32; 256], mut noise: impl FnMut() -> f32) {
        let copystart = spx_band_bin(self.strtf);
        let copyend = self.begin_bin();
        let sizes = &self.sizes[..self.nbands];
        // translation
        let mut wrap = [false; 17];
        let (mut copy, mut insert) = (copystart, copyend);
        for (b, &size) in sizes.iter().enumerate() {
            if copy + size > copyend {
                copy = copystart;
                wrap[b] = true;
            }
            for _ in 0..size {
                if copy == copyend {
                    copy = copystart;
                }
                if let (Some(&v), true) = (tc.get(copy), insert < tc.len()) {
                    tc[insert] = v;
                }
                insert += 1;
                copy += 1;
            }
        }
        // banded RMS energy of the translated coefficients
        let mut rms = [0f32; 17];
        let mut at = copyend;
        for (b, &size) in sizes.iter().enumerate() {
            let band = tc.get(at..at + size).unwrap_or_default();
            rms[b] = (band.iter().map(|v| v * v).sum::<f32>() / size as f32).sqrt();
            at += size;
        }
        // notch filter around the baseband / extension border and each wrap point
        if let Some(code) = atten {
            let g = [spx_atten(code, 0), spx_atten(code, 1), spx_atten(code, 2)];
            let mut notch = |center: usize| {
                for (k, gain) in [g[0], g[1], g[2], g[1], g[0]].into_iter().enumerate() {
                    if let Some(v) = (center + k).checked_sub(2).and_then(|i| tc.get_mut(i)) {
                        *v *= gain;
                    }
                }
            };
            notch(copyend);
            let mut start = copyend;
            for (b, &size) in sizes.iter().enumerate() {
                if b > 0 && wrap[b] {
                    notch(start);
                }
                start += size;
            }
        }
        // noise blending and scaling
        let mut at = copyend;
        for (b, &size) in sizes.iter().enumerate() {
            let nscale = rms[b] * self.nblend[ch][b];
            let sscale = self.sblend[ch][b];
            let co = self.co[ch][b] * 32.0;
            for _ in 0..size {
                let n = noise();
                if let Some(v) = tc.get_mut(at) {
                    *v = (*v * sscale + n * nscale) * co;
                }
                at += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuation_matches_table_e3_14() {
        // rows 0, 4, 14 and 31 as printed
        let rows: [(usize, [f64; 3]); 4] = [
            (0, [0.954841604, 0.911722489, 0.870550563]),
            (4, [0.793700526, 0.629960525, 0.500000000]),
            (14, [0.500000000, 0.250000000, 0.125000000]),
            (31, [0.227930622, 0.051952369, 0.011841536]),
        ];
        for (code, want) in rows {
            for (i, w) in want.into_iter().enumerate() {
                assert!((f64::from(spx_atten(code, i)) - w).abs() < 1e-7, "{code} {i}");
            }
        }
    }

    #[test]
    fn default_band_structures() {
        assert_eq!(DEFAULT_CPLBNDSTRC.iter().filter(|&&b| b).count(), 8);
        assert!(DEFAULT_CPLBNDSTRC[8] && !DEFAULT_CPLBNDSTRC[9] && !DEFAULT_CPLBNDSTRC[12] && DEFAULT_CPLBNDSTRC[17]);
        assert_eq!(DEFAULT_SPXBNDSTRC.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect::<Vec<_>>(), vec![8, 10, 12, 14, 16]);
        // every frame-based strategy sends new exponents in block 0
        assert!(FRAME_EXPSTR.iter().all(|s| s[0] != 0));
    }

    #[test]
    fn spectral_extension_translation_and_scaling() {
        // begin at sub-band 2 (bin 49), end at 5 (bin 85), copy from sub-band 0 (bin 25): three
        // 12-bin bands, the third wraps back to the copy start
        let mut s = Spx::new();
        s.inu = true;
        (s.strtf, s.begin, s.end, s.nbands) = (0, 2, 5, 3);
        s.sizes[..3].copy_from_slice(&[12, 12, 12]);
        s.co[0][..3].copy_from_slice(&[1.0 / 32.0, 0.5 / 32.0, 0.25 / 32.0]);
        s.nblend[0] = [0.0; 17];
        s.sblend[0] = [1.0; 17];
        let mut tc = [0f32; 256];
        for (i, v) in tc[25..49].iter_mut().enumerate() {
            *v = i as f32 + 1.0;
        }
        s.synthesize(0, None, &mut tc, || 0.0);
        assert_eq!(tc[49], 1.0);
        assert_eq!(tc[61], 13.0 * 0.5);
        assert_eq!(tc[73], 1.0 * 0.25);
        assert_eq!(tc[84], 12.0 * 0.25);
        assert_eq!(tc[85], 0.0);
        // pure noise: the band gets the energy of its translated coefficients
        s.nblend[0] = [1.0; 17];
        s.sblend[0] = [0.0; 17];
        s.co[0] = [1.0 / 32.0; 17];
        let mut tc2 = [0f32; 256];
        tc2[25..49].fill(2.0);
        let mut k = 0;
        s.synthesize(0, None, &mut tc2, || {
            k += 1;
            if k % 2 == 0 { 1.0 } else { -1.0 }
        });
        assert!(tc2[49..85].iter().all(|v| v.abs() == 2.0));
    }
}
