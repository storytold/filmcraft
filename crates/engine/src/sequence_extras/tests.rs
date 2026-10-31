//! Tests of the M3.11 Sequence / Markers commands (`sequence_extras`).

use std::sync::Arc;

use filmcraft_project::{ItemKind, MarkerKind, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn menu_ids(path: &[&str]) -> Vec<&'static str> {
    crate::commands::command_specs().iter().filter(|c| c.menu == path).map(|c| c.id).collect()
}

fn pos(ids: &[&str], id: &str) -> usize {
    ids.iter().position(|x| *x == id).unwrap_or_else(|| panic!("{id} not in {ids:?}"))
}

#[test]
fn sequence_and_markers_menu_order_matches_premiere() {
    let seq = menu_ids(&["Sequence"]);
    let order = [
        "sequence.settings",
        "sequence.deleteRenderFilesInToOut",
        "sequence.matchFrame",
        "sequence.addEdit",
        "trim.edit",
        "sequence.applyVideoTransition",
        "sequence.applyAudioTransition",
        "trim.applyDefaultTransition",
        "sequence.lift",
        "sequence.showThroughEdits",
        "sequence.normalizeMixTrack",
        "sequence.makeSubsequence",
        "sequence.transcribe",
        "sequence.simplify",
        "sequence.addTracks",
    ];
    for w in order.windows(2) {
        assert!(pos(&seq, w[0]) < pos(&seq, w[1]), "{} before {}: {seq:?}", w[0], w[1]);
    }
    assert_eq!(pos(&seq, "trim.applyDefaultTransition"), pos(&seq, "sequence.applyAudioTransition") + 1);
    let spec = crate::commands::find("trim.applyDefaultTransition").unwrap();
    assert_eq!(spec.label, "Apply Default Transitions to Selection");
    assert_eq!(Session::default().shortcuts.primary("trim.applyDefaultTransition").as_deref(), Some("Shift+D"));
    assert_eq!(crate::commands::find("sequence.normalizeMixTrack").unwrap().label, "Normalize Mix Track…");
    let caps = menu_ids(&["Sequence", "Captions"]);
    assert_eq!(pos(&caps, "captions.showActiveOnly"), pos(&caps, "captions.showAll") + 1);
    let markers = menu_ids(&["Markers"]);
    assert_eq!(pos(&markers, "markers.addFlashCue"), pos(&markers, "markers.addChapter") + 1);
}

#[test]
fn apply_default_transitions_to_selected_clips() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    // shot 3 (City Night): its in edge has no transition yet, its out edge has the dip to black
    let c = q.video_tracks[0].items[2].clone();
    let before_v = q.video_tracks[0].transitions.len();
    let before_a = q.audio_tracks[0].transitions.len();
    assert!(s.execute("trim.applyDefaultTransition", json!({})).is_err(), "nothing selected");
    s.execute("timeline.select", json!({"clips": [c.id.0]})).unwrap();
    assert!(s.is_enabled("trim.applyDefaultTransition"));
    let n_undo = s.history.undo.len();
    let r = s.execute("trim.applyDefaultTransition", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n_undo + 1, "one undo step");
    let q = s.active_sequence().unwrap();
    let vt = &q.video_tracks[0].transitions;
    assert_eq!(vt.len(), before_v + 1, "only the free in edge of the video clip gets one: {r}");
    let cut = vt.iter().find(|t| t.to == Some(c.id)).expect("transition at the in point");
    assert_eq!(cut.effect.effect, "cross_dissolve");
    assert_eq!(cut.start + Tick(cut.duration.0 / 2), c.start, "centred on the cut");
    // the linked audio clip (selected with it) gets audio transitions at its free edges
    assert!(q.audio_tracks[0].transitions.len() > before_a);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].transitions.len(), before_v);
}

#[test]
fn apply_video_transition_goes_to_the_selection_not_the_playhead() {
    // #1124: ⌘D with a clip selected adds the transition at its free edges wherever the playhead is
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let c = q.video_tracks[0].items[2].clone();
    let before_v = q.video_tracks[0].transitions.len();
    let before_a = q.audio_tracks[0].transitions.len();
    s.execute("playhead.set", json!({"time": (c.start + Tick(c.duration.0 / 2)).0})).unwrap();
    assert!(s.execute("sequence.applyVideoTransition", json!({})).is_err(), "no selection, no edit point at the playhead");
    s.execute("timeline.select", json!({"clips": [c.id.0]})).unwrap();
    let n_undo = s.history.undo.len();
    let r = s.execute("sequence.applyVideoTransition", json!({"effect": "wipe", "frames": 4})).unwrap();
    assert_eq!(r["transitions"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.history.undo.len(), n_undo + 1, "one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Apply Video Transition");
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].transitions.len(), before_v + 1, "the out edge keeps its dip to black");
    let t = q.video_tracks[0].transitions.iter().find(|t| t.to == Some(c.id)).expect("transition at the in point");
    assert_eq!(t.effect.effect, "wipe");
    assert_eq!(t.duration, s.sequence_rate().tick_of(4));
    assert_eq!(q.audio_tracks[0].transitions.len(), before_a, "video only, though the linked audio is selected");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].transitions.len(), before_v);

    // a selected edit point gets the transition at that edge only
    s.execute("trim.selectEditPoint", json!({"clip": c.id.0, "edge": "in"})).unwrap();
    s.execute("sequence.applyVideoTransition", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].transitions.len(), before_v + 1);
    assert!(q.video_tracks[0].transitions.iter().any(|t| t.to == Some(c.id) && t.effect.effect == "cross_dissolve"));

    // ⇧⌘D with the clip selected: audio only
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("timeline.select", json!({"clips": [c.id.0]})).unwrap();
    s.execute("sequence.applyAudioTransition", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].transitions.len(), before_v);
    assert!(q.audio_tracks[0].transitions.len() > before_a);
}

#[test]
fn normalize_mix_track_hits_the_target_peak() {
    let mut s = demo();
    let before = crate::sequence_extras::mix_peak_db(&s).unwrap();
    assert!(before.is_finite());
    let r = s.execute("sequence.normalizeMixTrack", json!({"db": -3.0})).unwrap();
    let after = crate::sequence_extras::mix_peak_db(&s).unwrap();
    assert!((after + 3.0).abs() < 0.05, "peak {after} dB ({r})");
    assert!((r["peakBeforeDb"].as_f64().unwrap() - before).abs() < 1e-9);
    assert!((s.active_sequence().unwrap().master_volume_db - r["masterVolumeDb"].as_f64().unwrap()).abs() < 1e-9);
    assert_eq!(s.history.undo.last().unwrap().0, "Normalize Mix Track");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().master_volume_db, 0.0);
    assert!(s.execute("sequence.normalizeMixTrack", json!({"db": 40.0})).is_err());
    // no audio: disabled
    s.execute("file.newSequence", json!({"name": "Empty"})).unwrap();
    assert!(!s.is_enabled("sequence.normalizeMixTrack"));
}

#[test]
fn transcribe_sequence_runs_transcript_generate_on_its_audio() {
    let mut s = demo();
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    t.words.push(Word::new("Hello", Tick::from_seconds_f64(0.1), Tick::from_seconds_f64(0.4)));
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    // a project selection must not change what Transcribe Sequence transcribes
    let music = s.project.items.values().find(|i| i.name == "Ambient_Score.wav").unwrap().id;
    s.execute("project.select", json!({"items": [music.0]})).unwrap();
    let r = s.execute("sequence.transcribe", json!({"track": "A1"})).unwrap();
    let q = s.active_sequence().unwrap();
    let expected: std::collections::BTreeSet<_> = q.audio_tracks[0].items.iter().map(|i| i.item).collect();
    assert_eq!(r["items"].as_array().unwrap().len(), expected.len(), "{r}");
    for i in &expected {
        assert!(s.project.transcripts.contains_key(i));
    }
    assert!(!s.project.transcripts.contains_key(&music));
    assert!(s.execute("sequence.transcribe", json!({"track": "A9"})).is_err());
}

#[test]
fn simplify_sequence_makes_a_clean_copy() {
    let mut s = demo();
    let orig_id = s.state.active_sequence.unwrap();
    let orig = s.active_sequence().unwrap().clone();
    // remove the V2 overlay and the A2 score, disable shot 2 (video + linked audio), blur shot 1
    let overlay = orig.video_tracks[1].items[0].id;
    let score = orig.audio_tracks[1].items[0].id;
    s.execute("timeline.select", json!({"clips": [overlay.0, score.0]})).unwrap();
    s.execute("edit.clear", json!({"clips": [overlay.0, score.0]})).unwrap();
    let c2 = orig.video_tracks[0].items[1].id;
    s.execute("timeline.select", json!({"clips": [c2.0]})).unwrap();
    s.execute("clip.enable", json!({})).unwrap();
    let blur = orig.video_tracks[0].items[0].id;
    s.execute("effects.apply", json!({"clips": [blur.0], "effect": "gaussian_blur"})).unwrap();
    let n_items = s.project.items.len();
    let r = s.execute("sequence.simplify", json!({"removeDisabled": true, "removeEmptyTracks": true, "closeGaps": true, "removeVideoEffects": true})).unwrap();
    assert_eq!(s.project.items.len(), n_items + 1);
    let new_id = filmcraft_project::ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(new_id), "the copy opens");
    assert_eq!(s.project.item(new_id).unwrap().name, "Main Edit (Simplified)");
    let q = s.project.sequence(new_id).unwrap();
    q.check().unwrap();
    assert!(q.find_item(c2).is_none());
    assert!(q.all_tracks().all(|t| t.items.iter().all(|i| i.enabled)));
    assert_eq!(r["removedClips"], 2, "{r}");
    // V2 / V3 / A2 / A3 were empty
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 1), "{r}");
    // the gap left by shot 2 is closed: V1 is continuous from 0
    let v1 = &q.video_tracks[0].items;
    assert_eq!(v1[0].start, Tick::ZERO);
    for w in v1.windows(2) {
        assert_eq!(w[0].end(), w[1].start);
    }
    assert_eq!(r["closedGaps"].as_i64().unwrap(), orig.video_tracks[0].items[1].duration.0);
    // effects: Gaussian Blur and the demo's Lumetri gone, Motion & Opacity kept
    assert!(q.video_tracks.iter().flat_map(|t| &t.items).all(|i| i.effects.iter().all(|e| e.def().is_some_and(|d| d.intrinsic))));
    assert!(q.video_tracks[0].items[0].effect("motion").is_some());
    // the original is untouched
    let o = s.project.sequence(orig_id).unwrap();
    assert!(o.find_item(c2).is_some() && o.video_tracks.len() == 3);
    // keep audio only
    s.execute("sequence.open", json!({"item": orig_id.0})).unwrap();
    let r2 = s.execute("sequence.simplify", json!({"keep": "audio", "name": "Audio Only"})).unwrap();
    let q2 = s.project.sequence(filmcraft_project::ItemId(r2["sequence"].as_u64().unwrap())).unwrap();
    assert!(q2.video_tracks.is_empty() && !q2.audio_tracks.is_empty());
    assert!(q2.all_tracks().flat_map(|t| &t.items).all(|i| i.link.is_none()), "links to removed video are dropped");
    assert!(s.execute("sequence.simplify", json!({"keep": "nope"})).is_err());
    // undo removes the copy
    let n = s.project.items.len();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), n - 1);
    assert!(matches!(s.project.item(orig_id).unwrap().kind, ItemKind::Sequence(_)));
}

#[test]
fn simplify_moves_clips_down_where_they_fit() {
    let s = demo();
    let mut q = s.active_sequence().unwrap().clone();
    // the V2 overlay sits over V1 shots: it can't move; delete V1 under it and it can
    let ov = q.video_tracks[1].items[0].clone();
    let o = crate::sequence_extras::SimplifyOptions::from_params(&json!({"moveClipsDown": true, "removeEmptyTracks": false})).unwrap();
    let mut a = q.clone();
    crate::sequence_extras::simplify_sequence(&s.project, &mut a, &o);
    assert!(a.video_tracks[1].item(ov.id).is_some());
    let under: Vec<_> = q.video_tracks[0].items.iter().filter(|i| i.range().overlaps(&ov.range())).map(|i| i.id).collect();
    filmcraft_edit::delete_items(&mut q, &under);
    for t in &mut q.video_tracks {
        t.transitions.clear();
    }
    let r = crate::sequence_extras::simplify_sequence(&s.project, &mut q, &o);
    assert_eq!(r["movedClips"], 1);
    assert!(q.video_tracks[0].item(ov.id).is_some() && q.video_tracks[1].items.is_empty());
    q.check().unwrap();
}

#[test]
fn show_active_caption_track_only() {
    let mut s = demo();
    assert!(!s.is_enabled("captions.showActiveOnly"), "no caption tracks");
    let a = s.execute("captions.newTrack", json!({"name": "English"})).unwrap();
    let b = s.execute("captions.newTrack", json!({"name": "French"})).unwrap();
    let id_of = |v: &serde_json::Value| v["track"].as_u64().or_else(|| v.as_u64()).unwrap();
    let (ta, tb) = (id_of(&a), id_of(&b));
    s.execute("captions.showActiveOnly", json!({"track": tb})).unwrap();
    let on: Vec<(u64, bool)> = s.active_sequence().unwrap().caption_tracks.iter().map(|t| (t.id.0, t.enabled)).collect();
    assert!(on.contains(&(tb, true)) && on.contains(&(ta, false)), "{on:?}");
    s.execute("captions.showActiveOnly", json!({"track": "C1"})).unwrap();
    let first = s.active_sequence().unwrap().caption_tracks[0].id.0;
    assert!(s.active_sequence().unwrap().caption_tracks.iter().all(|t| t.enabled == (t.id.0 == first)));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().caption_tracks.iter().any(|t| t.id.0 == tb && t.enabled));
    assert!(s.execute("captions.showActiveOnly", json!({"track": "C9"})).is_err());
}

#[test]
fn flash_cue_markers_round_trip() {
    let mut s = demo();
    let r = s.execute("markers.addFlashCue", json!({"seconds": 2.0, "comment": "cue"})).unwrap();
    let id = r["marker"].as_u64().unwrap();
    let q = s.active_sequence().unwrap();
    let m = q.markers.iter().find(|m| m.id.0 == id).unwrap();
    assert_eq!((m.kind, m.name.as_str(), m.comment.as_str()), (MarkerKind::FlashCue, "Cue Point 1", "cue"));
    assert_eq!(m.start, s.sequence_rate().snap(Tick::from_seconds_f64(2.0)));
    let back = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project;
    assert_eq!(back, *s.project);
    let r2 = s.execute("markers.addFlashCue", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.markers.iter().find(|m| m.id.0 == r2["marker"].as_u64().unwrap()).unwrap().name, "Cue Point 2");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().markers.iter().all(|m| m.kind != MarkerKind::FlashCue));
}

/// Apply Video Transition (Cmd+D) at both ends of an adjustment layer with Lumetri Color fades the
/// grade in and out, as on a clip: halfway through each dissolve the picture is about halfway
/// between the original and the graded frame, not the original with a hard cut after it.
#[test]
fn cmd_d_on_an_adjustment_layer_fades_its_effects_in_and_out() {
    let mut s = demo();
    let item = s.execute("file.newAdjustmentLayer", json!({"seconds": 4})).unwrap()["item"].as_u64().unwrap();
    let layer = s.execute("timeline.place", json!({"item": item, "track": "V3", "seconds": 1.5})).unwrap()["clips"][0].as_u64().unwrap();
    s.execute("effects.apply", json!({"clips": [layer], "effect": "lumetri"})).unwrap();
    s.execute("effects.setParam", json!({"clip": layer, "effect": "lumetri", "param": "exposure", "value": 2.0})).unwrap();
    s.execute("timeline.select", json!({"clips": [layer]})).unwrap();
    for edge in ["in", "out"] {
        s.execute("sequence.applyVideoTransition", json!({"clip": layer, "edge": edge, "frames": 24})).unwrap();
    }
    let transitions: Vec<_> = s.active_sequence().unwrap().video_tracks[2].transitions.clone();
    assert_eq!(transitions.len(), 2, "a dissolve at each end");
    let mean = |s: &Session, t: Tick| {
        let img = s.render_program_at(0.25, t).unwrap();
        img.px.chunks(4).map(|c| f64::from(c[0] + c[1] + c[2])).sum::<f64>() / (img.w * img.h) as f64
    };
    for tr in &transitions {
        let mid = tr.start + Tick(tr.duration.0 / 2);
        let mf = mean(&s, mid);
        // the original frame: the layer switched off; the graded frame: without the dissolves
        s.execute("clip.enable", json!({"clips": [layer]})).unwrap();
        let m0 = mean(&s, mid);
        s.execute("edit.undo", json!({})).unwrap();
        let ids: Vec<u64> = transitions.iter().map(|t| t.id.0).collect();
        s.execute("sequence.removeTransition", json!({"transitions": ids})).unwrap();
        let m1 = mean(&s, mid);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(m1 > m0 * 1.2, "the grade brightens the frame: {m0} → {m1}");
        let k = (mf - m0) / (m1 - m0);
        assert!((0.25..0.75).contains(&k), "halfway through the dissolve the grade is about half on: {k:.3}");
    }
}
