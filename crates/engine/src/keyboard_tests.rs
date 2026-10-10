//! Keyboard parity commands (M3.12): the shortcut tables, navigation, selection, trimming,
//! nudging, targeting, clip volume, text layers, poster and export frames.

use super::*;
use crate::shortcut_presets as presets;
use crate::shortcuts::Chord;
use filmcraft_project::ParamValue;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// (id, start, end) of the items on a track.
fn items(s: &Session, kind: TrackKind, idx: usize) -> Vec<(u64, Tick, Tick)> {
    s.active_sequence().unwrap().tracks(kind)[idx].items.iter().map(|i| (i.id.0, i.start, i.end())).collect()
}

fn clip(s: &Session, id: u64) -> filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1.clone()
}

fn track_of(s: &Session, id: u64) -> TrackId {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().0
}

/// Target only V1.
fn target_v1(s: &mut Session) {
    let tg = s.targeting();
    let v1 = s.active_sequence().unwrap().video_tracks[0].id;
    for t in tg.targeted {
        s.execute("timeline.setTargeting", json!({"track": t.0, "targeted": t == v1})).unwrap();
    }
    s.execute("timeline.setTargeting", json!({"track": v1.0, "targeted": true})).unwrap();
}

#[test]
fn shortcut_tables_have_no_key_used_twice_in_one_scope() {
    let tables: [(&str, Vec<presets::Entry>); 4] = [
        ("Premiere", presets::premiere()),
        ("FilmCraft panels", presets::FILMCRAFT_PANEL.iter().chain(presets::PREMIERE_PANEL).copied().collect()),
        ("Final Cut", presets::FINAL_CUT.to_vec()),
        ("Avid", presets::AVID.to_vec()),
    ];
    for (name, table) in tables {
        let mut seen: std::collections::BTreeMap<(String, String), &str> = Default::default();
        for (id, keys, panel) in table {
            let chord = Chord::parse(keys).unwrap_or_else(|e| panic!("{name}: `{keys}` for {id}: {e}")).canonical();
            if let Some(other) = seen.insert((panel.to_string(), chord.clone()), id) {
                panic!("{name}: {chord} in scope `{panel}` is bound to both {other} and {id}");
            }
        }
    }
    // the FilmCraft Default set (registry defaults + panel tables + the Premiere audit) has no
    // same-context conflicts for the new commands either
    let s = Session::default();
    let new_ids: Vec<&str> = keyboard::commands().iter().map(|c| c.id).collect();
    for (ctx, key, ids) in s.shortcuts.conflicts(crate::shortcuts::Platform::Mac) {
        assert!(!ids.iter().any(|i| new_ids.contains(&i.as_str())), "conflict on {key} ({ctx}): {ids:?}");
    }
}

#[test]
fn premiere_engine_entries_exist() {
    // UI-owned ids (tools, transport, panels…) are checked by the ui-egui keyboard test
    let ui_prefixes = [
        "tool.",
        "playback.",
        "window.",
        "view.",
        "app.",
        "help.",
        "mode.",
        "multicam.",
        "projectPanel.",
        "textPanel.",
        "effectControls.",
        "panel.",
        "mediaBrowser.",
    ];
    let ui_ids = ["timeline.expandAllTracks", "timeline.minimizeAllTracks", "graphics.beginTextEditing"];
    for (id, _, _) in presets::premiere() {
        if id.starts_with("timeline.") && (id.contains("Height") || id.contains("Screen")) || ui_ids.contains(&id) {
            continue;
        }
        if ui_prefixes.iter().any(|p| id.starts_with(p)) && commands::find(id).is_none() {
            continue;
        }
        assert!(commands::find(id).is_some(), "Premiere table names unknown engine command `{id}`");
    }
}

#[test]
fn edit_point_navigation_follows_targets_or_any_track() {
    let mut s = demo();
    target_v1(&mut s);
    let v1 = items(&s, TrackKind::Video, 0);
    let mut v1_edges: Vec<Tick> = v1.iter().flat_map(|c| [c.1, c.2]).collect();
    v1_edges.dedup();
    let all = s.active_sequence().unwrap().edit_points();
    s.set_playhead(Tick::ZERO);
    // Down walks V1's edit points only
    for e in v1_edges.iter().skip(1) {
        s.execute("playhead.nextEdit", json!({})).unwrap();
        assert_eq!(s.playhead(), *e);
    }
    // Shift+Up walks every track's edit points backwards
    let mut expect: Vec<Tick> = all.iter().copied().filter(|t| *t < s.playhead()).collect();
    expect.reverse();
    for e in expect.iter().take(3) {
        s.execute("playhead.prevEditAnyTrack", json!({})).unwrap();
        assert_eq!(s.playhead(), *e);
    }
    s.set_playhead(Tick::ZERO);
    s.execute("playhead.nextEditAnyTrack", json!({})).unwrap();
    assert_eq!(s.playhead(), all[1]);
}

#[test]
fn select_clip_at_playhead_next_previous_and_go_to_selected_clip() {
    let mut s = demo();
    target_v1(&mut s);
    let v1 = items(&s, TrackKind::Video, 0);
    let rate = s.sequence_rate();
    s.set_playhead(v1[1].1 + rate.tick_of(3));
    // D: the clip under the playhead on V1 (and its linked audio)
    let r = s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    assert!(s.state.selection.contains(&ClipId(v1[1].0)), "{r}");
    assert!(!s.state.selection.is_empty());
    // Cmd+Down / Cmd+Up step along the track
    s.execute("timeline.selectNextClip", json!({})).unwrap();
    assert!(s.state.selection.contains(&ClipId(v1[2].0)));
    assert!(!s.state.selection.contains(&ClipId(v1[1].0)));
    s.execute("timeline.selectPrevClip", json!({})).unwrap();
    s.execute("timeline.selectPrevClip", json!({})).unwrap();
    assert!(s.state.selection.contains(&ClipId(v1[0].0)));
    // Shift+Home / Shift+End
    s.execute("timeline.select", json!({"clips": [v1[1].0]})).unwrap();
    s.execute("playhead.selectedClipStart", json!({})).unwrap();
    assert_eq!(s.playhead(), v1[1].1);
    s.execute("playhead.selectedClipEnd", json!({})).unwrap();
    assert_eq!(s.playhead(), v1[1].2 - rate.frame_duration(), "parks on the clip's last frame");
    // nothing selected: disabled with a reason
    s.execute("edit.deselectAll", json!({})).unwrap();
    assert!(s.execute("playhead.selectedClipStart", json!({})).is_err());
}

#[test]
fn extend_previous_and_next_edit_roll_cuts_to_the_playhead() {
    let mut s = demo();
    target_v1(&mut s);
    let rate = s.sequence_rate();
    let v1 = items(&s, TrackKind::Video, 0);
    let dur0 = s.active_sequence().unwrap().duration();
    let undo0 = s.history.undo.len();
    // Shift+Q: the cut before the playhead rolls to it
    let ph = v1[1].1 + rate.tick_of(10);
    s.set_playhead(ph);
    s.execute("trim.extendPreviousEdit", json!({})).unwrap();
    assert_eq!(clip(&s, v1[0].0).end(), ph);
    assert_eq!(clip(&s, v1[1].0).start, ph);
    assert_eq!(s.active_sequence().unwrap().duration(), dur0, "a roll keeps the duration");
    assert_eq!(s.history.undo.len(), undo0 + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Extend Previous Edit To Playhead");
    s.undo();
    assert_eq!(clip(&s, v1[0].0).end(), v1[0].2);
    // Shift+W: the cut after the playhead rolls back to it
    let ph = v1[1].2 - rate.tick_of(4);
    s.set_playhead(ph);
    s.execute("trim.extendNextEdit", json!({})).unwrap();
    assert_eq!(clip(&s, v1[1].0).end(), ph);
    assert_eq!(clip(&s, v1[2].0).start, ph);
}

#[test]
fn nudge_slip_and_slide_the_selection_with_undo() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let v1 = items(&s, TrackKind::Video, 0);
    let last = *v1.last().unwrap();
    s.execute("timeline.select", json!({"clips": [last.0]})).unwrap();
    let sel: Vec<(ClipId, Tick)> = s.state.selection.iter().map(|c| (*c, clip(&s, c.0).start)).collect();
    assert!(sel.len() > 1, "the linked audio is selected too");
    // Cmd+Right: one frame later, linked audio too
    s.execute("timeline.nudgeRight", json!({})).unwrap();
    for (c, st) in &sel {
        assert_eq!(clip(&s, c.0).start, *st + rate.tick_of(1));
    }
    assert_eq!(s.history.undo.last().unwrap().0, "Nudge");
    s.execute("timeline.nudgeLeft5", json!({})).unwrap();
    assert_eq!(clip(&s, last.0).start, last.1 - rate.tick_of(4));
    s.undo();
    s.undo();
    assert_eq!(clip(&s, last.0).start, last.1);
    // Alt+Up / Alt+Down: to the track above (V1 -> V2) and back
    let v1_id = track_of(&s, last.0);
    s.state.selection = vec![ClipId(last.0)];
    s.execute("timeline.nudgeUp", json!({})).unwrap();
    assert_eq!(track_of(&s, last.0), s.active_sequence().unwrap().video_tracks[1].id);
    assert_eq!(clip(&s, last.0).start, last.1, "a vertical nudge keeps the time");
    s.execute("timeline.nudgeDown", json!({})).unwrap();
    assert_eq!(track_of(&s, last.0), v1_id);
    assert!(s.execute("timeline.nudgeDown", json!({})).is_err(), "there is no track below V1");
    // slip: the source In moves, the clip stays
    let mid = v1[1];
    s.execute("timeline.select", json!({"clips": [mid.0]})).unwrap();
    let in0 = clip(&s, mid.0).source_in;
    s.execute("timeline.slipLeft5", json!({})).unwrap();
    assert_eq!(clip(&s, mid.0).source_in, in0 + rate.tick_of(5));
    assert_eq!(clip(&s, mid.0).start, mid.1);
    // slide: the clip moves, its neighbours absorb it
    s.execute("timeline.slideRight", json!({})).unwrap();
    assert_eq!(clip(&s, mid.0).start, mid.1 + rate.tick_of(1));
    assert_eq!(clip(&s, v1[0].0).end(), mid.1 + rate.tick_of(1));
    assert_eq!(s.history.undo.last().unwrap().0, "Slide");
}

#[test]
fn targeting_toggles() {
    let mut s = demo();
    let seq = s.active_sequence().unwrap().clone();
    let vids: Vec<TrackId> = seq.video_tracks.iter().map(|t| t.id).collect();
    let auds: Vec<TrackId> = seq.audio_tracks.iter().map(|t| t.id).collect();
    // Cmd+0: all video targeted → none
    s.execute("timeline.toggleAllVideoTargets", json!({})).unwrap();
    assert!(vids.iter().all(|t| !s.targeting().targeted.contains(t)));
    assert!(auds.iter().all(|t| s.targeting().targeted.contains(t)), "audio untouched");
    s.execute("timeline.toggleAllVideoTargets", json!({})).unwrap();
    assert!(vids.iter().all(|t| s.targeting().targeted.contains(t)));
    // Cmd+9
    s.execute("timeline.toggleAllAudioTargets", json!({})).unwrap();
    assert!(auds.iter().all(|t| !s.targeting().targeted.contains(t)));
    // Toggle Target Audio 2
    s.execute("timeline.toggleTargetA2", json!({})).unwrap();
    assert_eq!(s.targeting().targeted.iter().filter(|t| auds.contains(t)).collect::<Vec<_>>(), vec![&auds[1]]);
    assert!(s.execute("timeline.toggleTargetA8", json!({})).is_err(), "no A8 in the demo");
    // Move All Audio Targets Up: A2 → A1 (the track above)
    s.execute("timeline.moveAudioTargetsUp", json!({})).unwrap();
    assert_eq!(s.targeting().targeted.iter().filter(|t| auds.contains(t)).collect::<Vec<_>>(), vec![&auds[0]]);
    assert!(s.execute("timeline.moveAudioTargetsUp", json!({})).is_err(), "already at the top");
    // Cmd+Alt+0 / Cmd+Alt+9: source patching off and back on (to V1 / A1)
    s.execute("timeline.toggleAllSourceVideo", json!({})).unwrap();
    assert_eq!(s.targeting().video_dest, None);
    s.execute("timeline.toggleAllSourceVideo", json!({})).unwrap();
    assert_eq!(s.targeting().video_dest, Some(vids[0]));
    s.execute("timeline.toggleAllSourceAudio", json!({})).unwrap();
    assert_eq!(s.targeting().audio_dest, None);
    // mute targeted audio tracks: undoable
    s.execute("timeline.toggleMuteTargetedAudio", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().audio_tracks[0].muted);
    assert!(!s.active_sequence().unwrap().audio_tracks[1].muted);
    s.undo();
    assert!(!s.active_sequence().unwrap().audio_tracks[0].muted);
    s.execute("timeline.toggleOutputTargetedVideo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks.iter().all(|t| !t.enabled));
    s.execute("timeline.toggleOutputTargetedVideo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks.iter().all(|t| t.enabled));
}

fn level(s: &Session, id: u64) -> f64 {
    clip(s, id).effect("volume").unwrap().params["level"].value.as_f64().unwrap()
}

#[test]
fn clip_volume_keys_and_scrub_toggle() {
    let mut s = demo();
    let a1 = items(&s, TrackKind::Audio, 0);
    let c = a1[0].0;
    let l0 = level(&s, c);
    s.execute("timeline.select", json!({"clips": [c]})).unwrap();
    s.execute("clip.volumeUp", json!({})).unwrap();
    assert_eq!(level(&s, c), l0 + 1.0);
    s.execute("clip.volumeDownMany", json!({})).unwrap();
    assert_eq!(level(&s, c), l0 + 1.0 - s.prefs.audio.large_volume_adjustment);
    s.undo();
    s.undo();
    assert_eq!(level(&s, c), l0);
    // a video clip's linked audio follows it
    let v = items(&s, TrackKind::Video, 0)[0].0;
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    s.execute("clip.volumeUp", json!({})).unwrap();
    assert_eq!(level(&s, c), l0 + 1.0);
    // Shift+S
    let on = s.prefs.audio.scrub_audio;
    let r = s.execute("audio.toggleScrubbing", json!({})).unwrap();
    assert_eq!(r["scrubAudio"], json!(!on));
    assert_eq!(s.prefs.audio.scrub_audio, !on);
}

#[test]
fn text_layer_keys_change_size_leading_alignment_and_position() {
    let mut s = demo();
    s.set_playhead(Tick::ZERO);
    let r = s.execute("graphics.newText", json!({"text": "Hello"})).unwrap();
    let c = r["clip"].as_u64().unwrap();
    s.execute("timeline.select", json!({"clips": [c]})).unwrap();
    let layer = |s: &Session| {
        let it = clip(s, c);
        let idx = filmcraft_project::graphic::layer_indices(&it.effects);
        it.effects[*idx.last().unwrap()].clone()
    };
    let f = |s: &Session, p: &str| layer(s).params[p].value.as_f64().unwrap_or(f64::NAN);
    let size0 = f(&s, "size");
    s.execute("graphics.fontSizeUp", json!({})).unwrap();
    s.execute("graphics.fontSizeUp5", json!({})).unwrap();
    assert_eq!(f(&s, "size"), size0 + 6.0);
    s.execute("graphics.leadingDown", json!({})).unwrap();
    assert_eq!(f(&s, "leading"), -1.0);
    s.execute("graphics.alignTextRight", json!({})).unwrap();
    assert_eq!(layer(&s).params["align"].value, ParamValue::Choice(2));
    let pos = |s: &Session| match layer(s).params["position"].value {
        ParamValue::Vec2(v) => (v.x, v.y),
        _ => panic!("position"),
    };
    let p0 = pos(&s);
    s.execute("graphics.nudgeRight5", json!({})).unwrap();
    s.execute("graphics.nudgeUp", json!({})).unwrap();
    assert_eq!(pos(&s), (p0.0 + 5.0, p0.1 - 1.0));
    // all undoable
    for _ in 0..6 {
        s.undo();
    }
    assert_eq!(f(&s, "size"), size0);
    assert_eq!(pos(&s), p0);
}

#[test]
fn nudge_object_moves_motion_position_of_video_clips() {
    let mut s = demo();
    let v = items(&s, TrackKind::Video, 0)[0];
    s.state.selection = vec![ClipId(v.0)];
    let pos = |s: &Session| match clip(s, v.0).effect("motion").unwrap().params["position"].value {
        ParamValue::Vec2(p) => (p.x, p.y),
        _ => panic!("position"),
    };
    let p0 = pos(&s);
    s.execute("graphics.nudgeLeft", json!({})).unwrap();
    s.execute("graphics.nudgeDown5", json!({})).unwrap();
    assert_eq!(pos(&s), (p0.0 - 1.0, p0.1 + 5.0));
}

#[test]
fn poster_frame_and_export_frame() {
    let mut s = demo();
    let v = items(&s, TrackKind::Video, 0)[0];
    let item = clip(&s, v.0).item;
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("source.setPlayhead", json!({"frame": 12})).unwrap();
    let r = s.execute("clip.setPosterFrame", json!({})).unwrap();
    let rate = s.project.item(item).unwrap().frame_rate();
    assert_eq!(r["posterFrame"], json!(rate.tick_of(12).0));
    assert_eq!(keyboard::poster_frame(s.project.item(item).unwrap()), Some(rate.tick_of(12)));
    s.execute("clip.clearPosterFrame", json!({})).unwrap();
    assert_eq!(keyboard::poster_frame(s.project.item(item).unwrap()), None);
    s.undo();
    assert!(keyboard::poster_frame(s.project.item(item).unwrap()).is_some());

    // Shift+E writes the Program frame
    let dir = std::env::temp_dir().join(format!("fc-export-frame-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("frame.png");
    let r = s.execute("file.exportFrame", json!({"path": path.to_string_lossy()})).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let seq = s.active_sequence().unwrap();
    assert_eq!((r["width"].as_u64().unwrap(), r["height"].as_u64().unwrap()), (seq.settings.width as u64, seq.settings.height as u64));
    // with Import, the still lands in the project
    let n = s.project.items.len();
    s.execute("file.exportFrame", json!({"path": dir.join("frame2.tiff").to_string_lossy(), "format": "tiff", "import": true})).unwrap();
    assert_eq!(s.project.items.len(), n + 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reveal_nested_sequence_opens_it_at_the_matching_frame() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    let v = items(&s, TrackKind::Video, 0)[1];
    s.execute("timeline.select", json!({"clips": [v.0]})).unwrap();
    s.execute("clip.nest", json!({"name": "Nest"})).unwrap();
    let nest_clip = s.active_sequence().unwrap().video_tracks[0].items.iter().find(|i| s.project.sequence(i.item).is_some()).unwrap().clone();
    let rate = s.sequence_rate();
    s.set_playhead(nest_clip.start + rate.tick_of(7));
    s.state.selection = vec![nest_clip.id];
    let r = s.execute("sequence.revealNested", json!({})).unwrap();
    assert_eq!(s.state.active_sequence, Some(nest_clip.item));
    assert_ne!(s.state.active_sequence, Some(outer));
    assert_eq!(s.playhead(), nest_clip.source_in + rate.tick_of(7), "{r}");
}

// ------------------------------------------------------------------ Effect Controls ▸ Clear

/// The first V1 clip with Gaussian Blur and Crop applied and two Scale keyframes: (clip id,
/// Motion's index, Scale keyframe times).
fn clip_with_effects_and_keys(s: &mut Session) -> (u64, usize, Vec<i64>) {
    let it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let id = it.id.0;
    for fx in ["gaussian_blur", "crop"] {
        s.execute("effects.apply", json!({"clips": [id], "effect": fx})).unwrap();
    }
    let rate = s.sequence_rate();
    for f in [0, 5] {
        s.set_playhead(it.start + rate.tick_of(f));
        s.execute("effects.addKeyframe", json!({"clip": id, "effect": "motion", "param": "scale"})).unwrap();
    }
    let it = clip(s, id);
    let motion = it.effects.iter().position(|e| e.effect == "motion").unwrap();
    let keys: Vec<i64> = it.effect("motion").unwrap().param("scale").unwrap().keyframes.iter().map(|k| k.time.0).collect();
    assert_eq!(keys.len(), 2);
    (id, motion, keys)
}

fn effect_index(s: &Session, id: u64, effect: &str) -> usize {
    clip(s, id).effects.iter().position(|e| e.effect == effect).unwrap()
}

/// Backspace / Delete in Effect Controls (`effects.clear`): the named keyframes when there are
/// any, else the named effects, each time as one undo step; fixed effects and the clip stay.
#[test]
fn effects_clear_takes_keyframes_first_then_effects_in_one_undo_step() {
    let mut s = demo();
    let (id, motion, keys) = clip_with_effects_and_keys(&mut s);
    let (blur, crop) = (effect_index(&s, id, "gaussian_blur"), effect_index(&s, id, "crop"));
    let n = clip(&s, id).effects.len();
    let kf = |t: i64| json!({"effect": motion, "param": "scale", "mediaTime": t});
    let undo = s.history.undo.len();
    // keyframes named (one twice): only they go; the effect named beside them stays
    let r = s.execute("effects.clear", json!({"clip": id, "keyframes": [kf(keys[0]), kf(keys[1]), kf(keys[0])], "effects": [blur]})).unwrap();
    assert_eq!(r, json!({"keyframes": 2, "effects": 0}));
    let it = clip(&s, id);
    assert!(it.effect("motion").unwrap().param("scale").unwrap().keyframes.is_empty());
    assert_eq!(it.effects.len(), n);
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    s.undo();
    assert_eq!(clip(&s, id).effect("motion").unwrap().param("scale").unwrap().keyframes.len(), 2, "undo brings both back");
    // effects by id work for keyframes too
    s.execute("effects.clear", json!({"clip": id, "keyframes": [{"effect": "motion", "param": "scale", "mediaTime": keys[1]}]})).unwrap();
    assert_eq!(clip(&s, id).effect("motion").unwrap().param("scale").unwrap().keyframes.len(), 1);
    s.undo();
    // no keyframes named: the effects go, as one step; the clip stays in the timeline
    let undo = s.history.undo.len();
    let r = s.execute("effects.clear", json!({"clip": id, "effects": [blur, crop, blur]})).unwrap();
    assert_eq!(r, json!({"keyframes": 0, "effects": 2}));
    let it = clip(&s, id);
    assert!(!it.effects.iter().any(|e| e.effect == "gaussian_blur" || e.effect == "crop"));
    assert!(it.effect("motion").is_some() && it.effect("opacity").is_some(), "fixed effects stay");
    assert_eq!(it.effects.len(), n - 2);
    assert_eq!(s.history.undo.len(), undo + 1);
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Remove Effects"));
    s.undo();
    assert_eq!(clip(&s, id).effects.len(), n, "undo brings both back");
    // nothing named: nothing happens, and no undo step
    let undo = s.history.undo.len();
    for p in [json!({"clip": id}), json!({"clip": id, "effects": [], "keyframes": null})] {
        assert_eq!(s.execute("effects.clear", p).unwrap(), json!({"keyframes": 0, "effects": 0}));
    }
    assert_eq!(s.history.undo.len(), undo);
}

/// AGENTS.md §0: `effects.clear` checks everything it is given before it changes anything.
#[test]
fn effects_clear_refuses_hostile_params_and_changes_nothing() {
    let mut s = demo();
    let (id, motion, keys) = clip_with_effects_and_keys(&mut s);
    let blur = effect_index(&s, id, "gaussian_blur");
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let kf = |effect: serde_json::Value, param: serde_json::Value, t: serde_json::Value| json!({"effect": effect, "param": param, "mediaTime": t});
    let good = kf(json!(motion), json!("scale"), json!(keys[0]));
    let mut cases = vec![
        json!({}),
        json!(null),
        json!([id]),
        json!({"clip": null}),
        json!({"clip": "x"}),
        json!({"clip": u64::MAX}),
        json!({"clip": 1e300}),
        json!({"clip": id, "effects": blur}),
        json!({"clip": id, "effects": "all"}),
        json!({"clip": id, "effects": {"0": blur}}),
        json!({"clip": id, "effects": [-1]}),
        json!({"clip": id, "effects": [1.5]}),
        json!({"clip": id, "effects": [1e300]}),
        json!({"clip": id, "effects": [u64::MAX]}),
        json!({"clip": id, "effects": [null]}),
        json!({"clip": id, "effects": ["gaussian_blur"]}),
        json!({"clip": id, "effects": [blur, 9999]}),
        json!({"clip": id, "effects": [motion]}),
        json!({"clip": id, "effects": [blur, motion]}),
        json!({"clip": id, "effects": vec![blur; 10_001]}),
        json!({"clip": id, "keyframes": "all"}),
        json!({"clip": id, "keyframes": [7]}),
        json!({"clip": id, "keyframes": [{}]}),
        json!({"clip": id, "keyframes": vec![good.clone(); 10_001]}),
        json!({"clip": id, "keyframes": [good.clone(), kf(json!(motion), json!("nope"), json!(keys[1]))]}),
    ];
    for effect in [json!(-1), json!("no_such"), json!(9999), json!(u64::MAX), json!(null), json!([motion]), json!(1.5)] {
        cases.push(json!({"clip": id, "keyframes": [kf(effect, json!("scale"), json!(keys[0]))]}));
    }
    for param in [json!(null), json!(5), json!(""), json!("scale\u{0}")] {
        cases.push(json!({"clip": id, "keyframes": [kf(json!(motion), param, json!(keys[0]))]}));
    }
    for t in [json!(null), json!("0"), json!(1.5), json!(keys[0] + 1), json!(i64::MIN), json!(i64::MAX), json!(u64::MAX)] {
        cases.push(json!({"clip": id, "keyframes": [kf(json!(motion), json!("scale"), t)]}));
    }
    for mask in [json!("x"), json!(-1), json!(99), json!(u64::MAX)] {
        let mut k = good.clone();
        k["mask"] = mask;
        cases.push(json!({"clip": id, "keyframes": [k]}));
    }
    for p in cases {
        assert!(s.execute("effects.clear", p.clone()).is_err(), "accepted {p}");
    }
    assert_eq!(*s.project, *before, "nothing was edited");
    assert_eq!(s.history.undo.len(), undo, "and nothing was added to the undo history");
}
