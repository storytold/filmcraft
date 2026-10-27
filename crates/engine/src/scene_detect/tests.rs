//! Scene Edit Detection on generated footage with known cut points: a movie made with our own
//! ProRes encoder from two demo scenes (no ffmpeg needed), and an H.264 file that ffmpeg (fixture
//! generator only) builds from three lavfi sources.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{session_with, tmp_dir};

/// Write a 24 fps ProRes movie of demo scenes played one after the other: `shots` = (scene,
/// frames). The cuts are at the running sums of the frame counts.
fn make_cut_movie(path: &Path, shots: &[(DemoScene, i64)], w: u32, h: u32) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("fixture");
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut sources: Vec<(ItemId, filmcraft_media::SharedSource)> = Vec::new();
    let mut at = Tick::ZERO;
    for (k, (scene, frames)) in shots.iter().enumerate() {
        let dur = rate.tick_of(*frames);
        // each shot starts at a different point of its scene
        let src = GeneratorSource::new(Generator::Demo(*scene), w, h, rate, rate.tick_of(200));
        let clip = MediaClip {
            media: MediaRef::Generator(Generator::Demo(*scene)),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let item = p.add_item(&format!("shot{k}"), Label::Iris, ItemKind::Media(clip), None);
        let mut v = p.make_track_item(item, TrackKind::Video, at, TimeRange::new(rate.tick_of(10 * k as i64), dur), rate).unwrap();
        for e in &mut v.effects {
            filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
        }
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        sources.push((item, Arc::new(src)));
        at += dur;
    }
    let settings =
        filmcraft_export::ExportSettings { format: filmcraft_export::Format::ProRes, path: path.to_string_lossy().into_owned(), ..Default::default() };
    let provider = move |id: ItemId| sources.iter().find(|(i, _)| *i == id).map(|(_, s)| s.clone());
    filmcraft_export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).unwrap();
}

fn select_v1(s: &mut Session) -> filmcraft_project::ClipId {
    let c = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips": [c.0]})).unwrap();
    c
}

#[test]
fn detects_cuts_in_our_own_render_and_applies_cuts_markers_and_subclips() {
    let dir = tmp_dir("scene-own");
    let path = dir.join("cuts.mov");
    // Ocean Sunset 20 frames, City Night 16, Forest 14: cuts at frames 20 and 36
    make_cut_movie(&path, &[(DemoScene::OceanSunset, 20), (DemoScene::CityNight, 16), (DemoScene::Forest, 14)], 160, 90);
    let (mut s, items, _) = session_with(&[&path]);
    let rate = s.sequence_rate();
    assert!(!s.is_enabled("clip.sceneEditDetection"), "nothing selected");
    select_v1(&mut s);
    assert!(s.is_enabled("clip.sceneEditDetection"));
    let n_undo = s.history.undo.len();
    let r = s.execute("clip.sceneEditDetection", json!({"wait": true, "applyCuts": true, "generateMarkers": true, "createSubclips": true})).unwrap();
    assert_eq!(r["clips"][0]["cuts"], json!([rate.tick_of(20).0, rate.tick_of(36).0]), "{r}");
    assert_eq!(s.history.undo.len(), n_undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Scene Edit Detection");
    // cuts: V1 and the linked A1 are in three pieces
    let q = s.active_sequence().unwrap();
    let starts: Vec<Tick> = q.video_tracks[0].items.iter().map(|i| i.start).collect();
    assert_eq!(starts, vec![Tick::ZERO, rate.tick_of(20), rate.tick_of(36)]);
    q.check().unwrap();
    // markers on the master clip, in media time
    let m = s.project.item(items[0]).unwrap().as_media().unwrap();
    assert_eq!(m.markers.iter().map(|x| x.start).collect::<Vec<_>>(), vec![rate.tick_of(20), rate.tick_of(36)]);
    // one subclip per shot in a "<clip> Scenes" bin
    let subs: Vec<TimeRange> = s
        .project
        .items
        .values()
        .filter_map(|i| match &i.kind {
            ItemKind::Subclip { parent, range, .. } if *parent == items[0] => Some(*range),
            _ => None,
        })
        .collect();
    assert_eq!(
        subs,
        vec![
            TimeRange::new(Tick::ZERO, rate.tick_of(20)),
            TimeRange::new(rate.tick_of(20), rate.tick_of(16)),
            TimeRange::new(rate.tick_of(36), rate.tick_of(14))
        ]
    );
    assert!(s.project.root.children.iter().any(|c| matches!(c, filmcraft_project::BinEntry::Bin(b) if b.name == "cuts.mov Scenes" && b.children.len() == 3)));
    // undo restores everything
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 1);
    assert!(s.project.item(items[0]).unwrap().as_media().unwrap().markers.is_empty());
    assert!(!s.project.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })));
    // sensitivity 0 still finds these hard cuts; nothing chosen to do is an error
    select_v1(&mut s);
    let r = s.execute("clip.sceneEditDetection", json!({"wait": true, "applyCuts": false, "generateMarkers": true, "sensitivity": 0})).unwrap();
    assert_eq!(r["clips"][0]["cuts"].as_array().unwrap().len(), 2);
    assert!(s.execute("clip.sceneEditDetection", json!({"applyCuts": false})).is_err());
}

#[test]
fn background_job_reports_progress_and_cancel_changes_nothing() {
    let dir = tmp_dir("scene-job");
    let path = dir.join("job.mov");
    make_cut_movie(&path, &[(DemoScene::Dunes, 18), (DemoScene::Plasma, 18)], 128, 72);
    let (mut s, _, _) = session_with(&[&path]);
    select_v1(&mut s);
    let r = s.execute("clip.sceneEditDetection", json!({})).unwrap();
    let job = r["job"].as_u64().unwrap();
    let t0 = std::time::Instant::now();
    loop {
        s.poll_persistence();
        let j = s.execute("jobs.list", json!({})).unwrap();
        let j = j.as_array().unwrap().iter().find(|x| x["id"] == job).unwrap().clone();
        if j["finished"] == true && s.scene_jobs.is_empty() {
            assert_eq!(j["total"], 36);
            assert_eq!(j["done"], 36);
            break;
        }
        assert!(t0.elapsed().as_secs() < 120, "job never finished");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 2, "cut applied when the job finished");
    // a cancelled job leaves the project alone
    s.execute("edit.undo", json!({})).unwrap();
    select_v1(&mut s);
    let rev = s.revision;
    let r = s.execute("clip.sceneEditDetection", json!({})).unwrap();
    s.execute("jobs.cancel", json!({"job": r["job"]})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.scene_jobs.is_empty() {
        s.poll_persistence();
        assert!(t0.elapsed().as_secs() < 120);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(s.revision, rev);
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 1);
}

fn ffmpeg_fixture() -> Option<PathBuf> {
    let ffmpeg = filmcraft_testkit::oracle::ffmpeg_or_skip("scene_detect")?;
    let out = filmcraft_testkit::fixtures_dir("engine/scene").join("three_shots.mp4");
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        std::process::Command::new(&ffmpeg)
            .args(["-y", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", "testsrc2=size=192x108:rate=24:duration=1"])
            .args(["-f", "lavfi", "-i", "smptebars=size=192x108:rate=24:duration=1.5"])
            .args(["-t", "1", "-f", "lavfi", "-i", "mandelbrot=size=192x108:rate=24"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3.5"])
            .args(["-filter_complex", "[0:v][1:v][2:v]concat=n=3:v=1:a=0,format=yuv420p[v]", "-map", "[v]", "-map", "3:a"])
            .args(["-c:v", "libx264", "-preset", "fast", "-g", "30", "-c:a", "aac", "-shortest"])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// ffmpeg-made H.264 + AAC: testsrc2 (24 frames), SMPTE bars (36), Mandelbrot zoom (24). The cuts
/// are at frames 24 and 60; the moving test pattern and the zoom don't trigger.
#[test]
fn detects_the_cuts_of_an_ffmpeg_concat_and_cuts_linked_audio() {
    let Some(src) = ffmpeg_fixture() else { return };
    let dir = tmp_dir("scene-ff");
    let media = dir.join("three_shots.mp4");
    std::fs::copy(&src, &media).unwrap();
    let (mut s, _, _) = session_with(&[&media]);
    let rate = s.sequence_rate();
    assert_eq!(rate, FrameRate::FPS_24);
    // link the V1 / A1 pair, as an edit from the Source monitor would
    let mut p = (*s.project).clone();
    let seq = s.state.active_sequence.unwrap();
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items[0].link = Some(7777);
    q.audio_tracks[0].items[0].link = Some(7777);
    s.project = Arc::new(p);
    select_v1(&mut s);
    for sensitivity in [20.0, 50.0, 80.0] {
        let r = s.execute("clip.sceneEditDetection", json!({"wait": true, "applyCuts": false, "generateMarkers": true, "sensitivity": sensitivity})).unwrap();
        assert_eq!(r["clips"][0]["cuts"], json!([rate.tick_of(24).0, rate.tick_of(60).0]), "sensitivity {sensitivity}: {r}");
        s.execute("edit.undo", json!({})).unwrap();
    }
    s.execute("clip.sceneEditDetection", json!({"wait": true})).unwrap();
    let q = s.active_sequence().unwrap();
    let v: Vec<Tick> = q.video_tracks[0].items.iter().map(|i| i.start).collect();
    let a: Vec<Tick> = q.audio_tracks[0].items.iter().map(|i| i.start).collect();
    assert_eq!(v, vec![Tick::ZERO, rate.tick_of(24), rate.tick_of(60)]);
    assert_eq!(a, v, "linked audio is cut too");
    // each piece keeps a link to its partner
    for (x, y) in q.video_tracks[0].items.iter().zip(&q.audio_tracks[0].items) {
        assert!(x.link.is_some() && x.link == y.link);
    }
}
