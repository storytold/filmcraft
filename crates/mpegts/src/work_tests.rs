//! Equality and counting tests for the redundant-work removals: the old implementation is kept
//! here as the reference, the new one must give the same results.

use super::*;

/// The bitwise CRC-32 that `ts::crc32_mpeg` replaced.
fn crc32_bitwise(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

/// Deterministic xorshift bytes.
fn noise(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

#[test]
fn table_crc_equals_the_bitwise_crc() {
    // the standard check value of CRC-32/MPEG-2
    assert_eq!(ts::crc32_mpeg(b"123456789"), 0x0376_E6E7);
    assert_eq!(ts::crc32_mpeg(&[]), 0xFFFF_FFFF);
    for b in 0..=255u8 {
        assert_eq!(ts::crc32_mpeg(&[b]), crc32_bitwise(&[b]));
    }
    for n in [0usize, 1, 2, 3, 4, 5, 15, 16, 17, 183, 184, 1023, 1024, 4096] {
        for seed in 1..6u32 {
            let d = noise(seed * 7919 + n as u32, n);
            assert_eq!(ts::crc32_mpeg(&d), crc32_bitwise(&d), "len {n} seed {seed}");
        }
    }
    // a section followed by its own CRC checks to zero
    let mut sec = noise(5, 40);
    let crc = ts::crc32_mpeg(&sec);
    sec.extend(crc.to_be_bytes());
    assert_eq!(ts::crc32_mpeg(&sec), 0);
}

/// MPEG-1 layer II frame, 48 kHz 192 kb/s (576 bytes), distinct per `k` and `salt`.
fn mp2_frame(k: usize, salt: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFD, 0xA4, 0x00];
    v.extend((0..572).map(|i| (i as u8) ^ (k as u8) ^ salt));
    v
}

fn pes_packet(stream_id: u8, payload: &[u8]) -> Vec<u8> {
    let len = 3 + payload.len();
    let mut v = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x80, 0, 0];
    v.extend_from_slice(payload);
    v
}

/// Two audio streams interleaved in maximal PES packets over more than three window refills,
/// with a pack header per PES packet; MPEG-2 or MPEG-1 pack headers.
fn big_program_stream(mpeg1: bool, frames: usize) -> (Vec<u8>, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let a: Vec<Vec<u8>> = (0..frames).map(|k| mp2_frame(k, 0)).collect();
    let b: Vec<Vec<u8>> = (0..frames).map(|k| mp2_frame(k, 0x55)).collect();
    let (ea, eb) = (a.concat(), b.concat());
    let mut out = Vec::new();
    for (ca, cb) in ea.chunks(60_000).zip(eb.chunks(60_000)) {
        for (id, chunk) in [(0xC0, ca), (0xC1, cb)] {
            if mpeg1 {
                out.extend([0, 0, 1, 0xBA, 0x21, 0, 1, 0, 1, 0x80, 0, 1]);
            } else {
                out.extend([0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8]);
            }
            out.extend(pes_packet(id, chunk));
        }
    }
    out.extend([0, 0, 1, 0xB9]);
    (out, a, b)
}

#[test]
fn program_stream_with_large_pes_packets_gives_back_every_frame() {
    for mpeg1 in [false, true] {
        let (data, a, b) = big_program_stream(mpeg1, 2000); // about 1.2 MB per stream
        assert!(data.len() > 2 << 20, "more than the 1 MiB window twice");
        let f = open(&data).unwrap();
        assert_eq!(f.format, Format::Ps { mpeg1 });
        assert_eq!(f.streams.len(), 2);
        for (s, frames) in f.streams.iter().zip([&a, &b]) {
            assert_eq!(s.units.len(), frames.len());
            for (i, fr) in frames.iter().enumerate() {
                assert_eq!(&f.read_unit(&data, f.streams.iter().position(|x| std::ptr::eq(x, s)).unwrap(), i).unwrap(), fr, "mpeg1 {mpeg1} unit {i}");
            }
        }
        // cut inside the last packets: never panics, and the units before the cut are intact
        for cut in [data.len() - 1, data.len() - 4, data.len() - 30_000, data.len() - 61_000, 100_000, 1 << 20] {
            let d = &data[..cut];
            let Ok(f) = open(d) else { continue };
            for (si, frames) in [&a, &b].into_iter().enumerate() {
                let Some(s) = f.streams.get(si) else { continue };
                for i in 0..s.units.len().saturating_sub(1) {
                    assert_eq!(f.read_unit(d, si, i).unwrap(), frames[i], "cut {cut} stream {si} unit {i}");
                }
            }
        }
    }
}
