//! ComfyUI clips against the in-process fake server: placeholders, generation of every output
//! kind, clip fitting, ordering, uploads, failures and hostile parameters.

use std::sync::Arc;

use filmcraft_comfyui::fake::{FakeComfy, FakeOutput};
use filmcraft_media::{DemoScene, Generator};
use filmcraft_project::{ClipId, ItemId, MediaRef, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::*;
use crate::Session;
use crate::media_test_util::{make_movie, tmp_dir};

fn workflow() -> Value {
    json!({
        "3": {"class_type": "KSampler", "inputs": {"seed": 42, "steps": 20, "model": ["4", 0]}},
        "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "model.safetensors"}},
        "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}, "_meta": {"title": "Prompt"}},
        "10": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "FilmCraft"}}
    })
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn wav(seconds: f64) -> Vec<u8> {
    let n = (48_000.0 * seconds) as usize;
    crate::voiceover::write_wav_f32(&vec![0.1; n], 48_000)
}

/// A session with a 64×36 24 fps sequence and `fake` as its ComfyUI server.
fn session(fake: Arc<FakeComfy>) -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
    s.comfyui.transport = Some(fake);
    s
}

fn new_clip(s: &mut Session, extra: Value) -> (ItemId, ClipId) {
    let mut p = json!({"workflow": workflow(), "name": "Shot"});
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    let r = s.execute("comfyui.newClip", p).unwrap();
    (ItemId(r["item"].as_u64().unwrap()), ClipId(r["clip"].as_u64().unwrap()))
}

fn generate(s: &mut Session, dir: &std::path::Path, extra: Value) -> crate::Result<Value> {
    let mut p = json!({"wait": true, "dir": dir.to_string_lossy()});
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    s.execute("comfyui.generate", p)
}

/// (track kind, start, duration, link) of every clip of `item` in the active sequence.
fn clips_of(s: &Session, item: ItemId) -> Vec<(TrackKind, Tick, Tick, Option<u64>)> {
    let q = s.active_sequence().unwrap();
    q.all_tracks().flat_map(|t| t.items.iter().filter(|it| it.item == item).map(move |it| (t.kind, it.start, it.duration, it.link))).collect()
}

#[test]
fn new_clip_is_a_placeholder_with_a_recipe() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, clip) = new_clip(&mut s, json!({"duration": 2, "inputs": [{"node": 6, "input": "text", "value": "a dog"}]}));
    let pi = s.project.item(item).unwrap();
    assert_eq!(pi.name, "Shot");
    assert!(matches!(pi.as_media().unwrap().media, MediaRef::Generator(Generator::ColorMatte { color }) if color == PLACEHOLDER));
    let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
    assert_eq!((it.start, it.duration), (Tick::ZERO, Tick::from_seconds_f64(2.0)));
    let r = recipe_of(&s, item).unwrap();
    assert_eq!(r.inputs, vec![Binding::value("6", "text", json!("a dog"))]);
    // one undo step takes the whole clip away
    assert_eq!(s.history.undo.last().unwrap().0, "New ComfyUI Clip");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(item).is_none() && s.project.generated.is_empty());
}

#[test]
fn inspect_lists_inputs_with_overrides() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let w = s.execute("comfyui.inspect", json!({"workflow": workflow()})).unwrap();
    let nodes = w["nodes"].as_array().unwrap();
    assert_eq!(nodes.iter().map(|n| n["node"].as_str().unwrap()).collect::<Vec<_>>(), ["3", "4", "6", "9", "10"]);
    assert_eq!(w["seeds"], json!([["3", "seed"]]));
    let (item, _) = new_clip(&mut s, json!({"inputs": [{"node": "6", "input": "text", "value": "a dog"}]}));
    let c = s.execute("comfyui.inspect", json!({"item": item.0})).unwrap();
    let prompt = c["nodes"].as_array().unwrap().iter().find(|n| n["node"] == "6").unwrap();
    assert_eq!(prompt["title"], "Prompt");
    assert_eq!(prompt["inputs"][0]["override"]["value"], "a dog");
    assert_eq!(c["generated"], false);
    assert_eq!(c["server"], filmcraft_comfyui::DEFAULT_SERVER);
}

#[test]
fn set_inputs_edits_the_recipe_in_one_undo_step() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, clip) = new_clip(&mut s, json!({"inputs": [{"node": "6", "input": "text", "value": "a dog"}]}));
    s.state.selection = vec![clip];
    s.execute(
        "comfyui.setInputs",
        json!({"inputs": [{"node": "6", "input": "text", "value": "a fox"}, {"node": "3", "input": "steps", "value": 8}], "name": "Fox"}),
    )
    .unwrap();
    let r = recipe_of(&s, item).unwrap();
    assert_eq!(r.inputs.len(), 2);
    assert_eq!(r.inputs[0].value, Some(json!("a fox")));
    assert_eq!(s.project.item(item).unwrap().name, "Fox");
    // removing an override
    s.execute("comfyui.setInputs", json!({"item": item.0, "inputs": [{"node": "3", "input": "steps"}]})).unwrap();
    assert_eq!(recipe_of(&s, item).unwrap().inputs.len(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(recipe_of(&s, item).unwrap().inputs.len(), 2);
    // a link is not a value
    assert!(s.execute("comfyui.setInputs", json!({"item": item.0, "inputs": [{"node": "6", "input": "clip", "value": 1}]})).is_err());
}

#[test]
fn image_result_links_the_item() {
    let fake = Arc::new(FakeComfy::new(Box::new(|wf| {
        assert_eq!(wf["6"]["inputs"]["text"], "a dog");
        Ok(vec![FakeOutput::file("9", "images", "FilmCraft_00001_.png", png(64, 36))])
    })));
    let mut s = session(fake.clone());
    let (item, clip) = new_clip(&mut s, json!({"duration": 3, "inputs": [{"node": "6", "input": "text", "value": "a dog"}]}));
    let dir = tmp_dir("comfy-image");
    let n0 = s.history.undo.len();
    let r = generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(s.history.undo.len(), n0 + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Generate ComfyUI Clip");
    let m = s.project.item(item).unwrap().as_media().unwrap().clone();
    let MediaRef::File { path } = &m.media else { panic!("{:?}", m.media) };
    assert!(path.ends_with("Shot 001.png"), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), png(64, 36));
    // a still keeps the clip's length
    let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
    assert_eq!(it.duration, Tick::from_seconds_f64(3.0));
    assert_eq!(r["items"][0]["lastRun"]["media"], json!(path));
    assert!(s.render_program(0.5).is_some());
    // again: a new version, the item follows
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let MediaRef::File { path: p2 } = &s.project.item(item).unwrap().as_media().unwrap().media else { panic!() };
    assert!(p2.ends_with("Shot 002.png"), "{p2}");
    assert_eq!(fake.queued().len(), 2);
    // undo goes back to version 1
    s.execute("edit.undo", json!({})).unwrap();
    assert!(matches!(&s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::File { path: p } if p == path));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn video_with_sound_is_trimmed_and_gets_its_audio() {
    let dir = tmp_dir("comfy-video");
    let movie = dir.join("in.mov");
    make_movie(&movie, DemoScene::Plasma, 64, 36, 24);
    let bytes = std::fs::read(&movie).unwrap();
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("20", "gifs", "clip_00001.mov", bytes)]));
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 5}));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let clips = clips_of(&s, item);
    let one_second = Tick::from_seconds_f64(1.0);
    assert_eq!(clips.len(), 2, "{clips:?}");
    assert_eq!((clips[0].0, clips[0].2), (TrackKind::Video, one_second));
    assert_eq!((clips[1].0, clips[1].2), (TrackKind::Audio, one_second));
    assert!(clips[0].3.is_some() && clips[0].3 == clips[1].3);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn sound_only_moves_to_an_audio_track() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("12", "audio", "voice.wav", wav(1.0))]));
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 4}));
    let dir = tmp_dir("comfy-audio");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let clips = clips_of(&s, item);
    assert_eq!(clips.len(), 1, "{clips:?}");
    assert_eq!(clips[0].0, TrackKind::Audio);
    assert_eq!(clips[0].2, Tick::from_seconds_f64(1.0));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_separate_sound_lies_under_the_picture() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![
        FakeOutput::file("9", "images", "frame.png", png(64, 36)),
        FakeOutput::file("12", "audio", "music.wav", wav(2.0)),
    ]));
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 3}));
    let dir = tmp_dir("comfy-sound");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let pic = clips_of(&s, item);
    assert_eq!(pic.len(), 1);
    let sound = s.project.items.values().find(|i| i.name.starts_with("Shot") && i.id != item).map(|i| i.id).unwrap();
    let snd = clips_of(&s, sound);
    assert_eq!(snd.len(), 1, "{snd:?}");
    assert_eq!((snd[0].0, snd[0].1, snd[0].2), (TrackKind::Audio, pic[0].1, Tick::from_seconds_f64(2.0)));
    assert_eq!(snd[0].3, pic[0].3);
    assert!(pic[0].3.is_some());
    // one undo step for the whole result
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(sound).is_none());
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::Generator(_)));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn text_only_becomes_a_title() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::text("30", "A wide shot of a harbour at dawn")]));
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 2}));
    let dir = tmp_dir("comfy-text");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::Generator(Generator::TransparentVideo)));
    let c = s.execute("comfyui.inspect", json!({"item": item.0})).unwrap();
    assert_eq!(c["lastRun"]["texts"], json!(["A wide shot of a harbour at dawn"]));
    let q = s.active_sequence().unwrap();
    let titles: Vec<_> = q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| crate::graphics::is_graphic(&s, q, it.id)).collect();
    assert_eq!(titles.len(), 1);
    assert_eq!(titles[0].duration, Tick::from_seconds_f64(2.0));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn clips_generate_in_timeline_order() {
    let fake = Arc::new(FakeComfy::new(Box::new(|wf| {
        let t = wf["6"]["inputs"]["text"].as_str().unwrap_or_default().to_string();
        Ok(vec![FakeOutput::file("9", "images", &format!("{t}.png"), png(8, 8))])
    })));
    let mut s = session(fake.clone());
    let late = new_clip(&mut s, json!({"name": "Late", "time": Tick::from_seconds_f64(10.0).0, "inputs": [{"node": "6", "input": "text", "value": "second"}]}));
    let early = new_clip(&mut s, json!({"name": "Early", "time": 0, "inputs": [{"node": "6", "input": "text", "value": "first"}]}));
    let dir = tmp_dir("comfy-order");
    s.state.selection = vec![late.1, early.1];
    generate(&mut s, &dir, json!({})).unwrap();
    let order: Vec<Value> = fake.queued().iter().map(|w| w["6"]["inputs"]["text"].clone()).collect();
    assert_eq!(order, [json!("first"), json!("second")]);
    assert_eq!(targets(&s, &json!({"items": [late.0.0, early.0.0]})), vec![early.0, late.0]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn file_inputs_are_uploaded() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]));
    let mut s = session(fake.clone());
    let dir = tmp_dir("comfy-upload");
    let input = dir.join("first frame.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    let (item, _) = new_clip(&mut s, json!({"inputs": [{"node": "10", "input": "image", "file": input.to_string_lossy()}]}));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let up = fake.uploads();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].1, png(4, 4));
    assert_eq!(fake.queued()[0]["10"]["inputs"]["image"], json!(up[0].0));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_failed_run_changes_nothing() {
    let fake = Arc::new(FakeComfy::new(Box::new(|_| Err("CUDA out of memory".into()))));
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({}));
    let before = s.project.clone();
    let dir = tmp_dir("comfy-fail");
    let e = generate(&mut s, &dir, json!({"item": item.0})).unwrap_err().to_string();
    assert!(e.contains("CUDA out of memory"), "{e}");
    assert_eq!(*s.project, *before);
    assert!(!s.comfyui.generating(item));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn new_seeds_are_kept_with_the_clip() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]));
    let mut s = session(fake.clone());
    let (item, _) = new_clip(&mut s, json!({}));
    let dir = tmp_dir("comfy-seed");
    generate(&mut s, &dir, json!({"item": item.0, "randomizeSeeds": true})).unwrap();
    let seed = fake.queued()[0]["3"]["inputs"]["seed"].as_u64().unwrap();
    assert_ne!(seed, 42);
    assert!(seed < 1 << 50);
    let r = recipe_of(&s, item).unwrap();
    assert_eq!(r.inputs, vec![Binding::value("3", "seed", json!(seed))]);
    // the same recipe again: the same seed
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(fake.queued()[1]["3"]["inputs"]["seed"].as_u64(), Some(seed));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn background_job_applies_when_polled() {
    let mut fake = FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]);
    fake.polls = 3;
    let fake = Arc::new(fake);
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({}));
    let dir = tmp_dir("comfy-bg");
    let r = s.execute("comfyui.generate", json!({"item": item.0, "dir": dir.to_string_lossy()})).unwrap();
    let job = r["job"].as_u64().unwrap();
    assert!(s.comfyui.generating(item));
    assert!(s.execute("comfyui.generate", json!({"item": item.0})).is_err(), "already generating");
    let t0 = std::time::Instant::now();
    while s.comfyui.generating(item) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "job never finished");
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.poll_persistence();
    }
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::File { .. }));
    let j = s.jobs.iter().find(|j| j.id == job).unwrap().to_json();
    assert_eq!(j["finished"], true, "{j}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn recipes_survive_save_and_open() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, _) = new_clip(&mut s, json!({"inputs": [{"node": "6", "input": "text", "value": "a dog"}]}));
    let dir = tmp_dir("comfy-save");
    let path = dir.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(recipe_of(&t, item), recipe_of(&s, item));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn settings_are_checked_and_saved() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let r = s.execute("comfyui.settings", json!({"server": "http://gpu.local:8188/", "timeoutMinutes": 0, "check": true})).unwrap();
    assert_eq!(r["settings"]["server"], "http://gpu.local:8188");
    assert_eq!(r["settings"]["timeoutMinutes"], 1);
    assert_eq!(r["reachable"], true);
    assert!(s.execute("comfyui.settings", json!({"server": "gpu.local:8188"})).is_err());
    assert!(s.execute("comfyui.settings", json!({"server": "http://"})).is_err());
}

#[test]
fn hostile_parameters_are_errors() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let bad = [
        ("comfyui.newClip", json!({})),
        ("comfyui.newClip", json!({"workflow": {"nodes": [], "links": []}})),
        ("comfyui.newClip", json!({"workflow": [1, 2]})),
        ("comfyui.newClip", json!({"workflow": workflow(), "inputs": "text"})),
        ("comfyui.newClip", json!({"workflow": workflow(), "inputs": [{"input": "text"}]})),
        ("comfyui.newClip", json!({"workflow": workflow(), "inputs": [{"node": "99", "input": "text", "value": 1}]})),
        ("comfyui.newClip", json!({"path": "/no/such/workflow.json"})),
        ("comfyui.inspect", json!({})),
        ("comfyui.inspect", json!({"item": 123456})),
        ("comfyui.setInputs", json!({"item": 123456})),
        ("comfyui.generate", json!({"items": [1, 2, 3]})),
        ("comfyui.settings", json!({"server": 7})),
    ];
    for (id, p) in bad {
        let before = s.project.clone();
        let r = s.execute(id, p.clone());
        assert!(r.is_err() || id == "comfyui.settings", "{id} {p}: {r:?}");
        assert_eq!(*s.project, *before, "{id} {p}");
    }
    // a huge or NaN length is clamped / refused, never a panic
    let r = s.execute("comfyui.newClip", json!({"workflow": workflow(), "duration": f64::MAX}));
    assert!(r.is_ok(), "{r:?}");
    // a damaged recipe in a project file is not a ComfyUI clip
    let (item, _) = new_clip(&mut s, json!({}));
    let mut p = (*s.project).clone();
    p.generated.insert(item, std::sync::Arc::new(filmcraft_project::Generation { provider: PROVIDER.into(), recipe: json!("junk"), last_run: Value::Null }));
    s.project = std::sync::Arc::new(p);
    assert!(recipe_of(&s, item).is_none());
    assert!(s.execute("comfyui.generate", json!({"item": item.0})).is_err());
}

#[cfg(not(feature = "comfyui"))]
#[test]
fn without_a_transport_generate_is_disabled() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
    let r = s.execute("comfyui.newClip", json!({"workflow": workflow()})).unwrap();
    let item = r["item"].as_u64().unwrap();
    s.state.project_selection = vec![ItemId(item)];
    let e = s.execute("comfyui.generate", json!({"item": item})).unwrap_err().to_string();
    assert!(e.contains("not available in this build"), "{e}");
}
