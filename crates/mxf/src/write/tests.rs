//! Writer tests: files read back with our own demuxer; truncation and mutation never panic.

use std::io::Cursor;

use super::*;
use crate::{Codec, OperationalPattern, PartitionKind, TrackKind, Wrapping, open};

fn prores_like(i: usize) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 40];
    v.extend_from_slice(b"icpf");
    v.extend(std::iter::repeat_n(i as u8, 32 + i * 3));
    v
}

fn vc3_like(i: usize) -> Vec<u8> {
    let mut v = vec![0x00, 0x00, 0x02, 0x80, 0x01];
    v.extend(std::iter::repeat_n(i as u8 ^ 0x5A, 500 + 7 * i));
    v
}

/// 16-bit stereo PCM: frame k = (k, -k).
fn pcm16(start: usize, n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for k in start..start + n {
        let s = (k % 30000) as i16;
        v.extend_from_slice(&s.to_le_bytes());
        v.extend_from_slice(&(-s).to_le_bytes());
    }
    v
}

/// 24-bit mono PCM: frame k = 1000 * k (wrapping inside the 24-bit range).
fn pcm24(start: usize, n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for k in start..start + n {
        let s = ((k as i64 * 1000) % 8_000_000) as i32 - 4_000_000;
        v.extend_from_slice(&s.to_le_bytes()[..3]);
    }
    v
}

fn op1a_config(rate: Rational, coding: PictureCoding) -> WriterConfig {
    let mut cfg = WriterConfig::new(Pattern::Op1a, rate, PackageIds::from_seed("test", "Test Clip"));
    cfg.picture = Some(PictureDesc::new(coding, 64, 36));
    cfg.sound = vec![SoundDesc { sample_rate: 48_000, channels: 2, bits: 16 }, SoundDesc { sample_rate: 48_000, channels: 1, bits: 24 }];
    cfg.timecode = Some(StartTimecode { frames: 90_000, rate, drop_frame: false });
    cfg
}

#[test]
fn op1a_rejects_an_unbounded_pcm_edit_unit_before_padding() {
    let mut cfg = WriterConfig::new(Pattern::Op1a, Rational::new(1, 36_000), PackageIds::from_seed("low-rate", "Low Rate"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::ProRes { profile: 3 }, 64, 36));
    cfg.sound = vec![SoundDesc { sample_rate: 48_000, channels: 2, bits: 16 }];
    let err = MxfWriter::new(Cursor::new(Vec::new()), cfg).err().unwrap();
    assert!(err.to_string().contains("PCM edit unit"));
}

/// 10 ProRes-like pictures at 25 fps with stereo 16-bit and mono 24-bit sound pushed in uneven chunks.
fn build_op1a() -> Vec<u8> {
    let cfg = op1a_config(Rational::new(25, 1), PictureCoding::ProRes { profile: 3 });
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    let total = 10 * 1920;
    let (mut a, mut b) = (0, 0);
    for i in 0..10 {
        // sound before and after the picture, in chunks unrelated to the edit units
        let n = 1500.min(total - a);
        w.push_sound(0, &pcm16(a, n)).unwrap();
        a += n;
        w.push_picture(&prores_like(i), FrameInfo::intra()).unwrap();
        let m = 2500.min(total - b);
        w.push_sound(1, &pcm24(b, m)).unwrap();
        b += m;
    }
    w.push_sound(0, &pcm16(a, total - a)).unwrap();
    assert_eq!(w.edit_units(), 10);
    w.finish().unwrap().into_inner()
}

#[test]
fn op1a_reads_back() {
    let f = build_op1a();
    let m = open(&f).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.operational_pattern, OperationalPattern::Generalized { item: 1, package: 1 });
    assert_eq!(m.material_package_name.as_deref(), Some("Test Clip"));
    assert_eq!(m.product_name.as_deref(), Some("FilmCraft"));
    let tc = m.timecode.unwrap();
    assert_eq!(tc.format(), "01:00:00:00");
    // header rewritten closed and complete; body and footer partitions; footer metadata used
    let kinds: Vec<(PartitionKind, u8)> = m.partitions.iter().map(|p| (p.kind, p.status)).collect();
    assert_eq!(kinds, vec![(PartitionKind::Header, 4), (PartitionKind::Body, 4), (PartitionKind::Footer, 4)]);
    assert!(m.partitions.iter().all(|p| p.footer_partition == m.partitions[2].offset));
    assert_eq!(m.tracks.len(), 3);
    let v = m.track_of_kind(TrackKind::Picture).unwrap();
    let t = &m.tracks[v];
    assert_eq!(t.codec, Codec::ProRes { profile: Some(3) });
    assert_eq!(t.duration, Some(10));
    assert_eq!(t.edit_rate, Rational::new(25, 1));
    assert_eq!(t.wrapping, Wrapping::Frame);
    assert!(t.indexed || t.codec.intra_only());
    let p = t.picture.as_ref().unwrap();
    assert_eq!((p.frame_width(), p.frame_height(), p.component_depth), (64, 36, 10));
    assert_eq!(p.aspect_ratio, Rational::new(16, 9));
    for i in 0..10 {
        assert_eq!(m.read_sample(&f, v, i).unwrap(), prores_like(i));
    }
    let sounds: Vec<usize> = (0..m.tracks.len()).filter(|&i| m.tracks[i].kind == TrackKind::Sound).collect();
    assert_eq!(sounds.len(), 2);
    let s0 = &m.tracks[sounds[0]];
    assert_eq!(s0.codec, Codec::Pcm);
    assert_eq!(s0.stored_sample_frames(), 10 * 1920);
    assert_eq!(s0.chunks.len(), 10);
    let pcm = m.read_pcm(&f, sounds[0], 1900, 100).unwrap();
    for k in 0..100 {
        assert_eq!(pcm[0][k], (1900 + k) as f32 / 32768.0);
        assert_eq!(pcm[1][k], -((1900 + k) as f32) / 32768.0);
    }
    let s1 = &m.tracks[sounds[1]];
    assert_eq!(s1.sound.as_ref().unwrap().bits, 24);
    let pcm = m.read_pcm(&f, sounds[1], 0, 10 * 1920).unwrap();
    for k in (0..10 * 1920).step_by(97) {
        let want = ((k as i64 * 1000) % 8_000_000) as i32 - 4_000_000;
        assert_eq!(pcm[0][k], want as f32 / 8_388_608.0, "sample {k}");
    }
    // index: one entry per content package, two sound slices
    let entries: usize = m.index_segments.iter().map(|s| s.entries.len()).sum();
    assert_eq!(entries, 10);
    assert_eq!(m.index_segments[0].slice_count, 2);
    assert_eq!(m.index_segments[0].delta_entries.len(), 3);
}

#[test]
fn ntsc_sound_follows_the_edit_unit_pattern() {
    let rate = Rational::new(30000, 1001);
    let mut cfg = op1a_config(rate, PictureCoding::Vc3 { cid: 1274 });
    cfg.sound.truncate(1);
    cfg.timecode = Some(StartTimecode { frames: 107_892, rate, drop_frame: true });
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    let n = 12;
    let total = (n as u64 * 48_000 * 1001 / 30_000) as usize;
    w.push_sound(0, &pcm16(0, total)).unwrap();
    for i in 0..n {
        w.push_picture(&vc3_like(i), FrameInfo::intra()).unwrap();
    }
    let f = w.finish().unwrap().into_inner();
    let m = open(&f).unwrap();
    assert_eq!(m.timecode.unwrap().format(), "01:00:00;00");
    let a = m.track_of_kind(TrackKind::Sound).unwrap();
    let sizes: Vec<u64> = m.tracks[a].chunks.iter().map(|c| c.samples).collect();
    assert_eq!(sizes[..5], [1601, 1602, 1601, 1602, 1602]);
    assert_eq!(m.tracks[a].stored_sample_frames(), total as u64);
    let pcm = m.read_pcm(&f, a, 0, total).unwrap();
    assert!((0..total).all(|k| pcm[0][k] == (k % 30000) as f32 / 32768.0));
    let v = m.track_of_kind(TrackKind::Picture).unwrap();
    assert_eq!(m.tracks[v].codec, Codec::Vc3);
}

#[test]
fn long_gop_index_gives_presentation_order_and_key_frames() {
    let rate = Rational::new(25, 1);
    let mut cfg = op1a_config(rate, PictureCoding::Avc { profile_idc: 100, intra: false });
    cfg.sound.clear();
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    // stored I0 P3 B1 B2 P6 B4 B5 I7(IDR)
    let order = [(0, CodedKind::Intra, true), (3, CodedKind::Predicted, false), (1, CodedKind::Bidirectional, false), (2, CodedKind::Bidirectional, false)];
    let more = [(6, CodedKind::Predicted, false), (4, CodedKind::Bidirectional, false), (5, CodedKind::Bidirectional, false), (7, CodedKind::Intra, true)];
    for (i, (d, kind, key)) in order.iter().chain(more.iter()).enumerate() {
        let au = vec![0, 0, 0, 1, if *key { 0x65 } else { 0x41 }, i as u8];
        w.push_picture(&au, FrameInfo { key: *key, kind: *kind, display: Some(*d) }).unwrap();
    }
    let f = w.finish().unwrap().into_inner();
    let m = open(&f).unwrap();
    let t = &m.tracks[0];
    assert_eq!(t.codec, Codec::Avc { intra: false });
    assert!(t.temporal_offsets && t.indexed);
    let pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
    assert_eq!(pts, vec![0, 3, 1, 2, 6, 4, 5, 7]);
    let keys: Vec<bool> = t.samples.iter().map(|s| s.key).collect();
    assert_eq!(keys, vec![true, false, false, false, false, false, false, true]);
    let b: Vec<bool> = t.samples.iter().map(|s| s.b_picture).collect();
    assert_eq!(b, vec![false, false, true, true, false, true, true, false]);
    assert_eq!(t.sample_at(4), Some(5));
}

#[test]
fn op_atom_picture_is_clip_wrapped() {
    let mut cfg = WriterConfig::new(Pattern::OpAtom, Rational::new(24, 1), PackageIds::from_seed("atom-v", "Atom V"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::Vc3 { cid: 1272 }, 64, 36));
    cfg.timecode = Some(StartTimecode { frames: 86_400, rate: Rational::new(24, 1), drop_frame: false });
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    for i in 0..9 {
        w.push_picture(&vc3_like(i), FrameInfo::intra()).unwrap();
    }
    assert!(w.push_sound(0, &[0, 0]).is_err());
    let f = w.finish().unwrap().into_inner();
    let m = open(&f).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.operational_pattern, OperationalPattern::Atom);
    assert_eq!(m.timecode.unwrap().format(), "01:00:00:00");
    let t = &m.tracks[0];
    assert_eq!(t.wrapping, Wrapping::Clip);
    assert_eq!(t.codec, Codec::Vc3);
    assert_eq!(t.samples.len(), 9);
    for i in 0..9 {
        assert_eq!(m.read_sample(&f, 0, i).unwrap(), vc3_like(i));
    }
}

#[test]
fn op_atom_pcm_helper() {
    let ids = PackageIds::from_seed("atom-a", "Atom A1");
    let samples: Vec<i32> = (0..4801).map(|k| (k * 7 % 65536) - 32768).collect();
    for rate in [None, Some(Rational::new(25, 1))] {
        let opts = OpAtomPcm { sample_rate: 48_000, bits: 16, channels: 1, edit_rate: rate, ids: ids.clone(), timecode: None };
        let f = write_opatom_pcm(&opts, &samples).unwrap();
        let m = open(&f).unwrap();
        assert_eq!(m.operational_pattern, OperationalPattern::Atom);
        let t = &m.tracks[0];
        assert_eq!(t.kind, TrackKind::Sound);
        assert_eq!(t.stored_sample_frames(), 4801);
        assert_eq!(t.duration, Some(if rate.is_some() { 3 } else { 4801 }));
        let pcm = m.read_pcm(&f, 0, 0, 4801).unwrap();
        assert!(samples.iter().zip(&pcm[0]).all(|(s, p)| *s as f32 / 32768.0 == *p));
    }
    // 24-bit stereo
    let opts = OpAtomPcm { sample_rate: 48_000, bits: 24, channels: 2, edit_rate: None, ids, timecode: None };
    let f = write_opatom_pcm(&opts, &[8_388_607, -8_388_608, 1, -1]).unwrap();
    let m = open(&f).unwrap();
    let pcm = m.read_pcm(&f, 0, 0, 2).unwrap();
    assert_eq!(pcm[0], vec![8_388_607.0 / 8_388_608.0, 1.0 / 8_388_608.0]);
    assert_eq!(pcm[1], vec![-1.0, -1.0 / 8_388_608.0]);
}

#[test]
fn package_ids_are_written() {
    let f = build_op1a();
    let ids = PackageIds::from_seed("test", "Test Clip");
    assert!(f.windows(32).any(|w| w == ids.material.0));
    assert!(f.windows(32).any(|w| w == ids.file.0));
    assert_ne!(ids.material, ids.file);
    assert_eq!(Umid::from_seed(b"x"), Umid::from_seed(b"x"));
    assert_ne!(Umid::from_seed(b"x"), Umid::from_seed(b"y"));
}

#[test]
fn long_files_split_the_index_into_segments() {
    let mut cfg = op1a_config(Rational::new(25, 1), PictureCoding::ProRes { profile: 4 });
    cfg.sound.clear();
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    for i in 0..12_000 {
        w.push_picture(&[0, 0, 0, 9, b'i', b'c', b'p', b'f', i as u8], FrameInfo::intra()).unwrap();
    }
    let f = w.finish().unwrap().into_inner();
    let m = open(&f).unwrap();
    assert!(m.index_segments.len() >= 2);
    assert_eq!(m.index_segments.iter().map(|s| s.entries.len()).sum::<usize>(), 12_000);
    assert_eq!(m.tracks[0].samples.len(), 12_000);
    assert_eq!(m.read_sample(&f, 0, 11_999).unwrap()[8], (11_999 % 256) as u8);
}

#[test]
fn rejects_bad_configs() {
    let ids = PackageIds::from_seed("x", "x");
    let cfg = WriterConfig::new(Pattern::Op1a, Rational::new(25, 1), ids.clone());
    assert!(MxfWriter::new(Cursor::new(Vec::new()), cfg).is_err());
    let mut cfg = WriterConfig::new(Pattern::OpAtom, Rational::new(25, 1), ids);
    cfg.sound = vec![SoundDesc { sample_rate: 48_000, channels: 1, bits: 16 }; 2];
    assert!(MxfWriter::new(Cursor::new(Vec::new()), cfg).is_err());
}

#[test]
fn timestamps() {
    assert_eq!(Timestamp::from_unix(0), Timestamp { year: 1970, month: 1, day: 1, ..Default::default() });
    let t = Timestamp::from_unix(1_700_000_000);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (2023, 11, 14, 22, 13, 20));
    let t = Timestamp::from_unix(951_782_400); // 2000-02-29
    assert_eq!((t.year, t.month, t.day), (2000, 2, 29));
    // Years far outside chrono's own range are preserved with month/day/time intact and only the
    // year clamped to the MXF `u16`, exactly as the old hand-rolled conversion did (not collapsed
    // to the epoch — reviewer FredApps). `10_000_000_000_000` s → 318857-05-20 17:46:40.
    let t = Timestamp::from_unix(10_000_000_000_000);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (65535, 5, 20, 17, 46, 40));
    // Year 10000 (just past chrono's range) still fits the u16 field.
    let t = Timestamp::from_unix(253_402_300_800);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (10000, 1, 1, 0, 0, 0));
    // A negative year clamps to 0 but keeps its month/day/time.
    let t = Timestamp::from_unix(-10_000_000_000_000);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (0, 8, 13, 6, 13, 20));
    // The `i64` extremes the old code accepted.
    let t = Timestamp::from_unix(i64::MAX);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (65535, 12, 4, 15, 30, 7));
    let t = Timestamp::from_unix(i64::MIN);
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (0, 1, 27, 8, 29, 52));
}

#[test]
fn truncated_and_mutated_files_never_panic() {
    let f = build_op1a();
    let mut opened = 0;
    for cut in (0..f.len()).step_by(41) {
        let part = f[..cut].to_vec();
        if let Ok(m) = open(&part) {
            opened += 1;
            for (ti, t) in m.tracks.iter().enumerate() {
                for i in 0..t.samples.len() {
                    assert!(m.read_sample(&part, ti, i).is_ok(), "cut {cut}");
                }
                let _ = m.read_pcm(&part, ti, 0, 3000);
            }
        }
    }
    assert!(opened > 20, "a truncated file with a complete header still opens ({opened})");
    let atom = write_opatom_pcm(
        &OpAtomPcm { sample_rate: 48_000, bits: 24, channels: 1, edit_rate: None, ids: PackageIds::from_seed("m", "m"), timecode: None },
        &(0..3000).collect::<Vec<i32>>(),
    )
    .unwrap();
    let mut seed = 0x9E37_79B9u64;
    for src in [&f, &atom] {
        for cut in (0..src.len()).step_by(53) {
            let _ = open(&src[..cut].to_vec()).map(|m| m.read_pcm(&src[..cut].to_vec(), 0, 0, 100).ok());
        }
        for _ in 0..400 {
            let mut g = src.clone();
            for _ in 0..6 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed as usize) % g.len();
                g[at] = (seed >> 40) as u8;
            }
            if let Ok(m) = open(&g) {
                for (ti, t) in m.tracks.iter().enumerate() {
                    for i in 0..t.samples.len().min(12) {
                        let _ = m.read_sample(&g, ti, i);
                    }
                    let _ = m.read_pcm(&g, ti, 0, 500);
                }
            }
        }
    }
}

/// #210 review: a whole-clip read of long stereo PCM (more samples than the old fixed
/// 16 777 216-sample request cap, about 175 s at 48 kHz) must work; only reads far past the stored
/// essence are refused.
#[test]
fn long_pcm_reads_backed_by_the_file_are_not_capped() {
    let frames = 200 * 48_000;
    let samples: Vec<i32> = (0..frames * 2).map(|k| (k % 60_000) as i32 - 30_000).collect();
    let opts = OpAtomPcm { sample_rate: 48_000, bits: 16, channels: 2, edit_rate: None, ids: PackageIds::from_seed("long", "Long"), timecode: None };
    let f = write_opatom_pcm(&opts, &samples).unwrap();
    let m = open(&f).unwrap();
    assert_eq!(m.tracks[0].stored_sample_frames(), frames as u64);
    // a little past the end is zero padding, as before
    let pcm = m.read_pcm(&f, 0, 0, frames + 10).unwrap();
    assert_eq!((pcm.len(), pcm[0].len()), (2, frames + 10));
    for k in [0, 1, frames / 2, frames - 1] {
        assert_eq!(pcm[0][k], samples[2 * k] as f32 / 32768.0, "frame {k}");
        assert_eq!(pcm[1][k], samples[2 * k + 1] as f32 / 32768.0, "frame {k}");
    }
    assert_eq!(pcm[0][frames..], [0.0; 10]);
    // a request that is mostly not backed by data is refused before allocating
    assert!(m.read_pcm(&f, 0, 0, frames + (1 << 24)).is_err());
}

/// A TIFF-headed stand-in for a DNG dataset (the demuxer only carries the bytes).
fn dng_like(i: usize) -> Vec<u8> {
    let mut v = b"II*\0\x08\0\0\0".to_vec();
    v.extend(std::iter::repeat_n(i as u8 ^ 0x33, 300 + 11 * i));
    v
}

#[test]
fn cinemadng_op1a_reads_back() {
    for linear_raw in [false, true] {
        let mut cfg = op1a_config(Rational::new(24, 1), PictureCoding::CinemaDng { linear_raw });
        cfg.sound.truncate(1);
        let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
        for i in 0..6 {
            w.push_picture(&dng_like(i), FrameInfo::intra()).unwrap();
            w.push_sound(0, &pcm16(i * 2000, 2000)).unwrap();
        }
        let f = w.finish().unwrap().into_inner();
        // CinemaDNG §7.1.1.2: picture element key 06.0E.2B.34.01.02.01.01.0D.01.03.01.15.01.42.01
        let key = [0x06, 0x0E, 0x2B, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0D, 0x01, 0x03, 0x01, 0x15, 0x01, 0x42, 0x01];
        assert_eq!(f.windows(16).filter(|k| *k == key).count(), 6);
        // §7.2.1: the essence container label of the content kind
        let ec = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0F, 0x0A, 0x0D, 0x0B, 0x02, 0x7F, if linear_raw { 2 } else { 1 }, 0x00];
        assert!(f.windows(16).any(|k| k == ec));
        let m = open(&f).unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let v = m.track_of_kind(TrackKind::Picture).unwrap();
        let t = &m.tracks[v];
        assert_eq!(t.codec, Codec::CinemaDng);
        assert_eq!(t.wrapping, Wrapping::Frame);
        assert_eq!(t.samples.len(), 6);
        let p = t.picture.as_ref().unwrap();
        assert!(p.rgba, "RGBA picture essence descriptor (§7.2.2)");
        assert_eq!((p.frame_width(), p.frame_height()), (64, 36));
        assert!(p.picture_coding.is_none());
        for i in 0..6 {
            assert!(t.samples[i].key);
            assert_eq!(m.read_sample(&f, v, i).unwrap(), dng_like(i));
        }
        assert_eq!(m.timecode.unwrap().format(), "01:02:30:00");
    }
}

#[test]
fn cinemadng_is_never_clip_wrapped() {
    let mut cfg = WriterConfig::new(Pattern::OpAtom, Rational::new(24, 1), PackageIds::from_seed("t", "T"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::CinemaDng { linear_raw: false }, 64, 36));
    assert!(MxfWriter::new(Cursor::new(Vec::new()), cfg).is_err());
}
