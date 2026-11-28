use serde_json::json;

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn captions_import_edit_export_and_burn_in() {
    let dir = std::env::temp_dir().join(format!("fc-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let srt = dir.join("talk.srt");
    std::fs::write(
        &srt,
        "\u{feff}1\r\n00:00:00,500 --> 00:00:02,000\r\nHello there\r\n\r\n2\r\n00:00:02,500 --> 00:00:04,000\r\nSecond line\r\nwith two rows\r\n",
    )
    .unwrap();
    let mut s = demo();
    let r = s.execute("file.import", json!({"paths": [srt.to_string_lossy()]})).unwrap();
    assert_eq!(r["documents"][0]["captions"], 2, "{r}");
    let q = s.active_sequence().unwrap();
    let rate = q.settings.frame_rate;
    let tr = q.caption_tracks[0].clone();
    assert_eq!(tr.name, "talk");
    assert_eq!(tr.captions[0].start, rate.snap_nearest(Tick(TICKS_PER_SECOND / 2)), "snapped to frames");
    let first = tr.captions[0].id;
    let second = tr.captions[1].id;

    // edit text, split, merge, trim, move, add at playhead, delete — each one undoable
    s.execute("captions.setText", json!({"caption": first.0, "text": "Hi!", "speaker": "Ann"})).unwrap();
    let split_at = rate.tick_of(rate.frame_at(tr.captions[1].start) + 10);
    let r = s.execute("captions.split", json!({"caption": second.0, "time": split_at.0})).unwrap();
    let right = ClipId(r["captions"][0].as_u64().unwrap());
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions.len(), 3);
    s.execute("captions.merge", json!({"captions": [second.0, right.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions.len(), 2);
    s.execute("captions.trim", json!({"caption": second.0, "edge": "out", "deltaFrames": 12})).unwrap();
    s.execute("captions.move", json!({"captions": [second.0], "deltaFrames": 24})).unwrap();
    s.execute("playhead.set", json!({"seconds": 8.0})).unwrap();
    let r = s.execute("captions.add", json!({"text": "Added"})).unwrap();
    let added = ClipId(r["caption"].as_u64().unwrap());
    assert_eq!(s.state.caption_selection, vec![added]);
    s.execute("edit.clear", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().find_caption(added).is_none(), "Clear deletes selected captions");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().find_caption(added).is_some());
    let list = s.execute("captions.list", json!({})).unwrap();
    assert_eq!(list["tracks"][0]["captions"][0]["text"], "Hi!");
    assert_eq!(list["tracks"][0]["captions"][0]["speaker"], "Ann");
    s.active_sequence().unwrap().check().unwrap();

    // export in all three formats and read back: same texts
    for ext in ["srt", "vtt", "scc"] {
        let path = dir.join(format!("out.{ext}")).to_string_lossy().to_string();
        let r = s.execute("captions.export", json!({"path": path})).unwrap();
        assert_eq!(r["captions"], 3, "{ext}: {r}");
        let back = filmcraft_captions::parse(&std::fs::read(&path).unwrap(), filmcraft_captions::Format::from_name(ext).unwrap()).unwrap();
        let texts: Vec<String> = back.cues.iter().map(|c| filmcraft_project::plain_text(&c.text)).collect();
        assert_eq!(texts.len(), 3, "{ext}");
        assert_eq!(texts[0], "Hi!", "{ext}");
    }

    // the program renders captions; hiding the track removes them
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let with = s.render_program(0.25).unwrap();
    s.execute("captions.hideAll", json!({})).unwrap();
    let without = s.render_program(0.25).unwrap();
    let diff = with.px.iter().zip(&without.px).filter(|(a, b)| (*a - *b).abs() > 0.05).count();
    assert!(diff > 200, "captions change the picture ({diff} samples)");
    let half = with.w * 4 * (with.h / 2);
    let top_diff = with.px[..half].iter().zip(&without.px[..half]).filter(|(a, b)| (*a - *b).abs() > 0.05).count();
    assert_eq!(top_diff, 0, "bottom-anchored captions stay in the lower half");
    s.execute("captions.showAll", json!({})).unwrap();
    let plan = filmcraft_render::plan::plan_frame(
        &s.project,
        s.state.active_sequence.unwrap(),
        s.playhead(),
        filmcraft_render::RenderOptions { scale: 0.25, captions: true, ..Default::default() },
        filmcraft_render::plan::DeviceCaps::ALL,
        &s.media.provider(s.project.clone(), s.services.clone()),
    );
    let cpu = filmcraft_render::plan::execute_cpu(&plan);
    let pd = cpu.px.iter().zip(&with.px).filter(|(a, b)| (*a - *b).abs() > 0.02).count();
    assert!(pd < cpu.px.len() / 200, "the GPU plan matches the CPU render ({pd})");

    // save/open keeps caption tracks
    let proj = dir.join("cap.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    let before = s.active_sequence().unwrap().caption_tracks.clone();
    s.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks, before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caption_track_follows_sync_locked_insert() {
    let mut s = demo();
    s.execute("captions.newTrack", json!({"format": "CEA-608"})).unwrap();
    s.execute("playhead.set", json!({"seconds": 4.0})).unwrap();
    s.execute("captions.add", json!({"text": "later"})).unwrap();
    let start = s.active_sequence().unwrap().caption_tracks[0].captions[0].start;
    let item = s.project.items.values().find(|i| i.as_media().is_some_and(|m| m.info.has_video())).unwrap().id;
    s.execute("source.open", json!({"item": item.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    s.execute("source.insert", json!({})).unwrap();
    let after = s.active_sequence().unwrap().caption_tracks[0].captions[0].start;
    assert!(after > start, "caption moved right with the insert");
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn caption_commands_need_tracks_and_navigate() {
    let mut s = demo();
    assert!(s.execute("captions.split", json!({})).is_err(), "disabled without captions");
    for (sec, text) in [(1.0, "one"), (3.0, "two"), (6.0, "three")] {
        s.execute("playhead.set", json!({"seconds": sec})).unwrap();
        s.execute("captions.add", json!({"text": text, "durationSeconds": 1.0})).unwrap();
    }
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    s.execute("captions.next", json!({})).unwrap();
    s.execute("captions.next", json!({})).unwrap();
    let id = s.state.caption_selection[0];
    assert_eq!(s.active_sequence().unwrap().find_caption(id).unwrap().1.text, "two");
    s.execute("captions.previous", json!({})).unwrap();
    let id = s.state.caption_selection[0];
    assert_eq!(s.active_sequence().unwrap().find_caption(id).unwrap().1.text, "one");
    s.execute("captions.setStyle", json!({"color": "#ffff00", "anchor": "top", "size": 40})).unwrap();
    let st = &s.active_sequence().unwrap().caption_tracks[0].style;
    assert_eq!(st.color, [255, 255, 0, 255]);
    assert!(s.execute("captions.setStyle", json!({"anchor": "sideways"})).is_err());
    s.execute("captions.setTimes", json!({"caption": id.0, "startSeconds": 0.5, "endSeconds": 2.5})).unwrap();
    assert!(s.execute("captions.setTimes", json!({"caption": id.0, "endSeconds": 3.5})).is_err(), "would overlap `two`");
    s.execute("captions.deleteTrack", json!({"track": "C1"})).unwrap();
    assert!(s.active_sequence().unwrap().caption_tracks.is_empty());
}

#[test]
fn caption_list_track_filter_never_falls_back_to_every_track() {
    let mut s = demo();
    s.execute("captions.newTrack", json!({})).unwrap();
    s.execute("captions.newTrack", json!({})).unwrap();
    let all = s.execute("captions.list", json!({})).unwrap();
    let ids: Vec<u64> = all["tracks"].as_array().unwrap().iter().map(|t| t["id"].as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 2, "{all}");
    for (i, id) in ids.iter().enumerate() {
        for sel in [json!(id), json!(format!("C{}", i + 1))] {
            let r = s.execute("captions.list", json!({"track": sel})).unwrap();
            let got: Vec<u64> = r["tracks"].as_array().unwrap().iter().map(|t| t["id"].as_u64().unwrap()).collect();
            assert_eq!(got, vec![*id], "{sel}");
        }
    }
    let missing = ids.iter().max().unwrap() + 999;
    for sel in [json!(missing), json!("C3"), json!("C0"), json!("nope")] {
        assert!(s.execute("captions.list", json!({"track": sel})).is_err(), "{sel} names no caption track");
    }
}

fn caption_spans(s: &Session) -> Vec<(f64, f64, String)> {
    let secs = |t: Tick| (t.seconds() * 10.0).round() / 10.0; // the demo sequence snaps to 23.976 fps frames
    s.active_sequence().unwrap().caption_tracks[0].captions.iter().map(|c| (secs(c.start), secs(c.end()), c.text.clone())).collect()
}

/// (clip id, start) of the clips on V1, and every caption's start.
fn v1_and_caption_starts(s: &Session) -> (Vec<(u64, Tick)>, Vec<Tick>) {
    let q = s.active_sequence().unwrap();
    (q.video_tracks[0].items.iter().map(|i| (i.id.0, i.start)).collect(), q.caption_tracks[0].captions.iter().map(|c| c.start).collect())
}

#[test]
fn timeline_move_takes_the_listed_captions_along_in_one_undo_step() {
    let mut s = demo();
    for (sec, text) in [(1.0, "early"), (9.0, "late one"), (12.0, "late two")] {
        s.execute("playhead.set", json!({"seconds": sec})).unwrap();
        s.execute("captions.add", json!({"text": text, "durationSeconds": 1.0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let ids: Vec<u64> = q.caption_tracks[0].captions.iter().map(|c| c.id.0).collect();
    let (clip, start) = (q.video_tracks[0].items[2].id, q.video_tracks[0].items[2].start);
    let (clips_before, caps_before) = v1_and_caption_starts(&s);
    let undo_before = s.history.undo.len();
    let shift = 2 * TICKS_PER_SECOND;

    // the captions named move by the clip's own offset; `early` is not named and stays
    let r = s
        .execute(
            "timeline.move",
            json!({"moves": [{"clip": clip.0, "track": "V1", "time": (start + Tick(shift)).0}], "linked": false, "captions": [ids[1], ids[2], ids[1]]}),
        )
        .unwrap();
    assert_eq!(r["captions"], json!([ids[1], ids[2]]), "each caption once");
    let (clips_after, caps_after) = v1_and_caption_starts(&s);
    assert_eq!(clips_after.iter().find(|c| c.0 == clip.0).unwrap().1, start + Tick(shift));
    assert_eq!(caps_after, vec![caps_before[0], caps_before[1] + Tick(shift), caps_before[2] + Tick(shift)]);
    assert_eq!(s.history.undo.len(), undo_before + 1, "clips and captions are one undo step");
    s.active_sequence().unwrap().check().unwrap();

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1_and_caption_starts(&s), (clips_before, caps_before), "undo restores both");
}

#[test]
fn timeline_move_with_captions_that_cannot_follow_changes_nothing() {
    let mut s = demo();
    for (sec, text) in [(2.0, "stays"), (5.0, "moves")] {
        s.execute("playhead.set", json!({"seconds": sec})).unwrap();
        s.execute("captions.add", json!({"text": text, "durationSeconds": 1.0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let moving = q.caption_tracks[0].captions[1].id.0;
    let (clip, start) = (q.video_tracks[0].items[1].id, q.video_tracks[0].items[1].start);
    let before = v1_and_caption_starts(&s);
    let undo_before = s.history.undo.len();

    // 4 s earlier would put `moves` on top of `stays`: the clip does not move either
    let back = Tick(start.0 - 4 * TICKS_PER_SECOND).max(Tick::ZERO);
    let r = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": back.0}], "linked": false, "captions": [moving]}));
    assert!(r.is_err(), "the captions cannot follow: {r:?}");
    assert_eq!(v1_and_caption_starts(&s), before);
    assert_eq!(s.history.undo.len(), undo_before, "a refused move leaves no undo step");

    // a copy leaves the originals where they are, so there is nothing for captions to follow
    let r = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V2", "time": start.0}], "copy": true, "captions": [moving]}));
    assert!(r.is_err());
    assert_eq!(v1_and_caption_starts(&s), before);

    // a caption that does not exist, or sits on a locked track, refuses the move as well
    let missing = s.execute(
        "timeline.move",
        json!({"moves": [{"clip": clip.0, "track": "V1", "time": (start + Tick(TICKS_PER_SECOND)).0}], "captions": [987_654_321u64]}),
    );
    assert!(missing.is_err());
    assert_eq!(v1_and_caption_starts(&s), before, "the clip did not move either");
    s.execute("captions.setTrack", json!({"track": "C1", "locked": true})).unwrap();
    let locked =
        s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": (start + Tick(TICKS_PER_SECOND)).0}], "captions": [moving]}));
    assert!(locked.is_err());
    assert_eq!(v1_and_caption_starts(&s), before);
    assert_eq!(s.history.undo.len(), undo_before + 1, "only the lock itself was recorded");
    s.active_sequence().unwrap().check().unwrap();
}

/// One caption at 5 s, and the second clip of V1 (its id and start).
fn caption_and_second_clip(s: &mut Session) -> (u64, filmcraft_project::ClipId, Tick) {
    s.execute("playhead.set", json!({"seconds": 5.0})).unwrap();
    s.execute("captions.add", json!({"text": "one", "durationSeconds": 1.0})).unwrap();
    let q = s.active_sequence().unwrap();
    (q.caption_tracks[0].captions[0].id.0, q.video_tracks[0].items[1].id, q.video_tracks[0].items[1].start)
}

#[test]
fn timeline_move_refuses_a_malformed_captions_list() {
    let mut s = demo();
    let (cap, clip, start) = caption_and_second_clip(&mut s);
    let before = v1_and_caption_starts(&s);
    let undo_before = s.history.undo.len();
    let later = (start + Tick(TICKS_PER_SECOND)).0;
    // an absent `captions` is an empty list; anything but an array of caption ids is refused
    for captions in [json!("x"), json!(5), json!(null), json!({"id": cap}), json!([cap, "oops"]), json!([cap, null]), json!([cap, -1]), json!([cap, 1.5])] {
        let r = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": later}], "linked": false, "captions": captions}));
        assert!(r.is_err(), "{captions} should be refused");
        assert_eq!(v1_and_caption_starts(&s), before, "{captions}: nothing moved, the clip included");
    }
    assert_eq!(s.history.undo.len(), undo_before, "refused moves leave no undo step");
    s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": later}], "linked": false})).unwrap();
}

#[test]
fn timeline_move_checks_the_captions_even_when_the_clips_keep_their_time() {
    let mut s = demo();
    let (cap, clip, start) = caption_and_second_clip(&mut s);
    let before = v1_and_caption_starts(&s);
    let same_time =
        |track: &str, captions: serde_json::Value| json!({"moves": [{"clip": clip.0, "track": track, "time": start.0}], "linked": false, "captions": captions});

    // a clip that only changes track gives the captions an offset of zero, but an unknown id is still refused
    assert!(s.execute("timeline.move", same_time("V2", json!([987_654_321u64]))).is_err());
    assert_eq!(v1_and_caption_starts(&s), before);
    // a real caption is accepted, and is not reported as moved: it did not
    let r = s.execute("timeline.move", same_time("V2", json!([cap]))).unwrap();
    assert!(r.get("captions").is_none(), "no caption moved: {r}");
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions[0].start, before.1[0]);
    // a locked caption track is refused at an offset of zero too
    s.execute("captions.setTrack", json!({"track": "C1", "locked": true})).unwrap();
    assert!(s.execute("timeline.move", same_time("V1", json!([cap]))).is_err());
    assert_eq!(s.active_sequence().unwrap().video_tracks[1].items.iter().filter(|i| i.id == clip).count(), 1, "the clip stayed on V2");
    // when they do move, the reply lists them
    s.execute("captions.setTrack", json!({"track": "C1", "locked": false})).unwrap();
    let later = (start + Tick(TICKS_PER_SECOND)).0;
    let r = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V2", "time": later}], "linked": false, "captions": [cap]})).unwrap();
    assert_eq!(r["captions"], json!([cap]), "{r}");
}

/// An insert already carries the unlocked, sync-locked caption tracks (once), so naming captions as
/// well would shift them a second time: the combination is refused.
#[test]
fn timeline_move_with_insert_leaves_sync_locked_captions_to_the_insert() {
    let mut s = demo();
    let (cap, clip, _) = caption_and_second_clip(&mut s);
    let before = v1_and_caption_starts(&s);
    let undo_before = s.history.undo.len();
    // inserted at the start, the clip lands before the caption, which the insert pushes later
    let to = 0;
    let r = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": to}], "linked": false, "insert": true, "captions": [cap]}));
    assert!(r.is_err(), "insert and captions together are refused");
    assert_eq!(v1_and_caption_starts(&s), before);
    assert_eq!(s.history.undo.len(), undo_before);
    // the insert alone is unchanged, and still moves the sync-locked caption itself
    s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V1", "time": to}], "linked": false, "insert": true})).unwrap();
    assert_ne!(s.active_sequence().unwrap().caption_tracks[0].captions[0].start, before.1[0], "the insert shifted the caption on its own");
}

#[test]
fn captions_duplicate_copies_and_overwrites() {
    let mut s = demo();
    for (sec, text) in [(1.0, "one"), (3.0, "two")] {
        s.execute("playhead.set", json!({"seconds": sec})).unwrap();
        s.execute("captions.add", json!({"text": text, "durationSeconds": 1.0})).unwrap();
    }
    let one = s.active_sequence().unwrap().caption_tracks[0].captions[0].id;
    // Option-drag: a copy 5 s later, the original stays; the copy is selected
    let r = s.execute("captions.duplicate", json!({"captions": [one.0], "delta": 5 * TICKS_PER_SECOND})).unwrap();
    assert_eq!(caption_spans(&s), vec![(1.0, 2.0, "one".into()), (3.0, 4.0, "two".into()), (6.0, 7.0, "one".into())]);
    assert_eq!(s.state.caption_selection.len(), 1);
    assert_eq!(r["captions"][0].as_u64(), Some(s.state.caption_selection[0].0));
    // a copy dropped half over `two` overwrites that half
    s.execute("captions.duplicate", json!({"captions": [one.0], "delta": TICKS_PER_SECOND * 3 / 2})).unwrap();
    assert_eq!(caption_spans(&s)[1..3], [(2.5, 3.5, "one".into()), (3.5, 4.0, "two".into())]);
    // never before zero; zero delta is nothing
    s.execute("captions.duplicate", json!({"captions": [one.0], "delta": -100 * TICKS_PER_SECOND})).unwrap();
    assert_eq!(caption_spans(&s)[0], (0.0, 1.0, "one".into()));
    assert!(s.execute("captions.duplicate", json!({"captions": [one.0], "delta": 0})).is_err());
    assert!(s.execute("captions.duplicate", json!({"captions": [one.0]})).is_err(), "needs a delta");
    assert!(s.execute("captions.duplicate", json!({"captions": [], "delta": TICKS_PER_SECOND})).is_err(), "needs a caption");
    let before = s.active_sequence().unwrap().caption_tracks[0].captions.len();
    s.execute("captions.duplicate", json!({"captions": [one.0, one.0], "delta": 20 * TICKS_PER_SECOND})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions.len(), before + 1, "a caption named twice is copied once");
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn captions_copy_cut_and_paste() {
    let mut s = demo();
    // Copy / Paste with only captions selected copies captions, pasted at the playhead
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("captions.add", json!({"text": "hi", "durationSeconds": 1.0})).unwrap();
    assert!(s.state.selection.is_empty());
    assert_eq!(s.execute("edit.copy", json!({})).unwrap()["copied"], 1);
    s.execute("playhead.set", json!({"seconds": 10.0})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(caption_spans(&s), vec![(1.0, 2.0, "hi".into()), (10.0, 11.0, "hi".into())]);
    assert!((s.playhead().seconds() - 11.0).abs() < 0.05, "the playhead moves past the paste");
    // Cut takes it away and Paste puts it back
    s.execute("edit.cut", json!({})).unwrap();
    assert_eq!(caption_spans(&s), vec![(1.0, 2.0, "hi".into())]);
    s.execute("playhead.set", json!({"seconds": 4.0})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(caption_spans(&s)[1], (4.0, 5.0, "hi".into()));
    // copying clips afterwards makes Paste place clips again, not captions
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.caption_clipboard.is_empty() && !s.state.clipboard.is_empty());
    // undo takes back a caption paste as one step
    let mut s = demo();
    s.execute("captions.add", json!({"text": "x", "durationSeconds": 1.0})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 5.0})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(caption_spans(&s).len(), 1);
}

#[test]
fn set_style_font_style() {
    let mut s = demo();
    s.execute("captions.add", json!({"text": "Styled", "seconds": 1.0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].style.font_style, "SemiBold");
    s.execute("captions.setStyle", json!({"fontStyle": " Medium "})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].style.font_style, "Medium");
    assert_eq!(s.execute("captions.list", json!({})).unwrap()["tracks"][0]["style"]["fontStyle"], "Medium");
    s.execute("captions.setStyle", json!({"reset": true})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].style.font_style, "SemiBold");
}

/// The caption track language reaches TTML, and import can set it (#4025).
#[test]
fn caption_track_language_survives_import_and_ttml_export() {
    let dir = std::env::temp_dir().join(format!("fc-cap-lang-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let srt = dir.join("pt.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nOlá\n").unwrap();
    let ttml = dir.join("out.ttml").to_string_lossy().to_string();
    let mut s = demo();
    let r = s.execute("captions.import", json!({"path": srt.to_string_lossy()})).unwrap();
    s.execute("captions.setTrack", json!({"track": "C1", "language": "pt-BR"})).unwrap();
    s.execute("captions.export", json!({"path": ttml, "track": "C1"})).unwrap();
    let xml = std::fs::read_to_string(&ttml).unwrap();
    assert!(xml.contains(r#"xml:lang="pt-BR""#), "{xml}");
    assert_eq!(r["language"], "en", "an SRT says nothing of its language: {r}");
    // import takes a language, and a TTML file brings its own
    let r = s.execute("captions.import", json!({"path": srt.to_string_lossy(), "language": "es-419"})).unwrap();
    assert_eq!(r["language"], "es-419");
    let r = s.execute("captions.import", json!({"path": ttml})).unwrap();
    assert_eq!(r["language"], "pt-BR");
    let list = s.execute("captions.list", json!({})).unwrap();
    let langs: Vec<&str> = list["tracks"].as_array().unwrap().iter().map(|t| t["language"].as_str().unwrap()).collect();
    assert_eq!(langs, ["pt-BR", "es-419", "pt-BR"], "new tracks go on top: {list}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `captions.export` lists the characters an SCC or MCC file cannot carry (#4023).
#[test]
fn caption_export_warns_about_characters_608_and_708_cannot_carry() {
    let dir = std::env::temp_dir().join(format!("fc-cap-charset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = demo();
    s.execute("captions.newTrack", json!({"format": "CEA-608", "language": "pt-BR"})).unwrap();
    s.execute("captions.add", json!({"text": "Espere… isso é “demais” — não?", "seconds": 0.0, "durationSeconds": 2.0})).unwrap();
    let export = |s: &mut Session, ext: &str| s.execute("captions.export", json!({"path": dir.join(format!("cc.{ext}")).to_string_lossy()})).unwrap();
    let ellipsis = "CEA-608 has no \"…\" (U+2026): dropped once, first in the caption at 00:00:00;00";
    assert_eq!(export(&mut s, "scc")["warnings"], json!([ellipsis]));
    let em_dash = "CEA-708 has no \"—\" (U+2014): written as \"?\" once, first in the caption at 00:00:00;00";
    assert_eq!(export(&mut s, "mcc")["warnings"], json!([ellipsis, em_dash]));
    assert_eq!(export(&mut s, "srt")["warnings"], json!([]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caption_track_styles_copy_paste_and_saved_presets_are_undoable_and_persisted() {
    let dir = std::env::temp_dir().join(format!("fc-caption-style-presets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = demo();
    s.caption_style_presets.set_dir(&dir);
    let first = s.execute("captions.newTrack", json!({"name": "First"})).unwrap()["track"].as_u64().unwrap();
    let second = s.execute("captions.newTrack", json!({"name": "Second"})).unwrap()["track"].as_u64().unwrap();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("captions.add", json!({"track": first, "text": "Keep my timing"})).unwrap();
    s.execute("playhead.set", json!({"seconds": 4.0})).unwrap();
    s.execute("captions.add", json!({"track": second, "text": "Also keep mine"})).unwrap();
    s.execute("captions.setStyle", json!({"track": first, "font": "Inter", "size": 42.0, "color": "#22aaff", "anchor": "top"})).unwrap();
    let first_style = s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == first).unwrap().style.clone();
    let second_captions = s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == second).unwrap().captions.clone();

    s.execute("captions.copyStyle", json!({"track": first})).unwrap();
    s.execute("captions.setStyle", json!({"track": second, "size": 24.0, "anchor": "bottom"})).unwrap();
    s.execute("captions.pasteStyle", json!({"track": second})).unwrap();
    let target = s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == second).unwrap();
    assert_eq!(target.style, first_style);
    assert_eq!(target.captions, second_captions, "style paste preserves caption text and timing");
    s.execute("edit.undo", json!({})).unwrap();
    let target = s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == second).unwrap();
    assert_eq!(target.style.size, 24.0, "pasting a style is one undo step");

    s.execute("captions.stylePreset.save", json!({"track": first, "name": "Broadcast Blue"})).unwrap();
    s.execute("captions.setStyle", json!({"track": first, "size": 18.0})).unwrap();
    s.execute("captions.stylePreset.apply", json!({"track": first, "name": "Broadcast Blue"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == first).unwrap().style, first_style);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks.iter().find(|t| t.id.0 == first).unwrap().style.size, 18.0);
    let list = s.execute("captions.stylePreset.list", json!({})).unwrap();
    assert_eq!(list["presets"][0]["name"], "Broadcast Blue");

    let mut loaded = crate::captions::CaptionStyleLibrary::default();
    loaded.set_dir(&dir);
    assert_eq!(loaded.find("Broadcast Blue"), Some(first_style));
    assert!(loaded.delete("Broadcast Blue").unwrap());
    assert!(loaded.find("Broadcast Blue").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caption_style_preset_commands_reject_hostile_parameters_without_mutation() {
    let mut s = demo();
    s.execute("captions.newTrack", json!({"name": "Hostile test"})).unwrap();
    let original_style = s.active_sequence().unwrap().caption_tracks[0].style.clone();
    for name in [String::new(), "x".repeat(101), "line\nbreak".to_string()] {
        assert!(s.execute("captions.stylePreset.save", json!({"track": "C1", "name": name})).is_err());
    }
    assert!(s.execute("captions.stylePreset.apply", json!({"track": "C1", "name": "missing"})).is_err());
    assert!(s.execute("captions.stylePreset.delete", json!({"name": "missing"})).is_err());
    assert!(s.execute("captions.pasteStyle", json!({"track": "C1"})).is_err());
    assert!(s.caption_style_presets.all().is_empty());
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].style, original_style);
}

#[test]
fn caption_style_preset_save_does_not_replace_a_file_that_failed_to_load() {
    let dir = std::env::temp_dir().join(format!("fc-caption-style-presets-invalid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("caption-style-presets.json");
    let original = b"not a valid preset file";
    std::fs::write(&path, original).unwrap();

    let mut library = crate::captions::CaptionStyleLibrary::default();
    library.set_dir(&dir);
    assert!(library.save("Replacement", Default::default()).is_err());
    assert!(library.all().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caption_style_preset_save_and_load_share_unicode_name_normalization() {
    let dir = std::env::temp_dir().join(format!("fc-caption-style-presets-unicode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut library = crate::captions::CaptionStyleLibrary::default();
    library.set_dir(&dir);
    library.save("Café", Default::default()).unwrap();
    library.save("CAFÉ", Default::default()).unwrap();
    assert_eq!(library.all().len(), 1);

    let mut loaded = crate::captions::CaptionStyleLibrary::default();
    loaded.set_dir(&dir);
    assert_eq!(loaded.all().len(), 1);
    assert!(loaded.find("CAFÉ").is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caption_style_preset_save_requires_a_persistence_directory() {
    let mut library = crate::captions::CaptionStyleLibrary::default();
    assert!(library.save("Temporary", Default::default()).is_err());
    assert!(library.all().is_empty());

    let mut session = demo();
    session.execute("captions.newTrack", json!({"name": "No persistence"})).unwrap();
    assert!(session.execute("captions.stylePreset.save", json!({"track": "C1", "name": "Temporary"})).is_err());
    assert!(session.caption_style_presets.all().is_empty());
}
