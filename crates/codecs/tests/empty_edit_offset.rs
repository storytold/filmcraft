//! An MP4 whose video edit list starts with an empty edit (a start offset, e.g. from
//! `-output_ts_offset`) must present each picture at its edit-adjusted time, not one frame early.
//!
//! Repro from #1276: `testsrc` prints each frame's own generation time on the picture, so the
//! rendered frame identifies itself. The file's edit list is `[53 ms empty, then media from the
//! B-frame skip]`; ffprobe reports `start_time ≈ 0.053` and frame 0 presented at 0.052979.
//! Sequence time 2.1042 s (inside frame 50 of a Sequence From Clip at 24 fps) must show the
//! picture ffprobe places on screen there — before the fix the source answered one frame early.

mod common;

use common::*;

use filmcraft_media::FrameRequest;
use filmcraft_time::Tick;

/// The issue's exact generator: 24 fps testsrc, B-frames, a 53 ms leading empty edit.
fn late_fixture(ff: &std::path::Path) -> Option<std::path::PathBuf> {
    fixture(
        ff,
        "empty_edit_offset.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=24:decimals=3",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "48",
            "-bf",
            "2",
            "-output_ts_offset",
            "0.0533",
        ],
    )
}

/// Presentation (edit-applied) pts_time of every frame, in order, per ffprobe.
fn presentation_times(f: &std::path::Path) -> Vec<f64> {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts_time", "-of", "csv=p=0", f.to_str().unwrap()])
        .output()
        .expect("ffprobe frames");
    let log = String::from_utf8_lossy(&o.stdout);
    log.lines().filter_map(|l| l.trim_end_matches(',').trim().parse::<f64>().ok()).collect()
}

#[test]
fn leading_empty_edit_presents_at_the_edit_adjusted_time() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(f) = late_fixture(&ff) else {
        eprintln!("SKIPPED: ffmpeg without libx264");
        return;
    };
    let src = filmcraft_codecs::open_bytes("empty_edit_offset.mp4", bytes(&f)).unwrap();
    let times = presentation_times(&f);
    assert!(times.len() > 60, "the fixture presents many frames");

    // The clip's first presented picture sits at the file's start time (the leading empty edit).
    let start = *times.first().unwrap();
    assert!((start - 0.053).abs() < 0.005, "the fixture starts ~53 ms in: {start}");

    // The issue's probe: sequence time 2.1042 s (frame 50's window in a 24 fps Sequence From
    // Clip). Sequence time 0 is the clip's first picture, so the file presentation time is
    // 2.1042 + start. ffprobe's own answer: the last frame whose pts_time is <= that.
    let at = Tick::from_rational(21042, 1, 10_000);
    let pres = 2.1042 + start;
    let want_idx = times.iter().rposition(|&p| p <= pres + 1e-6).unwrap();
    // testsrc labels each frame with its generation time; the rendered picture must be the frame
    // ffprobe presents there — ffmpeg's own decode of the file (edit applied, timestamps rebased:
    // frame `want_idx` of it).
    let raw = ffmpeg_frames(&ff, &f, "yuv420p");
    let frame_bytes = 640 * 360 * 3 / 2;
    assert!(raw.len() >= (want_idx + 1) * frame_bytes, "ffmpeg decodes the wanted presented frame");
    let want = raw_planes(&raw[want_idx * frame_bytes..(want_idx + 1) * frame_bytes], 640, 360, 320, 180, 1);
    let got = src.video_frame(FrameRequest::full(at)).unwrap();
    let got_planes = planes(&got);
    if got_planes != want {
        // Which of ffmpeg's presented frames did we actually render? (Report the index, not 3 MB.)
        let hit = (0..raw.len() / frame_bytes).find(|&i| got_planes == raw_planes(&raw[i * frame_bytes..(i + 1) * frame_bytes], 640, 360, 320, 180, 1));
        panic!("the picture at sequence 2.1042 s (file presentation {pres:.4} s) must be ffmpeg's presented frame {want_idx}, got frame {hit:?}");
    }
}
