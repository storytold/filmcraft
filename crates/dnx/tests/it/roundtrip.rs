//! Encoder → decoder round trips and robustness against corrupt data (no ffmpeg needed).

use filmcraft_dnx::{ChromaFormat, DecodeOptions, Encoder, EncoderConfig, Frame, Profile, decode_frame, decode_frame_with, probe};

/// A synthetic picture: gradients, a ring pattern and some hashed texture.
fn picture(w: u32, h: u32, chroma: ChromaFormat, depth: u8, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, false);
    let max = (1u32 << depth) - 1;
    let s = depth as u32 - 8;
    let hash = |x: u32, y: u32| (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ seed.wrapping_mul(83492791)) % 17;
    for y in 0..h {
        for x in 0..w {
            let r = (((x as f32 - w as f32 / 2.0).powi(2) + (y as f32 - h as f32 / 2.0).powi(2)).sqrt() / 6.0).sin();
            let v = (16 << s) as f32 + (219 << s) as f32 * (0.5 + 0.3 * r + 0.2 * (x as f32 / w as f32 - 0.5)) + (hash(x, y) << s) as f32;
            f.y[(y * w + x) as usize] = (v as u32).min(max) as u16;
        }
    }
    let cw = f.chroma_width();
    for y in 0..f.chroma_height() {
        for x in 0..cw {
            let i = (y * cw + x) as usize;
            f.cb[i] = (((128 + (x * 90 / cw) as i32 - 45) as u32) << s) as u16;
            f.cr[i] = (((128 + (y * 80 / f.chroma_height()) as i32 - 40) as u32) << s) as u16;
        }
    }
    f
}

fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (peak * peak / mse).log10() }
}

#[test]
fn roundtrip_all_profiles() {
    for (profile, depth, w, h) in [
        (Profile::Lb, 8, 640, 360),
        (Profile::Sq, 8, 1280, 720),
        (Profile::Hq, 8, 720, 486),
        (Profile::Hqx, 10, 1000, 562),
        (Profile::Hqx, 12, 512, 288),
        (Profile::R444, 10, 400, 300),
        (Profile::R444, 12, 64, 48),
    ] {
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.bit_depth = depth;
        let chroma = cfg.chroma();
        let mut enc = Encoder::with_config(cfg).unwrap();
        for seed in 0..2 {
            let src = picture(w, h, chroma, depth, seed);
            let data = enc.encode(&src).unwrap();
            assert_eq!(data.len() as u32, enc.frame_size());
            assert_eq!(&data[data.len() - 4..], &[0x60, 0x0D, 0xC0, 0xDE]);
            let hdr = probe(&data).unwrap();
            assert_eq!((hdr.cid, hdr.width, hdr.lines, hdr.bit_depth), (profile.cid(), w, h, depth));
            let dec = decode_frame(&data).unwrap();
            let single = decode_frame_with(&data, &DecodeOptions { threads: false, ..Default::default() }).unwrap();
            assert_eq!(dec, single, "threaded and single-threaded decodes differ");
            assert_eq!((dec.width, dec.height, dec.chroma, dec.bit_depth, dec.rgb), (w, h, chroma, depth, false));
            let peak = ((1u32 << depth) - 1) as f64;
            let p = psnr(&src.y, &dec.y, peak);
            let pc = psnr(&src.cb, &dec.cb, peak);
            let min = if profile == Profile::Lb { 30.0 } else { 38.0 };
            assert!(p > min && pc > min, "{profile:?} {depth}-bit {w}x{h}: PSNR {p:.1}/{pc:.1}");
        }
    }
}

#[test]
fn rejects_bad_input() {
    let mut enc = Encoder::new(Profile::Hq, 64, 64).unwrap();
    assert!(enc.encode(&Frame::new(32, 64, ChromaFormat::Yuv422, 8, false)).is_err());
    assert!(enc.encode(&Frame::new(64, 64, ChromaFormat::Yuv444, 8, false)).is_err());
    assert!(Encoder::with_config(EncoderConfig { bit_depth: 10, ..EncoderConfig::new(Profile::Hq, 64, 64) }).is_err());
    assert!(decode_frame(&[0u8; 100]).is_err());
    assert!(decode_frame(&[0u8; 1000]).is_err());
    // An unsupported raster is an error, not a panic.
    assert!(Encoder::new(Profile::Hq, 0, 0).is_err());
    assert!(Encoder::new(Profile::Hq, 20_000, 64).is_err());
}

#[test]
fn corrupt_streams_never_panic() {
    let mut enc = Encoder::new(Profile::Hq, 256, 128).unwrap();
    let good = enc.encode(&picture(256, 128, ChromaFormat::Yuv422, 8, 1)).unwrap();
    let mut seed = 0x1234_5678u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..400 {
        let mut d = good.clone();
        let n = 1 + rnd() % 8;
        for _ in 0..n {
            // mostly the payload, sometimes the header
            let pos = if i % 4 == 0 { rnd() as usize % 0x280 } else { 0x280 + rnd() as usize % (d.len() - 0x280) };
            d[pos] ^= 1 << (rnd() % 8);
        }
        if i % 5 == 0 {
            d.truncate(rnd() as usize % d.len());
        }
        let _ = decode_frame(&d);
    }
}
