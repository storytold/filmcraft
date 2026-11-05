//! A file with an extension FilmCraft does not know imports through a `format` plug-in. Real host,
//! and the plug-in is `tests/fixtures/raw-rgb.wat`: a reader and writer of a 12 byte header raw RGB
//! format, written in WAT so the test needs no built binary.

mod common;

use filmcraft_engine::Session;
use filmcraft_media::FrameRequest;
use filmcraft_plugins::{HostConfig, PluginHost};
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::Tick;
use serde_json::json;

const RAW: &str = "org.example.raw-rgb";

/// The raw RGB plug-in as a package. It asks for `fs.picked` and nothing else.
fn raw_package() -> Vec<u8> {
    let manifest = json!({
        "schema": 1, "id": RAW, "name": "Raw RGB image", "version": "1.0.0",
        "abi": { "craft": 1 },
        "permissions": { "required": ["fs.picked"] },
        "limits": { "memory_mb": 64 },
        "contributions": [{ "kind": "format", "id": "rgbraw", "entry": 0, "extensions": ["rgbraw"] }]
    });
    common::package(&common::fixture_text("raw-rgb"), &manifest)
}

fn rgb_file(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut f = b"RGB8".to_vec();
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&h.to_le_bytes());
    f.extend_from_slice(rgb);
    f
}

fn import(session: &mut Session, path: &std::path::Path) -> Result<serde_json::Value, filmcraft_engine::EngineError> {
    session.execute("file.import", json!({"paths": [path]}))
}

#[test]
fn an_unknown_extension_imports_through_the_plugin_only_while_it_is_installed_and_granted() {
    let dir = std::env::temp_dir().join(format!("fc-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pic.rgbraw");
    std::fs::write(&file, rgb_file(2, 1, &[10, 20, 30, 40, 50, 60])).unwrap();

    let mut session = Session::default();
    let host = PluginHost::start(&mut session, HostConfig::default()).unwrap();

    // Before the install nobody knows the extension.
    assert!(import(&mut session, &file).is_err(), "FilmCraft alone cannot open .rgbraw");

    common::install(&host, &raw_package());
    assert!(host.surfaces.formats.handles("rgbraw"));

    let r = import(&mut session, &file).unwrap();
    let id = ItemId(r["items"][0].as_u64().unwrap());
    let Some(ItemKind::Media(clip)) = session.project.item(id).map(|i| &i.kind) else { panic!("not a media item") };
    let v = clip.info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (2, 1));
    let src = session.media.full_res_source(&session.project, id, &*session.services).expect("a source");
    let frame = src.video_frame(FrameRequest::full(Tick(0))).unwrap();
    let filmcraft_frame::PixelData::Rgba8(px) = &frame.data else { panic!("not RGBA8") };
    assert_eq!(&px[..8], [10, 20, 30, 255, 40, 50, 60, 255], "the plug-in's pixels reach the frame");

    // A file the plug-in rejects is an import error that names the plug-in's reason.
    let bad = dir.join("bad.rgbraw");
    std::fs::write(&bad, b"nope").unwrap();
    let err = import(&mut session, &bad).unwrap_err().to_string();
    assert!(err.contains("12 byte header"), "{err}");

    // Removing the plug-in takes the format away with it.
    host.manager.remove(RAW).unwrap();
    assert!(!host.surfaces.formats.handles("rgbraw"));
    assert!(import(&mut session, &file).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
