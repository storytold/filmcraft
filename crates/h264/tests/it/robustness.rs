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

fn is_idr(au: &[u8]) -> bool {
    annexb_nals(au).iter().any(|n| n.first().is_some_and(|h| h & 0x1f == 5))
}

fn decode_all(dec: &mut Decoder, aus: &[&[u8]], first_pts: usize) -> Vec<filmcraft_h264::Picture> {
    let mut pics = Vec::new();
    for (i, au) in aus.iter().enumerate() {
        pics.extend(dec.decode(au, (first_pts + i) as i64).unwrap());
    }
    pics.extend(dec.flush());
    assert_eq!(dec.take_error(), None);
    pics
}

fn assert_same(a: &[filmcraft_h264::Picture], b: &[filmcraft_h264::Picture]) {
    assert_eq!(a.len(), b.len());
    for (p, q) in a.iter().zip(b) {
        assert_eq!((p.pts, p.poc, p.key, p.width, p.height), (q.pts, q.poc, q.key, q.width, q.height));
        assert!(p.y == q.y && p.u == q.u && p.v == q.v, "picture {} differs", p.pts);
    }
}

#[test]
fn reset_mid_stream_matches_a_new_decoder() {
    for name in ["keyint10", "open_gop", "slices4"] {
        let f = common::fixture(name);
        let Some((h264, _)) = common::ensure(f) else { return };
        let data = std::fs::read(h264).unwrap();
        let aus = common::split_access_units(&data);
        let idrs: Vec<usize> = (0..aus.len()).filter(|&i| is_idr(aus[i])).collect();
        for (threads, draft) in [(1, false), (3, false), (8, false), (8, true)] {
            for stop in [1, 4, 9, 13, 22].map(|s| s.min(aus.len())) {
                // seek to the next IDR, or back to the start
                let next = idrs.iter().copied().find(|&i| i >= stop).unwrap_or(0);
                let mut dec = Decoder::with_threads(threads);
                dec.set_draft(draft);
                for (i, au) in aus[..stop].iter().enumerate() {
                    let _ = dec.decode(au, i as i64);
                }
                dec.reset();
                assert_eq!(dec.draft(), draft);
                let got = decode_all(&mut dec, &aus[next..], next);
                let mut fresh = Decoder::with_threads(threads);
                fresh.set_draft(draft);
                let want = decode_all(&mut fresh, &aus[next..], next);
                assert!(want.len() >= aus.len() - next - 1);
                assert_same(&got, &want);
            }
        }
        // length-prefixed input: the avcC configuration is reapplied after the reset
        let (avcc, samples) = to_avcc(&data);
        let samples: Vec<&[u8]> = samples.iter().map(|s| s.as_slice()).collect();
        let mut dec = Decoder::from_avcc(&avcc).unwrap();
        for s in &samples[..7] {
            let _ = dec.decode(s, 0);
        }
        dec.reset();
        assert_eq!(dec.nal_length_size(), None);
        dec.configure_avcc(&avcc).unwrap();
        let got = decode_all(&mut dec, &samples, 0);
        let want = decode_all(&mut Decoder::from_avcc(&avcc).unwrap(), &samples, 0);
        assert_same(&got, &want);
    }
}

#[test]
fn rapid_resets_do_not_hang() {
    let f = common::fixture("bench_1080p");
    let Some((h264, _)) = common::ensure(f) else { return };
    let data = std::fs::read(h264).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let aus = common::split_access_units(&data);
        let mut seed = 7u64;
        for threads in [2, 8, 16] {
            let mut dec = Decoder::with_threads(threads);
            for round in 0..40 {
                // mostly from the IDR (pictures in flight at the reset), sometimes from a random point
                let start = if round % 4 == 3 { xorshift(&mut seed) as usize % aus.len() } else { 0 };
                let n = 1 + xorshift(&mut seed) as usize % 12;
                for au in aus.iter().skip(start).take(n) {
                    let _ = dec.decode(au, 0);
                }
                dec.reset();
            }
            let got = decode_all(&mut dec, &aus, 0);
            let want = decode_all(&mut Decoder::with_threads(threads), &aus, 0);
            assert_same(&got, &want);
        }
        tx.send(()).unwrap();
    });
    rx.recv_timeout(std::time::Duration::from_secs(300)).expect("decoder hung after reset");
}
