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

/// `sourceIn` and `duration` accept the same time objects as `time`, and mean the same range as
/// the equivalent ticks.
#[test]
fn place_source_range_in_seconds_equals_ticks() {
    let place = |source_in: serde_json::Value, duration: serde_json::Value| {
        let mut s = setup();
        s.execute("timeline.place", json!({"item":5,"track":"V1","time":0,"sourceIn":source_in,"duration":duration,"audio":false})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        (clip.source_in, clip.duration)
    };
    let (sin, dur) = (Tick::from_seconds_f64(1.0), Tick::from_seconds_f64(2.0));
    let by_ticks = place(json!(sin.0), json!(dur.0));
    assert_eq!(by_ticks.0, sin);
    assert_eq!(place(json!({"seconds": 1.0}), json!({"seconds": 2.0})), by_ticks);
    assert_eq!(place(json!({"seconds": 1.0}), json!(dur.0)), by_ticks);
    assert_eq!(place(json!(sin.0), json!({"seconds": 2.0})), by_ticks);
}

/// `frame` and `timecode` objects read against the rate of the thing they measure: `sourceIn` the
/// item's, `duration` the sequence's.
#[test]
fn place_source_range_in_frames_and_timecode_equals_ticks() {
    let place = |source_in: serde_json::Value, duration: serde_json::Value| {
        let mut s = setup();
        s.execute("timeline.place", json!({"item":5,"track":"V1","time":0,"sourceIn":source_in,"duration":duration,"audio":false})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        (clip.source_in, clip.duration)
    };
    let s = setup();
    let item_rate = s.project.item(ItemId(5)).unwrap().frame_rate();
    let seq_rate = s.sequence_rate();
    let (sin, dur) = (item_rate.tick_of(12), seq_rate.tick_of(48));
    let by_ticks = place(json!(sin.0), json!(dur.0));
    assert_eq!(by_ticks.0, sin);
    assert_eq!(place(json!({"frame": 12}), json!({"frame": 48})), by_ticks);
    assert_eq!(place(json!({"time": sin.0}), json!({"time": dur.0})), by_ticks);
    assert_eq!(place(json!(sin.0), json!({"timecode": "00:00:02:00"})), by_ticks, "48 frames at 24 fps");
}

/// A 40 s, 25 fps offline file whose timecode starts at 14:30:42:00, in a 25 fps sequence.
fn timecoded_session() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Timecode test","fps":25})).unwrap();
    let p = json!({"name":"A001","audio":false,"fps":25,"seconds":40,"timecode":"14:30:42:00"});
    let item = s.execute("file.newOfflineFile", p).unwrap()["item"].as_u64().unwrap();
    (s, ItemId(item))
}

/// `{"sourceTimecode": …}` is the timecode on the clip, read against its start timecode (a
/// subclip's against its parent's); `{"timecode": …}` still counts from the clip's first frame.
#[test]
fn place_source_in_by_source_timecode_reads_the_clip_start_timecode() {
    let (mut s, item) = timecoded_session();
    let sub = s.execute("clip.makeSubclip", json!({"item":item.0,"startFrame":25,"endFrame":100})).unwrap()["item"].as_u64().unwrap();
    let want = s.project.item(item).unwrap().frame_rate().tick_of(50);
    for (i, (it, source_in)) in
        [(item.0, json!({"sourceTimecode":"14:30:44:00"})), (item.0, json!({"timecode":"00:00:02:00"})), (sub, json!({"sourceTimecode":"14:30:44:00"}))]
            .into_iter()
            .enumerate()
    {
        s.execute("timeline.place", json!({"item":it,"track":"V1","seconds":i * 10,"sourceIn":source_in,"duration":{"seconds":1}})).unwrap();
    }
    let items = &s.active_sequence().unwrap().video_tracks[0].items;
    assert_eq!(items.iter().map(|c| c.source_in).collect::<Vec<_>>(), [want; 3]);
}

/// A drop-frame source timecode (`;`) counts as drop-frame, as the start timecode a file carries does.
#[test]
fn place_source_in_by_drop_frame_source_timecode() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Drop-frame test","fps":29.97})).unwrap();
    let item = s.execute("file.newOfflineFile", json!({"name":"B001","audio":false,"fps":29.97,"seconds":40})).unwrap()["item"].as_u64().unwrap();
    // 01:00:00;00 drop-frame is frame 107892
    s.execute("clip.modifyTimecode", json!({"item":item,"frame":107_892})).unwrap();
    s.execute("timeline.place", json!({"item":item,"track":"V1","time":0,"sourceIn":{"sourceTimecode":"01:00:02;00"}})).unwrap();
    let rate = s.project.item(ItemId(item)).unwrap().frame_rate();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items[0].source_in, rate.tick_of(60));
}

/// A `sourceIn` outside the clip is refused and changes nothing (#3955): the timecode on the clip
/// passed as `timecode` (52 244 s into a 40 s clip), source timecodes before its first frame and
/// past its last, and malformed source timecodes. The first and last frames are accepted.
#[test]
fn place_source_in_outside_the_clip_is_refused() {
    let (mut s, item) = timecoded_session();
    let before = s.project.to_json();
    for source_in in [
        json!({"timecode":"14:30:44:00"}),
        json!({"seconds":40.0}),
        json!({"sourceTimecode":"14:30:41:24"}),
        json!({"sourceTimecode":"14:31:22:00"}),
        json!({"sourceTimecode":"nope"}),
        json!({"sourceTimecode":5}),
    ] {
        let e = s.execute("timeline.place", json!({"item":item.0,"track":"V1","time":0,"sourceIn":source_in})).unwrap_err();
        assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.place"), "{source_in}: {e:?}");
        assert_eq!(s.project.to_json(), before, "{source_in}");
    }
    let e = s.execute("timeline.place", json!({"item":item.0,"track":"V1","time":0,"sourceIn":{"timecode":"14:30:44:00"}})).unwrap_err().to_string();
    assert!(e.contains("00:00:39:24") && e.contains("14:30:42:00") && e.contains("sourceTimecode"), "{e}");
    for source_in in [json!({"sourceTimecode":"14:30:42:00"}), json!({"sourceTimecode":"14:31:21:24"}), json!({"timecode":"00:00:39:24"}), json!({"frame":0})] {
        s.execute("timeline.place", json!({"item":item.0,"track":"V1","time":0,"sourceIn":source_in,"duration":{"frame":1}})).unwrap();
    }
}

/// The error names the frames the check accepts when the item's bounds fall between frame
/// boundaries: a subclip from 10.5 to 25.5 frames takes frames 11 to 25, not 10 to 24.
#[test]
fn place_source_in_error_names_the_frames_it_accepts() {
    let (mut s, item) = timecoded_session();
    let fd = s.project.item(item).unwrap().frame_rate().frame_duration().0;
    let p = json!({"item":item.0,"start":fd * 10 + fd / 2,"end":fd * 25 + fd / 2});
    let sub = s.execute("clip.makeSubclip", p).unwrap()["item"].as_u64().unwrap();
    for frame in [11, 25] {
        s.execute("timeline.place", json!({"item":sub,"track":"V1","time":0,"sourceIn":{"frame":frame},"duration":{"frame":1}})).unwrap();
    }
    for frame in [10, 26] {
        let e = s.execute("timeline.place", json!({"item":sub,"track":"V1","time":0,"sourceIn":{"frame":frame}})).unwrap_err().to_string();
        assert!(e.contains("whose frames run 00:00:00:11 to 00:00:01:00"), "frame {frame}: {e}");
    }
}

/// A `sourceIn` between frame starts (only ticks or seconds give one), or one on a clip that no
/// frame starts in, is reported in ticks: as a frame it would read as frame 25 of a clip whose
/// frames run to 25, or as a clip with no frames that still takes some ticks.
#[test]
fn place_source_in_between_frames_is_reported_in_ticks() {
    let (mut s, item) = timecoded_session();
    let fd = s.project.item(item).unwrap().frame_rate().frame_duration().0;
    let (first, end) = (fd * 10 + fd / 2, fd * 25 + fd / 2);
    let sub = s.execute("clip.makeSubclip", json!({"item":item.0,"start":first,"end":end})).unwrap()["item"].as_u64().unwrap();
    for sin in [fd * 25 + fd * 3 / 4, end, fd * 10 + fd / 4] {
        let e = s.execute("timeline.place", json!({"item":sub,"track":"V1","time":0,"sourceIn":sin})).unwrap_err().to_string();
        assert!(e.contains(&format!("`sourceIn` {sin} ticks is outside the clip, which takes {first} to {end} ticks")), "{e}");
        // the clip has a start timecode: the hint names its first frame there, in ticks too
        assert!(e.contains("it starts at 14:30:42:11") && e.contains("sourceTimecode"), "{e}");
    }
    let tiny = json!({"item":item.0,"start":fd * 10 + fd / 4,"end":fd * 10 + fd * 3 / 4});
    let tiny = s.execute("clip.makeSubclip", tiny).unwrap()["item"].as_u64().unwrap();
    let e = s.execute("timeline.place", json!({"item":tiny,"track":"V1","time":0,"sourceIn":{"frame":10}})).unwrap_err().to_string();
    assert!(e.contains("ticks is outside the clip, which takes") && !e.contains("frame") && !e.contains("sourceTimecode"), "{e}");
    s.execute("timeline.place", json!({"item":tiny,"track":"V1","time":0,"sourceIn":fd * 10 + fd / 2,"duration":{"frame":1}})).unwrap();
}

/// An item with no frames (an empty sequence) takes no `sourceIn`: frame 0 is already its end.
#[test]
fn place_source_in_on_an_item_with_no_frames_is_refused() {
    let mut s = Session::default();
    let empty = s.execute("file.newSequence", json!({"name":"Empty"})).unwrap()["sequence"].as_u64().unwrap();
    s.execute("file.newSequence", json!({"name":"Main"})).unwrap();
    let before = s.project.to_json();
    let e = s.execute("timeline.place", json!({"item":empty,"track":"V1","time":0,"sourceIn":{"frame":0}})).unwrap_err();
    assert!(matches!(&e, EngineError::BadParams { cmd, msg } if cmd == "timeline.place" && msg.contains("no frames")), "{e:?}");
    assert_eq!(s.project.to_json(), before);
}

/// A `sourceIn` or `duration` that is neither ticks nor a time object (or is negative) is refused
/// before anything changes, also when `track` or `audioTrack` is `"new"`: no new track, the same
/// revision, and the redo history intact.
#[test]
fn place_malformed_source_range_is_refused_before_any_change() {
    for (key, bad) in [
        ("sourceIn", json!("1s")),
        ("sourceIn", json!({"seconds": "x"})),
        ("sourceIn", json!({})),
        ("sourceIn", json!({"bogus": 1})),
        ("sourceIn", json!(1.5)),
        ("sourceIn", json!(true)),
        ("sourceIn", json!(-1)),
        ("sourceIn", json!({"seconds": -1.0})),
        ("duration", json!("2s")),
        ("duration", json!({"seconds": "x"})),
        ("duration", json!({"timecode": "nope"})),
        ("duration", json!([1])),
        ("duration", json!(-1)),
        ("duration", json!({"frame": -24})),
    ] {
        // a placement undone: the redo history holds something worth keeping
        let mut s = setup();
        s.execute("timeline.place", json!({"item":5,"track":"V1","time":0})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
        assert!(redo >= 1);
        let before = s.project.to_json();
        let (nv, na) = (s.active_sequence().unwrap().video_tracks.len(), s.active_sequence().unwrap().audio_tracks.len());
        let revision = s.revision;

        for tracks in [
            json!({"track":"V1","audio":false}),
            json!({"track":"new","audio":false}),
            json!({"audioTrack":"new","video":false}),
            json!({"track":"new","audioTrack":"new"}),
        ] {
            let mut p = json!({"item":5,"time":0});
            for (k, v) in tracks.as_object().into_iter().flatten() {
                p[k.as_str()] = v.clone();
            }
            p[key] = bad.clone();
            assert!(s.execute("timeline.place", p).is_err(), "{key} = {bad} with {tracks}");
            let q = s.active_sequence().unwrap();
            assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (nv, na), "{key} = {bad} with {tracks}: no track added");
            assert_eq!(counts(&s), (0, 0));
            assert_eq!(s.project.to_json(), before, "{key} = {bad} with {tracks}");
            assert_eq!(s.revision, revision, "{key} = {bad} with {tracks}: revision");
            assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo), "{key} = {bad} with {tracks}: history");
        }
    }
}

/// Parameters that are wrong in other ways (stream flags, a negative `time`, a track that does not
/// exist) are refused before the `"new"` track is added as well.
#[test]
fn place_on_new_track_with_other_bad_params_changes_nothing() {
    let mut s = setup();
    s.execute("timeline.place", json!({"item":5,"track":"V1","time":0})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    let before = s.project.to_json();
    let revision = s.revision;
    for params in [
        json!({"item":5,"track":"new","audioTrack":"new","time":0,"video":"yes"}),
        json!({"item":5,"track":"new","audioTrack":"new","time":0,"audio":2}),
        json!({"item":5,"track":"new","audioTrack":"new","time":0,"video":false,"audio":false}),
        json!({"item":5,"track":"new","audioTrack":"new","time":-1}),
        json!({"item":5,"track":"new","audioTrack":"A99","time":0}),
        json!({"item":5,"track":"V99","audioTrack":"new","time":0}),
        json!({"item":999_999,"track":"new","time":0}),
    ] {
        assert!(s.execute("timeline.place", params.clone()).is_err(), "{params}");
        assert_eq!(s.project.to_json(), before, "{params}");
        assert_eq!(s.revision, revision, "{params}: revision");
        assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo), "{params}: history");
    }
}

/// A placement that only fails after the new track was added (a track of the wrong kind) is rolled
/// back whole: the project, the undo history and the redo history are as before.
#[test]
fn place_on_new_track_rolls_back_a_late_failure() {
    let mut s = setup();
    s.execute("timeline.place", json!({"item":5,"track":"V1","time":0})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    let before = s.project.to_json();
    // `audioTrack` names a video track: only known once the tracks exist
    assert!(s.execute("timeline.place", json!({"item":5,"track":"new","audioTrack":"V1","time":0})).is_err());
    assert_eq!(s.project.to_json(), before);
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
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

/// A subclip's `sourceIn` frame / timecode counts frames of the parent media (24 fps here), not
/// the subclip's own default rate (23.976), the same as the Source monitor's marks.
#[test]
fn place_subclip_source_in_frames_use_the_parent_media_rate() {
    let (mut s, sub) = subclip_session(true, false);
    let ItemKind::Subclip { parent, .. } = s.project.item(sub).unwrap().kind else { panic!("expected a subclip") };
    let want = s.project.item(parent).unwrap().frame_rate().tick_of(25);
    s.execute("timeline.place", json!({"item":sub.0,"track":"V1","time":0,"sourceIn":{"frame":25}})).unwrap();
    s.execute("timeline.place", json!({"item":sub.0,"track":"V1","time":want.0 * 10,"sourceIn":{"timecode":"00:00:01:01"}})).unwrap();
    let items = &s.active_sequence().unwrap().video_tracks[0].items;
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|c| c.source_in == want), "{:?} != {want:?}", items.iter().map(|c| c.source_in).collect::<Vec<_>>());
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

/// Video and audio tracks of the active sequence that hold a clip.
fn filled_tracks(s: &Session) -> (usize, usize) {
    let q = s.active_sequence().unwrap();
    (q.video_tracks.iter().filter(|t| !t.items.is_empty()).count(), q.audio_tracks.iter().filter(|t| !t.items.is_empty()).count())
}

/// #1887: the Timeline's drop preview draws `placement_lanes` tracks, so they must be the tracks
/// `timeline.place` fills, for every way an item can spread over several tracks.
#[test]
fn placement_lanes_are_the_tracks_a_placement_fills() {
    let full = |s: &Session, item: ItemId| TimeRange::new(Tick::ZERO, s.project.item(item).unwrap().duration());
    // picture and sound, either one alone
    for (video, audio) in [(true, true), (true, false), (false, true)] {
        let mut s = setup();
        let lanes = s.placement_lanes(ItemId(5), full(&s, ItemId(5)), video, audio);
        s.execute("timeline.place", json!({"item":5,"track":"V1","audioTrack":"A1","time":0,"video":video,"audio":audio})).unwrap();
        assert_eq!(lanes, filled_tracks(&s), "video {video}, audio {audio}");
    }
    // Modify ▸ Audio Channels with two clips, and a second container audio stream: three audio tracks
    let mut s = setup();
    let m = std::sync::Arc::make_mut(&mut s.project).item_mut(ItemId(5)).unwrap().as_media_mut().unwrap();
    m.info.audio_streams.push(m.info.audio_streams[0].clone());
    m.interpret.audio_channels = Some(filmcraft_project::AudioChannelMap { format: filmcraft_project::AudioChannels::Mono, clips: vec![vec![1], vec![0]] });
    let lanes = s.placement_lanes(ItemId(5), full(&s, ItemId(5)), true, true);
    assert_eq!(lanes, (1, 3));
    s.execute("timeline.place", json!({"item":5,"track":"V1","audioTrack":"A1","time":0})).unwrap();
    assert_eq!(lanes, filled_tracks(&s));
    // the nest toggle off: a sequence edits in as its clips, one track per source track with clips
    let mut s = setup();
    let active = s.state.active_sequence;
    let demo = s.project.items.values().find(|i| i.as_sequence().is_some() && Some(i.id) != active).map(|i| i.id).unwrap();
    s.execute("sequence.nestSequences", json!({"on": false})).unwrap();
    let lanes = s.placement_lanes(demo, full(&s, demo), true, true);
    assert!(lanes.0 > 1 && lanes.1 > 0, "the demo sequence spans several tracks: {lanes:?}");
    s.execute("timeline.place", json!({"item":demo.0,"track":"V1","audioTrack":"A1","time":0})).unwrap();
    assert_eq!(lanes, filled_tracks(&s));
    // nested, the same sequence is one clip per kind
    let mut s = setup();
    let lanes = s.placement_lanes(demo, full(&s, demo), true, true);
    s.execute("timeline.place", json!({"item":demo.0,"track":"V1","audioTrack":"A1","time":0})).unwrap();
    assert_eq!(lanes, filled_tracks(&s));
    assert_eq!(lanes, (1, 1));
}

/// A media timecode is drop-frame when `;` comes before its frames, wherever it is typed: the start
/// of a new offline file or of Modify ▸ Timecode, a `sourceTimecode`, and Frame Hold Options'
/// source timecode all count 01:00:00;00 at 29.97 as frame 107892 (what a drop-frame file stores)
/// and 01:00:00:00 as 108000, so a source timecode 2 s after the start is 60 frames in.
#[test]
fn media_timecodes_read_drop_frame_alike_wherever_typed() {
    // (start timecode, the command that sets it, the timecode 2 s later, the start in frames)
    for (start, set_by, later, frames) in [
        ("01:00:00;00", "file.newOfflineFile", "01:00:02;00", 107_892),
        ("01:00:00;00", "clip.modifyTimecode", "01:00:02;00", 107_892),
        ("01:00:00:00", "file.newOfflineFile", "01:00:02:00", 108_000),
        ("01:00:00:00", "clip.modifyTimecode", "01:00:02:00", 108_000),
    ] {
        let why = format!("{start} set by {set_by}");
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"name":"Drop-frame test","fps":29.97})).unwrap();
        let mut p = json!({"name":"B001","audio":false,"fps":29.97,"seconds":40});
        if set_by == "file.newOfflineFile" {
            p["timecode"] = json!(start);
        }
        let item = s.execute("file.newOfflineFile", p).unwrap()["item"].as_u64().unwrap();
        if set_by == "clip.modifyTimecode" {
            s.execute("clip.modifyTimecode", json!({"item":item,"timecode":start})).unwrap();
        }
        let it = s.project.item(ItemId(item)).unwrap();
        assert_eq!(it.as_media().unwrap().info.start_timecode, Some(frames), "{why}: start");
        let two_s = it.frame_rate().tick_of(60);
        let r = s.execute("timeline.place", json!({"item":item,"track":"V1","time":0,"sourceIn":{"sourceTimecode":later}})).unwrap();
        assert_eq!(s.active_sequence().unwrap().video_tracks[0].items[0].source_in, two_s, "{why}: sourceTimecode");
        s.execute("clip.frameHoldOptions", json!({"clips":[r["clips"][0]],"holdOn":"sourceTimecode","timecode":later})).unwrap();
        assert_eq!(s.active_sequence().unwrap().video_tracks[0].items[0].frame_hold, Some(two_s), "{why}: Frame Hold Options");
    }
}

/// With `matchSequence` the sequence takes the clip's settings in the same undo step as the
/// placement. A placement its parameters refuse is refused before that, so the project, its
/// revision and its history stay as they were (a rolled-back group still moves the revision on).
#[test]
fn place_with_match_sequence_refuses_before_changing_the_sequence() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Match test","fps":24})).unwrap();
    // a 25 fps clip with no sound, 40 s from 14:30:42:00
    let p = json!({"name":"A001","audio":false,"fps":25,"seconds":40,"timecode":"14:30:42:00"});
    let item = s.execute("file.newOfflineFile", p).unwrap()["item"].as_u64().unwrap();
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    let before = s.project.to_json();
    let revision = s.revision;
    for (params, why) in [
        (json!({"item":item,"sourceIn":{"sourceTimecode":"14:30:41:00"}}), "a source timecode before the clip"),
        (json!({"item":item,"sourceIn":{"frame":5000}}), "a sourceIn past the clip's end"),
        (json!({"item":item,"track":"A1"}), "an audio track for a clip with no sound"),
        (json!({"item":item,"video":"yes"}), "a stream flag that is not a boolean"),
        (json!({"items":[item],"sourceIn":0}), "sourceIn with items"),
        (json!({"items":[item, 999_999]}), "an item that does not exist"),
    ] {
        let mut p = params;
        p["matchSequence"] = json!(true);
        p["time"] = json!(0);
        if p.get("track").is_none() {
            p["track"] = json!("V1");
        }
        assert!(s.execute("timeline.place", p).is_err(), "{why}");
        assert_eq!(s.project.to_json(), before, "{why}: project");
        assert_eq!(s.revision, revision, "{why}: revision");
        assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo), "{why}: history");
    }
    // a placement that is right still takes the clip's rate, in one undo step
    s.execute("timeline.place", json!({"item":item,"track":"V1","time":0,"matchSequence":true})).unwrap();
    assert_eq!(s.active_sequence().unwrap().settings.frame_rate, s.project.item(ItemId(item)).unwrap().frame_rate());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.to_json(), before);
}
