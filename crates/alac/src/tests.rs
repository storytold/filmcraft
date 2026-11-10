//! A reference decoder of whole frames (Apple's rules) and round trips through it. The ffmpeg and
//! symphonia oracles are in `tests/`.

use super::*;
use crate::ag::tests::Reader;

/// Decode one frame written by [`Encoder`] into planar samples.
fn decode_frame(data: &[u8], cookie: &[u8], frame_length: usize) -> Vec<Vec<i32>> {
    let bits = u32::from(cookie[5]);
    let channels = usize::from(cookie[9]);
    let mut out = vec![Vec::new(); channels];
    let mut r = Reader::new(data, 0);
    let mut ch = 0;
    loop {
        let id = r.bits(3);
        if id == ID_END {
            break;
        }
        let pair = id == ID_CPE;
        let _tag = r.bits(4);
        assert_eq!(r.bits(12), 0);
        let partial = r.bits(1) == 1;
        let shift = r.bits(2);
        let escape = r.bits(1) == 1;
        let n = if partial { r.bits(32) as usize } else { frame_length };
        let nch = 1 + usize::from(pair);
        if escape {
            for _ in 0..n {
                for c in 0..nch {
                    let v = r.bits(bits) as i32;
                    out[ch + c].push(predict::sign_extend(v, bits));
                }
            }
            ch += nch;
            continue;
        }
        let mix_bits = r.bits(8);
        let mix_res = r.bits(8) as u8 as i8 as i32;
        let mut heads = Vec::new();
        for _ in 0..nch {
            let md = r.bits(8);
            let po = r.bits(8);
            let order = (po & 31) as usize;
            let coefs: Vec<i16> = (0..order).map(|_| r.bits(16) as u16 as i16).collect();
            heads.push((md & 15, po >> 5, coefs));
        }
        let sh = shift * 8;
        let mut low = vec![vec![0i32; n]; nch];
        for j in 0..n {
            for l in low.iter_mut() {
                l[j] = r.bits(sh) as i32;
            }
        }
        let chan_bits = bits - sh + u32::from(pair);
        let mut planes = Vec::new();
        for (den, pbf, mut coefs) in heads {
            let res = r.decode(n, ag::AgParams { mb: 10, pb: 40 * pbf / 4, kb: 14 }, chan_bits);
            planes.push(predict::reconstruct(&res, &mut coefs, chan_bits, den));
        }
        if pair && mix_res != 0 {
            let (u, v) = (&planes[0], &planes[1]);
            let l: Vec<i32> = u.iter().zip(v).map(|(&a, &b)| a + b - ((mix_res * b) >> mix_bits)).collect();
            let rr: Vec<i32> = l.iter().zip(v).map(|(&a, &b)| a - b).collect();
            planes = vec![l, rr];
        }
        for (c, p) in planes.into_iter().enumerate() {
            out[ch + c].extend(p.iter().zip(&low[c]).map(|(&h, &lo)| (h << sh) | lo));
        }
        ch += nch;
    }
    out
}

fn noise(seed: &mut u64) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Test material at `bits`: music-like tones with a little noise, white noise, silence with
/// clicks, and a full-scale square.
fn signal(kind: &str, n: usize, bits: u8, seed: u64) -> Vec<i32> {
    let max = ((1i64 << (bits - 1)) - 1) as f64;
    let mut s = seed | 1;
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let v = match kind {
                "music" => 0.5 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() + 0.3 * (2.0 * std::f64::consts::PI * 1375.0 * t).sin() + 0.01 * noise(&mut s),
                "noise" => noise(&mut s),
                "clicks" => f64::from(u8::from(i % 3001 == 0)),
                "square" => {
                    if (i / 37) % 2 == 0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => 0.0,
            };
            ((v * max).round() as i64).clamp(-(max as i64) - 1, max as i64) as i32
        })
        .collect()
}

fn round_trip(planar: &[Vec<i32>], cfg: EncoderConfig) -> usize {
    let mut enc = Encoder::new(cfg).unwrap();
    let refs: Vec<&[i32]> = planar.iter().map(Vec::as_slice).collect();
    let mut frames = enc.encode_frames(&refs).unwrap();
    frames.extend(enc.finish());
    let cookie = enc.magic_cookie();
    let mut got = vec![Vec::new(); planar.len()];
    for f in &frames {
        for (g, d) in got.iter_mut().zip(decode_frame(f, &cookie, cfg.frame_length as usize)) {
            g.extend(d);
        }
    }
    assert_eq!(got.len(), planar.len());
    for (c, (g, want)) in got.iter().zip(planar).enumerate() {
        assert!(g == want, "channel {c} differs ({} vs {} samples)", g.len(), want.len());
    }
    assert_eq!(u32::from_be_bytes(cookie[12..16].try_into().unwrap()), frames.iter().map(Vec::len).max().unwrap_or(0) as u32);
    frames.iter().map(Vec::len).sum()
}

#[test]
fn every_signal_depth_and_layout_round_trips() {
    for bits in [16u8, 20, 24] {
        for kind in ["music", "noise", "clicks", "square", "silence"] {
            for ch in 1u8..=8 {
                let planar: Vec<Vec<i32>> = (0..ch).map(|c| signal(kind, 10_000, bits, u64::from(c) * 31 + u64::from(bits))).collect();
                round_trip(&planar, EncoderConfig::new(48_000, ch, bits));
            }
        }
    }
}

#[test]
fn compresses_music_and_stores_noise_verbatim() {
    let planar = vec![signal("music", 48_000, 16, 1), signal("music", 48_000, 16, 2)];
    let size = round_trip(&planar, EncoderConfig::new(48_000, 2, 16));
    // the tones carry noise at 1 % of full scale: about 9.6 bits of entropy per sample, 60 % of raw
    assert!(size * 100 < 48_000 * 2 * 2 * 72, "music: {size} bytes");
    // a quiet, clean signal shrinks far more
    let quiet: Vec<i32> = signal("music", 48_000, 16, 5).iter().map(|v| v / 64).collect();
    let size = round_trip(&[quiet.clone(), quiet.iter().map(|v| v / 2).collect()], EncoderConfig::new(48_000, 2, 16));
    // noise of about ±3 left: some 4.5 bits per sample with the code's overhead, under a third of raw
    assert!(size * 3 < 48_000 * 2 * 2, "quiet music: {size} bytes");
    // identical channels: mixing makes the side channel zero
    let mono = signal("music", 48_000, 16, 3);
    let same = round_trip(&[mono.clone(), mono.clone()], EncoderConfig::new(48_000, 2, 16));
    let one = round_trip(&[mono], EncoderConfig::new(48_000, 1, 16));
    assert!(same < one + one / 4, "identical stereo {same} vs mono {one}");
    // white noise does not compress: frames fall back to verbatim, a little over raw size
    let noise = vec![signal("noise", 20_000, 24, 4)];
    let size = round_trip(&noise, EncoderConfig::new(48_000, 1, 24));
    assert!(size <= 20_000 * 3 + 64 * 8, "noise: {size} bytes");
    // stereo noise too: verbatim pairs at 24 bits
    let noise2 = vec![signal("noise", 20_000, 24, 5), signal("noise", 20_000, 24, 6)];
    let size = round_trip(&noise2, EncoderConfig::new(48_000, 2, 24));
    assert!(size <= 20_000 * 6 + 64 * 8, "stereo noise: {size} bytes");
}

#[test]
fn frame_lengths_and_partial_frames() {
    for (fl, n) in [(4096u32, 1usize), (4096, 4095), (4096, 4096), (4096, 4097), (16, 1000), (16_384, 40_000), (1152, 5000)] {
        let planar = vec![signal("music", n, 16, 7), signal("square", n, 16, 8)];
        round_trip(&planar, EncoderConfig { frame_length: fl, ..EncoderConfig::new(44_100, 2, 16) });
    }
}

#[test]
fn magic_cookie_layout() {
    let enc = Encoder::new(EncoderConfig::new(96_000, 6, 24)).unwrap();
    let c = enc.magic_cookie();
    assert_eq!(c.len(), 24);
    assert_eq!(&c[..4], &4096u32.to_be_bytes());
    assert_eq!(&c[4..12], &[0, 24, 40, 10, 14, 6, 0, 255]);
    assert_eq!(&c[12..20], &[0; 8], "max frame bytes and bitrate unknown before encoding");
    assert_eq!(&c[20..], &96_000u32.to_be_bytes());
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    for cfg in [
        EncoderConfig::new(48_000, 0, 16),
        EncoderConfig::new(48_000, 9, 16),
        EncoderConfig::new(48_000, 2, 8),
        EncoderConfig::new(48_000, 2, 32),
        EncoderConfig::new(48_000, 2, 33),
        EncoderConfig::new(0, 2, 16),
        EncoderConfig { frame_length: 0, ..EncoderConfig::new(48_000, 2, 16) },
        EncoderConfig { frame_length: 65_536, ..EncoderConfig::new(48_000, 2, 16) },
    ] {
        assert!(Encoder::new(cfg).is_err(), "{cfg:?}");
    }
    let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 16)).unwrap();
    assert_eq!(enc.encode_frames(&[&[0, 1]]), Err(Error::Channels { expected: 2, got: 1 }));
    assert_eq!(enc.encode_frames(&[&[0, 1], &[0]]), Err(Error::UnequalChannels));
    assert_eq!(enc.encode_frames(&[&[40_000], &[0]]), Err(Error::SampleRange(40_000)));
    assert!(enc.finish().is_none());
    // the extremes of 24 bits are fine, one past them is not
    let mut enc = Encoder::new(EncoderConfig::new(48_000, 1, 24)).unwrap();
    assert!(enc.encode_frames(&[&[-(1 << 23), (1 << 23) - 1]]).is_ok());
    assert_eq!(enc.encode_frames(&[&[1 << 23]]), Err(Error::SampleRange(1 << 23)));
}

#[test]
fn chunking_does_not_change_the_frames() {
    let planar: Vec<Vec<i32>> = (0..2).map(|c| signal("music", 30_000, 16, c + 3)).collect();
    let cfg = EncoderConfig { frame_length: 1152, ..EncoderConfig::new(48_000, 2, 16) };
    let encode = |chunk: usize| {
        let mut enc = Encoder::new(cfg).unwrap();
        let mut frames = Vec::new();
        let mut at = 0;
        while at < 30_000 {
            let end = (at + chunk).min(30_000);
            let parts: Vec<&[i32]> = planar.iter().map(|c| &c[at..end]).collect();
            frames.extend(enc.encode_frames(&parts).unwrap());
            at = end;
        }
        frames.extend(enc.finish());
        frames
    };
    let whole = encode(30_000);
    assert_eq!(whole.len(), 30_000usize.div_ceil(1152));
    for chunk in [1, 999, 1152, 4800, 7000] {
        assert!(encode(chunk) == whole, "chunks of {chunk}");
    }
}
