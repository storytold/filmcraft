//! Deterministic mutation fuzzing: the demuxer must never panic on corrupted input.

use crate::common;

use filmcraft_isobmff::*;
use std::io::Cursor;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn synthetic_files() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for brand in [Brand::Mp4, Brand::Mov] {
        let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(brand)).unwrap();
        let v = w.add_track(TrackConfig::new(SampleEntry::avc(AvcConfig::new(vec![vec![0x67, 0x64, 0, 0x1F]], vec![vec![0x68]], 4), 64, 64), 25)).unwrap();
        let mut cfg = TrackConfig::new(SampleEntry::aac(vec![0x11, 0x90], 2, 48000), 48000);
        cfg.media_start = Some(1024);
        let a = w.add_track(cfg).unwrap();
        let p = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: 8000.0 };
        let pc = w.add_track(TrackConfig::new(SampleEntry::pcm(p), 8000)).unwrap();
        w.add_timecode_track(v, TimecodeConfig { flags: 1, timescale: 30000, frame_duration: 1001, frames_per_second: 30, ..Default::default() }, 1000)
            .unwrap();
        for i in 0..20u8 {
            w.write_sample(v, WriteSample { data: &[i; 20], duration: 1, composition_offset: (i % 3) as i32, is_sync: i % 10 == 0 }).unwrap();
            w.write_sample(a, WriteSample { data: &[i; 7], duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
            w.write_sample(pc, WriteSample { data: &[i; 16], duration: 0, composition_offset: 0, is_sync: true }).unwrap();
        }
        out.push(w.finish().unwrap().into_inner());
    }
    let mut w = FragmentedWriter::new(Vec::new(), WriterOptions::new(Brand::Mp4));
    let v = w.add_track(TrackConfig::new(SampleEntry::jpeg(16, 16), 1000)).unwrap();
    for i in 0..12u8 {
        w.write_sample(v, WriteSample { data: &[i; 9], duration: 40, composition_offset: -1, is_sync: i % 4 == 0 }).unwrap();
        if i % 4 == 3 {
            w.flush_fragment().unwrap();
        }
    }
    out.push(w.finish().unwrap());
    out
}

fn exercise(data: &[u8]) -> bool {
    let r = open(data);
    let ok = r.is_ok();
    if let Ok(f) = r {
        for (ti, t) in f.tracks.iter().enumerate() {
            let n = t.samples.len();
            for i in [0, n / 2, n.saturating_sub(1)] {
                let _ = f.read_sample(data, ti, i);
                let _ = t.sync_sample_before(i);
                let _ = t.presentation_pts(i);
            }
            let _ = t.sample_at_pts(i64::MIN);
            let _ = t.sample_at_pts(i64::MAX);
            let _ = t.sample_at_presentation_time(0);
            if let Some(CodecConfig::Timecode(tc)) = t.codec() {
                let _ = tc.format_frame(tc.start_frame.unwrap_or(u32::MAX));
            }
        }
    }
    ok
}

fn mutate_all(base: &[u8], seed: u64, iterations: usize) {
    let mut rng = Rng(seed | 1);
    // Focus mutations on the moov/moof boxes (where parsing happens).
    let hot: Vec<(usize, usize)> = {
        let mut v = Vec::new();
        for name in [b"moov", b"moof"] {
            let mut from = 0;
            while let Some(p) = base[from..].windows(4).position(|w| w == name) {
                let at = from + p;
                let start = at.saturating_sub(4);
                let size = u32::from_be_bytes(base[start..start + 4].try_into().unwrap()) as usize;
                v.push((start, (start + size.max(8)).min(base.len())));
                from = at + 4;
            }
        }
        if v.is_empty() {
            v.push((0, base.len()));
        }
        v
    };
    let mut parsed = 0usize;
    for _ in 0..iterations {
        let mut d = base.to_vec();
        let (lo, hi) = hot[rng.below(hot.len())];
        let edits = 1 + rng.below(4);
        for _ in 0..edits {
            let pos = if rng.below(10) < 8 { lo + rng.below(hi - lo) } else { rng.below(d.len()) };
            let pos = pos.min(d.len() - 1);
            match rng.below(6) {
                0 => d[pos] ^= 1 << rng.below(8),
                1 => d[pos] = rng.next() as u8,
                2 | 3 => {
                    // Overwrite a 32-bit field with an extreme value.
                    let v: u32 = match rng.below(5) {
                        0 => 0,
                        1 => 1,
                        2 => u32::MAX,
                        3 => 0x8000_0000,
                        _ => rng.next() as u32,
                    };
                    let end = (pos + 4).min(d.len());
                    d[pos..end].copy_from_slice(&v.to_be_bytes()[..end - pos]);
                }
                4 => d.truncate(pos.max(8)),
                _ => {
                    let n = rng.below(16).min(d.len() - pos);
                    d.drain(pos..pos + n);
                }
            }
            if d.is_empty() {
                break;
            }
        }
        parsed += exercise(&d) as usize;
    }
    eprintln!("seed {seed:#x}: {parsed}/{iterations} mutated files still parsed");
}

#[test]
fn mutated_synthetic_files_never_panic() {
    for (i, f) in synthetic_files().iter().enumerate() {
        exercise(f);
        mutate_all(f, 0x5EED_0000 + i as u64, 20_000);
    }
}

#[test]
fn mutated_ffmpeg_fixtures_never_panic() {
    for (i, name) in ["h264_bframes.mp4", "frag.mp4", "tmcd_2997df.mov", "prores.mov", "hevc.mp4", "aac.mp4"].iter().enumerate() {
        let Some(p) = common::fixture(name) else {
            eprintln!("skipping {name}: fixture unavailable");
            continue;
        };
        let data = std::fs::read(p).unwrap();
        mutate_all(&data, 0xF00D + i as u64, 5000);
    }
}

#[test]
fn truncations_never_panic() {
    for f in synthetic_files() {
        for n in 0..f.len().min(4096) {
            exercise(&f[..n]);
        }
    }
}
