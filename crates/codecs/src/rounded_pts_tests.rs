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

/// An MP4 at 90 kHz with these sample durations.
fn mp4_90k(durations: &[u32]) -> Arc<[u8]> {
    mp4_durations(90_000, durations)
}

/// An MP4 at timescale `ts` with the given sample durations.
fn mp4_durations(ts: u32, durations: &[u32]) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), ts)).unwrap();
    for (k, &duration) in durations.iter().enumerate() {
        w.write_sample(t, WriteSample { data: &picture(k), duration, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// A clip whose frame times were rounded to whole milliseconds inside a 90 kHz track (#1700). A
/// 30 fps clip's durations are 2970 and 3060 units in turn, and a 60 fps clip's 1440 and 1530; the
/// old median reads the first as 30.303 fps and the second as 58.824 fps. Both mean to 3000/1500.
fn mp4_ms_rounded(rate: FrameRate) -> Arc<[u8]> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), 90_000)).unwrap();
    let n = frame_count(rate);
    let nominal_scale = i128::from(rate.num) / i128::from(rate.den);
    let grid = 90_000 / nominal_scale as i64;
    let stamp = |k: usize| ((2 * k as i64 * grid + 90) / 180) * 90;
    for k in 0..n {
        w.write_sample(t, WriteSample { data: &picture(k), duration: (stamp(k + 1) - stamp(k)) as u32, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// A phone-style 90 kHz track with whole-millisecond frame times keeps its rate and every frame.
#[test]
fn mp4_ms_rounded_frame_times_get_back_to_a_standard_rate() {
    let mut failures = Vec::new();
    for rate in [FrameRate::FPS_30, FrameRate::FPS_60] {
        let src = crate::open_bytes("synthetic.mov", mp4_ms_rounded(rate)).unwrap();
        let got = src.info().video.as_ref().map(|v| v.frame_rate);
        if got != Some(rate) {
            failures.push(format!("{rate}: reads as {got:?}"));
        }
        failures.extend(problems(src.as_ref(), rate).into_iter().map(|p| format!("{rate}: {p}")));
    }
    assert!(failures.is_empty(), "mov:\n{}", failures.join("\n"));
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
        let src = crate::open_bytes("synthetic.mov", mp4_90k(&durations)).unwrap();
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

/// Asks for every frame at its start, its middle and nine tenths in, expects that frame each time.
fn inside_frame_problems(src: &dyn MediaSource, rate: FrameRate) -> Vec<String> {
    let frame = rate.frame_duration().0;
    let mut wrong = Vec::new();
    for k in 0..frame_count(rate) {
        for tenths in [0, 5, 9] {
            let t = rate.tick_of(k as i64) + Tick(frame / 10 * tenths);
            let got = index_of(&src.video_frame(FrameRequest::full(t)).unwrap());
            if got != k {
                wrong.push(format!("frame {k} + {tenths}/10: got {got}"));
            }
        }
    }
    wrong
}

/// An MP4 whose timescale is `per_frame` units per frame of `rate` (an integer rate), every sample
/// that many units long.
fn mp4_units_per_frame(rate: FrameRate, per_frame: u32) -> Arc<[u8]> {
    let ts = (rate.num / rate.den) as u32 * per_frame;
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let t = w.add_track(TrackConfig::new(SampleEntry::jpeg((BITS * BLOCK) as u16, BLOCK as u16), ts)).unwrap();
    for k in 0..frame_count(rate) {
        w.write_sample(t, WriteSample { data: &picture(k), duration: per_frame, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// An MP4 muxed with timescale = frame rate (one unit per frame, as penumbra writes its recordings):
/// half a unit is half a frame, so rounding to the nearest unit showed the next frame from every
/// frame's middle on. Two units per frame had the same problem from three quarters in.
#[test]
fn mp4_with_a_timescale_of_one_or_two_units_per_frame_finds_the_frame_inside_it() {
    let mut failures = Vec::new();
    for rate in [FrameRate::FPS_24, FrameRate::FPS_25, FrameRate::FPS_30, FrameRate::FPS_60] {
        for per_frame in [1, 2] {
            let src = crate::open_bytes("synthetic.mov", mp4_units_per_frame(rate, per_frame)).unwrap();
            for p in inside_frame_problems(src.as_ref(), rate).into_iter().take(3) {
                failures.push(format!("{rate}, {per_frame} units per frame: {p}"));
            }
        }
    }
    assert!(failures.is_empty(), "mov:\n{}", failures.join("\n"));
}

/// A Matroska file whose `TimestampScale` is one frame of `rate` in whole nanoseconds, `scale_ns`
/// (rounded down or up where a frame is no whole number of them).
fn mkv_scale(rate: FrameRate, scale_ns: u64) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MJPEG");
    spec.video_size = Some((BITS * BLOCK, BLOCK));
    spec.default_duration_ns = Some(scale_ns);
    let opts = MuxOptions { timestamp_scale: scale_ns, ..MuxOptions::default() };
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], opts).unwrap();
    for k in 0..frame_count(rate) {
        let ns = (k as i128 * 1_000_000_000 * rate.den as i128 / rate.num as i128) as i64;
        w.write_frame(0, ns, true, &picture(k), None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// The same for Matroska with a timestamp unit of one frame: exact at 25 fps, a fraction of a
/// nanosecond short of or past the frame at 24, 30 and 60 fps.
#[test]
fn mkv_with_a_timestamp_unit_of_one_frame_finds_the_frame_inside_it() {
    let mut failures = Vec::new();
    for rate in [FrameRate::FPS_24, FrameRate::FPS_25, FrameRate::FPS_30, FrameRate::FPS_60] {
        let exact = 1_000_000_000 * rate.den as u64;
        let floor = exact / rate.num as u64;
        let ceil = exact.div_ceil(rate.num as u64);
        for scale_ns in if floor == ceil { vec![floor] } else { vec![floor, ceil] } {
            let src = crate::open_bytes("synthetic.mkv", mkv_scale(rate, scale_ns)).unwrap();
            for p in inside_frame_problems(src.as_ref(), rate).into_iter().take(3) {
                failures.push(format!("{rate}, {scale_ns} ns units: {p}"));
            }
        }
    }
    assert!(failures.is_empty(), "mkv:\n{}", failures.join("\n"));
}

/// A Matroska file with `TimestampScale` `scale_ns` and a frame at each of `starts` (units of it).
fn mkv_starts(scale_ns: u64, starts: &[i64]) -> Arc<[u8]> {
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MJPEG");
    spec.video_size = Some((BITS * BLOCK, BLOCK));
    let opts = MuxOptions { timestamp_scale: scale_ns, ..MuxOptions::default() };
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], opts).unwrap();
    for (k, &s) in starts.iter().enumerate() {
        w.write_frame(0, s * scale_ns as i64, true, &picture(k), None).unwrap();
    }
    w.finish().unwrap().into_inner().into()
}

/// A coarse unit is no constant rate: at two units a frame (timescale 50, nominal 25 fps), a track
/// with one sample a unit long has a frame starting between two nominal frames. Moving a request
/// at that start back to its nominal frame would show the frame before; such a track keeps the
/// lookup by the nearest unit, in both containers.
#[test]
fn variable_rate_track_with_a_coarse_unit_keeps_its_off_grid_frame() {
    let durations = [2u32, 1, 2, 2, 2, 2, 2, 2];
    let starts: Vec<i64> = durations.iter().scan(0i64, |t, &d| Some(std::mem::replace(t, *t + i64::from(d)))).collect();
    let sources = [
        ("mov", crate::open_bytes("synthetic.mov", mp4_durations(50, &durations)).unwrap()),
        ("mkv", crate::open_bytes("synthetic.mkv", mkv_starts(20_000_000, &starts)).unwrap()),
    ];
    let mut failures = Vec::new();
    for (name, src) in &sources {
        assert_eq!(src.info().video.as_ref().unwrap().frame_rate, FrameRate::FPS_25, "{name}: nominal rate");
        // every unit up to the last frame: the last frame starting at or before it
        for t in 0..=*starts.last().unwrap() {
            let want = starts.iter().rposition(|&s| s <= t).unwrap();
            let got = index_of(&src.video_frame(FrameRequest::full(Tick::from_rational(t, 1, 50))).unwrap());
            if got != want {
                failures.push(format!("{name}: at {t}/50 s wanted frame {want}, got {got}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
