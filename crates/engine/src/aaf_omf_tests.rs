//! File ▸ Export ▸ AAF / OMF and their import, end to end with real media written by our encoders.

use std::sync::Arc;

use filmcraft_project::{ItemId, ItemKind, MediaRef, Param, ParamValue, TrackKind};
use filmcraft_time::FrameRate;
use serde_json::json;

use crate::Session;
use crate::media_test_util::{make_movie, session_with, tmp_dir};

/// Two 2-second ProRes + PCM movies cut back to back on V1/A1 (24 fps).
fn setup(name: &str) -> (Session, Vec<ItemId>, ItemId, std::path::PathBuf) {
    let dir = tmp_dir(name);
    let a = dir.join("a.mov");
    let b = dir.join("b.mov");
    make_movie(&a, filmcraft_media::DemoScene::Aurora, 64, 36, 48);
    make_movie(&b, filmcraft_media::DemoScene::Plasma, 64, 36, 48);
    let (mut s, items, seq) = session_with(&[&a, &b]);
    // trim both clips so handles exist: A plays media 12..36, B 6..30
    let rate = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(seq).unwrap();
    for t in q.all_tracks_mut() {
        for (i, c) in t.items.iter_mut().enumerate() {
            c.start = rate.tick_of(i as i64 * 24);
            c.source_in = rate.tick_of(if i == 0 { 12 } else { 6 });
            c.duration = rate.tick_of(24);
        }
    }
    s.project = Arc::new(p);
    (s, items, seq, dir)
}

fn audio_of(s: &Session, item: ItemId, start: i64, n: usize) -> Vec<Vec<f32>> {
    let src = s.media.full_res_source(&s.project, item, &*s.services).expect("online");
    src.audio(start, n, 48_000).unwrap().channels
}

fn imported_seq(r: &serde_json::Value) -> ItemId {
    let docs = r["documents"].as_array().cloned().unwrap_or_default();
    let seq = r["sequences"][0].as_u64().or_else(|| docs.first().and_then(|d| d["sequences"][0].as_u64())).unwrap_or_else(|| panic!("no sequence in {r}"));
    ItemId(seq)
}

fn path_of(s: &Session, item: ItemId) -> String {
    match &s.project.item(item).unwrap().kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => path.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

#[test]
fn aaf_embedded_trimmed_audio_round_trips_sample_exact() {
    let (mut s, items, seq, dir) = setup("aaf-embed");
    let path = dir.join("cut.aaf").to_string_lossy().into_owned();
    let r = s.execute("file.exportAaf", json!({"path": path, "audio": "embedded", "trimAudio": true, "handles": 6, "sequence": seq.0})).unwrap();
    assert!(r["mediaFiles"].as_array().unwrap().is_empty(), "{r}");
    let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
    let n = imported_seq(&r);
    let q = s.project.sequence(n).unwrap().clone();
    let orig = s.project.sequence(seq).unwrap().clone();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let a: Vec<_> = q.tracks(kind)[0].items.iter().map(|c| (c.start, c.duration)).collect();
        let b: Vec<_> = orig.tracks(kind)[0].items.iter().map(|c| (c.start, c.duration)).collect();
        assert_eq!(a, b, "{kind:?}");
    }
    // video links to the original movies, audio to WAV files extracted next to the AAF
    assert_eq!(std::path::Path::new(&path_of(&s, q.video_tracks[0].items[0].item)), std::path::Path::new(&path_of(&s, items[0])));
    for (c, oc) in q.audio_tracks[0].items.iter().zip(&orig.audio_tracks[0].items) {
        let p = path_of(&s, c.item);
        assert!(p.ends_with(".wav") && std::path::Path::new(&p).exists(), "{p}");
        assert!(!s.project.item(c.item).unwrap().as_media().unwrap().offline);
        // trimmed: the essence starts 6 frames (handles) before the clip
        assert_eq!(c.source_in, FrameRate::FPS_24.tick_of(6));
        let got = audio_of(&s, c.item, c.source_in.to_units_floor(48_000), 4800);
        let want = audio_of(&s, oc.item, oc.source_in.to_units_floor(48_000), 4800);
        for ch in 0..2 {
            for i in 0..4800 {
                assert!((got[ch][i] - want[ch][i]).abs() <= 1.0 / 32_768.0 + 1e-6, "ch {ch} sample {i}: {} vs {}", got[ch][i], want[ch][i]);
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn aaf_separate_aiff_breakout_and_linked() {
    let (mut s, items, seq, dir) = setup("aaf-separate");
    let path = dir.join("sep.aaf").to_string_lossy().into_owned();
    let r = s
        .execute(
            "file.exportAaf",
            json!({"path": path, "audio": "separate", "audioFormat": "aiff", "bitDepth": 24, "breakoutToMono": true, "trimAudio": true, "handles": 0}),
        )
        .unwrap();
    let files: Vec<String> = r["mediaFiles"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(files.len(), 4, "two media × two channels: {r}");
    for f in &files {
        let b = std::fs::read(f).unwrap();
        let (pcm, ch, sr, bits) = filmcraft_interchange::wav::parse_aiff(&b).unwrap();
        assert_eq!((ch, sr, bits), (1, 48_000, 24));
        assert_eq!(pcm.len(), 24 * 2000 * 3, "one second of mono 24-bit audio (no handles)");
    }
    let r = s.execute("file.importAaf", json!({"path": path})).unwrap();
    let n = imported_seq(&r);
    let q = s.project.sequence(n).unwrap();
    let mono: Vec<_> = q.audio_tracks.iter().filter(|t| !t.items.is_empty()).collect();
    assert_eq!(mono.len(), 2);
    assert!(mono.iter().all(|t| t.channels == filmcraft_project::AudioChannels::Mono));

    // Avid-style consolidated OP-Atom MXF audio: our MXF reader opens every file
    let path = dir.join("mxf.aaf").to_string_lossy().into_owned();
    let r = s.execute("file.exportAaf", json!({"path": path, "audio": "separate", "audioFormat": "mxf", "breakoutToMono": true, "trimAudio": true})).unwrap();
    let files: Vec<String> = r["mediaFiles"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(files.len(), 4);
    for f in &files {
        let b = std::fs::read(f).unwrap();
        let m = filmcraft_mxf::open(&b).unwrap();
        assert_eq!(m.operational_pattern.name(), "OP-Atom");
        let t = m.track_of_kind(filmcraft_mxf::TrackKind::Sound).unwrap();
        assert_eq!(m.tracks[t].stored_sample_frames(), 48_000);
    }
    let r = s.execute("file.importAaf", json!({"path": path})).unwrap();
    let n = imported_seq(&r);
    let a = &s.project.sequence(n).unwrap().audio_tracks[0].items[0];
    assert!(path_of(&s, a.item).ends_with(".mxf"));
    assert!(!s.project.item(a.item).unwrap().as_media().unwrap().offline, "the MXF audio links");

    // linked: no media is written, the document points at the original movies
    let path = dir.join("linked.aaf").to_string_lossy().into_owned();
    let r = s.execute("file.exportAaf", json!({"path": path, "audio": "linked", "sequence": seq.0})).unwrap();
    assert!(r["mediaFiles"].as_array().unwrap().is_empty());
    let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
    let n = imported_seq(&r);
    let q = s.project.sequence(n).unwrap();
    let a = &q.audio_tracks[0].items[0];
    assert_eq!(std::path::Path::new(&path_of(&s, a.item)), std::path::Path::new(&path_of(&s, items[0])));
    assert_eq!(a.source_in, FrameRate::FPS_24.tick_of(12));
    assert!(!s.project.item(a.item).unwrap().as_media().unwrap().offline);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn aaf_video_mixdown() {
    let (mut s, _items, seq, dir) = setup("aaf-mixdown");
    let path = dir.join("mix.aaf").to_string_lossy().into_owned();
    let r = s.execute("file.exportAaf", json!({"path": path, "mixdownVideo": true, "audio": "linked", "sequence": seq.0})).unwrap();
    let files = r["mediaFiles"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let mov = files[0].as_str().unwrap();
    assert!(mov.ends_with("Video Mixdown.mov") && std::path::Path::new(mov).exists());
    let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
    let n = imported_seq(&r);
    let q = s.project.sequence(n).unwrap();
    assert_eq!(q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>(), 1);
    let v = &q.video_tracks[0].items[0];
    assert_eq!(v.duration, FrameRate::FPS_24.tick_of(48));
    assert_eq!(std::path::Path::new(&path_of(&s, v.item)), std::path::Path::new(mov));
    let m = s.project.item(v.item).unwrap().as_media().unwrap();
    assert!(!m.offline);
    assert_eq!(m.info.video.as_ref().map(|v| (v.width, v.height)), Some((64, 36)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn omf_renders_clip_effects_into_embedded_audio() {
    let (mut s, _items, seq, dir) = setup("omf-render");
    // −6.0206 dB on the first audio clip: half amplitude
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(seq).unwrap();
    q.audio_tracks[0].items[0].effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-6.020_599_913)));
    s.project = Arc::new(p);
    let orig = s.project.sequence(seq).unwrap().clone();
    let path = dir.join("mix.omf").to_string_lossy().into_owned();
    let r = s.execute("file.exportOmf", json!({"path": path, "renderAudioEffects": true, "handles": 0, "sequence": seq.0})).unwrap();
    assert!(r["bytes"].as_u64().unwrap() > 48_000 * 2 * 2, "audio is encapsulated: {r}");
    let r = s.execute("file.importAaf", json!({"path": path})).unwrap();
    let n = imported_seq(&r);
    let q = s.project.sequence(n).unwrap().clone();
    assert!(q.video_tracks.iter().all(|t| t.items.is_empty()));
    let a = &q.audio_tracks[0].items;
    assert_eq!(a.len(), 2);
    assert_eq!(
        a.iter().map(|c| (c.start, c.duration)).collect::<Vec<_>>(),
        orig.audio_tracks[0].items.iter().map(|c| (c.start, c.duration)).collect::<Vec<_>>()
    );
    // no gain left on the clips: it is in the samples
    assert!(a.iter().all(|c| c.effect("volume").and_then(|e| e.param("level")).is_none_or(|p| p.value.as_f64() == Some(0.0))));
    let oc = &orig.audio_tracks[0].items[0];
    let got = audio_of(&s, a[0].item, a[0].source_in.to_units_floor(48_000), 2400);
    let want = audio_of(&s, oc.item, oc.source_in.to_units_floor(48_000), 2400);
    let peak = want[0].iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.01, "the demo scene has audio");
    for i in 0..2400 {
        assert!((got[0][i] - want[0][i] * 0.5).abs() < 2e-3, "sample {i}: {} vs {}", got[0][i], want[0][i] * 0.5);
    }
    // the second clip is unchanged
    let oc2 = &orig.audio_tracks[0].items[1];
    let got = audio_of(&s, a[1].item, a[1].source_in.to_units_floor(48_000), 2400);
    let want = audio_of(&s, oc2.item, oc2.source_in.to_units_floor(48_000), 2400);
    for i in 0..2400 {
        assert!((got[0][i] - want[0][i]).abs() < 2e-3);
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// `setup`, with both clips nested into "Inner" (one linked picture + sound nest on V1 / A1) and
/// −6.0206 dB (half amplitude) on the nest's sound clip.
fn setup_nested(name: &str) -> (Session, Vec<ItemId>, ItemId, ItemId, std::path::PathBuf) {
    let (mut s, items, seq, dir) = setup(name);
    s.execute("sequence.open", json!({"item": seq.0})).unwrap();
    let clips: Vec<u64> = s.project.sequence(seq).unwrap().all_tracks().flat_map(|t| t.items.iter().map(|c| c.id.0)).collect();
    s.execute("timeline.select", json!({"clips": clips})).unwrap();
    let inner = ItemId(s.execute("clip.nest", json!({"name": "Inner"})).unwrap()["sequence"].as_u64().unwrap());
    let mut p = (*s.project).clone();
    let q = p.sequence_mut(seq).unwrap();
    assert_eq!((q.video_tracks[0].items.len(), q.audio_tracks[0].items.len()), (1, 1), "one linked nest");
    q.audio_tracks[0].items[0].effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-6.020_599_913)));
    s.project = Arc::new(p);
    (s, items, seq, inner, dir)
}

/// Premiere Pro renders the sound of a nested sequence into an OMF as one clip named after the
/// sequence (seen in an OMF it exported). So do we: the nest is mixed, with what is on its clip.
#[test]
fn omf_renders_the_sound_of_a_nested_sequence() {
    let (mut s, items, seq, inner, dir) = setup_nested("omf-nest");
    let nest = s.project.sequence(seq).unwrap().audio_tracks[0].items[0].clone();
    let path = dir.join("nest.omf").to_string_lossy().into_owned();
    let r = s.execute("file.exportOmf", json!({"path": path, "handles": 0, "sequence": seq.0})).unwrap();
    assert!(!r["report"].to_string().contains("nested"), "{r}");
    let r = s.execute("file.importAaf", json!({"path": path})).unwrap();
    let q = s.project.sequence(imported_seq(&r)).unwrap().clone();
    let a = &q.audio_tracks[0].items;
    assert_eq!(a.len(), 1, "the nest is one clip");
    assert_eq!((a[0].name.as_str(), a[0].start, a[0].duration), ("Inner", nest.start, nest.duration));
    assert!(s.project.sequence(a[0].item).is_none(), "media in the document, not a sequence");
    // what it plays: the nested sequence's two clips one after the other, at half amplitude
    let inside = s.project.sequence(inner).unwrap().audio_tracks[0].items.clone();
    assert_eq!(inside.len(), 2);
    let got = audio_of(&s, a[0].item, a[0].source_in.to_units_floor(48_000), 96_000);
    for (k, oc) in inside.iter().enumerate() {
        let want = audio_of(&s, oc.item, oc.source_in.to_units_floor(48_000), 48_000);
        assert_eq!(oc.item, items[k]);
        let peak = want[0].iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.01, "the demo scene has audio");
        // (away from the cut between the two clips)
        for i in 480..47_520 {
            let g = got[0][k * 48_000 + i];
            assert!((g - want[0][i] * 0.5).abs() < 2e-3, "clip {k} sample {i}: {g} vs {}", want[0][i] * 0.5);
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// An AAF holds a nested sequence as a composition of its own (as Premiere Pro's does): it comes
/// back as a sequence that the nest clips use, with its own clips, and with embedded audio the
/// media inside it is in the document too.
#[test]
fn aaf_keeps_a_nested_sequence_as_a_sequence() {
    let (mut s, items, seq, inner, dir) = setup_nested("aaf-nest");
    let path = dir.join("nest.aaf").to_string_lossy().into_owned();
    let r = s.execute("file.exportAaf", json!({"path": path, "audio": "embedded", "trimAudio": true, "handles": 6, "sequence": seq.0})).unwrap();
    assert!(!r["report"].to_string().contains("gap"), "{r}");
    let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
    let q = s.project.sequence(imported_seq(&r)).unwrap().clone();
    let (v, a) = (&q.video_tracks[0].items, &q.audio_tracks[0].items);
    assert_eq!((v.len(), a.len()), (1, 1));
    let nested = v[0].item;
    assert_ne!(nested, inner, "a sequence of the imported document");
    assert_eq!((a[0].item, s.project.item(nested).unwrap().name.as_str()), (nested, "Inner"));
    assert!(v[0].link.is_some() && v[0].link == a[0].link, "picture and sound of the nest are linked");
    let level = a[0].effect("volume").and_then(|e| e.param("level")).and_then(|p| p.value.as_f64()).unwrap();
    assert!((level + 6.0206).abs() < 1e-2, "the level stays on the nest's clip: {level}");
    // inside: the two clips, their picture linked to the movies, their sound in the document
    let n = s.project.sequence(nested).unwrap().clone();
    let orig = s.project.sequence(inner).unwrap().clone();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let got: Vec<_> = n.tracks(kind)[0].items.iter().map(|c| (c.start, c.duration)).collect();
        let want: Vec<_> = orig.tracks(kind)[0].items.iter().map(|c| (c.start, c.duration)).collect();
        assert_eq!(got, want, "{kind:?}");
    }
    assert_eq!(std::path::Path::new(&path_of(&s, n.video_tracks[0].items[0].item)), std::path::Path::new(&path_of(&s, items[0])));
    for (c, oc) in n.audio_tracks[0].items.iter().zip(&orig.audio_tracks[0].items) {
        let p = path_of(&s, c.item);
        assert!(p.ends_with(".wav") && std::path::Path::new(&p).exists(), "{p}");
        let got = audio_of(&s, c.item, c.source_in.to_units_floor(48_000), 4800);
        let want = audio_of(&s, oc.item, oc.source_in.to_units_floor(48_000), 4800);
        for i in 0..4800 {
            assert!((got[0][i] - want[0][i]).abs() <= 1.0 / 32_768.0 + 1e-6, "sample {i}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn commands_are_registered_with_menus_and_validate_params() {
    for (id, menu) in [("file.exportAaf", &["File", "Export"][..]), ("file.exportOmf", &["File", "Export"][..]), ("file.importAaf", &[][..])] {
        let c = crate::commands::find(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(c.menu, menu);
    }
    let (mut s, _, seq, dir) = setup("aaf-params");
    let path = dir.join("x.aaf").to_string_lossy().into_owned();
    assert!(s.execute("file.exportAaf", json!({"sequence": seq.0})).is_err(), "path is required");
    assert!(s.execute("file.exportAaf", json!({"path": path, "bitDepth": 20})).is_err());
    assert!(s.execute("file.exportOmf", json!({"path": path, "audio": "linked"})).is_err(), "OMF has no linked mode");
    assert!(s.execute("file.importAaf", json!({"path": dir.join("a.mov").to_string_lossy()})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

// Selected-stream essence uses only PR361 schema13 placement and decoder fields.
mod selected_streams {
    use crate::{Services, Session};
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource};
    use filmcraft_project::{ClipId, ItemId, ItemKind, Label, MediaClip, MediaRef, SequenceSettings};
    use filmcraft_time::{FrameRate, Tick};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(usize, i64, usize, u32)>>>;

    struct Tones {
        info: MediaInfo,
        calls: Calls,
    }

    impl Tones {
        fn new(streams: usize) -> Self {
            let source = filmcraft_media::generators::GeneratorSource::new(
                filmcraft_media::Generator::BarsAndTone,
                32,
                18,
                FrameRate::FPS_24,
                Tick::from_seconds_f64(2.0),
            );
            let mut info = source.info().clone();
            info.kind = MediaKind::AudioOnly;
            info.video = None;
            info.audio_streams = vec![info.audio_streams[0].clone(); streams];
            Self { info, calls: Arc::default() }
        }

        fn sample(stream: usize, channel: usize, frame: i64, rate: u32) -> f32 {
            let frequency = (stream * 2 + channel + 1) as f64 * 240.0;
            let amplitude = 0.05 * (stream + 1) as f64;
            (amplitude * (std::f64::consts::TAU * frequency * frame as f64 / rate as f64).sin()) as f32
        }
    }

    impl MediaSource for Tones {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            self.audio_stream(0, start, frames, sample_rate)
        }
        fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            if stream >= self.info.audio_streams.len() {
                return Err(MediaError::NoStream("audio"));
            }
            self.calls.lock().unwrap().push((stream, start, frames, sample_rate));
            Ok(AudioBuffer {
                sample_rate,
                channels: (0..2).map(|channel| (0..frames).map(|i| Self::sample(stream, channel, start + i as i64, sample_rate)).collect()).collect(),
            })
        }
    }

    #[derive(Default)]
    struct MemoryFiles {
        writes: Mutex<Vec<(String, Vec<u8>)>>,
    }
    impl Services for MemoryFiles {
        fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn write_file(&self, path: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.writes.lock().unwrap().push((path.to_owned(), bytes.to_vec()));
            Ok(())
        }
        fn export_in_memory(&self) -> bool {
            true
        }
    }

    fn add_source(s: &mut Session, streams: usize) -> (ItemId, Calls) {
        let source = Arc::new(Tones::new(streams));
        let calls = source.calls.clone();
        let mut project = (*s.project).clone();
        let item = project.add_item(
            "distinct tones",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::File { path: format!("tone-source-{streams}.wav") },
                info: source.info.clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        s.project = Arc::new(project);
        s.media.insert(item, source);
        (item, calls)
    }

    fn fixture() -> (Session, ItemId, ItemId, Calls, Arc<MemoryFiles>) {
        let files = Arc::new(MemoryFiles::default());
        let mut session = Session::new(files.clone());
        let mut project = (*session.project).clone();
        let sequence =
            project.new_sequence("streams", SequenceSettings { width: 32, height: 18, frame_rate: FrameRate::FPS_24, ..Default::default() }, 0, 1, None);
        session.project = Arc::new(project);
        session.state.active_sequence = Some(sequence);
        session.state.open_sequences = vec![sequence];
        let (item, calls) = add_source(&mut session, 3);
        session.state.project_selection = vec![item];
        session.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
        (session, item, sequence, calls, files)
    }

    fn place(s: &mut Session, item: ItemId, seconds: f64) -> Vec<ClipId> {
        let result = s.execute("timeline.place", json!({"item": item.0, "track": "A1", "seconds": seconds})).unwrap();
        result["clips"].as_array().unwrap().iter().map(|id| ClipId(id.as_u64().unwrap())).collect()
    }

    #[test]
    fn aaf_essence_distinguishes_streams_and_linked_refusal_precedes_writes() {
        use crate::aaf_omf::{AudioMode, AudioPlan};
        use filmcraft_interchange::essence::{EssenceData, EssenceKey, NestNeeds};
        let (mut s, item, sequence, calls, files) = fixture();
        place(&mut s, item, 0.0);
        let plan = AudioPlan {
            mode: AudioMode::Embedded,
            aiff: false,
            mxf: false,
            sample_rate: 48_000,
            bits: 16,
            trim: true,
            handles: Tick::ZERO,
            render_effects: false,
            breakout: false,
            nests: NestNeeds::Inside,
        };
        let (essences, paths) = crate::aaf_omf::prepare_audio(&s, sequence, &plan, "ignored").unwrap();
        assert!(paths.is_empty());
        assert_eq!(essences.len(), 3);
        for (stream, essence) in essences.iter().enumerate() {
            assert_eq!(essence.key, EssenceKey::media(item, stream));
            let EssenceData::Embedded(bytes) = &essence.data else { panic!("expected embedded PCM") };
            for channel in 0..2 {
                let offset = (25 * 2 + channel) * 2;
                let actual = i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                let expected = (Tones::sample(stream, channel, 25, 48_000) * 32767.0).round() as i16;
                assert_eq!(actual, expected);
            }
        }
        let options = filmcraft_interchange::aaf::AafOptions {
            media: filmcraft_interchange::essence::MediaOptions { essence: essences, ..Default::default() },
            ..Default::default()
        };
        let (aaf, _) = filmcraft_interchange::aaf::export(&s.project, sequence, &options).unwrap();
        let omf_options = filmcraft_interchange::omf::OmfOptions { media: options.media.clone(), ..Default::default() };
        let (omf, _) = filmcraft_interchange::omf::export(&s.project, sequence, &omf_options).unwrap();
        for (kind, document) in [("AAF", aaf), ("OMF", omf)] {
            let (imported, extracted, _) = if kind == "AAF" {
                filmcraft_interchange::aaf::import(&document, &Default::default()).unwrap()
            } else {
                filmcraft_interchange::omf::import(&document, &Default::default()).unwrap()
            };
            assert_eq!(extracted.len(), 3, "{kind}: composition memo must retain distinct selected-stream essence");
            let tracks = &imported.project.sequence(imported.sequences[0]).unwrap().audio_tracks;
            assert_eq!(tracks.len(), 3, "{kind}");
            let mut source_ids: Vec<_> = tracks.iter().map(|track| track.items[0].item).collect();
            source_ids.sort();
            source_ids.dedup();
            assert_eq!(source_ids.len(), 3, "{kind}: each stream remains a distinct imported source");
            let mut waveforms: Vec<_> = extracted.iter().map(|media| media.wav.clone()).collect();
            waveforms.sort();
            waveforms.dedup();
            assert_eq!(waveforms.len(), 3, "{kind}");
        }
        assert!(calls.lock().unwrap().iter().any(|call| call.0 == 2));
        assert!(files.writes.lock().unwrap().is_empty());
        let error =
            s.execute("file.exportAaf", json!({"path": "must-not-be-written.aaf", "sequence": sequence.0, "audio": "linked", "trimAudio": false})).unwrap_err();
        assert!(error.to_string().contains("embedded") || error.to_string().contains("separate"));
        assert!(files.writes.lock().unwrap().is_empty());
    }
}
