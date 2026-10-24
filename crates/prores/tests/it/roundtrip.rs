//! Encoder → decoder round trips on synthetic content (no external tools needed).

use crate::common;
use common::psnr;
use filmcraft_prores::*;

/// Deterministic test picture: gradients, a sharp-edged grid, a disc and mild noise.
pub fn synth(w: u32, h: u32, chroma: ChromaFormat, depth: u8, alpha: bool, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, alpha);
    let max = ((1u32 << depth) - 1) as f32;
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s % 1000) as f32 / 1000.0 - 0.5
    };
    let cw = f.chroma_width() as usize;
    let (wu, hu) = (w as usize, h as usize);
    for y in 0..hu {
        for x in 0..wu {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let grid = if (x / 24 + y / 24) % 2 == 0 { 0.15 } else { 0.0 };
            let disc = if (fx - 0.6).powi(2) + (fy - 0.4).powi(2) < 0.04 { 0.2 } else { 0.0 };
            let v = 0.1 + 0.5 * fx * (1.0 - fy) + grid + disc + 0.01 * rnd();
            f.y[y * wu + x] = (v.clamp(0.0, 1.0) * max) as u16;
            if let Some(a) = f.alpha.as_mut() {
                a[y * wu + x] = if disc > 0.0 { max as u16 } else { ((fx * max) as u16) & !7 };
            }
        }
        for x in 0..cw {
            let fx = x as f32 / cw as f32;
            f.cb[y * cw + x] = ((0.5 + 0.2 * (fx * 6.0).sin()) * max) as u16;
            f.cr[y * cw + x] = ((0.5 - 0.15 * (y as f32 / 40.0).cos()) * max) as u16;
        }
    }
    f
}

fn quality(profile: Profile, w: u32, h: u32) -> (f64, f64, usize, usize) {
    let chroma = profile.chroma();
    let src = synth(w, h, chroma, 10, chroma == ChromaFormat::Yuv444, 7);
    let mut enc = Encoder::new(profile, w, h);
    let coded = enc.encode(&src).unwrap();
    let hdr = probe(&coded).unwrap();
    assert_eq!((hdr.width as u32, hdr.height as u32, hdr.chroma), (w, h, chroma));
    let dec = decode_frame_with(&coded, &DecodeOptions { bit_depth: Some(10), threads: true }).unwrap();
    assert_eq!(dec.alpha, src.alpha, "alpha is lossless");
    let py = psnr(&dec.y, &src.y, 1023.0);
    let pc = psnr(&[dec.cb, dec.cr].concat(), &[src.cb, src.cr].concat(), 1023.0);
    (py, pc, coded.len(), enc.target_frame_bytes())
}

#[test]
fn every_profile_round_trips() {
    let mut last = 0.0;
    for p in [Profile::Proxy, Profile::Lt, Profile::Standard, Profile::Hq] {
        let (py, pc, size, target) = quality(p, 640, 360);
        println!("{p:?}: Y {py:.2} dB, C {pc:.2} dB, {size} / {target} bytes");
        assert!(size as f64 <= target as f64 * 1.15);
        assert!(py >= last - 0.5, "quality should not drop with a higher profile");
        assert!(py > 38.0 && pc > 38.0);
        last = py;
    }
    let (py, pc, _, _) = quality(Profile::Hq, 640, 360);
    assert!(py >= 45.0 && pc >= 45.0);
    for p in [Profile::P4444, Profile::P4444Xq] {
        let (py, pc, size, target) = quality(p, 320, 240);
        println!("{p:?}: Y {py:.2} dB, C {pc:.2} dB, {size} / {target} bytes");
        assert!(py >= 45.0 && pc >= 45.0);
    }
}

#[test]
fn odd_sizes_and_slice_widths() {
    for (w, h) in [(1u32, 1u32), (17, 9), (70, 38), (1918, 34), (720, 486)] {
        for log2 in [0u8, 1, 3] {
            let src = synth(w, h, ChromaFormat::Yuv422, 10, false, w);
            let mut cfg = EncoderConfig::new(Profile::Hq, w, h);
            cfg.log2_slice_mbs = log2;
            let coded = Encoder::with_config(cfg).encode(&src).unwrap();
            let dec = decode_frame(&coded).unwrap();
            assert_eq!((dec.width, dec.height, dec.bit_depth), (w, h, 10));
            assert_eq!(dec.cb.len(), w.div_ceil(2) as usize * h as usize);
            assert!(psnr(&dec.y, &src.y, 1023.0) > 40.0, "{w}x{h} log2 {log2}");
        }
    }
}

#[test]
fn twelve_bit_input_and_output() {
    let src = synth(256, 128, ChromaFormat::Yuv444, 12, true, 3);
    let coded = Encoder::new(Profile::P4444Xq, 256, 128).encode(&src).unwrap();
    let dec = decode_frame(&coded).unwrap();
    assert_eq!(dec.bit_depth, 12);
    assert!(psnr(&dec.y, &src.y, 4095.0) > 50.0);
    assert_eq!(dec.alpha, src.alpha);
}

#[test]
fn metadata_round_trips() {
    let src = synth(64, 32, ChromaFormat::Yuv422, 10, false, 1);
    let mut cfg = EncoderConfig::new(Profile::Standard, 64, 32);
    cfg.color = ColorInfo { primaries: 9, transfer: 16, matrix: 9 };
    cfg.aspect_ratio = 1;
    cfg.frame_rate_code = 3;
    let coded = Encoder::with_config(cfg).encode(&src).unwrap();
    let hdr = probe(&coded).unwrap();
    assert_eq!(&hdr.encoder_id, b"fmcr");
    let dec = decode_frame(&coded).unwrap();
    assert_eq!(dec.color, ColorInfo { primaries: 9, transfer: 16, matrix: 9 });
    assert_eq!((dec.aspect_ratio, dec.frame_rate_code, dec.interlace), (1, 3, Interlace::Progressive));
}

#[test]
fn rejects_bad_input() {
    let mut enc = Encoder::new(Profile::Hq, 64, 32);
    assert!(enc.encode(&Frame::new(32, 32, ChromaFormat::Yuv422, 10, false)).is_err());
    assert!(enc.encode(&Frame::new(64, 32, ChromaFormat::Yuv444, 10, false)).is_err());
    let mut short = Frame::new(64, 32, ChromaFormat::Yuv422, 10, false);
    short.cb.truncate(10);
    assert!(enc.encode(&short).is_err());
    assert!(decode_frame(&[]).is_err());
    assert!(decode_frame(b"\0\0\0\x20icpfxxxxxxxxxxxxxxxxxxxxxxxx").is_err());
}

#[test]
fn profile_identifiers() {
    for p in Profile::ALL {
        assert_eq!(Profile::from_fourcc(&p.fourcc()), Some(p));
    }
    assert_eq!(Profile::Hq.nominal_bits_per_mb(), 900);
    assert_eq!(Profile::Proxy.chroma(), ChromaFormat::Yuv422);
    assert_eq!(Profile::P4444Xq.chroma(), ChromaFormat::Yuv444);
}
