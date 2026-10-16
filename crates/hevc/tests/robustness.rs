//! hvcC / length-prefixed input and resilience against damaged streams.

mod common;

use filmcraft_bitstream::annexb_nals;
use filmcraft_hevc::Decoder;

/// Build an hvcC record and length-prefixed samples from an Annex-B stream.
fn to_hvcc(data: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut sets: [Vec<Vec<u8>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut samples = Vec::new();
    for au in common::split_access_units(data) {
        let mut sample = Vec::new();
        for nal in annexb_nals(au) {
            let t = (nal[0] >> 1) & 0x3f;
            match t {
                32..=34 => sets[(t - 32) as usize].push(nal.to_vec()),
                _ => {
                    sample.extend((nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }
        if !sample.is_empty() {
            samples.push(sample);
        }
    }
    for s in sets.iter_mut() {
        s.dedup();
    }
    let mut hvcc = vec![1u8];
    hvcc.extend([0u8; 20]); // profile / level / misc fields (not used by the decoder)
    hvcc.push(0xfc | 3); // lengthSizeMinusOne = 3
    hvcc.push(3);
    for (k, s) in sets.iter().enumerate() {
        hvcc.push(0x80 | (32 + k as u8));
        hvcc.extend((s.len() as u16).to_be_bytes());
        for n in s {
            hvcc.extend((n.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(n);
        }
    }
    (hvcc, samples)
}

#[test]
fn hvcc_input_matches_reference() {
    for name in ["medium", "intra_main10"] {
        let f = common::fixture(name);
        let Some((hevc, yuv)) = common::ensure(f) else { return };
        let (hvcc, samples) = to_hvcc(&std::fs::read(hevc).unwrap());
        let mut dec = Decoder::from_hvcc(&hvcc).unwrap();
        assert_eq!(dec.nal_length_size(), Some(4));
        let mut pics = Vec::new();
        for (i, s) in samples.iter().enumerate() {
            pics.extend(dec.decode(s, i as i64 * 1000).unwrap());
        }
        pics.extend(dec.flush());
        let reference = std::fs::read(yuv).unwrap();
        common::compare(&pics, &reference, f.width as usize, f.height as usize, f.bit_depth).unwrap();
        let mut pts: Vec<i64> = pics.iter().map(|p| p.pts).collect();
        pts.sort();
        assert_eq!(pts, (0..samples.len() as i64).map(|i| i * 1000).collect::<Vec<_>>());
        let p0 = &pics[0];
        assert!(p0.key);
        assert_eq!(p0.bit_depth, f.bit_depth);
        assert_eq!((p0.y_stride, p0.uv_stride), (f.width as usize, f.width as usize / 2));
    }
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Damaged streams must never panic or hang; errors are fine.
#[test]
fn corrupted_streams_do_not_panic() {
    for name in ["medium", "bframes", "intra_main10", "rect_amp", "weightp", "no_wpp", "slices4"] {
        let f = common::fixture(name);
        let Some((hevc, _)) = common::ensure(f) else { return };
        let data = std::fs::read(hevc).unwrap();
        let mut seed = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        for round in 0..12 {
            let mut d = data.clone();
            for _ in 0..(1 + round * 3) {
                let pos = 100 + (xorshift(&mut seed) as usize) % (d.len() - 100);
                d[pos] ^= 1 << (xorshift(&mut seed) % 8);
            }
            if round % 4 == 3 {
                d.truncate(d.len() * 2 / 3);
            }
            for (threads, intra) in [(1, None), (2, Some(true)), (4, None)] {
                let mut dec = Decoder::with_threads(threads);
                dec.set_intra_picture_parallel(intra);
                for au in common::split_access_units(&d) {
                    let _ = dec.decode(au, 0);
                }
                let _ = dec.flush();
                let _ = dec.take_error();
            }
        }
    }
}

/// Damaged slice segment headers (entry point offsets, slice addresses, flags) must never panic
/// or hang, also when pictures are split over threads by their entry points.
#[test]
fn corrupted_slice_headers_do_not_panic() {
    for name in ["medium", "slices4", "no_wpp"] {
        let f = common::fixture(name);
        let Some((hevc, _)) = common::ensure(f) else { return };
        let data = std::fs::read(hevc).unwrap();
        // payload starts of the slice NAL units
        let slices: Vec<usize> = (0..data.len().saturating_sub(4))
            .filter(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && (data[i + 3] >> 1) & 0x3f < 32)
            .map(|i| i + 3)
            .collect();
        let mut seed = 0x0bad_5eed_u64 ^ name.len() as u64;
        for _ in 0..16 {
            let mut d = data.clone();
            for _ in 0..6 {
                let s = slices[(xorshift(&mut seed) as usize) % slices.len()];
                // the slice header with its entry points lies in the first bytes
                let pos = s + 2 + (xorshift(&mut seed) as usize) % 24;
                if let Some(b) = d.get_mut(pos) {
                    *b ^= 1 << (xorshift(&mut seed) % 8);
                }
            }
            for (threads, intra) in [(2, Some(true)), (4, Some(true)), (8, None)] {
                let mut dec = Decoder::with_threads(threads);
                dec.set_intra_picture_parallel(intra);
                for au in common::split_access_units(&d) {
                    let _ = dec.decode(au, 0);
                }
                let _ = dec.flush();
                let _ = dec.take_error();
            }
        }
    }
}

#[test]
fn garbage_input_is_rejected_gracefully() {
    let mut seed = 42u64;
    for len in [0usize, 1, 5, 100, 5000] {
        for _ in 0..20 {
            let mut g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
            if len > 10 {
                g[..4].copy_from_slice(&[0, 0, 0, 1]);
            }
            let mut dec = Decoder::new();
            let _ = dec.decode(&g, 0);
            let _ = dec.flush();
        }
    }
    assert!(Decoder::from_hvcc(&[1, 2, 3]).is_err());
}
