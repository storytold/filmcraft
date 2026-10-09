//! Frame lookup against timestamps rounded to the container's timebase (issue #73).
//!
//! WebM stores timestamps in 1 ms units, so at 30 fps frame 2 (66.67 ms) is stored as 67. Flooring
//! the requested time to 66 picked frame 1 instead: every third frame was skipped and the one
//! before it shown twice. MP4 has the same problem when a muxer picks a 1 ms track timescale.
//!
//! The streams are synthetic Motion-JPEG: each picture shows its index as a row of black and
//! white blocks, so a decoded frame says which sample it came from.

use std::io::Cursor;
use std::sync::Arc;

use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_isobmff::{Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_media::{FrameRequest, MediaSource};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

/// Bits of the frame index drawn per picture, one `BLOCK`-pixel square each.
const BITS: u32 = 8;
const BLOCK: u32 = 16;
const SECONDS: i64 = 3;

/// The rates that don't divide 1 ms evenly (affected), and 25 fps (never was).
const RATES: [FrameRate; 7] =
    [FrameRate::FPS_30, FrameRate::FPS_29_97, FrameRate::FPS_60, FrameRate::FPS_59_94, FrameRate::FPS_24, FrameRate::FPS_23_976, FrameRate::FPS_25];

/// How the muxer quantised the exact frame times to milliseconds.
#[derive(Clone, Copy, Debug)]
enum Stamp {
    /// To the nearest millisecond, halves up (mkvmerge, ffmpeg).
    Nearest,
    /// Truncated.
    Floor,
}

fn picture(index: usize) -> Vec<u8> {
    let img = image::GrayImage::from_fn(BITS * BLOCK, BLOCK, |x, _| image::Luma([if (index >> (x / BLOCK)) & 1 == 1 { 255 } else { 0 }]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img).unwrap();
    out
}

fn index_of(f: &VideoFrame) -> usize {
    let PixelData::Rgba8(px) = &f.data else { panic!("MJPEG decodes to RGBA8") };
    (0..BITS).filter(|b| px[((BLOCK / 2 * f.width + b * BLOCK + BLOCK / 2) * 4) as usize] > 127).map(|b| 1 << b).sum()
}

fn frame_count(rate: FrameRate) -> usize {
    rate.frame_at(Tick(SECONDS * TICKS_PER_SECOND)) as usize
}

/// Frame `k`'s start in milliseconds, as the muxer stores it.
fn stamp_ms(rate: FrameRate, k: usize, stamp: Stamp) -> i64 {
    let (num, den) = (rate.num as i128, rate.den as i128);
    let exact2 = 2 * k as i128 * 1000 * den; // twice the time in ms, times num
    (match stamp {
        Stamp::Nearest => (exact2 + num) / (2 * num),
        Stamp::Floor => exact2 / (2 * num),
    }) as i64
}

fn webm(rate: FrameRate, stamp: Stamp) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MJPEG");
    spec.video_size = Some((BITS * BLOCK, BLOCK));
    spec.default_duration_ns = Some((1_000_000_000 * rate.den / rate.num) as u64);
    let opts = MuxOptions { doc_type: "webm".into(), ..MuxOptions::default() };
    assert_eq!(opts.timestamp_scale, 1_000_000, "1 ms timestamps");
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], opts).unwrap();
    for k in 0..frame_count(rate) {
        w.write_frame(0, stamp_ms(rate, k, stamp) * 1_000_000, true, &picture(k), None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

fn mp4_1ms(rate: FrameRate, stamp: Stamp) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), 1000)).unwrap();
    let n = frame_count(rate);
    for k in 0..n {
        let duration = (stamp_ms(rate, k + 1, stamp) - stamp_ms(rate, k, stamp)) as u32;
        w.write_sample(t, WriteSample { data: &picture(k), duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// Plays the stream back at its own rate (every frame's start), and asks for every frame's
/// midpoint too. Returns what went wrong: frames never shown, frames shown twice, wrong midpoints.
fn problems(src: &dyn MediaSource, rate: FrameRate) -> Vec<String> {
    let n = frame_count(rate);
    let at = |t: Tick| index_of(&src.video_frame(FrameRequest::full(t)).unwrap());
    let mut shown = vec![0usize; n];
    for k in 0..n {
        shown[at(rate.tick_of(k as i64)).min(n - 1)] += 1;
    }
    let mut out = Vec::new();
    let dropped: Vec<usize> = (0..n).filter(|&k| shown[k] == 0).collect();
    let repeated: Vec<usize> = (0..n).filter(|&k| shown[k] > 1).collect();
    if !dropped.is_empty() || !repeated.is_empty() {
        out.push(format!(
            "{} of {n} frames never shown {:?}…, {} shown twice {:?}…",
            dropped.len(),
            &dropped[..dropped.len().min(4)],
            repeated.len(),
            &repeated[..repeated.len().min(4)]
        ));
    }
    let mid: Vec<(usize, usize)> = (0..n).map(|k| (k, at(rate.tick_of(k as i64) + Tick(rate.frame_duration().0 / 2)))).filter(|(k, got)| k != got).collect();
    if !mid.is_empty() {
        out.push(format!("{} mid-frame requests wrong, e.g. (wanted, got) {:?}", mid.len(), &mid[..mid.len().min(4)]));
    }
    out
}

fn check(container: &str, open: fn(FrameRate, Stamp) -> Arc<[u8]>) {
    let mut failures = Vec::new();
    for rate in RATES {
        for stamp in [Stamp::Nearest, Stamp::Floor] {
            let src = crate::open_bytes(&format!("synthetic.{container}"), open(rate, stamp)).unwrap();
            for p in problems(src.as_ref(), rate) {
                failures.push(format!("{rate}, {stamp:?} ms stamps: {p}"));
            }
        }
    }
    assert!(failures.is_empty(), "{container}:\n{}", failures.join("\n"));
}

#[test]
fn webm_with_1ms_timestamps_shows_every_frame_once() {
    check("webm", webm);
}

#[test]
fn mp4_with_a_1ms_timescale_shows_every_frame_once() {
    check("mov", mp4_1ms);
}

/// A WebM without DefaultDuration: its rate comes from the 1 ms timestamps alone.
fn webm_no_default_duration(rate: FrameRate, stamp: Stamp) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MJPEG");
    spec.video_size = Some((BITS * BLOCK, BLOCK));
    spec.default_duration_ns = None;
    let opts = MuxOptions { doc_type: "webm".into(), ..MuxOptions::default() };
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], opts).unwrap();
    for k in 0..frame_count(rate) {
        w.write_frame(0, stamp_ms(rate, k, stamp) * 1_000_000, true, &picture(k), None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// Millisecond timestamps make single frame durations lie (60 fps is stored as 17, 17, 16 ms;
/// the median, 17 ms, read as 58.82 fps), so the detected rate must still be the real one.
#[test]
fn the_frame_rate_of_millisecond_timestamps_is_the_real_rate() {
    let mut wrong = Vec::new();
    for (container, open) in [("mov", mp4_1ms as fn(FrameRate, Stamp) -> Arc<[u8]>), ("webm", webm_no_default_duration)] {
        for rate in RATES {
            for stamp in [Stamp::Nearest, Stamp::Floor] {
                let src = crate::open_bytes(&format!("synthetic.{container}"), open(rate, stamp)).unwrap();
                let got = src.info().frame_rate();
                if got != rate {
                    wrong.push(format!("{container} {rate} ({stamp:?}): detected {got}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// An OBS screen recording: 60 fps on a 90 kHz timescale with times rounded to the millisecond
/// (1530, 1530, 1440 ticks) and a couple of dropped frames (a 3060 tick gap). It is 60 fps,
/// not 58.824 (= 1000 / 17, what the median used to say).
#[test]
fn an_obs_recording_with_dropped_frames_is_60_fps() {
    let mut durs: Vec<i64> = (0..1999).map(|k| if k % 3 == 2 { 1440 } else { 1530 }).collect();
    durs[500] = 3060;
    durs[1400] = 2970;
    assert_eq!(crate::rate_from_durations(durs.clone(), 90_000.0), FrameRate::FPS_60);
    // the first few seconds alone (what a short clip has) say the same
    assert_eq!(crate::rate_from_durations(durs[..240].to_vec(), 90_000.0), FrameRate::FPS_60);
    // exact streams keep their exact rate, standard or not
    assert_eq!(crate::rate_from_durations(vec![1001; 300], 60_000.0), FrameRate::FPS_59_94);
    assert_eq!(crate::rate_from_durations(vec![1001; 300], 30_000.0), FrameRate::FPS_29_97);
    assert_eq!(crate::rate_from_durations(vec![2399; 300], 90_000.0), FrameRate::from_f64(90_000.0 / 2399.0));
    // nothing usable: the default rate, never a zero one
    for bad in [vec![], vec![0, -5], vec![i64::MAX, i64::MAX]] {
        let r = crate::rate_from_durations(bad, 90_000.0);
        assert!(r.num > 0 && r.den > 0);
    }
    assert_eq!(crate::rate_from_durations(vec![1500; 10], 0.0), FrameRate::default());
    assert_eq!(crate::rate_from_durations(vec![1500; 10], f64::NAN), FrameRate::default());
}
