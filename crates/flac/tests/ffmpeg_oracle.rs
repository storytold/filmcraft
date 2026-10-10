//! ffmpeg as an external decoder oracle (skipped when absent): every stream must decode without an
//! error to exactly the samples that went in, with the STREAMINFO MD5 matching ffmpeg's hash of the
//! decoded audio.

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_flac::{Encoder, EncoderConfig, Error, STREAMINFO_OFFSET};

fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("flac oracle")
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// Test signals at `bps`, full scale: music-like (two sines and noise), a sweep, white noise,
/// a full-scale square, silence and a lone click.
fn signal(kind: &str, n: usize, bps: u8, seed: u64) -> Vec<i32> {
    let max = f64::from((1i32 << (bps - 1)) - 1);
    let mut r = Rng(seed | 1);
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let v = match kind {
                "music" => 0.5 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() + 0.3 * (2.0 * std::f64::consts::PI * 1375.0 * t).sin() + 0.05 * r.next(),
                "sweep" => (2.0 * std::f64::consts::PI * (40.0 + 4000.0 * t) * t).sin() * 0.9,
                "noise" => r.next(),
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
            ((v * max).round() as i32).clamp(-(max as i32) - 1, max as i32)
        })
        .collect()
}

fn encode(planar: &[Vec<i32>], cfg: EncoderConfig, chunk: usize) -> Result<Vec<u8>, Error> {
    let mut enc = Encoder::new(cfg)?;
    let mut file = enc.header();
    let n = planar.first().map_or(0, Vec::len);
    let mut at = 0;
    while at < n {
        let end = (at + chunk).min(n);
        let parts: Vec<&[i32]> = planar.iter().map(|c| &c[at..end]).collect();
        file.extend(enc.encode(&parts)?);
        at = end;
    }
    file.extend(enc.finish());
    let o = STREAMINFO_OFFSET as usize;
    file[o..o + 34].copy_from_slice(&enc.streaminfo());
    Ok(file)
}

fn run(ffmpeg: &Path, args: &[&str], input: &Path) -> (Vec<u8>, String) {
    let out = Command::new(ffmpeg).args(["-v", "error", "-i"]).arg(input).args(args).output().unwrap();
    assert!(out.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Encode, decode with ffmpeg, compare every sample and the MD5; returns the file size.
fn round_trip(name: &str, planar: &[Vec<i32>], cfg: EncoderConfig, chunk: usize) -> Option<usize> {
    let ffmpeg = ffmpeg()?;
    let bps = cfg.bits_per_sample;
    let rate = cfg.sample_rate;
    let file = encode(planar, cfg, chunk).unwrap();
    let path = filmcraft_testkit::fixtures_dir("flac").join(format!("{name}.flac"));
    std::fs::write(&path, &file).unwrap();

    // samples: s32 holds each one shifted to the top bits
    let (raw, err) = run(&ffmpeg, &["-f", "s32le", "-c:a", "pcm_s32le", "-"], &path);
    assert!(err.is_empty(), "{name}: decoder errors: {err}");
    let decoded: Vec<i32> = raw.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) >> (32 - bps)).collect();
    let n = planar[0].len();
    assert_eq!(decoded.len(), n * planar.len(), "{name}: sample count");
    for (i, frame) in decoded.chunks_exact(planar.len()).enumerate() {
        for (c, &v) in frame.iter().enumerate() {
            assert_eq!(v, planar[c][i], "{name}: channel {c} sample {i}");
        }
    }

    // STREAMINFO: total samples, rate, and the MD5 against ffmpeg's hash of the decoded audio. FLAC
    // hashes samples unscaled; ffmpeg rescales other sizes (20-bit becomes 24-bit), so only 8, 16
    // and 24 bit compare
    let si = &file[STREAMINFO_OFFSET as usize..STREAMINFO_OFFSET as usize + 34];
    let pcm = match bps {
        8 => Some("s8"),
        16 => Some("s16le"),
        24 => Some("s24le"),
        _ => None,
    };
    if let Some(pcm) = pcm {
        let (hash, _) = run(&ffmpeg, &["-c:a", &format!("pcm_{pcm}"), "-f", "hash", "-hash", "md5", "-"], &path);
        let hash = String::from_utf8_lossy(&hash);
        let ours: String = si[18..34].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash.trim().strip_prefix("MD5=").unwrap_or(""), ours, "{name}: MD5");
    }
    let total = (u64::from(si[13] & 0x0F) << 32) | u64::from(u32::from_be_bytes([si[14], si[15], si[16], si[17]]));
    assert_eq!(total, n as u64, "{name}: total samples");
    let si_rate = (u32::from(si[10]) << 12) | (u32::from(si[11]) << 4) | (u32::from(si[12]) >> 4);
    assert_eq!(si_rate, rate);
    Some(file.len())
}

#[test]
fn every_signal_round_trips_exactly() {
    for (kind, seed) in [("music", 1), ("sweep", 2), ("noise", 3), ("square", 4), ("silence", 5), ("click", 6)] {
        for bps in [16u8, 24] {
            let planar = vec![signal(kind, 20_000, bps, seed), signal(kind, 20_000, bps, seed + 100)];
            if round_trip(&format!("{kind}-{bps}"), &planar, EncoderConfig::new(48_000, 2, bps), 4800).is_none() {
                return;
            }
        }
    }
}

#[test]
fn channel_layouts_rates_and_sizes() {
    let cases: [(&str, u8, u8, u32, usize); 6] = [
        ("mono-8bit", 1, 8, 8_000, 3_001),
        ("mono-24-44k1", 1, 24, 44_100, 9_999),
        ("stereo-20bit", 2, 20, 96_000, 10_000),
        ("5.1-24", 6, 24, 48_000, 12_345),
        ("odd-rate", 2, 16, 37_800, 5_000),
        ("tiny", 2, 16, 48_000, 1),
    ];
    for (name, ch, bps, rate, n) in cases {
        let planar: Vec<Vec<i32>> = (0..ch).map(|c| signal(if c == 3 { "silence" } else { "music" }, n, bps, u64::from(c) + 7)).collect();
        if round_trip(name, &planar, EncoderConfig::new(rate, ch, bps), 1000).is_none() {
            return;
        }
    }
}

#[test]
fn levels_and_block_sizes_trade_time_for_size() {
    let planar = vec![signal("music", 48_000, 16, 11), signal("sweep", 48_000, 16, 12)];
    let mut sizes = Vec::new();
    for level in 0..=8 {
        let cfg = EncoderConfig { level, ..EncoderConfig::new(48_000, 2, 16) };
        let Some(size) = round_trip(&format!("level{level}"), &planar, cfg, 48_000) else { return };
        sizes.push(size);
    }
    // the effortful levels are never larger than the fastest, and all beat PCM
    assert!(sizes[8] <= sizes[0] && sizes[5] <= sizes[0], "{sizes:?}");
    assert!(sizes.iter().all(|&s| s < 48_000 * 4), "{sizes:?}");
    for bs in [16u16, 192, 1152, 4608, 65_535] {
        let cfg = EncoderConfig { block_size: bs, ..EncoderConfig::new(48_000, 2, 16) };
        round_trip(&format!("block{bs}"), &planar, cfg, 777);
    }
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    assert!(Encoder::new(EncoderConfig::new(48_000, 0, 16)).is_err());
    assert!(Encoder::new(EncoderConfig::new(48_000, 9, 16)).is_err());
    assert!(Encoder::new(EncoderConfig::new(48_000, 2, 32)).is_err());
    assert!(Encoder::new(EncoderConfig::new(0, 2, 16)).is_err());
    assert!(Encoder::new(EncoderConfig { block_size: 8, ..EncoderConfig::new(48_000, 2, 16) }).is_err());
    let mut enc = Encoder::new(EncoderConfig::new(48_000, 2, 16)).unwrap();
    assert_eq!(enc.encode(&[&[0, 1]]), Err(Error::Channels { expected: 2, got: 1 }));
    assert_eq!(enc.encode(&[&[0, 1], &[0]]), Err(Error::UnequalChannels));
    assert_eq!(enc.encode(&[&[40_000], &[0]]), Err(Error::SampleRange(40_000)));
    assert_eq!(enc.encode(&[&[-32_769], &[0]]), Err(Error::SampleRange(-32_769)));
    // nothing encoded: no frames, and a header that still says "unknown"
    assert!(enc.finish().is_empty());
    assert_eq!(&enc.header()[..4], b"fLaC");
}
