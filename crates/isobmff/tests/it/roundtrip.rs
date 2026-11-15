//! Property tests: write with our muxers, read back, compare sample tables and bytes.

use filmcraft_isobmff::*;
use proptest::prelude::*;
use std::io::Cursor;

#[derive(Clone, Debug)]
struct S {
    size: usize,
    duration: u32,
    cto: i32,
    sync: bool,
}

fn sample_strategy() -> impl Strategy<Value = S> {
    (0usize..300, 1u32..3000, -200i32..2000, any::<bool>()).prop_map(|(size, duration, cto, sync)| S { size, duration, cto, sync })
}

fn payload(track: usize, i: usize, n: usize) -> Vec<u8> {
    (0..n).map(|k| (track * 31 + i * 7 + k) as u8).collect()
}

fn entry(kind: u8) -> SampleEntry {
    match kind {
        0 => SampleEntry::avc(AvcConfig::new(vec![vec![0x67, 0x64, 0, 0x28, 1, 2]], vec![vec![0x68, 3]], 4), 320, 240),
        1 => SampleEntry::prores(FourCc(*b"apcn"), 1920, 1080),
        2 => SampleEntry::aac(vec![0x11, 0x90], 2, 48000),
        _ => SampleEntry::jpeg(64, 64),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn progressive_roundtrip(
        tracks in prop::collection::vec((0u8..4, prop::collection::vec(sample_strategy(), 0..40)), 1..4),
        order in prop::collection::vec(0usize..4, 0..200),
        mov in any::<bool>(),
        fast in any::<bool>(),
        with_pcm in any::<bool>(),
        start in prop::option::of(0i64..500),
    ) {
        let brand = if mov { Brand::Mov } else { Brand::Mp4 };
        let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(brand)).unwrap();
        for (k, (kind, _)) in tracks.iter().enumerate() {
            let mut cfg = TrackConfig::new(entry(*kind), 1000 + k as u32 * 11);
            cfg.media_start = start;
            w.add_track(cfg).unwrap();
        }
        let pcm_track = if with_pcm {
            let p = PcmConfig { bits: 24, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48000.0 };
            Some(w.add_track(TrackConfig::new(SampleEntry::pcm(p), 48000)).unwrap())
        } else { None };
        // Interleave: follow `order`, then flush the rest.
        let mut next = vec![0usize; tracks.len()];
        let mut pcm_chunks = Vec::new();
        let emit = |w: &mut Mp4Writer<Cursor<Vec<u8>>>, t: usize, next: &mut Vec<usize>| {
            let i = next[t];
            let s = &tracks[t].1[i];
            let d = payload(t, i, s.size);
            w.write_sample(t, WriteSample { data: &d, duration: s.duration, composition_offset: s.cto, is_sync: s.sync }).unwrap();
            next[t] += 1;
        };
        for (step, &o) in order.iter().enumerate() {
            if let Some(p) = pcm_track && step % 5 == 0 {
                let d = payload(9, step, 6 * (1 + step % 7));
                w.write_sample(p, WriteSample { data: &d, duration: 0, composition_offset: 0, is_sync: true }).unwrap();
                pcm_chunks.push(d);
            }
            let t = o % tracks.len();
            if next[t] < tracks[t].1.len() {
                emit(&mut w, t, &mut next);
            }
        }
        for t in 0..tracks.len() {
            while next[t] < tracks[t].1.len() {
                emit(&mut w, t, &mut next);
            }
        }
        let out = if fast { w.finish_faststart() } else { w.finish() }.unwrap().into_inner();
        let f = open(out.as_slice()).unwrap();
        prop_assert_eq!(f.is_quicktime, mov);
        prop_assert_eq!(f.tracks.len(), tracks.len() + pcm_track.is_some() as usize);
        for (k, (kind, samples)) in tracks.iter().enumerate() {
            let t = &f.tracks[k];
            prop_assert_eq!(t.codec(), Some(&entry(*kind).codec));
            prop_assert_eq!(t.samples.len(), samples.len());
            let mut dts = 0i64;
            for (i, (s, r)) in samples.iter().zip(&t.samples).enumerate() {
                prop_assert_eq!(r.size as usize, s.size);
                prop_assert_eq!(r.duration, s.duration);
                prop_assert_eq!(r.dts, dts);
                prop_assert_eq!(r.pts, dts + s.cto as i64);
                prop_assert_eq!(r.is_sync, s.sync);
                prop_assert_eq!(f.read_sample(out.as_slice(), k, i).unwrap(), payload(k, i, s.size));
                dts += s.duration as i64;
            }
            prop_assert_eq!(t.duration, dts as u64);
            match start {
                Some(st) if !samples.is_empty() || st == 0 => prop_assert_eq!(t.edit_offset, -st),
                _ => {}
            }
        }
        if let Some(p) = pcm_track {
            let t = &f.tracks[p];
            let got: Vec<u8> = (0..t.samples.len()).flat_map(|i| f.read_sample(out.as_slice(), p, i).unwrap()).collect();
            prop_assert_eq!(got, pcm_chunks.concat());
            let frames: u64 = t.samples.iter().map(|s| s.duration as u64).sum();
            prop_assert_eq!(frames, pcm_chunks.iter().map(|c| c.len() as u64 / 6).sum::<u64>());
        }
    }

    #[test]
    fn fragmented_roundtrip(
        samples in prop::collection::vec(sample_strategy(), 1..60),
        cuts in prop::collection::vec(any::<bool>(), 60),
    ) {
        let mut w = FragmentedWriter::new(Vec::new(), WriterOptions::new(Brand::Mp4));
        let v = w.add_track(TrackConfig::new(entry(0), 90000)).unwrap();
        let a = w.add_track(TrackConfig::new(entry(2), 48000)).unwrap();
        for (i, s) in samples.iter().enumerate() {
            w.write_sample(v, WriteSample { data: &payload(0, i, s.size), duration: s.duration, composition_offset: s.cto, is_sync: s.sync }).unwrap();
            w.write_sample(a, WriteSample { data: &payload(1, i, s.size / 2), duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
            if cuts[i] { w.flush_fragment().unwrap(); }
        }
        let out = w.finish().unwrap();
        let f = open(out.as_slice()).unwrap();
        prop_assert!(f.fragmented);
        let mut dts = 0i64;
        for (i, s) in samples.iter().enumerate() {
            let r = f.tracks[0].samples[i];
            prop_assert_eq!((r.size as usize, r.dts, r.pts, r.is_sync), (s.size, dts, dts + s.cto as i64, s.sync));
            prop_assert_eq!(f.read_sample(out.as_slice(), 0, i).unwrap(), payload(0, i, s.size));
            prop_assert_eq!(f.read_sample(out.as_slice(), 1, i).unwrap(), payload(1, i, s.size / 2));
            prop_assert_eq!(f.tracks[1].samples[i].dts, 1024 * i as i64);
            dts += s.duration as i64;
        }
    }
}

#[test]
fn sample_lookup() {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = w.add_track(TrackConfig::new(entry(0), 25)).unwrap();
    // I P B B pattern: decode order I0 P3 B1 B2 | I4 P7 B5 B6
    let cto = [1, 3, 0, 0, 1, 3, 0, 0];
    for (i, c) in cto.iter().enumerate() {
        w.write_sample(t, WriteSample { data: &[i as u8], duration: 1, composition_offset: *c, is_sync: i % 4 == 0 }).unwrap();
    }
    let out = w.finish().unwrap().into_inner();
    let f = open(out.as_slice()).unwrap();
    let tr = &f.tracks[0];
    // pts: 1,4,2,3,5,8,6,7
    assert_eq!(tr.sample_at_pts(0), Some(0));
    assert_eq!(tr.sample_at_pts(2), Some(2));
    assert_eq!(tr.sample_at_pts(4), Some(1));
    assert_eq!(tr.sample_at_pts(7), Some(7));
    assert_eq!(tr.sample_at_pts(100), Some(5));
    assert_eq!(tr.sync_sample_before(3), 0);
    assert_eq!(tr.sync_sample_before(4), 4);
    assert_eq!(tr.sync_sample_before(7), 4);
    assert_eq!(tr.sync_samples(), &[0, 4]);
}

#[test]
fn large_offsets_use_co64_and_largesize_mdat() {
    // Files > 4 GiB are impractical in tests, so patch a small file into the same layout the
    // writer produces for them: co64 chunk offsets and a 16-byte large-size mdat header.
    let mut moov = Vec::new();
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = w.add_track(TrackConfig::new(entry(3), 30)).unwrap();
    w.write_sample(t, WriteSample { data: b"abc", duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    let out = w.finish().unwrap().into_inner();
    // Replace stco with co64 (same entry count, 64-bit offsets) and rebuild.
    let f = open(out.as_slice()).unwrap();
    let off = f.tracks[0].samples[0].offset;
    let stco = out.windows(4).position(|w| w == b"stco").unwrap() - 4;
    let moov_at = out.windows(4).position(|w| w == b"moov").unwrap() - 4;
    moov.extend_from_slice(&out[moov_at..stco]);
    moov.extend_from_slice(&24u32.to_be_bytes());
    moov.extend_from_slice(b"co64");
    moov.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    moov.extend_from_slice(&off.to_be_bytes());
    // Fix up sizes of moov/trak/mdia/minf/stbl (+4 bytes each).
    for name in [b"moov", b"trak", b"mdia", b"minf", b"stbl"] {
        let p = moov.windows(4).position(|w| w == name).unwrap() - 4;
        let s = u32::from_be_bytes(moov[p..p + 4].try_into().unwrap()) + 4;
        moov[p..p + 4].copy_from_slice(&s.to_be_bytes());
    }
    let mut file = out[..moov_at].to_vec();
    file.extend_from_slice(&moov);
    // ftyp is 32 bytes for MP4; then `free` (8) + `mdat` (8) → one large-size mdat header.
    let ftyp_len = u32::from_be_bytes(file[0..4].try_into().unwrap()) as usize;
    assert_eq!(&file[ftyp_len + 4..ftyp_len + 8], b"free");
    let mdat_len = (moov_at - ftyp_len) as u64;
    file[ftyp_len..ftyp_len + 4].copy_from_slice(&1u32.to_be_bytes());
    file[ftyp_len + 4..ftyp_len + 8].copy_from_slice(b"mdat");
    file[ftyp_len + 8..ftyp_len + 16].copy_from_slice(&mdat_len.to_be_bytes());
    let g = open(file.as_slice()).unwrap();
    assert_eq!(g.read_sample(file.as_slice(), 0, 0).unwrap(), b"abc");
    // size == 0 ("extends to end of file") on a trailing box.
    let mut z = file.clone();
    z.extend_from_slice(&[0, 0, 0, 0, b'f', b'r', b'e', b'e', 1, 2, 3]);
    assert!(open(z.as_slice()).is_ok());
}

/// Found by `progressive_roundtrip`: only zero-byte samples must not be written as uniform
/// `stsz` size 0, which readers take to mean "a size table follows".
#[test]
fn all_empty_samples_roundtrip() {
    for n in [1usize, 3] {
        let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        w.add_track(TrackConfig::new(entry(0), 1000)).unwrap();
        for _ in 0..n {
            w.write_sample(0, WriteSample { data: &[], duration: 1, composition_offset: 0, is_sync: false }).unwrap();
        }
        let out = w.finish().unwrap().into_inner();
        let f = open(out.as_slice()).unwrap();
        assert_eq!(f.tracks[0].samples.len(), n);
        assert!(f.tracks[0].samples.iter().all(|s| s.size == 0));
    }
}
