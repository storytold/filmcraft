//! Audio packets placed by container timestamps (#236).
//!
//! An AAC access unit decodes to 1024 samples, 21.33 ms at 48 kHz. Matroska stores timestamps in
//! whole milliseconds, and OBS's remux to MP4 carries that rounding into sample durations of 1008,
//! 1008, 1056… Placing each decoded packet at its own timestamp overwrote the end of one packet and
//! left a hole of silence after another: playback and export crackled throughout. The streams here
//! are AAC from our own encoder muxed with such timestamps; read back block by block, as playback
//! reads them, they must be exactly the access units decoded back to back.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_matroska::{LacingMode, MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_media::MediaSource;

use crate::audio::PacketDecoder;
use crate::{MkvSource, Mp4Source};

/// A second of a stereo tone (440 Hz left, 660 Hz right) encoded with our AAC encoder: the
/// AudioSpecificConfig and the access units.
fn aac(rate: u32) -> (Vec<u8>, Vec<Vec<u8>>) {
    let tone = |hz: f32| (0..rate as usize).map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()).collect::<Vec<f32>>();
    let (l, r) = (tone(440.0), tone(660.0));
    let mut enc = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(rate, 2, 160_000)).unwrap();
    let mut units = enc.encode(&[&l, &r]);
    units.extend(enc.flush());
    (enc.audio_specific_config(), units)
}

/// The access units decoded back to back.
fn continuous(asc: &[u8], rate: u32, units: &[Vec<u8>]) -> Vec<Vec<f32>> {
    let mut dec = PacketDecoder::aac(asc, rate).unwrap();
    let mut out = vec![Vec::new(), Vec::new()];
    for u in units {
        for (o, c) in out.iter_mut().zip(dec.decode(u, 0).unwrap()) {
            o.extend(c);
        }
    }
    out
}

/// Access unit `k`'s start to the nearest millisecond, as muxers stamp it.
fn stamp_ms(rate: u32, k: usize) -> i64 {
    (k as i64 * 1024 * 2000 + rate as i64) / (2 * rate as i64)
}

/// Matroska (1 ms timestamps), with the units after `gap` delayed by `gap_ms` (a real gap).
fn mkv(asc: &[u8], rate: u32, units: &[Vec<u8>], lacing: LacingMode, gap: usize, gap_ms: i64) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Audio, "A_AAC");
    spec.codec_private = asc.to_vec();
    spec.audio = Some((rate as f64, 2, None));
    let opts = MuxOptions { lacing, ..MuxOptions::default() };
    assert_eq!(opts.timestamp_scale, 1_000_000, "1 ms timestamps");
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], opts).unwrap();
    for (k, u) in units.iter().enumerate() {
        let ms = stamp_ms(rate, k) + if k >= gap { gap_ms } else { 0 };
        w.write_frame(0, ms * 1_000_000, true, u, None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// MP4 as OBS remuxes Matroska: each millisecond timestamp rescaled to the sample rate, the
/// differences written as sample durations.
fn mp4_remuxed(asc: &[u8], rate: u32, units: &[Vec<u8>]) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::aac(asc.to_vec(), 2, rate), rate)).unwrap();
    let at = |k: usize| (stamp_ms(rate, k) * rate as i64 + 500) / 1000;
    for (k, u) in units.iter().enumerate() {
        let duration = (at(k + 1) - at(k)) as u32;
        w.write_sample(t, WriteSample { data: u, duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// `frames` samples per channel read in 512-sample blocks at `rate`, as playback reads them.
fn played(src: &dyn MediaSource, rate: u32, frames: usize) -> Vec<Vec<f32>> {
    let mut out = vec![Vec::new(), Vec::new()];
    let mut pos = 0;
    while pos < frames {
        let n = 512.min(frames - pos);
        let b = src.audio(pos as i64, n, rate).unwrap();
        for (o, c) in out.iter_mut().zip(&b.channels) {
            o.extend_from_slice(c);
        }
        pos += n;
    }
    out
}

/// Samples of `got` that differ from `want[k - shift]` (silence where that is out of range), and
/// the first few.
fn mismatches(got: &[Vec<f32>], want: &[Vec<f32>], shift: usize) -> (usize, Vec<(usize, usize)>) {
    let mut bad = Vec::new();
    for (c, (g, w)) in got.iter().zip(want).enumerate() {
        for (k, x) in g.iter().enumerate() {
            let expect = k.checked_sub(shift).and_then(|j| w.get(j)).copied().unwrap_or(0.0);
            if x.to_bits() != expect.to_bits() {
                bad.push((c, k));
            }
        }
    }
    (bad.len(), bad.into_iter().take(4).collect())
}

/// Runs of two or more exact zeros inside the signal: holes left between packets.
fn holes(got: &[Vec<f32>]) -> usize {
    got.iter().map(|c| c.windows(2).filter(|w| w[0] == 0.0 && w[1] == 0.0).count()).sum()
}

#[test]
fn matroska_millisecond_stamps_play_back_to_back() {
    for rate in [48_000, 44_100] {
        let (asc, units) = aac(rate);
        let want = continuous(&asc, rate, &units);
        let frames = units.len().saturating_sub(1) * 1024;
        for lacing in [LacingMode::None, LacingMode::Xiph] {
            let bytes = mkv(&asc, rate, &units, lacing, usize::MAX, 0);
            let src = MkvSource::open("tone.mkv", bytes.clone()).unwrap();
            let got = played(&src, rate, frames);
            let (n, first) = mismatches(&got, &want, 0);
            assert_eq!(n, 0, "{rate} Hz, {lacing:?}: {n} samples differ from the continuous decode, first (channel, sample) {first:?}");
            // random access (a fresh source, so a cold decoder primed with the packet before) gives
            // the same samples
            let src = MkvSource::open("tone.mkv", bytes).unwrap();
            for start in [30_011usize, 7_000] {
                let got = src.audio(start as i64, 4_800, rate).unwrap().channels;
                let at: Vec<Vec<f32>> = want.iter().map(|c| c[start..start + 4_800].to_vec()).collect();
                assert_eq!(mismatches(&got, &at, 0).0, 0, "{rate} Hz, {lacing:?}: seek to {start}");
            }
        }
    }
}

#[test]
fn obs_remuxed_mp4_plays_back_to_back() {
    for rate in [48_000, 44_100] {
        let (asc, units) = aac(rate);
        let want = continuous(&asc, rate, &units);
        let bytes = mp4_remuxed(&asc, rate, &units);
        let src = Mp4Source::open("tone.mp4", bytes).unwrap();
        let got = played(&src, rate, units.len().saturating_sub(1) * 1024);
        let (n, first) = mismatches(&got, &want, 0);
        assert_eq!(n, 0, "{rate} Hz: {n} samples differ from the continuous decode, first (channel, sample) {first:?}");
    }
}

#[test]
fn a_real_gap_stays_where_the_timestamps_put_it() {
    let rate = 48_000;
    let (asc, units) = aac(rate);
    let want = continuous(&asc, rate, &units);
    // 100 ms of the recording missing before unit 20: it plays as silence, and the units after it
    // keep their timestamps (to the millisecond)
    let src = MkvSource::open("gap.mkv", mkv(&asc, rate, &units, LacingMode::None, 20, 100)).unwrap();
    let shift = ((stamp_ms(rate, 20) + 100) * 48 - 20 * 1024) as usize;
    assert!((4_700..4_900).contains(&shift), "{shift}");
    let got = played(&src, rate, units.len().saturating_sub(1) * 1024 + shift);
    let before: Vec<Vec<f32>> = got.iter().map(|c| c[..20 * 1024].to_vec()).collect();
    assert_eq!(mismatches(&before, &want, 0).0, 0);
    assert!(got.iter().all(|c| c[20 * 1024..20 * 1024 + shift].iter().all(|x| *x == 0.0)), "the gap is silent");
    let after: Vec<Vec<f32>> = got.iter().map(|c| c[20 * 1024 + shift..].to_vec()).collect();
    let rest: Vec<Vec<f32>> = want.iter().map(|c| c[20 * 1024..].to_vec()).collect();
    let (n, first) = mismatches(&after, &rest, 0);
    assert_eq!(n, 0, "{n} samples after the gap differ, first {first:?}");
}

#[test]
fn playing_at_another_rate_leaves_no_holes() {
    // a 44.1 kHz recording in a 48 kHz sequence (export's default rate)
    let (asc, units) = aac(44_100);
    let src = MkvSource::open("tone.mkv", mkv(&asc, 44_100, &units, LacingMode::None, usize::MAX, 0)).unwrap();
    let frames = (units.len().saturating_sub(2) * 1024) * 48_000 / 44_100;
    let got = played(&src, 48_000, frames);
    // skip the encoder's priming (the first frame decodes to near-silence)
    let tail: Vec<Vec<f32>> = got.iter().map(|c| c[2048..].to_vec()).collect();
    assert_eq!(holes(&tail), 0);
}

/// #790: the encoder priming stored as `CodecDelay` (1024 samples: 21.333 ms at 48 kHz, 23.220 ms
/// at 44.1 kHz) is trimmed exactly. The demuxer takes it off the millisecond timestamps rounded to
/// whole milliseconds; starting the first packet there played the track 16 (48 kHz) or 10 (44.1 kHz)
/// samples late.
#[test]
fn matroska_codec_delay_is_trimmed_to_the_sample() {
    for rate in [48_000u32, 44_100] {
        let (asc, units) = aac(rate);
        let want = continuous(&asc, rate, &units);
        let delay_ns = (1024 * 1_000_000_000 + rate as i64 / 2) / rate as i64;
        let mut spec = TrackSpec::new(TrackKind::Audio, "A_AAC");
        spec.codec_private = asc.clone();
        spec.audio = Some((rate as f64, 2, None));
        spec.codec_delay_ns = delay_ns as u64;
        let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], MuxOptions::default()).unwrap();
        for (k, u) in units.iter().enumerate() {
            let start_ns = (k as i64 * 1024 * 1_000_000_000 + rate as i64 / 2) / rate as i64;
            w.write_frame(0, start_ns - delay_ns, true, u, None).unwrap();
        }
        let bytes: Arc<[u8]> = w.finish().unwrap().into_inner().into();
        let src = MkvSource::open("primed.mkv", bytes).unwrap();
        let frames = units.len().saturating_sub(2) * 1024;
        let got = played(&src, rate, frames);
        let trimmed: Vec<Vec<f32>> = want.iter().map(|c| c[1024..].to_vec()).collect();
        let (n, first) = mismatches(&got, &trimmed, 0);
        assert_eq!(n, 0, "{rate} Hz: {n} samples differ from the decode with the priming trimmed, first (channel, sample) {first:?}");
    }
}
