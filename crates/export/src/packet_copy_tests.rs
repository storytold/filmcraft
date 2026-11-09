//! Generated codec fixtures only; independent FFmpeg decoding is optional.
use super::*;
use filmcraft_isobmff::{Mp4File, TrackKind};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings};
use filmcraft_render::SourceMap;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = filmcraft_testkit::fixtures::workspace_root().join("target/packet-copy-tests").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn name(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_colored(root: &Scratch, rate: FrameRate, intra_only: bool, color: [f32; 4], profile: H264Profile) -> (Project, ItemId, String) {
    let (original, seq, mut provider) = tests::project();
    let key = original.sequence(seq).unwrap().video_tracks[0].items[0].item;
    provider.0.insert(
        key,
        Arc::new(filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::ColorMatte { color },
            320,
            180,
            rate,
            Tick(2 * filmcraft_time::TICKS_PER_SECOND),
        )),
    );
    let mut original = (*original).clone();
    let q = original.sequence_mut(seq).unwrap();
    q.settings.width = 64;
    q.settings.height = 36;
    q.settings.frame_rate = rate;
    q.audio_tracks.clear();
    q.video_tracks[0].items[0].duration = rate.tick_of(12);
    let source = root.name("source.mp4");
    let settings =
        ExportSettings { path: source.clone(), include_audio: false, h264_intra_only: intra_only, h264_profile: profile, max_threads: 2, ..Default::default() };
    export(&Arc::new(original), seq, &settings, &provider, &Progress::default()).unwrap();
    let bytes: Arc<[u8]> = std::fs::read(&source).unwrap().into();
    let media = filmcraft_codecs::open_bytes("source.mp4", bytes).unwrap();
    let mut p = Project::new("copied");
    let item = p.add_item(
        "source",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::File { path: source.clone() },
            info: media.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let seq = p.new_sequence("copy", SequenceSettings { width: 64, height: 36, frame_rate: rate, ..Default::default() }, 1, 1, None);
    for at in [0, 12] {
        let clip = p.make_track_item(item, filmcraft_project::TrackKind::Video, rate.tick_of(at), TimeRange::new(Tick::ZERO, rate.tick_of(12)), rate).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    }
    (p, seq, source)
}

fn fixture_with(root: &Scratch, rate: FrameRate, intra_only: bool) -> (Project, ItemId, String) {
    fixture_colored(root, rate, intra_only, [1., 0., 0., 1.], H264Profile::High)
}

fn fixture(root: &Scratch, rate: FrameRate) -> (Project, ItemId, String) {
    fixture_with(root, rate, true)
}

/// Any decoded-frame request in this source would fail the test. Container/source
/// resolution remains available, preserving the host's offline/access policy.
struct MetadataOnly(filmcraft_media::MediaInfo);
impl filmcraft_media::MediaSource for MetadataOnly {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.0
    }
    fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        panic!("copy requested decoded video");
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        panic!("copy requested decoded audio");
    }
}
fn metadata_sources(p: &Project) -> SourceMap {
    let mut m = SourceMap::default();
    for (key, item) in &p.items {
        if let Some(media) = item.as_media() {
            m.0.insert(*key, Arc::new(MetadataOnly(media.info.clone())));
        }
    }
    m
}

fn settings(root: &Scratch) -> ExportSettings {
    ExportSettings { path: root.name("copy.mp4"), include_audio: false, video_mode: VideoMode::CopyRequired, ..Default::default() }
}
fn movie(path: &str) -> (Vec<u8>, Mp4File) {
    let data = std::fs::read(path).unwrap();
    let movie = filmcraft_isobmff::open(&data).unwrap();
    (data, movie)
}
fn payloads(path: &str) -> Vec<Vec<u8>> {
    let (data, m) = movie(path);
    let ti = m.track_of_kind(TrackKind::Video).unwrap();
    (0..m.tracks[ti].samples.len()).map(|i| m.read_sample(&data, ti, i).unwrap()).collect()
}

#[test]
fn compressed_packets_are_copied_exactly_without_a_source_decoder() {
    for (rate, intra) in [(FrameRate::FPS_24, true), (FrameRate::new(30000, 1001), true), (FrameRate::FPS_24, false), (FrameRate::new(30000, 1001), false)] {
        let root = Scratch::new();
        let (p, seq, source) = fixture_with(&root, rate, intra);
        let settings = settings(&root);
        let provider = metadata_sources(&p);
        let progress = Progress::default();
        let report = export(&Arc::new(p), seq, &settings, &provider, &progress).unwrap();
        assert_eq!(report.copied_video_frames, 24);
        assert_eq!(report.frames, 24);
        assert_eq!(progress.done.load(Ordering::Relaxed), 24);
        assert!(progress.finished.load(Ordering::Relaxed));
        let expected = payloads(&source);
        let actual = payloads(&settings.path);
        assert_eq!(actual, [expected.clone(), expected].concat());
        let (_, m) = movie(&settings.path);
        let t = &m.tracks[m.track_of_kind(TrackKind::Video).unwrap()];
        let (_, src) = movie(&source);
        let st = &src.tracks[src.track_of_kind(TrackKind::Video).unwrap()];
        if !intra {
            assert!(st.samples.windows(2).any(|s| s[1].pts < s[0].pts), "fixture must exercise reordered B-frames");
        }
        for (i, s) in t.samples.iter().enumerate() {
            let original = &st.samples[i % 12];
            assert_eq!(s.dts, i as i64 * rate.den);
            assert_eq!(s.pts, s.dts + original.pts - original.dts);
            assert_eq!(s.duration, rate.den as u32);
        }
        assert_eq!(t.edit_offset, st.edit_offset);
        let bytes = std::fs::read(&settings.path).unwrap();
        assert!(bytes.windows(4).position(|s| s == b"moov").unwrap() < bytes.windows(4).position(|s| s == b"mdat").unwrap());
        // Own decoder must also read every copied picture and retain the source display.
        let decoded = filmcraft_codecs::open_bytes("copy.mp4", bytes.into()).unwrap();
        let original = filmcraft_codecs::open_bytes("source.mp4", std::fs::read(source).unwrap().into()).unwrap();
        for i in 0..24 {
            let req = filmcraft_media::FrameRequest::full(rate.tick_of(i));
            let src = filmcraft_media::FrameRequest::full(rate.tick_of(i % 12));
            assert_eq!(decoded.video_frame(req).unwrap().to_rgba8(), original.video_frame(src).unwrap().to_rgba8());
        }
        if let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("packet copy independent decode") {
            let out = std::process::Command::new(ffmpeg).args(["-v", "error", "-xerror", "-i", &settings.path, "-f", "null", "-"]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }
}

#[test]
fn strict_mode_rejects_edits_and_does_not_create_destination() {
    let root = Scratch::new();
    let (original, seq, _) = fixture(&root, FrameRate::FPS_24);
    for case in 0..11 {
        let mut p = original.clone();
        let mut s = settings(&root);
        let q = p.sequence_mut(seq).unwrap();
        match case {
            0 => q.video_tracks[0].items[0].speed = 0.5,
            1 => q.video_tracks[0].items[0].source_in = FrameRate::FPS_24.tick_of(1),
            2 => q.video_tracks[0].items[0].duration = FrameRate::FPS_24.tick_of(11),
            3 => q.video_tracks[0].items[1].start = FrameRate::FPS_24.tick_of(13),
            4 => s.include_audio = true,
            5 => s.scale = 0.5,
            6 => s.effects.timecode_overlay.enabled = true,
            7 => q.settings.width = 128,
            8 => s.frame_rate = Some(FrameRate::FPS_30),
            9 => {
                q.video_tracks[0].items[0]
                    .effect_mut("motion")
                    .unwrap()
                    .params
                    .insert("position".into(), filmcraft_project::Param::new(filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(123., 456.))));
            }
            _ => {
                q.video_tracks[0].items[0]
                    .effect_mut("opacity")
                    .unwrap()
                    .params
                    .insert("opacity".into(), filmcraft_project::Param::new(filmcraft_project::ParamValue::Float(50.)));
            }
        }
        let err = export(&Arc::new(p.clone()), seq, &s, &metadata_sources(&p), &Progress::default()).unwrap_err();
        assert!(err.to_string().contains("copyRequired["), "{case}: {err}");
        assert!(!Path::new(&s.path).exists());
    }
}

#[test]
fn strict_mode_never_truncates_existing_output_or_source() {
    let root = Scratch::new();
    let (p, seq, source) = fixture(&root, FrameRate::FPS_24);
    let original = std::fs::read(&source).unwrap();
    let s = ExportSettings { path: source.clone(), ..settings(&root) };
    assert!(export(&Arc::new(p.clone()), seq, &s, &metadata_sources(&p), &Progress::default()).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let s = settings(&root);
    std::fs::hard_link(&source, &s.path).unwrap();
    assert!(export(&Arc::new(p.clone()), seq, &s, &metadata_sources(&p), &Progress::default()).is_err());
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn strict_mode_cancellation_and_stepped_host_never_fall_back_to_render() {
    let root = Scratch::new();
    let (p, seq, _) = fixture(&root, FrameRate::FPS_24);
    let s = settings(&root);
    let progress = Progress::default();
    progress.cancel.store(true, Ordering::Relaxed);
    assert!(matches!(export(&Arc::new(p.clone()), seq, &s, &metadata_sources(&p), &progress), Err(ExportError::Cancelled)));
    assert!(!Path::new(&s.path).exists());
    assert!(Exporter::new(Arc::new(p), seq, &s, &Progress::default()).is_err());
}

#[test]
fn old_presets_default_to_render_and_copy_policy_roundtrips() {
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"path":"out.mp4"})).unwrap();
    assert_eq!(s.video_mode, VideoMode::Render);
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"videoMode":"copyRequired"})).unwrap();
    assert_eq!(s.video_mode, VideoMode::CopyRequired);
    assert_eq!(serde_json::to_value(&s).unwrap()["videoMode"], "copyRequired");
    let s = ExportSettings { caption_sidecar: Some("srt".into()), ..s };
    assert!(matches!(s.validate(), Err(ExportError::CopyIneligible { code: "conversion", .. })));
}

#[test]
fn distinct_compatible_sources_join_but_different_codec_configuration_rejects() {
    for profile in [H264Profile::High, H264Profile::Baseline] {
        let first = Scratch::new();
        let second = Scratch::new();
        let (mut p, seq, red) = fixture(&first, FrameRate::FPS_24);
        let (blue_project, blue_seq, blue) = fixture_colored(&second, FrameRate::FPS_24, true, [0., 0., 1., 1.], profile);
        let blue_id = blue_project.sequence(blue_seq).unwrap().video_tracks[0].items[0].item;
        let kind = blue_project.items.get(&blue_id).unwrap().kind.clone();
        let imported = p.add_item("blue", Label::Iris, kind, None);
        p.sequence_mut(seq).unwrap().video_tracks[0].items[1].item = imported;
        let settings = settings(&first);
        let result = export(&Arc::new(p.clone()), seq, &settings, &metadata_sources(&p), &Progress::default());
        if profile == H264Profile::High {
            let report = result.unwrap();
            assert_eq!(report.copied_video_frames, 24);
            let a = payloads(&red);
            let b = payloads(&blue);
            assert_ne!(a, b);
            assert_eq!(payloads(&settings.path), [a, b].concat());
        } else {
            assert!(matches!(result, Err(ExportError::CopyIneligible { code: "codec", .. })), "{result:?}");
            assert!(!Path::new(&settings.path).exists());
        }
    }
}

#[test]
fn strict_copy_respects_host_missing_media_and_never_creates_output() {
    let root = Scratch::new();
    let (p, seq, _) = fixture(&root, FrameRate::FPS_24);
    let settings = settings(&root);
    let result = export(&Arc::new(p), seq, &settings, &SourceMap::default(), &Progress::default());
    assert!(matches!(result, Err(ExportError::CopyIneligible { code: "media", .. })));
    assert!(!Path::new(&settings.path).exists());
}

#[test]
fn strict_copy_rejects_unknown_container_color_signaling() {
    let root = Scratch::new();
    let (p, seq, source) = fixture(&root, FrameRate::FPS_24);
    let mut bytes = std::fs::read(&source).unwrap();
    let color = bytes.windows(8).position(|b| b == b"colrnclx").unwrap();
    // Unspecified primaries cannot prove compatibility with a Rec.709 export.
    bytes[color + 8..color + 10].copy_from_slice(&2u16.to_be_bytes());
    std::fs::write(source, bytes).unwrap();
    let settings = settings(&root);
    let result = export(&Arc::new(p.clone()), seq, &settings, &metadata_sources(&p), &Progress::default());
    assert!(matches!(result, Err(ExportError::CopyIneligible { code: "color", .. })));
    assert!(!Path::new(&settings.path).exists());
}
