#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Session fixtures for the tests in this crate.

use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, ItemId, Label, SequenceSettings, TimeInterpolation, TrackItem};
use filmcraft_engine::time::{FrameRate, Tick};

pub fn clip(id: u64, start: Tick, duration: Tick) -> TrackItem {
    TrackItem {
        id: ClipId(id),
        item: ItemId(1),
        name: format!("clip-{id}"),
        label: Label::Iris,
        custom_label_color: None,
        start,
        duration,
        source_in: Tick::ZERO,
        speed: 1.0,
        reverse: false,
        enabled: true,
        link: None,
        group: None,
        effects: Vec::new(),
        markers: Vec::new(),
        gain_db: 0.0,
        frame_hold: None,
        scale_to_frame: false,
        essential: None,
        multicam: None,
        time_interpolation: TimeInterpolation::default(),
        hold_filters: false,
        field_options: None,
        source_channels: Vec::new(),
        audio_stream: 0,
        graphic: None,
        maintain_pitch: false,
    }
}

/// `Session::default`, then a sequence, the same shape `place.rs` tests use.
pub fn session_with(rate: FrameRate, tracks: usize, clips: &[(usize, TrackItem)]) -> Session {
    let mut session = Session::default();
    let seq_id = session.project_mut().new_sequence("Seq", SequenceSettings { frame_rate: rate, ..Default::default() }, tracks, 0, None);
    {
        let seq = session.project_mut().sequence_mut(seq_id).expect("sequence");
        for (index, item) in clips {
            seq.video_tracks.get_mut(*index).expect("video track").items.push(item.clone());
        }
    }
    session.state.active_sequence = Some(seq_id);
    session.state.open_sequences = vec![seq_id];
    session
}

pub fn video_session() -> (Session, ClipId) {
    let rate = FrameRate::FPS_24;
    let clip = clip(1, rate.tick_of(0), rate.tick_of(24));
    let id = clip.id;
    let mut session = session_with(rate, 1, &[(0, clip)]);
    session.state.selection = vec![id];
    (session, id)
}

/// A 24 fps sequence with one clip of a demo generator and nothing else: media `City_Night_Drive.mp4`
/// has no file, like the generator clips of the demo project. The clip shows frames 6 to 16 of
/// `frames` frames and is selected. Returns the session, the sequence and the clip.
pub fn generator_nest(w: u32, h: u32, frames: i64) -> (Session, ItemId, ClipId) {
    use filmcraft_engine::project::{ItemKind, MediaClip, MediaRef, TrackKind};
    use filmcraft_engine::time::TimeRange;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{DemoScene, Generator, MediaSource};

    let rate = FrameRate::FPS_24;
    let scene = Generator::Demo(DemoScene::CityNight);
    let src = GeneratorSource::new(scene.clone(), w, h, rate, rate.tick_of(frames));
    let media = MediaClip {
        media: MediaRef::Generator(scene),
        info: src.info().clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let mut session = Session::default();
    let item = session.project_mut().add_item("City_Night_Drive.mp4", Label::Iris, ItemKind::Media(Box::new(media)), None);
    let seq = session.project_mut().new_sequence("Seq", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let clip =
        session.project_mut().make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(rate.tick_of(6), rate.tick_of(10)), rate).expect("track item");
    let clip_id = clip.id;
    session.project_mut().sequence_mut(seq).expect("sequence").video_tracks[0].items.push(clip);
    session.state.active_sequence = Some(seq);
    session.state.open_sequences = vec![seq];
    session.state.selection = vec![clip_id];
    (session, seq, clip_id)
}

/// The demo project's edit: `scenes` demo generators back to back on V1, each with the audio clip
/// the editor makes next to it on A1, and one audio-only generator on A2 (`Ambient_Score.wav`).
/// All of them are selected. No source has a file. Returns the session and the sequence.
pub fn demo_edit(scenes: &[filmcraft_media::DemoScene], w: u32, h: u32, frames: i64) -> (Session, ItemId) {
    use filmcraft_engine::project::{ItemKind, MediaClip, MediaRef, TrackKind};
    use filmcraft_engine::time::TimeRange;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{Generator, MediaSource};

    let rate = FrameRate::FPS_24;
    let media_of = |scene: Generator| {
        let src = GeneratorSource::new(scene.clone(), w, h, rate, rate.tick_of(frames));
        MediaClip {
            media: MediaRef::Generator(scene),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }
    };
    let mut session = Session::default();
    let seq = session.project_mut().new_sequence("Seq", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 2, None);
    let mut selection = Vec::new();
    for (index, scene) in scenes.iter().enumerate() {
        let item = session.project_mut().add_item(&format!("{scene:?}.mp4"), Label::Iris, ItemKind::Media(Box::new(media_of(Generator::Demo(*scene)))), None);
        let at = rate.tick_of(index as i64 * 10);
        let range = TimeRange::new(rate.tick_of(6), rate.tick_of(10));
        let video = session.project_mut().make_track_item(item, TrackKind::Video, at, range, rate).expect("video clip");
        let audio = session.project_mut().make_track_item(item, TrackKind::Audio, at, range, rate).expect("audio clip");
        selection.extend([video.id, audio.id]);
        let sequence = session.project_mut().sequence_mut(seq).expect("sequence");
        sequence.video_tracks[0].items.push(video);
        sequence.audio_tracks[0].items.push(audio);
    }
    let score = session.project_mut().add_item(
        "Ambient_Score.wav",
        Label::Iris,
        ItemKind::Media(Box::new(media_of(Generator::Demo(filmcraft_media::DemoScene::Aurora)))),
        None,
    );
    let clip =
        session.project_mut().make_track_item(score, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).expect("score");
    selection.push(clip.id);
    session.project_mut().sequence_mut(seq).expect("sequence").audio_tracks[1].items.push(clip);
    session.state.active_sequence = Some(seq);
    session.state.open_sequences = vec![seq];
    session.state.selection = selection;
    (session, seq)
}

/// The sequence of [`generator_nest`] wrapped in another one: the selected clip of the returned
/// session shows the whole inner sequence, a source with no file of its own. Returns the session,
/// the outer sequence and the clip.
pub fn nested_generator(w: u32, h: u32, frames: i64) -> (Session, ItemId, ClipId) {
    use filmcraft_engine::project::TrackKind;
    use filmcraft_engine::time::TimeRange;

    let (mut session, inner, _) = generator_nest(w, h, frames);
    let rate = FrameRate::FPS_24;
    let outer = session.project_mut().new_sequence("Outer", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let span = session.project.item(inner).expect("inner").duration();
    let clip = session.project_mut().make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, span), rate).expect("nested clip");
    let clip_id = clip.id;
    session.project_mut().sequence_mut(outer).expect("outer").video_tracks[0].items.push(clip);
    session.state.active_sequence = Some(outer);
    session.state.open_sequences = vec![outer];
    session.state.selection = vec![clip_id];
    (session, outer, clip_id)
}

/// Tests that start renders share the process-wide render registry ([`crate::prepare`]). They take
/// this lock, so one test's cancel cannot stop another's render.
pub fn prepare_serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct TempProject {
    pub path: String,
}

impl TempProject {
    pub fn new(suffix: &str) -> Self {
        let dir = if suffix.is_empty() { format!("/tmp/fc-editin-{}", std::process::id()) } else { format!("/tmp/fc-editin-{}-{suffix}", std::process::id()) };
        let _ = std::fs::remove_dir_all(&dir);
        Self { path: format!("{dir}/Film.fcproj") }
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        if let Some(parent) = std::path::Path::new(&self.path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

pub fn project_json(session: &Session) -> String {
    serde_json::to_string(session.project.as_ref()).expect("project json")
}

/// Write a `frames`-frame `w` x `h` 24 fps ProRes `.mov` of a demo scene to `path`: a real file the
/// media pool can open, like the result movie EffectCraft writes.
pub fn write_movie(path: &str, w: u32, h: u32, frames: i64) {
    use std::sync::Arc;

    use filmcraft_engine::project::{ItemKind, MediaClip, MediaRef, SequenceSettings, TrackKind};
    use filmcraft_engine::time::TimeRange;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{DemoScene, Generator, MediaSource};

    let rate = FrameRate::FPS_24;
    let dur = rate.tick_of(frames);
    let scene = Generator::Demo(DemoScene::Aurora);
    let src = GeneratorSource::new(scene.clone(), w, h, rate, dur);
    let clip = MediaClip {
        media: MediaRef::Generator(scene),
        info: src.info().clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let mut project = filmcraft_engine::project::Project::new("fixture");
    let item = project.add_item("scene", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let seq = project.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let video = project.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, dur), rate).expect("track item");
    project.sequence_mut(seq).expect("sequence").video_tracks[0].items.push(video);
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir).expect("movie dir");
    }
    let settings = filmcraft_export::ExportSettings { format: filmcraft_export::Format::ProRes, path: path.to_string(), ..Default::default() };
    let shared: filmcraft_media::SharedSource = Arc::new(src);
    let provider = move |id: ItemId| (id == item).then(|| shared.clone());
    filmcraft_export::export(&Arc::new(project), seq, &settings, &provider, &Default::default()).expect("export the fixture movie");
}

/// A link record for `link_id` with the sample format of the link tests.
pub fn record(link_id: &str) -> crate::record::LinkRecord {
    use serde_json::Map;

    use crate::record::{Format, LinkRecord};
    LinkRecord {
        v: 1,
        link_id: link_id.to_string(),
        target_app: "effectcraft".to_string(),
        target_project: "/tmp/ec.ecproj".to_string(),
        target_comp: "c".to_string(),
        nested_sequence: "7".to_string(),
        result_path: "/tmp/x.mov".to_string(),
        result_epoch: 0,
        format: Format { w: 320, h: 180, rate_num: 24, rate_den: 1, duration_ticks: 24, alpha: false, extra: Map::new() },
        created: "2026-10-22T00:00:00Z".to_string(),
        extra: Map::new(),
    }
}

/// An offline item the way `link.editIn` makes it (the result file is not there yet), carrying a
/// link record whose `result_path` is `path`.
pub fn linked_offline(session: &mut Session, link_id: &str, path: &str) -> ItemId {
    let made = session
        .execute(
            "file.newOfflineFile",
            serde_json::json!({"name": "Result", "fileName": path, "video": true, "audio": false, "width": 160, "height": 90, "fps": 24.0, "seconds": 1.0}),
        )
        .expect("offline item");
    let item = ItemId(made["item"].as_u64().expect("item id"));
    let mut linked = record(link_id);
    linked.result_path = path.to_string();
    let entry = session.project_mut().item_mut(item).expect("the new item");
    // `file.newOfflineFile` keeps only a file name, so the full path goes in here.
    entry.as_media_mut().expect("a media item").media = filmcraft_engine::project::MediaRef::File { path: path.to_string() };
    entry.metadata.insert(crate::link_ops::LINK_KEY.to_string(), serde_json::to_string(&linked).expect("record json"));
    item
}
