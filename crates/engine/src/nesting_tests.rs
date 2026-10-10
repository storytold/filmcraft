//! Nested sequences: a sequence can never contain itself, whichever
//! command is asked to put it there, and Nest… keeps what was selected.

use super::*;
use filmcraft_project::TrackKind;
use filmcraft_time::TimeRange;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip_ids(s: &Session, kind: TrackKind, idx: usize) -> Vec<u64> {
    s.active_sequence().unwrap().tracks(kind)[idx].items.iter().map(|i| i.id.0).collect()
}

/// Nest the second V1 clip of the active sequence; returns (outer sequence, nested sequence).
fn nest_one(s: &mut Session, name: &str) -> (ItemId, ItemId) {
    let outer = s.state.active_sequence.unwrap();
    let v = clip_ids(s, TrackKind::Video, 0)[1];
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    let r = s.execute("clip.nest", json!({"name": name})).unwrap();
    (outer, ItemId(r["sequence"].as_u64().unwrap()))
}

/// The command fails with the self-nesting message and changes nothing (project and undo history).
fn refused(s: &mut Session, command: &str, params: serde_json::Value) {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute(command, params).expect_err(command).to_string();
    assert!(e.contains("cannot be nested inside itself"), "{command}: {e}");
    assert!(Arc::ptr_eq(&before, &s.project), "{command} left the project alone");
    assert_eq!(s.history.undo.len(), undo, "{command} added no undo step");
    assert_eq!(s.project.nest_cycle(), None);
}

#[test]
fn a_sequence_cannot_be_placed_in_itself() {
    let mut s = demo();
    let a = s.state.active_sequence.unwrap();
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0}));
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0, "insert": true}));
    // from the Source monitor as well
    s.execute("source.open", json!({"item": a.0})).unwrap();
    refused(&mut s, "source.insert", json!({}));
    refused(&mut s, "source.overwrite", json!({}));
}

#[test]
fn a_sequence_cannot_be_placed_in_a_sequence_it_contains() {
    let mut s = demo();
    let (outer, nested) = nest_one(&mut s, "Inner");
    // outer holds Inner: Inner cannot take outer, nor itself
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "timeline.place", json!({"item": outer.0, "seconds": 0.0}));
    refused(&mut s, "timeline.place", json!({"item": nested.0, "seconds": 0.0}));
    // one level further: a third sequence that holds outer cannot go into Inner either
    s.execute("sequence.open", json!({"item": outer.0})).unwrap();
    let third = ItemId(s.execute("file.newSequence", json!({"name": "Third"})).unwrap()["sequence"].as_u64().unwrap());
    s.execute("sequence.open", json!({"item": third.0})).unwrap();
    s.execute("timeline.place", json!({"item": outer.0, "seconds": 0.0})).expect("a nest of a nest is fine");
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "timeline.place", json!({"item": third.0, "seconds": 0.0}));
    // the other direction stays allowed
    s.execute("sequence.open", json!({"item": third.0})).unwrap();
    s.execute("timeline.place", json!({"item": nested.0, "seconds": 30.0})).expect("Inner may be used twice");
}

#[test]
fn pasting_a_nest_into_its_own_sequence_is_refused() {
    let mut s = demo();
    let (_, nested) = nest_one(&mut s, "Inner");
    // copy the nest clip, open the nested sequence and paste it there
    let nest = s.active_sequence().unwrap().video_tracks[0].items.iter().find(|i| i.item == nested).unwrap().id;
    s.execute("timeline.select", json!({"clips": [nest.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "edit.paste", json!({}));
    refused(&mut s, "edit.pasteInsert", json!({}));
}

#[test]
fn a_project_opened_with_a_cycle_can_still_be_edited_and_repaired() {
    let mut s = demo();
    let a = s.state.active_sequence.unwrap();
    // what a damaged project file would hold: the sequence on its own track
    let mut p = (*s.project).clone();
    let rate = p.sequence(a).unwrap().settings.frame_rate;
    let it = p.make_track_item(a, TrackKind::Video, Tick(900 * filmcraft_time::TICKS_PER_SECOND), TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    let bad = it.id;
    p.sequence_mut(a).unwrap().video_tracks[0].items.push(it);
    s.project = Arc::new(p);
    assert_eq!(s.project.nest_cycle(), Some(a));
    // unrelated edits still work
    let first = clip_ids(&s, TrackKind::Video, 0)[0];
    s.execute("timeline.select", json!({"clips": [first]})).unwrap();
    s.execute("clip.enable", json!({})).expect("edits are not locked out");
    // and removing the clip repairs it, after which the guard is back
    s.execute("timeline.select", json!({"clips": [bad.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(s.project.nest_cycle(), None);
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0}));
}

/// The first V1 transition that joins two clips: (transition, outgoing clip, incoming clip).
fn joining_transition(s: &Session) -> (filmcraft_project::Transition, ClipId, ClipId) {
    let v1 = &s.active_sequence().unwrap().video_tracks[0];
    let t = v1.transitions.iter().find(|t| t.from.is_some() && t.to.is_some()).expect("the demo has a transition between two clips").clone();
    let (from, to) = (t.from.unwrap(), t.to.unwrap());
    (t, from, to)
}

fn frame_at(s: &Session, t: Tick) -> filmcraft_render::Image {
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, ..Default::default() };
    filmcraft_render::render_sequence(&s.project, s.state.active_sequence.unwrap(), t, opts, &provider).unwrap()
}

#[test]
fn nest_keeps_transitions_track_names_and_channel_layouts() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    s.edit_sequence("setup", |q, _, _| {
        q.video_tracks[0].name = "Picture".into();
        q.audio_tracks[0].name = "Dialogue".into();
        q.audio_tracks[0].channels = filmcraft_project::AudioChannels::Mono;
        Ok(())
    })
    .unwrap();
    let (trn, from, to) = joining_transition(&s);
    let mid = trn.start + Tick(trn.duration.0 / 2);
    let before = frame_at(&s, mid);
    let start = s.active_sequence().unwrap().find_item(from).unwrap().1.start;
    let audio_transitions = s.active_sequence().unwrap().audio_tracks[0].transitions.len();
    s.execute("timeline.select", json!({"clips": [from.0, to.0]})).unwrap();
    let linked: Vec<ClipId> = s.state.selection.clone();
    let nested = ItemId(s.execute("clip.nest", json!({"name": "Inner"})).unwrap()["sequence"].as_u64().unwrap());

    let q = s.project.sequence(nested).unwrap();
    q.check().unwrap();
    // the transition came along, on the same track, at the same place relative to its clips
    let inner = q.video_tracks[0].transitions.iter().find(|t| t.id == trn.id).expect("the transition is inside the nest");
    assert_eq!((inner.start, inner.duration, inner.from, inner.to), (trn.start - start, trn.duration, Some(from), Some(to)));
    // so did the one between the linked sound clips, if the demo has it
    let inner_audio =
        q.audio_tracks[0].transitions.iter().filter(|t| t.from.is_some_and(|c| linked.contains(&c)) && t.to.is_some_and(|c| linked.contains(&c))).count();
    let outer_q = s.project.sequence(outer).unwrap();
    assert_eq!(outer_q.audio_tracks[0].transitions.len() + inner_audio, audio_transitions, "audio transitions moved, none lost");
    // tracks are laid out like the parent's
    assert_eq!(q.video_tracks[0].name, "Picture");
    assert_eq!(q.audio_tracks[0].name, "Dialogue");
    assert_eq!(q.audio_tracks[0].channels, filmcraft_project::AudioChannels::Mono);
    // and there are only as many as the nested clips need (Premiere: V1 + A1 here, not the parent's three each)
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 1));
    // the parent no longer holds the transition, and shows the same picture through the nest
    assert!(outer_q.video_tracks[0].transitions.iter().all(|t| t.id != trn.id));
    outer_q.check().unwrap();
    let after = frame_at(&s, mid);
    assert_eq!((before.w, before.h), (after.w, after.h));
    let worst = before.px.iter().zip(&after.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(worst < 0.01, "the frame inside the transition is unchanged by nesting (worst channel difference {worst})");
}

#[test]
fn nest_leaves_transitions_to_clips_outside_the_selection_out() {
    let mut s = demo();
    let (trn, from, _to) = joining_transition(&s);
    // only the outgoing clip is nested: the transition needs both, so it is not carried
    s.execute("timeline.select", json!({"clips": [from.0]})).unwrap();
    let nested = ItemId(s.execute("clip.nest", json!({"name": "Half"})).unwrap()["sequence"].as_u64().unwrap());
    let q = s.project.sequence(nested).unwrap();
    q.check().unwrap();
    assert!(q.all_tracks().all(|t| t.transitions.iter().all(|t| t.id != trn.id)));
    s.active_sequence().unwrap().check().unwrap();
}

/// Nest the 4th and 5th V1 clips (with their sound) as "Inner". Returns the nested sequence, the
/// nest's video clip in the outer sequence, and the two nested clips' ids and lengths.
fn nest_two(s: &mut Session) -> (ItemId, ClipId, [(ClipId, Tick); 2]) {
    let v1 = &s.active_sequence().unwrap().video_tracks[0];
    let pair = [(v1.items[3].id, v1.items[3].duration), (v1.items[4].id, v1.items[4].duration)];
    s.execute("timeline.select", json!({"clips": [pair[0].0.0, pair[1].0.0]})).unwrap();
    let nested = ItemId(s.execute("clip.nest", json!({"name": "Inner"})).unwrap()["sequence"].as_u64().unwrap());
    let nest = s.active_sequence().unwrap().video_tracks[0].items.iter().find(|i| i.item == nested).unwrap().id;
    (nested, nest, pair)
}

fn nest_clip(s: &Session, id: ClipId) -> filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

fn mix_at(s: &Session, t: Tick, frames: usize) -> filmcraft_frame::AudioBuffer {
    let q = s.active_sequence().unwrap();
    let sr = q.settings.sample_rate as i64;
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    filmcraft_render::audio::mix_sequence(&s.project, q, t.to_units_floor(sr), frames, &provider)
}

#[test]
fn a_nest_keeps_its_length_when_its_sequence_gets_shorter() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    let (nested, nest, [(_, first), (second_id, second)]) = nest_two(&mut s);
    let whole = nest_clip(&s, nest);
    assert_eq!(whole.duration, first + second);
    assert_eq!(s.project.nest_overhang(&whole), None);
    // remove the second clip inside the nest
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    s.execute("timeline.select", json!({"clips": [second_id.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), first);
    s.execute("sequence.open", json!({"item": outer.0})).unwrap();
    // the nest is as long as before; its end is now past the contents
    let c = nest_clip(&s, nest);
    assert_eq!(c.range(), whole.range());
    let empty = s.project.nest_overhang(&c).expect("the end of the nest is empty");
    assert_eq!(empty, TimeRange::new(c.start + first, second));
    // there it shows and plays what the sequence would without the nest
    let t = empty.start + Tick(empty.duration.0 / 2);
    let (with_frame, with_mix) = (frame_at(&s, t), mix_at(&s, t, 4800));
    s.execute("timeline.select", json!({"clips": [nest.0]})).unwrap();
    s.execute("clip.enable", json!({})).unwrap();
    let (without_frame, without_mix) = (frame_at(&s, t), mix_at(&s, t, 4800));
    assert!(with_frame.px == without_frame.px, "nothing to see past the contents");
    assert_eq!(with_mix.channels, without_mix.channels, "nothing to hear past the contents");
    s.execute("clip.enable", json!({})).unwrap();
    // it cannot be trimmed out further, and trimming the empty part off leaves a sound nest
    s.execute("timeline.trim", json!({"clip": nest.0, "edge": "out", "deltaFrames": 12})).ok();
    assert!(nest_clip(&s, nest).duration <= whole.duration, "no more empty time is added");
    let back = nest_clip(&s, nest).duration - first;
    s.execute("timeline.trim", json!({"clip": nest.0, "edge": "out", "delta": -back.0})).unwrap();
    let c = nest_clip(&s, nest);
    assert_eq!(c.duration, first);
    assert_eq!(s.project.nest_overhang(&c), None);
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn a_nest_can_be_trimmed_out_to_reveal_what_its_sequence_gained() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    let rate = s.sequence_rate();
    let (nested, nest, [(_, first), (_, second)]) = nest_two(&mut s);
    let whole = nest_clip(&s, nest);
    // make room after the nest in the outer sequence: drop everything that follows it on V1/A1
    let later: Vec<u64> = s.active_sequence().unwrap().video_tracks[0].items.iter().filter(|i| i.start >= whole.end()).map(|i| i.id.0).collect();
    s.execute("timeline.select", json!({"clips": later})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    // add 48 frames of media to the end of the nested sequence
    let media = s.project.items.values().find(|i| i.name == "Desert_Dunes.mp4").unwrap().id;
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    s.execute("timeline.place", json!({"item": media.0, "time": (first + second).0, "duration": rate.tick_of(48).0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), first + second + rate.tick_of(48));
    s.execute("sequence.open", json!({"item": outer.0})).unwrap();
    // the nest did not grow by itself
    assert_eq!(nest_clip(&s, nest).range(), whole.range());
    // trimming reveals the new material, up to the end of the contents and no further
    s.execute("timeline.trim", json!({"clip": nest.0, "edge": "out", "deltaFrames": 10})).unwrap();
    assert_eq!(nest_clip(&s, nest).duration, whole.duration + rate.tick_of(10));
    s.execute("timeline.trim", json!({"clip": nest.0, "edge": "out", "deltaFrames": 500})).ok();
    let c = nest_clip(&s, nest);
    assert_eq!(c.duration, whole.duration + rate.tick_of(48));
    assert_eq!(s.project.nest_overhang(&c), None);
}

#[test]
fn the_outer_sequence_shows_changes_made_inside_a_nest() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    let (nested, nest, [(first_id, first), _]) = nest_two(&mut s);
    let t = nest_clip(&s, nest).start + Tick(first.0 / 2);
    let before = frame_at(&s, t);
    let revision = s.revision;
    // disable the clip that is on screen, inside the nest
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    s.execute("timeline.select", json!({"clips": [first_id.0]})).unwrap();
    s.execute("clip.enable", json!({})).unwrap();
    s.execute("sequence.open", json!({"item": outer.0})).unwrap();
    assert_ne!(s.revision, revision, "frames cached for the outer sequence are stale");
    let after = frame_at(&s, t);
    assert!(before.px != after.px, "the outer sequence shows the nest's new contents");
}

// ---- Nest… as Premiere Pro 26.5.2 does it (observed in the app, 2026-10-06)

#[test]
fn after_nest_nothing_is_selected_and_the_new_sequence_is_selected_in_the_project() {
    let mut s = demo();
    let v = clip_ids(&s, TrackKind::Video, 0)[1];
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    let r = s.execute("clip.nest", json!({"name": "Inner"})).unwrap();
    let nested = ItemId(r["sequence"].as_u64().unwrap());
    assert!(s.state.selection.is_empty());
    assert_eq!(s.state.project_selection, vec![nested]);
    // the result names the nest's clips: one picture and one sound clip, linked, where the clips were
    let clips: Vec<ClipId> = r["clips"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())).collect();
    let q = s.active_sequence().unwrap();
    let (vt, vc) = q.find_item(clips[0]).unwrap();
    let (at, ac) = q.find_item(clips[1]).unwrap();
    assert_eq!((vt, at), (q.video_tracks[0].id, q.audio_tracks[0].id));
    assert!(vc.item == nested && ac.item == nested && vc.link.is_some() && vc.link == ac.link);
    // each shows the nested sequence from its start, for the length of what was nested
    let inner = s.project.sequence(nested).unwrap().duration();
    assert_eq!((vc.source_in, vc.duration, ac.source_in, ac.duration), (Tick::ZERO, inner, Tick::ZERO, inner));
    assert_eq!(clips.len(), 2);
}

/// Move the sound of V1 clip `n` alone to audio track `track` ("A3"), keeping its time.
fn move_sound(s: &mut Session, n: usize, track: &str) -> (filmcraft_project::TrackItem, filmcraft_project::TrackItem) {
    let q = s.active_sequence().unwrap();
    let picture = q.video_tracks[0].items[n].clone();
    let sound = q.all_tracks().flat_map(|t| t.items.iter()).find(|i| i.id != picture.id && i.link == picture.link).unwrap().clone();
    s.execute("timeline.move", json!({"moves": [{"clip": sound.id.0, "track": track, "time": sound.start.0}], "linked": false})).unwrap();
    let sound = s.active_sequence().unwrap().find_item(sound.id).unwrap().1.clone();
    (picture, sound)
}

fn nest_of(s: &mut Session, clips: &[ClipId], name: &str) -> (ItemId, Vec<ClipId>) {
    s.execute("timeline.select", json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>()})).unwrap();
    let r = s.execute("clip.nest", json!({"name": name})).unwrap();
    (ItemId(r["sequence"].as_u64().unwrap()), r["clips"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())).collect())
}

fn track_index(s: &Session, kind: TrackKind, clip: ClipId) -> usize {
    let q = s.active_sequence().unwrap();
    let (track, _) = q.find_item(clip).unwrap();
    q.tracks(kind).iter().position(|t| t.id == track).unwrap()
}

#[test]
fn clips_on_two_video_tracks_with_their_sound_on_one_track_nest_as_a_linked_pair() {
    let mut s = demo();
    // two neighbouring V1 clips with their sound on A1; the second one's picture goes up to V2
    let (first, second) = {
        let v1 = &s.active_sequence().unwrap().video_tracks[0].items;
        (v1[4].clone(), v1[5].clone())
    };
    s.execute("timeline.move", json!({"moves": [{"clip": second.id.0, "track": "V2", "time": second.start.0}], "linked": false})).unwrap();
    assert_eq!((track_index(&s, TrackKind::Video, first.id), track_index(&s, TrackKind::Video, second.id)), (0, 1));
    let (nested, clips) = nest_of(&mut s, &[first.id, second.id], "Stack");
    assert_eq!(clips.len(), 2, "picture and sound");
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let (picture, sound) = (q.find_item(clips[0]).unwrap().1, q.find_item(clips[1]).unwrap().1);
    assert!(picture.item == nested && sound.item == nested && picture.link.is_some() && picture.link == sound.link);
    assert_eq!((track_index(&s, TrackKind::Video, clips[0]), track_index(&s, TrackKind::Audio, clips[1])), (0, 0));
    // inside: the pictures on V1 and V2, both sounds on A1
    let inner = s.project.sequence(nested).unwrap();
    assert_eq!((inner.video_tracks.len(), inner.audio_tracks.len()), (2, 1));
    assert!(inner.video_tracks[0].item(first.id).is_some() && inner.video_tracks[1].item(second.id).is_some());
    assert_eq!(inner.audio_tracks[0].items.len(), 2);
}

#[test]
fn clips_with_their_sound_on_several_tracks_nest_as_picture_only_and_the_sound_stays() {
    let mut s = demo();
    // two neighbouring V1 clips; the second one's sound goes to A3
    let first = s.active_sequence().unwrap().video_tracks[0].items[3].clone();
    let first_sound = s.active_sequence().unwrap().audio_tracks[0].items.iter().find(|i| i.link == first.link).unwrap().clone();
    let (second, second_sound) = move_sound(&mut s, 4, "A3");
    assert_eq!(track_index(&s, TrackKind::Audio, second_sound.id), 2);
    let (nested, clips) = nest_of(&mut s, &[first.id, second.id], "Picture");
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    // one clip came back: the picture, unlinked, on V1
    assert_eq!(clips.len(), 1);
    let nest = q.find_item(clips[0]).unwrap().1;
    assert!(nest.item == nested && nest.link.is_none());
    assert_eq!(track_index(&s, TrackKind::Video, clips[0]), 0);
    assert!(q.audio_tracks.iter().all(|t| t.items.iter().all(|i| i.item != nested)), "no sound clip for the nest");
    // the sound is where it was, the same clips, no longer linked to a picture
    for (sound, at) in [(&first_sound, 0), (&second_sound, 2)] {
        let left = q.audio_tracks[at].item(sound.id).expect("the sound stays in the sequence");
        assert_eq!((left.range(), left.link), (sound.range(), None));
    }
    // inside: both pictures on V1; copies of the sound, linked to their pictures, on A1 and A2
    // (the tracks used follow A1 without the gap)
    let inner = s.project.sequence(nested).unwrap();
    inner.check().unwrap();
    assert_eq!((inner.video_tracks.len(), inner.audio_tracks.len()), (1, 2));
    for (track, sound, picture) in [(0, &first_sound, &first), (1, &second_sound, &second)] {
        let copy = &inner.audio_tracks[track].items[0];
        assert!(copy.id != sound.id && copy.item == sound.item && copy.link == picture.link, "track {track}");
    }
}

#[test]
fn a_nest_whose_sound_track_is_taken_is_picture_only_and_moves_up_a_video_track() {
    let mut s = demo();
    // the first and third V1 clips, but not the second: the nest spans all three, and the second
    // clip and its sound are in the way on V1 and A1
    let (first, between, third) = {
        let v1 = &s.active_sequence().unwrap().video_tracks[0].items;
        (v1[0].clone(), v1[1].clone(), v1[2].clone())
    };
    let sounds: Vec<filmcraft_project::TrackItem> =
        s.active_sequence().unwrap().audio_tracks[0].items.iter().filter(|i| i.link == first.link || i.link == third.link).cloned().collect();
    let before = (*s.project).clone();
    let (nested, clips) = nest_of(&mut s, &[first.id, third.id], "Around");
    let q = s.active_sequence().unwrap();
    q.check().expect("no two clips overlap on a track");
    assert_eq!(clips.len(), 1, "picture only");
    let nest = q.find_item(clips[0]).unwrap().1.clone();
    assert!(nest.item == nested && nest.link.is_none());
    // V1 still holds the clip in between, so the nest took the first track above that was free for its whole length
    let index = track_index(&s, TrackKind::Video, clips[0]);
    assert!(index > 0 && q.video_tracks[0].item(between.id).is_some());
    let was = before.sequence(s.state.active_sequence.unwrap()).unwrap();
    let free = |i: usize| was.video_tracks.get(i).is_none_or(|t| !t.items.iter().any(|x| x.range().overlaps(&nest.range())));
    assert!(free(index) && (1..index).all(|i| !free(i)), "track {index}");
    // the sound stays on A1
    for sound in &sounds {
        assert_eq!(q.audio_tracks[0].item(sound.id).map(|i| (i.range(), i.link)), Some((sound.range(), None)));
    }
}

#[test]
fn sound_on_a_track_other_than_a1_keeps_an_empty_a1_in_the_nest() {
    let mut s = demo();
    // one clip whose sound is on A3: a linked nest with its sound still on A3
    let (picture, sound) = move_sound(&mut s, 1, "A3");
    let (nested, clips) = nest_of(&mut s, &[picture.id], "Third");
    assert_eq!(clips.len(), 2);
    assert_eq!((track_index(&s, TrackKind::Video, clips[0]), track_index(&s, TrackKind::Audio, clips[1])), (0, 2));
    // inside: A1 is there and empty, the sound is on A2
    let inner = s.project.sequence(nested).unwrap();
    assert_eq!((inner.video_tracks.len(), inner.audio_tracks.len()), (1, 2));
    assert!(inner.audio_tracks[0].items.is_empty());
    assert!(inner.audio_tracks[1].item(sound.id).is_some());
}

/// The arrangement Premiere was probed with: the 4th V1 clip stays on V1 with its sound on
/// `sound_track`; the 5th and 6th clips' pictures go up to V2, unlinked from their sound. Nesting
/// the 4th and 6th leaves the 5th's picture on V2, a track the selection has a clip on, inside the
/// nest's span. Returns the clips to nest and the one in the way.
fn stacked(s: &mut Session, sound_track: &str) -> ([ClipId; 2], ClipId) {
    let (first, between, last) = {
        let v1 = &s.active_sequence().unwrap().video_tracks[0].items;
        (v1[3].id, v1[4].clone(), v1[5].clone())
    };
    if sound_track != "A1" {
        move_sound(s, 3, sound_track);
    }
    for clip in [&between, &last] {
        s.execute("timeline.select", json!({"clips": [clip.id.0]})).unwrap();
        s.execute("clip.link", json!({})).unwrap();
        s.execute("timeline.move", json!({"moves": [{"clip": clip.id.0, "track": "V2", "time": clip.start.0}], "linked": false})).unwrap();
    }
    assert_eq!((track_index(s, TrackKind::Video, between.id), track_index(s, TrackKind::Video, last.id)), (1, 1));
    ([first, last.id], between.id)
}

#[test]
fn a_clip_sharing_a_track_with_the_selection_pushes_the_nest_above_it_when_the_sound_is_on_a_higher_track() {
    // sound on A3 (free): a linked nest, on V3 because V2 holds another clip and is numbered below the sound's track
    let mut s = demo();
    let (picked, in_the_way) = stacked(&mut s, "A3");
    let (nested, clips) = nest_of(&mut s, &picked, "Above");
    assert_eq!(clips.len(), 2, "linked: the sound track is free");
    assert_eq!((track_index(&s, TrackKind::Video, clips[0]), track_index(&s, TrackKind::Audio, clips[1])), (2, 2));
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert!(q.video_tracks[0].items.iter().all(|i| !i.range().overlaps(&q.find_item(clips[0]).unwrap().1.range())), "V1 was free all along");
    assert_eq!(track_index(&s, TrackKind::Video, in_the_way), 1);
    assert_eq!(s.project.sequence(nested).unwrap().video_tracks.len(), 2);
}

#[test]
fn a_clip_on_a_selected_track_numbered_at_or_above_the_sound_track_does_not_push_the_nest() {
    // sound on A2 (taken by the score, so picture only): V2 is not below the sound's track, the nest stays on V1
    let mut s = demo();
    let (picked, in_the_way) = stacked(&mut s, "A2");
    let (_, clips) = nest_of(&mut s, &picked, "Second");
    assert_eq!(clips.len(), 1, "picture only: A2 is taken");
    assert_eq!(track_index(&s, TrackKind::Video, clips[0]), 0);
    assert_eq!(track_index(&s, TrackKind::Video, in_the_way), 1);
    s.active_sequence().unwrap().check().unwrap();

    // sound on A1 (taken by the clip in between's sound): the same
    let mut s = demo();
    let (picked, _) = stacked(&mut s, "A1");
    let (_, clips) = nest_of(&mut s, &picked, "First");
    assert_eq!(clips.len(), 1);
    assert_eq!(track_index(&s, TrackKind::Video, clips[0]), 0);
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn a_nest_with_no_track_to_go_on_gets_a_new_one() {
    // as the first case, with V3 taken as well: a fourth video track is added for the nest
    let mut s = demo();
    let (picked, _) = stacked(&mut s, "A3");
    let (overlay, at) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[1].items[0].id, q.find_item(picked[0]).unwrap().1.start)
    };
    s.execute("timeline.move", json!({"moves": [{"clip": overlay.0, "track": "V3", "time": at.0}], "linked": false})).unwrap();
    assert_eq!(track_index(&s, TrackKind::Video, overlay), 2);
    let tracks = s.active_sequence().unwrap().video_tracks.len();
    let (_, clips) = nest_of(&mut s, &picked, "Top");
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!((tracks, q.video_tracks.len()), (3, 4));
    assert_eq!(track_index(&s, TrackKind::Video, clips[0]), 3);
    // one undo takes the nest and its track away again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3);
}

#[test]
fn make_subsequence_loads_the_new_sequence_in_the_source_monitor() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    let v = clip_ids(&s, TrackKind::Video, 0)[1];
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    let sub = ItemId(s.execute("sequence.makeSubsequence", json!({})).unwrap()["sequence"].as_u64().unwrap());
    assert_eq!(s.state.source_item, Some(sub));
    assert_eq!(s.state.project_selection, vec![sub]);
    // it is not opened in the Timeline, and the selection is left alone
    assert_eq!(s.state.active_sequence, Some(outer));
    assert!(!s.state.open_sequences.contains(&sub));
    assert!(s.state.selection.contains(&ClipId(v)));
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::OpenSource(i) if *i == sub)));
}
