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

/// An MP4 at a 90 kHz timescale whose frames after the first start `late` units after the frame
/// grid, as phone footage does (issue #1282): the first sample lasts `late` units longer.
fn mp4_90k_late(rate: FrameRate, late: i64) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), 90_000)).unwrap();
    let start = |k: usize| {
        let grid = (k as i128 * 90_000 * rate.den as i128 + rate.num as i128 / 2) / rate.num as i128;
        grid as i64 + if k > 0 { late } else { 0 }
    };
    for k in 0..frame_count(rate) {
        let duration = (start(k + 1) - start(k)) as u32;
        w.write_sample(t, WriteSample { data: &picture(k), duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

fn check_90k_late(late: i64) {
    let mut failures = Vec::new();
    for rate in RATES {
        let src = crate::open_bytes("synthetic.mov", mp4_90k_late(rate, late)).unwrap();
        for p in problems(src.as_ref(), rate) {
            failures.push(format!("{rate}, frames {late}/90000 s late: {p}"));
        }
    }
    assert!(failures.is_empty(), "mov:\n{}", failures.join("\n"));
}

#[test]
fn mp4_with_frames_1ms_after_the_grid_shows_every_frame_once() {
    check_90k_late(90);
}

#[test]
fn mp4_with_frames_on_the_grid_shows_every_frame_once() {
    check_90k_late(0);
}

/// An MP4 with these sample durations and track timescale.
fn mp4_90k(durations: &[u32], timescale: u32) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), timescale)).unwrap();
    for (k, &duration) in durations.iter().enumerate() {
        w.write_sample(t, WriteSample { data: &picture(k), duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// A constant-rate clip whose every picture time was rounded to a whole millisecond (issue #1700):
/// at a fine track timescale the frames alternate around the grid and the median duration is the
/// long one. The reported rate and the frame grid both come back to the millisecond-round times:
/// every picture plays once at its standard rate.
#[test]
fn mp4_ms_rounded_frame_times_get_back_to_their_rate() {
    let cases: [(&str, Vec<u32>); 2] = [
        // 120 frames over 4 s, alternating 33 and 34 ms.
        ("30 fps", (0..120).map(|k: u64| ((k + 1) * 90_000 / 30 - k * 90_000 / 30) as u32).collect()),
        // 180 frames over 3 s, alternating 1501 and 1502.
        ("59.94 fps", (0..180).map(|k: u64| ((k + 1) * 1_501_500 / 1000 - k * 1_501_500 / 1000) as u32).collect()),
    ];
    let mut failures = Vec::new();
    for (case_index, (name, durations)) in cases.into_iter().enumerate() {
        let src = crate::open_bytes("synthetic.mov", mp4_90k(&durations, 90_000)).unwrap();
        let want = [FrameRate::FPS_30, FrameRate::FPS_59_94];
        let got = src.info().video.as_ref().map(|v| v.frame_rate);
        if got != Some(want[case_index]) {
            failures.push(format!("{name}: reported {got:?}, want {:?}", want[case_index]));
        }
        let rate = want[case_index];
        for p in problems(src.as_ref(), rate) {
            failures.push(format!("{name}: {p}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures[..failures.len().min(8)].join("\n"));
}

/// Files that are not a constant rate gone slightly off its grid keep their own times: whatever is
/// asked (each frame's start, the unit before it, every 30 fps grid point), the frame shown is the
/// last one starting at or before it.
#[test]
fn mp4_variable_rate_and_on_grid_frame_times_stay_put() {
    let cases: [(&str, Vec<u32>); 5] = [
        // 1/30 s and 1/20 s in turn
        ("alternating", (0..60).map(|k| if k % 2 == 0 { 3000 } else { 4500 }).collect()),
        // 30 fps, then every frame 0.4 frame later (one frame lasts 1.4 frames)
        ("shifted", (0..80).map(|k| if k == 40 { 4200 } else { 3000 }).collect()),
        // 30 fps with a frame shown for a fifth of a frame: it and the next are both near one grid point
        (
            "stutter",
            (0..80)
                .map(|k| match k {
                    40 => 600,
                    41 => 5400,
                    _ => 3000,
                })
                .collect(),
        ),
        // 30 fps with two frames by one grid point, one a unit after it and one five units after
        (
            "one unit and five after one point",
            (0..80)
                .map(|k| match k {
                    39 => 3001,
                    40 => 4,
                    41 => 2995,
                    _ => 3000,
                })
                .collect(),
        ),
        // 59.94 fps on its grid, stamps truncated to the timescale (1501, 1502, …)
        ("59.94 truncated", (0..120).map(|k: u64| ((k + 1) * 1_501_500 / 1000 - k * 1_501_500 / 1000) as u32).collect()),
    ];
    let mut failures = Vec::new();
    for (name, durations) in cases {
        let src = crate::open_bytes("synthetic.mov", mp4_90k(&durations, 90_000)).unwrap();
        let starts: Vec<i64> = durations.iter().scan(0i64, |t, &d| Some(std::mem::replace(t, *t + i64::from(d)))).collect();
        let end = *starts.last().unwrap();
        let mut asked: Vec<i64> = starts.iter().flat_map(|&s| [s, s - 1]).filter(|&s| s >= 0).collect();
        asked.extend((0..).map(|k| k * 3000).take_while(|&t| t <= end));
        for t in asked {
            let want = starts.iter().rposition(|&s| s <= t).unwrap();
            let got = index_of(&src.video_frame(FrameRequest::full(Tick::from_rational(t, 1, 90_000))).unwrap());
            if got != want {
                failures.push(format!("{name}: at {t}/90000 s wanted frame {want}, got {got}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures[..failures.len().min(8)].join("\n"));
}
