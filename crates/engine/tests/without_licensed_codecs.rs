//! An engine built without the `h264`, `hevc`, `aac` and `prores` features: exporting those
//! formats fails with an error that names the missing feature, and the other formats still export.
//!
//! `cargo test -p filmcraft-engine --no-default-features --test without_licensed_codecs`

#![cfg(not(any(feature = "h264", feature = "hevc", feature = "aac", feature = "prores")))]

use filmcraft_engine::Session;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn out(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("filmcraft-without-licensed-codecs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().to_string()
}

#[test]
fn left_out_export_formats_name_their_feature() {
    let mut s = demo();
    for (format, feature) in [("h264", "h264"), ("prores", "prores")] {
        let params = json!({"format": format, "path": out(format), "range": "custom", "startSeconds": 1.0, "endSeconds": 1.25, "wait": true});
        let e = s.execute("file.exportMedia", params).expect_err(format);
        assert!(e.to_string().contains(&format!("`{feature}`")), "{format}: {e}");
    }
    assert!(!filmcraft_export::Format::H264.has_builtin_encoder());
    assert!(!filmcraft_export::Format::ProRes.has_builtin_encoder());
}

#[test]
fn other_formats_still_export() {
    let mut s = demo();
    let params = json!({"format": "mjpeg", "path": out("mjpeg"), "range": "custom", "startSeconds": 1.0, "endSeconds": 1.25, "wait": true});
    let r = s.execute("file.exportMedia", params).unwrap();
    let path = r["path"].as_str().unwrap();
    assert!(std::fs::metadata(path).unwrap().len() > 0, "{path}");
}
