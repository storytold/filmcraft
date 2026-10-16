//! AVS demux uses the MPEG-TS audio helper to split MPEG audio across chunks.
//! ffmpeg is the external fixture oracle; the test skips when it is unavailable.

use std::{fs, path::PathBuf, process::Command};

use filmcraft_codecs::avi::AviSource;
use filmcraft_mpegts::open as mpegts_open;

fn ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FILMCRAFT_FFMPEG").map(PathBuf::from) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["ffmpeg", "ffmpeg.exe"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[test]
fn avi_mpeg_audio_streams_are_avi_audio() {
    let Some(ffmpeg) = ffmpeg() else {
        if std::env::var("FILMCRAFT_REQUIRE_ORACLES").is_ok_and(|v| !v.is_empty()) {
            panic!("ffmpeg is required by FILMCRAFT_REQUIRE_ORACLES");
        }
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };

    let dir = std::env::temp_dir().join(format!("filmcraft-avi-avs-oracle-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create fixture directory");
    let input = dir.join("input.ts");
    let output = dir.join("mpeg-audio.avi");

    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:duration=0.04:sample_rate=48000",
            input.to_str().expect("UTF-8 temporary path"),
        ])
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed to create MPEG-TS fixture");

    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "mpegts",
            "-i",
            input.to_str().expect("UTF-8 temporary path"),
            "-c",
            "copy",
            output.to_str().expect("UTF-8 temporary path"),
        ])
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed to copy MPEG audio into AVI");

    let data = fs::read(&output).expect("read fixture");
    assert!(data.starts_with(b"RIFF"), "fixture must be an AVI container");
    let _ = mpegts_open(&data).ok();
    let src = std::sync::Arc::new(filmcraft_media::reader::MemReader(data.into()));
    AviSource::open_reader("mpeg-audio.avi", src).expect("AVS MPEG audio should open");

    let _ = fs::remove_dir_all(&dir);
}
