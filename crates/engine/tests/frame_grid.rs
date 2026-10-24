//! An export asks a clip's media for frames on its output frame grid only: 60 and 59.94 fps H.264
//! with B-frames in a 30 fps export leaves out the non-reference pictures between the exported
//! frames, and every exported frame is the one the Program monitor shows (ffmpeg-made media).
//!
//! A binary of its own: it reads the process-wide decode counters (`filmcraft_codecs::gop_stats`),
//! which other tests decoding video in the same process would add to.

use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

use filmcraft_engine::Session;
use filmcraft_project::ItemId;
use filmcraft_time::Tick;

/// Two seconds of 160 x 90 H.264 at `rate` in one GOP, B-frames placed by `x264` options.
fn fixture(name: &str, rate: &str, x264: &str) -> Option<PathBuf> {
    let ffmpeg = filmcraft_testkit::oracle::ffmpeg_or_skip("frame grid")?;
    let path = filmcraft_testkit::fixtures_dir("engine/frame_grid").join(name);
    let src = format!("testsrc2=size=160x90:rate={rate}:duration=2");
    filmcraft_testkit::fixtures::generate(&path, |tmp| {
        Command::new(&ffmpeg)
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &src])
            .args(["-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv420p", "-x264-params", x264])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A 30 fps sequence the size of the media, with `file` overwritten at 0.
fn session(file: &std::path::Path) -> Session {
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [file.to_string_lossy()]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    s.execute("file.newSequence", json!({"name": "S", "width": 160, "height": 90, "fps": 30})).unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    s.execute("source.overwrite", json!({})).unwrap();
    s
}

/// Exports 1.5 s of `file` (45 frames) and checks them against the monitor, that the export decoded
/// from the start once (a sample left out that a frame then asks for costs a restart) and that it
/// left out at least `min_skipped` samples.
fn exported_frames_match_the_monitor(file: &std::path::Path, min_skipped: u64) {
    let dir = filmcraft_testkit::workspace_root().join("target").join("export-tests").join(format!("engine-frame-grid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("f.png").to_string_lossy().to_string();
    let mut s = session(file);
    let before = filmcraft_codecs::gop_stats();
    s.execute("file.exportMedia", json!({"path": out, "format": "png", "range": "custom", "startSeconds": 0, "endSeconds": 1.5, "wait": true})).unwrap();
    let stats = filmcraft_codecs::gop_stats() - before;
    // The monitor decodes in a session of its own (its own decoder and cache, no frame grid).
    let monitor = session(file);
    let count = 45u64;
    for j in 0..count {
        let path = filmcraft_export::image_sequence_path(&out, j, count);
        let exported = filmcraft_testkit::golden::read_png(std::path::Path::new(&path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let shown = monitor.render_program_at(1.0, Tick::from_rational(j as i64, 1, 30)).expect("monitor frame").over_black_rgba8();
        assert!(exported.px == shown, "{}: exported frame {j} differs from the monitor's", file.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(stats.seeks, 1, "{}: {stats:?}", file.display());
    assert!(stats.skipped >= min_skipped, "{}: {} samples left out", file.display(), stats.skipped);
}

#[test]
fn a_30_fps_export_of_60_fps_media_skips_the_non_reference_pictures_between_its_frames() {
    // every B-frame non-reference: samples 0, 2, … 88 are exported, the odd ones between them are
    // the B-frames (44)
    let Some(plain) = fixture("b1_no_pyramid.mp4", "60", "keyint=120:min-keyint=120:scenecut=0:bframes=1:b-adapt=0:b-pyramid=none") else { return };
    exported_frames_match_the_monitor(&plain, 40);
    // the middle B-frame of three is a reference (x264's default pyramid): the odd ones are not
    let Some(pyramid) = fixture("b3_pyramid.mp4", "60", "keyint=120:min-keyint=120:scenecut=0:bframes=3:b-adapt=0:b-pyramid=normal") else { return };
    exported_frames_match_the_monitor(&pyramid, 40);
    // 59.94 fps: the exported samples drift off the even ones, and the edit list delays every
    // picture by the B-frames' reorder (a sample is presented after its pts); four non-reference
    // B-frames per P-frame, 4 of every 5 samples between the exported ones (~36)
    let Some(ntsc) = fixture("b4_5994.mp4", "60000/1001", "keyint=120:min-keyint=120:scenecut=0:bframes=4:b-adapt=0:b-pyramid=none") else { return };
    exported_frames_match_the_monitor(&ntsc, 30);
}
