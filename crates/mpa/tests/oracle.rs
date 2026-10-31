//! Layer I and Layer II against ffmpeg's decoder as the reference.
//!
//! - Encoded streams: ffmpeg's MP2 encoder (mono, stereo, MPEG-1 and MPEG-2 sampling frequencies,
//!   every Layer II allocation table, and bit rate / mode combinations ISO/IEC 11172-3 does not
//!   allow but ffmpeg writes: its default for mono is 384 kbit/s); twolame, when installed, for
//!   joint stereo and dual channel.
//! - Synthetic streams: valid random frames for what no installed encoder writes: Layer I (mono,
//!   stereo, dual channel, joint stereo at every bound) and Layer II joint stereo / dual channel
//!   with every allocation table. ffmpeg decodes them as it would any stream.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_bitstream::BitWriter;
use filmcraft_mpa::{Decoder, Mode, parse_header};

/// A stream from an encoder.
struct Encoded {
    name: &'static str,
    input: &'static str,
    /// Encoder arguments after the input (ffmpeg) or flags (twolame).
    enc: &'static [&'static str],
    rate: u32,
    channels: usize,
    /// The bit rate the frames must carry (proves which combination is covered).
    kbps: u32,
    mode: Mode,
    twolame: bool,
}

const TONES: &str = "aevalsrc=exprs=0.3*sin(2*PI*441*t)+0.1*sin(2*PI*3100*t)|0.25*sin(2*PI*660*t)+0.1*sin(2*PI*(500+4000*t)*t)";

const ENCODED: &[Encoded] = &[
    // ffmpeg's default for mono: 384 kbit/s, which ISO/IEC 11172-3 does not allow (table a)
    Encoded {
        name: "mono_48k_default",
        input: "sine=frequency=1000:sample_rate=48000",
        enc: &[],
        rate: 48_000,
        channels: 1,
        kbps: 384,
        mode: Mode::Mono,
        twolame: false,
    },
    Encoded {
        name: "stereo_48k_192k",
        input: TONES,
        enc: &["-ar", "48000", "-b:a", "192k"],
        rate: 48_000,
        channels: 2,
        kbps: 192,
        mode: Mode::Stereo,
        twolame: false,
    },
    // table b
    Encoded {
        name: "stereo_44k_256k",
        input: TONES,
        enc: &["-ar", "44100", "-b:a", "256k"],
        rate: 44_100,
        channels: 2,
        kbps: 256,
        mode: Mode::Stereo,
        twolame: false,
    },
    // table b, mono above 192 kbit/s (not allowed)
    Encoded {
        name: "mono_44k_320k",
        input: "anoisesrc=color=pink:sample_rate=44100:amplitude=0.3:seed=5",
        enc: &["-b:a", "320k"],
        rate: 44_100,
        channels: 1,
        kbps: 320,
        mode: Mode::Mono,
        twolame: false,
    },
    // table c, and stereo below 64 kbit/s (not allowed)
    Encoded {
        name: "stereo_48k_32k",
        input: TONES,
        enc: &["-ar", "48000", "-b:a", "32k"],
        rate: 48_000,
        channels: 2,
        kbps: 32,
        mode: Mode::Stereo,
        twolame: false,
    },
    Encoded {
        name: "stereo_44k_96k",
        input: TONES,
        enc: &["-ar", "44100", "-b:a", "96k"],
        rate: 44_100,
        channels: 2,
        kbps: 96,
        mode: Mode::Stereo,
        twolame: false,
    },
    // table d
    Encoded {
        name: "mono_32k_48k",
        input: "sine=frequency=440:sample_rate=32000",
        enc: &["-b:a", "48k"],
        rate: 32_000,
        channels: 1,
        kbps: 48,
        mode: Mode::Mono,
        twolame: false,
    },
    // table a at 32 kHz
    Encoded {
        name: "stereo_32k_128k",
        input: TONES,
        enc: &["-ar", "32000", "-b:a", "128k"],
        rate: 32_000,
        channels: 2,
        kbps: 128,
        mode: Mode::Stereo,
        twolame: false,
    },
    // MPEG-2 lower sampling frequencies (ISO/IEC 13818-3 table)
    Encoded {
        name: "lsf_stereo_24k_160k",
        input: TONES,
        enc: &["-ar", "24000", "-b:a", "160k"],
        rate: 24_000,
        channels: 2,
        kbps: 160,
        mode: Mode::Stereo,
        twolame: false,
    },
    Encoded {
        name: "lsf_mono_22k_64k",
        input: "sine=frequency=700:sample_rate=22050",
        enc: &["-b:a", "64k"],
        rate: 22_050,
        channels: 1,
        kbps: 64,
        mode: Mode::Mono,
        twolame: false,
    },
    Encoded {
        name: "lsf_stereo_16k_32k",
        input: TONES,
        enc: &["-ar", "16000", "-b:a", "32k"],
        rate: 16_000,
        channels: 2,
        kbps: 32,
        mode: Mode::Stereo,
        twolame: false,
    },
    // twolame: joint stereo (intensity coding above the bound) and dual channel
    Encoded {
        name: "twolame_joint_48k_128k",
        input: TONES,
        enc: &["-b", "128", "-m", "j"],
        rate: 48_000,
        channels: 2,
        kbps: 128,
        mode: Mode::JointStereo,
        twolame: true,
    },
    Encoded {
        name: "twolame_joint_44k_64k",
        input: TONES,
        enc: &["-b", "64", "-m", "j"],
        rate: 44_100,
        channels: 2,
        kbps: 64,
        mode: Mode::JointStereo,
        twolame: true,
    },
    Encoded {
        name: "twolame_dual_44k_192k",
        input: TONES,
        enc: &["-b", "192", "-m", "d"],
        rate: 44_100,
        channels: 2,
        kbps: 192,
        mode: Mode::DualChannel,
        twolame: true,
    },
];

fn tool(name: &str) -> Option<PathBuf> {
    filmcraft_testkit::oracle::find_tool(name)
}

fn make_encoded(ff: &Path, s: &Encoded) -> Option<PathBuf> {
    let out = filmcraft_testkit::fixtures_dir("mpa").join(format!("{}.mp2", s.name));
    if s.twolame {
        let twolame = tool("twolame")?;
        return filmcraft_testkit::fixtures::generate(&out, |tmp| {
            let wav = tmp.with_extension("wav");
            let made = Command::new(ff)
                .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("{}:s={}", s.input, s.rate), "-t", "1", "-c:a", "pcm_s16le"])
                .arg(&wav)
                .stdin(Stdio::null())
                .status()
                .is_ok_and(|x| x.success())
                && Command::new(twolame).arg("--quiet").args(s.enc).arg(&wav).arg(tmp).stdin(Stdio::null()).status().is_ok_and(|x| x.success());
            let _ = std::fs::remove_file(&wav);
            made
        });
    }
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        Command::new(ff)
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", s.input, "-t", "1", "-c:a", "mp2"])
            .args(s.enc)
            .args(["-f", "mp2"])
            .arg(tmp)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|x| x.success())
    })
}

/// The frames of a raw stream (an ID3 tag or garbage before the first frame is not expected).
fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut p = 0;
    while let Some(h) = data.get(p..).and_then(|b| parse_header(b).ok()) {
        let Some(f) = data.get(p..p + h.frame_bytes).filter(|_| h.frame_bytes > 0) else { break };
        out.push(f);
        p += h.frame_bytes;
    }
    out
}

/// ffmpeg's decode of `file`, interleaved f32.
fn ffmpeg_decode(ff: &Path, file: &Path) -> Vec<f32> {
    let o = Command::new(ff).args(["-v", "error", "-f", "mp3", "-i"]).arg(file).args(["-f", "f32le", "-"]).output().unwrap();
    assert!(o.status.success(), "{}: {}", file.display(), String::from_utf8_lossy(&o.stderr));
    o.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Decode `file` with our decoder and return the worst channel's SNR against ffmpeg (dB).
fn snr_vs_ffmpeg(ff: &Path, file: &Path, rate: u32, channels: usize) -> f64 {
    let data = std::fs::read(file).unwrap();
    let fr = frames(&data);
    assert!(fr.len() > 10, "{}: {} frames", file.display(), fr.len());
    let want = ffmpeg_decode(ff, file);
    let n = want.len() / channels;
    // ffmpeg drops the first frame of a stream whose second frame changes the channel mode
    // (twolame starts in joint stereo and switches to stereo): start there too
    let first = if first_frame_mode_changes(&data) { 1 } else { 0 };
    let mut dec = Decoder::new();
    let mut ours: Vec<Vec<f32>> = vec![Vec::new(); channels];
    for f in &fr[first..] {
        let d = dec.decode(f).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!((d.header.sample_rate, d.channels.len()), (rate, channels), "{}", file.display());
        for (o, c) in ours.iter_mut().zip(d.channels) {
            o.extend(c);
        }
    }
    assert_eq!(n, ours[0].len(), "{}: sample count", file.display());
    let mut worst = f64::INFINITY;
    for (c, ours) in ours.iter().enumerate() {
        let (mut sig, mut err) = (0f64, 0f64);
        for (i, &a) in ours.iter().enumerate() {
            let b = want[i * channels + c] as f64;
            sig += b * b;
            err += (a as f64 - b) * (a as f64 - b);
        }
        worst = worst.min(10.0 * (sig / err.max(1e-30)).log10());
    }
    worst
}

fn first_frame_mode_changes(data: &[u8]) -> bool {
    let f = frames(data);
    f.len() > 1 && parse_header(f[0]).unwrap().mode != parse_header(f[1]).unwrap().mode
}

/// The minimum SNR against ffmpeg's float decoder. ffmpeg's own fixed-point decoder agrees with
/// it to ~77 dB (the error is at the 16-bit LSB, ~-96 dBFS); we measure 73-84 dB. A wrong table
/// entry or a misread field costs tens of dB.
const MIN_SNR: f64 = 70.0;

#[test]
fn encoded_streams_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for s in ENCODED {
        let Some(f) = make_encoded(&ff, s) else {
            assert!(s.twolame, "could not generate {}", s.name);
            eprintln!("{}: twolame not installed, skipped", s.name);
            continue;
        };
        let data = std::fs::read(&f).unwrap();
        let h = parse_header(&data).unwrap();
        assert_eq!((h.layer, h.bitrate_kbps, h.mode), (2, s.kbps, s.mode), "{}", s.name);
        let snr = snr_vs_ffmpeg(&ff, &f, s.rate, s.channels);
        println!("{}: {} Hz, {} kbit/s, {:?}: SNR vs ffmpeg {snr:.1} dB", s.name, s.rate, s.kbps, s.mode);
        assert!(snr >= MIN_SNR, "{}: SNR {snr:.1} dB < {MIN_SNR}", s.name);
    }
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// Header bytes (no CRC): `lsf` MPEG-2, `layer` 1/2, the indices, `mode` 0-3, `ext`.
fn header(lsf: bool, layer: u8, bitrate_index: u8, rate_index: u8, mode: u8, ext: u8) -> [u8; 4] {
    let version = if lsf { 0b10 } else { 0b11 };
    [0xFF, 0xE0 | version << 3 | (4 - layer) << 1 | 1, bitrate_index << 4 | rate_index << 2, mode << 6 | ext << 4]
}

/// A valid random Layer I frame: random allocations (as many as the frame holds), scalefactors
/// and samples.
fn random_layer1(rng: &mut Rng, h: [u8; 4]) -> Vec<u8> {
    let hd = parse_header(&h).unwrap();
    let nch = hd.channels();
    let bound = if hd.mode == Mode::JointStereo { 4 * (hd.mode_extension as usize + 1) } else { 32 };
    let mut alloc = [[0u32; 32]; 2];
    for sb in 0..32 {
        for ch in 0..nch {
            if (ch == 0 || sb < bound) && rng.below(3) != 0 {
                alloc[ch][sb] = 1 + rng.below(14);
            }
        }
        if sb >= bound {
            alloc[1][sb] = alloc[0][sb];
        }
    }
    let bits = |alloc: &[[u32; 32]; 2]| {
        let mut n = 32 + 4 * (32 + bound.min(32) * (nch - 1));
        for sb in 0..32 {
            for ch in 0..nch {
                if alloc[ch][sb] != 0 {
                    n += 6;
                    if ch == 0 || sb < bound {
                        n += 12 * (alloc[ch][sb] as usize + 1);
                    }
                }
            }
        }
        n
    };
    // drop allocations from the top until the frame holds them
    let mut sb = 31;
    while bits(&alloc) > hd.frame_bytes * 8 {
        alloc[0][sb] = 0;
        alloc[1][sb] = 0;
        sb -= 1;
    }
    let mut w = BitWriter::new();
    w.write_bytes(&h);
    for sb in 0..32 {
        for ch in 0..if sb < bound { nch } else { 1 } {
            w.write_bits(alloc[ch][sb], 4);
        }
    }
    let mut scf = || 10 + rng.below(40);
    let mut scfs = vec![];
    for sb in 0..32 {
        for ch in 0..nch {
            if alloc[ch][sb] != 0 {
                scfs.push(scf());
            }
        }
    }
    for s in scfs {
        w.write_bits(s, 6);
    }
    for _ in 0..12 {
        for sb in 0..32 {
            for ch in 0..if sb < bound { nch } else { 1 } {
                let nb = alloc[ch][sb] + 1;
                if alloc[ch][sb] != 0 {
                    // any code but the forbidden all-ones
                    w.write_bits(rng.below((1 << nb) - 1), nb);
                }
            }
        }
    }
    let mut f = w.finish();
    f.resize(hd.frame_bytes, 0);
    f
}

/// Allocation field widths (nbal) per subband of the Layer II table the test frames use, by the
/// same selection as the decoder; every table's allocation value 1 is the 3-step class.
fn nbal(lsf: bool, rate: u32, kbps_per_channel: u32) -> Vec<u32> {
    let rep = |v: &[(u32, usize)]| v.iter().flat_map(|&(n, k)| std::iter::repeat_n(n, k)).collect::<Vec<_>>();
    if lsf {
        rep(&[(4, 4), (3, 7), (2, 19)])
    } else if (rate == 48_000 && kbps_per_channel >= 56) || (56..=80).contains(&kbps_per_channel) {
        rep(&[(4, 11), (3, 12), (2, 4)])
    } else if kbps_per_channel >= 96 {
        rep(&[(4, 11), (3, 12), (2, 7)])
    } else if rate == 32_000 {
        rep(&[(4, 2), (3, 10)])
    } else {
        rep(&[(4, 2), (3, 6)])
    }
}

/// A valid random Layer II frame using only the 3-step class (allocation value 1, grouped).
fn random_layer2(rng: &mut Rng, h: [u8; 4]) -> Vec<u8> {
    let hd = parse_header(&h).unwrap();
    let nch = hd.channels();
    let nbal = nbal(h[1] & 0x08 == 0, hd.sample_rate, hd.bitrate_kbps / nch as u32);
    let sblimit = nbal.len();
    let bound = if hd.mode == Mode::JointStereo { (4 * (hd.mode_extension as usize + 1)).min(sblimit) } else { sblimit };
    let mut alloc = [[false; 32]; 2];
    let mut scfsi = [[0u32; 32]; 2];
    for sb in 0..sblimit {
        for ch in 0..nch {
            alloc[ch][sb] = if ch == 1 && sb >= bound { alloc[0][sb] } else { rng.below(3) != 0 };
            scfsi[ch][sb] = rng.below(4);
        }
    }
    let scf_bits = |s: u32| {
        if s == 0 {
            18
        } else if s == 2 {
            6
        } else {
            12
        }
    };
    let bits = |alloc: &[[bool; 32]; 2]| {
        let mut n = 32;
        for sb in 0..sblimit {
            n += nbal[sb] as usize * if sb < bound { nch } else { 1 };
            for ch in 0..nch {
                if alloc[ch][sb] {
                    n += 2 + scf_bits(scfsi[ch][sb]);
                    if ch == 0 || sb < bound {
                        n += 12 * 5;
                    }
                }
            }
        }
        n
    };
    let mut sb = sblimit - 1;
    while bits(&alloc) > hd.frame_bytes * 8 {
        alloc[0][sb] = false;
        alloc[1][sb] = false;
        sb -= 1;
    }
    let mut w = BitWriter::new();
    w.write_bytes(&h);
    for sb in 0..sblimit {
        for ch in 0..if sb < bound { nch } else { 1 } {
            w.write_bits(alloc[ch][sb] as u32, nbal[sb]);
        }
    }
    for sb in 0..sblimit {
        for ch in 0..nch {
            if alloc[ch][sb] {
                w.write_bits(scfsi[ch][sb], 2);
            }
        }
    }
    for sb in 0..sblimit {
        for ch in 0..nch {
            if alloc[ch][sb] {
                for _ in 0..scf_bits(scfsi[ch][sb]) / 6 {
                    w.write_bits(10 + rng.below(40), 6);
                }
            }
        }
    }
    for _ in 0..12 {
        for sb in 0..sblimit {
            for ch in 0..if sb < bound { nch } else { 1 } {
                if alloc[ch][sb] {
                    w.write_bits(rng.below(27), 5);
                }
            }
        }
    }
    let mut f = w.finish();
    f.resize(hd.frame_bytes, 0);
    f
}

/// (name, layer, MPEG-2, bitrate index, sampling frequency index, mode, mode extension)
const SYNTHETIC: &[(&str, u8, bool, u8, u8, u8, u8)] = &[
    ("l1_mono_44k_192k", 1, false, 6, 0, 3, 0),
    ("l1_stereo_48k_384k", 1, false, 12, 1, 0, 0),
    ("l1_dual_32k_256k", 1, false, 8, 2, 2, 0),
    ("l1_joint4_48k_192k", 1, false, 6, 1, 1, 0),
    ("l1_joint8_44k_320k", 1, false, 10, 0, 1, 1),
    ("l1_joint12_32k_128k", 1, false, 4, 2, 1, 2),
    ("l1_joint16_48k_448k", 1, false, 14, 1, 1, 3),
    ("l1_lsf_stereo_24k_128k", 1, true, 8, 1, 0, 0),
    ("l1_lsf_joint_16k_64k", 1, true, 4, 2, 1, 1),
    ("l2_joint4_48k_192k", 2, false, 10, 1, 1, 0),
    ("l2_joint8_44k_256k", 2, false, 12, 0, 1, 1),
    ("l2_joint12_48k_64k", 2, false, 4, 1, 1, 2),
    ("l2_joint16_32k_96k", 2, false, 6, 2, 1, 3),
    ("l2_dual_44k_160k", 2, false, 9, 0, 2, 0),
    ("l2_dual_32k_384k", 2, false, 14, 2, 2, 0),
    ("l2_lsf_joint_22k_96k", 2, true, 10, 0, 1, 1),
    ("l2_lsf_mono_24k_160k", 2, true, 14, 1, 3, 0),
];

#[test]
fn synthetic_streams_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    for (k, &(name, layer, lsf, br, sr, mode, ext)) in SYNTHETIC.iter().enumerate() {
        let out = filmcraft_testkit::fixtures_dir("mpa").join(format!("{name}.mp{layer}"));
        let f = filmcraft_testkit::fixtures::generate(&out, |tmp| {
            let mut rng = Rng(0x9E37_79B9 ^ k as u32);
            let h = header(lsf, layer, br, sr, mode, ext);
            let data: Vec<u8> = (0..40).flat_map(|_| if layer == 1 { random_layer1(&mut rng, h) } else { random_layer2(&mut rng, h) }).collect();
            std::fs::write(tmp, data).is_ok()
        })
        .unwrap();
        let h = parse_header(&std::fs::read(&f).unwrap()).unwrap();
        let snr = snr_vs_ffmpeg(&ff, &f, h.sample_rate, h.channels());
        println!("{name}: SNR vs ffmpeg {snr:.1} dB");
        assert!(snr >= MIN_SNR, "{name}: SNR {snr:.1} dB < {MIN_SNR}");
    }
}

/// Damaged frames (bit flips, truncation, scrambled headers) give errors or noise, never a panic.
#[test]
fn damaged_frames_never_panic() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let mut rng = Rng(0x00C0_FFEE);
    let mut streams: Vec<Vec<u8>> = ENCODED.iter().filter(|s| !s.twolame).take(5).map(|s| std::fs::read(make_encoded(&ff, s).unwrap()).unwrap()).collect();
    let mut synth = Rng(7);
    streams.push((0..8).flat_map(|_| random_layer1(&mut synth, header(false, 1, 12, 1, 1, 1))).collect());
    for data in &streams {
        let mut dec = Decoder::new();
        for fr in frames(data).into_iter().take(8) {
            for k in 0..300 {
                let mut m = fr.to_vec();
                match k % 3 {
                    0 => {
                        for _ in 0..1 + rng.below(8) {
                            let bit = rng.below(m.len() as u32 * 8) as usize;
                            m[bit / 8] ^= 0x80 >> (bit % 8);
                        }
                    }
                    1 => m.truncate(rng.below(m.len() as u32) as usize),
                    _ => {
                        // keep the sync word; scramble version, layer, rates and mode
                        m[1] ^= rng.next() as u8 & 0x1F;
                        m[2] ^= rng.next() as u8;
                        m[3] ^= rng.next() as u8;
                    }
                }
                if let Ok(d) = dec.decode(&m) {
                    assert!(d.channels.iter().flatten().all(|s| s.is_finite()));
                }
            }
        }
    }
}

#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("mpa fixtures") else { return };
    for s in ENCODED {
        let out = filmcraft_testkit::fixtures_dir("mpa").join(format!("{}.mp2", s.name));
        filmcraft_testkit::fixtures::generate_and_report(&format!("mpa/{}", s.name), &[out], || make_encoded(&ff, s));
    }
}
