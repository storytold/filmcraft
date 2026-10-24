//! RFC 6716 / RFC 8251 conformance vectors.
//!
//! The official vectors (opus_testvectors-rfc8251) are test data downloaded to
//! `target/fixtures/opus/vectors/` (never committed). The test is skipped when they are absent.
//! For each vector we check (1) the final range-coder state after every packet against the value
//! stored in the `.bit` file (bit-exact parsing), and (2) the decoded PCM against the reference
//! `.dec` output using our implementation of the opus_compare-style quality metric
//! (see `common::opus_quality`), for stereo (`.dec`) and mono (`m.dec`) output at 48 kHz.

use crate::common;

use std::path::PathBuf;

use filmcraft_opus::StreamDecoder;

fn vectors_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/opus/vectors");
    d.join("testvector01.bit").exists().then_some(d)
}

fn read_bit(path: &PathBuf) -> Vec<(Vec<u8>, u32)> {
    let data = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= data.len() {
        let len = u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        let rng = u32::from_be_bytes(data[p + 4..p + 8].try_into().unwrap());
        p += 8;
        if p + len > data.len() {
            break;
        }
        out.push((data[p..p + len].to_vec(), rng));
        p += len;
    }
    out
}

fn read_pcm(path: &PathBuf) -> Vec<f32> {
    std::fs::read(path).unwrap().as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect()
}

/// Decodes a vector; returns (interleaved output, index of first range mismatch).
fn decode_vector(packets: &[(Vec<u8>, u32)], rate: u32, channels: usize, no_inv: bool) -> (Vec<f32>, Option<usize>) {
    let mut dec = StreamDecoder::new(rate, channels).unwrap();
    if no_inv {
        dec.set_phase_inversion_disabled(true);
    }
    let mut out = Vec::new();
    let mut mismatch = None;
    for (i, (pkt, rng)) in packets.iter().enumerate() {
        let r = if pkt.is_empty() { dec.decode_interleaved(None, &mut out) } else { dec.decode_interleaved(Some(pkt), &mut out) };
        if r.is_err() {
            mismatch.get_or_insert(i);
            continue;
        }
        if !pkt.is_empty() && dec.final_range() != *rng {
            mismatch.get_or_insert(i);
        }
    }
    (out, mismatch)
}

#[test]
fn rfc8251_test_vectors() {
    let Some(dir) = vectors_dir() else {
        eprintln!("opus test vectors not found; skipping");
        return;
    };
    let mut failures = Vec::new();
    for v in 1..=12 {
        let name = format!("testvector{v:02}");
        let packets = read_bit(&dir.join(format!("{name}.bit")));
        for (channels, suffix) in [(2usize, ""), (2usize, "m"), (1usize, "m")] {
            let (out, mismatch) = decode_vector(&packets, 48000, channels, suffix == "m");
            let mut reference = read_pcm(&dir.join(format!("{name}{suffix}.dec")));
            if channels == 1 {
                // Mono output is compared against the downmix of the phase-inversion-free reference.
                reference = reference.as_chunks::<2>().0.iter().map(|c| 0.5 * (c[0] + c[1])).collect();
            }
            let q = common::opus_quality(&reference, &out, channels, 48000);
            let snr = common::snr_db(&reference, &out);
            let pass = mismatch.is_none() && q >= 0.0;
            eprintln!(
                "{name} {}: range {} quality {q:6.1} snr {snr:6.1} dB  len {} / ref {} -> {}",
                match (channels, suffix) {
                    (2, "") => "stereo  ",
                    (2, _) => "stereo-m",
                    _ => "mono    ",
                },
                mismatch.map_or("ok".to_string(), |i| format!("MISMATCH@{i}")),
                out.len(),
                reference.len(),
                if pass { "PASS" } else { "FAIL" }
            );
            if !pass {
                failures.push(format!("{name}{suffix} ({channels} ch)"));
            }
        }
    }
    assert!(failures.is_empty(), "failing vectors: {failures:?}");
}

/// Sanity check of the quality metric calibration: additive white noise at 48 dB SNR sits at the
/// pass threshold (quality ~0), at 70 dB it is clearly above it.
#[test]
fn quality_metric_calibration() {
    let Some(dir) = vectors_dir() else {
        return;
    };
    let reference = read_pcm(&dir.join("testvector01.dec"));
    let reference = &reference[..reference.len().min(48000 * 2 * 20)];
    let p: f64 = reference.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / reference.len() as f64;
    for (snr, lo, hi) in [(48.0f64, -15.0, 15.0), (70.0, 60.0, 101.0)] {
        let amp = (p / 10f64.powf(snr / 10.0) * 3.0).sqrt();
        let mut seed = 1u32;
        let noisy: Vec<f32> = reference
            .iter()
            .map(|v| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                v + (((seed >> 8) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) * amp) as f32
            })
            .collect();
        let q = common::opus_quality(reference, &noisy, 2, 48000);
        eprintln!("white noise at {snr} dB SNR: quality {q:.1}");
        assert!(q > lo && q < hi, "{snr} dB -> {q}");
    }
}

/// Diagnostic (`VEC=n cargo test ... -- --ignored`): per-second SNR/quality with packet modes.
#[test]
#[ignore]
fn vector_diag() {
    let Some(dir) = vectors_dir() else {
        return;
    };
    let v: usize = std::env::var("VEC").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let name = format!("testvector{v:02}");
    let packets = read_bit(&dir.join(format!("{name}.bit")));
    let mut dec = StreamDecoder::new(48000, 2).unwrap();
    let mut out = Vec::new();
    let mut lines = Vec::new();
    for (pkt, _) in &packets {
        let start = out.len() / 2;
        if pkt.is_empty() {
            dec.decode_interleaved(None, &mut out).unwrap();
            lines.push((start, "LOST".to_string()));
        } else {
            let p = filmcraft_opus::Packet::parse(pkt).unwrap();
            dec.decode_interleaved(Some(pkt), &mut out).unwrap();
            lines.push((start, format!("{:?} {:?} st{} n{}", p.toc.mode, p.toc.bandwidth, p.toc.stereo as u8, p.frames.len())));
        }
    }
    let reference = read_pcm(&dir.join(format!("{name}.dec")));
    let sec = 48000 * 2;
    for s in 0..reference.len() / sec {
        let r = &reference[s * sec..(s + 1) * sec];
        let o = &out[s * sec..(s + 1) * sec];
        let mut kinds: Vec<&String> = lines.iter().filter(|(st, _)| *st >= s * 48000 && *st < (s + 1) * 48000).map(|(_, c)| c).collect();
        kinds.dedup();
        eprintln!("{s:3}s snr {:6.1} q {:6.1} {:?}", common::snr_db(r, o), common::opus_quality(r, o, 2, 48000), kinds);
    }
}
