//! Safe, vectorisable chroma copies for CoreVideo's biplanar surfaces.
//!
//! Each destination gets an exact-size iterator, so Vec reserves once per row and LLVM can
//! vectorise the strided loads. Alternating `push` into two vectors checks capacity per sample
//! and prevents that transformation. No architecture-specific intrinsics or extra threads.

pub(crate) fn append_u8(pairs: &[[u8; 2]], u: &mut Vec<u8>, v: &mut Vec<u8>) {
    u.extend(pairs.iter().map(|p| p[0]));
    v.extend(pairs.iter().map(|p| p[1]));
}

/// CoreVideo stores 10-bit samples in the high bits of native-endian 16-bit words.
pub(crate) fn append_u10(quads: &[[u8; 4]], u: &mut Vec<u16>, v: &mut Vec<u16>) {
    u.extend(quads.iter().map(|q| u16::from_ne_bytes([q[0], q[1]]) >> 6));
    v.extend(quads.iter().map(|q| u16::from_ne_bytes([q[2], q[3]]) >> 6));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_u8(pairs: &[[u8; 2]], u: &mut Vec<u8>, v: &mut Vec<u8>) {
        for p in pairs {
            u.push(p[0]);
            v.push(p[1]);
        }
    }

    fn scalar_u10(quads: &[[u8; 4]], u: &mut Vec<u16>, v: &mut Vec<u16>) {
        for q in quads {
            u.push(u16::from_ne_bytes([q[0], q[1]]) >> 6);
            v.push(u16::from_ne_bytes([q[2], q[3]]) >> 6);
        }
    }

    #[test]
    fn copies_match_scalar_with_tails_offsets_and_existing_samples() {
        // Empty, short, odd, vector boundaries, and real frame widths. Prefixes exercise appending
        // rows and growing an undersized pooled vector; byte offsets exercise unaligned input.
        for n in [0, 1, 2, 7, 8, 15, 16, 17, 31, 32, 33, 959, 960, 1920, 4096] {
            for offset in 0..16 {
                let bytes: Vec<u8> = (0..offset + n * 4).map(|i| (i * 131 + i / 7) as u8).collect();
                let pairs = bytes[offset..offset + n * 2].as_chunks::<2>().0;
                let (mut u, mut v) = (vec![19], vec![71]);
                let (mut ru, mut rv) = (u.clone(), v.clone());
                append_u8(pairs, &mut u, &mut v);
                scalar_u8(pairs, &mut ru, &mut rv);
                assert_eq!((u, v), (ru, rv), "8-bit width {n}, offset {offset}");

                let quads = bytes[offset..].as_chunks::<4>().0;
                let (mut u, mut v) = (vec![1023], vec![0]);
                let (mut ru, mut rv) = (u.clone(), v.clone());
                append_u10(quads, &mut u, &mut v);
                scalar_u10(quads, &mut ru, &mut rv);
                assert_eq!((u, v), (ru, rv), "10-bit width {n}, offset {offset}");
            }
        }
    }

    #[test]
    fn ten_bit_samples_ignore_low_padding_bits() {
        let quads: Vec<[u8; 4]> = (0..=1023u16)
            .map(|i| {
                let u = ((i << 6) | 63).to_ne_bytes();
                let v = (((1023 - i) << 6) | 31).to_ne_bytes();
                [u[0], u[1], v[0], v[1]]
            })
            .collect();
        let (mut u, mut v) = (Vec::new(), Vec::new());
        append_u10(&quads, &mut u, &mut v);
        assert_eq!(u, (0..=1023).collect::<Vec<u16>>());
        assert_eq!(v, (0..=1023).rev().collect::<Vec<u16>>());
    }

    #[test]
    #[ignore = "release-only chroma copy benchmark; run with --ignored --nocapture"]
    fn bench_chroma_copy() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        fn measure<T>(rows: usize, mut u: Vec<T>, mut v: Vec<T>, mut copy: impl FnMut(&mut Vec<T>, &mut Vec<T>)) -> Duration {
            let start = Instant::now();
            for _ in 0..30 {
                u.clear();
                v.clear();
                for _ in 0..rows {
                    copy(&mut u, &mut v);
                }
                black_box((&u, &v));
            }
            start.elapsed() / 30
        }

        for (width, rows) in [(1920usize, 540usize), (3840, 1080), (3840, 2160)] {
            let pairs: Vec<[u8; 2]> = (0..width / 2).map(|i| [i as u8, (i * 7) as u8]).collect();
            let quads: Vec<[u8; 4]> = pairs.iter().map(|p| [p[0], 255, p[1], 127]).collect();
            for ten in [false, true] {
                let mut times = [Vec::new(), Vec::new()];
                for round in 0..5 {
                    // Alternate order to reduce bias from heating and CPU frequency changes.
                    for index in [round % 2, 1 - round % 2] {
                        let n = width / 2 * rows;
                        let elapsed = if ten {
                            measure(rows, Vec::with_capacity(n), Vec::with_capacity(n), |u, v| {
                                if index == 0 { scalar_u10(black_box(&quads), u, v) } else { append_u10(black_box(&quads), u, v) }
                            })
                        } else {
                            measure(rows, Vec::with_capacity(n), Vec::with_capacity(n), |u, v| {
                                if index == 0 { scalar_u8(black_box(&pairs), u, v) } else { append_u8(black_box(&pairs), u, v) }
                            })
                        };
                        times[index].push(elapsed);
                    }
                }
                for t in &mut times {
                    t.sort();
                }
                let (before, after) = (times[0][2], times[1][2]);
                eprintln!(
                    "{width}px, {rows} chroma rows, {} bits: scalar {before:?}, vectorisable {after:?}, {:.2}x",
                    if ten { 10 } else { 8 },
                    before.as_secs_f64() / after.as_secs_f64()
                );
            }
        }
    }
}
