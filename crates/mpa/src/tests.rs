use super::*;
use tables::window_half;

/// A frame header: MPEG-1 unless `lsf`, `layer` 1/2, no CRC, `bitrate_index`, `rate_index`
/// (0: 44.1 kHz, 1: 48 kHz, 2: 32 kHz), `mode` 0-3 and `mode_extension`.
fn header(lsf: bool, layer: u8, bitrate_index: u8, rate_index: u8, mode: u8, ext: u8) -> [u8; 4] {
    let version = if lsf { 0b10 } else { 0b11 };
    [0xFF, 0xE0 | version << 3 | (4 - layer) << 1 | 1, bitrate_index << 4 | rate_index << 2, mode << 6 | ext << 4]
}

/// A frame of `header` with all-zero audio data: no subband has an allocation.
fn silent(h: [u8; 4]) -> Vec<u8> {
    let mut f = h.to_vec();
    f.resize(parse_header(&h).unwrap().frame_bytes, 0);
    f
}

#[test]
fn headers() {
    // Layer II 48 kHz stereo 192 kbit/s: 144 * 192000 / 48000 = 576 bytes
    let h = parse_header(&header(false, 2, 10, 1, 0, 0)).unwrap();
    assert_eq!((h.version, h.layer, h.bitrate_kbps, h.sample_rate, h.frame_bytes, h.channels(), h.samples()), (Version::Mpeg1, 2, 192, 48_000, 576, 2, 1152));
    // ffmpeg's default for mono: Layer II 384 kbit/s, which ISO/IEC 11172-3 does not allow
    let h = parse_header(&header(false, 2, 14, 1, 3, 0)).unwrap();
    assert_eq!((h.bitrate_kbps, h.frame_bytes, h.mode, h.channels()), (384, 1152, Mode::Mono, 1));
    // Layer I 44.1 kHz 384 kbit/s with padding: (12 * 384000 / 44100 + 1) * 4 = 420
    let mut b = header(false, 1, 12, 0, 1, 2);
    b[2] |= 2;
    let h = parse_header(&b).unwrap();
    assert_eq!((h.layer, h.frame_bytes, h.samples(), h.mode, h.mode_extension, h.bound()), (1, 420, 384, Mode::JointStereo, 2, 12));
    // MPEG-2 Layer II 24 kHz 160 kbit/s: 144 * 160000 / 24000 = 960; Layer I 22.05 kHz 256 kbit/s
    let h = parse_header(&header(true, 2, 14, 1, 0, 0)).unwrap();
    assert_eq!((h.version, h.sample_rate, h.frame_bytes), (Version::Mpeg2, 24_000, 960));
    let h = parse_header(&header(true, 1, 14, 0, 0, 0)).unwrap();
    assert_eq!((h.sample_rate, h.bitrate_kbps, h.frame_bytes), (22_050, 256, 4 * (12 * 256_000 / 22_050)));
    // MPEG-2.5 and Layer III headers parse too
    assert_eq!(parse_header(&[0xFF, 0xE5, 0x10, 0xC0]).unwrap().sample_rate, 11_025);
    let h = parse_header(&[0xFF, 0xFB, 0x90, 0x44]).unwrap();
    assert_eq!((h.layer, h.frame_bytes), (3, 417));
}

#[test]
fn bad_headers_are_errors() {
    assert_eq!(parse_header(&[]), Err(Error::Invalid("truncated header")));
    assert_eq!(parse_header(&[0xFF, 0xFD]), Err(Error::Invalid("truncated header")));
    assert_eq!(parse_header(&[0x00, 0xFD, 0x90, 0x00]), Err(Error::NoSync));
    assert_eq!(parse_header(&[0xFF, 0x1D, 0x90, 0x00]), Err(Error::NoSync));
    assert_eq!(parse_header(&[0xFF, 0xED, 0x90, 0x00]), Err(Error::Invalid("reserved version")));
    assert_eq!(parse_header(&[0xFF, 0xF9, 0x90, 0x00]), Err(Error::Invalid("reserved layer")));
    assert_eq!(parse_header(&[0xFF, 0xFD, 0xF0, 0x00]), Err(Error::Invalid("forbidden bit rate index")));
    assert_eq!(parse_header(&[0xFF, 0xFD, 0x9C, 0x00]), Err(Error::Invalid("reserved sampling frequency")));
    let mut d = Decoder::new();
    assert_eq!(d.decode(&[0xFF, 0xFB, 0x90, 0x44]).unwrap_err(), Error::Unsupported("Layer III"));
    assert_eq!(d.decode(&[0xFF, 0xFD, 0x04, 0x00, 0, 0]).unwrap_err(), Error::Unsupported("free-format bit rate"));
    assert_eq!(d.decode(&silent(header(false, 2, 10, 1, 0, 0))[..100]).unwrap_err(), Error::Invalid("truncated frame"));
}

#[test]
fn silent_frames_decode_to_silence() {
    let mut d = Decoder::new();
    for (h, ch, n) in [
        (header(false, 2, 14, 1, 3, 0), 1, 1152),
        (header(false, 2, 1, 2, 0, 0), 2, 1152),
        (header(true, 2, 8, 0, 1, 3), 2, 1152),
        (header(false, 1, 6, 0, 2, 0), 2, 384),
        (header(false, 1, 4, 1, 3, 0), 1, 384),
    ] {
        let f = d.decode(&silent(h)).unwrap();
        assert_eq!(f.channels.len(), ch, "{h:?}");
        assert!(f.channels.iter().all(|c| c.len() == n && c.iter().all(|&s| s == 0.0)), "{h:?}");
    }
}

/// Table 3-B.3 as typed in must be the smooth prototype filter: with every other block of 64
/// negated back, its third differences stay within the rounding of the 2⁻¹⁶ units (a mistyped
/// coefficient shows up as a spike of hundreds).
#[test]
fn synthesis_window_is_the_smooth_prototype() {
    let h: Vec<f64> = (0..=256).map(|i| window_half(i).unwrap() as f64 * if (i / 64) % 2 == 1 { -1.0 } else { 1.0 }).collect();
    let d1: Vec<f64> = h.windows(2).map(|w| w[1] - w[0]).collect();
    let d2: Vec<f64> = d1.windows(2).map(|w| w[1] - w[0]).collect();
    let worst = d2.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
    assert!(worst <= 8.0, "{worst}");
    assert_eq!(window_half(257), None);
    let w = tables::window();
    // symmetry of the full window and the table's endpoints
    assert_eq!((w[0], w[256], w[511], w[448]), (0.0, 75_038.0 / 65_536.0, 1.0 / 65_536.0, w[64]));
    assert_eq!(w[300], -w[212]);
}

#[test]
fn layer2_allocation_tables() {
    use tables::AllocTable as T;
    for t in [T::A, T::B, T::C, T::D, T::Lsf] {
        for sb in 0..t.sblimit() {
            let row = t.row(sb);
            assert!(row.len().is_power_of_two() && row[0] == 0, "{t:?} {sb}");
        }
    }
    // the allowed combinations (Table 3-B.2) and the ones encoders write anyway
    assert_eq!(T::select(false, 48_000, 192), T::A);
    assert_eq!(T::select(false, 44_100, 64), T::A);
    assert_eq!(T::select(false, 44_100, 96), T::B);
    assert_eq!(T::select(false, 32_000, 192), T::B);
    assert_eq!(T::select(false, 48_000, 32), T::C);
    assert_eq!(T::select(false, 44_100, 48), T::C);
    assert_eq!(T::select(false, 32_000, 48), T::D);
    assert_eq!(T::select(false, 48_000, 384), T::A);
    assert_eq!(T::select(false, 44_100, 384), T::B);
    assert_eq!(T::select(false, 32_000, 16), T::D);
    assert_eq!(T::select(true, 24_000, 160), T::Lsf);
}

#[test]
fn requantization_is_symmetric() {
    assert_eq!([0, 1, 2].map(|v| requantize(v, 3)), [-2.0 / 3.0, 0.0, 2.0 / 3.0]);
    assert_eq!(requantize(4, 9), 0.0);
    assert_eq!(requantize(0, 65_535), -65_534.0 / 65_535.0);
}

/// Random audio data behind valid headers, and damaged headers: errors or noise, never a panic.
#[test]
fn random_frames_never_panic() {
    let mut x = 0x1234_5678u32;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let mut d = Decoder::new();
    for k in 0..4000 {
        let mut h = header(k % 3 == 0, 1 + (k % 2) as u8, 1 + (rnd() % 14) as u8, (rnd() % 3) as u8, (rnd() % 4) as u8, (rnd() % 4) as u8);
        if k % 5 == 0 {
            h[1] &= 0xFE; // CRC present
        }
        if k % 7 == 0 {
            h[2] ^= rnd() as u8;
            h[3] ^= rnd() as u8;
        }
        let len = parse_header(&h).map(|p| p.frame_bytes).unwrap_or(64);
        let mut f = h.to_vec();
        f.extend((0..len.saturating_sub(4) + (rnd() % 8) as usize).map(|_| rnd() as u8));
        if k % 11 == 0 {
            f.truncate(rnd() as usize % f.len());
        }
        if let Ok(fr) = d.decode(&f) {
            assert!(fr.channels.iter().flatten().all(|s| s.is_finite()));
        }
    }
}

/// Output of the decoder, bit for bit, on deterministic synthetic streams.
///
/// Each stream is a run of valid headers (mono, stereo, joint stereo at several bounds and dual
/// channel; Layer I and II; MPEG-1 and MPEG-2; several bit rates and sampling frequencies)
/// followed by xorshift noise, decoded by one `Decoder` so that the synthesis filter's history
/// carries from frame to frame. Frames the decoder rejects are counted and skipped. The hash is
/// FNV-1a over the bits of every decoded sample.
///
/// The golden values were captured from the decoder before the synthesis filterbank was
/// rewritten (`synthesize` with the shifting `V` buffer and the 64-row matrix): check out that
/// commit, run `cargo test -p filmcraft-mpa decoded_samples_are_bit_exact -- --nocapture`, and
/// copy the printed `(ok, hash)` pairs. A change of the filter's arithmetic (summation order,
/// grouping, fused multiply-add) shows up here; a legitimate one needs new goldens and a reason.
#[test]
fn decoded_samples_are_bit_exact() {
    // (lsf, layer, bitrate_index, rate_index, mode, mode_extension)
    const STREAMS: &[(bool, u8, u8, u8, u8, u8)] = &[
        (false, 2, 10, 1, 0, 0),
        (false, 2, 14, 1, 3, 0),
        (false, 2, 6, 0, 1, 0),
        (false, 2, 9, 0, 1, 3),
        (false, 2, 5, 2, 2, 0),
        (false, 2, 2, 1, 0, 0),
        (true, 2, 8, 0, 1, 2),
        (true, 2, 4, 2, 3, 0),
        (false, 1, 12, 0, 1, 2),
        (false, 1, 14, 1, 0, 0),
        (false, 1, 8, 2, 3, 0),
        (true, 1, 10, 0, 2, 0),
    ];
    let mut got = vec![];
    for (n, &(lsf, layer, br, rate, mode, ext)) in STREAMS.iter().enumerate() {
        let mut x = 0x9E37_79B9u32 ^ (n as u32).wrapping_mul(0x85EB_CA6B);
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let mut d = Decoder::new();
        let (mut ok, mut hash) = (0u32, 0xCBF2_9CE4_8422_2325u64);
        for _ in 0..60 {
            let h = header(lsf, layer, br, rate, mode, ext);
            let len = parse_header(&h).map_or(0, |p| p.frame_bytes);
            let mut f = h.to_vec();
            // sparse noise: most allocations are zero, so many frames are valid and quiet
            f.extend((0..len.saturating_sub(4)).map(|_| (rnd() & rnd() & rnd()) as u8));
            if let Ok(fr) = d.decode(&f) {
                ok += 1;
                for s in fr.channels.iter().flatten() {
                    hash = (hash ^ u64::from(s.to_bits())).wrapping_mul(0x0000_0100_0000_01B3);
                }
            }
        }
        got.push((ok, hash));
    }
    println!("{got:?}");
    const GOLDEN: &[(u32, u64)] = &[
        (59, 0xea8448e85fe3df1b),
        (60, 0xc6ecc7f674c186d4),
        (60, 0xb38a4d7cba7bfbde),
        (59, 0xf37f8a5abff50363),
        (60, 0x1e489c563c40c852),
        (49, 0x52ab5c8f9db13cc1),
        (60, 0x401b8039ec831783),
        (60, 0x3555c5d49d12ecca),
        (59, 0xb063c5f8c79edaef),
        (58, 0xcedb49aa85eb2e29),
        (58, 0x3ee32a2c6b9da56b),
        (57, 0xbb913530021557b1),
    ];
    assert_eq!(got, GOLDEN);
}

/// `reset` forgets the filter history, including where the ring buffer of `V` starts.
#[test]
fn reset_matches_a_fresh_decoder() {
    let mut x = 0x2545_F491u32;
    let mut f = header(false, 2, 10, 1, 0, 0).to_vec();
    f.extend((0..572).map(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x & (x >> 7) & (x >> 13)) as u8
    }));
    let first = Decoder::new().decode(&f).unwrap();
    let mut d = Decoder::new();
    for _ in 0..3 {
        d.decode(&f).unwrap();
    }
    d.reset();
    let again = d.decode(&f).unwrap();
    assert!(first.channels.iter().flatten().any(|&s| s != 0.0));
    assert_eq!(first.channels, again.channels);
}
