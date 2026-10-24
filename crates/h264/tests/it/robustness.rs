//! avcC / length-prefixed input and resilience against damaged streams.

use crate::common;

use filmcraft_bitstream::annexb_nals;
use filmcraft_h264::Decoder;

/// Build an avcC record and length-prefixed samples from an Annex-B stream.
fn to_avcc(data: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    let mut samples = Vec::new();
    for au in common::split_access_units(data) {
        let mut sample = Vec::new();
        for nal in annexb_nals(au) {
            match nal[0] & 0x1f {
                7 => sps.push(nal.to_vec()),
                8 => pps.push(nal.to_vec()),
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
    sps.dedup();
    pps.dedup();
    let s0 = &sps[0];
    let mut avcc = vec![1, s0[1], s0[2], s0[3], 0xff, 0xe0 | sps.len() as u8];
    for s in &sps {
        avcc.extend((s.len() as u16).to_be_bytes());
        avcc.extend_from_slice(s);
    }
    avcc.push(pps.len() as u8);
    for p in &pps {
        avcc.extend((p.len() as u16).to_be_bytes());
        avcc.extend_from_slice(p);
    }
    (avcc, samples)
}

#[test]
fn avcc_input_matches_reference() {
    for name in ["main_cabac_b", "baseline_qcif"] {
        let f = common::fixture(name);
        let Some((h264, yuv)) = common::ensure(f) else { return };
        let (avcc, samples) = to_avcc(&std::fs::read(h264).unwrap());
        let mut dec = Decoder::from_avcc(&avcc).unwrap();
        assert_eq!(dec.nal_length_size(), Some(4));
        let mut pics = Vec::new();
        for (i, s) in samples.iter().enumerate() {
            pics.extend(dec.decode(s, i as i64 * 1000).unwrap());
        }
        pics.extend(dec.flush());
        let reference = std::fs::read(yuv).unwrap();
        common::compare(&pics, &reference, f.width as usize, f.height as usize).unwrap();
        // pts are passed through reordering
        let mut pts: Vec<i64> = pics.iter().map(|p| p.pts).collect();
        pts.sort();
        assert_eq!(pts, (0..samples.len() as i64).map(|i| i * 1000).collect::<Vec<_>>());
        let p0 = &pics[0];
        assert!(p0.key);
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
    for name in ["main_cabac_b", "baseline_cif_noise", "cavlc_b", "bpyramid"] {
        let f = common::fixture(name);
        let Some((h264, _)) = common::ensure(f) else { return };
        let data = std::fs::read(h264).unwrap();
        let mut seed = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        for round in 0..12 {
            let mut d = data.clone();
            // flip bytes after the parameter sets
            for _ in 0..(1 + round * 3) {
                let pos = 64 + (xorshift(&mut seed) as usize) % (d.len() - 64);
                d[pos] ^= 1 << (xorshift(&mut seed) % 8);
            }
            if round % 4 == 3 {
                d.truncate(d.len() * 2 / 3);
            }
            for threads in [1, 4] {
                let mut dec = Decoder::with_threads(threads);
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
        let mut g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
        if len > 10 {
            g[..4].copy_from_slice(&[0, 0, 0, 1]);
        }
        let mut dec = Decoder::new();
        let _ = dec.decode(&g, 0);
        let _ = dec.flush();
    }
    assert!(Decoder::from_avcc(&[1, 2, 3]).is_err());
}
