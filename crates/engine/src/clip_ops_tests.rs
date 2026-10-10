//! Tests of the M3.10 Edit / Clip / File menu commands (`clip_ops`): each one executes, checks the
//! result, undoes / redoes where it edits, and checks its disabled state.

use filmcraft_project::{AudioChannels, FieldProcessing, ItemId, ItemKind, Label, TimeInterpolation, TrackItem};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn item_named(s: &Session, name: &str) -> ItemId {
    s.project.items.values().find(|i| i.name == name).map(|i| i.id).unwrap_or_else(|| panic!("no item {name}"))
}

fn v1(s: &Session) -> Vec<TrackItem> {
    s.active_sequence().unwrap().video_tracks[0].items.clone()
}

fn clip(s: &Session, id: u64) -> TrackItem {
    s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(id)).unwrap().1.clone()
}

fn select(s: &mut Session, clips: &[u64]) {
    s.execute("timeline.select", json!({"clips": clips})).unwrap();
}

fn menu_of(id: &str) -> Vec<&'static str> {
    crate::commands::find(id).unwrap_or_else(|| panic!("no command {id}")).menu.to_vec()
}

fn f64_param(it: &TrackItem, effect: &str, param: &str) -> f64 {
    it.effect(effect).unwrap().param(param).unwrap().value.as_f64().unwrap()
}

#[test]
fn menus_and_shortcuts_follow_premiere() {
    let ids: Vec<&str> = crate::commands::command_specs().iter().map(|c| c.id).collect();
    let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap_or_else(|| panic!("{id} missing"));
    // Edit ▸ Label: Select Label Group, then the 16 colours in Premiere's order
    let label: Vec<&str> = crate::commands::command_specs().iter().filter(|c| c.menu == ["Edit", "Label"]).map(|c| c.label).collect();
    assert_eq!(label[0], "Select Label Group");
    assert_eq!(&label[1..], Label::ALL.iter().map(|l| l.name()).collect::<Vec<_>>().as_slice());
    assert!(crate::commands::find("edit.label").unwrap().menu.is_empty(), "the parameterised label command is for agents only");
    assert_eq!(pos("edit.pasteAttributes"), pos("edit.pasteInsert") + 1);
    assert_eq!(pos("edit.removeAttributes"), pos("edit.pasteAttributes") + 1);
    assert_eq!(pos("edit.selectAllMatching"), pos("edit.selectAll") + 1);
    assert!(pos("edit.removeUnused") > pos("edit.label.yellow") && pos("edit.consolidateDuplicates") == pos("edit.removeUnused") + 1);
    assert_eq!(pos("file.newSequenceFromClip"), pos("file.newSequence") + 1);
    assert_eq!(pos("file.newBinFromSelection"), pos("file.newBin") + 1);
    // Close Project, Close All Projects, Close All Other Projects (M3.11), Save
    assert_eq!(pos("file.closeProject") + 3, pos("file.save"));
    assert_eq!(pos("file.saveAll"), pos("file.saveCopy") + 2, "Save a Copy, Save as Template (M3.11), Save All");
    assert_eq!(menu_of("clip.timeInterpolation.opticalFlow"), ["Clip", "Video Options", "Time Interpolation"]);
    assert_eq!(menu_of("clip.audioGain"), ["Clip", "Audio Options"]);
    assert_eq!(menu_of("clip.replaceFromSourceMatchFrame"), ["Clip", "Replace With Clip"]);
    // Video Options in Premiere's order
    let vo: Vec<&str> = crate::commands::command_specs().iter().filter(|c| c.menu.get(1) == Some(&"Video Options")).map(|c| c.label).collect();
    assert_eq!(
        vo,
        [
            "Frame Hold Options…",
            "Add Frame Hold",
            "Insert Frame Hold Segment",
            "Field Options…",
            "Frame Sampling",
            "Frame Blending",
            "Optical Flow",
            "Scale to Frame Size",
            "Fit to frame",
            "Fill frame"
        ]
    );
    let sc = |id: &str| crate::commands::find(id).unwrap().shortcut;
    assert_eq!(sc("edit.pasteAttributes"), Some("Cmd+Alt+V"));
    assert_eq!(sc("clip.makeSubclip"), Some("Cmd+U"));
    assert_eq!(sc("clip.audioChannels"), Some("Shift+G"));
    assert_eq!(sc("file.newBinFromSelection"), Some("Shift+B"));
    assert_eq!(sc("file.closeProject"), Some("Cmd+Shift+W"));
    // every id is unique
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
}

#[test]
fn label_commands_and_select_label_group() {
    let mut s = demo();
    assert!(s.execute("edit.label.rose", json!({})).is_err(), "disabled with nothing selected");
    let c = v1(&s)[0].id.0;
    select(&mut s, &[c]);
    s.execute("edit.label.rose", json!({})).unwrap();
    assert_eq!(clip(&s, c).label, Label::Rose);
    assert_eq!(s.history.undo.last().unwrap().0, "Label Rose");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, c).label, Label::Iris);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip(&s, c).label, Label::Rose);
    // project items too
    let item = item_named(&s, "Ocean_Sunset.mp4");
    s.state.selection.clear();
    s.execute("project.select", json!({"items": [item.0]})).unwrap();
    s.execute("edit.label.teal", json!({})).unwrap();
    assert_eq!(s.project.item(item).unwrap().label, Label::Teal);
    // Select Label Group (project): every Iris item comes along with the Teal one? No: only Teal.
    s.execute("edit.selectLabelGroup", json!({})).unwrap();
    assert_eq!(s.state.project_selection, vec![item]);
    s.execute("project.select", json!({"items": [item_named(&s, "Aurora_Timelapse.mp4").0]})).unwrap();
    let r = s.execute("edit.selectLabelGroup", json!({})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 5, "the other five Iris footage items: {r}");
    // Select Label Group (timeline): the Mango overlay alone, the Iris clips together
    s.state.project_selection.clear();
    let ov = s.active_sequence().unwrap().video_tracks[1].items[0].id.0;
    s.state.selection = vec![filmcraft_project::ClipId(ov)];
    s.execute("edit.selectLabelGroup", json!({})).unwrap();
    assert_eq!(s.state.selection.len(), 1);
}

#[test]
fn select_all_matching_selects_every_use_of_the_source() {
    let mut s = demo();
    let c = v1(&s)[4].clone();
    select(&mut s, &[c.id.0]);
    s.execute("edit.selectAllMatching", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(s.state.selection.len(), 3, "V1 clip, its audio and the V2 overlay of the same source");
    assert!(s.state.selection.iter().all(|x| q.find_item(*x).unwrap().1.item == c.item));
    s.state.selection.clear();
    assert!(s.execute("edit.selectAllMatching", json!({})).is_err());
}

#[test]
fn paste_attributes_with_scaled_keyframes_and_remove_attributes() {
    let mut s = demo();
    let (a, b) = (v1(&s)[0].clone(), v1(&s)[2].clone());
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation", "value": 30.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "opacity", "param": "opacity", "value": 40.0})).unwrap();
    s.execute("timeline.select", json!({"clips": [a.id.0]})).unwrap();
    s.execute("effects.apply", json!({"clips": [a.id.0], "effect": "gaussian_blur"})).unwrap();
    // animate scale over the whole of clip A (media time keyframes)
    let a_now = clip(&s, a.id.0);
    {
        let mut p = (*s.project).clone();
        let seq = s.state.active_sequence.unwrap();
        let it = p.sequence_mut(seq).unwrap().find_item_mut(a.id).unwrap().1;
        let sc = it.effect_mut("motion").unwrap().param_mut("scale").unwrap();
        sc.put_keyframe(a_now.source_in, filmcraft_project::ParamValue::Float(100.0));
        sc.put_keyframe(a_now.source_out(), filmcraft_project::ParamValue::Float(200.0));
        s.project = std::sync::Arc::new(p);
    }
    select(&mut s, &[a.id.0]);
    s.execute("edit.copy", json!({})).unwrap();
    s.state.selection.clear();
    assert!(s.execute("edit.pasteAttributes", json!({})).is_err(), "nothing selected");
    select(&mut s, &[b.id.0]);
    let r = s.execute("edit.pasteAttributes", json!({"opacity": false})).unwrap();
    assert_eq!(r["clips"], 1, "video only; the audio partner has nothing different to paste: {r}");
    let got = clip(&s, b.id.0);
    assert_eq!(f64_param(&got, "motion", "rotation"), 30.0);
    assert_eq!(f64_param(&got, "opacity", "opacity"), 100.0, "opacity left alone");
    assert!(got.effect("gaussian_blur").is_some());
    let kf: Vec<Tick> = got.effect("motion").unwrap().param("scale").unwrap().keyframes.iter().map(|k| k.time).collect();
    assert_eq!(kf, vec![got.source_in, got.source_out()], "Scale Attribute Times fits A's keyframes to B");
    // without scaling, keyframes keep their distance from the clip start
    s.execute("edit.undo", json!({})).unwrap();
    assert!(clip(&s, b.id.0).effect("gaussian_blur").is_none(), "one undo step");
    s.execute("edit.pasteAttributes", json!({"scaleTimes": false, "effects": false})).unwrap();
    let got = clip(&s, b.id.0);
    let kf: Vec<Tick> = got.effect("motion").unwrap().param("scale").unwrap().keyframes.iter().map(|k| k.time).collect();
    assert_eq!(kf, vec![got.source_in, got.source_in + (a_now.source_out() - a_now.source_in)]);
    assert!(got.effect("gaussian_blur").is_none(), "effects unticked");
    assert_eq!(f64_param(&got, "opacity", "opacity"), 40.0);
    // Remove Attributes: Motion back to defaults, effects gone, opacity kept when unticked
    s.execute("edit.pasteAttributes", json!({})).unwrap();
    s.execute("edit.removeAttributes", json!({"opacity": false})).unwrap();
    let got = clip(&s, b.id.0);
    assert_eq!(f64_param(&got, "motion", "rotation"), 0.0);
    assert_eq!(f64_param(&got, "motion", "scale"), 100.0);
    assert!(got.effect("motion").unwrap().param("scale").unwrap().keyframes.is_empty());
    assert_eq!(got.effect("motion").unwrap().param("position").unwrap().value.as_vec2().unwrap(), filmcraft_geom::Vec2::new(960.0, 540.0));
    assert!(got.effect("gaussian_blur").is_none());
    assert_eq!(f64_param(&got, "opacity", "opacity"), 40.0);
    // audio: volume pasted onto another audio clip
    let aud = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    s.execute("timeline.select", json!({"clips": [aud[0].id.0]})).unwrap();
    s.execute("effects.setParam", json!({"clip": aud[0].id.0, "effect": "volume", "param": "level", "value": -3.0})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    select(&mut s, &[aud[0].id.0]);
    s.execute("edit.copy", json!({})).unwrap();
    select(&mut s, &[aud[3].id.0]);
    s.execute("edit.pasteAttributes", json!({})).unwrap();
    assert_eq!(f64_param(&clip(&s, aud[3].id.0), "volume", "level"), -3.0);
}

#[test]
fn remove_unused_and_consolidate_duplicates() {
    let mut s = demo();
    let n = s.project.items.len();
    let bars = item_named(&s, "Bars and Tone");
    let r = s.execute("edit.removeUnused", json!({})).unwrap();
    let removed: Vec<u64> = r["removed"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert!(removed.contains(&bars.0));
    assert_eq!(removed.len(), 3, "bars, leader and colour matte: {r}");
    assert!(s.project.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_))).count() == 2, "sequences are kept");
    assert_eq!(s.project.items.len(), n - 3);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), n);
    // a subclip of an unused clip keeps its parent; removing an unused subclip too
    // duplicates: Duplicate makes a copy of the same media; use it on the timeline then consolidate
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    let dup = ItemId(s.execute("edit.duplicate", json!({})).unwrap()["items"][0].as_u64().unwrap());
    s.execute("timeline.place", json!({"item": dup.0, "track": "V3", "audioTrack": "A3", "seconds": 1.0})).unwrap();
    let r = s.execute("edit.consolidateDuplicates", json!({})).unwrap();
    assert_eq!(r["removed"], json!([dup.0]));
    assert!(s.project.item(dup).is_none());
    let q = s.active_sequence().unwrap();
    assert!(q.all_tracks().flat_map(|t| t.items.iter()).all(|i| i.item != dup), "clips now use the kept item");
    assert_eq!(q.video_tracks[2].items[0].item, ocean);
    assert_eq!(s.execute("edit.consolidateDuplicates", json!({})).unwrap()["removed"], json!([]));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(dup).is_some());
}

#[test]
fn new_sequence_from_clip_bin_from_selection_and_offline_file() {
    let mut s = demo();
    assert!(s.execute("file.newSequenceFromClip", json!({})).is_err(), "nothing selected in the Project panel");
    let a = item_named(&s, "Desert_Dunes.mp4");
    let b = item_named(&s, "Misty_Forest.mp4");
    s.execute("project.select", json!({"items": [a.0, b.0]})).unwrap();
    let undo0 = s.history.undo.len();
    let r = s.execute("file.newSequenceFromClip", json!({})).unwrap();
    let seq = ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(seq));
    assert_eq!(s.project.item(seq).unwrap().name, "Desert_Dunes.mp4");
    let q = s.project.sequence(seq).unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 2, "both clips, one after the other");
    assert_eq!(q.video_tracks[0].items[1].start, q.video_tracks[0].items[0].end());
    assert_eq!(q.audio_tracks[0].items.len(), 2);
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(seq).is_none());
    // Bin From Selection
    s.execute("project.select", json!({"items": [a.0, b.0]})).unwrap();
    let r = s.execute("file.newBinFromSelection", json!({})).unwrap();
    let bin = filmcraft_project::BinId(r["bin"].as_u64().unwrap());
    let found = s.project.root.find_bin(bin).unwrap();
    assert_eq!(found.name, "Bin 01");
    assert_eq!(s.project.root.parent_of(a), Some(bin));
    assert_eq!(s.project.root.parent_of(b), Some(bin));
    // Offline File
    let r = s
        .execute("file.newOfflineFile", json!({"fileName": "A001C003.mov", "tapeName": "A001", "timecode": "01:00:00:00", "seconds": 4.0, "audio": false}))
        .unwrap();
    let it = s.project.item(ItemId(r["item"].as_u64().unwrap())).unwrap();
    let m = it.as_media().unwrap();
    assert!(m.offline && !m.info.has_audio() && m.info.video.is_some());
    assert_eq!(m.info.duration, s.sequence_rate().snap_nearest(Tick(4 * TICKS_PER_SECOND)));
    assert_eq!(m.info.start_timecode, Some(3600 * 24));
    assert_eq!(it.metadata["Tape Name"], "A001");
    assert!(s.execute("file.newOfflineFile", json!({"video": false, "audio": false})).is_err());
}

#[test]
fn close_project_and_save_all() {
    let mut s = demo();
    assert!(s.execute("file.saveAll", json!({})).is_err(), "never saved: use Save As");
    let dir = tmp_dir("close-project");
    let path = dir.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    s.execute("project.select", json!({"items": [item_named(&s, "Ocean_Sunset.mp4").0]})).unwrap();
    s.execute("edit.label.rose", json!({})).unwrap();
    assert!(s.is_dirty());
    s.execute("file.saveAll", json!({})).unwrap();
    assert!(!s.is_dirty());
    s.execute("edit.label.tan", json!({})).unwrap();
    assert!(s.execute("file.closeProject", json!({})).is_err(), "unsaved changes");
    s.execute("file.closeProject", json!({"force": true})).unwrap();
    assert!(s.project.items.is_empty() && s.path.is_none() && s.state.active_sequence.is_none());
    assert!(!s.history.can_undo());
}

#[test]
fn make_and_edit_subclips() {
    let mut s = demo();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    let r24 = filmcraft_time::FrameRate::FPS_23_976;
    s.execute("source.open", json!({"item": ocean.0})).unwrap();
    s.execute("project.setMarks", json!({"item": ocean.0, "in": r24.tick_of(24).0, "out": r24.tick_of(71).0})).unwrap();
    let r = s.execute("clip.makeSubclip", json!({})).unwrap();
    let sub = ItemId(r["item"].as_u64().unwrap());
    let it = s.project.item(sub).unwrap();
    assert_eq!(it.name, "Ocean_Sunset.mp4.Subclip");
    let ItemKind::Subclip { parent, range, restrict_trims } = it.kind.clone() else { panic!() };
    assert_eq!((parent, range.start, range.end(), restrict_trims), (ocean, r24.tick_of(24), r24.tick_of(72), true));
    assert_eq!(s.project.root.parent_of(sub), s.project.root.parent_of(ocean), "made in the parent's bin");
    // edit it in: the clip shows the subclip's media
    s.execute("source.open", json!({"item": sub.0})).unwrap();
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    let r = s.execute("source.overwrite", json!({})).unwrap();
    let c = clip(&s, r["clips"][0].as_u64().unwrap());
    assert_eq!((c.source_in, c.duration), (r24.tick_of(24), r24.tick_of(48)));
    assert_eq!(crate::media_duration(&s.project, &s.media, sub), Some(r24.tick_of(72)), "trims stop at the subclip's end");
    // Edit Subclip: new range, then no restriction
    s.execute("project.select", json!({"items": [sub.0]})).unwrap();
    s.execute("clip.editSubclip", json!({"startFrame": 10, "endFrame": 20, "restrictTrims": false})).unwrap();
    let ItemKind::Subclip { range, restrict_trims, .. } = s.project.item(sub).unwrap().kind.clone() else { panic!() };
    assert_eq!((range.start, range.end(), restrict_trims), (r24.tick_of(10), r24.tick_of(20), false));
    assert_eq!(crate::media_duration(&s.project, &s.media, sub), crate::media_duration(&s.project, &s.media, ocean));
    assert!(s.execute("clip.editSubclip", json!({"startFrame": 20, "endFrame": 10})).is_err());
    s.execute("clip.editSubclip", json!({"convertToMaster": true})).unwrap();
    assert!(s.project.item(sub).unwrap().as_media().is_some(), "now a master clip of the same media");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(matches!(s.project.item(sub).unwrap().kind, ItemKind::Subclip { .. }));
    // from a timeline clip (nothing in the Source monitor)
    s.state.source_item = None;
    s.state.project_selection.clear();
    let tl = v1(&s)[1].clone();
    select(&mut s, &[tl.id.0]);
    let r = s.execute("clip.makeSubclip", json!({"name": "Shot 2", "restrictTrims": false})).unwrap();
    let ItemKind::Subclip { parent, range, .. } = s.project.item(ItemId(r["item"].as_u64().unwrap())).unwrap().kind.clone() else { panic!() };
    assert_eq!((parent, range.start, range.end()), (tl.item, tl.source_in, tl.source_out()));
}

#[test]
fn modify_audio_channels_and_breakout_to_mono() {
    let mut s = demo();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    let r = s.execute("clip.audioChannels", json!({"format": "mono"})).unwrap();
    assert_eq!(r["items"][0]["clips"], json!([[0], [1]]));
    let map = s.project.item(ocean).unwrap().as_media().unwrap().interpret.audio_channels.clone().unwrap();
    assert_eq!(map.format, AudioChannels::Mono);
    // editing it in makes one mono clip per channel on A1 and A2
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 1})).unwrap();
    let r = s.execute("timeline.place", json!({"item": ocean.0, "track": "V3", "audioTrack": "A3", "seconds": 40.0})).unwrap();
    let ids: Vec<u64> = r["clips"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    let q = s.active_sequence().unwrap();
    let a3 = q.audio_tracks[2].items.iter().find(|i| ids.contains(&i.id.0)).unwrap();
    let a4 = q.audio_tracks[3].items.iter().find(|i| ids.contains(&i.id.0)).unwrap();
    assert_eq!((a3.source_channels.clone(), a4.source_channels.clone()), (vec![0], vec![1]));
    assert_eq!(a3.link, a4.link);
    // bad channel lists are refused; custom map with two channels in one clip
    assert!(s.execute("clip.audioChannels", json!({"format": "stereo", "clips": [[0, 5]]})).is_err());
    s.execute("clip.audioChannels", json!({"format": "stereo", "clips": [[1, 0]]})).unwrap();
    assert_eq!(s.project.item(ocean).unwrap().as_media().unwrap().interpret.audio_channels.as_ref().unwrap().clips, vec![vec![1, 0]]);
    s.execute("clip.audioChannels", json!({"format": "stereo"})).unwrap();
    assert!(s.project.item(ocean).unwrap().as_media().unwrap().interpret.audio_channels.is_none(), "plain stereo is the default");
    // timeline clips: pick the channel a clip plays
    s.state.project_selection.clear();
    let a1 = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    s.state.selection = vec![a1];
    s.execute("clip.audioChannels", json!({"channels": [1]})).unwrap();
    assert_eq!(clip(&s, a1.0).source_channels, vec![1]);
    // Breakout to Mono
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    let r = s.execute("clip.breakoutToMono", json!({})).unwrap();
    let made: Vec<ItemId> = r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect();
    assert_eq!(made.len(), 2);
    let names: Vec<&str> = made.iter().map(|i| s.project.item(*i).unwrap().name.as_str()).collect();
    assert_eq!(names, ["Ocean_Sunset.mp4 Left", "Ocean_Sunset.mp4 Right"]);
    let right = s.project.item(made[1]).unwrap().as_media().unwrap().interpret.audio_channels.clone().unwrap();
    assert_eq!(right.clips, vec![vec![1]]);
    s.state.project_selection = vec![item_named(&s, "Bars and Tone")];
    s.project = std::sync::Arc::new({
        let mut p = (*s.project).clone();
        if let Some(m) = p.item_mut(item_named(&s, "Bars and Tone")).and_then(|i| i.as_media_mut()) {
            m.info.audio_streams.clear();
        }
        p
    });
    assert!(s.execute("clip.breakoutToMono", json!({})).is_err(), "no audio");
}

#[test]
fn modify_timecode_and_reset() {
    let mut s = demo();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    let r = s.execute("clip.modifyTimecode", json!({"timecode": "01:00:00:00", "tapeName": "Reel 1"})).unwrap();
    let m = s.project.item(ocean).unwrap();
    let rate = m.as_media().unwrap().frame_rate();
    assert_eq!(r["startTimecode"], 3600 * rate.timecode_base());
    assert_eq!(m.metadata["Tape Name"], "Reel 1");
    s.execute("clip.modifyTimecode", json!({"reset": true})).unwrap();
    assert_eq!(s.project.item(ocean).unwrap().as_media().unwrap().info.start_timecode, None);
    assert!(s.execute("clip.modifyTimecode", json!({})).is_err());
}

#[test]
fn frame_hold_options_add_frame_hold_and_hold_segment() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let c = v1(&s)[1].clone();
    select(&mut s, &[c.id.0]);
    // Hold on In point
    s.execute("clip.frameHoldOptions", json!({"holdOn": "in"})).unwrap();
    assert_eq!(clip(&s, c.id.0).frame_hold, Some(c.source_in));
    // Out point / playhead / source timecode / sequence time
    s.execute("clip.frameHoldOptions", json!({"holdOn": "out", "holdFilters": true})).unwrap();
    let got = clip(&s, c.id.0);
    assert_eq!(got.frame_hold, Some(rate.snap(c.source_out() - Tick(1))));
    assert!(got.hold_filters);
    s.execute("playhead.set", json!({"time": (c.start + rate.tick_of(10)).0})).unwrap();
    s.execute("clip.frameHoldOptions", json!({"holdOn": "playhead"})).unwrap();
    assert_eq!(clip(&s, c.id.0).frame_hold, Some(rate.snap(c.source_in + rate.tick_of(10))));
    s.execute("clip.frameHoldOptions", json!({"holdOn": "sourceTimecode", "timecode": "00:00:03:00"})).unwrap();
    assert_eq!(clip(&s, c.id.0).frame_hold, Some(rate.tick_of(72)));
    s.execute("clip.frameHoldOptions", json!({"holdOn": "sequenceTime", "time": (c.start + rate.tick_of(5)).0})).unwrap();
    assert_eq!(clip(&s, c.id.0).frame_hold, Some(rate.snap(c.source_in + rate.tick_of(5))));
    assert!(s.execute("clip.frameHoldOptions", json!({"holdOn": "nowhere"})).is_err());
    s.execute("clip.frameHoldOptions", json!({"enabled": false})).unwrap();
    assert_eq!(clip(&s, c.id.0).frame_hold, None);
    // Hold Filters: effects animate through a hold unless it is on
    let mut held = clip(&s, c.id.0);
    held.frame_hold = Some(held.source_in);
    let late = held.start + rate.tick_of(20);
    assert_eq!(held.source_time_at(late), held.source_in);
    assert_eq!(held.effect_time_at(late), held.moving_source_time_at(late));
    held.hold_filters = true;
    assert_eq!(held.effect_time_at(late), held.source_in);

    // Add Frame Hold splits at the playhead and holds the rest of the clip
    let n = v1(&s).len();
    let t = c.start + rate.tick_of(12);
    s.execute("playhead.set", json!({"time": t.0})).unwrap();
    select(&mut s, &[c.id.0]);
    let r = s.execute("clip.frameHold", json!({})).unwrap();
    assert_eq!(v1(&s).len(), n + 1);
    let right = clip(&s, r["clips"][0].as_u64().unwrap());
    assert_eq!((right.start, right.frame_hold), (t, Some(c.source_in + rate.tick_of(12))));
    assert_eq!(clip(&s, c.id.0).frame_hold, None, "the left part plays normally");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s).len(), n);
    // Insert Frame Hold Segment: a 2 s hold inserted at the playhead, rippling the sequence
    let dur0 = s.active_sequence().unwrap().duration();
    let r = s.execute("clip.insertFrameHoldSegment", json!({})).unwrap();
    let seg = clip(&s, r["clip"].as_u64().unwrap());
    assert_eq!((seg.start, seg.duration, seg.frame_hold), (t, rate.snap_nearest(Tick(2 * TICKS_PER_SECOND)), Some(c.source_in + rate.tick_of(12))));
    assert_eq!(s.active_sequence().unwrap().duration(), dur0 + seg.duration);
    assert_eq!(v1(&s).len(), n + 2, "split + segment");
    s.active_sequence().unwrap().check().unwrap();
    s.execute("playhead.set", json!({"seconds": 999.0})).unwrap();
    s.state.selection.clear();
    assert!(s.execute("clip.insertFrameHoldSegment", json!({})).is_err(), "no clip under the playhead");
}

#[test]
fn field_options_and_time_interpolation_are_stored() {
    let mut s = demo();
    let c = v1(&s)[0].id.0;
    select(&mut s, &[c]);
    s.execute("clip.fieldOptions", json!({"reverseFieldDominance": true, "processing": "flickerRemoval"})).unwrap();
    let fo = clip(&s, c).field_options.unwrap();
    assert!(fo.reverse_field_dominance && fo.processing == FieldProcessing::FlickerRemoval);
    assert!(s.execute("clip.fieldOptions", json!({"processing": "wobble"})).is_err());
    s.execute("clip.timeInterpolation.frameBlending", json!({})).unwrap();
    assert_eq!(clip(&s, c).time_interpolation, TimeInterpolation::FrameBlending);
    s.execute("clip.setTimeInterpolation", json!({"mode": "opticalFlow"})).unwrap();
    assert_eq!(clip(&s, c).time_interpolation, TimeInterpolation::OpticalFlow);
    s.execute("clip.speedDuration", json!({"speed": 50.0, "interpolation": "frameSampling"})).unwrap();
    assert_eq!(clip(&s, c).time_interpolation, TimeInterpolation::FrameSampling);
    // survives a save / load
    let bytes = filmcraft_format::encode(&s.project, true);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    let seq = s.state.active_sequence.unwrap();
    assert_eq!(back.sequence(seq).unwrap().find_item(filmcraft_project::ClipId(c)).unwrap().1, &clip(&s, c));
}

fn mean_abs_diff(a: &filmcraft_render::Image, b: &filmcraft_render::Image) -> f32 {
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.px.len() as f32
}

#[test]
fn frame_blending_renders_in_between_frames() {
    let mut s = demo();
    let c = v1(&s)[2].clone();
    select(&mut s, &[c.id.0]);
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    select(&mut s, &[c.id.0]);
    s.execute("clip.speedDuration", json!({"speed": 50.0})).unwrap();
    let rate = s.sequence_rate();
    // an odd frame at 50 % lands half-way between two source frames
    let t = c.start + rate.tick_of(31);
    s.execute("playhead.set", json!({"time": t.0})).unwrap();
    let it = clip(&s, c.id.0);
    let src_rate = filmcraft_time::FrameRate::FPS_23_976;
    assert!(filmcraft_render::interpolation_blend(&it, t, src_rate).is_none(), "frame sampling");
    let sampled = s.render_program(0.125).unwrap();
    s.execute("clip.timeInterpolation.frameBlending", json!({})).unwrap();
    let it = clip(&s, c.id.0);
    let (next, w) = filmcraft_render::interpolation_blend(&it, t, src_rate).unwrap();
    assert!((w - 0.5).abs() < 0.01, "half-way: {w}");
    assert_eq!(next, src_rate.tick_of(src_rate.frame_at(it.source_time_at(t)) + 1));
    let blended = s.render_program(0.125).unwrap();
    assert!(mean_abs_diff(&sampled, &blended) > 1e-4, "blending changes the frame");
    // the blend is the average of the two source frames: the next timeline frame samples the later one
    s.execute("clip.timeInterpolation.frameSampling", json!({})).unwrap();
    s.execute("playhead.set", json!({"time": (t + rate.frame_duration()).0})).unwrap();
    let later = s.render_program(0.125).unwrap();
    let mut avg = sampled.clone();
    for (a, b) in avg.px.iter_mut().zip(&later.px) {
        *a = (*a + b) * 0.5;
    }
    assert!(mean_abs_diff(&avg, &blended) < mean_abs_diff(&sampled, &blended), "closer to the average than to either frame");
    // optical flow falls back to blending
    s.execute("clip.timeInterpolation.opticalFlow", json!({})).unwrap();
    s.execute("playhead.set", json!({"time": t.0})).unwrap();
    assert!(mean_abs_diff(&s.render_program(0.125).unwrap(), &blended) < 1e-6);
    // at 100 % nothing is blended
    s.execute("clip.speedDuration", json!({"speed": 100.0})).unwrap();
    assert!(filmcraft_render::interpolation_blend(&clip(&s, c.id.0), t, src_rate).is_none());
}

#[test]
fn fit_and_fill_frame() {
    let mut s = demo();
    let r = s.execute("file.newColorMatte", json!({"width": 960, "height": 1080, "color": "#ff0000"})).unwrap();
    let matte = r["item"].as_u64().unwrap();
    let r = s.execute("timeline.place", json!({"item": matte, "track": "V3", "seconds": 0.0})).unwrap();
    let c = r["clips"][0].as_u64().unwrap();
    select(&mut s, &[c]);
    s.execute("clip.fillFrame", json!({})).unwrap();
    assert!((f64_param(&clip(&s, c), "motion", "scale") - 200.0).abs() < 1e-9);
    s.execute("clip.fitToFrame", json!({})).unwrap();
    let got = clip(&s, c);
    assert!((f64_param(&got, "motion", "scale") - 100.0).abs() < 1e-9);
    assert_eq!(got.effect("motion").unwrap().param("anchor").unwrap().value.as_vec2().unwrap(), filmcraft_geom::Vec2::new(480.0, 540.0));
    s.execute("edit.undo", json!({})).unwrap();
    assert!((f64_param(&clip(&s, c), "motion", "scale") - 200.0).abs() < 1e-9);
    s.state.selection.clear();
    assert!(s.execute("clip.fitToFrame", json!({})).is_err());
}

#[test]
fn extract_audio_writes_a_wav_and_imports_it() {
    let mut s = demo();
    let dir = tmp_dir("extract-audio");
    let forest = item_named(&s, "Misty_Forest.mp4");
    s.execute("project.select", json!({"items": [forest.0]})).unwrap();
    // unsaved project, generator media: there is nowhere to put the file
    assert!(s.execute("clip.extractAudio", json!({})).is_err());
    s.execute("file.saveAs", json!({"path": dir.join("x.fcproj").to_string_lossy()})).unwrap();
    let n0 = s.history.undo.len();
    let r = s.execute("clip.extractAudio", json!({})).unwrap();
    let e = &r["extracted"][0];
    let path = e["path"].as_str().unwrap();
    assert!(path.ends_with("Misty_Forest Audio Extracted.wav"), "{path}");
    assert!(std::path::Path::new(path).exists());
    let item = s.project.item(ItemId(e["item"].as_u64().unwrap())).unwrap();
    let m = item.as_media().unwrap();
    assert!(m.info.video.is_none());
    assert_eq!(m.info.audio().unwrap().channels, 2);
    let src_dur = s.project.item(forest).unwrap().duration();
    assert!((m.info.duration - src_dur).0.abs() < TICKS_PER_SECOND / 100, "same length");
    assert_eq!(s.project.root.parent_of(item.id), s.project.root.parent_of(forest));
    assert_eq!(s.history.undo.len(), n0 + 1);
    // again: a new name, not an overwrite
    let r = s.execute("clip.extractAudio", json!({"items": [forest.0]})).unwrap();
    assert!(r["extracted"][0]["path"].as_str().unwrap().ends_with("Audio Extracted 2.wav"));
}

#[test]
fn replace_with_clip_from_source_match_frame_and_bin() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let c = v1(&s)[3].clone();
    let forest = item_named(&s, "Misty_Forest.mp4");
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    select(&mut s, &[c.id.0]);
    assert!(s.execute("clip.replaceFromSource", json!({})).is_err(), "nothing in the Source monitor");
    s.execute("source.open", json!({"item": forest.0})).unwrap();
    s.execute("project.setMarks", json!({"item": forest.0, "in": rate.tick_of(10).0})).unwrap();
    s.execute("clip.replaceFromSource", json!({})).unwrap();
    let got = clip(&s, c.id.0);
    assert_eq!((got.item, got.name.as_str(), got.source_in, got.duration), (forest, "Misty_Forest.mp4", rate.tick_of(10), c.duration));
    assert_eq!(got.effects, c.effects, "effects stay with the timeline clip");
    s.execute("edit.undo", json!({})).unwrap();
    // Match Frame: the Source monitor frame lands on the sequence playhead
    s.execute("playhead.set", json!({"time": (c.start + rate.tick_of(6)).0})).unwrap();
    s.execute("source.setPlayhead", json!({"frame": 50})).unwrap();
    s.execute("clip.replaceFromSourceMatchFrame", json!({})).unwrap();
    let got = clip(&s, c.id.0);
    assert_eq!(got.source_in, rate.tick_of(44));
    assert_eq!(got.source_time_at(c.start + rate.tick_of(6)), rate.tick_of(50));
    s.execute("source.setPlayhead", json!({"frame": 2})).unwrap();
    assert!(s.execute("clip.replaceFromSourceMatchFrame", json!({})).is_err(), "not enough media before the frame");
    // From Bin: the Project panel selection
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    s.execute("project.select", json!({"items": [dunes.0]})).unwrap();
    s.execute("clip.replaceFromBin", json!({})).unwrap();
    assert_eq!((clip(&s, c.id.0).item, clip(&s, c.id.0).source_in), (dunes, Tick::ZERO));
    // an audio-only item can't replace a video clip
    let music = item_named(&s, "Ambient_Score.wav");
    assert!(s.execute("clip.replaceFromBin", json!({"item": music.0})).is_err());
}

/// Replace With Clip ▸ From Bin restarts the clip at the new item's In point (as Premiere does);
/// `keepSourceIn` keeps the clip's place in the media instead.
#[test]
fn replace_from_bin_keeps_the_source_in_on_request() {
    let mut s = demo();
    let rate = s.sequence_rate();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let c = v1(&s)[3].clone();
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    let forest = item_named(&s, "Misty_Forest.mp4");
    // disabled without a clip and a replacement, with or without the new parameter
    s.execute("timeline.select", json!({"clips": []})).unwrap();
    assert!(matches!(s.execute("clip.replaceFromBin", json!({"keepSourceIn": true})), Err(crate::EngineError::Disabled(..))));
    // give the clip a source In that is not the start of its media
    s.execute("timeline.trim", json!({"clip": c.id.0, "edge": "in", "mode": "regular", "deltaFrames": 7})).unwrap();
    let trimmed = clip(&s, c.id.0);
    assert_eq!(trimmed.source_in, c.source_in + rate.tick_of(7));
    select(&mut s, &[c.id.0]);
    s.execute("project.select", json!({"items": [dunes.0]})).unwrap();
    let before = s.project.clone();
    // default: the replacement starts at the item's In point, and the result is what it was
    let r = s.execute("clip.replaceFromBin", json!({})).unwrap();
    assert_eq!((clip(&s, c.id.0).item, clip(&s, c.id.0).source_in), (dunes, Tick::ZERO));
    assert_eq!(r, json!({"clips": [c.id.0], "item": dunes.0, "short": []}));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    // keepSourceIn: the same place in the new media, everything else as before
    let r = s.execute("clip.replaceFromBin", json!({"keepSourceIn": true})).unwrap();
    let got = clip(&s, c.id.0);
    assert_eq!((got.item, got.source_in, got.start, got.duration), (dunes, trimmed.source_in, trimmed.start, trimmed.duration));
    assert_eq!(r, json!({"clips": [c.id.0], "item": dunes.0, "short": [], "sourceInClamped": []}));
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("clip.replaceFromBin", json!({"keepSourceIn": false})).unwrap();
    assert_eq!(clip(&s, c.id.0).source_in, Tick::ZERO);
    s.execute("edit.undo", json!({})).unwrap();

    // a subclip that restricts trims has no media outside its range: a kept In before or after it
    // moves to its first or last frame, and the result names the clip
    let sub = |s: &mut Session, from: Tick, to: Tick, restrict: bool| {
        let r = s.execute("clip.makeSubclip", json!({"item": forest.0, "start": from.0, "end": to.0, "restrictTrims": restrict})).unwrap();
        ItemId(r["item"].as_u64().unwrap())
    };
    let kept = trimmed.source_in;
    let media_frame = s.project.item(forest).unwrap().as_media().unwrap().frame_rate().frame_duration();
    assert!(kept > rate.tick_of(10) && media_frame > Tick::ZERO);
    let later = sub(&mut s, kept + rate.tick_of(3), kept + rate.tick_of(15), true);
    let earlier = sub(&mut s, Tick::ZERO, kept - rate.tick_of(2), true);
    let loose = sub(&mut s, kept + rate.tick_of(3), kept + rate.tick_of(15), false);
    for (item, want, clamped) in [(later, kept + rate.tick_of(3), true), (earlier, kept - rate.tick_of(2) - media_frame, true), (loose, kept, false)] {
        let r = s.execute("clip.replaceFromBin", json!({"item": item.0, "keepSourceIn": true})).unwrap();
        let got = clip(&s, c.id.0);
        assert_eq!((got.item, got.source_in, got.start, got.duration), (item, want, trimmed.start, trimmed.duration));
        assert_eq!(r["sourceInClamped"], if clamped { json!([c.id.0]) } else { json!([]) });
        s.execute("edit.undo", json!({})).unwrap();
    }
    // a kept In that is already inside the subclip stays where it is
    let around = sub(&mut s, kept - rate.tick_of(5), kept + rate.tick_of(23), true);
    let r = s.execute("clip.replaceFromBin", json!({"item": around.0, "keepSourceIn": true})).unwrap();
    assert_eq!((clip(&s, c.id.0).source_in, &r["sourceInClamped"]), (kept, &json!([])));
    // hostile values of the flag fall back to the default
    s.execute("edit.undo", json!({})).unwrap();
    for junk in [json!("yes"), json!(1), json!(null), json!([true])] {
        let r = s.execute("clip.replaceFromBin", json!({"item": dunes.0, "keepSourceIn": junk})).unwrap();
        assert_eq!((clip(&s, c.id.0).source_in, r.get("sourceInClamped")), (Tick::ZERO, None));
        s.execute("edit.undo", json!({})).unwrap();
    }
}

/// A replacement shorter than the clip's slot left the clip running past the end of its media
/// without a word. The three Replace With Clip commands now say which clips are short, by how
/// many sequence frames.
#[test]
fn replace_with_clip_reports_clips_that_run_past_the_new_media() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let demo_seq = s.state.active_sequence.unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let c = v1(&s)[3].clone();
    let slot = rate.frame_at(c.duration);
    let forest = item_named(&s, "Misty_Forest.mp4");
    let forest_len = s.project.item(forest).unwrap().as_media().unwrap().info.duration;
    assert!(slot > 12 && forest_len > c.duration + rate.tick_of(40), "the clip is longer than 12 frames and shorter than the media");
    let state = |s: &Session| {
        let it = clip(s, c.id.0);
        (it.item, it.source_in, it.start, it.duration)
    };
    select(&mut s, &[c.id.0]);

    // From Bin, with 12 frames of media (a subclip that restricts trims): the edit is what it
    // always was, and the result says the clip is short
    let r = s.execute("clip.makeSubclip", json!({"item": forest.0, "start": rate.tick_of(10).0, "end": rate.tick_of(22).0})).unwrap();
    let short = ItemId(r["item"].as_u64().unwrap());
    let before = s.project.clone();
    let r = s.execute("clip.replaceFromBin", json!({"item": short.0})).unwrap();
    assert_eq!(state(&s), (short, rate.tick_of(10), c.start, c.duration), "the edit itself is not changed");
    assert_eq!(r["short"], json!([{"clip": c.id.0, "shortByFrames": slot - 12}]));
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
    s.execute("edit.undo", json!({})).unwrap();
    // long enough media: nothing is short
    assert_eq!(s.execute("clip.replaceFromBin", json!({"item": forest.0})).unwrap()["short"], json!([]));
    s.execute("edit.undo", json!({})).unwrap();

    // From Source Monitor, with the Source In 20 frames before the end of the media
    let frames = |t: Tick| (t.0 + rate.frame_duration().0 - 1) / rate.frame_duration().0;
    s.execute("source.open", json!({"item": forest.0})).unwrap();
    let mark = forest_len - rate.tick_of(20);
    s.execute("project.setMarks", json!({"item": forest.0, "in": mark.0})).unwrap();
    let r = s.execute("clip.replaceFromSource", json!({})).unwrap();
    let got = clip(&s, c.id.0);
    assert_eq!(r["short"], json!([{"clip": c.id.0, "shortByFrames": frames(got.source_out() - forest_len)}]));
    assert_eq!(frames(got.source_out() - forest_len), slot - 20);
    s.execute("edit.undo", json!({})).unwrap();
    // Match Frame: the last frame of the media on the clip's first frame
    s.execute("playhead.set", json!({"time": c.start.0})).unwrap();
    s.execute("source.setPlayhead", json!({"time": (forest_len - rate.tick_of(1)).0})).unwrap();
    let r = s.execute("clip.replaceFromSourceMatchFrame", json!({})).unwrap();
    assert_eq!(r["short"], json!([{"clip": c.id.0, "shortByFrames": slot - 1}]));
    s.execute("edit.undo", json!({})).unwrap();
    // with enough media after the frame, nothing is short
    s.execute("source.setPlayhead", json!({"frame": 5})).unwrap();
    assert_eq!(s.execute("clip.replaceFromSourceMatchFrame", json!({})).unwrap()["short"], json!([]));
    s.execute("edit.undo", json!({})).unwrap();

    // never more than the clip itself: an In that lies past the end of the new media altogether
    s.execute("timeline.trim", json!({"clip": c.id.0, "edge": "in", "mode": "regular", "deltaFrames": 7})).unwrap();
    let trimmed = clip(&s, c.id.0);
    s.execute("file.newSequence", json!({"name": "Brief"})).unwrap();
    let brief = s.state.active_sequence.unwrap();
    s.execute("timeline.place", json!({"item": forest.0, "time": 0})).unwrap();
    let placed = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.execute("timeline.trim", json!({"clip": placed.id.0, "edge": "out", "mode": "regular", "delta": (rate.tick_of(3) - placed.duration).0})).unwrap();
    s.execute("sequence.open", json!({"item": demo_seq.0})).unwrap();
    select(&mut s, &[c.id.0]);
    assert!(trimmed.source_in > s.project.sequence(brief).unwrap().duration(), "the kept In is past the end of the nested sequence");
    let r = s.execute("clip.replaceFromBin", json!({"clips": [c.id.0], "item": brief.0, "keepSourceIn": true})).unwrap();
    assert_eq!(clip(&s, c.id.0).source_in, trimmed.source_in);
    assert_eq!(r["short"], json!([{"clip": c.id.0, "shortByFrames": rate.frame_at(trimmed.duration)}]), "all of the clip, not more");
}

#[test]
fn audio_source_channels_pick_what_a_clip_plays() {
    let mut it = v1(&demo())[0].clone();
    assert_eq!(filmcraft_render::audio::source_pair(&it, 2), (0, 1));
    assert_eq!(filmcraft_render::audio::source_pair(&it, 1), (0, 0));
    it.source_channels = vec![1];
    assert_eq!(filmcraft_render::audio::source_pair(&it, 2), (1, 1));
    it.source_channels = vec![1, 0];
    assert_eq!(filmcraft_render::audio::source_pair(&it, 2), (1, 0));
    it.source_channels = vec![5];
    assert_eq!(filmcraft_render::audio::source_pair(&it, 2), (0, 0), "missing channels fall back to the first");
}

#[test]
fn subclip_source_monitor_trims_and_markers() {
    let mut s = demo();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    let r24 = filmcraft_time::FrameRate::FPS_23_976;
    let f = |n: i64| r24.tick_of(n);
    // clip markers on the master clip at frames 10, 30, 60 and 90
    let mut p = (*s.project).clone();
    if let Some(ItemKind::Media(m)) = p.item_mut(ocean).map(|i| &mut i.kind) {
        for (k, n) in [10, 30, 60, 90].into_iter().enumerate() {
            m.markers.push(filmcraft_project::Marker {
                id: filmcraft_project::MarkerId(9000 + k as u64),
                start: f(n),
                duration: Tick::ZERO,
                name: format!("m{n}"),
                comment: String::new(),
                kind: filmcraft_project::MarkerKind::Comment,
                color: Label::Green,
            });
        }
    }
    s.project = std::sync::Arc::new(p);
    s.execute("project.setMarks", json!({"item": ocean.0, "in": f(24).0, "out": f(71).0})).unwrap();
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    s.state.source_item = None;
    let sub = ItemId(s.execute("clip.makeSubclip", json!({})).unwrap()["item"].as_u64().unwrap());
    // the Source monitor shows the subclip's range, opens at its In and keeps the playhead inside
    s.execute("source.open", json!({"item": sub.0})).unwrap();
    let v = s.execute("source.inspect", json!({})).unwrap();
    assert_eq!((v["start"].as_i64(), v["end"].as_i64(), v["playhead"].as_i64()), (Some(f(24).0), Some(f(72).0), Some(f(24).0)));
    assert_eq!(v["media"], ocean.0);
    // inherited markers: the master clip's markers inside the range
    let names: Vec<&str> = v["markers"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["m30", "m60"]);
    assert_eq!(s.execute("source.setPlayhead", json!({"frame": 0})).unwrap()["time"], f(24).0);
    assert_eq!(s.execute("source.setPlayhead", json!({"frame": 500})).unwrap()["time"], f(71).0);
    // edited in on V3 at 100 s: In 24, 48 frames
    let r = s.execute("timeline.place", json!({"item": sub.0, "track": "V3", "seconds": 100.0})).unwrap();
    let c = r["clips"][0].as_u64().unwrap();
    let start = clip(&s, c).start;
    assert_eq!((clip(&s, c).source_in, clip(&s, c).duration), (f(24), f(48)));
    // restricted trims: no handles beyond the subclip on either side
    let trim = |s: &mut Session, edge: &str, d: i64| {
        s.execute("timeline.trim", json!({"clip": c, "edge": edge, "deltaFrames": d})).unwrap();
        let it = clip(s, c);
        (r24.frame_at(it.start - start), r24.frame_at(it.source_in), r24.frame_at(it.duration))
    };
    assert_eq!(trim(&mut s, "in", -10), (0, 24, 48), "head stops at the subclip's In");
    assert_eq!(trim(&mut s, "out", 10), (0, 24, 48), "tail stops at the subclip's Out");
    assert_eq!(trim(&mut s, "in", 6), (6, 30, 42));
    assert_eq!(trim(&mut s, "in", -10), (0, 24, 48), "only back to the subclip's In");
    // slip stays inside the subclip too
    trim(&mut s, "out", -8);
    s.execute("timeline.slip", json!({"clip": c, "deltaFrames": -20})).unwrap();
    assert_eq!(r24.frame_at(clip(&s, c).source_in), 24);
    s.execute("timeline.slip", json!({"clip": c, "deltaFrames": 20})).unwrap();
    assert_eq!(r24.frame_at(clip(&s, c).source_in), 32, "In + 40 frames used = the subclip's Out");
    // without the restriction the master clip's media is the limit (and the monitor shows it all,
    // with the subclip's range as In / Out)
    s.execute("project.select", json!({"items": [sub.0]})).unwrap();
    s.execute("clip.editSubclip", json!({"restrictTrims": false})).unwrap();
    s.execute("timeline.slip", json!({"clip": c, "deltaFrames": -20})).unwrap();
    assert_eq!(r24.frame_at(clip(&s, c).source_in), 12);
    let v = s.execute("source.inspect", json!({})).unwrap();
    assert_eq!((v["start"].as_i64(), v["markIn"].as_i64(), v["markOut"].as_i64()), (Some(0), Some(f(24).0), Some(f(71).0)));
    assert_eq!(v["subclip"]["restrictTrims"], false);
    // Convert to Master Clip: the whole media, marked with the range, keeping the inherited markers
    s.execute("clip.editSubclip", json!({"convertToMaster": true})).unwrap();
    let m = s.project.item(sub).unwrap().as_media().unwrap().clone();
    assert_eq!((m.mark_in, m.mark_out), (Some(f(24)), Some(f(71))));
    assert_eq!(m.markers.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["m30", "m60"]);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(matches!(s.project.item(sub).unwrap().kind, ItemKind::Subclip { restrict_trims: false, .. }));
}

/// #29: a Color Matte's color can be changed after it is created; the generated frames follow,
/// and undo brings the old color back (the media pool regenerates the matte, it doesn't keep
/// serving the first one).
#[test]
fn color_matte_color_changes_and_undoes() {
    let mut s = demo();
    let matte = s.execute("file.newColorMatte", json!({"width": 64, "height": 36, "color": "#ff0000"})).unwrap()["item"].as_u64().unwrap();
    let px = |s: &Session| {
        let f = s.source(ItemId(matte)).unwrap().video_frame(filmcraft_media::FrameRequest::full(Tick::ZERO)).unwrap();
        f.to_rgba8()[..3].to_vec()
    };
    assert_eq!(px(&s), [255, 0, 0]);
    let r = s.execute("project.matteColor", json!({"item": matte, "color": "#0000ff"})).unwrap();
    assert_eq!(r["color"], "#0000ff");
    assert_eq!(px(&s), [0, 0, 255]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(px(&s), [255, 0, 0]);
    // the selected matte when no item is named; anything else is refused
    s.state.project_selection = vec![ItemId(matte)];
    s.execute("project.matteColor", json!({"color": "#00ff00"})).unwrap();
    assert_eq!(px(&s), [0, 255, 0]);
    let bars = s.execute("file.newBarsAndTone", json!({})).unwrap()["item"].as_u64().unwrap();
    assert!(s.execute("project.matteColor", json!({"item": bars, "color": "#00ff00"})).is_err());
    assert!(s.execute("project.matteColor", json!({"item": matte, "color": "green"})).is_err());
}

/// A damaged project whose subclip is its own parent (#66): `media_duration` followed the chain
/// without a bound and overflowed the stack. It gives up after a few hops now.
#[test]
fn a_cyclic_subclip_chain_has_no_media_duration() {
    let mut s = demo();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("source.open", json!({"item": ocean.0})).unwrap();
    let sub = ItemId(s.execute("clip.makeSubclip", json!({})).unwrap()["item"].as_u64().unwrap());
    s.execute("clip.editSubclip", json!({"item": sub.0, "restrictTrims": false})).unwrap();
    let Some(ItemKind::Subclip { parent, .. }) = std::sync::Arc::make_mut(&mut s.project).item_mut(sub).map(|i| &mut i.kind) else { panic!() };
    *parent = sub;
    assert_eq!(crate::media_duration(&s.project, &s.media, sub), None);
}

/// #164: Add Edit with a clip selected cut every targeted track. Like Premiere it now cuts only
/// the selected clips under the playhead; with nothing selected there, the targeted tracks.
#[test]
fn add_edit_cuts_only_the_selected_clips() {
    let mut s = demo();
    let count = |s: &Session| {
        let q = s.active_sequence().unwrap();
        (q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>(), q.audio_tracks.iter().map(|t| t.items.len()).sum::<usize>())
    };
    // a V1 clip and a time inside it
    let first = v1(&s)[1].clone();
    let t = first.start + filmcraft_time::Tick(first.duration.0 / 2);
    let (v0, a0) = count(&s);
    s.state.selection = vec![first.id];
    s.execute("sequence.addEdit", json!({"time": t.0})).unwrap();
    let (v1n, a1n) = count(&s);
    assert_eq!((v1n, a1n), (v0 + 1, a0), "only the selected V1 clip is cut");
    // nothing selected under the playhead: every targeted track is cut
    s.execute("edit.undo", json!({})).unwrap();
    s.state.selection.clear();
    let r = s.execute("sequence.addEdit", json!({"time": t.0})).unwrap();
    assert!(r["cuts"].as_u64().unwrap() > 1, "{r}");
}

#[test]
fn linked_slip_keeps_picture_and_sound_aligned_at_unequal_media_limits() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Slip"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let r = s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": true})).unwrap();
    let item = r["item"].as_u64().unwrap();
    let fr = s.sequence_rate().tick_of(1);
    let r =
        s.execute("timeline.place", json!({"item": item, "track": "V1", "audioTrack": "A1", "time": 0, "sourceIn": fr.0 * 20, "duration": fr.0 * 60})).unwrap();
    let (v, a) = (r["clips"][0].as_u64().unwrap(), r["clips"][1].as_u64().unwrap());
    // lengthen only the picture so the pair has unequal tail handles
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": v, "edge": "out", "deltaFrames": 20})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    s.execute("timeline.slip", json!({"clip": v, "deltaFrames": 30})).unwrap();
    assert_eq!(clip(&s, v).source_in, Tick(fr.0 * 20));
    assert_eq!(clip(&s, a).source_in, Tick(fr.0 * 20), "sound must not slip alone");
    s.execute("timeline.slip", json!({"clip": v, "deltaFrames": -10})).unwrap();
    assert_eq!((clip(&s, v).source_in, clip(&s, a).source_in), (Tick(fr.0 * 10), Tick(fr.0 * 10)));
}

#[test]
fn reset_keeps_keyframes_and_resets_one_parameter() {
    // #284: Reset Effect used to replace the effect with a fresh one, deleting every keyframe
    let mut s = demo();
    let a = v1(&s)[0].clone();
    let sec = Tick(TICKS_PER_SECOND);
    let (t0, t1, mid) = (a.start, a.start + sec, a.start + Tick(TICKS_PER_SECOND / 2));
    for (t, v) in [(t0, 150.0), (t1, 50.0)] {
        s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "scale", "value": v, "keyframe": true, "time": t.0})).unwrap();
    }
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation", "value": 30.0})).unwrap();
    let idx = clip(&s, a.id.0).effects.iter().position(|e| e.effect == "motion").unwrap();
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    s.execute("effects.reset", json!({"clip": a.id.0, "index": idx})).unwrap();
    let it = clip(&s, a.id.0);
    let scale = it.effect("motion").unwrap().param("scale").unwrap();
    // (the playhead snaps to a frame)
    let mid_media = it.source_time_at(s.playhead());
    let kf: Vec<(Tick, f64)> = scale.keyframes.iter().map(|k| (k.time, k.value.as_f64().unwrap())).collect();
    assert_eq!(kf, vec![(it.source_time_at(t0), 150.0), (mid_media, 100.0), (it.source_time_at(t1), 50.0)], "keyframes kept, default at the playhead");
    assert_eq!(f64_param(&it, "motion", "rotation"), 0.0);
    // one parameter: only it goes back to its default
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation", "value": 45.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": a.id.0, "effect": "opacity", "param": "opacity", "value": 40.0})).unwrap();
    s.execute("effects.resetParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation"})).unwrap();
    let it = clip(&s, a.id.0);
    assert_eq!(f64_param(&it, "motion", "rotation"), 0.0);
    assert_eq!(f64_param(&it, "opacity", "opacity"), 40.0);
    assert_eq!(it.effect("motion").unwrap().param("scale").unwrap().keyframes.len(), 3);
    assert!(s.execute("effects.resetParam", json!({"clip": a.id.0, "effect": "motion", "param": "nope"})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(f64_param(&clip(&s, a.id.0), "motion", "rotation"), 45.0);
}

#[test]
fn keyboard_slip_keeps_linked_clips_in_sync_past_the_shorter_handle() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Slip"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let r = s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": true})).unwrap();
    let item = r["item"].as_u64().unwrap();
    let fr = s.sequence_rate().tick_of(1);
    let r =
        s.execute("timeline.place", json!({"item": item, "track": "V1", "audioTrack": "A1", "time": 0, "sourceIn": fr.0 * 20, "duration": fr.0 * 60})).unwrap();
    let (v, a) = (r["clips"][0].as_u64().unwrap(), r["clips"][1].as_u64().unwrap());
    // lengthen only the picture: it keeps 10 frames of tail handle, the sound keeps 20
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": v, "edge": "out", "deltaFrames": 10})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    s.execute("timeline.select", json!({"clips": [v, a]})).unwrap();
    for _ in 0..3 {
        s.execute("timeline.slipLeft5", json!({})).unwrap();
    }
    assert_eq!((clip(&s, v).source_in, clip(&s, a).source_in), (Tick(fr.0 * 30), Tick(fr.0 * 30)));
    // with linked selection off each selected clip still slips on its own
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [v, a]})).unwrap();
    s.execute("timeline.slipLeft5", json!({})).unwrap();
    assert_eq!((clip(&s, v).source_in, clip(&s, a).source_in), (Tick(fr.0 * 30), Tick(fr.0 * 35)));
}
