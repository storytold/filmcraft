//! Synchronisation and multi-camera editing end to end.
//!
//! Fixtures (ffmpeg, `target/fixtures/multicam/`): one "event" (pink noise gated into bursts, so
//! it is not periodic) recorded by three devices that started at different times — two cameras
//! (MJPEG + PCM `.mov` with QuickTime timecode) and an external recorder (`.wav`) — each with its
//! own gain and independent noise. The true offsets are known to the sample.
//!
//! The editing tests use movies made with our own encoders (no ffmpeg).

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, Marker, MarkerId, MarkerKind, MulticamAudio, Sequence, TrackItem};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{make_movie, psnr, tmp_dir};

const SR: i64 = 48_000;
/// Event samples at which each recording starts (camera A, camera B, recorder).
const START_A: i64 = 96_000;
const START_B: i64 = 96_000 + 121_234;
const START_REC: i64 = 48_000;
const SAMPLE: Tick = Tick(TICKS_PER_SECOND / SR);

fn ff(ffmpeg: &Path, args: &[&str]) -> bool {
    Command::new(ffmpeg).args(["-v", "error", "-y"]).args(args).status().map(|s| s.success()).unwrap_or(false)
}

/// The event as heard by one device: starts at event sample `start`, `secs` long, `gain`, white
/// noise of amplitude `noise` (seed `seed`).
fn mic_filter(start: i64, secs: f64, gain: f64, noise: f64, seed: u32) -> String {
    let len = (secs * SR as f64) as i64;
    format!(
        "anoisesrc=d=60:c=pink:r=48000:a=0.35:s=7,volume='if(lt(mod(t*2.7\\,1)\\,0.55)\\,1\\,0.08)':eval=frame,atrim=start_sample={start},asetpts=N/SR/TB,volume={gain}[s];\
         anoisesrc=d=60:c=white:r=48000:a={noise}:s={seed}[n];[s][n]amix=inputs=2:normalize=0:duration=first,atrim=end_sample={len}[a]"
    )
}

/// (camera A .mov, camera B .mov, recorder .wav), or None without ffmpeg.
fn fixtures() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let ffmpeg = filmcraft_testkit::ffmpeg_or_skip("multicam fixtures")?;
    let dir = filmcraft_testkit::fixtures_dir("multicam");
    let cam = |name: &str, src: &str, start: i64, secs: f64, gain: f64, noise: f64, seed: u32, tc: &str| {
        filmcraft_testkit::fixtures::generate(&dir.join(name), |tmp| {
            let t = tmp.to_string_lossy();
            let video = format!("{src}=s=320x180:r=24:d={secs}");
            ff(
                &ffmpeg,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    &video,
                    "-filter_complex",
                    &mic_filter(start, secs, gain, noise, seed),
                    "-map",
                    "0:v",
                    "-map",
                    "[a]",
                    "-c:v",
                    "mjpeg",
                    "-q:v",
                    "8",
                    "-c:a",
                    "pcm_s16le",
                    "-ar",
                    "48000",
                    "-ac",
                    "1",
                    "-timecode",
                    tc,
                    "-shortest",
                    &t,
                ],
            )
        })
    };
    let a = cam("camA.mov", "testsrc2", START_A, 12.0, 1.0, 0.02, 11, "01:00:00:00")?;
    let b = cam("camB.mov", "smptebars", START_B, 10.0, 0.25, 0.05, 12, "01:00:02:12")?;
    let rec = filmcraft_testkit::fixtures::generate(&dir.join("recorder.wav"), |tmp| {
        let t = tmp.to_string_lossy();
        ff(
            &ffmpeg,
            &[
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000",
                "-filter_complex",
                &mic_filter(START_REC, 16.0, 1.5, 0.003, 13),
                "-map",
                "[a]",
                "-c:a",
                "pcm_s16le",
                "-ac",
                "1",
                &t,
            ],
        )
    })?;
    Some((a, b, rec))
}

fn import(s: &mut Session, files: &[&Path]) -> Vec<ItemId> {
    let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let r = s.execute("file.import", json!({"paths": paths})).unwrap();
    r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect()
}

fn ids(v: &[ItemId]) -> Value {
    json!(v.iter().map(|i| i.0).collect::<Vec<_>>())
}

/// Run a Project-panel command on `items` (selected first, as in the UI).
fn on_items(s: &mut Session, cmd: &str, items: &[ItemId], mut p: Value) -> crate::Result<Value> {
    s.execute("project.select", json!({"items": ids(items)})).unwrap();
    p["items"] = ids(items);
    s.execute(cmd, p)
}

/// Timeline time at which event sample `m` plays in clip `ti` (whose media starts at event sample
/// `start`).
fn event_time(ti: &TrackItem, start: i64, m: i64) -> Tick {
    let media = Tick::from_units(m - start, SR);
    ti.start + (media - ti.source_in)
}

fn find_clip(q: &Sequence, item: ItemId, video: bool) -> &TrackItem {
    let tracks = if video { &q.video_tracks } else { &q.audio_tracks };
    tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.item == item).unwrap_or_else(|| panic!("no clip of {item:?}"))
}

#[test]
fn audio_sync_multicam_sample_accurate() {
    let Some((a, b, rec)) = fixtures() else { return };
    let mut s = Session::default();
    let items = import(&mut s, &[&a, &b, &rec]);
    let t0 = std::time::Instant::now();
    let r =
        on_items(&mut s, "clip.createMulticam", &items, json!({"method": "audio", "audio": "switch", "name": "Interview MC", "processedBin": true})).unwrap();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let seq_id = ItemId(r["sequence"].as_u64().unwrap());
    let q = s.project.sequence(seq_id).unwrap().clone();
    // structure: two cameras with video, an audio-only recorder, switching audio
    let mc = q.multicam.as_ref().unwrap();
    assert_eq!(mc.cameras.len(), 3);
    assert_eq!(q.video_tracks.len(), 2);
    assert_eq!(q.audio_tracks.len(), 3);
    assert_eq!(mc.audio, MulticamAudio::SwitchAudio);
    assert_eq!(mc.cameras[0].name, "camA.mov");
    assert!(mc.cameras[2].video_track.is_none() && mc.cameras[2].audio_tracks.len() == 1, "the recorder is an audio-only source");
    assert_eq!(q.video_tracks[0].name, "camA.mov");
    assert_eq!(s.project.item(seq_id).unwrap().type_label(), "Multi-Camera Source Sequence");
    // processed clips moved into their bin
    let bin = s.project.root.children.iter().find_map(|c| match c {
        filmcraft_project::BinEntry::Bin(b) if b.name == "Processed Clips" => Some(b.clone()),
        _ => None,
    });
    assert_eq!(bin.map(|b| b.children.len()), Some(3));
    // every recording plays event sample m at the same timeline time (±1 sample)
    let clips = [(find_clip(&q, items[0], false), START_A), (find_clip(&q, items[1], false), START_B), (find_clip(&q, items[2], false), START_REC)];
    let m = 300_000;
    let t_ref = event_time(clips[0].0, clips[0].1, m);
    let mut worst = Tick::ZERO;
    for (c, start) in &clips[1..] {
        let d = (event_time(c, *start, m) - t_ref).abs();
        worst = worst.max(d);
    }
    // video clips sit on frame boundaries and keep the same alignment as their audio
    for (k, item) in items[..2].iter().enumerate() {
        let v = find_clip(&q, *item, true);
        assert_eq!(v.start, q.settings.frame_rate.snap(v.start), "frame-aligned");
        assert_eq!((v.start, v.source_in), (clips[k].0.start, clips[k].0.source_in));
    }
    let report = &r["clips"];
    eprintln!(
        "audio sync: worst error {:.2} samples over 3 recordings; B lag {} (truth {}), recorder lag {} (truth {}); {ms:.0} ms",
        worst.0 as f64 / SAMPLE.0 as f64,
        report[1]["audio"]["lagSamples"],
        START_B - START_A,
        report[2]["audio"]["lagSamples"],
        START_REC - START_A
    );
    assert!(worst <= SAMPLE, "worst {} samples", worst.0 as f64 / SAMPLE.0 as f64);
    assert!(report[1]["audio"]["reliable"].as_bool().unwrap() && report[2]["audio"]["reliable"].as_bool().unwrap(), "{report}");
    // undo removes the sequence and restores the bins
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.sequence(seq_id).is_none());
}

#[test]
fn timecode_marker_and_in_point_sync_within_one_frame() {
    let Some((a, b, _)) = fixtures() else { return };
    let mut s = Session::default();
    let items = import(&mut s, &[&a, &b]);
    let truth = Tick::from_units(START_B - START_A, SR);
    let rate = FrameRate::FPS_24;
    let offset_of = |s: &Session, r: &Value| -> Tick {
        let q = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap();
        let (va, vb) = (find_clip(q, items[0], true), find_clip(q, items[1], true));
        // timeline time of media 0 of each camera; B starts `truth` after A in the event
        (vb.start - vb.source_in) - (va.start - va.source_in)
    };
    // timecode: A 01:00:00:00, B 01:00:02:12 = 2.5 s later (the true offset is 2.5257 s)
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "timecode"})).unwrap();
    let off = offset_of(&s, &r);
    assert_eq!(off, rate.tick_of(60), "timecode offset is exactly 60 frames");
    assert!((off - truth).abs() <= rate.frame_duration(), "within a frame of the truth");
    // ignoring hours gives the same result here
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "timecode", "ignoreHours": true})).unwrap();
    assert_eq!(offset_of(&s, &r), rate.tick_of(60));
    // clip markers: the same event moment marked in both clips (frame accuracy)
    let event_frame_a = 72; // 3 s into camera A
    let at_b = rate.tick_of(event_frame_a) - truth;
    let mut p = (*s.project).clone();
    for (it, t) in [(items[0], rate.tick_of(event_frame_a)), (items[1], rate.snap(at_b))] {
        let m = p.item_mut(it).unwrap().as_media_mut().unwrap();
        m.markers.push(Marker {
            id: MarkerId(900 + it.0),
            start: t,
            duration: Tick::ZERO,
            name: "clap".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: filmcraft_project::Label::Rose,
        });
    }
    s.project = std::sync::Arc::new(p);
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "marker", "marker": "clap"})).unwrap();
    let off = offset_of(&s, &r);
    assert!((off - truth).abs() <= rate.frame_duration(), "marker sync {} vs {}", off.0, truth.0);
    // In points: A's In at 2.5 s, B's at 0 → B starts 2.5 s after A
    let mut p = (*s.project).clone();
    p.item_mut(items[0]).unwrap().as_media_mut().unwrap().mark_in = Some(rate.tick_of(60));
    s.project = std::sync::Arc::new(p);
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "in"})).unwrap();
    assert_eq!(offset_of(&s, &r), rate.tick_of(60));
    // Out points: A ends at 12 s, B at 10 s, both Outs unset → ends aligned: B media 0 at 2 s
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "out"})).unwrap();
    assert_eq!(offset_of(&s, &r), rate.tick_of(48));
    // a missing marker is an error, not a guess
    assert!(on_items(&mut s, "clip.createMulticam", &items, json!({"method": "marker", "marker": "nope"})).is_err());
}

#[test]
fn merge_clips_by_audio() {
    let Some((a, _, rec)) = fixtures() else { return };
    let mut s = Session::default();
    let items = import(&mut s, &[&a, &rec]);
    let r = on_items(&mut s, "clip.mergeClips", &items, json!({"method": "audio", "removeVideoAudio": true})).unwrap();
    let id = ItemId(r["item"].as_u64().unwrap());
    let q = s.project.sequence(id).unwrap();
    assert_eq!(s.project.item(id).unwrap().type_label(), "Merged Clip");
    assert_eq!(s.project.item(id).unwrap().name, "camA.mov - Merged");
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 1), "camera audio removed");
    let v = find_clip(q, items[0], true);
    let ar = find_clip(q, items[1], false);
    assert_eq!(v.link, ar.link, "merged parts are linked");
    // the recorder's event sample m plays with the camera's
    let m = 400_000;
    let d = (event_time(ar, START_REC, m) - (v.start + (Tick::from_units(m - START_A, SR) - v.source_in))).abs();
    assert!(d <= SAMPLE, "merge offset error {} samples", d.0 as f64 / SAMPLE.0 as f64);
    assert!(on_items(&mut s, "clip.mergeClips", &items[..1], json!({})).is_err(), "needs audio clips");
}

#[test]
fn timeline_synchronize_by_audio_and_timecode() {
    let Some((a, b, rec)) = fixtures() else { return };
    let mut s = Session::default();
    let items = import(&mut s, &[&a, &b, &rec]);
    let mut p = (*s.project).clone();
    let rate = FrameRate::FPS_24;
    let seq = p.new_sequence("Edit", filmcraft_project::SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() }, 2, 3, None);
    // camera B on V2/A2 at 20 s, recorder on A3 at 1 s, camera A on V1/A1 at 5 s
    let place = |p: &mut filmcraft_project::Project, it: ItemId, at: i64, v: Option<usize>, a: usize| {
        let d = p.item(it).unwrap().duration();
        let range = filmcraft_time::TimeRange::new(Tick::ZERO, d);
        let link = p.alloc_id();
        if let Some(vt) = v {
            let mut vi = p.make_track_item(it, filmcraft_project::TrackKind::Video, rate.tick_of(at), range, rate).unwrap();
            vi.link = Some(link);
            p.sequence_mut(seq).unwrap().video_tracks[vt].items.push(vi);
        }
        let mut ai = p.make_track_item(it, filmcraft_project::TrackKind::Audio, rate.tick_of(at), range, rate).unwrap();
        ai.link = Some(link);
        p.sequence_mut(seq).unwrap().audio_tracks[a].items.push(ai);
    };
    place(&mut p, items[0], 120, Some(0), 0);
    place(&mut p, items[1], 480, Some(1), 1);
    place(&mut p, items[2], 24, None, 2);
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(seq);
    let all: Vec<u64> = s.project.sequence(seq).unwrap().all_tracks().flat_map(|t| t.items.iter().map(|i| i.id.0)).collect();
    s.execute("timeline.select", json!({"clips": all})).unwrap();
    let r = s.execute("clip.synchronize", json!({"method": "audio"})).unwrap();
    assert_eq!(r["moved"], 2, "{r}");
    let q = s.active_sequence().unwrap().clone();
    let ca = find_clip(&q, items[0], false);
    assert_eq!(ca.start, rate.tick_of(120), "the reference (V1) stays put");
    let m = 350_000;
    for (it, start) in [(items[1], START_B), (items[2], START_REC)] {
        let c = find_clip(&q, it, false);
        let d = (event_time(c, start, m) - event_time(ca, START_A, m)).abs();
        assert!(d <= SAMPLE, "timeline sync error {} samples", d.0 as f64 / SAMPLE.0 as f64);
        assert_eq!(c.start, rate.snap(c.start), "moved clips stay on frames");
    }
    let vb = find_clip(&q, items[1], true);
    let ab = find_clip(&q, items[1], false);
    assert_eq!((vb.start, vb.source_in), (ab.start, ab.source_in), "linked partners moved together");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(find_clip(s.active_sequence().unwrap(), items[1], false).start, rate.tick_of(480));
    // timecode: B lands exactly 60 frames after A
    s.execute("timeline.select", json!({"clips": all})).unwrap();
    let r = s.execute("clip.synchronize", json!({"method": "timecode", "track": "V1"})).unwrap();
    assert!(r["moved"].as_u64().unwrap() >= 1);
    let q = s.active_sequence().unwrap();
    assert_eq!(find_clip(q, items[1], true).start - find_clip(q, items[0], true).start, rate.tick_of(60));
}

// ------------------------------------------------------------------ editing (our own media)

const W: u32 = 160;
const H: u32 = 90;

/// Three 4-second cameras (different scenes, own audio), a multi-camera source sequence by In
/// points, and a sequence holding one multi-camera clip on V1/A1. Returns (session, camera items,
/// source sequence, edit sequence).
fn mc_session(audio: &str) -> (Session, Vec<ItemId>, ItemId, ItemId) {
    let dir = tmp_dir("mc-edit");
    let files: Vec<PathBuf> = ["a.mov", "b.mov", "c.mov"].iter().map(|f| dir.join(f)).collect();
    for (f, sc) in files.iter().zip([DemoScene::OceanSunset, DemoScene::CityNight, DemoScene::Aurora]) {
        make_movie(f, sc, W, H, 96);
    }
    let mut s = Session::default();
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
    let items = import(&mut s, &refs);
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "in", "audio": audio, "cameraNames": "track"})).unwrap();
    let src = ItemId(r["sequence"].as_u64().unwrap());
    let mut p = (*s.project).clone();
    let edit =
        p.new_sequence("Cut", filmcraft_project::SequenceSettings { width: W, height: H, frame_rate: FrameRate::FPS_24, ..Default::default() }, 2, 2, None);
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(edit);
    s.execute("source.open", json!({"item": src.0})).unwrap();
    s.execute("source.overwrite", json!({})).unwrap();
    s.set_playhead(Tick::ZERO);
    (s, items, src, edit)
}

fn v1(s: &Session) -> Vec<(i64, u32)> {
    let q = s.active_sequence().unwrap();
    let rate = q.settings.frame_rate;
    q.video_tracks[0].items.iter().map(|i| (rate.frame_at(i.start), i.multicam.unwrap().angle)).collect()
}

fn a1(s: &Session) -> Vec<(i64, u32)> {
    let q = s.active_sequence().unwrap();
    let rate = q.settings.frame_rate;
    q.audio_tracks[0].items.iter().map(|i| (rate.frame_at(i.start), i.multicam.unwrap().angle)).collect()
}

fn frame(s: &mut Session, f: i64) -> Vec<u8> {
    let t = s.sequence_rate().tick_of(f);
    s.set_playhead(t);
    s.render_program(1.0).unwrap().over_black_rgba8()
}

/// The frame of camera `item` at its media frame `f`, rendered alone.
fn camera_frame(s: &Session, item: ItemId, f: i64) -> Vec<u8> {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_render::render_item(&s.project, item, FrameRate::FPS_24.tick_of(f), 1.0, &provider).unwrap().unwrap().over_black_rgba8()
}

#[test]
fn multicam_clip_structure_and_render() {
    let (mut s, items, src, _) = mc_session("camera1");
    let q = s.project.sequence(src).unwrap();
    let mc = q.multicam.as_ref().unwrap();
    assert_eq!(mc.cameras.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Camera 1", "Camera 2", "Camera 3"]);
    assert_eq!(mc.audio, MulticamAudio::Camera1);
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (3, 3));
    // edited in as an enabled multi-camera clip on angle 0 (video and audio)
    assert_eq!(v1(&s), [(0, 0)]);
    assert_eq!(a1(&s), [(0, 0)]);
    let inspect = s.execute("multicam.inspect", json!({"frame": 10})).unwrap();
    assert_eq!(inspect["source"], src.0);
    assert_eq!(inspect["shown"], json!([0, 1, 2]));
    // the clip shows angle 0 only (not the composite, whose top track is camera 3)
    assert!(psnr(&frame(&mut s, 10), &camera_frame(&s, items[0], 10)) > 40.0);
    s.execute("multicam.switchAngle", json!({"camera": 3})).unwrap();
    assert_eq!(v1(&s), [(0, 2)]);
    assert!(psnr(&frame(&mut s, 10), &camera_frame(&s, items[2], 10)) > 40.0);
    // disabling multi-camera shows the plain nest (top track = camera 3 here, so pick camera 2)
    s.execute("multicam.switchAngle", json!({"camera": 2})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.execute("clip.multicamEnable", json!({"enabled": false})).unwrap();
    assert!(psnr(&frame(&mut s, 10), &camera_frame(&s, items[2], 10)) > 40.0, "plain nest: the top track");
    s.execute("clip.multicamEnable", json!({})).unwrap();
    assert!(psnr(&frame(&mut s, 10), &camera_frame(&s, items[1], 10)) > 40.0, "re-enabled, angle remembered");
    // the GPU plan draws the angle's media frame directly (one layer, no CPU pass) and matches
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let edit_id = s.state.active_sequence.unwrap();
    let t10 = FrameRate::FPS_24.tick_of(10);
    let plan = filmcraft_render::plan::plan_frame(&s.project, edit_id, t10, filmcraft_render::RenderOptions::default(), &provider).unwrap();
    match &plan {
        filmcraft_render::plan::FramePlan::Layers { layers, .. } => {
            assert_eq!(layers.len(), 1);
            assert!(!matches!(layers[0].frame.data, filmcraft_frame::PixelData::RgbaF32(_)), "a decoded frame, not a CPU render");
        }
        _ => panic!("expected layers"),
    }
    let via_plan = filmcraft_render::plan::execute_cpu(&plan).unwrap();
    s.set_playhead(t10);
    let reference = s.render_program(1.0).unwrap();
    assert!(psnr(&via_plan.over_black_rgba8(), &reference.over_black_rgba8()) > 40.0);
    // grid of the angles
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let (grid, angles) = filmcraft_render::multicam::render_grid(&s.project, src, Tick::ZERO, 0.25, &provider).unwrap().unwrap();
    assert_eq!(angles, [0, 1, 2]);
    assert_eq!((grid.w, grid.h), (2 * 40, 2 * 23));
}

#[test]
fn switch_audio_follows_video() {
    let (mut s, _, src, _) = mc_session("switch");
    // audio follows video off (Premiere's default): only the picture switches
    s.execute("multicam.switchAngle", json!({"camera": 2, "frame": 5})).unwrap();
    assert_eq!((v1(&s), a1(&s)), (vec![(0, 1)], vec![(0, 0)]));
    s.execute("multicam.audioFollowsVideo", json!({"enabled": true})).unwrap();
    s.execute("multicam.switchAngle", json!({"camera": 3})).unwrap();
    assert_eq!((v1(&s), a1(&s)), (vec![(0, 2)], vec![(0, 2)]));
    // video only (Ctrl-click) leaves the audio alone; audio only switches just the audio
    s.execute("multicam.switchAngle", json!({"camera": 1, "videoOnly": true})).unwrap();
    assert_eq!((v1(&s), a1(&s)), (vec![(0, 0)], vec![(0, 2)]));
    s.execute("multicam.switchAngle", json!({"camera": 2, "audioOnly": true})).unwrap();
    assert_eq!((v1(&s), a1(&s)), (vec![(0, 0)], vec![(0, 1)]));
    // switching audio plays the selected angle's track of the source
    let q = s.project.sequence(src).unwrap();
    let on = q.multicam.as_ref().unwrap().audible_tracks(Some(1));
    assert_eq!(on, q.multicam.as_ref().unwrap().cameras[1].audio_tracks);
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let edit_seq = s.active_sequence().unwrap().clone();
    let mix = filmcraft_render::audio::mix_sequence(&s.project, &edit_seq, 24_000, 4_800, &provider);
    let only_b = filmcraft_render::audio::mix_sequence(&s.project, &q.with_angle_audio(Some(1)), 24_000, 4_800, &provider);
    let err = mix.channels[0].iter().zip(&only_b.channels[0]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "multi-camera audio = camera 2's track ({err})");
    assert!(mix.channels[0].iter().any(|v| v.abs() > 1e-3), "audible");
    // undo steps back one switch at a time
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(a1(&s), [(0, 2)]);
}

/// Live switching from a simulated playback clock: play 0 → 84 at 24 fps, press 2 at frame 24,
/// 3 at frame 48, 3 again at 60 (no new edit), 1 at 72; stop at 84.
#[test]
fn live_switch_recording_from_playback_clock() {
    let (mut s, items, _, _) = mc_session("switch");
    s.execute("multicam.audioFollowsVideo", json!({"enabled": true})).unwrap();
    let undo_before = s.history.undo.len();
    let presses: [(i64, u64); 4] = [(24, 2), (48, 3), (60, 3), (72, 1)];
    let rate = s.sequence_rate();
    s.execute("multicam.recordStart", json!({"frame": 0})).unwrap();
    for f in 0..=84i64 {
        // the clock: one tick per frame
        s.set_playhead(rate.tick_of(f));
        if let Some((_, cam)) = presses.iter().find(|(pf, _)| *pf == f) {
            s.execute("multicam.cut", json!({"camera": cam})).unwrap();
            // the cut is live: the program shows the new angle from this frame on
            let want = items[*cam as usize - 1];
            assert!(psnr(&frame(&mut s, f + 2), &camera_frame(&s, want, f + 2)) > 40.0, "live cut at {f}");
            s.set_playhead(rate.tick_of(f));
        }
    }
    let r = s.execute("multicam.recordStop", json!({"frame": 84})).unwrap();
    assert_eq!(r["cuts"], 4);
    // 0–24 cam 1, 24–48 cam 2, 48–72 cam 3 (the second press of 3 adds no edit), 72–96 cam 1
    // (healed with the untouched rest of the clip after the stop point)
    let expect = vec![(0, 0), (24, 1), (48, 2), (72, 0)];
    assert_eq!(v1(&s), expect);
    assert_eq!(a1(&s), expect, "audio followed the video");
    assert_eq!(s.history.undo.len(), undo_before + 1, "one undo step for the whole pass");
    assert_eq!(s.history.undo.last().unwrap().0, "Record Multi-Camera");
    // pieces are continuous in source time
    let q = s.active_sequence().unwrap();
    for w in q.video_tracks[0].items.windows(2) {
        assert_eq!(w[0].source_out(), w[1].source_in);
        assert!(w[0].link.is_some());
    }
    // a second pass changes only its own range and is its own undo step
    s.execute("multicam.recordStart", json!({"frame": 30})).unwrap();
    s.execute("multicam.cut", json!({"camera": 3, "frame": 30})).unwrap();
    s.execute("multicam.recordStop", json!({"frame": 40})).unwrap();
    assert_eq!(v1(&s), vec![(0, 0), (24, 1), (30, 2), (40, 1), (48, 2), (72, 0)]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s), expect);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s), vec![(0, 0)]);
    // stopped: a key press switches the clip at the playhead instead of cutting
    s.execute("multicam.cut", json!({"camera": 2, "frame": 10})).unwrap();
    assert_eq!(v1(&s), vec![(0, 1)]);
}

#[test]
fn flatten_replaces_with_camera_clips() {
    let (mut s, items, _, _) = mc_session("switch");
    s.execute("multicam.audioFollowsVideo", json!({"enabled": true})).unwrap();
    s.execute("multicam.recordStart", json!({"frame": 0})).unwrap();
    s.execute("multicam.cut", json!({"camera": 2, "frame": 30})).unwrap();
    s.execute("multicam.cut", json!({"camera": 3, "frame": 60})).unwrap();
    s.execute("multicam.recordStop", json!({"frame": 96})).unwrap();
    let before: Vec<Vec<u8>> = [5, 35, 70].iter().map(|f| frame(&mut s, *f)).collect();
    let all: Vec<u64> = s.active_sequence().unwrap().all_tracks().flat_map(|t| t.items.iter().map(|i| i.id.0)).collect();
    s.execute("timeline.select", json!({"clips": all})).unwrap();
    let r = s.execute("clip.multicamFlatten", json!({})).unwrap();
    assert_eq!(r["flattened"], 6);
    let q = s.active_sequence().unwrap().clone();
    let v: Vec<(ItemId, i64, i64)> = q.video_tracks[0].items.iter().map(|i| (i.item, i.start.0, i.source_in.0)).collect();
    let rate = q.settings.frame_rate;
    assert_eq!(v, vec![(items[0], 0, 0), (items[1], rate.tick_of(30).0, rate.tick_of(30).0), (items[2], rate.tick_of(60).0, rate.tick_of(60).0)]);
    assert!(q.all_tracks().flat_map(|t| t.items.iter()).all(|i| i.multicam.is_none()));
    let a: Vec<ItemId> = q.audio_tracks[0].items.iter().map(|i| i.item).collect();
    assert_eq!(a, vec![items[0], items[1], items[2]], "audio flattened to the switched cameras");
    for (i, f) in [5, 35, 70].iter().enumerate() {
        assert!(psnr(&before[i], &frame(&mut s, *f)) > 45.0, "flattened frame {f} looks the same");
    }
    // linked camera video/audio stay linked
    assert_eq!(q.video_tracks[0].items[1].link, q.audio_tracks[0].items[1].link);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s).len(), 3);
}

#[test]
fn edit_cameras_and_save_load() {
    let (mut s, _, src, _) = mc_session("all");
    s.execute("multicam.editCameras", json!({"sequence": src.0, "cameras": [{"angle": 1, "name": "Wide", "enabled": false}], "audio": "switch"})).unwrap();
    let mc = s.project.sequence(src).unwrap().multicam.clone().unwrap();
    assert_eq!((mc.cameras[1].name.as_str(), mc.cameras[1].enabled, mc.audio), ("Wide", false, MulticamAudio::SwitchAudio));
    assert_eq!(mc.shown_angles(), [0, 2]);
    // keys pick from the shown cameras: camera 2 is angle 2 now
    s.execute("multicam.switchAngle", json!({"camera": 2})).unwrap();
    assert_eq!(v1(&s), [(0, 2)]);
    assert!(s.execute("multicam.switchAngle", json!({"camera": 3})).is_err());
    let dir = tmp_dir("mc-save");
    let path = dir.join("mc.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["schema_version"], filmcraft_format::SCHEMA_VERSION);
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(*t.project, *s.project, "multi-camera data survives save/load");
    assert_eq!(t.project.sequence(src).unwrap().multicam.as_ref().unwrap().cameras[1].name, "Wide");
}

#[test]
fn any_nest_can_be_multicam() {
    // a plain nested sequence with two video tracks: angles are its tracks
    let (mut s, items, _, _) = mc_session("camera1");
    let mut p = (*s.project).clone();
    let rate = FrameRate::FPS_24;
    let nest = p.new_sequence("Nest", filmcraft_project::SequenceSettings { width: W, height: H, frame_rate: rate, ..Default::default() }, 2, 0, None);
    for (k, it) in items[..2].iter().enumerate() {
        let mut v = p
            .make_track_item(*it, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate)
            .unwrap();
        for e in &mut v.effects {
            filmcraft_project::resolve_auto_points(e, (W, H), (W, H));
        }
        p.sequence_mut(nest).unwrap().video_tracks[k].items.push(v);
    }
    let edit = s.state.active_sequence.unwrap();
    let mut v =
        p.make_track_item(nest, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
    assert!(v.multicam.is_none(), "plain nests start as plain nests");
    for e in &mut v.effects {
        filmcraft_project::resolve_auto_points(e, (W, H), (W, H));
    }
    let clip = v.id;
    let q = p.sequence_mut(edit).unwrap();
    q.video_tracks[1].items.push(v);
    s.project = std::sync::Arc::new(p);
    assert!(psnr(&frame(&mut s, 3), &camera_frame(&s, items[1], 3)) > 40.0, "nest: top track");
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.execute("clip.multicamEnable", json!({})).unwrap();
    assert!(psnr(&frame(&mut s, 3), &camera_frame(&s, items[0], 3)) > 40.0, "angle 0 = track V1 of the nest");
    s.execute("multicam.switchAngle", json!({"clips": [clip.0], "angle": 1})).unwrap();
    assert!(psnr(&frame(&mut s, 3), &camera_frame(&s, items[1], 3)) > 40.0);
}

#[test]
fn camera_keys_cut_and_select() {
    let (mut s, _, _, _) = mc_session("camera1");
    // Ctrl+2 at frame 30: an edit there, camera 2 after it
    s.set_playhead(FrameRate::FPS_24.tick_of(30));
    s.execute("multicam.cutToCamera2", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 0), (30, 1)]);
    // Ctrl+3 at the existing edit: no new edit, the right piece switches
    s.execute("multicam.cutToCamera3", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 0), (30, 2)]);
    // 2 while stopped switches the clip under the playhead
    s.set_playhead(FrameRate::FPS_24.tick_of(10));
    s.execute("multicam.selectCamera2", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 1), (30, 2)]);
    // the keys are bound by default
    let keys: Vec<(String, String)> =
        s.shortcuts.bindings.iter().filter(|b| b.command.starts_with("multicam.")).map(|b| (b.command.clone(), b.keys.clone())).collect();
    assert!(keys.contains(&("multicam.selectCamera1".into(), "1".into())), "{keys:?}");
    assert!(keys.contains(&("multicam.cutToCamera2".into(), "Ctrl+2".into())), "{keys:?}");
    // ⌃9 is Premiere's macOS key; off macOS Ctrl+9 is Toggle All Audio Targets (⌘9 there), which
    // wins, so Cut to Camera 9 has no default key on Windows, Linux and FreeBSD
    let nine = keys.contains(&("multicam.cutToCamera9".into(), "Ctrl+9".into()));
    assert_eq!(nine, cfg!(target_os = "macos"), "{keys:?}");
}

/// Colour of camera `k` (0-based) in the many-angle tests: distinct reds and greens.
fn cam_rgb(k: usize) -> [u8; 3] {
    [(k * 12) as u8, (250 - k * 12) as u8, 128]
}

/// `n` colour-matte cameras (64×36, 4 s) in a multi-camera source sequence, edited into a sequence
/// on V1/A1. Returns (session, source sequence).
fn many_angles(n: usize) -> (Session, ItemId) {
    let mut s = Session::default();
    let mut items = Vec::new();
    for k in 0..n {
        let [r, g, b] = cam_rgb(k);
        let hex = format!("#{r:02x}{g:02x}{b:02x}");
        let r = s.execute("file.newColorMatte", json!({"color": hex, "seconds": 4.0, "width": 64, "height": 36, "name": format!("Cam {}", k + 1)})).unwrap();
        items.push(ItemId(r["item"].as_u64().unwrap()));
    }
    let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "in", "cameraNames": "clip"})).unwrap();
    let src = ItemId(r["sequence"].as_u64().unwrap());
    let mut p = (*s.project).clone();
    let edit =
        p.new_sequence("Cut", filmcraft_project::SequenceSettings { width: 64, height: 36, frame_rate: FrameRate::FPS_24, ..Default::default() }, 2, 2, None);
    s.project = std::sync::Arc::new(p);
    s.state.active_sequence = Some(edit);
    s.execute("source.open", json!({"item": src.0})).unwrap();
    s.execute("source.overwrite", json!({})).unwrap();
    s.set_playhead(FrameRate::FPS_24.tick_of(10));
    (s, src)
}

/// A media source that records the scale of every frame request.
struct Recording {
    inner: filmcraft_media::SharedSource,
    scales: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
}

impl filmcraft_media::MediaSource for Recording {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        self.inner.info()
    }
    fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<std::sync::Arc<filmcraft_frame::VideoFrame>> {
        self.scales.lock().unwrap().push(req.scale);
        self.inner.video_frame(req)
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
}

#[test]
fn more_than_sixteen_angles_page_and_switch() {
    let (mut s, src) = many_angles(20);
    assert_eq!(s.project.sequence(src).unwrap().multicam.as_ref().unwrap().cameras.len(), 20);
    // automatic layout: 4×4 pages, two of them
    let g = s.execute("multicam.grid", json!({})).unwrap();
    assert_eq!((g["cols"].as_u64(), g["rows"].as_u64(), g["pages"].as_u64(), g["page"].as_u64()), (Some(4), Some(4), Some(2), Some(0)));
    assert_eq!(g["cells"].as_array().unwrap().len(), 16);
    assert_eq!(g["cells"][0]["active"], true, "angle 0 is on");
    // page 2 holds cameras 17–20
    s.execute("multicam.nextPage", json!({})).unwrap();
    let g = s.execute("multicam.grid", json!({})).unwrap();
    let cams: Vec<u64> = g["cells"].as_array().unwrap().iter().map(|c| c["camera"].as_u64().unwrap()).collect();
    assert_eq!((g["page"].as_u64(), cams), (Some(1), vec![17, 18, 19, 20]));
    assert_eq!(g["cells"][3]["name"], "Cam 20");
    // past the last page clamps
    assert_eq!(s.execute("multicam.nextPage", json!({})).unwrap()["page"], 1);
    // keys 1–9 pick cameras on the shown page: 2 → camera 18 (angle 17)
    s.execute("multicam.selectCamera2", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 17)]);
    assert_eq!(a1(&s), [(0, 0)], "audio follows video is off");
    // absolute cameras beyond 16 switch from any page; angle is 0-based
    s.execute("multicam.page", json!({"page": 0})).unwrap();
    s.execute("multicam.cut", json!({"camera": 20})).unwrap();
    assert_eq!(v1(&s), [(0, 19)]);
    // Ctrl+3 on page 1 at frame 30: an edit, camera 3 after it
    s.set_playhead(FrameRate::FPS_24.tick_of(30));
    s.execute("multicam.cutToCamera3", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 19), (30, 2)]);
    // the program shows the switched angle's colour
    let px = frame(&mut s, 40);
    let c = &px[(18 * 64 + 32) * 4..][..3];
    assert!(c.iter().zip(cam_rgb(2)).all(|(a, b)| (*a as i32 - b as i32).abs() <= 3), "{c:?}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s), [(0, 19)]);
    // fixed layouts re-page: 3×3 → three pages; 2×2 → five; switching layout resets the page
    s.execute("multicam.gridLayout", json!({"layout": "3x3"})).unwrap();
    assert_eq!(s.execute("multicam.page", json!({"page": 7})).unwrap(), json!({"page": 2, "pages": 3}));
    let g = s.execute("multicam.grid", json!({})).unwrap();
    assert_eq!(g["cells"].as_array().unwrap().len(), 2, "cameras 19 and 20");
    assert_eq!(g["cells"][1]["active"], true, "camera 20 is on");
    s.execute("multicam.gridLayout", json!({"layout": "2x2"})).unwrap();
    let g = s.execute("multicam.grid", json!({})).unwrap();
    assert_eq!((g["page"].as_u64(), g["pages"].as_u64(), g["layout"].as_str()), (Some(0), Some(5), Some("2x2")));
    assert!(s.execute("multicam.gridLayout", json!({"layout": "5x5"})).is_err());
    // the view settings are editor state: they survive a state round trip
    let st: crate::EditorState = serde_json::from_value(serde_json::to_value(&s.state).unwrap()).unwrap();
    assert_eq!(st.multicam_view.layout, Some(2));
}

#[test]
fn grid_pages_decode_only_their_angles_at_reduced_resolution() {
    let (mut s, src) = many_angles(20);
    let scales = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let recording = {
        let scales = scales.clone();
        move |item: ItemId| {
            use filmcraft_render::SourceProvider;
            provider.source(item).map(|inner| std::sync::Arc::new(Recording { inner, scales: scales.clone() }) as filmcraft_media::SharedSource)
        }
    };
    let t = FrameRate::FPS_24.tick_of(10);
    let (img, angles) = filmcraft_render::multicam::render_grid_page(&s.project, src, t, 0.25, None, 1, &recording).unwrap().unwrap();
    assert_eq!(angles, [16, 17, 18, 19]);
    // 4×4 cells of 16×9 (¼ of 64×36): only the page's four angles decoded, each at ¼ scale
    assert_eq!((img.w, img.h), (64, 36));
    let asked = scales.lock().unwrap().clone();
    assert_eq!(asked.len(), 4, "{asked:?}");
    assert!(asked.iter().all(|s| (*s - 0.25).abs() < 1e-6), "{asked:?}");
    // cell 0 is camera 17, cell 3 camera 20; empty cells are black
    let px = img.over_black_rgba8();
    let at = |x: usize, y: usize| px[(y * 64 + x) * 4..][..3].to_vec();
    for (cell, cam) in [(0usize, 16usize), (3, 19)] {
        let c = at(cell % 4 * 16 + 8, cell / 4 * 9 + 4);
        assert!(c.iter().zip(cam_rgb(cam)).all(|(a, b)| (*a as i32 - b as i32).abs() <= 3), "cell {cell}: {c:?}");
    }
    assert_eq!(at(8, 20), [0, 0, 0]);
    // Auto-Adjust Multi-Camera Playback Quality: the view asks for a lower cell scale while playing
    let q =
        |s: &mut Session, playing: bool| s.execute("multicam.grid", json!({"cellPixels": 16.0, "playing": playing})).unwrap()["cellScale"].as_f64().unwrap();
    assert_eq!((q(&mut s, false), q(&mut s, true)), (0.25, 0.25));
    s.execute("multicam.autoAdjustQuality", json!({})).unwrap();
    assert_eq!((q(&mut s, false), q(&mut s, true)), (0.25, 0.0625));
}

#[test]
fn selection_top_down_and_view_toggles() {
    let (mut s, src) = many_angles(3);
    // a second multi-camera clip of the same source stacked on V2
    let edit = s.state.active_sequence.unwrap();
    s.execute("timeline.place", json!({"item": src.0, "track": "V2", "frame": 0})).unwrap();
    let ids = |s: &Session| {
        let q = s.project.sequence(edit).unwrap();
        (q.video_tracks[0].items[0].id.0, q.video_tracks[1].items[0].id.0)
    };
    let (low, high) = ids(&s);
    // untargeted tracks: lowest first by default, topmost with Selection Top Down
    let clip = |s: &mut Session| s.execute("multicam.inspect", json!({})).unwrap()["clip"].as_u64().unwrap();
    let expect_default = clip(&mut s);
    assert!(expect_default == low || expect_default == high);
    s.execute("multicam.selectionTopDown", json!({"enabled": true})).unwrap();
    let top = clip(&mut s);
    s.execute("multicam.selectionTopDown", json!({"enabled": false})).unwrap();
    let bottom = clip(&mut s);
    assert_eq!((top, bottom), (high, low));
    // preview monitor on by default; transmit has no device yet
    assert!(s.state.multicam_view.show_preview);
    assert_eq!(s.execute("multicam.showPreviewMonitor", json!({})).unwrap()["enabled"], false);
    let t = s.execute("multicam.transmitView", json!({})).unwrap();
    assert_eq!((t["enabled"].as_bool(), t["device"].is_null()), (Some(true), true));
    assert_eq!(s.execute("multicam.audioFollowsVideo", json!({})).unwrap()["enabled"], true);
}

/// Process CPU time in seconds (user + system), for the perf test.
fn process_cpu() -> f64 {
    let out = Command::new("ps").args(["-o", "cputime=", "-p", &std::process::id().to_string()]).output().ok();
    let txt = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    // [[dd-]hh:]mm:ss(.ss)
    txt.split(':').fold(0.0, |acc, p| acc * 60.0 + p.trim().parse::<f64>().unwrap_or(0.0))
}

/// Multi-Camera view cost: a 2×2 grid of four 1080p H.264 angles at ¼ scale per cell, played
/// sequentially, against the program (one angle at ½ scale). Run with
/// `cargo test --release -p filmcraft-engine --lib multicam_tests::perf -- --ignored --nocapture`.
#[test]
#[ignore]
fn perf_grid_of_four_1080p_angles() {
    let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("multicam perf") else { return };
    let dir = filmcraft_testkit::fixtures_dir("multicam");
    let srcs = ["testsrc2", "smptehdbars", "mandelbrot", "rgbtestsrc"];
    let files: Vec<PathBuf> = srcs
        .iter()
        .map(|src| {
            filmcraft_testkit::fixtures::generate(&dir.join(format!("perf-{src}.mp4")), |tmp| {
                let v = format!("{src}=s=1920x1080:r=24");
                ff(
                    &ffmpeg,
                    &["-f", "lavfi", "-i", &v, "-t", "4", "-c:v", "libx264", "-preset", "veryfast", "-g", "48", "-pix_fmt", "yuv420p", &tmp.to_string_lossy()],
                )
            })
            .expect("fixture")
        })
        .collect();
    // a fresh session (empty decoder caches) per measurement
    let setup = || {
        let mut s = Session::default();
        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let items = import(&mut s, &refs);
        let r = on_items(&mut s, "clip.createMulticam", &items, json!({"method": "in"})).unwrap();
        (s, ItemId(r["sequence"].as_u64().unwrap()))
    };
    let rate = FrameRate::FPS_24;
    let frames = 72;
    let (s, src) = setup();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let q = s.project.sequence(src).unwrap().clone();
    let t0 = std::time::Instant::now();
    for f in 0..frames {
        let opts = filmcraft_render::RenderOptions { scale: 0.5, ..Default::default() };
        filmcraft_render::multicam::render_angle(&s.project, &q, 0, rate.tick_of(f), opts, &provider).unwrap();
    }
    let one_ms = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    let (s, src) = setup();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let cpu0 = crate::multicam_tests::process_cpu();
    let t0 = std::time::Instant::now();
    for f in 0..frames {
        let (g, _) = filmcraft_render::multicam::render_grid(&s.project, src, rate.tick_of(f), 0.25, &provider).unwrap().unwrap();
        assert_eq!((g.w, g.h), (960, 540));
    }
    let grid_ms = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    let grid_cpu = (crate::multicam_tests::process_cpu() - cpu0) * 1000.0 / frames as f64;
    eprintln!(
        "Multi-Camera grid (4 × 1080p H.264, ¼-scale cells): {grid_ms:.1} ms/frame wall, {grid_cpu:.1} ms/frame CPU; one angle at ½ scale: {one_ms:.1} ms/frame wall (24 fps budget 41.7 ms)"
    );
}
