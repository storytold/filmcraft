use super::*;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::json;

fn setup() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Source test","width":1280,"height":720,"fps":24})).unwrap();
    s.execute("source.open", json!({"item":5})).unwrap();
    s
}
fn counts(s: &Session) -> (usize, usize) {
    let q = s.active_sequence().unwrap();
    (q.video_tracks.iter().map(|t| t.items.len()).sum(), q.audio_tracks.iter().map(|t| t.items.len()).sum())
}

#[test]
fn unmarked_source_defaults_to_full_span_without_writing_marks() {
    let s = setup();
    let view = clip_ops::source_view(&s, ItemId(5)).unwrap();
    assert_eq!(view.selected_range(), TimeRange::from_bounds(view.start, view.end));
    assert!(view.mark_in.is_none() && view.mark_out.is_none());
}

#[test]
fn marked_range_places_video_audio_or_linked_both_as_one_undo_step() {
    for (video, audio, expected) in [(true, false, (1, 0)), (false, true, (0, 1)), (true, true, (1, 1))] {
        let mut s = setup();
        let start = Tick::from_seconds_f64(1.0);
        let out = Tick::from_seconds_f64(3.0);
        s.execute("project.setMarks", json!({"item":5,"in":start.0,"out":out.0})).unwrap();
        let view = clip_ops::source_view(&s, ItemId(5)).unwrap();
        let range = view.selected_range();
        assert_eq!(range.end(), out + view.rate.frame_duration());
        let before = s.project.to_json();
        s.execute(
            "timeline.place",
            json!({"item":5,"track":"V1","audioTrack":"A1","time":0,"sourceIn":range.start.0,"duration":range.duration.0,"video":video,"audio":audio}),
        )
        .unwrap();
        assert_eq!(counts(&s), expected);
        let q = s.active_sequence().unwrap();
        for clip in q.all_tracks().flat_map(|t| t.items.iter()) {
            assert_eq!(clip.source_in, start);
            assert_eq!(clip.duration, q.settings.frame_rate.snap_nearest(range.duration));
        }
        if video && audio {
            assert!(q.video_tracks[0].items[0].link.is_some());
            assert_eq!(q.video_tracks[0].items[0].link, q.audio_tracks[0].items[0].link);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(counts(&s), expected);
    }
}

#[test]
fn invalid_stream_choices_and_locked_destinations_do_not_mutate_project() {
    let mut s = setup();
    let before = s.project.to_json();
    for params in [json!({"video":false,"audio":false}), json!({"video":"yes"}), json!({"audio":2})] {
        let mut p = json!({"item":5,"track":"V1","audioTrack":"A1","time":0});
        for (k, v) in params.as_object().unwrap() {
            p[k] = v.clone();
        }
        assert!(s.execute("timeline.place", p).is_err());
        assert_eq!(s.project.to_json(), before);
    }
    let id = s.state.active_sequence.unwrap();
    std::sync::Arc::make_mut(&mut s.project).sequence_mut(id).unwrap().audio_tracks[0].locked = true;
    let locked = s.project.to_json();
    assert!(s.execute("timeline.place", json!({"item":5,"track":"V1","audioTrack":"A1","time":0,"video":true,"audio":true})).is_err());
    assert_eq!(s.project.to_json(), locked);
}

#[test]
fn audio_only_source_drops_only_sound() {
    let mut s = setup();
    s.execute("timeline.place", json!({"item":11,"track":"A2","time":0,"video":false,"audio":true})).unwrap();
    assert_eq!(counts(&s), (0, 1));
    assert_eq!(s.active_sequence().unwrap().audio_tracks[1].items.len(), 1);
}

fn subclip_session(video: bool, audio: bool) -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Subclip test","fps":24})).unwrap();
    let parent =
        s.execute("file.newOfflineFile", json!({"name":"Parent","video":video,"audio":audio,"fps":24,"seconds":10})).unwrap()["item"].as_u64().unwrap();
    let sub = s.execute("clip.makeSubclip", json!({"item":parent,"startFrame":10,"endFrame":60})).unwrap()["item"].as_u64().unwrap();
    (s, ItemId(sub))
}

fn assert_subclip_placement(s: &Session, item: ItemId, expected: (usize, usize)) {
    assert_eq!(counts(s), expected);
    let q = s.active_sequence().unwrap();
    let ItemKind::Subclip { range, .. } = &s.project.item(item).unwrap().kind else { panic!("expected a subclip") };
    for clip in q.all_tracks().flat_map(|t| t.items.iter()) {
        assert_eq!(clip.item, item);
        assert_eq!(clip.source_in, range.start);
        assert_eq!(clip.duration, q.settings.frame_rate.snap_nearest(range.duration));
    }
    if expected == (1, 1) {
        let video = &q.video_tracks[0].items[0];
        let audio = &q.audio_tracks[0].items[0];
        assert!(video.link.is_some());
        assert_eq!(video.link, audio.link);
    }
}

#[test]
fn subclip_placement_keeps_parent_streams_range_and_history() {
    for (video, audio, expected) in [(true, true, (1, 1)), (false, true, (0, 1)), (true, false, (1, 0))] {
        let (mut s, sub) = subclip_session(video, audio);
        let before = s.project.to_json();
        s.execute("timeline.place", json!({"item":sub.0,"track":"V1","audioTrack":"A1","time":0,"video":true,"audio":true})).unwrap();
        assert_subclip_placement(&s, sub, expected);
        let placed = s.project.to_json();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), placed);
        s.project = std::sync::Arc::new(Project::from_json(&placed).unwrap());
        assert_subclip_placement(&s, sub, expected);
    }
}

#[test]
fn subclip_audio_can_be_placed_on_an_audio_track() {
    for video in [false, true] {
        let (mut s, sub) = subclip_session(video, true);
        s.execute("timeline.place", json!({"item":sub.0,"track":"A1","time":0,"video":false,"audio":true})).unwrap();
        assert_subclip_placement(&s, sub, (0, 1));
    }
    let (mut s, sub) = subclip_session(true, false);
    let before = s.project.to_json();
    assert!(s.execute("timeline.place", json!({"item":sub.0,"track":"A1","time":0,"video":false,"audio":true})).is_err());
    assert_eq!(s.project.to_json(), before);
}

#[test]
fn source_insert_and_overwrite_keep_subclip_sound() {
    for command in ["source.insert", "source.overwrite"] {
        for (video, audio, expected) in [(true, true, (1, 1)), (false, true, (0, 1)), (true, false, (1, 0))] {
            let (mut s, sub) = subclip_session(video, audio);
            s.execute("source.open", json!({"item":sub.0})).unwrap();
            s.execute(command, json!({})).unwrap();
            assert_subclip_placement(&s, sub, expected);
        }
    }
}

#[test]
fn subclip_placement_can_add_tracks_for_parent_streams() {
    for (video, audio, expected) in [(true, true, (1, 1)), (false, true, (0, 1)), (true, false, (1, 0))] {
        let (mut s, sub) = subclip_session(video, audio);
        let before = s.project.to_json();
        s.execute("timeline.place", json!({"item":sub.0,"track":"new","audioTrack":"new","time":0})).unwrap();
        assert_eq!(counts(&s), expected);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
    }
}

#[test]
fn subclip_placement_keeps_parent_channel_map_and_all_audio_streams() {
    let (mut s, sub) = subclip_session(true, true);
    let parent = s.project.resolve_media(sub).unwrap().0;
    let m = std::sync::Arc::make_mut(&mut s.project).item_mut(parent).unwrap().as_media_mut().unwrap();
    m.info.audio_streams.push(m.info.audio_streams[0].clone());
    m.interpret.audio_channels = Some(filmcraft_project::AudioChannelMap { format: filmcraft_project::AudioChannels::Mono, clips: vec![vec![1], vec![0]] });
    s.execute("timeline.place", json!({"item":sub.0,"track":"V1","audioTrack":"A1","time":0})).unwrap();
    assert_eq!(counts(&s), (1, 3));
    let q = s.active_sequence().unwrap();
    let link = q.video_tracks[0].items[0].link;
    assert!(link.is_some());
    for (index, (stream, channels)) in [(0, vec![1]), (0, vec![0]), (1, vec![])].into_iter().enumerate() {
        let clip = &q.audio_tracks[index].items[0];
        assert_eq!(clip.item, sub);
        assert_eq!(clip.audio_stream, stream);
        assert_eq!(clip.source_channels, channels);
        assert_eq!(clip.link, link);
    }
}

#[test]
fn malformed_source_spans_are_bounded() {
    let s = setup();
    let mut view = clip_ops::source_view(&s, ItemId(5)).unwrap();
    view.mark_in = Some(Tick::MAX);
    view.mark_out = Some(Tick::ZERO);
    let range = view.selected_range();
    assert_eq!(range.start, view.end);
    assert_eq!(range.duration, Tick::ZERO);
    view.end = Tick::ZERO;
    view.start = Tick::from_seconds_f64(1.0);
    assert_eq!(view.selected_range().duration, Tick::ZERO);
}

/// #1101: a placement that starts between frames must not run past `time + duration`, and the
/// frame count `sequence.inspect` reports is the one export renders.
#[test]
fn sub_frame_place_ends_at_requested_end_and_inspect_matches_export_frames() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"f","width":640,"height":360,"fps":24,"video":1,"audio":2})).unwrap();
    let rate = s.sequence_rate();
    let fd = rate.frame_duration().0;
    let end = rate.tick_of(120).0;
    s.execute("timeline.place", json!({"item":5,"track":"V1","audioTrack":"A1","frame":0,"sourceIn":0,"duration":end})).unwrap();
    // a third of a frame after frame 60, ending exactly on frame 120
    let at = rate.tick_of(60).0 + fd / 3;
    let placed = s.execute("timeline.place", json!({"item":5,"track":"A2","time":at,"sourceIn":0,"duration":end - at})).unwrap();
    let clip = placed["clips"][0].as_u64().unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let c = js["audio"][1]["items"].as_array().unwrap().iter().find(|c| c["clip"] == clip).unwrap().clone();
    assert!(c["end"].as_i64().unwrap() <= end, "clip runs past the requested end: {c}");
    assert_eq!(rate.snap(Tick(c["start"].as_i64().unwrap())).0, c["start"].as_i64().unwrap(), "clip starts between frames: {c}");
    assert_eq!(js["duration"], end);
    let frames = s.execute("export.resolve", json!({})).unwrap()["frames"].clone();
    assert_eq!(js["durationFrames"], frames);
    assert_eq!(frames, 120);
}

/// A sequence that ends between frames (a clip moved to a sub-frame time) still reports the
/// frame count export renders, the partial last frame included.
#[test]
fn inspect_duration_frames_counts_a_partial_last_frame_like_export() {
    let mut s = setup();
    let rate = s.sequence_rate();
    let placed = s.execute("timeline.place", json!({"item":5,"track":"V1","audioTrack":"A1","frame":0,"sourceIn":0,"duration":rate.tick_of(48).0})).unwrap();
    let clips: Vec<u64> = placed["clips"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).collect();
    let tracks = s.active_sequence().map(|q| (q.video_tracks[0].id.0, q.audio_tracks[0].id.0)).unwrap();
    let t = rate.frame_duration().0 / 3;
    s.execute("timeline.move", json!({"moves":[{"clip":clips[0],"track":tracks.0,"time":t},{"clip":clips[1],"track":tracks.1,"time":t}]})).unwrap();
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let frames = s.execute("export.resolve", json!({})).unwrap()["frames"].clone();
    assert_eq!(js["durationFrames"], frames);
    assert_eq!(frames, 49);
}
