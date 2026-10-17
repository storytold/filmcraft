use filmcraft_engine::Session;
use filmcraft_project::{HexColor, Label};
use serde_json::json;

#[test]
fn custom_marker_properties_survive_undo_and_project_reopen() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Review"})).unwrap();
    let id = s.execute("markers.add", json!({"frame":0,"name":"Note","author":"BENJI","color":"#ff80c0"})).unwrap()["marker"].as_u64().unwrap();
    let seq = s.state.active_sequence.unwrap();
    let original = s.active_sequence().unwrap().markers[0].clone();
    assert_eq!(original.rgb(), [255, 128, 192]);
    assert_eq!(original.color_value(), "#FF80C0");
    assert_eq!(original.author, "BENJI");
    let loaded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
    assert_eq!(loaded.project.sequence(seq).unwrap().markers[0], original);
    s.execute("markers.edit", json!({"marker":id,"color":"White"})).unwrap_err();
    s.execute("markers.edit", json!({"marker":id,"comment":"Keep"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[0].custom_color, original.custom_color);
    s.execute("markers.edit", json!({"marker":id,"color":"Blue","author":""})).unwrap();
    let marker = &s.active_sequence().unwrap().markers[0];
    assert_eq!(marker.color, Label::Blue);
    assert_eq!(marker.custom_color, None);
    assert_eq!(marker.author, "");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[0].custom_color, HexColor::parse("#FF80C0"));
    assert_eq!(s.active_sequence().unwrap().markers[0].author, "BENJI");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[0].custom_color, None);
    let inspected = s.execute("sequence.inspect", json!({})).unwrap();
    assert_eq!(inspected["markers"][0]["author"], "");
    assert_eq!(inspected["markers"][0]["color"], "Blue");
}

#[test]
fn invalid_marker_properties_leave_project_and_undo_unchanged() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({})).unwrap();
    let id = s.execute("markers.add", json!({})).unwrap()["marker"].as_u64().unwrap();
    for change in [
        json!({"color":"#123"}),
        json!({"color":"#00000000"}),
        json!({"color":"#€aaaa"}),
        json!({"color":[0,0,0]}),
        json!({"author":true}),
        json!({"author":"a\nb"}),
        json!({"author":"a".repeat(1025)}),
    ] {
        let before = s.project.to_json();
        let history = s.history.undo.len();
        let mut params = change;
        params["marker"] = json!(id);
        assert!(s.execute("markers.edit", params).is_err());
        assert_eq!(s.project.to_json(), before);
        assert_eq!(s.history.undo.len(), history);
    }
}

#[test]
fn all_marker_creation_commands_accept_hex_and_author() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({})).unwrap();
    for cmd in ["markers.add", "markers.addRange", "markers.addChapter", "markers.addFlashCue"] {
        s.execute(cmd, json!({"name":"Cue", "durationFrames":10,"color":"#FFFFFF","author":"Editor"})).unwrap();
        let marker = s.active_sequence().unwrap().markers.last().unwrap();
        assert_eq!(marker.rgb(), [255; 3], "{cmd}");
        assert_eq!(marker.author, "Editor", "{cmd}");
    }
}

#[test]
fn custom_palette_is_project_local_bounded_and_undoable() {
    let mut s = Session::default();
    let before = s.project.to_json();
    for invalid in [json!("Rose"), json!("#123"), json!(true), json!(42)] {
        assert!(s.execute("colors.addCustom", json!({"color":invalid})).is_err());
        assert_eq!(s.project.to_json(), before);
    }
    s.execute("colors.addCustom", json!({"color":"#ff80c0"})).unwrap();
    let history = s.history.undo.len();
    s.execute("colors.addCustom", json!({"color":"#FF80C0"})).unwrap();
    assert_eq!(s.history.undo.len(), history, "duplicates do not create edits");
    let loaded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
    assert_eq!(loaded.project.custom_colors, s.project.custom_colors);
    assert!(Session::default().project.custom_colors.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
    s.execute("edit.redo", json!({})).unwrap();
    for value in 0..63 {
        s.execute("colors.addCustom", json!({"color":format!("#{value:06X}")})).unwrap();
    }
    assert_eq!(s.project.custom_colors.len(), 64);
    let full = s.project.to_json();
    assert!(s.execute("colors.addCustom", json!({"color":"#FFFFFF"})).is_err());
    assert_eq!(s.project.to_json(), full);
}

#[test]
fn custom_clip_colors_share_palette_and_survive_save_preset_switch_and_undo() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let before = s.project.to_json();
    let history = s.history.undo.len();
    s.execute("edit.label", json!({"clips":[clip.id.0],"label":"#12abcd","saveColor":true})).unwrap();
    let color = HexColor::parse("#12ABCD");
    assert_eq!(s.active_sequence().unwrap().find_item(clip.id).unwrap().1.custom_label_color, color);
    assert_eq!(s.project.custom_colors, vec![color.unwrap()]);
    s.execute("sequence.linkedSelection", json!({"on":false})).unwrap();
    s.execute("timeline.select", json!({"clips":[clip.id.0]})).unwrap();
    s.execute("edit.selectLabelGroup", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![clip.id]);
    assert_eq!(s.history.undo.len(), history + 1);
    let loaded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
    assert_eq!(loaded.project.sequence(seq).unwrap().find_item(clip.id).unwrap().1.custom_label_color, color);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("edit.label.violet", json!({"clips":[clip.id.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(clip.id).unwrap().1.custom_label_color, None);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(clip.id).unwrap().1.custom_label_color, color);
    s.execute("edit.label", json!({"items":[clip.item.0],"label":"#12ABCD"})).unwrap();
    let mut project = (*s.project).clone();
    let created = project
        .make_track_item(
            clip.item,
            filmcraft_project::TrackKind::Video,
            clip.start,
            filmcraft_time::TimeRange::new(clip.source_in, clip.duration),
            s.sequence_rate(),
        )
        .unwrap();
    assert_eq!(created.custom_label_color, color);
    s.execute("project.select", json!({"items":[clip.item.0]})).unwrap();
    let copy = s.execute("edit.duplicate", json!({})).unwrap()["items"][0].as_u64().unwrap();
    assert_eq!(s.project.item(filmcraft_project::ItemId(copy)).unwrap().custom_label_color, color);
    let before = s.project.to_json();
    for params in [json!({"label":"#XYZXYZ"}), json!({"label":"#FFFFFF","clips":[-1]}), json!({"label":"#FFFFFF","clips":[u64::MAX],"saveColor":true})] {
        assert!(s.execute("edit.label", params).is_err());
        assert_eq!(s.project.to_json(), before);
    }
}

#[test]
fn metadata_label_edits_use_the_effective_color_and_clear_custom_overrides() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    let preset = s.project.item(item).unwrap().label;
    s.execute("edit.label", json!({"items":[item.0],"label":"#123456"})).unwrap();
    let metadata = s.execute("metadata.get", json!({"item":item.0})).unwrap();
    assert_eq!(metadata["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Label").unwrap()["value"], "#123456");
    let result = s.execute("metadata.set", json!({"item":item.0,"field":"Label","value":preset.name()})).unwrap();
    assert_eq!(result["changed"], true);
    assert_eq!(s.project.item(item).unwrap().custom_label_color, None);
    s.execute("edit.undo", json!({})).unwrap();
    let result = s.execute("metadata.set", json!({"item":item.0,"field":"Label","value":"#123456"})).unwrap();
    assert_eq!(result["changed"], false);
    assert_eq!(result["label"], "#123456");
    s.execute("metadata.set", json!({"item":item.0,"field":"Label","value":"#abcdef"})).unwrap();
    assert_eq!(s.project.item(item).unwrap().label_value(), "#ABCDEF");
    s.execute("metadata.set", json!({"item":item.0,"field":"Label","value":"Red"})).unwrap();
    assert_eq!(s.project.item(item).unwrap().label, Label::Rose);
    assert_eq!(s.project.item(item).unwrap().custom_label_color, None);
}

#[test]
fn marker_and_clip_aliases_resolve_to_the_same_unchanged_presets() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    for (alias, preset) in Label::COLOR_ALIASES {
        let id = s.execute("markers.add", json!({"color":alias.to_lowercase()})).unwrap()["marker"].as_u64().unwrap();
        let marker = s.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap();
        assert_eq!(marker.color, preset);
        assert_eq!(marker.rgb(), preset.rgb());
        s.execute("edit.label", json!({"clips":[clip.0],"label":alias})).unwrap();
        assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.label, preset);
        s.execute("markers.filterColors", json!({"color":alias,"visible":false})).unwrap();
        assert!(s.state.hidden_marker_colors.contains(&preset));
    }
    assert_eq!(Label::Rose.rgb(), [0x80, 0x18, 0x36]);
    assert_eq!(Label::Mango.rgb(), [0x80, 0x3f, 0x17]);
    assert_eq!(Label::Teal.rgb(), [0x1f, 0x4d, 0x45]);
}

#[test]
fn marker_time_edits_are_sorted_exact_and_reject_hostile_ranges_atomically() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({})).unwrap();
    let first = s.execute("markers.add", json!({"frame":10})).unwrap()["marker"].as_u64().unwrap();
    let second = s.execute("markers.add", json!({"frame":20})).unwrap()["marker"].as_u64().unwrap();
    let rate = s.sequence_rate();
    s.execute("markers.edit", json!({"marker":first,"frame":30,"durationFrames":5})).unwrap();
    let markers = &s.active_sequence().unwrap().markers;
    assert_eq!(markers[0].id.0, second);
    assert_eq!(markers[1].start, rate.tick_of(30));
    assert_eq!(markers[1].duration, rate.tick_of(5));
    for change in [
        json!({"frame":-1}),
        json!({"frame":i64::MAX}),
        json!({"durationFrames":-1}),
        json!({"durationFrames":"2"}),
        json!({"time":i64::MAX}),
        json!({"time":true}),
        json!({"frame":1,"time":0}),
        json!({"name":false}),
    ] {
        let before = s.project.to_json();
        let history = s.history.undo.len();
        let mut p = change;
        p["marker"] = json!(first);
        assert!(s.execute("markers.edit", p).is_err());
        assert_eq!(s.project.to_json(), before);
        assert_eq!(s.history.undo.len(), history);
    }
    s.execute("markers.edit", json!({"marker":first,"durationFrames":0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[1].duration, filmcraft_time::Tick::ZERO);
    s.execute("markers.edit", json!({"marker":first,"time":123})).unwrap();
    s.execute("markers.edit", json!({"marker":first,"author":"Preserve subframes"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[0].start.0, 123);
}

#[test]
fn bulk_marker_changes_are_owner_scoped_atomic_and_one_undo() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({})).unwrap();
    let owner = s.state.active_sequence.unwrap();
    let a = s.execute("markers.add", json!({"frame":10})).unwrap()["marker"].as_u64().unwrap();
    let b = s.execute("markers.add", json!({"frame":10})).unwrap()["marker"].as_u64().unwrap();
    s.execute("file.newSequence", json!({})).unwrap();
    let before = s.project.to_json();
    let history = s.history.undo.len();
    s.execute("markers.edit", json!({"sequence":owner.0,"markers":[a,b],"color":"#123456"})).unwrap();
    assert_eq!(s.history.undo.len(), history + 1);
    assert!(s.project.sequence(owner).unwrap().markers.iter().all(|m| m.rgb() == [0x12, 0x34, 0x56]));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
    for params in [
        json!({"markers":[a,u64::MAX]}),
        json!({"markers":[]}),
        json!({"marker":a,"markers":[b]}),
        json!({"markers":[-1]}),
        json!({"marker":a,"target":"nonsense"}),
    ] {
        let mut p = params;
        p["sequence"] = json!(owner.0);
        for cmd in ["markers.edit", "markers.delete"] {
            assert!(s.execute(cmd, p.clone()).is_err());
            assert_eq!(s.project.to_json(), before);
            assert_eq!(s.history.undo.len(), history);
        }
    }
    s.execute("markers.delete", json!({"sequence":owner.0,"marker":a})).unwrap();
    assert_eq!(s.project.sequence(owner).unwrap().markers.iter().map(|m| m.id.0).collect::<Vec<_>>(), vec![b]);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("markers.delete", json!({"sequence":owner.0,"markers":[a,b]})).unwrap();
    assert_eq!(s.history.undo.len(), history + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn source_marker_edits_keep_their_owner_and_enforce_bounds() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let source = filmcraft_project::ItemId(5);
    s.execute("source.open", json!({"item":source.0})).unwrap();
    let marker = s.execute("markers.add", json!({"target":"source","time":0})).unwrap()["marker"].as_u64().unwrap();
    let view = filmcraft_engine::clip_ops::source_view(&s, source).unwrap();
    let other = s.project.items.values().find(|i| i.as_media().is_some() && i.id != source).unwrap().id;
    s.execute("source.open", json!({"item":other.0})).unwrap();
    s.execute("markers.edit", json!({"target":"source","item":source.0,"marker":marker,"frame":1,"durationFrames":2})).unwrap();
    assert_eq!(filmcraft_engine::clip_ops::source_view(&s, source).unwrap().markers[0].start, view.rate.tick_of(1));
    let before = s.project.to_json();
    let history = s.history.undo.len();
    for change in [json!({"time":view.end.0}), json!({"time":view.end.0-1,"durationFrames":2}), json!({"durationFrames":i64::MAX}), json!({"time":-1})] {
        let mut p = change;
        p["target"] = json!("source");
        p["item"] = json!(source.0);
        p["marker"] = json!(marker);
        assert!(s.execute("markers.edit", p).is_err());
        assert_eq!(s.project.to_json(), before);
        assert_eq!(s.history.undo.len(), history);
    }
    assert!(s.execute("markers.delete", json!({"target":"source","marker":marker})).is_err());
    s.execute("markers.delete", json!({"target":"source","item":source.0,"marker":marker})).unwrap();
    assert!(filmcraft_engine::clip_ops::source_view(&s, source).unwrap().markers.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn named_palette_management_preserves_assignments_and_roundtrips() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({})).unwrap();
    s.execute("markers.add", json!({"color":"#123456"})).unwrap();
    s.execute("colors.addCustom", json!({"color":"#123456","name":"Review"})).unwrap();
    let before = s.project.to_json();
    s.execute("colors.updateCustom", json!({"color":"#123456","replacement":"#654321","name":"Approved"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers[0].color_value(), "#123456");
    let new = HexColor::parse("654321").unwrap();
    assert_eq!(s.project.custom_color_names[&new], "Approved");
    let loaded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
    assert_eq!(loaded.project.custom_color_names, s.project.custom_color_names);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
    for (cmd, p) in [
        ("colors.updateCustom", json!({"color":"#123456","replacement":"oops"})),
        ("colors.updateCustom", json!({"color":"#FFFFFF"})),
        ("colors.addCustom", json!({"color":"#FFFFFF","name":"bad\nname"})),
        ("colors.removeCustom", json!({"color":true})),
    ] {
        let history = s.history.undo.len();
        assert!(s.execute(cmd, p).is_err());
        assert_eq!(s.project.to_json(), before);
        assert_eq!(s.history.undo.len(), history);
    }
    s.execute("colors.removeCustom", json!({"color":"#123456"})).unwrap();
    assert!(s.project.custom_colors.is_empty());
    assert!(s.project.custom_color_names.is_empty());
    assert_eq!(s.active_sequence().unwrap().markers[0].color_value(), "#123456");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn subclip_marker_edits_share_parent_storage_but_obey_view_bounds() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("source.open", json!({"item":5})).unwrap();
    let rate = filmcraft_engine::clip_ops::source_view(&s, filmcraft_project::ItemId(5)).unwrap().rate;
    let inside = s.execute("markers.add", json!({"target":"source","time":rate.tick_of(15).0})).unwrap()["marker"].as_u64().unwrap();
    let outside = s.execute("markers.add", json!({"target":"source","time":rate.tick_of(5).0})).unwrap()["marker"].as_u64().unwrap();
    let sub = s.execute("clip.makeSubclip", json!({"item":5,"startFrame":10,"endFrame":30})).unwrap()["item"].as_u64().unwrap();
    s.execute("markers.edit", json!({"target":"source","item":sub,"marker":inside,"frame":20,"durationFrames":10,"author":"Shared"})).unwrap();
    let parent = filmcraft_engine::clip_ops::source_view(&s, filmcraft_project::ItemId(5)).unwrap();
    assert_eq!(parent.markers.iter().find(|m| m.id.0 == inside).unwrap().author, "Shared");
    let before = s.project.to_json();
    for p in [
        json!({"marker":outside,"name":"Hidden"}),
        json!({"marker":inside,"frame":5}),
        json!({"marker":inside,"durationFrames":11}),
        json!({"markers":[inside,outside],"color":"Blue"}),
    ] {
        let mut p = p;
        p["target"] = json!("source");
        p["item"] = json!(sub);
        assert!(s.execute("markers.edit", p).is_err());
        assert_eq!(s.project.to_json(), before);
    }
    s.execute("markers.delete", json!({"target":"source","item":sub,"marker":inside})).unwrap();
    assert_eq!(filmcraft_engine::clip_ops::source_view(&s, filmcraft_project::ItemId(5)).unwrap().markers.len(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn custom_marker_filters_share_show_all_and_reject_invalid_updates() {
    let mut s = Session::default();
    s.execute("markers.filterColors", json!({"hidden":["Red","#123456"]})).unwrap();
    assert!(s.state.hidden_marker_colors.contains(&Label::Rose));
    assert_eq!(s.state.hidden_custom_marker_colors, vec![HexColor::parse("123456").unwrap()]);
    let before = serde_json::to_value(&s.state).unwrap();
    for p in [json!({"hidden":["Blue","bad"]}), json!({"hidden":true}), json!({"color":"#123456","visible":"false"})] {
        assert!(s.execute("markers.filterColors", p).is_err());
        assert_eq!(serde_json::to_value(&s.state).unwrap(), before);
    }
    s.execute("markers.showAllMarkerColors", json!({})).unwrap();
    assert!(s.state.hidden_marker_colors.is_empty());
    assert!(s.state.hidden_custom_marker_colors.is_empty());
    s.execute("markers.filterColors", json!({"color":"#123456","visible":false})).unwrap();
    assert!(s.is_enabled("markers.showAllMarkerColors"));
    s.execute("markers.showAllMarkerColors", json!({})).unwrap();
    assert!(s.state.hidden_custom_marker_colors.is_empty());
}
