//! High 4:2:2 (profile_idc 122, 4:2:2 chroma, 8 to 10 bits) and High 4:2:2 Intra, verified with
//! ffmpeg/ffprobe as an external oracle (skipped when absent): every stream decodes without a message,
//! with the exact frame count, bit-identical to the encoder's reconstruction (#1221, stage 2).

use crate::common;

use common::{Yuv16, ffmpeg, ffmpeg_decode_as, ffprobe, le16, out_dir, psnr10, synth, synth10};
use filmcraft_h264enc::*;
use std::process::Command;

/// 4:2:2 picture `t` at `bits` (8 or 10): [`synth10`]'s luma, and chroma at full height with detail
/// in every row, so vertical chroma resolution, the 8x16 chroma prediction and the four horizontal
/// chroma deblocking edges all matter.
fn synth422(w: usize, h: usize, t: usize, bits: u8) -> Yuv16 {
    let b = synth10(w, h, t);
    let tf = t as f32;
    let cw = w / 2;
    let mut u = vec![0u16; cw * h];
    let mut v = vec![0u16; cw * h];
    for yy in 0..h {
        for xx in 0..cw {
            let fx = xx as f32 / cw as f32;
            let fy = yy as f32 / h as f32;
            // a fine horizontal stripe pattern only full-height chroma can carry
            let stripes = if (yy + t) % 6 < 3 { 90.0 } else { -90.0 };
            u[yy * cw + xx] = (512.0 + 220.0 * (fx * 3.0 + tf * 0.03).sin() + stripes * (fy - 0.5)).round().clamp(0.0, 1023.0) as u16;
            v[yy * cw + xx] = (512.0 + 200.0 * (fy * 9.0 - tf * 0.04).cos() * fx).round().clamp(0.0, 1023.0) as u16;
        }
    }
    let shift = 10 - bits as u32;
    let down = |p: Vec<u16>| p.into_iter().map(|s| s >> shift).collect();
    Yuv16 { w, h, y: down(b.y), u: down(u), v: down(v) }
}

/// A borrowed 16-bit 4:2:2 encoder frame over `f`'s planes.
fn frame(f: &Yuv16) -> YuvFrame16<'_> {
    YuvFrame16 { y: &f.y, u: &f.u, v: &f.v, y_stride: f.w, uv_stride: f.w / 2 }
}

/// A 10-bit High 4:2:2 configuration with the given preset, B-frames, rate control and slices.
fn cfg(w: usize, h: usize, preset: Preset, bframes: u8, rate: RateControl, slices: usize) -> EncoderConfig {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c.profile = Profile::High422;
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

/// Flush `enc` and gather the stream, its packets and the reconstruction in presentation order.
fn finish(mut enc: Encoder, mut packets: Vec<Packet>) -> Run {
    packets.extend(enc.flush());
    let mut recon = enc.take_recon();
    recon.sort_by_key(|r| r.pts);
    let stream = packets.iter().flat_map(|p| p.data.iter().copied()).collect();
    Run { stream, packets, recon }
}

/// Encode `frames` with `c`, capturing the reconstruction.
fn encode(c: EncoderConfig, frames: &[Yuv16]) -> Run {
    let eight = c.bit_depth == 8;
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if eight {
            let b = |p: &[u16]| p.iter().map(|&s| s as u8).collect::<Vec<u8>>();
            let (y, u, v) = (b(&f.y), b(&f.u), b(&f.v));
            packets.extend(enc.encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: f.w, uv_stride: f.w / 2 }, i as i64).unwrap());
        } else {
            packets.extend(enc.encode16(&frame(f), i as i64).unwrap());
        }
    }
    finish(enc, packets)
}

/// Decode as `pix_fmt` (`yuv422p10le` or `yuv422p`) and compare every frame with the reconstruction;
/// returns the decoded frames.
fn verify_as(name: &str, run: &Run, w: usize, h: usize, n: usize, pix_fmt: &str, bytes: usize) -> Vec<u8> {
    let path = out_dir("high422").join(format!("{name}.h264"));
    std::fs::write(&path, &run.stream).unwrap();
    let (err, dec) = ffmpeg_decode_as(&path, pix_fmt);
    assert!(err.trim().is_empty(), "{name}: ffmpeg reported errors: {err}");
    let (ys, cs) = (w * h * bytes, w / 2 * h * bytes);
    let fs = ys + 2 * cs;
    assert_eq!(dec.len(), n * fs, "{name}: decoded frame count");
    assert_eq!(run.recon.len(), n);
    for (i, r) in run.recon.iter().enumerate() {
        let d = &dec[i * fs..(i + 1) * fs];
        assert_eq!((r.u.len(), r.v.len()), (cs, cs), "{name}: full-height chroma in the reconstruction");
        assert!(d[..ys] == r.y[..], "{name}: frame {i} luma differs from reconstruction");
        assert!(d[ys..ys + cs] == r.u[..], "{name}: frame {i} Cb differs");
        assert!(d[ys + cs..] == r.v[..], "{name}: frame {i} Cr differs");
    }
    dec
}

/// Decode `run` with ffmpeg as 10-bit 4:2:2 and check it matches the encoder's reconstruction.
fn verify(name: &str, run: &Run, w: usize, h: usize, n: usize) -> Vec<u8> {
    verify_as(name, run, w, h, n, "yuv422p10le", 2)
}

/// Mean PSNR (10-bit scale) of the decoded luma and chroma planes against the source.
fn mean_psnr(dec: &[u8], frames: &[Yuv16]) -> (f64, f64) {
    let (w, h) = (frames[0].w, frames[0].h);
    let (ys, cs) = (w * h * 2, w / 2 * h * 2);
    let fs = ys + 2 * cs;
    let n = frames.len() as f64;
    let mut py = 0.0;
    let mut pc = 0.0;
    for (i, f) in frames.iter().enumerate() {
        let d = &dec[i * fs..(i + 1) * fs];
        py += psnr10(&le16(&d[..ys]), &f.y);
        pc += (psnr10(&le16(&d[ys..ys + cs]), &f.u) + psnr10(&le16(&d[ys + cs..]), &f.v)) / 2.0;
    }
    (py / n, pc / n)
}

/// The ffprobe `entries` of the first video stream in `path`.
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
    let cases: [(&str, usize, usize, Preset, u8, RateControl, usize); 9] = [
        ("speed_p", 176, 144, Preset::Speed, 0, RateControl::Qp(26), 1),
        ("balanced_b2", 320, 240, Preset::Balanced, 2, RateControl::Qp(30), 2),
        ("quality_b3_slices", 320, 240, Preset::Quality, 3, RateControl::Qp(22), 3),
        ("crf_b2", 320, 240, Preset::Balanced, 2, RateControl::Crf(23.0), 1),
        // cropping in whole rows (CropUnitY = 1 for 4:2:2)
        ("odd_size", 330, 250, Preset::Speed, 1, RateControl::Qp(34), 4),
        ("odd_rows", 176, 146, Preset::Balanced, 1, RateControl::Qp(28), 1),
        // QP' down to 0: the largest chroma DC levels (qP,DC = QP'C + 3) and mb_qp_delta range
        ("lowest_qp", 176, 144, Preset::Balanced, 1, RateControl::Crf(-12.0), 2),
        ("cbr", 320, 240, Preset::Speed, 2, RateControl::Cbr { kbps: 1500 }, 0),
        // strong deblocking on all four horizontal chroma edges, chroma QP mapping above 30
        ("high_qp", 320, 240, Preset::Balanced, 2, RateControl::Qp(46), 1),
    ];
    for (name, w, h, preset, b, rate, slices) in cases {
        let n = 12;
        let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
        let run = encode(cfg(w, h, preset, b, rate, slices), &frames);
        let dec = verify(name, &run, w, h, n);
        let (py, pc) = mean_psnr(&dec, &frames);
        let floor = if name == "high_qp" { 20.0 } else { 30.0 };
        assert!(py > floor && pc > floor, "{name}: mean psnr luma {py:.2} chroma {pc:.2} (reordering?)");
        for w in run.packets.windows(2) {
            assert!(w[1].dts > w[0].dts, "{name}: dts must increase");
        }
    }
}

/// High 4:2:2 at 8 bits (8-bit 4:2:2 input, decoded as `yuv422p`) and at 9 bits.
#[test]
fn bit_exact_eight_and_nine_bits() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 8);
    for (name, bits, pix_fmt, bytes, rate) in [
        ("eight_qp", 8u8, "yuv422p", 1, RateControl::Qp(26)),
        ("eight_crf_low", 8, "yuv422p", 1, RateControl::Crf(0.0)),
        ("nine_qp", 9, "yuv422p9le", 2, RateControl::Qp(30)),
    ] {
        let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, bits)).collect();
        let mut c = cfg(w, h, Preset::Quality, 2, rate, 2);
        c.bit_depth = bits;
        let run = encode(c, &frames);
        verify_as(name, &run, w, h, n, pix_fmt, bytes);
    }
}

/// Every intra chroma mode on 8x16 blocks (DC per 4x4 block, horizontal, vertical, plane with
/// yCF = 4) at every edge of the picture, with and without 8x8 transforms.
#[test]
fn bit_exact_intra_chroma_modes() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (208, 160, 3);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
    for (name, preset, q) in [("intra_speed", Preset::Speed, 18u8), ("intra_quality", Preset::Quality, 30)] {
        let mut c = cfg(w, h, preset, 0, RateControl::Qp(q), 1);
        c.intra_only = true;
        let run = encode(c, &frames);
        verify(name, &run, w, h, n);
    }
}

/// The point of 4:2:2: chroma keeps its vertical detail. The same source through 4:2:0 (High 10,
/// chroma averaged over row pairs) cannot get close on the stripe pattern.
#[test]
fn keeps_vertical_chroma_detail() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (256, 192, 2);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
    let run = encode(cfg(w, h, Preset::Balanced, 0, RateControl::Crf(-6.0), 1), &frames);
    let (_, pc422) = mean_psnr(&verify("detail422", &run, w, h, n), &frames);
    // 4:2:0: average chroma row pairs, encode with High 10, compare the upsampled (row-doubled) decode
    let mut c = cfg(w, h, Preset::Balanced, 0, RateControl::Crf(-6.0), 1);
    c.profile = Profile::High10;
    let mut enc = Encoder::new(c).unwrap();
    let mut stream = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let cw = w / 2;
        let half = |p: &[u16]| -> Vec<u16> {
            (0..h / 2).flat_map(|r| (0..cw).map(move |x| (r, x))).map(|(r, x)| (p[2 * r * cw + x] + p[(2 * r + 1) * cw + x]).div_ceil(2)).collect()
        };
        let (u, v) = (half(&f.u), half(&f.v));
        for p in enc.encode16(&YuvFrame16 { y: &f.y, u: &u, v: &v, y_stride: w, uv_stride: cw }, i as i64).unwrap() {
            stream.extend(p.data);
        }
    }
    for p in enc.flush() {
        stream.extend(p.data);
    }
    let path = out_dir("high422").join("detail420.h264");
    std::fs::write(&path, &stream).unwrap();
    let (err, dec) = ffmpeg_decode_as(&path, "yuv422p10le");
    assert!(err.trim().is_empty(), "{err}");
    let (_, pc420) = mean_psnr(&dec, &frames);
    eprintln!("chroma PSNR: High 4:2:2 {pc422:.2} dB, High 10 4:2:0 {pc420:.2} dB");
    assert!(pc422 > pc420 + 6.0, "4:2:2 {pc422:.2} dB vs 4:2:0 {pc420:.2} dB");
}

/// High 4:2:2 Intra: every picture is an IDR, the SPS has constraint_set3 and no reference frames.
#[test]
fn intra_only_is_high422_intra() {
    if ffmpeg().is_none() || ffprobe().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 6);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
    let mut c = cfg(w, h, Preset::Balanced, 3, RateControl::Crf(18.0), 2);
    c.intra_only = true;
    c.keyint = 250;
    let run = encode(c, &frames);
    verify("intra", &run, w, h, n);
    assert!(run.packets.iter().all(|p| p.keyframe && p.frame_type == FrameType::Idr));
    assert!(run.packets.iter().all(|p| p.pts == p.dts), "no reordering");
    let info = probe(&out_dir("high422").join("intra.h264"), "stream=profile,pix_fmt,bits_per_raw_sample,refs");
    assert!(info.contains("profile=High 4:2:2 Intra"), "{info}");
    assert!(info.contains("pix_fmt=yuv422p10le"), "{info}");
    assert!(info.contains("bits_per_raw_sample=10"), "{info}");
    let first = String::from_utf8_lossy(&run.packets[0].data).into_owned();
    assert!(first.contains("profile=high422intra") && first.contains("bitdepth=10") && first.contains("refs=0"), "{first}");
}

#[test]
fn ffprobe_reports_high422() {
    if ffmpeg().is_none() || ffprobe().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 5);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
    let run = encode(cfg(w, h, Preset::Speed, 2, RateControl::Qp(28), 1), &frames);
    verify("probe", &run, w, h, n);
    let info = probe(&out_dir("high422").join("probe.h264"), "stream=profile,pix_fmt,width,height");
    assert!(info.contains("profile=High 4:2:2\n"), "{info}");
    assert!(info.contains("pix_fmt=yuv422p10le"), "{info}");
    assert!(info.contains("width=176") && info.contains("height=144"), "{info}");
}

/// MP4-style samples: the avcC carries profile 122, chroma_format 2 and the bit depths, and the
/// length-prefixed stream rebuilt from it decodes bit-exactly.
#[test]
fn length_prefixed_with_avcc() {
    if ffmpeg().is_none() {
        return;
    }
    let (w, h, n) = (176, 144, 6);
    let frames: Vec<Yuv16> = (0..n).map(|t| synth422(w, h, t, 10)).collect();
    let mut c = cfg(w, h, Preset::Speed, 1, RateControl::Qp(26), 1);
    c.format = PacketFormat::LengthPrefixed;
    let mut enc = Encoder::new(c).unwrap();
    enc.set_recon_capture(true);
    let avcc = enc.avcc();
    assert_eq!(&avcc[..6], &[1, 122, 0, avcc[3], 0xFF, 0xE1]);
    let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
    let sps = avcc[8..8 + sps_len].to_vec();
    let pps_len = u16::from_be_bytes([avcc[9 + sps_len], avcc[10 + sps_len]]) as usize;
    let pps = avcc[11 + sps_len..11 + sps_len + pps_len].to_vec();
    // High-profile extension: chroma_format 2, bit depths 10, no SPS extensions
    assert_eq!(&avcc[11 + sps_len + pps_len..], &[0xFE, 0xFA, 0xFA, 0x00]);
    let mut packets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        packets.extend(enc.encode16(&frame(f), i as i64).unwrap());
    }
    let run = finish(enc, packets);
    let mut stream = Vec::new();
    for nal in [&sps, &pps] {
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(nal);
    }
    for p in &run.packets {
        let mut i = 0;
        while i < p.data.len() {
            let len = u32::from_be_bytes(p.data[i..i + 4].try_into().unwrap()) as usize;
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(&p.data[i + 4..i + 4 + len]);
            i += 4 + len;
        }
    }
    verify("avcc", &Run { stream, packets: run.packets, recon: run.recon }, w, h, n);
}

/// 4:2:0-sized or short planes are too short for a 4:2:2 encoder, through 16-bit and 8-bit input
/// alike: an error, not a panic or a wrong read.
#[test]
fn rejects_420_sized_chroma_and_bad_settings() {
    let mut enc = Encoder::new(cfg(64, 64, Preset::Speed, 0, RateControl::Qp(26), 1)).unwrap();
    let y = vec![512u16; 64 * 64];
    let c420 = vec![512u16; 32 * 32];
    assert!(matches!(enc.encode16(&YuvFrame16 { y: &y, u: &c420, v: &c420, y_stride: 64, uv_stride: 32 }, 0), Err(Error::BadFrame)));
    // one chroma row short of 64, a short Cr only, and short luma
    let short_c = vec![512u16; 32 * 63];
    let c422_16 = vec![512u16; 32 * 64];
    let short_y = vec![512u16; 64 * 63];
    for (what, f) in [
        ("short chroma", YuvFrame16 { y: &y, u: &short_c, v: &short_c, y_stride: 64, uv_stride: 32 }),
        ("short Cr", YuvFrame16 { y: &y, u: &c422_16, v: &short_c, y_stride: 64, uv_stride: 32 }),
        ("short luma", YuvFrame16 { y: &short_y, u: &c422_16, v: &c422_16, y_stride: 64, uv_stride: 32 }),
        ("chroma stride below the plane width", YuvFrame16 { y: &y, u: &c422_16, v: &c422_16, y_stride: 64, uv_stride: 31 }),
    ] {
        assert!(matches!(enc.encode16(&f, 0), Err(Error::BadFrame)), "16-bit {what}");
    }
    // the 8-bit path into the same 10-bit 4:2:2 encoder
    let (y8, short_y8) = (vec![128u8; 64 * 64], vec![128u8; 64 * 63]);
    let (c420_8, short_c8, c422_8) = (vec![128u8; 32 * 32], vec![128u8; 32 * 63], vec![128u8; 32 * 64]);
    for (what, f) in [
        ("4:2:0-sized chroma", YuvFrame { y: &y8, u: &c420_8, v: &c420_8, y_stride: 64, uv_stride: 32 }),
        ("short chroma", YuvFrame { y: &y8, u: &short_c8, v: &short_c8, y_stride: 64, uv_stride: 32 }),
        ("short Cr", YuvFrame { y: &y8, u: &c422_8, v: &short_c8, y_stride: 64, uv_stride: 32 }),
        ("short luma", YuvFrame { y: &short_y8, u: &c422_8, v: &c422_8, y_stride: 64, uv_stride: 32 }),
    ] {
        assert!(matches!(enc.encode(&f, 0), Err(Error::BadFrame)), "8-bit {what}");
    }
    let c422 = vec![512u16; 32 * 64];
    assert!(enc.encode16(&YuvFrame16 { y: &y, u: &c422, v: &c422, y_stride: 64, uv_stride: 32 }, 0).is_ok());
    for bits in [0u8, 7, 11, 16] {
        let mut c = cfg(64, 64, Preset::Speed, 0, RateControl::Qp(26), 1);
        c.bit_depth = bits;
        assert!(matches!(Encoder::new(c), Err(Error::InvalidConfig(_))), "bit depth {bits}");
    }
    // 8-bit input is taken at 8 bits and scaled up exactly at 10
    let src = synth(64, 64, 0);
    let mut enc = Encoder::new(cfg(64, 64, Preset::Speed, 0, RateControl::Qp(26), 1)).unwrap();
    let (u, v) = (vec![128u8; 32 * 64], vec![128u8; 32 * 64]);
    assert!(enc.encode(&YuvFrame { y: &src.y, u: &u, v: &v, y_stride: 64, uv_stride: 32 }, 0).is_ok());
}
