//! ffmpeg as an external decoder oracle (skipped when absent): every stream, wrapped in a minimal
//! CAF file, must decode without a message to exactly the samples that went in. The size is also
//! compared with ffmpeg's own ALAC encoder on the same material.

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_alac::{Encoder, EncoderConfig};

fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("alac oracle")
}

fn rng(seed: &mut u64) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Test signals at `bits`, near full scale: music-like (two sines and a little noise), a sweep,
/// white noise, a full-scale square, silence and a lone click.
fn signal(kind: &str, n: usize, bits: u8, seed: u64) -> Vec<i32> {
    let max = ((1i64 << (bits - 1)) - 1) as f64;
    let mut r = seed | 1;
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let v = match kind {
                "music" => 0.5 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() + 0.3 * (2.0 * std::f64::consts::PI * 1375.0 * t).sin() + 0.002 * rng(&mut r),
                "sweep" => (2.0 * std::f64::consts::PI * (40.0 + 4000.0 * t) * t).sin() * 0.9,
                "noise" => rng(&mut r),
                "square" => {
                    if (i / 37) % 2 == 0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                "click" => f64::from(u8::from(i == n / 2)),
                _ => 0.0,
            };
            ((v * max).round() as i64).clamp(-(max as i64) - 1, max as i64) as i32
        })
        .collect()
}

/// A CAF file (Apple Core Audio Format) holding the frames: `desc`, `kuki` (the magic cookie),
/// `pakt` (one variable-length size per packet) and `data`.
fn caf(frames: &[Vec<u8>], cookie: &[u8], cfg: EncoderConfig, total: u64) -> Vec<u8> {
    let mut f = b"caff".to_vec();
    f.extend_from_slice(&[0, 1, 0, 0]);
    let chunk = |f: &mut Vec<u8>, id: &[u8; 4], body: &[u8]| {
        f.extend_from_slice(id);
        f.extend_from_slice(&(body.len() as i64).to_be_bytes());
        f.extend_from_slice(body);
    };
    let mut desc = (cfg.sample_rate as f64).to_be_bytes().to_vec();
    desc.extend_from_slice(b"alac");
    let flag: u32 = match cfg.bits_per_sample {
        16 => 1,
        20 => 2,
        24 => 3,
        _ => 4,
    };
    for v in [flag, 0, cfg.frame_length, u32::from(cfg.channels), 0] {
        desc.extend_from_slice(&v.to_be_bytes());
    }
    chunk(&mut f, b"desc", &desc);
    chunk(&mut f, b"kuki", cookie);
    let mut pakt = (frames.len() as i64).to_be_bytes().to_vec();
    pakt.extend_from_slice(&(total as i64).to_be_bytes());
    pakt.extend_from_slice(&0i32.to_be_bytes());
    let remainder = (frames.len() as u64 * u64::from(cfg.frame_length)).saturating_sub(total) as i32;
    pakt.extend_from_slice(&remainder.to_be_bytes());
    for fr in frames {
        let mut v = fr.len() as u32;
        let mut bytes = vec![(v & 0x7f) as u8];
        v >>= 7;
        while v > 0 {
            bytes.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        bytes.reverse();
        pakt.extend_from_slice(&bytes);
    }
    chunk(&mut f, b"pakt", &pakt);
    let mut data = 0u32.to_be_bytes().to_vec(); // edit count
    for fr in frames {
        data.extend_from_slice(fr);
    }
    chunk(&mut f, b"data", &data);
    f
}

fn encode(planar: &[Vec<i32>], cfg: EncoderConfig, chunk: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut enc = Encoder::new(cfg).unwrap();
    let n = planar.first().map_or(0, Vec::len);
    let mut frames = Vec::new();
    let mut at = 0;
    while at < n {
        let end = (at + chunk).min(n);
        let parts: Vec<&[i32]> = planar.iter().map(|c| &c[at..end]).collect();
        frames.extend(enc.encode_frames(&parts).unwrap());
        at = end;
    }
    frames.extend(enc.finish());
    (frames, enc.magic_cookie())
}

fn run(ffmpeg: &Path, args: &[&str], input: &Path) -> Vec<u8> {
    let out = Command::new(ffmpeg).args(["-v", "warning", "-i"]).arg(input).args(args).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.trim().is_empty(), "ffmpeg on {}: {err}", input.display());
    out.stdout
}

/// ffmpeg's output channel for each ALAC channel: it reorders 3 to 8 channels to its own layout.
fn ffmpeg_order(channels: usize) -> Vec<usize> {
    match channels {
        // ALAC C L R (Ls Rs) (LFE) → ffmpeg L R C (LFE) (Ls Rs)
        3 => vec![1, 2, 0],
        5 => vec![1, 2, 0, 3, 4],
        6 => vec![1, 2, 0, 5, 3, 4],
        _ => (0..channels).collect(),
    }
}

/// Encode, decode with ffmpeg, compare every sample; returns the encoded size in bytes.
fn round_trip(name: &str, planar: &[Vec<i32>], cfg: EncoderConfig, chunk: usize) -> Option<usize> {
    let ffmpeg = ffmpeg()?;
    let n = planar[0].len();
    let (frames, cookie) = encode(planar, cfg, chunk);
    let path = filmcraft_testkit::fixtures_dir("alac").join(format!("{name}.caf"));
    std::fs::write(&path, caf(&frames, &cookie, cfg, n as u64)).unwrap();
    // s32 holds each sample shifted to the top bits
    let raw = run(&ffmpeg, &["-f", "s32le", "-c:a", "pcm_s32le", "-"], &path);
    let bits = u32::from(cfg.bits_per_sample);
    let decoded: Vec<i32> = raw.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) >> (32 - bits)).collect();
    let ch = planar.len();
    assert_eq!(decoded.len(), n * ch, "{name}: sample count");
    let order = ffmpeg_order(ch);
    for (i, frame) in decoded.chunks_exact(ch).enumerate() {
        for (c, &src) in order.iter().enumerate() {
            assert_eq!(frame[c], planar[src][i], "{name}: channel {src} sample {i}");
        }
    }
    Some(frames.iter().map(Vec::len).sum())
}

#[test]
fn every_signal_round_trips_exactly() {
    for (kind, seed) in [("music", 1), ("sweep", 2), ("noise", 3), ("square", 4), ("silence", 5), ("click", 6)] {
        for bits in [16u8, 20, 24, 32] {
            let planar = vec![signal(kind, 20_000, bits, seed), signal(kind, 20_000, bits, seed + 100)];
            if round_trip(&format!("{kind}-{bits}"), &planar, EncoderConfig::new(48_000, 2, bits), 4800).is_none() {
                return;
            }
        }
    }
}

#[test]
fn channel_layouts_rates_and_frame_lengths() {
    let cases: [(&str, u8, u8, u32, usize, u32); 8] = [
        ("mono-16", 1, 16, 44_100, 9_999, 4096),
        ("mono-24-96k", 1, 24, 96_000, 30_000, 4096),
        ("5.1-24", 6, 24, 48_000, 12_345, 4096),
        ("5.1-16", 6, 16, 48_000, 9_000, 4096),
        ("3ch-16", 3, 16, 48_000, 5_000, 4096),
        ("short-frames", 2, 16, 48_000, 5_000, 1152),
        ("long-frames", 2, 24, 48_000, 40_000, 16_384),
        ("tiny", 2, 16, 48_000, 1, 4096),
    ];
    for (name, ch, bits, rate, n, fl) in cases {
        let planar: Vec<Vec<i32>> = (0..ch).map(|c| signal(if c == 3 { "silence" } else { "music" }, n, bits, u64::from(c) + 7)).collect();
        let cfg = EncoderConfig { frame_length: fl, ..EncoderConfig::new(rate, ch, bits) };
        if round_trip(name, &planar, cfg, 1000).is_none() {
            return;
        }
    }
}

/// On music-like stereo the stream is about as small as ffmpeg's own ALAC encoder makes it.
#[test]
fn size_close_to_ffmpegs_encoder() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let n = 96_000;
    let planar = vec![signal("music", n, 16, 11), signal("sweep", n, 16, 12)];
    let Some(ours) = round_trip("size-ours", &planar, EncoderConfig::new(48_000, 2, 16), n) else { return };
    let dir = filmcraft_testkit::fixtures_dir("alac");
    let pcm: Vec<u8> = (0..n).flat_map(|i| [planar[0][i] as i16, planar[1][i] as i16]).flat_map(i16::to_le_bytes).collect();
    let src = dir.join("size-src.s16");
    std::fs::write(&src, pcm).unwrap();
    let out = dir.join("size-ffmpeg.caf");
    let st = Command::new(&ffmpeg)
        .args(["-v", "error", "-y", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i"])
        .arg(&src)
        .args(["-c:a", "alac"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    // the data chunk: everything after the `data` id, size and edit count
    let theirs = std::fs::read(&out).unwrap();
    let at = theirs.windows(4).rposition(|w| w == b"data").unwrap() + 16;
    let theirs = theirs.len() - at;
    eprintln!("ALAC stereo music: ours {ours} bytes, ffmpeg {theirs} bytes, raw {}", n * 4);
    assert!(ours * 100 <= theirs * 110, "ours {ours} vs ffmpeg {theirs}");
}
