//! Golden hashes of the encoder output for a fixed set of synthetic signals.
//!
//! Encoder speed-ups must not change a single output byte, so this pins the encoded frames and the
//! final STREAMINFO (which holds the MD5 of the input) of 64 deterministic cases: noise, sines, silence,
//! full-scale squares, wasted bits and a music-like mix, at 8/12/16/24/32 bits, 1 to 4 channels,
//! several block sizes (with a short final frame), and levels 0 to 8.
//!
//! The hashes were captured from the encoder on upstream/main (68919ed8) before the per-order
//! predictor kernels and merged partition sums were added. Signals use integer arithmetic only. The
//! encoder itself uses `f64` (window, autocorrelation), so a platform whose libm rounds `cos`
//! differently could in principle differ; the hashes were made on x86-64 Linux.
//!
//! An intentional change of the output (for example a different LPC order choice) regenerates the
//! table with `cargo test -p filmcraft-flac --test golden_output -- --ignored --nocapture print`.

use filmcraft_flac::{Encoder, EncoderConfig};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// FNV-1a, 64 bit.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A sine-like wave from an integer resonator: `y[n] = 2cos(w) y[n-1] - y[n-2]` in Q14, amplitude `amp`.
fn resonator(n: usize, coef_q14: i64, amp: i64) -> Vec<i64> {
    let (mut y1, mut y2) = (amp, amp * 9 / 10);
    (0..n)
        .map(|_| {
            let y = ((coef_q14 * y1) >> 14) - y2;
            y2 = y1;
            y1 = y.clamp(-amp, amp);
            y1
        })
        .collect()
}

/// One channel of `kind` at `bps` bits, values always inside the sample range.
fn signal(kind: &str, n: usize, bps: u8, seed: u64) -> Vec<i32> {
    let max = (1i64 << (bps - 1)) - 1;
    let mut r = Rng(seed | 1);
    let sine = resonator(n, 32_000 - (seed as i64 % 7) * 900, max / 2);
    let sine2 = resonator(n, 27_000, max / 4);
    (0..n)
        .map(|i| {
            let noise = |r: &mut Rng, amp: i64| (r.next() >> 11) as i64 % (2 * amp + 1) - amp;
            let v = match kind {
                "noise" => noise(&mut r, max),
                "sine" => sine[i],
                "music" => sine[i] + sine2[i] + noise(&mut r, max / 64),
                "square" => {
                    if (i / 37) % 2 == 0 {
                        max
                    } else {
                        -max - 1
                    }
                }
                // low 3 bits always zero (wasted bits) on a quiet sine
                "wasted" => (sine[i] / 8) * 8,
                "click" => {
                    if i == n / 2 {
                        max
                    } else {
                        0
                    }
                }
                // a silent stretch between two noisy ones
                "gap" => {
                    if (n / 3..2 * n / 3).contains(&i) {
                        0
                    } else {
                        noise(&mut r, max / 3)
                    }
                }
                _ => 0,
            };
            v.clamp(-max - 1, max) as i32
        })
        .collect()
}

struct Case {
    kind: &'static str,
    bps: u8,
    channels: u8,
    block: u16,
    level: u8,
    samples: usize,
    chunk: usize,
}

impl Case {
    fn name(&self) -> String {
        format!("{}-{}b-{}ch-bs{}-L{}-n{}-c{}", self.kind, self.bps, self.channels, self.block, self.level, self.samples, self.chunk)
    }

    /// Hash of every frame and the final STREAMINFO (the file header carries the crate version).
    fn hash(&self) -> u64 {
        let planar: Vec<Vec<i32>> = (0..self.channels).map(|c| signal(self.kind, self.samples, self.bps, 0x9E37_79B9 + u64::from(c) * 7919)).collect();
        let mut cfg = EncoderConfig::new(44_100, self.channels, self.bps);
        cfg.block_size = self.block;
        cfg.level = self.level;
        let mut enc = Encoder::new(cfg).expect("valid config");
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.samples {
            let end = (at + self.chunk).min(self.samples);
            let parts: Vec<&[i32]> = planar.iter().map(|c| &c[at..end]).collect();
            out.extend(enc.encode(&parts).expect("samples in range"));
            at = end;
        }
        out.extend(enc.finish());
        out.extend_from_slice(&enc.streaminfo());
        fnv(&out)
    }
}

fn cases() -> Vec<Case> {
    const KINDS: [&str; 8] = ["noise", "sine", "music", "square", "wasted", "click", "gap", "silence"];
    const BPS: [u8; 5] = [8, 12, 16, 24, 32];
    const BLOCKS: [u16; 5] = [16, 576, 1000, 4096, 4608];
    let mut v = Vec::new();
    // every kind at every bit depth; channels, block size and level rotate through their ranges
    for (k, kind) in KINDS.iter().enumerate() {
        for (b, &bps) in BPS.iter().enumerate() {
            let block = BLOCKS[(k + 2 * b) % BLOCKS.len()];
            let samples = 2 * usize::from(block) + 37 + (k * 101 + b * 13) % 700;
            v.push(Case {
                kind,
                bps,
                channels: 1 + ((k + b) % 4) as u8,
                block,
                level: ((k * 5 + b * 3) % 9) as u8,
                samples,
                chunk: 1 + (k * 977 + b * 311) % 5000,
            });
        }
    }
    // all levels on stereo music at 16 and 24 bits, and the 4096 blocks real files use
    for level in 0..=8 {
        for bps in [16u8, 24] {
            v.push(Case { kind: "music", bps, channels: 2, block: 4096, level, samples: 9000, chunk: 4096 });
        }
    }
    // 4 channels, short blocks and a block that is not a power of two
    for level in [0u8, 5, 8] {
        v.push(Case { kind: "noise", bps: 16, channels: 4, block: 1000, level, samples: 2500, chunk: 777 });
        v.push(Case { kind: "gap", bps: 16, channels: 2, block: 576, level, samples: 2000, chunk: 2000 });
    }
    v
}

/// (case name, hash), captured on upstream/main 68919ed8.
const GOLDEN: &[(&str, u64)] = &[
    ("noise-8b-1ch-bs16-L0-n69-c1", 0x69cba522cd83247a),
    ("noise-12b-2ch-bs1000-L3-n2050-c312", 0xbd8aa93b40b5b078),
    ("noise-16b-3ch-bs4608-L6-n9279-c623", 0xe1969205fcfb7d74),
    ("noise-24b-4ch-bs576-L0-n1228-c934", 0xd444404f16d70169),
    ("noise-32b-1ch-bs4096-L3-n8281-c1245", 0x45ca92f380216922),
    ("sine-8b-2ch-bs576-L5-n1290-c978", 0x135ff72f3b74e284),
    ("sine-12b-3ch-bs4096-L8-n8343-c1289", 0x95c4d0720e59d826),
    ("sine-16b-4ch-bs16-L2-n196-c1600", 0x84bf0b50ef111b77),
    ("sine-24b-1ch-bs1000-L5-n2177-c1911", 0xf4cb2b792c100e1c),
    ("sine-32b-2ch-bs4608-L8-n9406-c2222", 0x45aaf5d6091ecc85),
    ("music-8b-3ch-bs1000-L1-n2239-c1955", 0x3540af737b613e03),
    ("music-12b-4ch-bs4608-L4-n9468-c2266", 0x07d8c76385536b70),
    ("music-16b-1ch-bs576-L7-n1417-c2577", 0x1b5300d81d3a8eac),
    ("music-24b-2ch-bs4096-L1-n8470-c2888", 0x570f7821161e7932),
    ("music-32b-3ch-bs16-L4-n323-c3199", 0xb720fd6b9fb8d7c9),
    ("square-8b-4ch-bs4096-L6-n8532-c2932", 0xf1e702077432c346),
    ("square-12b-1ch-bs16-L0-n385-c3243", 0x2820a440a7465bb2),
    ("square-16b-2ch-bs1000-L3-n2366-c3554", 0x827ee17d45c447bc),
    ("square-24b-3ch-bs4608-L6-n9595-c3865", 0x88b6af280896f131),
    ("square-32b-4ch-bs576-L0-n1544-c4176", 0x8a72918d0834a2f1),
    ("wasted-8b-1ch-bs4608-L2-n9657-c3909", 0x015dafe8aa5fad3e),
    ("wasted-12b-2ch-bs576-L5-n1606-c4220", 0x2368a40a9423a203),
    ("wasted-16b-3ch-bs4096-L8-n8659-c4531", 0x62f96c904897cb7f),
    ("wasted-24b-4ch-bs16-L2-n512-c4842", 0x8c26e76e32aa90ca),
    ("wasted-32b-1ch-bs1000-L5-n2493-c153", 0x39924ae6db0d72ee),
    ("click-8b-2ch-bs16-L7-n574-c4886", 0x54b956cd16590b80),
    ("click-12b-3ch-bs1000-L1-n2555-c197", 0x5b5c00ba683e16fb),
    ("click-16b-4ch-bs4608-L4-n9784-c508", 0x906ae4960be0eb16),
    ("click-24b-1ch-bs576-L7-n1733-c819", 0x815a9e001b296ae1),
    ("click-32b-2ch-bs4096-L1-n8786-c1130", 0xfe751b5587691520),
    ("gap-8b-3ch-bs576-L3-n1795-c863", 0xa372ac31d477b4de),
    ("gap-12b-4ch-bs4096-L6-n8848-c1174", 0x0f98e2cbcff69b73),
    ("gap-16b-1ch-bs16-L0-n701-c1485", 0xf0de17bb815bc8eb),
    ("gap-24b-2ch-bs1000-L3-n2682-c1796", 0x09a7b31c70740cc1),
    ("gap-32b-3ch-bs4608-L6-n9911-c2107", 0x8ba6d94ee8c85b0c),
    ("silence-8b-4ch-bs1000-L8-n2044-c1840", 0x845724c286fa7300),
    ("silence-12b-1ch-bs4608-L2-n9273-c2151", 0xe09802e7a78bf137),
    ("silence-16b-2ch-bs576-L5-n1222-c2462", 0x9bfd29a65f5d4a8c),
    ("silence-24b-3ch-bs4096-L8-n8275-c2773", 0xfcf34cb0553f5b39),
    ("silence-32b-4ch-bs16-L2-n128-c3084", 0xc4c8f1f5d73208e0),
    ("music-16b-2ch-bs4096-L0-n9000-c4096", 0x7ee21e0646af3021),
    ("music-24b-2ch-bs4096-L0-n9000-c4096", 0x384bb9626dd0eeb3),
    ("music-16b-2ch-bs4096-L1-n9000-c4096", 0x7ee21e0646af3021),
    ("music-24b-2ch-bs4096-L1-n9000-c4096", 0x384bb9626dd0eeb3),
    ("music-16b-2ch-bs4096-L2-n9000-c4096", 0x7ee21e0646af3021),
    ("music-24b-2ch-bs4096-L2-n9000-c4096", 0x384bb9626dd0eeb3),
    ("music-16b-2ch-bs4096-L3-n9000-c4096", 0xf948f408d6ea5f90),
    ("music-24b-2ch-bs4096-L3-n9000-c4096", 0xc448a8514df4434d),
    ("music-16b-2ch-bs4096-L4-n9000-c4096", 0xd6524def929fdb8e),
    ("music-24b-2ch-bs4096-L4-n9000-c4096", 0xf8f9ddf3036880be),
    ("music-16b-2ch-bs4096-L5-n9000-c4096", 0xd6524def929fdb8e),
    ("music-24b-2ch-bs4096-L5-n9000-c4096", 0xf8f9ddf3036880be),
    ("music-16b-2ch-bs4096-L6-n9000-c4096", 0xd6524def929fdb8e),
    ("music-24b-2ch-bs4096-L6-n9000-c4096", 0xf8f9ddf3036880be),
    ("music-16b-2ch-bs4096-L7-n9000-c4096", 0x5be70a6777d3303e),
    ("music-24b-2ch-bs4096-L7-n9000-c4096", 0x70773b947683c189),
    ("music-16b-2ch-bs4096-L8-n9000-c4096", 0x5be70a6777d3303e),
    ("music-24b-2ch-bs4096-L8-n9000-c4096", 0x70773b947683c189),
    ("noise-16b-4ch-bs1000-L0-n2500-c777", 0x5d0bbda30208b8e9),
    ("gap-16b-2ch-bs576-L0-n2000-c2000", 0x294b47f7663f3861),
    ("noise-16b-4ch-bs1000-L5-n2500-c777", 0x5d0bbda30208b8e9),
    ("gap-16b-2ch-bs576-L5-n2000-c2000", 0xb8a5a6d18a43f0da),
    ("noise-16b-4ch-bs1000-L8-n2500-c777", 0x5d0bbda30208b8e9),
    ("gap-16b-2ch-bs576-L8-n2000-c2000", 0xb8a5a6d18a43f0da),
];

#[test]
fn encoder_output_matches_golden_hashes() {
    let all = cases();
    assert_eq!(all.len(), GOLDEN.len(), "golden table out of date: regenerate it");
    for (case, &(name, want)) in all.iter().zip(GOLDEN) {
        assert_eq!(case.name(), name, "case list changed");
        assert_eq!(case.hash(), want, "output changed for {name}");
    }
}

#[test]
#[ignore = "prints the golden table"]
fn print() {
    for c in cases() {
        println!("    (\"{}\", {:#018x}),", c.name(), c.hash());
    }
}
