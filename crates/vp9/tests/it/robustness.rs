//! Damaged, truncated and random input must never panic or hang (errors are fine).

use crate::common;

use filmcraft_vp9::Decoder;
use std::time::{Duration, Instant};

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn decode_all(frames: &[Vec<u8>], threads: usize) {
    let mut dec = Decoder::with_threads(threads);
    for (i, f) in frames.iter().enumerate() {
        let _ = dec.decode(f, i as i64);
    }
    let _ = dec.flush();
}

#[test]
fn corrupted_streams_do_not_panic() {
    for name in ["default_good", "altref_hidden", "profile2_10bit", "tiles_cols4", "aq_segmentation", "lossless", "profile1_444", "tiles_rows4"] {
        let f = common::fixture(name);
        let Some((ivf, _)) = common::ensure(f) else { return };
        let data = std::fs::read(ivf).unwrap();
        let frames: Vec<Vec<u8>> = common::ivf_frames(&data).into_iter().map(|f| f.to_vec()).collect();
        let mut seed = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        for round in 0..10 {
            let t0 = Instant::now();
            let mut d = frames.clone();
            for _ in 0..(1 + round * 4) {
                let fi = (xorshift(&mut seed) as usize) % d.len();
                let fr = &mut d[fi];
                if fr.is_empty() {
                    continue;
                }
                let pos = (xorshift(&mut seed) as usize) % fr.len();
                match round % 3 {
                    0 => fr[pos] ^= 1 << (xorshift(&mut seed) % 8),
                    1 => fr[pos] = xorshift(&mut seed) as u8,
                    _ => fr.truncate(pos),
                }
            }
            for threads in [1, 4] {
                decode_all(&d, threads);
            }
            assert!(t0.elapsed() < Duration::from_secs(60), "{name} round {round} too slow");
        }
    }
}

#[test]
fn truncated_frames_do_not_panic() {
    let f = common::fixture("default_good");
    let Some((ivf, _)) = common::ensure(f) else { return };
    let data = std::fs::read(ivf).unwrap();
    let frames: Vec<Vec<u8>> = common::ivf_frames(&data).into_iter().map(|f| f.to_vec()).collect();
    for cut in [1usize, 2, 3, 5, 8, 13, 21, 40, 100, 400] {
        let d: Vec<Vec<u8>> = frames.iter().map(|f| f[..cut.min(f.len())].to_vec()).collect();
        decode_all(&d, 1);
        decode_all(&d, 3);
    }
}

#[test]
fn garbage_input_is_rejected_gracefully() {
    let mut seed = 42u64;
    for len in [0usize, 1, 2, 5, 30, 100, 5000] {
        for k in 0..40 {
            let mut g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
            if len > 10 && k % 2 == 0 {
                // A plausible key frame start: frame marker, profile 0, key frame, sync code.
                g[0] = 0x82;
                g[1] = 0x49;
                g[2] = 0x83;
                g[3] = 0x42;
            }
            let mut dec = Decoder::with_threads(1 + k % 3);
            let _ = dec.decode(&g, 0);
            let _ = dec.decode(&g, 1);
        }
    }
    // Inter frame / show_existing without references.
    let mut dec = Decoder::new();
    assert!(dec.decode(&[0x88], 0).is_err());
    assert!(dec.decode(&[0x86, 0, 0, 0, 0], 0).is_err());
}

/// Huge declared frame sizes must not allocate absurd amounts or panic.
#[test]
fn oversized_frame_is_rejected() {
    // Key frame header: marker 2, profile 0, show_existing 0, key, show, no error res,
    // sync, color (4 bits), width-1 = 0xffff, height-1 = 0xffff.
    let bits = "10".to_string() + "00" + "0" + "0" + "1" + "0" + "010010011000001101000010" + "0000" + &"1".repeat(32);
    let mut v = vec![0u8; bits.len().div_ceil(8) + 8];
    for (i, c) in bits.chars().enumerate() {
        if c == '1' {
            v[i / 8] |= 0x80 >> (i % 8);
        }
    }
    let mut dec = Decoder::new();
    assert!(dec.decode(&v, 0).is_err());
}
