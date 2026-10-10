use crate::{Session, keyboard};
use filmcraft_media::{DemoScene, Generator, generators::GeneratorSource};
use filmcraft_project::{ItemId, Label};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::json;
use std::sync::Arc;

fn session() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"width":128,"height":72,"fps":24})).unwrap();
    let item = crate::demo::add_generator(
        Arc::make_mut(&mut s.project),
        &s.media,
        GeneratorSource::new(Generator::Demo(DemoScene::Aurora), 128, 72, FrameRate::FPS_24, Tick(5 * TICKS_PER_SECOND)),
        "Source clip",
        Label::Iris,
        None,
    );
    s.execute("timeline.place", json!({"item":item.0,"track":"V1"})).unwrap();
    s.execute("source.open", json!({"item":item.0})).unwrap();
    (s, item)
}

#[test]
fn frame_export_selects_the_correct_monitor_time_and_original_media() {
    let (mut s, item) = session();
    let dir = std::env::temp_dir().join(format!("fc-frame-original-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proxy = dir.join("proxy.png");
    image::RgbaImage::from_pixel(16, 9, image::Rgba([255, 0, 0, 255])).save(&proxy).unwrap();
    Arc::make_mut(&mut s.project).item_mut(item).unwrap().as_media_mut().unwrap().proxy =
        Some(filmcraft_project::MediaRef::File { path: proxy.to_string_lossy().into_owned() });
    s.media.set_use_proxies(true);
    s.state.source_playhead = FrameRate::FPS_24.tick_of(20);
    s.set_playhead(FrameRate::FPS_24.tick_of(35));
    let before = s.project.to_json();
    let revision = s.revision;
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let expected_source = filmcraft_render::render_item(&s.project, item, s.state.source_playhead, 1.0, &provider).unwrap().to_rgba8();
    let preview_provider = s.media.provider(s.project.clone(), s.services.clone());
    let preview = filmcraft_render::render_item(&s.project, item, s.state.source_playhead, 1.0, &preview_provider).unwrap().to_rgba8();
    assert_ne!(preview, expected_source, "the preview really uses the red proxy");
    let seq = s.state.active_sequence.unwrap();
    let expected_program =
        filmcraft_render::render_sequence(&s.project, seq, s.playhead(), filmcraft_render::RenderOptions { captions: true, ..Default::default() }, &provider)
            .to_rgba8();
    assert_ne!(expected_source, expected_program);
    for (target, time, expected) in [("source", s.state.source_playhead, expected_source), ("program", s.playhead(), expected_program)] {
        let path = dir.join(format!("{target}.png"));
        let result = s.execute("file.exportFrame", json!({"target":target,"path":path.to_string_lossy()})).unwrap();
        assert_eq!(result["time"], json!(time.0));
        let image = image::load_from_memory(&std::fs::read(path).unwrap()).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (128, 72));
        assert_eq!(image.into_raw(), expected);
    }
    assert_eq!(s.project.to_json(), before);
    assert_eq!(s.revision, revision);
    assert!(s.media.use_proxies());
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn source_frame_export_works_without_an_active_sequence_and_rejects_bad_params() {
    let (mut s, item) = session();
    s.state.active_sequence = None;
    let path = std::env::temp_dir().join(format!("fc-source-frame-{}.png", std::process::id()));
    let r = s.execute("file.exportFrame", json!({"target":"source","path":path.to_string_lossy()})).unwrap();
    assert_eq!(r["width"], 128);
    assert!(s.execute("file.exportFrame", json!({"path":path.to_string_lossy()})).is_err());
    for params in [
        json!({"target":"wrong"}),
        json!({"target":3}),
        json!({"target":"source","time":-1}),
        json!({"target":"source","time":"bad"}),
        json!({"target":"source","item":0}),
        json!({"target":"source","format":false}),
        json!({"target":"source","import":"yes"}),
        json!({"target":"source","path":""}),
    ] {
        assert!(s.execute("file.exportFrame", params.clone()).is_err(), "{params}");
    }
    Arc::make_mut(&mut s.project).item_mut(item).unwrap().as_media_mut().unwrap().info.video = None;
    assert!(s.execute("file.exportFrame", json!({"target":"source","path":path.to_string_lossy()})).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn frame_export_snapshots_are_independent_of_later_monitor_selection() {
    let (mut s, item) = session();
    s.state.source_playhead = FrameRate::FPS_24.tick_of(19);
    let target = keyboard::frame_export_target(&s, &json!({"target":"source"})).unwrap();
    s.state.source_item = None;
    s.state.source_playhead = Tick::ZERO;
    let path = std::env::temp_dir().join(format!("fc-captured-frame-{}.bmp", std::process::id()));
    let r = s.execute("file.exportFrame", json!({"target":"source","item":item.0,"time":target.time.0,"format":"bmp","path":path.to_string_lossy()})).unwrap();
    assert_eq!(r["time"], json!(target.time.0));
    assert_eq!(&std::fs::read(&path).unwrap()[..2], b"BM");
    std::fs::remove_file(path).unwrap();
    let seq = s.state.active_sequence.unwrap();
    Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap().settings.frame_rate = FrameRate { num: 1, den: i64::MAX };
    assert!(keyboard::frame_export_target(&s, &json!({})).is_err());
}
