//! High 10 (profile_idc 110, 4:2:0, 8 to 10 bits) and High 10 Intra, verified with ffmpeg/ffprobe as an
//! external oracle (skipped when absent): every stream decodes without a message, with the exact frame
//! count, bit-identical to the encoder's reconstruction (#1221).

mod common;

use common::{Yuv16, ffmpeg, ffmpeg_decode_as, ffprobe, le16, out_dir, psnr10, synth, synth10};
use filmcraft_h264enc::*;
use std::process::Command;

fn cfg(w: usize, h: usize, preset: Preset, bframes: u8, rate: RateControl, slices: usize) -> EncoderConfig {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c.profile = Profile::High10;
    c.bit_depth = 10;
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

fn encode(c: EncoderConfig, frames: &[Yuv16]) -> Run {
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode16(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let stream = packets.iter().flat_map(|p| p.data.iter().copied()).collect();
    Run { stream, packets, recon }
}

/// Decode as `pix_fmt` and compare every frame with the reconstruction; returns the decoded frames.
fn verify_as(name: &str, run: &Run, w: usize, h: usize, n: usize, pix_fmt: &str, bytes: usize) -> Vec<u8> {
    let path = out_dir("high10").join(format!("{name}.h264"));
    std::fs::write(&path, &run.stream).unwrap();
    let (err, dec) = ffmpeg_decode_as(&path, pix_fmt);
    assert!(err.trim().is_empty(), "{name}: ffmpeg reported errors: {err}");
    let (ys, cs) = (w * h * bytes, w * h / 4 * bytes);
    let fs = ys + 2 * cs;
    assert_eq!(dec.len(), n * fs, "{name}: decoded frame count");
    assert_eq!(run.recon.len(), n);
    for (i, r) in run.recon.iter().enumerate() {
        let d = &dec[i * fs..(i + 1) * fs];
        assert!(d[..ys] == r.y[..], "{name}: frame {i} luma differs from reconstruction");
        assert!(d[ys..ys + cs] == r.u[..], "{name}: frame {i} Cb differs");
        assert!(d[ys + cs..] == r.v[..], "{name}: frame {i} Cr differs");
    }
    dec
}

fn verify(name: &str, run: &Run, w: usize, h: usize, n: usize) -> Vec<u8> {
    verify_as(name, run, w, h, n, "yuv420p10le", 2)
}

/// Mean luma PSNR (10-bit scale) of decoded `yuv420p10le` frames against the source.
fn mean_psnr(dec: &[u8], frames: &[Yuv16]) -> f64 {
    let (w, h) = (frames[0].w, frames[0].h);
    let fs = w * h * 3;
    frames.iter().enumerate().map(|(i, f)| psnr10(&le16(&dec[i * fs..i * fs + w * h * 2]), &f.y)).sum::<f64>() / frames.len() as f64
}

fn probe(path: &std::path::Path, entries: &str) -> String {
    let out = Command::new(ffprobe().unwrap())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn bit_exact_presets_bframes_slices() {
    if ffmpeg().is_none() {
        return;
    }
    let cases: [(&str, usize, usize, Preset, u8, RateControl, usize); 8] = [
        ("speed_p", 176, 144, Preset::Speed, 0, RateControl::Qp(26), 1),
        ("balanced_b2", 320, 240, Preset::Balanced, 2, RateControl::Qp(30), 2),
        ("quality_b3_slices", 320, 240, Preset::Quality, 3, RateControl::Qp(22), 3),
        ("crf_b2", 320, 240, Preset::Balanced, 2, RateControl::Crf(23.0), 1),
        ("odd_size", 330, 250, Preset::Speed, 1, RateControl::Qp(34), 4),
        // QPY 0 and below: QP' down to 0, the largest coefficient levels and mb_qp_delta range
        ("lowest_qp", 176, 144, Preset::Balanced, 1, RateControl::Crf(-12.0), 2),
        ("cbr", 320, 240, Preset::Speed, 2, RateControl::Cbr { kbps: 1200 }, 0),
        // strong deblocking (bS 4, large α/β/tC0 scaled to 10 bits) and chroma QP mapping above 30
        ("high_qp", 320, 240, Preset::Balanced, 2, RateControl::Qp(46), 1),
    ];
    for (name, w, h, preset, b, rate, slices) in cases {
        let n = 12;
        let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
        let run = encode(cfg(w, h, preset, b, rate, slices), &frames);
        let dec = verify(name, &run, w, h, n);
        let p = mean_psnr(&dec, &frames);
        let floor = if name == "high_qp" { 20.0 } else { 30.0 };
        assert!(p > floor, "{name}: mean psnr {p:.2} (reordering?)");
        for w in run.packets.windows(2) {
            assert!(w[1].dts > w[0].dts, "{name}: dts must increase");
        }
    }
}

/// 9-bit samples (QpBdOffset 6) are High 10 too.
#[test]
fn bit_exact_nine_bits() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 8);
    let frames: Vec<Yuv16> = (0..n)
        .map(|t| {
            let f = synth10(w, h, t);
            let half = |p: Vec<u16>| p.into_iter().map(|s| s >> 1).collect();
            Yuv16 { w, h, y: half(f.y), u: half(f.u), v: half(f.v) }
        })
        .collect();
    for (name, rate) in [("nine_qp", RateControl::Qp(30)), ("nine_crf_low", RateControl::Crf(-6.0))] {
        let mut c = cfg(w, h, Preset::Balanced, 2, rate, 2);
        c.bit_depth = 9;
        let run = encode(c, &frames);
        // ffmpeg outputs 9-bit H.264 as yuv420p9le (9-bit values in 16-bit little-endian samples)
        verify_as(name, &run, w, h, n, "yuv420p9le", 2);
    }
}

/// QPY can go below 0 at 10 bits (down to -12), and at the bottom of the range the stream is close
/// to the source.
#[test]
fn negative_qp_reaches_high_fidelity() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 4);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
    let run = encode(cfg(w, h, Preset::Balanced, 0, RateControl::Crf(-12.0), 1), &frames);
    let dec = verify("crf_minus12", &run, w, h, n);
    let p = mean_psnr(&dec, &frames);
    eprintln!("CRF -12: {p:.2} dB");
    assert!(p > 60.0, "CRF -12 psnr {p:.2}");
}

/// The point of High 10: smooth 10-bit content keeps its precision. An 8-bit High encode of the same
/// source, even at QP 0, can't get closer than the 8-bit rounding (about 59 dB on the 10-bit scale).
#[test]
fn ten_bits_keep_more_than_eight() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (256, 192, 3);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
    let run10 = encode(cfg(w, h, Preset::Balanced, 0, RateControl::Crf(-12.0), 1), &frames);
    let p10 = mean_psnr(&verify("precision10", &run10, w, h, n), &frames);
    // 8-bit: round the source, encode at QP 0 with High, compare the decode (scaled to 10 bits)
    let mut c8 = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c8.rate = RateControl::Qp(0);
    c8.bframes = 0;
    let mut enc = Encoder::new(c8).unwrap();
    let mut stream = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let r = |p: &[u16]| p.iter().map(|&s| ((s + 2) >> 2).min(255) as u8).collect::<Vec<u8>>();
        let (y, u, v) = (r(&f.y), r(&f.u), r(&f.v));
        for p in enc.encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: w, uv_stride: w / 2 }, i as i64).unwrap() {
            stream.extend(p.data);
        }
    }
    for p in enc.flush() {
        stream.extend(p.data);
    }
    let path = out_dir("high10").join("precision8.h264");
    std::fs::write(&path, &stream).unwrap();
    let (err, dec8) = ffmpeg_decode_as(&path, "yuv420p10le");
    assert!(err.trim().is_empty(), "{err}");
    let p8 = mean_psnr(&dec8, &frames);
    eprintln!("10-bit source: High 10 CRF -12 {p10:.2} dB, 8-bit High QP 0 {p8:.2} dB");
    assert!(p10 > p8 + 3.0, "10-bit {p10:.2} dB vs 8-bit {p8:.2} dB");
}

/// High 10 Intra: every picture is an IDR, the SPS has constraint_set3 and no reference frames.
#[test]
fn intra_only_is_high10_intra() {
    if ffmpeg().is_none() || ffprobe().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 6);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
    let mut c = cfg(w, h, Preset::Balanced, 3, RateControl::Crf(18.0), 2);
    c.intra_only = true;
    c.keyint = 250;
    let run = encode(c, &frames);
    verify("intra", &run, w, h, n);
    assert!(run.packets.iter().all(|p| p.keyframe && p.frame_type == FrameType::Idr));
    assert!(run.packets.iter().all(|p| p.pts == p.dts), "no reordering");
    let path = out_dir("high10").join("intra.h264");
    let info = probe(&path, "stream=profile,pix_fmt,bits_per_raw_sample,refs");
    assert!(info.contains("profile=High 10 Intra"), "{info}");
    assert!(info.contains("pix_fmt=yuv420p10le"), "{info}");
    assert!(info.contains("bits_per_raw_sample=10"), "{info}");
    let first = String::from_utf8_lossy(&run.packets[0].data).into_owned();
    assert!(first.contains("profile=high10intra") && first.contains("bitdepth=10") && first.contains("refs=0"), "{first}");
}

#[test]
fn ffprobe_reports_high10() {
    if ffmpeg().is_none() || ffprobe().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 5);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
    let run = encode(cfg(w, h, Preset::Speed, 2, RateControl::Qp(28), 1), &frames);
    verify("probe", &run, w, h, n);
    let info = probe(&out_dir("high10").join("probe.h264"), "stream=profile,pix_fmt,bits_per_raw_sample");
    assert!(info.contains("profile=High 10\n"), "{info}");
    assert!(info.contains("pix_fmt=yuv420p10le"), "{info}");
}

/// 8-bit pictures into a 10-bit encoder are scaled up exactly (v << 2).
#[test]
fn eight_bit_input_scales_up() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 4);
    let src: Vec<_> = (0..n).map(|t| synth(w, h, t)).collect();
    let mut enc = Encoder::new(cfg(w, h, Preset::Speed, 0, RateControl::Crf(-12.0), 1)).unwrap();
    enc.set_recon_capture(true);
    let mut packets = Vec::new();
    for (i, f) in src.iter().enumerate() {
        packets.extend(enc.encode(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let stream = packets.iter().flat_map(|p| p.data.iter().copied()).collect();
    let run = Run { stream, packets, recon };
    let dec = verify("from8", &run, w, h, n);
    let scaled: Vec<Yuv16> = src
        .iter()
        .map(|f| {
            let s = |p: &[u8]| p.iter().map(|&v| v as u16 * 4).collect();
            Yuv16 { w, h, y: s(&f.y), u: s(&f.u), v: s(&f.v) }
        })
        .collect();
    let p = mean_psnr(&dec, &scaled);
    assert!(p > 55.0, "8-bit input psnr {p:.2}");
}

/// Profile High 10 at 8 bits is a valid stream (decodes as yuv420p).
#[test]
fn high10_at_eight_bits() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 5);
    let src: Vec<_> = (0..n).map(|t| synth(w, h, t)).collect();
    let mut c = cfg(w, h, Preset::Balanced, 1, RateControl::Qp(26), 1);
    c.bit_depth = 8;
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let mut packets = Vec::new();
    for (i, f) in src.iter().enumerate() {
        packets.extend(enc.encode(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let stream = packets.iter().flat_map(|p| p.data.iter().copied()).collect();
    verify_as("high10_8bit", &Run { stream, packets, recon }, w, h, n, "yuv420p", 1);
}

/// MP4-style samples: the avcC carries profile 110 and bit_depth_luma/chroma_minus8 = 2, and the
/// length-prefixed stream rebuilt from it decodes bit-exactly.
#[test]
fn length_prefixed_with_avcc() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 6);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth10(w, h, t)).collect();
    let mut c = cfg(w, h, Preset::Speed, 1, RateControl::Qp(26), 1);
    c.format = PacketFormat::LengthPrefixed;
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let avcc = enc.avcc();
    assert_eq!(&avcc[..6], &[1, 110, 0, avcc[3], 0xFF, 0xE1]);
    let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
    let sps = &avcc[8..8 + sps_len];
    let pps_len = u16::from_be_bytes([avcc[9 + sps_len], avcc[10 + sps_len]]) as usize;
    let pps = &avcc[11 + sps_len..11 + sps_len + pps_len];
    // High-profile extension: chroma_format 1, bit depths 10, no SPS extensions
    assert_eq!(&avcc[11 + sps_len + pps_len..], &[0xFD, 0xFA, 0xFA, 0x00]);
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode16(&f.frame(), i as i64).unwrap());
    }
    packets.extend(enc.flush());
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

/// Hostile settings and input return errors (or are clipped), never panic.
#[test]
fn rejects_bad_depth_settings_and_input() {
    let base = || cfg(64, 64, Preset::Speed, 0, RateControl::Qp(26), 1);
    for bits in [0u8, 7, 11, 16, 255] {
        let mut c = base();
        c.bit_depth = bits;
        assert!(matches!(Encoder::new(c), Err(Error::InvalidConfig(_))), "bit depth {bits}");
    }
    for profile in [Profile::Baseline, Profile::Main, Profile::High] {
        let mut c = base();
        c.profile = profile;
        assert!(matches!(Encoder::new(c), Err(Error::InvalidConfig(_))), "{profile:?} at 10 bits");
    }
    // 16-bit input to an 8-bit encoder
    let mut enc8 = Encoder::new(EncoderConfig::new(64, 64, 30, 1)).unwrap();
    let f = synth10(64, 64, 0);
    assert!(matches!(enc8.encode16(&f.frame(), 0), Err(Error::DeepInput)));
    // short planes and absurd strides
    let mut enc = Encoder::new(base()).unwrap();
    let short = vec![0u16; 10];
    assert!(matches!(enc.encode16(&YuvFrame16 { y: &short, u: &short, v: &short, y_stride: 64, uv_stride: 32 }, 0), Err(Error::BadFrame)));
    let big = vec![0u16; 64 * 64];
    let huge = YuvFrame16 { y: &big, u: &big, v: &big, y_stride: usize::MAX, uv_stride: usize::MAX };
    assert!(matches!(enc.encode16(&huge, 0), Err(Error::BadFrame)));
    // out-of-range samples are clipped to 10 bits
    let over = Yuv16 { w: 64, h: 64, y: vec![u16::MAX; 64 * 64], u: vec![u16::MAX; 32 * 32], v: vec![40000; 32 * 32] };
    enc.set_recon_capture(true);
    enc.encode16(&over.frame(), 0).unwrap();
    enc.flush();
    let r = enc.take_recon();
    assert!(le16(&r[0].y).iter().chain(&le16(&r[0].u)).chain(&le16(&r[0].v)).all(|&s| s <= 1023));
}
