//! End-to-end tests using ffmpeg/ffprobe strictly as an external decoder oracle (skipped when absent).
//!
//! Every stream must decode with zero errors, with the exact frame count, and the decoder's output must be
//! bit-identical to the encoder's own reconstruction (catches prediction/transform/deblocking mismatches).

use crate::common;

use common::{Yuv, annexb_nals, ffmpeg, ffmpeg_decode, ffmpeg_source, ffprobe, out_dir, psnr, synth};
use filmcraft_h264enc::*;
use std::process::Command;

fn cfg(w: usize, h: usize, profile: Profile, preset: Preset, bframes: u8, rate: RateControl, slices: usize) -> EncoderConfig {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c.profile = profile;
    c.preset = preset;
    c.bframes = bframes;
    c.rate = rate;
    c.slices = slices;
    c.threads = 4;
    c
}

struct Run {
    stream: Vec<u8>,
    packets: Vec<Packet>,
    recon: Vec<ReconFrame>,
}

fn encode(c: EncoderConfig, frames: &[Yuv]) -> Run {
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let stream = packets.iter().flat_map(|p| p.data.iter().copied()).collect();
    Run { stream, packets, recon }
}

/// Decode with ffmpeg, check zero errors, exact frame count and bit-exact match with the reconstruction.
/// Returns the decoded frames (display order).
fn verify(name: &str, run: &Run, w: usize, h: usize, n: usize) -> Vec<u8> {
    let path = out_dir("oracle").join(format!("{name}.h264"));
    std::fs::write(&path, &run.stream).unwrap();
    let (err, dec) = ffmpeg_decode(&path);
    assert!(err.trim().is_empty(), "{name}: ffmpeg reported errors: {err}");
    let fs = w * h * 3 / 2;
    assert_eq!(dec.len(), n * fs, "{name}: decoded frame count");
    assert_eq!(run.recon.len(), n);
    for (i, r) in run.recon.iter().enumerate() {
        let d = &dec[i * fs..(i + 1) * fs];
        assert!(d[..w * h] == r.y[..], "{name}: frame {i} luma differs from reconstruction");
        assert!(d[w * h..w * h * 5 / 4] == r.u[..], "{name}: frame {i} Cb differs");
        assert!(d[w * h * 5 / 4..] == r.v[..], "{name}: frame {i} Cr differs");
    }
    dec
}

fn check_timestamps(run: &Run, n: usize) {
    let mut pts: Vec<i64> = run.packets.iter().map(|p| p.pts).collect();
    for w in run.packets.windows(2) {
        assert!(w[1].dts > w[0].dts, "dts must increase");
    }
    for p in &run.packets {
        assert!(p.dts <= p.pts, "dts {} > pts {}", p.dts, p.pts);
    }
    pts.sort();
    assert_eq!(pts, (0..n as i64).collect::<Vec<_>>());
    assert!(run.packets[0].keyframe);
}

#[test]
fn bit_exact_profiles_presets() {
    if ffmpeg().is_none() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let cases: [(&str, usize, usize, Profile, Preset, u8, RateControl, usize); 6] = [
        ("baseline_speed", 176, 144, Profile::Baseline, Preset::Speed, 0, RateControl::Qp(26), 1),
        ("baseline_lowqp_slices", 176, 144, Profile::Baseline, Preset::Balanced, 0, RateControl::Qp(8), 3),
        ("main_b2", 320, 240, Profile::Main, Preset::Balanced, 2, RateControl::Qp(30), 2),
        ("high_crf_b2", 320, 240, Profile::High, Preset::Balanced, 2, RateControl::Crf(23.0), 1),
        ("high_quality_b3_slices", 320, 240, Profile::High, Preset::Quality, 3, RateControl::Qp(22), 3),
        ("high_speed_odd_size", 330, 250, Profile::High, Preset::Speed, 1, RateControl::Qp(34), 4),
    ];
    for (name, w, h, profile, preset, b, rate, slices) in cases {
        let n = 12;
        let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
        let run = encode(cfg(w, h, profile, preset, b, rate, slices), &frames);
        let dec = verify(name, &run, w, h, n);
        check_timestamps(&run, n);
        // decoded display order must follow the source
        let fs = w * h * 3 / 2;
        for (i, f) in frames.iter().enumerate() {
            let p = psnr(&dec[i * fs..i * fs + w * h], &f.y);
            assert!(p > 28.0, "{name}: frame {i} psnr {p:.2} (reordering?)");
        }
    }
}

/// The oracle must see a damaged multi-slice picture (#74): a missing slice used to decode silently
/// at `-v error -ec 0`, and `-ec 0` at `-v warning` flags clean multi-slice pictures too.
#[test]
fn oracle_reports_damaged_slices() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 6);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    for (name, profile) in [("cavlc", Profile::Baseline), ("cabac", Profile::High)] {
        let run = encode(cfg(w, h, profile, Preset::Balanced, 0, RateControl::Qp(26), 3), &frames);
        // the undamaged 3-slice stream decodes without a single message
        verify(&format!("damage_{name}_clean"), &run, w, h, n);
        let slices: Vec<_> = annexb_nals(&run.stream).into_iter().filter(|r| matches!(run.stream[r.start + 3] & 0x1f, 1 | 5)).collect();
        assert_eq!(slices.len(), 3 * n, "{name}: three slices per picture");
        for (k, slice) in [(1usize, &slices[4]), (3, &slices[10])] {
            let mut dropped = run.stream.clone();
            dropped.drain(slice.clone());
            let mut truncated = run.stream.clone();
            truncated.drain(slice.start + slice.len() / 2..slice.end);
            for (damage, stream) in [("dropped", dropped), ("truncated", truncated)] {
                let path = out_dir("oracle").join(format!("damage_{name}_{damage}_{k}.h264"));
                std::fs::write(&path, &stream).unwrap();
                let (err, _) = ffmpeg_decode(&path);
                assert!(!err.trim().is_empty(), "{name}: the oracle missed a {damage} slice in picture {k}");
            }
        }
    }
}

#[test]
fn bit_exact_lavfi_sources() {
    if ffmpeg().is_none() {
        return;
    }
    for src in ["testsrc2", "mandelbrot"] {
        let (w, h, n) = (352, 288, 20);
        let frames = ffmpeg_source(src, w, h, n);
        let run = encode(cfg(w, h, Profile::High, Preset::Balanced, 3, RateControl::Crf(22.0), 2), &frames);
        let dec = verify(src, &run, w, h, n);
        let fs = w * h * 3 / 2;
        let mean: f64 = (0..n).map(|i| psnr(&dec[i * fs..i * fs + w * h], &frames[i].y)).sum::<f64>() / n as f64;
        assert!(mean > 38.0, "{src}: mean psnr {mean:.2}");
    }
}

#[test]
fn psnr_1080p_crf20() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (1920, 1080, 6);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    let mut c = cfg(w, h, Profile::High, Preset::Speed, 2, RateControl::Crf(20.0), 0);
    c.threads = 0;
    let run = encode(c, &frames);
    let dec = verify("psnr1080", &run, w, h, n);
    let fs = w * h * 3 / 2;
    let ps: Vec<f64> = (0..n).map(|i| psnr(&dec[i * fs..i * fs + w * h], &frames[i].y)).collect();
    let mean = ps.iter().sum::<f64>() / n as f64;
    eprintln!("1080p CRF 20 luma PSNR per frame: {ps:.2?} mean {mean:.2}");
    assert!(mean >= 40.0, "mean luma PSNR {mean:.2} < 40 dB");
    assert!(ps.iter().all(|&p| p >= 38.0), "{ps:?}");
}

fn bitrate(run: &Run, n: usize) -> f64 {
    run.stream.len() as f64 * 8.0 / (n as f64 / 30.0) / 1000.0
}

#[test]
fn abr_hits_target() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (640, 360, 90);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    for (name, rate, target) in [("cbr", RateControl::Cbr { kbps: 1500 }, 1500.0), ("vbr1pass", RateControl::Vbr { target_kbps: 1000, max_kbps: 2000 }, 1000.0)]
    {
        let run = encode(cfg(w, h, Profile::High, Preset::Speed, 2, rate, 0), &frames);
        verify(name, &run, w, h, n);
        let k = bitrate(&run, n);
        eprintln!("{name}: {k:.1} kbps (target {target})");
        assert!((k - target).abs() / target <= 0.10, "{name}: {k:.1} kbps vs target {target}");
    }
}

/// 1-pass VBR must not starve the opening second of a 60 fps export (#74): the rate model's initial guess
/// is several times too pessimistic, and a virtual history frozen at that guess kept the pictures after the
/// first one 6–9 dB below the rest until it faded out after one second.
#[test]
fn vbr_opening_second_is_not_starved() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n, fps) = (640, 360, 120, 60);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    let mut c = cfg(w, h, Profile::High, Preset::Balanced, 2, RateControl::Vbr { target_kbps: 2000, max_kbps: 3000 }, 0);
    c.fps_num = fps;
    c.keyint = 2 * fps;
    let run = encode(c, &frames);
    let dec = verify("vbr_opening", &run, w, h, n);
    let fs = w * h * 3 / 2;
    let ps: Vec<f64> = frames.iter().enumerate().map(|(i, f)| psnr(&dec[i * fs..i * fs + w * h], &f.y)).collect();
    let mean = |r: std::ops::Range<usize>| ps[r.clone()].iter().sum::<f64>() / r.len() as f64;
    // 0.1–0.5 s against everything after the first second (the same GOP, so no new I frame in between).
    let (opening, settled) = (mean(6..30), mean(60..n));
    let kbps = run.stream.len() as f64 * 8.0 / (n as f64 / fps as f64) / 1000.0;
    eprintln!("opening {opening:.2} dB, after 1 s {settled:.2} dB, {kbps:.1} kbps");
    assert!(settled - opening <= 3.0, "opening second starved: {opening:.2} dB vs {settled:.2} dB after 1 s");
    assert!((kbps - 2000.0).abs() / 2000.0 <= 0.10, "{kbps:.1} kbps vs target 2000");
}

#[test]
fn two_pass_vbr_hits_target() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (640, 360, 90);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    let rate = RateControl::Vbr { target_kbps: 800, max_kbps: 2400 };
    let mut c1 = cfg(w, h, Profile::High, Preset::Speed, 2, rate, 0);
    c1.pass = Pass::First;
    let mut enc = Encoder::new(c1.clone()).unwrap();
    for (i, f) in frames.iter().enumerate() {
        enc.encode(&f.frame(), i as i64).unwrap();
    }
    enc.flush();
    let stats = enc.pass_stats().unwrap();
    assert_eq!(stats.frames.len(), n);
    let p1: u64 = stats.frames.iter().map(|f| f.bits).sum();
    eprintln!("pass 1: {:.1} kbps; {:?}", p1 as f64 / (n as f64 / 30.0) / 1000.0, &stats.frames[..6]);
    let stats = PassStats::from_text(&stats.to_text()).unwrap();
    let mut c2 = c1;
    c2.pass = Pass::Second(stats);
    let run = encode(c2, &frames);
    verify("twopass", &run, w, h, n);
    let k = bitrate(&run, n);
    eprintln!("2-pass: {k:.1} kbps (target 800)");
    assert!((k - 800.0).abs() / 800.0 <= 0.10, "2-pass: {k:.1} kbps vs 800");
}

#[test]
fn bframes_order_via_ffprobe() {
    if ffmpeg().is_none() || ffprobe().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 13);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    let run = encode(cfg(w, h, Profile::High, Preset::Balanced, 3, RateControl::Qp(28), 1), &frames);
    verify("bframes", &run, w, h, n);
    check_timestamps(&run, n);
    let path = out_dir("oracle").join("bframes.h264");
    let out = Command::new(ffprobe().unwrap())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pict_type,coded_picture_number", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let types: Vec<String> = text.lines().map(|l| l.split(',').next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty()).collect();
    assert_eq!(types.len(), n, "{text}");
    // Display order as reported by the decoder must match what we encoded: I B B B P B B B P B B B P
    let ours: Vec<char> = {
        let mut v: Vec<(i64, FrameType)> = run.packets.iter().map(|p| (p.pts, p.frame_type)).collect();
        v.sort_by_key(|x| x.0);
        v.iter()
            .map(|x| match x.1 {
                FrameType::Idr | FrameType::I => 'I',
                FrameType::P => 'P',
                FrameType::B => 'B',
            })
            .collect()
    };
    let theirs: Vec<char> = types.iter().map(|t| t.chars().next().unwrap()).collect();
    assert_eq!(ours, theirs);
    assert!(theirs.contains(&'B'));
}

#[test]
fn length_prefixed_with_avcc() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 8);
    let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
    let mut c = cfg(w, h, Profile::High, Preset::Speed, 1, RateControl::Qp(26), 1);
    c.format = PacketFormat::LengthPrefixed;
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let avcc = enc.avcc();
    assert_eq!(avcc[0], 1);
    assert_eq!(avcc[1], 100);
    assert_eq!(avcc[4], 0xFF);
    let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
    let sps = &avcc[8..8 + sps_len];
    let pps_len = u16::from_be_bytes([avcc[9 + sps_len], avcc[10 + sps_len]]) as usize;
    let pps = &avcc[11 + sps_len..11 + sps_len + pps_len];
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
    // convert to Annex-B using the avcC parameter sets
    let mut stream = Vec::new();
    for nal in [sps, pps] {
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(nal);
    }
    for p in &packets {
        let mut i = 0;
        while i < p.data.len() {
            let len = u32::from_be_bytes(p.data[i..i + 4].try_into().unwrap()) as usize;
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(&p.data[i + 4..i + 4 + len]);
            i += 4 + len;
        }
    }
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    verify("avcc", &Run { stream, packets, recon }, w, h, n);
}

#[test]
fn rejects_bad_config() {
    assert!(Encoder::new(EncoderConfig::new(0, 16, 30, 1)).is_err());
    assert!(Encoder::new(EncoderConfig::new(15, 16, 30, 1)).is_err());
    assert!(Encoder::new(EncoderConfig::new(16, 16, 0, 1)).is_err());
    let mut c = EncoderConfig::new(64, 64, 30, 1);
    c.bframes = 4;
    assert!(Encoder::new(c).is_err());
    let mut enc = Encoder::new(EncoderConfig::new(64, 64, 30, 1)).unwrap();
    let y = vec![0u8; 10];
    assert!(enc.try_encode(&YuvFrame { y: &y, u: &y, v: &y, y_stride: 64, uv_stride: 32 }, 0).is_err());
}
