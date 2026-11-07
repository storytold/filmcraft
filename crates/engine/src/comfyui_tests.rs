//! ComfyUI clips against the in-process fake server: placeholders, generation of every output
//! kind, clip fitting, ordering, uploads, failures and hostile parameters.

use std::sync::Arc;

use filmcraft_comfyui::Transport;
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

/// A ComfyUI clip named Shot from [`workflow`], with `extra` parameters; its item and clip.
fn new_clip(s: &mut Session, extra: Value) -> (ItemId, ClipId) {
    let mut p = json!({"workflow": workflow(), "name": "Shot"});
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    let r = s.execute("comfyui.newClip", p).unwrap();
    (ItemId(r["item"].as_u64().unwrap()), ClipId(r["clip"].as_u64().unwrap()))
}

/// Generate and wait, writing files to `dir`.
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

/// A new clip is a placeholder matte at the playhead with its recipe, undone in one step.
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

/// An image becomes the clip's media; generating again makes a new version, undo goes back.
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
    // streamed to `<file>.part`, renamed when complete: no partial file is left
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Shot 001.png"]);
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

/// A video with sound shortens the clip to its length and adds a linked audio clip.
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

/// A sound-only result moves the clip to an audio track, as long as the sound.
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

/// A picture plus a separate sound output: the sound is imported and laid under it, linked.
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

/// Without `wait` the run is a background job, applied by `poll_persistence` when it finishes.
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
    // the command itself, not `execute`: that would first apply a job that already finished
    assert!(super::generate(&mut s, &json!({"item": item.0})).is_err(), "already generating");
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

/// Bad parameters and damaged recipes are errors that leave the project unchanged.
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
        ("comfyui.settings", json!({"server": "http://a b"})),
        ("comfyui.settings", json!({"server": "http://gpu:8188?x=1"})),
        ("comfyui.settings", json!({"server": "file:///etc/passwd"})),
        // a clip has no server of its own
        ("comfyui.newClip", json!({"workflow": workflow(), "server": "http://elsewhere:8188"})),
        ("comfyui.expose", json!({})),
        ("comfyui.expose", json!({"workflow": workflow(), "inputs": "text"})),
        ("comfyui.expose", json!({"workflow": workflow(), "inputs": [{"node": "99", "input": "text"}]})),
        ("comfyui.expose", json!({"workflow": workflow(), "inputs": [{"node": "6", "input": "clip"}]})),
        ("comfyui.expose", json!({"workflow": workflow(), "inputs": [{"node": "6"}]})),
        ("comfyui.expose", json!({"workflow": workflow(), "set": vec![json!({"node": "6", "input": "text"}); MAX_EXPOSED_INPUTS + 1]})),
    ];
    let prefs = s.prefs.comfyui.clone();
    for (id, p) in bad {
        let before = s.project.clone();
        let r = s.execute(id, p.clone());
        assert!(r.is_err(), "{id} {p}: {r:?}");
        assert_eq!(*s.project, *before, "{id} {p}");
    }
    assert_eq!(s.prefs.comfyui, prefs);
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
    let (item, _) = new_clip(&mut s, json!({}));
    assert!(s.execute("comfyui.setInputs", json!({"item": item.0, "server": "http://elsewhere:8188"})).is_err());
}

/// The project file of `s`, saved to `dir`, with `edit` applied to the recipe of `item` (what a
/// hand-made or hostile project could carry).
fn tampered_project(s: &mut Session, dir: &std::path::Path, item: ItemId, edit: impl FnOnce(&mut Value)) -> String {
    let path = dir.join("shared.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut doc["project"]["generated"][item.0.to_string()]["recipe"]);
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    path
}

/// A server named by a project file or a parameter is never used: only the preferences' one.
#[test]
fn a_project_never_chooses_the_server() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]));
    let mut a = session(fake.clone());
    let (item, _) = new_clip(&mut a, json!({}));
    let dir = tmp_dir("comfy-server");
    let path = tampered_project(&mut a, &dir, item, |r| r["server"] = json!("http://attacker.invalid:8188"));
    let mut b = Session::default();
    b.comfyui.transport = Some(fake);
    b.execute("file.open", json!({"path": path})).unwrap();
    // the recipe loads, without the server
    assert!(recipe_of(&b, item).is_some());
    let mine = b.prefs.comfyui.server.clone();
    assert_eq!(b.execute("comfyui.inspect", json!({"item": item.0})).unwrap()["server"], json!(mine));
    let r = generate(&mut b, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(r["server"], json!(mine));
    // and saving again drops it for good
    let again = dir.join("again.fcproj").to_string_lossy().into_owned();
    b.execute("file.saveAs", json!({"path": again})).unwrap();
    assert!(!std::fs::read_to_string(&again).unwrap().contains("attacker"));
    std::fs::remove_dir_all(dir).ok();
}

/// File inputs that come from an opened project are not uploaded until the user allows them.
#[test]
fn files_named_by_a_project_are_not_uploaded_until_confirmed() {
    let dir = tmp_dir("comfy-exfil");
    let secret = dir.join("id_rsa");
    std::fs::write(&secret, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
    let secret = secret.to_string_lossy().into_owned();
    let mut a = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, _) = new_clip(&mut a, json!({}));
    let path = tampered_project(&mut a, &dir, item, |r| r["inputs"] = json!([{"node": "10", "input": "image", "file": secret}]));
    // someone else opens the shared project and presses Generate
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]));
    let mut b = Session::default();
    b.comfyui.transport = Some(fake.clone());
    b.execute("file.open", json!({"path": path})).unwrap();
    let before = b.project.clone();
    let e = generate(&mut b, &dir, json!({"item": item.0})).unwrap_err().to_string();
    assert!(e.contains(&secret) && e.contains("not chosen in this session"), "{e}");
    assert!(fake.uploads().is_empty() && fake.queued().is_empty(), "nothing was sent");
    assert_eq!(*b.project, *before);
    assert!(b.jobs.is_empty() && !b.comfyui.generating(item));
    let c = b.execute("comfyui.inspect", json!({"item": item.0})).unwrap();
    assert_eq!(c["unconfirmedFiles"], json!([{"node": "10", "input": "image", "file": secret}]));
    // the user chooses the file (again): now it is theirs to send
    b.execute("comfyui.setInputs", json!({"item": item.0, "inputs": [{"node": "10", "input": "image", "file": secret}]})).unwrap();
    assert!(b.comfyui.confirmed(&secret));
    assert_eq!(b.execute("comfyui.inspect", json!({"item": item.0})).unwrap()["unconfirmedFiles"], json!([]));
    generate(&mut b, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(fake.uploads().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

/// Media items of the project upload; a non-media file named like one does not.
#[test]
fn project_media_upload_but_not_a_non_media_file_posing_as_one() {
    let dir = tmp_dir("comfy-media-input");
    let frame = dir.join("frame.png");
    std::fs::write(&frame, png(4, 4)).unwrap();
    let frame = frame.to_string_lossy().into_owned();
    let secret = dir.join("notes.txt");
    std::fs::write(&secret, b"private").unwrap();
    let secret = secret.to_string_lossy().into_owned();
    let fake = Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]));
    let mut s = session(fake.clone());
    let imported = s.execute("file.import", json!({"paths": [frame]})).unwrap();
    let media = ItemId(imported["items"][0].as_u64().unwrap());
    let (item, _) = new_clip(&mut s, json!({}));
    // a recipe from a project file (never confirmed) naming the project's own media: allowed
    let bind = |s: &mut Session, file: &str| {
        let mut r = recipe_of(s, item).unwrap();
        r.inputs = vec![Binding::file("10", "image", file)];
        let mut p = (*s.project).clone();
        p.generated.insert(
            item,
            Arc::new(filmcraft_project::Generation { provider: PROVIDER.into(), recipe: serde_json::to_value(&r).unwrap(), last_run: Value::Null }),
        );
        s.project = Arc::new(p);
    };
    bind(&mut s, &frame);
    assert!(!s.comfyui.confirmed(&frame));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(fake.uploads().len(), 1);
    assert_eq!(fake.uploads()[0].1, png(4, 4));
    // a media item whose path is not media (a hostile project's "footage") is not enough
    let mut p = (*s.project).clone();
    if let Some(m) = p.item_mut(media).and_then(|i| i.as_media_mut()) {
        m.media = MediaRef::File { path: secret.clone() };
    }
    s.project = Arc::new(p);
    bind(&mut s, &secret);
    let e = generate(&mut s, &dir, json!({"item": item.0})).unwrap_err().to_string();
    assert!(e.contains(&secret), "{e}");
    assert_eq!(fake.uploads().len(), 1, "nothing more was sent");
    std::fs::remove_dir_all(dir).ok();
}

/// A server whose `/view` fails for files named `broken…`.
struct Flaky(Arc<FakeComfy>);

impl Transport for Flaky {
    /// The fake's answers, except a 500 for the `broken` output file.
    fn get(&self, path: &str) -> filmcraft_comfyui::Result<filmcraft_comfyui::Response> {
        if path.starts_with("/view") && path.contains("broken") {
            return Ok(filmcraft_comfyui::Response { status: 500, body: b"oops".to_vec() });
        }
        self.0.get(path)
    }
    fn post(&self, path: &str, content_type: &str, body: &[u8]) -> filmcraft_comfyui::Result<filmcraft_comfyui::Response> {
        self.0.post(path, content_type, body)
    }
    fn pause(&self, _: std::time::Duration) {}
}

/// When one output fails to download, the files already written are removed.
#[test]
fn a_failed_download_leaves_no_files_behind() {
    let fake = Arc::new(FakeComfy::with_outputs(vec![
        FakeOutput::file("9", "images", "good.png", png(8, 8)),
        FakeOutput::file("10", "images", "broken.png", png(8, 8)),
    ]));
    let mut s = session(fake.clone());
    s.comfyui.transport = Some(Arc::new(Flaky(fake)));
    let (item, _) = new_clip(&mut s, json!({}));
    let before = s.project.clone();
    let dir = tmp_dir("comfy-partial");
    let out = dir.join("out");
    let e = generate(&mut s, &out, json!({"item": item.0})).unwrap_err().to_string();
    assert!(e.contains("broken.png") && e.contains("500"), "{e}");
    assert_eq!(*s.project, *before);
    // good.png was downloaded, then removed with the failed run; no `.part` either
    let left: Vec<_> = std::fs::read_dir(&out).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// Regression: the job id was `max + 1`, which overflowed (a panic in debug builds).
#[test]
fn job_ids_never_overflow() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))])));
    let (item, _) = new_clip(&mut s, json!({}));
    s.jobs.push(crate::Job { id: u64::MAX, label: "old".into(), progress: Default::default(), result: Default::default() });
    let dir = tmp_dir("comfy-jobid");
    let r = generate(&mut s, &dir, json!({"item": item.0}));
    assert!(r.is_err(), "{r:?}");
    assert!(!s.comfyui.generating(item));
    std::fs::remove_dir_all(dir).ok();
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

#[test]
fn exposed_inputs_are_kept_per_workflow() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let r = s.execute("comfyui.expose", json!({"workflow": workflow(), "inputs": [{"node": "6", "input": "text"}, {"node": 3, "input": "seed"}]})).unwrap();
    assert_eq!(r["exposed"], json!([{"node": "6", "input": "text"}, {"node": "3", "input": "seed"}]));
    // the workflow exported again with another prompt is the same workflow
    let mut again = workflow();
    again["6"]["inputs"]["text"] = json!("a harbour at dawn");
    let w = s.execute("comfyui.inspect", json!({"workflow": again})).unwrap();
    assert_eq!((&w["key"], &w["exposed"]), (&r["key"], &r["exposed"]));
    let prompt = w["nodes"].as_array().unwrap().iter().find(|n| n["node"] == "6").unwrap();
    assert_eq!(prompt["inputs"][0]["exposed"], true);
    // every clip made from it shows them; changing them through a clip changes the workflow's
    let (item, _) = new_clip(&mut s, json!({}));
    assert_eq!(s.execute("comfyui.inspect", json!({"item": item.0})).unwrap()["exposed"], r["exposed"]);
    let n0 = s.history.undo.len();
    s.execute("comfyui.expose", json!({"item": item.0, "inputs": [{"node": "3", "input": "seed"}], "exposed": false})).unwrap();
    let w = s.execute("comfyui.inspect", json!({"workflow": workflow()})).unwrap();
    assert_eq!(w["exposed"], json!([{"node": "6", "input": "text"}]));
    assert_eq!(s.history.undo.len(), n0, "a preference, not an edit");
    // another workflow has its own
    let mut other = workflow();
    other["11"] = json!({"class_type": "SaveImage", "inputs": {"filename_prefix": "x"}});
    assert_eq!(s.execute("comfyui.inspect", json!({"workflow": other})).unwrap()["exposed"], json!([]));
    // `set` replaces the list; an empty one forgets the workflow
    s.execute("comfyui.expose", json!({"workflow": workflow(), "set": [{"node": "10", "input": "image"}]})).unwrap();
    assert_eq!(s.execute("comfyui.inspect", json!({"workflow": workflow()})).unwrap()["exposed"], json!([{"node": "10", "input": "image"}]));
    s.execute("comfyui.expose", json!({"workflow": workflow(), "set": []})).unwrap();
    assert!(s.prefs.comfyui.exposed.is_empty());
}

/// Headless sessions (CLI, MCP) have no frame loop: a finished background generation is applied
/// before the next command.
#[test]
fn headless_commands_apply_finished_jobs() {
    let mut fake = FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]);
    fake.polls = 3;
    let mut s = session(Arc::new(fake));
    let (item, _) = new_clip(&mut s, json!({}));
    let dir = tmp_dir("comfy-headless");
    s.execute("comfyui.generate", json!({"item": item.0, "dir": dir.to_string_lossy()})).unwrap();
    let t0 = std::time::Instant::now();
    while s.comfyui.generating(item) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "job never applied");
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.execute("comfyui.inspect", json!({"item": item.0})).unwrap();
    }
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::File { .. }));
    std::fs::remove_dir_all(dir).ok();
}

/// A generation started before another project was opened changes nothing in that project, even
/// where an item there has the same id: the job is stopped and its results dropped.
#[test]
fn a_job_for_a_replaced_project_is_dropped() {
    let mut fake = FakeComfy::with_outputs(vec![FakeOutput::file("9", "images", "out.png", png(8, 8))]);
    fake.polls = 3;
    let mut s = session(Arc::new(fake));
    let (item, _) = new_clip(&mut s, json!({}));
    let dir = tmp_dir("comfy-replaced");
    let job = s.execute("comfyui.generate", json!({"item": item.0, "dir": dir.to_string_lossy()})).unwrap()["job"].as_u64().unwrap();
    s.execute("file.newProject", json!({"name": "Other"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Seq", "width": 64, "height": 36, "fps": 24})).unwrap();
    let (other, _) = new_clip(&mut s, json!({}));
    let finished = |s: &Session| s.jobs.iter().find(|j| j.id == job).is_none_or(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed));
    let t0 = std::time::Instant::now();
    while !finished(&s) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "job never ended");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.poll_persistence();
    assert!(!s.comfyui.generating(item));
    assert!(matches!(s.project.item(other).unwrap().as_media().unwrap().media, MediaRef::Generator(_)), "the other project's clip was relinked");
    std::fs::remove_dir_all(dir).ok();
}

/// Clips with the same name generating at the same time never share a file: a download in flight
/// (its `.part`) holds its name, and a name taken first moves on to the next number.
#[test]
fn same_named_clips_never_share_a_file() {
    use filmcraft_comfyui::Sink;
    let dir = tmp_dir("comfy-names");
    let d = dir.to_string_lossy().into_owned();
    let services = crate::FsServices;
    std::fs::write(dir.join("Shot 001.png.part"), b"in flight").unwrap();
    assert!(version_path(&services, &d, "Shot", "png", &Default::default()).unwrap().ends_with("Shot 002.png"));
    let cancel = AtomicBool::new(false);
    let held = Mutex::default();
    let out = filmcraft_comfyui::Output {
        node: "9".into(),
        key: "images".into(),
        kind: OutputKind::Image,
        file: Some(filmcraft_comfyui::FileRef { filename: "x.png".into(), subfolder: String::new(), folder: "output".into() }),
        text: None,
    };
    let files = |cancel| OutputFiles {
        services: &services,
        dir: &d,
        name: "Shot",
        in_memory: false,
        held: &held,
        cancel,
        current: String::new(),
        buffer: Vec::new(),
        written: Vec::new(),
    };
    let (mut a, mut b) = (files(&cancel), files(&cancel));
    drop(a.create(&out).unwrap());
    drop(b.create(&out).unwrap());
    assert!(a.current.ends_with("Shot 002.png") && b.current.ends_with("Shot 003.png"), "{} {}", a.current, b.current);
    std::fs::remove_dir_all(dir).ok();
}

/// A ComfyUI recipe leaves with its item, and an id a recipe still names is never handed out
/// again (a new item would become that ComfyUI clip).
#[test]
fn recipes_leave_with_their_item_and_keep_their_id() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, _) = new_clip(&mut s, json!({}));
    let mut p = (*s.project).clone();
    let g = p.generated.get(&item).cloned().unwrap();
    p.generated.insert(ItemId(1_000_000), g);
    assert!(p.max_id() >= 1_000_000);
    s.state.project_selection = vec![item];
    s.execute("project.delete", json!({"items": [item.0]})).unwrap();
    assert!(s.project.item(item).is_none());
    assert!(!s.project.generated.contains_key(&item));
}

/// A fake whose n-th run (0-based) produces `runs[n]` (the last one again after that).
fn fake_runs(runs: Vec<Vec<FakeOutput>>) -> Arc<FakeComfy> {
    let n = std::sync::atomic::AtomicUsize::new(0);
    Arc::new(FakeComfy::new(Box::new(move |_| {
        let k = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(runs.get(k).or(runs.last()).cloned().unwrap_or_default())
    })))
}

/// The title clips in the active sequence.
fn titles(s: &Session) -> usize {
    let q = s.active_sequence().unwrap();
    q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| crate::graphics::is_graphic(s, q, it.id)).count()
}

/// A name another job finished with is never taken over: each job claims its final name before
/// downloading, so a later job gets the next number and both files keep their own bytes.
#[test]
fn a_finished_name_is_never_taken_over() {
    use filmcraft_comfyui::Sink;
    use std::io::Write as _;
    let dir = tmp_dir("comfy-claim");
    let d = dir.to_string_lossy().into_owned();
    let services = crate::FsServices;
    let cancel = AtomicBool::new(false);
    let held = Mutex::default();
    let out = filmcraft_comfyui::Output {
        node: "9".into(),
        key: "images".into(),
        kind: OutputKind::Image,
        file: Some(filmcraft_comfyui::FileRef { filename: "x.png".into(), subfolder: String::new(), folder: "output".into() }),
        text: None,
    };
    let files = |cancel| OutputFiles {
        services: &services,
        dir: &d,
        name: "Shot",
        in_memory: false,
        held: &held,
        cancel,
        current: String::new(),
        buffer: Vec::new(),
        written: Vec::new(),
    };
    let (mut a, mut b) = (files(&cancel), files(&cancel));
    // A has chosen and claimed 001 and is still downloading when B starts, finishes and publishes
    a.create(&out).unwrap().write_all(b"from A").unwrap();
    b.create(&out).unwrap().write_all(b"from B").unwrap();
    b.close(&out, Some(6)).unwrap();
    a.close(&out, Some(6)).unwrap();
    assert_eq!(std::fs::read(dir.join("Shot 001.png")).unwrap(), b"from A");
    assert_eq!(std::fs::read(dir.join("Shot 002.png")).unwrap(), b"from B");
    // a download that did not complete releases its name
    let mut c = files(&cancel);
    drop(c.create(&out).unwrap());
    c.close(&out, None).unwrap();
    assert!(!dir.join("Shot 003.png").exists() && !dir.join("Shot 003.png.part").exists());
    std::fs::remove_dir_all(dir).ok();
}

/// A result never overwrites recipe edits made while it was generating: it is not applied, and
/// the clip keeps the new inputs.
#[test]
fn edits_during_a_run_are_not_overwritten() {
    let mut s = session(Arc::new(FakeComfy::with_outputs(vec![])));
    let (item, _) = new_clip(&mut s, json!({"inputs": [{"node": "6", "input": "text", "value": "a dog"}]}));
    let original = recipe_of(&s, item).unwrap();
    let dir = tmp_dir("comfy-stale");
    let file = dir.join("out.png");
    std::fs::write(&file, png(8, 8)).unwrap();
    let output = filmcraft_comfyui::Output {
        node: "9".into(),
        key: "images".into(),
        kind: OutputKind::Image,
        file: Some(filmcraft_comfyui::FileRef { filename: "out.png".into(), subfolder: String::new(), folder: "output".into() }),
        text: None,
    };
    let done = |original: Recipe| Done {
        item,
        recipe: original.clone(),
        original,
        result: Ok(("p".into(), vec![output.clone()], vec![(output.clone(), file.to_string_lossy().into_owned())])),
    };
    // the user changes the prompt while the run is going
    s.execute("comfyui.setInputs", json!({"item": item.0, "inputs": [{"node": "6", "input": "text", "value": "a fox"}]})).unwrap();
    let e = apply(&mut s, done(original)).unwrap_err().to_string();
    assert!(e.contains("changed while it was generating"), "{e}");
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::Generator(_)));
    assert_eq!(recipe_of(&s, item).unwrap().inputs[0].value, Some(json!("a fox")));
    // unchanged since the start: applied
    let current = recipe_of(&s, item).unwrap();
    apply(&mut s, done(current)).unwrap();
    assert!(matches!(s.project.item(item).unwrap().as_media().unwrap().media, MediaRef::File { .. }));
    std::fs::remove_dir_all(dir).ok();
}

/// Titles made from text output are replaced by the next run's, and removed when a run makes a
/// picture instead.
#[test]
fn titles_of_the_previous_run_are_replaced() {
    let fake = fake_runs(vec![
        vec![FakeOutput::text("30", "First take")],
        vec![FakeOutput::text("30", "Second take")],
        vec![FakeOutput::file("9", "images", "frame.png", png(64, 36))],
    ]);
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 2}));
    let dir = tmp_dir("comfy-titles");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(titles(&s), 1);
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(titles(&s), 1, "the first run's title was kept");
    // the second run is one undo step: undo brings the first title back
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(titles(&s), 1);
    s.execute("edit.redo", json!({})).unwrap();
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(titles(&s), 0, "a title stayed over the picture");
    std::fs::remove_dir_all(dir).ok();
}

/// Sound first, then a picture: the clip comes back to a video track (and leaves the audio track).
#[test]
fn a_picture_after_a_sound_goes_back_to_a_video_track() {
    let fake = fake_runs(vec![vec![FakeOutput::file("12", "audio", "voice.wav", wav(1.0))], vec![FakeOutput::file("9", "images", "frame.png", png(64, 36))]]);
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 3}));
    let dir = tmp_dir("comfy-back-to-video");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(clips_of(&s, item).iter().map(|c| c.0).collect::<Vec<_>>(), [TrackKind::Audio]);
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let clips = clips_of(&s, item);
    assert_eq!(clips.iter().map(|c| c.0).collect::<Vec<_>>(), [TrackKind::Video], "{clips:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// A video with sound regenerated as sound only keeps one audio clip (its linked partner), not two.
#[test]
fn sound_only_after_a_video_with_sound_keeps_one_audio_clip() {
    let dir = tmp_dir("comfy-video-then-sound");
    let movie = dir.join("in.mov");
    make_movie(&movie, DemoScene::Plasma, 64, 36, 24);
    let fake = fake_runs(vec![
        vec![FakeOutput::file("20", "gifs", "clip.mov", std::fs::read(&movie).unwrap())],
        vec![FakeOutput::file("12", "audio", "voice.wav", wav(0.5))],
    ]);
    let mut s = session(fake);
    let (item, _) = new_clip(&mut s, json!({"duration": 5}));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(clips_of(&s, item).len(), 2);
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let clips = clips_of(&s, item);
    assert_eq!(clips.len(), 1, "{clips:?}");
    assert_eq!((clips[0].0, clips[0].2, clips[0].3), (TrackKind::Audio, Tick::from_seconds_f64(0.5), None));
    std::fs::remove_dir_all(dir).ok();
}

/// A video whose audio the user unlinked keeps exactly its clips when it is generated again,
/// with sound or without: no second picture over it, no second sound, and the user's audio
/// clip is not deleted.
#[test]
fn unlinked_audio_is_not_a_sound_only_placement() {
    let dir = tmp_dir("comfy-unlinked");
    let movie = dir.join("in.mov");
    make_movie(&movie, DemoScene::Plasma, 64, 36, 24);
    let mov = std::fs::read(&movie).unwrap();
    let fake = fake_runs(vec![
        vec![FakeOutput::file("20", "gifs", "a.mov", mov.clone())],
        vec![FakeOutput::file("20", "gifs", "b.mov", mov)],
        vec![FakeOutput::file("9", "images", "frame.png", png(64, 36))],
    ]);
    let mut s = session(fake);
    let (item, clip) = new_clip(&mut s, json!({"duration": 5}));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(clips_of(&s, item).len(), 2);
    s.execute("clip.link", json!({"clips": [clip.0]})).unwrap();
    assert!(clips_of(&s, item).iter().all(|c| c.3.is_none()), "unlinked");
    let kinds = |s: &Session| {
        let mut k: Vec<TrackKind> = clips_of(s, item).iter().map(|c| c.0).collect();
        k.sort_by_key(|k| *k == TrackKind::Audio);
        k
    };
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(kinds(&s), [TrackKind::Video, TrackKind::Audio], "video with sound again");
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    assert_eq!(kinds(&s), [TrackKind::Video, TrackKind::Audio], "a picture without sound");
    std::fs::remove_dir_all(dir).ok();
}

/// The interleaving the claim exists for: A chooses a name, B publishes that very name, then A
/// claims it. A must move on to the next number and leave B's file alone, on disk and in memory
/// (hosts that save through their services), where in-flight names are held session-wide.
#[test]
fn a_chosen_name_published_by_another_job_is_not_overwritten() {
    use filmcraft_comfyui::Sink;
    use std::io::Write as _;
    let out = filmcraft_comfyui::Output {
        node: "9".into(),
        key: "images".into(),
        kind: OutputKind::Image,
        file: Some(filmcraft_comfyui::FileRef { filename: "x.png".into(), subfolder: String::new(), folder: "output".into() }),
        text: None,
    };
    for in_memory in [false, true] {
        let dir = tmp_dir("comfy-interleave");
        let d = dir.to_string_lossy().into_owned();
        let services = crate::FsServices;
        let cancel = AtomicBool::new(false);
        let held = Mutex::default();
        let files = || OutputFiles {
            services: &services,
            dir: &d,
            name: "Shot",
            in_memory,
            held: &held,
            cancel: &cancel,
            current: String::new(),
            buffer: Vec::new(),
            written: Vec::new(),
        };
        let (mut a, mut b) = (files(), files());
        // A chooses 001 and pauses before claiming it
        let chosen = version_path(&services, &d, "Shot", "png", &lock(&held)).unwrap();
        assert!(chosen.ends_with("Shot 001.png"));
        // B claims, downloads and publishes 001
        b.create(&out).unwrap().write_all(b"from B").unwrap();
        b.close(&out, Some(6)).unwrap();
        // A resumes with its stale choice
        a.claim(chosen, "png").unwrap().write_all(b"from A").unwrap();
        a.close(&out, Some(6)).unwrap();
        assert_eq!(std::fs::read(dir.join("Shot 001.png")).unwrap(), b"from B", "in_memory={in_memory}");
        assert_eq!(std::fs::read(dir.join("Shot 002.png")).unwrap(), b"from A", "in_memory={in_memory}");
        // two in-flight downloads at once never share a name either
        let (mut c, mut e) = (files(), files());
        c.create(&out).unwrap().write_all(b"from C").unwrap();
        e.create(&out).unwrap().write_all(b"from E").unwrap();
        assert_ne!(c.current, e.current, "in_memory={in_memory}");
        e.close(&out, Some(6)).unwrap();
        c.close(&out, Some(6)).unwrap();
        assert_eq!(std::fs::read(dir.join("Shot 003.png")).unwrap(), b"from C", "in_memory={in_memory}");
        assert_eq!(std::fs::read(dir.join("Shot 004.png")).unwrap(), b"from E", "in_memory={in_memory}");
        assert!(lock(&held).is_empty(), "names are released once written");
        std::fs::remove_dir_all(dir).ok();
    }
}

/// An audio clip the user unlinked follows a shorter result like its picture: it never runs past
/// the end of the new media.
#[test]
fn unlinked_audio_follows_a_shorter_result() {
    let dir = tmp_dir("comfy-unlinked-shorter");
    let (long, short) = (dir.join("long.mov"), dir.join("short.mov"));
    make_movie(&long, DemoScene::Plasma, 64, 36, 24);
    make_movie(&short, DemoScene::Plasma, 64, 36, 12);
    let fake = fake_runs(vec![
        vec![FakeOutput::file("20", "gifs", "a.mov", std::fs::read(&long).unwrap())],
        vec![FakeOutput::file("20", "gifs", "b.mov", std::fs::read(&short).unwrap())],
    ]);
    let mut s = session(fake);
    let (item, clip) = new_clip(&mut s, json!({"duration": 5}));
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    s.execute("clip.link", json!({"clips": [clip.0]})).unwrap();
    generate(&mut s, &dir, json!({"item": item.0})).unwrap();
    let clips = clips_of(&s, item);
    assert_eq!(clips.len(), 2, "{clips:?}");
    let half = Tick::from_seconds_f64(0.5);
    assert!(clips.iter().all(|c| c.2 == half), "{clips:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// When every version name is taken, no name is handed out: the claim is refused (in memory a
/// taken name would be overwritten), and it works again once a name is free.
#[test]
fn no_name_is_handed_out_when_every_version_is_taken() {
    use filmcraft_comfyui::Sink;
    let dir = tmp_dir("comfy-full");
    let d = dir.to_string_lossy().into_owned();
    let services = crate::FsServices;
    let cancel = AtomicBool::new(false);
    let held: Mutex<BTreeSet<String>> = Mutex::new((1..=MAX_VERSIONS).map(|n| dir.join(format!("Shot {n:03}.png")).to_string_lossy().into_owned()).collect());
    assert_eq!(version_path(&services, &d, "Shot", "png", &lock(&held)), None);
    let out = filmcraft_comfyui::Output {
        node: "9".into(),
        key: "images".into(),
        kind: OutputKind::Image,
        file: Some(filmcraft_comfyui::FileRef { filename: "x.png".into(), subfolder: String::new(), folder: "output".into() }),
        text: None,
    };
    for in_memory in [true, false] {
        let mut f = OutputFiles {
            services: &services,
            dir: &d,
            name: "Shot",
            in_memory,
            held: &held,
            cancel: &cancel,
            current: String::new(),
            buffer: Vec::new(),
            written: Vec::new(),
        };
        let e = f.create(&out).err().unwrap_or_default();
        assert!(e.contains("no free file name"), "in_memory={in_memory}: {e}");
        // in memory, a stale choice of a held name is refused too (on disk the files decide)
        if in_memory {
            let e = f.claim(dir.join("Shot 007.png").to_string_lossy().into_owned(), "png").err().unwrap_or_default();
            assert!(e.contains("no free file name"), "{e}");
        }
    }
    lock(&held).remove(&dir.join("Shot 050.png").to_string_lossy().into_owned());
    assert!(version_path(&services, &d, "Shot", "png", &lock(&held)).unwrap().ends_with("Shot 050.png"));
    std::fs::remove_dir_all(dir).ok();
}
