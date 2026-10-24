//! MPEG audio Layer I (MP1) in Matroska (`A_MPEG/L1`, #1402) against ffmpeg's decode. No
//! installed encoder writes Layer I, so the frames are synthetic (valid random Layer I frames,
//! as in `crates/mpa/tests/oracle.rs`) and ffmpeg only remuxes them into Matroska and decodes.

use crate::common;

use common::*;

struct Bits(Vec<u8>, usize);
impl Bits {
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.1.is_multiple_of(8) {
                self.0.push(0);
            }
            if (v >> i) & 1 == 1 {
                let last = self.0.len() - 1;
                self.0[last] |= 0x80 >> (self.1 % 8);
            }
            self.1 += 1;
        }
    }
}

/// Mono Layer I frames, MPEG-1, 48 kHz, 192 kbit/s (192-byte frames): allocations on the first 8
/// subbands only, so every frame fits.
fn layer1_stream(frames: usize) -> Vec<u8> {
    let mut seed = 0x2545_F491u32;
    let mut rnd = |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed % n
    };
    let mut out = Vec::new();
    for _ in 0..frames {
        let mut b = Bits(Vec::new(), 0);
        for byte in [0xFFu32, 0xFF, 6 << 4 | 1 << 2, 3 << 6] {
            b.put(byte, 8);
        }
        let alloc: Vec<u32> = (0..32).map(|sb| if sb < 8 { 1 + rnd(8) } else { 0 }).collect();
        for &a in &alloc {
            b.put(a, 4);
        }
        for &a in &alloc {
            if a != 0 {
                b.put(10 + rnd(30), 6);
            }
        }
        for _ in 0..12 {
            for &a in &alloc {
                if a != 0 {
                    b.put(rnd((1 << (a + 1)) - 1), a + 1);
                }
            }
        }
        let mut f = b.0;
        f.resize(192, 0);
        out.extend(f);
    }
    out
}

#[test]
fn mp1_in_matroska_matches_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let raw = dir().join("synthetic_layer1.mp1");
    std::fs::create_dir_all(dir()).unwrap();
    std::fs::write(&raw, layer1_stream(250)).unwrap();
    let Some(mkv) = fixture(&ff, "synthetic_layer1.mkv", &["-f", "mp3", "-i", raw.to_str().unwrap(), "-c:a", "copy"]) else {
        eprintln!("SKIPPED: this ffmpeg can't remux Layer I into Matroska");
        return;
    };
    let m = filmcraft_matroska::open(bytes(&mkv)).unwrap();
    assert!(matches!(m.tracks.first().map(|t| &t.codec), Some(filmcraft_matroska::Codec::Mp1)), "A_MPEG/L1 maps to Codec::Mp1");
    let src = filmcraft_codecs::open_bytes("synthetic_layer1.mkv", bytes(&mkv)).unwrap();
    let a = src.info().audio().cloned().unwrap();
    assert_eq!((a.sample_rate, a.channels), (48_000, 1));
    let want = ffmpeg_audio_f32(&ff, &mkv, &[]);
    let n = want.len().min(250 * 384) - 800;
    let got = src.audio(0, n, 48_000).unwrap();
    let peak = want.iter().fold(0f32, |m, x| m.max(x.abs()));
    // ffmpeg's CLI may or may not drop the decoder delay of a stream copy; search a few lags
    let (worst, lag) = (-2i64..=2)
        .map(|lag| ((2..n - 2).map(|i| (want[(i as i64 + lag) as usize] - got.channels[0][i]).abs()).fold(0f32, f32::max), lag))
        .fold((f32::INFINITY, 0), |m, x| if x.0 < m.0 { x } else { m });
    println!("mkv Layer I: max error {worst} at lag {lag}, peak {peak}");
    assert!(peak > 0.01 && worst <= 2e-4, "peak {peak}, max error {worst} at lag {lag}");
}
