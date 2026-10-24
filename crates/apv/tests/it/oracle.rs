//! Conformance, bit-exactness, and quality tests against ffmpeg's APV decoder (external oracle).

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_apv::{
    ChromaFormat, ContentLightLevel, DecodeOptions, Encoder, EncoderConfig, Frame, MasteringDisplay, Profile, decode_frame, decode_frame_with, probe,
};

fn ffmpeg() -> Option<PathBuf> {
    // The APV demuxer is new in ffmpeg 8.0; older toolchains are found but
    // cannot read the fixtures, so they skip like a missing tool (#1249).
    filmcraft_testkit::ffmpeg_with_format_or_skip("apv oracle", "apv", "8.0")
}

fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("apv")
}

fn native_pix_fmt(profile: Profile) -> &'static str {
    match profile {
        Profile::P422_10 => "yuv422p10le",
        Profile::P422_12 => "yuv422p12le",
        Profile::P444_10 => "yuv444p10le",
        Profile::P444_12 => "yuv444p12le",
        Profile::P4444_10 => "yuva444p10le",
        Profile::P4444_12 => "yuva444p12le",
        Profile::P400_10 => "gray10le",
    }
}

/// Decode a raw `.apv` file with ffmpeg (`-v error -xerror`) to planar `u16` samples.
fn ffmpeg_decode_raw(ff: &Path, apv_path: &Path, pix_fmt: &str) -> (Vec<u16>, String) {
    let out = Command::new(ff)
        .args(["-hide_banner", "-v", "error", "-xerror", "-f", "apv", "-i"])
        .arg(apv_path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("run ffmpeg");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "ffmpeg failed on {:?}: {}", apv_path, stderr);
    let samples = out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    (samples, stderr)
}

fn synthetic_frame(profile: Profile, w: u32, h: u32) -> Frame {
    let chroma = profile.default_chroma();
    let depth = profile.default_bit_depth();
    let mut f = Frame::new(w, h, chroma, depth, chroma == ChromaFormat::Yuv4444);
    let max_v = (1u32 << depth) - 1;
    let lo = 16u32 << (depth - 8);
    let hi = 235u32 << (depth - 8);
    let span = hi - lo;

    for y in 0..h {
        for x in 0..w {
            // Mix smooth gradients, diagonal edges, and high-frequency patterns
            let grad = (x * 7 + y * 13) % span;
            let checker = if ((x / 3) ^ (y / 5)) & 1 == 0 { 40 << (depth - 10) } else { 0 };
            f.y[(y * w + x) as usize] = (lo + (grad + checker) % span).min(max_v) as u16;
        }
    }
    let cw = f.chroma_width();
    let ch = f.chroma_height();
    for y in 0..ch {
        for x in 0..cw {
            let u = lo + ((x * 11 + y * 5 + 100) % span);
            let v = lo + ((x * 3 + y * 17 + 300) % span);
            f.cb[(y * cw + x) as usize] = u.min(max_v) as u16;
            f.cr[(y * cw + x) as usize] = v.min(max_v) as u16;
        }
    }
    if let Some(a) = f.alpha.as_mut() {
        for y in 0..h {
            for x in 0..w {
                let dx = x as i32 - (w as i32) / 2;
                let dy = y as i32 - (h as i32) / 2;
                a[(y * w + x) as usize] =
                    if dx * dx + dy * dy < ((w.min(h) as i32) / 3).pow(2) { max_v as u16 } else { ((x * 19 + y * 23) % (max_v + 1)) as u16 };
            }
        }
    }
    f
}

fn flatten_frame(f: &Frame) -> Vec<u16> {
    let mut v = Vec::new();
    v.extend_from_slice(&f.y);
    v.extend_from_slice(&f.cb);
    v.extend_from_slice(&f.cr);
    if let Some(a) = &f.alpha {
        v.extend_from_slice(a);
    }
    v
}

fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    if a.is_empty() {
        return 99.0;
    }
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (peak * peak / mse).log10() }
}

#[test]
fn all_profiles_bit_exact_with_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let dir = fixture_dir();

    for profile in Profile::ALL {
        let (w, h) = (320u32, 192u32);
        let src = synthetic_frame(profile, w, h);
        let mut cfg = EncoderConfig::new(profile, w, h);
        cfg.qp = Some(12);
        let mut enc = Encoder::with_config(cfg).expect("encoder init");

        let raw_au = enc.encode_raw_au(&src).expect("encode_raw_au");
        let apv_path = dir.join(format!("oracle_{}.apv", profile.short_name()));
        std::fs::write(&apv_path, &raw_au).expect("write .apv");

        let (ff_samples, stderr) = ffmpeg_decode_raw(&ff, &apv_path, native_pix_fmt(profile));
        assert!(stderr.trim().is_empty(), "{profile:?}: ffmpeg stderr: {stderr}");

        let ours = decode_frame(&raw_au).expect("our decode");
        let our_samples = flatten_frame(&ours);
        assert_eq!(our_samples.len(), ff_samples.len(), "{profile:?}: sample count mismatch");

        let max_diff = our_samples.iter().zip(&ff_samples).map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs()).max().unwrap_or(0);
        assert_eq!(max_diff, 0, "{profile:?}: our decoder must be 100% bit-exact with ffmpeg (max_diff={max_diff})");

        let peak = ((1u32 << profile.default_bit_depth()) - 1) as f64;
        let y_psnr = psnr(&ours.y, &src.y, peak);
        assert!(y_psnr > 45.0, "{profile:?}: expected high PSNR at qp=12, got {y_psnr:.2} dB");
    }
}

#[test]
fn odd_dimensions_and_multi_tile_bit_exact_with_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let dir = fixture_dir();

    for (profile, w, h) in [
        (Profile::P422_10, 640u32, 360u32),
        (Profile::P422_10, 334, 202),
        (Profile::P444_10, 333, 201),
        (Profile::P4444_12, 512, 288),
        (Profile::P400_10, 70, 38),
    ] {
        let mut src = synthetic_frame(profile, w, h);
        src.mastering_display = Some(MasteringDisplay {
            primaries: [(46400, 19136), (11141, 52232), (8585, 3015)],
            white_point: (20493, 21561),
            max_luminance: 1000 << 8,
            min_luminance: 1,
        });
        src.content_light = Some(ContentLightLevel { max_cll: 1000, max_fall: 400 });

        let mut enc = Encoder::new(profile, w, h).expect("encoder init");
        let raw_au = enc.encode_raw_au(&src).expect("encode");
        let apv_path = dir.join(format!("oracle_geom_{}_{w}x{h}.apv", profile.short_name()));
        std::fs::write(&apv_path, &raw_au).expect("write .apv");

        let (ff_samples, stderr) = ffmpeg_decode_raw(&ff, &apv_path, native_pix_fmt(profile));
        assert!(stderr.trim().is_empty(), "{profile:?} {w}x{h}: {stderr}");

        let hdr = probe(&raw_au).expect("probe");
        assert_eq!((hdr.width, hdr.height), (w, h));

        let ours = decode_frame(&raw_au).expect("decode");
        assert_eq!(ours.mastering_display, src.mastering_display);
        assert_eq!(ours.content_light, src.content_light);

        let our_samples = flatten_frame(&ours);
        assert_eq!(our_samples, ff_samples, "{profile:?} {w}x{h}: bit-exact match with ffmpeg failed");

        // Also verify single-threaded decode produces identical output
        let ours_st = decode_frame_with(&raw_au, &DecodeOptions { bit_depth: None, threads: false }).expect("decode single-threaded");
        assert_eq!(ours, ours_st);
    }
}

#[test]
fn custom_q_matrix_and_fh_tile_sizes_bit_exact_with_ffmpeg() {
    let Some(ff) = ffmpeg() else { return };
    let dir = fixture_dir();

    let profile = Profile::P444_12;
    let (w, h) = (512u32, 256u32);
    let src = synthetic_frame(profile, w, h);

    let mut q_matrix = [[16u8; 64]; 4];
    for (c, qm) in q_matrix.iter_mut().enumerate() {
        for y in 0..8 {
            for x in 0..8 {
                // Intentionally asymmetric (x != y weight) to verify exact (x, y) matrix orientation
                qm[y * 8 + x] = (10 + c * 3 + y * 5 + x * 2) as u8;
            }
        }
    }

    let mut cfg = EncoderConfig::new(profile, w, h);
    cfg.qp = Some(18);
    cfg.q_matrix = Some(q_matrix);
    cfg.write_tile_sizes_in_fh = true;
    cfg.tile_width_in_mbs = 16;
    cfg.tile_height_in_mbs = 8;

    let mut enc = Encoder::with_config(cfg).expect("encoder init");
    let raw_au = enc.encode_raw_au(&src).expect("encode");
    let apv_path = dir.join("oracle_custom_qmat_444-12.apv");
    std::fs::write(&apv_path, &raw_au).expect("write .apv");

    let (ff_samples, stderr) = ffmpeg_decode_raw(&ff, &apv_path, native_pix_fmt(profile));
    assert!(stderr.trim().is_empty(), "ffmpeg stderr: {stderr}");

    let hdr = probe(&raw_au).expect("probe");
    assert!(hdr.use_q_matrix);
    assert!(hdr.tile_sizes_in_fh.is_some());
    assert_eq!(hdr.num_tiles(), 4);

    let ours = decode_frame(&raw_au).expect("decode");
    let our_samples = flatten_frame(&ours);
    assert_eq!(our_samples, ff_samples, "asymmetric q_matrix decode must be bit-exact with ffmpeg");

    // Verify 8-bit and 16-bit rescaled output
    let d8 = decode_frame_with(&raw_au, &DecodeOptions { bit_depth: Some(8), threads: true }).expect("decode 8-bit");
    assert_eq!(d8.bit_depth, 8);
    assert!(d8.y.iter().all(|&s| s <= 255));

    let d16 = decode_frame_with(&raw_au, &DecodeOptions { bit_depth: Some(16), threads: true }).expect("decode 16-bit");
    assert_eq!(d16.bit_depth, 16);
}
