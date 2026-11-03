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

/// SCC and MCC time codes count from the sequence's start time code, as EDL and marker exports do,
/// so the captions line up with a master that starts at 01:00:00;00.
#[test]
fn scc_and_mcc_export_count_from_the_sequence_start_timecode() {
    let dir = std::env::temp_dir().join(format!("fc-cap-tc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Master", "fps": 29.97, "video": 1, "audio": 0})).unwrap();
    s.edit_sequence("Timing", |q, _, _| {
        q.settings.drop_frame = true;
        q.start_timecode = 107_892; // 01:00:00;00
        Ok(())
    })
    .unwrap();
    s.execute("playhead.set", json!({"seconds": 2.0})).unwrap();
    s.execute("captions.add", json!({"text": "Hello"})).unwrap();
    let rate = filmcraft_captions::scc::RATE;
    let start = s.active_sequence().unwrap().caption_tracks[0].captions[0].start;
    let expected = rate.tick_of(107_892 + rate.frame_at(rate.snap_nearest(start)));
    for ext in ["scc", "mcc"] {
        let path = dir.join(format!("master.{ext}")).to_string_lossy().to_string();
        s.execute("captions.export", json!({"path": path})).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let first = text.lines().find_map(|l| l.split_once('\t')).map(|(tc, _)| tc.to_string()).unwrap();
        assert!(first.starts_with("01:00:0"), "{ext}: the first time code is {first}");
        let back = filmcraft_captions::parse(&bytes, filmcraft_captions::Format::from_name(ext).unwrap()).unwrap();
        assert_eq!(back.cues[0].start, expected, "{ext}: the caption keeps its place after the start time code");
    }
    // An export's sidecar counts from the time code of its first frame, as an MXF export does.
    let out = dir.join("range.wav").to_string_lossy().to_string();
    let range = json!({"range": "custom", "startTime": rate.tick_of(30).0, "endTime": rate.tick_of(150).0});
    let mut params = json!({"format": "wav", "path": out, "captionSidecar": "scc", "wait": true});
    params.as_object_mut().unwrap().extend(range.as_object().unwrap().clone());
    s.execute("file.exportMedia", params).unwrap();
    let back = filmcraft_captions::parse(&std::fs::read(dir.join("range.scc")).unwrap(), filmcraft_captions::Format::Scc).unwrap();
    assert_eq!(back.cues[0].start, expected, "the sidecar keeps the caption at its time code");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sequence at `fps` whose time code starts at frame `start` (drop-frame when the rate has it),
/// with a caption at `seconds`; returns the session and the caption's start.
fn timecoded_sequence(fps: f64, start: i64, seconds: f64) -> (Session, Tick) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Master", "fps": fps, "video": 1, "audio": 0})).unwrap();
    s.edit_sequence("Timing", |q, _, _| {
        q.settings.drop_frame = q.settings.frame_rate.supports_drop_frame();
        q.start_timecode = start;
        Ok(())
    })
    .unwrap();
    s.execute("playhead.set", json!({"seconds": seconds})).unwrap();
    s.execute("captions.add", json!({"text": "Hello"})).unwrap();
    let at = s.active_sequence().unwrap().caption_tracks[0].captions[0].start;
    (s, at)
}

/// Re-importing an SCC or MCC export counts its time codes back from the sequence's start time
/// code, so the caption lands where it was; a file that counts from zero keeps its times.
#[test]
fn scc_and_mcc_import_count_from_the_sequence_start_timecode() {
    let dir = std::env::temp_dir().join(format!("fc-cap-tc-in-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, start) = timecoded_sequence(29.97, 107_892, 2.0); // 01:00:00;00
    for ext in ["scc", "mcc"] {
        for df in [true, false] {
            let path = dir.join(format!("master-{df}.{ext}")).to_string_lossy().to_string();
            s.execute("captions.export", json!({"path": path, "dropFrame": df})).unwrap();
            let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
            assert_eq!(r["documents"][0]["warnings"], json!([]), "{r}");
            let back = s.active_sequence().unwrap().caption_tracks[0].captions[0].start;
            assert_eq!(back, start, "{ext}, drop-frame {df}: the caption lands where it was");
        }
    }
    // The same caption in a file that counts from 00:00:00;00 keeps its time, with a warning.
    let zero = dir.join("zero.scc");
    std::fs::write(&zero, "Scenarist_SCC V1.0\n\n00:00:01;19\t9420 9420 94ae 94ae 9476 9476 97a1 97a1 c8e5 ecec ef80 942f 942f\n\n00:00:05;00\t942c 942c\n\n")
        .unwrap();
    let r = s.execute("file.import", json!({"paths": [zero.to_string_lossy()]})).unwrap();
    let warnings = r["documents"][0]["warnings"].to_string();
    assert!(warnings.contains("start before the sequence's start timecode 01;00;00;00"), "{r}");
    let kept = filmcraft_captions::scc::RATE.tick_of(60); // 00:00:02;00, where the file shows it
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions[0].start, kept);
    let _ = std::fs::remove_dir_all(&dir);
}

/// On a 25 fps sequence that starts at 01:00:00:00, a caption at 2 s carries the label an MXF
/// export gives that frame, 01:00:02:00, in SCC and MCC, drop-frame or not. A ranged sidecar
/// agrees with `captions.export` frame for frame, even where 25 and 29.97 fps frames don't line up.
#[test]
fn scc_and_mcc_keep_the_start_label_on_other_rates() {
    let dir = std::env::temp_dir().join(format!("fc-cap-tc-25-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let scc = filmcraft_captions::scc::RATE;
    let first = |path: &std::path::Path, ext: &str| {
        let doc = filmcraft_captions::parse(&std::fs::read(path).unwrap(), filmcraft_captions::Format::from_name(ext).unwrap()).unwrap();
        doc.cues[0].start
    };
    let (mut s, _) = timecoded_sequence(25.0, 90_000, 2.0); // 01:00:00:00
    for ext in ["scc", "mcc"] {
        for df in [true, false] {
            let path = dir.join(format!("master-{df}.{ext}"));
            s.execute("captions.export", json!({"path": path.to_string_lossy(), "dropFrame": df})).unwrap();
            let label = filmcraft_time::format_timecode_frames(scc.frame_at(first(&path, ext)), scc, df);
            assert_eq!(label, if df { "01;00;02;00" } else { "01:00:02:00" }, "{ext}, drop-frame {df}");
        }
    }
    // 1.12 s from a range that starts at 0.04 s: 32.4 frames at 29.97 from the range start, 33.6
    // from the sequence start
    let (mut s, _) = timecoded_sequence(25.0, 90_000, 1.12);
    let direct = dir.join("direct.scc");
    s.execute("captions.export", json!({"path": direct.to_string_lossy(), "dropFrame": false})).unwrap();
    let out = dir.join("range.wav").to_string_lossy().to_string();
    let range = json!({"range": "custom", "startTime": TICKS_PER_SECOND / 25, "endTime": 3 * TICKS_PER_SECOND});
    let mut params = json!({"format": "wav", "path": out, "captionSidecar": "scc", "wait": true});
    params.as_object_mut().unwrap().extend(range.as_object().unwrap().clone());
    s.execute("file.exportMedia", params).unwrap();
    assert_eq!(first(&dir.join("range.scc"), "scc"), first(&direct, "scc"), "the sidecar and captions.export agree");
    let _ = std::fs::remove_dir_all(&dir);
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
