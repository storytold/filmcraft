//! HEVC oracle tests: ffmpeg decodes silently plus frame-count plus bit-exact-vs-recon on I/P mixes and odd sizes.
//! Mirrors `crates/h264enc/tests/ffmpeg_oracle.rs`; skipped when ffmpeg is absent via `filmcraft_testkit::require_ffmpeg!`.

use filmcraft_hevcenc::{Encoder, EncoderConfig, Packet, Preset, RateControl, ReconFrame, YuvFrame};

struct Yuv {
    w: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Yuv {
    fn frame(&self) -> YuvFrame<'_> {
        YuvFrame { y: &self.y, u: &self.u, v: &self.v, y_stride: self.w, uv_stride: self.w / 2 }
    }
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

/// Four-region synth (gradient + grain + text strokes + panning texture), mirroring the h264enc oracle source.
fn synth(w: usize, h: usize, t: usize) -> Yuv {
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    let tf = t as f32;
    for yy in 0..h {
        for xx in 0..w {
            let region = (xx * 2 / w) + 2 * (yy * 2 / h);
            let val: f32 = match region {
                0 => 128.0 + 90.0 * ((xx as f32 / w as f32 * 6.0 + tf * 0.07).sin() * (yy as f32 / h as f32 * 4.0 - tf * 0.05).cos()),
                1 => {
                    let n = (hash((xx as u32).wrapping_mul(7919) ^ (yy as u32).wrapping_mul(104729) ^ (t as u32).wrapping_mul(1_000_003)) & 31) as f32;
                    60.0 + (xx as f32 * 0.1 + yy as f32 * 0.05) % 120.0 + n
                }
                2 => {
                    let sy = yy + t;
                    let (gx, gy, lx, ly) = (xx / 12, sy / 16, xx % 12, sy % 16);
                    let bits = hash((gx as u32) * 31 + gy as u32 * 977);
                    let inr = |v: usize, a: usize, b: usize| (a..=b).contains(&v);
                    let bar = inr(lx, 2, 9);
                    let stroke = (inr(lx, 2, 3) && bits & 1 != 0)
                        || (inr(lx, 8, 9) && bits & 2 != 0)
                        || (inr(ly, 3, 4) && bar && bits & 4 != 0)
                        || (inr(ly, 8, 9) && bar && bits & 8 != 0)
                        || (inr(ly, 12, 13) && bar && bits & 16 != 0);
                    if ly < 14 && lx < 11 && stroke { 235.0 } else { 20.0 }
                }
                _ => {
                    let (px, py) = (xx as f32 + tf * 1.75, yy as f32 + tf * 0.5);
                    128.0
                        + ((px / 9.0).sin() + (py / 7.0).cos() + ((px + py) / 23.0).sin()) * 40.0
                        + if ((px / 32.0).floor() as i64 + (py / 32.0).floor() as i64) % 2 == 0 { 30.0 } else { -30.0 }
                }
            };
            y[yy * w + xx] = val.clamp(0.0, 255.0) as u8;
        }
    }
    for yy in 0..h / 2 {
        for xx in 0..w / 2 {
            u[yy * w / 2 + xx] = (128.0 + 60.0 * (xx as f32 / (w / 2) as f32 * 3.0 + tf * 0.03).sin()) as u8;
            v[yy * w / 2 + xx] = (128.0 + 50.0 * (yy as f32 / (h / 2) as f32 * 5.0 - tf * 0.04).cos() * (xx as f32 / (w / 2) as f32)) as u8;
        }
    }
    Yuv { w, y, u, v }
}

fn cfg(w: usize, h: usize, preset: Preset, rate: RateControl) -> EncoderConfig {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c.preset = preset;
    c.rate = rate;
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

fn ffmpeg_decode(path: &std::path::Path) -> (String, Vec<u8>) {
    let out = std::process::Command::new(filmcraft_testkit::ffmpeg().unwrap())
        .args(["-nostdin", "-hide_banner", "-v", "warning", "-err_detect", "+crccheck+bitstream+buffer+explode", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    let mut diag = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        diag.push_str(&format!("ffmpeg exited with {}", out.status));
    }
    (diag, out.stdout)
}

/// Silent decode + exact frame count + keyframe/DTS ordering (ffmpeg conformance).
/// Bit-exact-vs-recon lives in `bit_exact_vs_recon` (ignored until the
/// multi-coefficient residual drift is fixed; see that test).
fn verify_conformance(name: &str, run: &Run, w: usize, h: usize, n: usize) {
    let dir = filmcraft_testkit::fixtures_dir(&format!("hevcenc/{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.h265"));
    std::fs::write(&path, &run.stream).unwrap();
    let (err, dec) = ffmpeg_decode(&path);
    assert!(err.trim().is_empty(), "{name}: ffmpeg reported errors: {err}");
    let fs = w * h * 3 / 2;
    assert_eq!(dec.len(), n * fs, "{name}: decoded frame count");
    assert_eq!(run.recon.len(), n);
}

fn verify_bit_exact(name: &str, run: &Run, w: usize, h: usize, n: usize) {
    let dir = filmcraft_testkit::fixtures_dir(&format!("hevcenc/{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.h265"));
    // Write this run's own stream: an earlier conformance run may have left a stale file behind.
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
}

fn cases() -> [(&'static str, usize, usize, Preset, RateControl); 5] {
    [
        ("idr_only_qp32", 64, 64, Preset::Balanced, RateControl::Qp(32)),
        ("ip_speed_qp26", 128, 96, Preset::Speed, RateControl::Qp(26)),
        ("ip_quality_crf23", 160, 112, Preset::Quality, RateControl::Crf(23.0)),
        ("odd_66x34", 66, 34, Preset::Balanced, RateControl::Qp(28)),
        ("odd_130x98_crf", 130, 98, Preset::Speed, RateControl::Crf(26.0)),
    ]
}

#[test]
fn conformance_ip_mixes_and_odd_sizes() {
    let _ff = filmcraft_testkit::require_ffmpeg!();
    let _probe = filmcraft_testkit::require_ffprobe!();
    for (name, w, h, preset, rate) in cases() {
        let n = 6;
        let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
        let run = encode(cfg(w, h, preset, rate), &frames);
        verify_conformance(name, &run, w, h, n);
        assert!(run.packets[0].keyframe);
        for w2 in run.packets.windows(2) {
            assert!(w2[1].dts >= w2[0].dts, "dts must not decrease");
        }
    }
}

/// Bit-exact-vs-recon (post-deblock) on every frame: the captured encoder
/// reconstruction must match ffmpeg's decode sample-for-sample (luma + chroma).
/// This caught five silent-desync encoder bugs (all found by diffing the encoder's
/// emitted bins against the decoder's parsed bins on the first failing TB/CU,
/// then proving the fix bit-exact): `last_sig_coeff` suffixes emitted
/// per-component instead of after both prefixes; a merge-with-zero-residual
/// fallback signalling skip bins after `skip_flag = 0`; `mvp_flag` emitted before
/// `mvd_coding` plus a non-compressing AMVP list; the MPM above-mode missing the
/// CTU-row unavailability rule; and the deblock map conflating chroma-only
/// residue into luma boundary strengths.
#[test]
fn bit_exact_vs_recon() {
    let _ff = filmcraft_testkit::require_ffmpeg!();
    let _probe = filmcraft_testkit::require_ffprobe!();
    for (name, w, h, preset, rate) in cases() {
        let n = 6;
        let frames: Vec<Yuv> = (0..n).map(|t| synth(w, h, t)).collect();
        let run = encode(cfg(w, h, preset, rate), &frames);
        verify_bit_exact(name, &run, w, h, n);
    }
}
