use super::*;

#[test]
fn computed_window_matches_table_7_33() {
    let w = window();
    for (n, (&a, &b)) in w.iter().zip(tables::WINDOW_TABLE.iter()).enumerate() {
        let b = b as f32 * 1e-5;
        assert!((a - b).abs() <= 6e-6, "w[{n}] = {a}, table {b}");
    }
    // princen-bradley: w[n]² + w[255-n]²... (the second half mirrors): w[n]² + w[511-n]² = 1
    for n in 0..256 {
        let m = w[255 - n];
        assert!((w[n] * w[n] + m * m - 1.0).abs() < 1e-5);
    }
}

#[test]
fn tables_are_consistent() {
    // masktab maps every bin of a band to that band
    for b in 0..50 {
        for bin in tables::BNDTAB[b] as usize..(tables::BNDTAB[b] + tables::BNDSZ[b]) as usize {
            if bin < 253 {
                assert_eq!(tables::MASKTAB[bin] as usize, b, "bin {bin}");
            }
        }
    }
    assert_eq!(tables::BNDTAB[49] as usize + tables::BNDSZ[49] as usize, 253);
    // latab is non-increasing from 64 to 0
    assert_eq!(tables::LATAB[0], 64);
    assert!(tables::LATAB.windows(2).all(|w| w[0] >= w[1]));
    assert!(tables::BAPTAB.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!((tables::BAPTAB[0], tables::BAPTAB[63]), (0, 15));
}

#[test]
fn inverse_fft_matches_the_definition() {
    for n in [64usize, 128] {
        let re0: Vec<f32> = (0..n).map(|i| ((i * 7 + 3) % 11) as f32 - 5.0).collect();
        let im0: Vec<f32> = (0..n).map(|i| ((i * 5 + 1) % 13) as f32 - 6.0).collect();
        let (mut re, mut im) = (re0.clone(), im0.clone());
        ifft(&mut re, &mut im);
        for t in 0..n {
            let (mut sr, mut si) = (0f64, 0f64);
            for k in 0..n {
                let a = 2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                sr += re0[k] as f64 * a.cos() - im0[k] as f64 * a.sin();
                si += re0[k] as f64 * a.sin() + im0[k] as f64 * a.cos();
            }
            assert!((sr - re[t] as f64).abs() < 1e-3 && (si - im[t] as f64).abs() < 1e-3, "n {n} t {t}");
        }
    }
}

#[test]
fn headers_and_gains() {
    // 48 kHz, 448 kb/s, 3/2 + LFE: bsid 8, bsmod 0, acmod 7 (cmixlev, surmixlev), lfeon
    let b = [0x0B, 0x77, 0, 0, 30, 8 << 3, 0b1110_0001, 0b0100_0000];
    let h = parse_header(&b).unwrap();
    assert_eq!((h.sample_rate, h.bitrate_kbps, h.frame_bytes, h.acmod, h.lfeon, h.channels()), (48_000, 448, 1792, 7, true, 6));
    assert_eq!(parse_header(&[0x0B, 0x77, 0, 0, 0, 17 << 3, 0, 0]), Err(Error::Unsupported("bsid 17".into())));
    assert_eq!(dynrng_gain(0), 1.0);
    assert!((dynrng_gain(0b1110_0000) - 0.5).abs() < 1e-7);
    assert!((dynrng_gain(0b0111_1111) - 16.0 * 63.0 / 64.0).abs() < 1e-5);
}

#[test]
fn channel_order() {
    let ch = |n: usize| (0..n).map(|i| vec![i as f32]).collect::<Vec<_>>();
    let ids = |v: Vec<Vec<f32>>| v.iter().map(|c| c[0] as usize).collect::<Vec<_>>();
    // coded L C R Ls Rs LFE → L R C LFE Ls Rs
    assert_eq!(ids(wav_order(ch(6), 7, true)), vec![0, 2, 1, 5, 3, 4]);
    assert_eq!(ids(wav_order(ch(3), 2, true)), vec![0, 1, 2]);
    assert_eq!(ids(wav_order(ch(1), 1, false)), vec![0]);
}

#[test]
fn garbage_never_panics() {
    let mut d = Decoder::new();
    let mut x = 0xACu32;
    for len in [0usize, 5, 8, 100, 2000] {
        for _ in 0..200 {
            let mut f: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            if len >= 6 {
                f[0] = 0x0B;
                f[1] = 0x77;
                f[4] &= 0x3F;
                f[5] = (f[5] & 7) | (8 << 3);
            }
            let _ = d.decode(&f);
        }
    }
}

/// MSB-first bit writer for synthetic E-AC-3 syncframes.
#[derive(Default)]
struct W {
    b: Vec<u8>,
    n: usize,
}

impl W {
    fn put(&mut self, bits: u32, v: u32) {
        for i in (0..bits).rev() {
            if self.n.is_multiple_of(8) {
                self.b.push(0);
            }
            let bit = ((v >> i) & 1) as u8;
            *self.b.last_mut().unwrap() |= bit << (7 - self.n % 8);
            self.n += 1;
        }
    }
}

/// An E-AC-3 syncframe of `words` 16-bit words: syncinfo and a bsi with no optional metadata
/// (fscod < 3 with `numblkscod`), then `audio` writes the rest; zero padded.
fn eac3_frame(strmtyp: u32, substreamid: u32, words: u32, numblkscod: u32, acmod: u32, audio: impl FnOnce(&mut W)) -> Vec<u8> {
    let mut w = W::default();
    w.put(16, 0x0B77);
    w.put(2, strmtyp);
    w.put(3, substreamid);
    w.put(11, words - 1);
    w.put(2, 0); // 48 kHz
    w.put(2, numblkscod);
    w.put(3, acmod);
    w.put(1, 0); // lfeon
    w.put(5, 16); // bsid
    w.put(5, 31); // dialnorm
    w.put(1, 0); // compre
    if strmtyp == 1 {
        w.put(1, 0); // chanmape
    }
    w.put(2, 0); // mixmdate, infomdate
    if strmtyp == 0 && numblkscod != 3 {
        w.put(1, 0); // convsync
    }
    w.put(1, 0); // addbsie
    audio(&mut w);
    let mut b = w.b;
    b.resize(words as usize * 2, 0);
    b
}

#[test]
fn eac3_headers() {
    let f = eac3_frame(0, 0, 384, 3, 2, |_| {});
    let h = parse_header(&f).unwrap();
    assert_eq!((h.sample_rate, h.blocks, h.samples(), h.frame_bytes, h.bitrate_kbps, h.channels()), (48_000, 6, 1536, 768, 192, 2));
    assert!(h.is_eac3() && h.is_primary());
    // one block per syncframe: 256 samples
    let h = parse_header(&eac3_frame(0, 0, 100, 0, 7, |_| {})).unwrap();
    assert_eq!((h.blocks, h.samples(), h.channels(), h.bitrate_kbps), (1, 256, 5, 300));
    // reduced sample rate (fscod 3, fscod2 1): six blocks at 22.05 kHz
    let mut f = eac3_frame(0, 0, 384, 0, 2, |_| {});
    f[4] = 0b1101_0100;
    let h = parse_header(&f).unwrap();
    assert_eq!((h.sample_rate, h.blocks, h.acmod), (22_050, 6, 2));
    f[4] = 0b1111_0100;
    assert_eq!(parse_header(&f), Err(Error::Invalid("reserved fscod2")));
    // a dependent substream and a second independent program are recognised and skipped
    let mut d = Decoder::new();
    for (strmtyp, id) in [(1, 0), (0, 1)] {
        let f = eac3_frame(strmtyp, id, 64, 3, 2, |_| {});
        let out = d.decode(&f).unwrap();
        assert!(!out.header.is_primary() && out.channels.is_empty());
    }
    assert_eq!(parse_header(&eac3_frame(3, 0, 64, 3, 2, |_| {})), Err(Error::Unsupported("reserved E-AC-3 stream type 3".into())));
}

#[test]
fn eac3_tools_we_do_not_decode_are_refused() {
    // mono, six blocks, AHT enabled and used: chexpstr D15 then reuse, so chahtinu is sent
    let aht = eac3_frame(0, 0, 64, 3, 1, |w| {
        w.put(1, 1); // expstre
        w.put(1, 1); // ahte
        w.put(2, 0); // snroffststr
        w.put(8, 0); // transproce … spxattene
        w.put(2, 1); // chexpstr[0] = D15
        w.put(10, 0); // chexpstr[1..6] = reuse
        w.put(5, 0); // convexpstr
        w.put(1, 1); // chahtinu
    });
    assert_eq!(Decoder::new().decode(&aht).unwrap_err(), Error::Unsupported("E-AC-3 adaptive hybrid transform".into()));
    // stereo with coupling in block 0 using enhanced coupling
    let ecpl = eac3_frame(0, 0, 64, 3, 2, |w| {
        w.put(1, 1); // expstre
        w.put(1, 0); // ahte
        w.put(2, 0); // snroffststr
        w.put(8, 0);
        w.put(1, 1); // cplinu[0]
        w.put(5, 0); // cplstre[1..6]
        w.put(6, 0b01_01_01); // block 0: cplexpstr, chexpstr × 2
        w.put(30, 0); // blocks 1-5: reuse
        w.put(10, 0); // convexpstr
        w.put(10, 0); // frmcsnroffst, frmfsnroffst
        w.put(1, 0); // blkstrtinfoe
        w.put(1, 0); // dynrnge
        w.put(1, 0); // spxinu
        w.put(1, 1); // ecplinu
    });
    assert_eq!(Decoder::new().decode(&ecpl).unwrap_err(), Error::Unsupported("E-AC-3 enhanced coupling".into()));
}

#[test]
fn eac3_single_block_frames_reuse_exponents_across_syncframes() {
    // mono, one block per syncframe; the first sends exponents (all 10, bandwidth code 0), the
    // second reuses them. Zero SNR offsets allocate no bits: every coefficient is dither.
    let frame = |new_exps: bool| {
        eac3_frame(0, 0, 32, 0, 1, |w| {
            // (expstre and ahte are not sent with fewer than six blocks)
            w.put(2, 0); // snroffststr
            w.put(8, 0); // transproce … spxattene
            w.put(2, new_exps as u32); // chexpstr[0]: D15 or reuse
            w.put(1, 0); // convexpstre
            w.put(10, 0); // frmcsnroffst, frmfsnroffst
            w.put(1, 0); // dynrnge
            w.put(1, 0); // spxinu
            if new_exps {
                w.put(6, 0); // chbwcod → 73 coefficients
                w.put(4, 10); // absolute exponent
                for _ in 0..24 {
                    w.put(7, 62); // three zero deltas
                }
                w.put(2, 0); // gainrng
            }
            w.put(1, 0); // convsnroffste
        })
    };
    let (first, second) = (frame(true), frame(false));
    let mut d = Decoder::new();
    let a = d.decode(&first).unwrap();
    assert_eq!((a.header.samples(), a.channels.len(), a.channels[0].len()), (256, 1, 256));
    let b = d.decode(&second).unwrap();
    let peak = |c: &[f32]| c.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak(&b.channels[0]) > 1e-5 && peak(&b.channels[0]) < 0.1, "{}", peak(&b.channels[0]));
    // after a reset there are no exponents to reuse: no coefficients, silence
    d.reset();
    let c = d.decode(&second).unwrap();
    assert_eq!(peak(&c.channels[0]), 0.0);
}

#[test]
fn eac3_garbage_never_panics() {
    let mut d = Decoder::new();
    let mut x = 0x5EEDu32;
    for len in [8usize, 40, 300, 1500, 4096] {
        for _ in 0..300 {
            let mut f: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            f[0] = 0x0B;
            f[1] = 0x77;
            // independent substream 0, frmsiz within the buffer, bsid 11-16
            let words = (len / 2).clamp(4, 2048) - 1;
            f[2] = (words >> 8) as u8 & 7;
            f[3] = words as u8;
            f[5] = (f[5] & 7) | ((11 + x % 6) as u8) << 3;
            let _ = d.decode(&f);
        }
    }
}

/// Golden hashes of the decoder output, so that a change of the bit reader cannot change a
/// decoded sample or an error. The values were made on upstream/main (68919ed8), before the
/// private bit-by-bit reader was replaced by the shared one: every frame below is decoded with a
/// fresh decoder, and every output sample bit (as `f32::to_bits`, channel after channel) and every
/// error (its `Debug` text) goes into an FNV-1a 64 hash. The frames are deterministic: valid
/// AC-3 and E-AC-3 syncframe headers (all fscod, frmsizecod, bsid, acmod and lfeon values in
/// the tables below) with random, zero-heavy or all-ones payload bits, plus truncated and
/// bit-flipped variants of the same frames.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// One payload byte for the given flavour: random, zero-heavy, or ones-heavy.
fn payload_byte(rng: &mut Rng, flavour: usize) -> u8 {
    let v = rng.next();
    match flavour % 3 {
        0 => (v >> 7) as u8,
        1 => ((v >> 7) as u8) & ((v >> 19) as u8) & ((v >> 3) as u8),
        _ => ((v >> 7) as u8) | ((v >> 19) as u8) | ((v >> 3) as u8),
    }
}

/// See `synthetic_frames`: 12 syncframes, back to back.
const REAL_FRAMES: &[u8] = include_bytes!("golden_frames.bin");

fn synthetic_frames() -> Vec<Vec<u8>> {
    let mut rng = Rng(0x00C0_FFEE);
    let mut frames = Vec::new();
    // AC-3: bsid 8 and the half / quarter rate variants 9 and 10, every fscod and a spread of
    // frmsizecod, acmod 0 to 7, lfeon off and on.
    for n in 0..1200usize {
        let fscod = n % 3;
        let frmsizecod = (n * 7 + n / 3) % 38;
        let bsid = [8u8, 8, 8, 9, 10, 6][n % 6];
        let acmod = (n / 3) % 8;
        let lfeon = (n / 24) % 2;
        let Ok(h) = parse_header(&[0x0B, 0x77, 0, 0, ((fscod << 6) | frmsizecod) as u8, bsid << 3, (acmod as u8) << 5, 0]) else { continue };
        let mut f = vec![0u8; h.frame_bytes];
        for b in f.iter_mut() {
            *b = payload_byte(&mut rng, n);
        }
        f[0] = 0x0B;
        f[1] = 0x77;
        f[4] = ((fscod << 6) | frmsizecod) as u8;
        f[5] = bsid << 3;
        // acmod in the top 3 bits; the mix-level bits that follow depend on acmod and are random
        f[6] = (f[6] & 0x1F) | ((acmod as u8) << 5);
        // lfeon follows the optional 2-bit fields (cmixlev, surmixlev, dsurmod)
        let mut at = 3;
        if acmod & 1 != 0 && acmod != 1 {
            at += 2;
        }
        if acmod & 4 != 0 {
            at += 2;
        }
        if acmod == 2 {
            at += 2;
        }
        let bit = 48 + at;
        f[bit / 8] = (f[bit / 8] & !(0x80 >> (bit % 8))) | ((lfeon as u8) * (0x80 >> (bit % 8)));
        frames.push(f);
    }
    // E-AC-3: strmtyp 0 / 1 / 2, fscod 0..=3 (3 = reduced rate), numblkscod, acmod, lfeon
    for n in 0..800usize {
        let words = 24 + (n * 13) % 400;
        let fscod = n % 4;
        let blk = (n / 4) % 4;
        let acmod = (n / 16) % 8;
        let lfeon = (n / 128) % 2;
        let strmtyp = [0u32, 0, 0, 1, 2][n % 5];
        let bsid = 11 + (n % 6) as u32;
        let mut f = vec![0u8; words * 2];
        for b in f.iter_mut() {
            *b = payload_byte(&mut rng, n);
        }
        let head: u32 = (strmtyp << 30)
            | (((n % 7) as u32) << 27)
            | (((words - 1) as u32) << 16)
            | ((fscod as u32) << 14)
            | ((if fscod == 3 { (n / 7 % 3) as u32 } else { blk as u32 }) << 12)
            | ((acmod as u32) << 9)
            | ((lfeon as u32) << 8)
            | (bsid << 3)
            | (f[5] as u32 & 7);
        f[0] = 0x0B;
        f[1] = 0x77;
        f[2..6].copy_from_slice(&head.to_be_bytes());
        frames.push(f);
    }
    // real frames (two of each stream, made with ffmpeg's ac3 / eac3 encoders from tones and noise:
    // mono 64k 32 kHz, coupled stereo 96k 44.1 kHz, noise stereo 96k, 5.1 192k; E-AC-3 noise
    // stereo 96k and 5.1 192k) reach exponents, bit allocation and mantissas of every block, which
    // random payload bits almost never do. Each one is decoded as is, with 60 bit-flipped copies
    // and cut at every 16th byte.
    let mut p = 0;
    while let Ok(h) = parse_header(&REAL_FRAMES[p..]) {
        let f = &REAL_FRAMES[p..p + h.frame_bytes];
        p += h.frame_bytes;
        frames.push(f.to_vec());
        for _ in 0..60 {
            let mut m = f.to_vec();
            for _ in 0..1 + rng.next() % 3 {
                let bit = 64 + rng.next() as usize % (m.len() * 8 - 64);
                m[bit / 8] ^= 0x80 >> (bit % 8);
            }
            frames.push(m);
        }
        for cut in (8..f.len()).step_by(16) {
            frames.push(f[..cut].to_vec());
        }
    }
    // truncated and bit-flipped variants of every 5th frame
    let base = frames.clone();
    for (i, f) in base.iter().enumerate().step_by(5) {
        let mut t = f.clone();
        t.truncate(rng.next() as usize % f.len());
        frames.push(t);
        let mut m = f.clone();
        for _ in 0..1 + rng.next() % 6 {
            let bit = rng.next() as usize % (m.len() * 8);
            m[bit / 8] ^= 0x80 >> (bit % 8);
        }
        frames.push(m);
        // cut the payload to just a few bytes after the header, so that the reads run past the end
        let mut s = f.clone();
        s.truncate((8 + i % 40).min(f.len()));
        frames.push(s);
    }
    frames
}

#[test]
fn decoded_output_matches_golden_hashes() {
    let (mut all, mut ok, mut err, mut audio) = (Fnv::new(), 0usize, 0usize, 0usize);
    for (i, f) in synthetic_frames().iter().enumerate() {
        let mut dec = Decoder::new();
        // decode each frame twice with the same decoder: the second pass also covers the
        // E-AC-3 carry-over and the overlap state
        for _ in 0..2 {
            match dec.decode(f) {
                Ok(d) => {
                    ok += 1;
                    audio += usize::from(!d.channels.is_empty());
                    all.bytes(&[1]);
                    for ch in &d.channels {
                        for s in ch {
                            all.bytes(&s.to_bits().to_le_bytes());
                        }
                    }
                }
                Err(e) => {
                    err += 1;
                    all.bytes(format!("{i}:{e:?}").as_bytes());
                }
            }
        }
    }
    eprintln!("golden: ok {ok} ({audio} with audio) err {err} hash {:#018x}", all.0);
    assert_eq!((ok, err, all.0), GOLDEN);
}

const GOLDEN: (usize, usize, u64) = (2960, 7024, 0x1dc9_31d6_0e89_e8c8);

/// The private reader this crate used before it moved to the shared `BitReader`, verbatim.
struct OldBits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> OldBits<'a> {
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

#[test]
fn bits_match_the_old_bit_by_bit_reader() {
    let mut rng = Rng(0x5EED_0001);
    for round in 0..3000usize {
        let len = (rng.next() % 40) as usize;
        let data: Vec<u8> = (0..len).map(|_| payload_byte(&mut rng, round)).collect();
        let (mut old, mut new) = (OldBits::new(&data), Bits::new(&data));
        for op in 0..80 {
            match rng.next() % 5 {
                // widths 0 to 40 include more than 32 bits, where only the low 32 bits are kept
                0 | 1 => {
                    let n = rng.next() % 41;
                    assert_eq!(old.read(n), new.read(n), "round {round} op {op}: read({n})");
                }
                2 => assert_eq!(old.bit(), new.bit(), "round {round} op {op}: bit"),
                3 => {
                    let n = (rng.next() % 70) as usize;
                    old.skip(n);
                    new.skip(n);
                }
                _ => {}
            }
            assert_eq!(old.overrun(), new.overrun(), "round {round} op {op}: overrun");
            assert_eq!(old.pos, new.r.position(), "round {round} op {op}: position");
        }
    }
}
