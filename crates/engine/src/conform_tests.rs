//! Interpret Footage ▸ Assume this frame rate: footage conformed to another rate plays every frame
//! of the file at the assumed rate (picture and sound), lasts accordingly, and keeps its clips,
//! marks, markers and keyframes on their frames; `"file"` and undo go back. Alpha Channel ▸
//! Ignore / Invert change the pictures of footage with alpha.

use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::Session;
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaInfo, MediaKind, MediaSource};
use filmcraft_project::{ClipId, ItemId, ItemKind, Keyframe, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

const FILE_RATE: FrameRate = FrameRate::FPS_50;

fn secs(s: f64) -> Tick {
    Tick::from_seconds_f64(s)
}

/// Two seconds of 50 fps footage: frame `k` is grey level `2k` at alpha 64, its sound is a ramp
/// of the file time in seconds. It logs the file times it is asked for.
struct Counter {
    info: MediaInfo,
    asked: Arc<Mutex<Vec<Tick>>>,
}

impl MediaSource for Counter {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.asked.lock().unwrap().push(req.time);
        let v = (FILE_RATE.frame_at(req.time) * 2).clamp(0, 255) as u8;
        Ok(Arc::new(VideoFrame::rgba8(8, 8, [v, v, v, 64].repeat(64))))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        let ramp = (0..frames).map(|i| (start + i as i64) as f32 / sample_rate as f32).collect();
        Ok(AudioBuffer { sample_rate, channels: vec![ramp] })
    }
}

struct Setup {
    s: Session,
    item: ItemId,
    clip: ClipId,
    audio: ClipId,
    asked: Arc<Mutex<Vec<Tick>>>,
}

/// The footage with a mark In at 0.4 s (frame 20) and a marker at 1 s (frame 50), cut into a
/// 25 fps sequence at 0 from 0.2 s (frame 10) for one second, picture and sound, with an Opacity
/// keyframe at 0.6 s (frame 30).
fn setup() -> Setup {
    let g = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::ColorMatte { color: [0.5; 4] }, 8, 8, FILE_RATE, secs(2.0));
    let mut info = g.info().clone();
    info.kind = MediaKind::Movie;
    info.audio_streams = vec![AudioStreamInfo { sample_rate: 48_000, channels: 1, codec: "pcm".into(), bits_per_sample: None }];
    let mut p = Project::new("conform");
    let marker = filmcraft_project::Marker {
        id: filmcraft_project::MarkerId(900),
        start: secs(1.0),
        duration: Tick::ZERO,
        name: "frame 50".into(),
        comment: String::new(),
        kind: Default::default(),
        color: Label::Iris,
    };
    let media = MediaClip {
        media: MediaRef::Generator(g.generator.clone()),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: Some(secs(0.4)),
        mark_out: None,
        markers: vec![marker],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("counter", Label::Iris, ItemKind::Media(Box::new(media)), None);
    let rate = FrameRate::FPS_25;
    let seq = p.new_sequence("s", SequenceSettings { width: 8, height: 8, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let range = TimeRange::new(secs(0.2), secs(1.0));
    let mut v = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
    v.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().keyframes =
        vec![Keyframe::new(secs(0.6), filmcraft_project::ParamValue::Float(50.0)), Keyframe::new(secs(1.1), filmcraft_project::ParamValue::Float(100.0))];
    let a = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate).unwrap();
    let (clip, audio) = (v.id, a.id);
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.push(v);
    q.audio_tracks[0].items.push(a);
    let mut s = Session { project: Arc::new(p), ..Default::default() };
    s.state.active_sequence = Some(seq);
    let asked = Arc::new(Mutex::new(Vec::new()));
    // the file has alpha; the project item says so only where a test wants it
    if let Some(v) = info.video.as_mut() {
        v.has_alpha = true;
    }
    s.media.insert(item, Arc::new(Counter { info, asked: asked.clone() }));
    Setup { s, item, clip, audio, asked }
}

fn media(s: &Session, item: ItemId) -> MediaClip {
    s.project.item(item).unwrap().as_media().unwrap().clone()
}

fn track_item(s: &Session, clip: ClipId) -> filmcraft_project::TrackItem {
    let q = s.active_sequence().unwrap();
    q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items).find(|t| t.id == clip).unwrap().clone()
}

/// The file time the decoder is asked for when the Program monitor shows sequence time `t`.
fn program_file_time(su: &mut Setup, t: Tick) -> Tick {
    su.asked.lock().unwrap().clear();
    su.s.render_program_at(1.0, t).unwrap();
    *su.asked.lock().unwrap().last().expect("the clip's frame was decoded")
}

#[test]
fn assumed_rate_conforms_picture_sound_and_every_media_time() {
    let mut su = setup();
    // at the file's rate: sequence frame 2 (0.08 s) shows file frame 14 (0.2 s + 0.08 s)
    assert_eq!(FILE_RATE.frame_at(program_file_time(&mut su, secs(0.08))), 14);
    let r = su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 25})).unwrap();
    assert_eq!((r["frameRate"].as_str(), r["fileFrameRate"].as_str()), (Some("25"), Some("50")), "{r}");
    let m = media(&su.s, su.item);
    assert_eq!((m.interpret.frame_rate, m.frame_rate()), (Some(FrameRate::FPS_25), FrameRate::FPS_25));
    // 100 frames at 25 fps: twice as long, with marks and markers on the same frames
    assert_eq!(m.duration(), secs(4.0));
    assert_eq!(m.mark_in, Some(secs(0.8)));
    assert_eq!(m.markers[0].start, secs(2.0));
    // the clips keep their place and length on the timeline and start on frame 10 (now at 0.4 s);
    // the keyframes stay on frames 30 and 55
    for c in [su.clip, su.audio] {
        let ti = track_item(&su.s, c);
        assert_eq!((ti.start, ti.duration, ti.source_in), (Tick::ZERO, secs(1.0), secs(0.4)));
    }
    let kf: Vec<Tick> = track_item(&su.s, su.clip).effect("opacity").unwrap().params["opacity"].keyframes.iter().map(|k| k.time).collect();
    assert_eq!(kf, [secs(1.2), secs(2.2)]);
    // the picture plays at half speed: sequence frames 2 and 3 show file frames 12 and 13
    assert_eq!(FILE_RATE.frame_at(program_file_time(&mut su, secs(0.08))), 12);
    assert_eq!(FILE_RATE.frame_at(program_file_time(&mut su, secs(0.12))), 13);
    // the source reports the conformed duration and rate, and every frame boundary lands on its frame
    let src = su.s.media.source_for(&su.s.project, su.item, &*su.s.services).unwrap();
    assert_eq!((src.info().duration, src.info().frame_rate()), (secs(4.0), FrameRate::FPS_25));
    for k in 0..100 {
        let f = src.video_frame(FrameRequest::full(FrameRate::FPS_25.tick_of(k))).unwrap();
        assert_eq!(i64::from(f.to_rgba8()[0]), 2 * k, "frame {k}");
    }
    // the sound is conformed with it: one second in plays the file's half second, at 48 kHz
    let a = src.audio(48_000, 4, 48_000).unwrap();
    assert_eq!(a.sample_rate, 48_000);
    assert!((a.channels[0][0] - 0.5).abs() < 1e-6, "{:?}", a.channels[0]);
    // the Source monitor and New Sequence From Clip see the assumed rate
    su.s.execute("file.newSequenceFromClip", json!({"items": [su.item.0]})).unwrap();
    assert_eq!(su.s.active_sequence().unwrap().settings.frame_rate, FrameRate::FPS_25);
}

#[test]
fn file_rate_and_undo_bring_every_frame_back() {
    let mut su = setup();
    // compared as JSON: the auto anchor and position are NaN, which never equals itself
    let json_of = |s: &Session| serde_json::to_string(&*s.project).unwrap();
    let before = json_of(&su.s);
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": "24000/1001"})).unwrap();
    assert_eq!(media(&su.s, su.item).frame_rate(), FrameRate::FPS_23_976);
    // undo restores the project exactly
    su.s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(json_of(&su.s), before);
    // "file" maps every time back onto the file's frames
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 25})).unwrap();
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": "file"})).unwrap();
    let m = media(&su.s, su.item);
    assert_eq!((m.interpret.frame_rate, m.duration(), m.mark_in), (None, secs(2.0), Some(secs(0.4))));
    let ti = track_item(&su.s, su.clip);
    assert_eq!((ti.duration, ti.source_in), (secs(1.0), secs(0.2)));
    assert_eq!(FILE_RATE.frame_at(program_file_time(&mut su, secs(0.08))), 14);
}

#[test]
fn a_faster_rate_shortens_clips_that_would_run_past_the_end() {
    let mut su = setup();
    // 100 frames at 100 fps last one second; the clip starts on frame 10 (0.1 s) and would run to 1.1 s
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 100})).unwrap();
    assert_eq!(media(&su.s, su.item).duration(), secs(1.0));
    for c in [su.clip, su.audio] {
        let ti = track_item(&su.s, c);
        assert_eq!((ti.source_in, ti.duration), (secs(0.1), secs(0.9)));
        assert!(ti.source_out() <= secs(1.0));
    }
}

#[test]
fn hostile_frame_rates_are_refused_and_change_nothing() {
    let mut su = setup();
    let rev = su.s.revision;
    for bad in [json!(0), json!(-25), json!(1001), json!(1e300), json!("fast"), json!("25/0"), json!("0/1"), json!("-25/1"), json!([25, 1]), json!({"fps": 25})]
    {
        assert!(su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": bad})).is_err(), "{bad}");
    }
    assert_eq!((su.s.revision, media(&su.s, su.item).interpret.frame_rate), (rev, None));
    // the same rate as the file's changes no time
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 50})).unwrap();
    let m = media(&su.s, su.item);
    assert_eq!((m.conform(), m.duration(), track_item(&su.s, su.clip).source_in), (None, secs(2.0), secs(0.2)));
    assert_eq!(Tick(TICKS_PER_SECOND), secs(1.0));
}

#[test]
fn ignore_and_invert_alpha_change_footage_with_alpha() {
    let mut su = setup();
    let alpha_of = |su: &Setup| {
        let src = su.s.media.source_for(&su.s.project, su.item, &*su.s.services).unwrap();
        (src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().to_rgba8()[3], src.info().video.as_ref().unwrap().has_alpha)
    };
    // footage without alpha: the options change nothing
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "invertAlpha": true})).unwrap();
    assert_eq!(media(&su.s, su.item).alpha(), None);
    assert_eq!(alpha_of(&su).0, 64);
    // with alpha: Invert, then Ignore (which wins), then neither
    let mut p = (*su.s.project).clone();
    p.item_mut(su.item).and_then(|i| i.as_media_mut()).unwrap().info.video.as_mut().unwrap().has_alpha = true;
    su.s.project = Arc::new(p);
    assert_eq!(alpha_of(&su), (191, true));
    let r = su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "ignoreAlpha": true})).unwrap();
    assert_eq!((r["ignoreAlpha"].as_bool(), r["invertAlpha"].as_bool()), (Some(true), Some(true)), "{r}");
    assert_eq!(alpha_of(&su), (255, false));
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "ignoreAlpha": false, "invertAlpha": false})).unwrap();
    assert_eq!(alpha_of(&su), (64, true));
    // with a conformed rate too
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 25, "invertAlpha": true})).unwrap();
    assert_eq!(alpha_of(&su), (191, true));
    assert!(su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "ignoreAlpha": "yes"})).is_err());
}

#[test]
fn inverting_premultiplied_and_planar_alpha() {
    use filmcraft_frame::{Chroma, PixelData};
    use filmcraft_project::AlphaInterpret::{Ignore, Invert};
    let f = |data| VideoFrame { width: 1, height: 1, data, color: Default::default(), par: (1, 1), pts: Tick::ZERO };
    let px = |v: &VideoFrame| match &v.data {
        PixelData::RgbaF32(p) => p.as_ref().clone(),
        _ => unreachable!(),
    };
    // premultiplied 0.5 grey at 25 %: inverted it is the same grey at 75 %
    let half = f(PixelData::RgbaF32(Arc::new(vec![0.125, 0.125, 0.125, 0.25])));
    assert_eq!(px(&crate::media_pool::interpret_alpha(&half, Invert)), [0.375, 0.375, 0.375, 0.75]);
    assert_eq!(px(&crate::media_pool::interpret_alpha(&half, Ignore)), [0.125, 0.125, 0.125, 1.0]);
    // hostile values stay finite
    let nan = f(PixelData::RgbaF32(Arc::new(vec![0.5, 0.5, 0.5, f32::NAN])));
    assert!(px(&crate::media_pool::interpret_alpha(&nan, Invert)).iter().all(|v| v.is_finite()));
    // a 10-bit alpha plane inverts within 0..=1023; Ignore drops it
    let y = |a: Vec<u16>| {
        f(PixelData::Yuv16 { planes: [Arc::new(vec![64]), Arc::new(vec![512]), Arc::new(vec![512])], chroma: Chroma::C444, bits: 10, alpha: Some(Arc::new(a)) })
    };
    match crate::media_pool::interpret_alpha(&y(vec![1023]), Invert).data {
        PixelData::Yuv16 { alpha: Some(a), .. } => assert_eq!(*a, [0]),
        _ => panic!("no alpha"),
    }
    assert!(matches!(crate::media_pool::interpret_alpha(&y(vec![7]), Ignore).data, PixelData::Yuv16 { alpha: None, .. }));
}

/// The media's split marks and transcript keep their frames and words, and the media pool lets go
/// of a conformed source when the item is removed or the pool is cleared.
#[test]
fn split_marks_transcript_and_pooled_sources_follow_the_assumed_rate() {
    let mut su = setup();
    let mut p = (*su.s.project).clone();
    p.item_mut(su.item).unwrap().split.audio_in = Some(secs(0.4));
    let word = filmcraft_project::Word { text: "hi".into(), start: secs(0.2), end: secs(0.6), speaker: None, confidence: 1.0 };
    p.transcripts.insert(su.item, Arc::new(filmcraft_project::Transcript { language: "en".into(), words: vec![word], ..Default::default() }));
    su.s.project = Arc::new(p);
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 25})).unwrap();
    assert_eq!(su.s.project.item(su.item).unwrap().split.audio_in, Some(secs(0.8)));
    let w = &su.s.project.transcripts[&su.item].words[0];
    assert_eq!((w.start, w.end), (secs(0.4), secs(1.2)));
    program_file_time(&mut su, secs(0.08));
    assert_eq!(su.s.media.interpreted_len(), 1, "the conformed source is kept for reuse");
    su.s.media.remove(su.item);
    assert_eq!(su.s.media.interpreted_len(), 0);
    let mut su = setup();
    su.s.execute("clip.interpretFootage", json!({"items": [su.item.0], "frameRate": 25})).unwrap();
    program_file_time(&mut su, secs(0.08));
    su.s.media.clear();
    assert_eq!(su.s.media.interpreted_len(), 0);
}
