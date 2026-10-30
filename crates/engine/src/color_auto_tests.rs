use filmcraft_project::{ClipId, ParamValue};
use serde_json::{Value, json};

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The first V1 clip under the playhead, moved to its middle (the demo starts in a fade from black).
fn pick_clip(s: &mut Session) -> ClipId {
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].item_at(s.playhead()).unwrap().clone();
    let mid = it.start + filmcraft_time::Tick(it.range().duration.0 / 2);
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    s.state.selection = vec![];
    it.id
}

fn lumetri(s: &Session, clip: ClipId) -> Option<filmcraft_project::EffectInstance> {
    let q = s.active_sequence()?;
    let (_, it) = q.find_item(clip)?;
    it.effects.iter().find(|e| e.effect == "lumetri").cloned()
}

fn float(e: &filmcraft_project::EffectInstance, p: &str) -> f64 {
    match e.param(p).map(|v| &v.value) {
        Some(ParamValue::Float(f)) => *f,
        o => panic!("{p}: {o:?}"),
    }
}

fn mean_rb(img: &filmcraft_render::Image) -> f32 {
    let (mut r, mut b) = (0f64, 0f64);
    for p in img.px.as_chunks::<4>().0 {
        r += p[0] as f64;
        b += p[2] as f64;
    }
    (r / b.max(1e-9)) as f32
}

#[test]
fn corrects_the_clip_at_the_playhead_in_one_undo_step() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    assert!(lumetri(&s, clip).is_none());
    let n = s.history.undo.len();
    let r = s.execute("lumetri.auto", json!({"clip": clip.0})).unwrap();
    assert_eq!(r["corrected"], 1, "{r}");
    let it = &r["items"][0];
    assert_eq!((it["applied"].as_bool(), it["frames"].as_u64()), (Some(true), Some(1)), "{it}");
    assert_eq!(it["set"]["into"], "lumetri");
    assert!(it["measured"]["pixels"].as_u64().unwrap() > 100, "{it}");
    let conf = it["confidence"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&conf), "{it}");
    let e = lumetri(&s, clip).expect("Lumetri added");
    assert!(e.enabled && e.param("basic_on").and_then(|p| p.value.as_bool()) == Some(true));
    assert_eq!(float(&e, "temperature"), it["set"]["temperature"].as_f64().unwrap());
    assert_eq!(float(&e, "tint"), it["set"]["tint"].as_f64().unwrap());
    assert_eq!(float(&e, "exposure"), it["set"]["exposure"].as_f64().unwrap());
    assert!(float(&e, "exposure").abs() <= 5.0 && float(&e, "temperature").abs() <= 100.0);
    // one undo step, which also takes the added effect away
    assert_eq!(s.history.undo.len(), n + 1, "{:?}", s.history.undo.iter().map(|u| &u.0).collect::<Vec<_>>());
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Auto Correct"));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(lumetri(&s, clip).is_none());
    s.execute("edit.redo", json!({})).unwrap();
    assert!(lumetri(&s, clip).is_some());
}

#[test]
fn a_warm_cast_is_measured_and_corrected() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    // a warm, dark cast through the Tint effect (the measurement sees every non-Lumetri effect)
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "tint"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "tint", "param": "white", "value": "#d9a06b"})).unwrap();
    let before = mean_rb(&s.render_program(0.125).unwrap());
    let r = s.execute("lumetri.auto", json!({"clip": clip.0})).unwrap();
    let it = &r["items"][0];
    let temp = it["set"]["temperature"].as_f64().unwrap();
    assert!(temp < -8.0, "a warm picture needs cooling: {it}");
    let after = mean_rb(&s.render_program(0.125).unwrap());
    assert!((after - 1.0).abs() < (before - 1.0).abs(), "red/blue {before} → {after} ({it})");
    // running it again measures the source, not the correction, so the values hold
    let r2 = s.execute("lumetri.auto", json!({"clip": clip.0})).unwrap();
    let temp2 = r2["items"][0]["set"]["temperature"].as_f64().unwrap();
    assert!((temp - temp2).abs() < 1.5, "{temp} vs {temp2}");
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().filter(|e| e.effect == "lumetri").count(), 1);
}

#[test]
fn sampled_frames_items_and_measure_only() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let item = s.active_sequence().unwrap().find_item(clip).unwrap().1.item;
    let n = s.history.undo.len();
    // measure only: nothing changes
    let r = s.execute("lumetri.auto", json!({"clip": clip.0, "frames": "sampled", "samples": 3, "apply": false})).unwrap();
    let it = &r["items"][0];
    assert_eq!((it["applied"].as_bool(), it["frames"].as_u64()), (Some(false), Some(3)), "{it}");
    let times: Vec<i64> = it["times"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    let range = s.active_sequence().unwrap().find_item(clip).unwrap().1.range();
    assert!(times.windows(2).all(|w| w[0] < w[1]) && times.iter().all(|t| range.contains(filmcraft_time::Tick(*t))), "{times:?}");
    assert!(it.get("set").is_none() && lumetri(&s, clip).is_none() && s.history.undo.len() == n);
    // hostile sample counts are bounded
    let r = s.execute("lumetri.auto", json!({"clip": clip.0, "frames": "sampled", "samples": 0, "apply": false})).unwrap();
    assert_eq!(r["items"][0]["frames"], 1);
    let r = s.execute("lumetri.auto", json!({"clip": clip.0, "frames": "sampled", "samples": u64::MAX, "apply": false})).unwrap();
    assert_eq!(r["items"][0]["frames"].as_u64(), Some(crate::color_auto::MAX_SAMPLES));
    // `items`: every video clip of the media item, one undo step each
    let clips_of_item = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.item == item).count();
    let r = s.execute("lumetri.auto", json!({"items": [item.0], "whiteBalance": false})).unwrap();
    assert_eq!(r["corrected"].as_u64(), Some(clips_of_item as u64), "{r}");
    assert_eq!(s.history.undo.len(), n + clips_of_item);
    let set = &r["items"][0]["set"];
    assert!(set.get("temperature").is_none() && set.get("exposure").is_some(), "{set}");
    assert_eq!(r["items"][0]["measured"]["whiteBalanceConfidence"], 0.0);
    // the Project panel selection stands in for `items`
    s.state.selection = vec![];
    s.state.project_selection = vec![item];
    let r = s.execute("lumetri.auto", json!({"exposure": false, "apply": false})).unwrap();
    assert_eq!(r["corrected"].as_u64(), Some(clips_of_item as u64));
}

#[test]
fn keyframes_are_kept() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    let t = s.playhead();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "lumetri", "param": "exposure", "value": 1.0, "keyframe": true, "time": t.0})).unwrap();
    s.execute(
        "effects.setParam",
        json!({"clip": clip.0, "effect": "lumetri", "param": "exposure", "value": -1.0, "keyframe": true, "time": t.0 + 254_016_000_000}),
    )
    .unwrap();
    let kf = lumetri(&s, clip).unwrap().param("exposure").unwrap().keyframes.clone();
    assert_eq!(kf.len(), 2);
    let r = s.execute("lumetri.auto", json!({"clip": clip.0})).unwrap();
    let set = &r["items"][0]["set"];
    assert_eq!(set["keyframed"], json!(["exposure"]), "{set}");
    let e = lumetri(&s, clip).unwrap();
    assert_eq!(e.param("exposure").unwrap().keyframes, kf, "keyframes untouched");
    assert_eq!(float(&e, "temperature"), set["temperature"].as_f64().unwrap());
}

#[test]
fn raw_falls_back_to_lumetri_with_a_note_for_other_media() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let n = s.history.undo.len();
    let r = s.execute("lumetri.auto", json!({"clip": clip.0, "into": "raw"})).unwrap();
    let it = &r["items"][0];
    assert_eq!(it["set"]["into"], "lumetri", "{it}");
    assert!(it["note"].as_str().unwrap().contains("Lumetri"), "{it}");
    assert!(lumetri(&s, clip).is_some());
    assert_eq!(s.history.undo.len(), n + 1);
}

#[test]
fn clips_without_video_and_hostile_params_error_cleanly() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    let err = |r: crate::Result<Value>| r.err().map(|e| e.to_string()).unwrap_or_else(|| panic!("expected an error"));
    assert!(err(s.execute("lumetri.auto", json!({"clip": audio.0}))).contains("not a video clip"));
    assert!(err(s.execute("lumetri.auto", json!({"clips": [audio.0, 9_999_999]}))).contains("not a video clip"));
    assert!(err(s.execute("lumetri.auto", json!({"clips": []}))).contains("clips"));
    // an id of nothing stands in for no selection: the command is disabled, cleanly
    let e = err(s.execute("lumetri.auto", json!({"items": [9_999_999]})));
    assert!(e.contains("items") || e.contains("select"), "{e}");
    assert!(err(s.execute("lumetri.auto", json!({"clip": clip.0, "frames": "nope"}))).contains("frames"));
    assert!(err(s.execute("lumetri.auto", json!({"clip": clip.0, "into": "nope"}))).contains("into"));
    assert!(err(s.execute("lumetri.auto", json!({"clip": clip.0, "whiteBalance": false, "exposure": false}))).contains("nothing to do"));
    // nothing selected and nothing named: disabled, not a crash
    assert!(s.execute("lumetri.auto", json!({})).is_err());
    assert!(lumetri(&s, clip).is_none());
    // a mixed batch: the bad clip is reported, the good one corrected
    let r = s.execute("lumetri.auto", json!({"clips": [audio.0, clip.0], "targetGrey": -5.0, "highlightCeiling": 1e9})).unwrap();
    assert_eq!((r["corrected"].as_u64(), r["errors"].as_array().map(Vec::len)), (Some(1), Some(1)), "{r}");
    assert_eq!(r["errors"][0]["clip"], audio.0);
    assert!(lumetri(&s, clip).is_some());
    // every number in the result is finite JSON
    assert!(serde_json::to_string(&r).unwrap().len() > 10);
}

#[test]
fn the_timeline_selection_counts_only_its_video_clips() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    // a linked A/V selection: the audio partner is left out, not reported as an error
    s.state.selection = vec![clip, audio];
    let r = s.execute("lumetri.auto", json!({})).unwrap();
    assert_eq!((r["corrected"].as_u64(), r["errors"].as_array().map(Vec::len)), (Some(1), Some(0)), "{r}");
    assert_eq!(r["items"][0]["clip"], clip.0, "{r}");
    // only audio selected: the same error as an empty selection
    s.state.selection = vec![audio];
    let e = s.execute("lumetri.auto", json!({})).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("select video clips"), "{e}");
}

#[test]
fn frames_that_do_not_render_are_reported_not_measured() {
    use filmcraft_time::Tick;
    let times = [Tick(10), Tick(20), Tick(30)];
    let (frames, failed) = super::render_samples(&times, |t| (t != Tick(20)).then_some(t.0 * 2));
    assert_eq!(frames, vec![(Tick(10), 20), (Tick(30), 60)]);
    assert_eq!(failed, vec![Tick(20)]);
    // the result names only the measured times; the failed ones become an `errors` entry
    let v = json!({"clip": 7, "frames": 2, "times": [10, 30], "failedTimes": [20]});
    let e = super::sample_error(&v).expect("an error entry");
    assert_eq!((e["clip"].clone(), e["times"].clone()), (json!(7), json!([20])), "{e}");
    assert!(e["error"].as_str().unwrap().contains("measured from the other 2"), "{e}");
    assert!(super::sample_error(&json!({"clip": 7, "frames": 3})).is_none());
    assert!(super::sample_error(&json!({"clip": 7, "failedTimes": []})).is_none());
    // a demo clip renders every sample: no failed times, no errors
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let r = s.execute("lumetri.auto", json!({"clip": clip.0, "frames": "sampled", "samples": 3, "apply": false})).unwrap();
    assert_eq!((r["items"][0]["times"].as_array().map(Vec::len), r["errors"].as_array().map(Vec::len)), (Some(3), Some(0)), "{r}");
    assert!(r["items"][0].get("failedTimes").is_none(), "{r}");
}

#[test]
fn an_audio_only_selection_leaves_the_command_disabled() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    s.state.selection = vec![audio];
    assert!(!s.is_enabled("lumetri.auto"), "audio only");
    s.state.selection = vec![clip, audio];
    assert!(s.is_enabled("lumetri.auto"), "a video clip with its audio");
    s.state.selection = vec![];
    assert!(!s.is_enabled("lumetri.auto"), "nothing selected");
}

#[test]
fn the_command_is_in_the_color_menu_and_the_registry() {
    let spec = crate::find_command("lumetri.auto").expect("registered");
    assert_eq!((spec.label, spec.menu), ("Auto Correct", &["Color"][..]));
    assert!(spec.params.contains("\"clips\":[id]") && spec.params.contains("\"items\":[id]") && spec.params.contains("\"into\""));
}
