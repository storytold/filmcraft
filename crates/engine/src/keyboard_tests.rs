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
    let ui_prefixes =
        ["tool.", "playback.", "window.", "view.", "app.", "help.", "mode.", "multicam.", "projectPanel.", "textPanel.", "panel.", "mediaBrowser."];
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

#[test]
fn keyboard_slide_keeps_linked_partners_aligned_at_a_neighbour_limit() {
    // #712: a three-frame sound-only follower limits only the sound partner
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Slide"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let r = s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": true})).unwrap();
    let item = r["item"].as_u64().unwrap();
    let fr = s.sequence_rate().tick_of(1);
    let r =
        s.execute("timeline.place", json!({"item": item, "track": "V1", "audioTrack": "A1", "time": 0, "sourceIn": fr.0 * 20, "duration": fr.0 * 60})).unwrap();
    let (v, a) = (r["clips"][0].as_u64().unwrap(), r["clips"][1].as_u64().unwrap());
    let r = s.execute("timeline.place", json!({"item": item, "track": "A1", "time": fr.0 * 60, "sourceIn": 0, "duration": fr.0 * 3})).unwrap();
    let f = r["clips"][0].as_u64().unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    s.execute("timeline.slideRight5", json!({})).unwrap();
    assert_eq!(clip(&s, v).start, Tick(fr.0 * 2), "the pair moves only as far as the sound may");
    assert_eq!(clip(&s, a).start, clip(&s, v).start, "picture and sound stay aligned");
    let fl = clip(&s, f);
    assert_eq!((fl.start, fl.duration, fl.source_in), (Tick(fr.0 * 62), fr, Tick(fr.0 * 2)));
    // nothing more to give: a further slide right moves neither partner
    s.execute("timeline.slideRight", json!({})).unwrap();
    assert_eq!((clip(&s, v).start, clip(&s, a).start), (Tick(fr.0 * 2), Tick(fr.0 * 2)));
}
