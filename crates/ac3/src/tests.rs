use super::*;

#[test]
fn computed_window_matches_table_7_33() {
    let w = window();
    for (n, (&a, &b)) in w.iter().zip(tables::WINDOW_TABLE.iter()).enumerate() {
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
