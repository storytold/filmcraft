//! Seeking in H.264 files whose samples are IDR pictures decodes only the requested picture
//! (`VideoDecoder::is_independent`), and every picture is the one ffmpeg decodes (bit for bit),
//! whatever the order of the seeks. Streams with P / B pictures or open GOPs (non-IDR intra
//! pictures) keep the decode-forward path and its exact pixels. ffmpeg is the fixture generator and
//! the pixel oracle only.
//!
//! One test function: the decode counters (`gop_stats`) are process-wide.

#[path = "it/common/mod.rs"]
mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::*;
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

struct Case {
    name: &'static str,
    size: (usize, usize),
    /// every sample is an IDR picture: seeks decode one picture each
    idr_only: bool,
    /// `yuvj420p` (full range) instead of `yuv420p`
    full_range: bool,
    /// out-of-order seeks must match ffmpeg too. Open GOPs: false, see `run_case`
    strict_order: bool,
    /// ffmpeg arguments after the input source
    args: &'static [&'static str],
}

/// Presentation times (seconds) of the frames of the video stream, from ffprobe.
fn frame_times(file: &Path) -> Vec<f64> {
    let Some(probe) = filmcraft_testkit::ffprobe() else { return Vec::new() };
    let o = std::process::Command::new(probe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts_time", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .expect("run ffprobe");
    assert!(o.status.success(), "ffprobe: {}", String::from_utf8_lossy(&o.stderr));
    let mut t: Vec<f64> = String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().trim_end_matches(',').parse().ok()).collect();
    t.sort_by(|a, b| a.total_cmp(b));
    t
}

fn at(seconds: f64) -> Tick {
    // 2 ms into the frame: ffprobe prints microseconds, a frame lasts 16 ms or more
    Tick((seconds * TICKS_PER_SECOND as f64).round() as i64 + TICKS_PER_SECOND / 500)
}

fn seek(src: &SharedSource, t: Tick) -> filmcraft_frame::VideoFrame {
    let f = src.video_frame(FrameRequest::full(t)).expect("frame");
    (*f).clone()
}

struct Reference {
    raw: Vec<u8>,
    w: usize,
    h: usize,
    times: Vec<f64>,
}

impl Reference {
    fn planes(&self, j: usize) -> Vec<Vec<u16>> {
        let (w, h) = (self.w, self.h);
        let fsize = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        raw_planes(&self.raw[j * fsize..(j + 1) * fsize], w, h, w.div_ceil(2), h.div_ceil(2), 1)
    }
}

fn run_case(ff: &Path, c: &Case) {
    let file_name = format!("idrseek_{}.mp4", c.name);
    let (w, h) = c.size;
    let input = format!("testsrc2=s={w}x{h}:r=30:d=3,noise=alls=10:allf=t");
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &input];
    args.extend_from_slice(c.args);
    let Some(file) = fixture(ff, &file_name, &args) else { panic!("{}: ffmpeg could not make the fixture", c.name) };
    let pix = if c.full_range { "yuvj420p" } else { "yuv420p" };
    let raw = ffmpeg_frames(ff, &file, pix);
    let times = frame_times(&file);
    let n = raw.len() / (w * h + 2 * w.div_ceil(2) * h.div_ceil(2));
    assert!(n >= 40 && times.len() == n, "{}: {n} frames, {} times", c.name, times.len());
    let r = Reference { raw, w, h, times };
    let check = |f: &filmcraft_frame::VideoFrame, j: usize, what: &str| {
        assert_eq!((f.width as usize, f.height as usize), (w, h), "{}: {what}: size", c.name);
        assert_eq!(max_diff(&planes(f), &r.planes(j)), 0, "{}: frame {j} ({what}) differs from ffmpeg", c.name);
    };

    // seeks: first, last, middle, out of order, repeated, then pseudo-random ones. With open GOPs
    // (`strict_order` false) the pixels of these are not compared: at HEAD, without this change,
    // a leading picture seeked after other frames are cached can come back as the wrong frame.
    let mut order = vec![0, n - 1, n / 2, 3, n / 2, n - 2, 1, n / 3, n - 1, 0];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    order.extend((0..24).map(|_| rng.below(n as u64) as usize));
    let src = filmcraft_codecs::open_bytes(&file_name, bytes(&file)).expect("open");
    let g0 = filmcraft_codecs::gop_stats();
    let mut seen = std::collections::HashSet::new();
    let mut multi = 0;
    for &j in &order {
        let before = filmcraft_codecs::gop_stats();
        let f = seek(&src, at(r.times[j]));
        let d = filmcraft_codecs::gop_stats() - before;
        if c.strict_order {
            check(&f, j, "seek");
        }
        if c.idr_only {
            let first = seen.insert(j);
            assert!(
                d.decoded <= 1 && d.frames <= 1 && d.seeks == 0,
                "{}: seek to {j}: {} samples fed, {} pictures, {} decoder seeks",
                c.name,
                d.decoded,
                d.frames,
                d.seeks
            );
            assert_eq!(d.decoded, first as u64, "{}: frame {j}: decoded once, then cached", c.name);
        } else {
            multi += (d.decoded > 1) as usize;
        }
    }
    let total = filmcraft_codecs::gop_stats() - g0;
    if c.idr_only {
        assert_eq!(total.decoded, seen.len() as u64, "{}: one picture decoded per distinct frame", c.name);
    } else {
        assert!(multi > 0, "{}: a stream with inter pictures still decodes forward from its sync samples", c.name);
    }

    // every frame seeked cold on a source of its own (no cached neighbour, no running decoder)
    if !c.idr_only {
        for j in 0..n {
            let src = filmcraft_codecs::open_bytes(&file_name, bytes(&file)).expect("open");
            check(&seek(&src, at(r.times[j])), j, "cold seek");
        }
    }

    // playback: every frame in order on a fresh source, then back to the start
    let src = filmcraft_codecs::open_bytes(&file_name, bytes(&file)).expect("open");
    let g1 = filmcraft_codecs::gop_stats();
    for j in (0..n).chain(0..3) {
        check(&seek(&src, at(r.times[j])), j, "sequential");
    }
    let d = filmcraft_codecs::gop_stats() - g1;
    if c.idr_only {
        assert!(d.decoded as usize <= n, "{}: {} samples fed for {n} frames", c.name, d.decoded);
    } else {
        assert!(d.seeks <= 3 && (d.decoded as usize) < n + 80, "{}: playback decoded {} samples with {} seeks for {n} frames", c.name, d.decoded, d.seeks);
    }

    if c.idr_only {
        // cancelled: no picture is decoded and the next request is served
        let src = filmcraft_codecs::open_bytes(&file_name, bytes(&file)).expect("open");
        let flag = Arc::new(AtomicBool::new(true));
        let g = filmcraft_codecs::gop_stats();
        let res = filmcraft_media::cancel::with_cancel(&flag, || src.video_frame(FrameRequest::full(at(r.times[n / 2]))));
        assert!(res.is_err(), "{}: cancelled request", c.name);
        assert_eq!((filmcraft_codecs::gop_stats() - g).decoded, 0, "{}: nothing decoded for a cancelled request", c.name);
        check(&seek(&src, at(r.times[n / 2])), n / 2, "after a cancelled request");

        // several seeks at once, each on a decoder of its own
        let src = filmcraft_codecs::open_bytes(&file_name, bytes(&file)).expect("open");
        let g = filmcraft_codecs::gop_stats();
        let per = 12usize;
        std::thread::scope(|scope| {
            for t in 0..4usize {
                let (src, r, check) = (&src, &r, &check);
                scope.spawn(move || {
                    for k in 0..per {
                        let j = (k * 5 + t * per * 5 + 1) % n;
                        check(&seek(src, at(r.times[j])), j, "concurrent seek");
                    }
                });
            }
        });
        let d = filmcraft_codecs::gop_stats() - g;
        assert!(d.decoded as usize <= 4 * per && d.seeks == 0, "{}: {} samples fed for {} concurrent seeks", c.name, d.decoded, 4 * per);
    }
}

#[test]
fn h264_seeks_decode_only_the_requested_idr_picture_and_match_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let cases = [
        Case {
            name: "idr_only",
            size: (320, 240),
            idr_only: true,
            full_range: false,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p"],
        },
        Case {
            name: "idr_keyint1",
            size: (320, 240),
            idr_only: true,
            full_range: false,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "keyint=1:scenecut=0", "-pix_fmt", "yuv420p"],
        },
        Case {
            name: "idr_full_range",
            size: (322, 242),
            idr_only: true,
            full_range: true,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-g", "1", "-pix_fmt", "yuvj420p", "-color_range", "pc"],
        },
        Case {
            name: "idr_1080p",
            size: (1920, 1080),
            idr_only: true,
            full_range: true,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "ultrafast", "-g", "1", "-pix_fmt", "yuvj420p", "-color_range", "pc", "-frames:v", "60"],
        },
        Case {
            name: "idr_vfr_holes",
            size: (320, 240),
            idr_only: true,
            full_range: false,
            strict_order: true,
            args: &["-vf", "select='not(eq(mod(n\\,7)\\,3))'", "-fps_mode", "vfr", "-c:v", "libx264", "-preset", "veryfast", "-g", "1", "-pix_fmt", "yuv420p"],
        },
        Case {
            name: "gop_with_b_frames",
            size: (320, 240),
            idr_only: false,
            full_range: false,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "bframes=3:b-pyramid=normal:keyint=30:scenecut=0", "-pix_fmt", "yuv420p"],
        },
        Case {
            name: "open_gop",
            size: (320, 240),
            idr_only: false,
            full_range: false,
            strict_order: false,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "open-gop=1:bframes=2:keyint=20:min-keyint=20:scenecut=0", "-pix_fmt", "yuv420p"],
        },
        Case {
            name: "idr_and_p",
            size: (320, 240),
            idr_only: false,
            full_range: false,
            strict_order: true,
            args: &["-c:v", "libx264", "-preset", "veryfast", "-g", "30", "-bf", "0", "-pix_fmt", "yuv420p"],
        },
    ];
    for c in &cases {
        run_case(&ff, c);
    }
    interlaced_idr_samples_are_not_decoded_alone(&ff);
}

/// With field coding (`frame_mbs_only_flag` 0) a sample can be one field of a frame, so an IDR
/// sample is not taken to be a whole picture: seeks keep the decode-forward path.
fn interlaced_idr_samples_are_not_decoded_alone(ff: &Path) {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=320x240:r=30:d=2",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-flags",
        "+ilme+ildct",
        "-x264-params",
        "interlaced=1:tff=1:keyint=1:scenecut=0",
        "-pix_fmt",
        "yuv420p",
    ];
    let Some(file) = fixture(ff, "idrseek_interlaced.mp4", &args) else {
        eprintln!("SKIPPED: ffmpeg could not make an interlaced H.264 fixture");
        return;
    };
    // the first sample of the file: the IDR picture
    let data = std::fs::read(&file).expect("read");
    let mp4 = filmcraft_isobmff::open(data.clone()).expect("mp4");
    let track = mp4.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video track");
    let sample = mp4.read_sample(&data, track, 0).expect("sample");
    let dec = filmcraft_codecs::software_video_decoder(&mp4.tracks[track].entries[0]).expect("decoder");
    assert!(!dec.is_independent(&sample), "an IDR sample of a field-coded stream is not decoded alone");
}
