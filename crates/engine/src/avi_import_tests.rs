//! #598: AVI files import (ffmpeg-made, skipped without it): the Project item describes the
//! streams, a sequence made from it plays picture and sound, and the Media Browser's streaming
//! probe opens it without reading it whole.

use serde_json::json;

use crate::Session;
use filmcraft_project::ItemId;

fn fixture() -> Option<std::path::PathBuf> {
    let ffmpeg = filmcraft_testkit::oracle::ffmpeg_or_skip("avi import")?;
    let path = filmcraft_testkit::fixtures_dir("engine/avi").join("h264_mp3.avi");
    filmcraft_testkit::fixtures::generate(&path, |tmp| {
        std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=2",
            ])
            .args(["-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p", "-c:a", "libmp3lame", "-f", "avi"])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[test]
fn avi_imports_plays_and_probes() {
    let Some(path) = fixture() else { return };
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert!(r["errors"].as_array().is_none_or(|e| e.is_empty()), "{r}");
    let item = ItemId(r["items"][0].as_u64().unwrap());
    let info = s.project.item(item).unwrap().as_media().unwrap().info.clone();
    assert_eq!(info.container, "AVI");
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.codec.as_str()), (160, 120, "H.264"));
    assert_eq!(info.audio_streams.first().map(|a| a.codec.as_str()), Some("MP3"));
    assert!((info.duration.seconds() - 2.0).abs() < 0.1, "{}", info.duration.seconds());
    // a sequence from the clip has its picture and sound
    s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    let q = s.active_sequence().unwrap().clone();
    assert!(!q.video_tracks[0].items.is_empty() && !q.audio_tracks[0].items.is_empty());
    let src = s.source(item).unwrap();
    assert!(src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick(filmcraft_time::TICKS_PER_SECOND))).is_ok());
    assert!(src.audio(48_000, 4_800, 48_000).unwrap().channels[0].iter().any(|v| v.abs() > 0.05));
    // the Media Browser's probe streams it (index now, frames on demand)
    let probed = s.media.probe_file(&path.to_string_lossy(), &*s.services).unwrap();
    assert_eq!(probed.info().container, "AVI");
}
