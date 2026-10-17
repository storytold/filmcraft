//! Time Remapping after an edit.
//!
//! A clip's Time Remapping Speed is its speed, as in Premiere:
//! - **Static**, it is the clip's constant speed. Setting it changes the clip's speed, and every
//!   other speed change (Speed/Duration, Rate Stretch, an import) shows up in it.
//! - **Keyframed**, it sets the speed of each part of the media ([`filmcraft_project::TimeMap`]),
//!   and the clip's `speed` holds the average speed, for the code (and the interchange formats)
//!   that only know a constant one.
//!
//! Either way a new speed keeps the media the clip showed and changes its length, as Speed/Duration
//! does without ripple: a slower clip grows up to the next clip on its track and is cut there.
//!
//! **Sound.** With Remap Audio on (the default), the clip's linked sound from the same source
//! follows: a keyframed curve is copied onto it (its keyframes sit at media times, which the
//! picture and the sound of one source share, so the two stay in sync), a static speed becomes its
//! speed, and it is retimed the same way. It plays varispeed, its pitch rising and falling with the
//! speed. Turned off, the sound plays at normal speed, as Premiere's picture-only Time Remapping.
//! The sound's copy of the curve is not edited on its own: it follows the picture.
//!
//! [`follow_time_remapping`] applies these rules after an edit, whichever command made it (the
//! parameter itself, its keyframes and their interpolation, Paste/Remove Attributes, presets).

use filmcraft_project::{ClipId, EffectInstance, ItemId, Param, ParamValue, Sequence, Track, TrackItem, time_remap};
use filmcraft_time::Tick;

/// Bring every clip's Time Remapping and timing in step after an edit. `before` is the sequence as
/// it was before the edit (None for a new or just-opened one: its clips' static speeds are only
/// brought in line with their speeds). `frame` is the sequence's frame duration: the shortest a
/// clip may become, and the grid a new length snaps to when it is only rounding away from it.
pub fn follow_time_remapping(before: Option<&Sequence>, seq: &mut Sequence, frame: Tick) {
    // pictures whose Time Remapping the edit changed, for their linked sound: (link, source, the
    // picture). Only the edit's own change counts: the static speed showing a new clip speed
    // (Speed/Duration of the picture alone) leaves the sound be.
    let mut changed: Vec<(u64, ItemId, TrackItem)> = Vec::new();
    for tr in seq.video_tracks.iter_mut() {
        follow_track(before, tr, frame, |it, edited| {
            if let (Some(link), true) = (it.link, edited) {
                changed.push((link, it.item, it.clone()));
            }
        });
    }
    for tr in seq.audio_tracks.iter_mut() {
        follow_track(before, tr, frame, |_, _| {});
        if tr.locked {
            continue;
        }
        let mut moved: Vec<ClipId> = Vec::new();
        for i in 0..tr.items.len() {
            let next_start = tr.items.get(i + 1).map_or(Tick::MAX, |n| n.start);
            let Some(it) = tr.items.get_mut(i) else { continue };
            let Some((_, _, picture)) = changed.iter().find(|(link, item, _)| it.link == Some(*link) && it.item == *item) else { continue };
            let old = before.and_then(|b| b.find_item(it.id)).map(|(_, o)| o);
            if sync_sound(it, old, picture, next_start, frame) {
                moved.push(it.id);
            }
        }
        for id in moved {
            crate::fades_follow_edges(tr, id);
        }
    }
}

/// [`follow`] every clip of a track; `seen` gets each clip (after) and whether the edit changed
/// its Time Remapping.
fn follow_track(before: Option<&Sequence>, tr: &mut Track, frame: Tick, mut seen: impl FnMut(&TrackItem, bool)) {
    let mut moved: Vec<ClipId> = Vec::new();
    for i in 0..tr.items.len() {
        let next_start = tr.items.get(i + 1).map_or(Tick::MAX, |n| n.start);
        let Some(it) = tr.items.get_mut(i) else { continue };
        let old = before.and_then(|b| b.find_item(it.id)).map(|(_, o)| o);
        let edited = old.is_some_and(|o| o.effect("time_remap").is_some() && o.effect("time_remap") != it.effect("time_remap"));
        if follow(it, old, next_start, frame) {
            moved.push(it.id);
        }
        seen(it, edited);
    }
    for id in moved {
        crate::fades_follow_edges(tr, id);
    }
}

/// A linked sound follows its picture's new Time Remapping (see the module docs). True when its
/// length changed.
fn sync_sound(it: &mut TrackItem, old: Option<&TrackItem>, picture: &TrackItem, next_start: Tick, frame: Tick) -> bool {
    let curve = picture.effect("time_remap").filter(|_| picture.is_remapped());
    match curve {
        Some(e) if picture.remaps_audio() => {
            let e: EffectInstance = e.clone();
            match it.effect_mut("time_remap") {
                Some(own) => *own = e,
                None => it.effects.push(e),
            }
        }
        _ => {
            let had_curve = it.effect("time_remap").is_some();
            it.effects.retain(|e| e.effect != "time_remap");
            if picture.remaps_audio() {
                it.speed = picture.speed.abs();
            } else if had_curve {
                // Remap Audio turned off: the sound plays at normal speed again
                it.speed = 1.0;
            } else {
                return false;
            }
        }
    }
    let moved = retime(it, old, next_start, frame);
    if it.is_remapped() && it.duration > Tick::ZERO {
        let avg = (it.source_out() - it.source_in).0 as f64 / it.duration.0 as f64;
        if avg.is_finite() && avg > 0.0 {
            it.speed = avg;
        }
    }
    moved
}

/// One clip; true when its length changed.
fn follow(it: &mut TrackItem, old: Option<&TrackItem>, next_start: Tick, frame: Tick) -> bool {
    let Some(e) = it.effect("time_remap") else { return false };
    if !e.enabled {
        // off, the clip plays at its own (average) speed, which shows the media it did
        return false;
    }
    let changed = matches!(old.and_then(|o| o.effect("time_remap")), Some(o) if o != e);
    let Some(speed) = e.param("speed") else { return false };
    if speed.is_animated() {
        let moved = changed && retime(it, old, next_start, frame);
        if it.duration > Tick::ZERO {
            let avg = (it.source_out() - it.source_in).0 as f64 / it.duration.0 as f64;
            if avg.is_finite() && avg > 0.0 {
                it.speed = avg;
            }
        }
        return moved;
    }
    let value = speed.value.as_f64();
    let was_remapped = old.is_some_and(TrackItem::is_remapped);
    if changed && let Some(v) = value {
        let rate = time_remap::rate_of_percent(v);
        if was_remapped || (rate - it.speed.abs()).abs() > 1e-12 {
            it.speed = rate;
            let moved = retime(it, old, next_start, frame);
            mirror(it);
            return moved;
        }
    }
    mirror(it);
    false
}

/// The static Time Remapping speed shows the clip's speed.
fn mirror(it: &mut TrackItem) {
    let v = it.speed.abs() * 100.0;
    if let Some(p) = it.effect_mut("time_remap").and_then(|e| e.param_mut("speed"))
        && !p.is_animated()
        && p.value.as_f64() != Some(v)
    {
        *p = Param::new(ParamValue::Float(v));
    }
}

/// Keep the media `old` showed at the clip's new speed: its length changes (up to the next clip on
/// the track). True when it did.
fn retime(it: &mut TrackItem, old: Option<&TrackItem>, next_start: Tick, frame: Tick) -> bool {
    let Some(old) = old else { return false };
    // the edit moved the clip or its media itself: that timing is the edit's own
    if it.frame_hold.is_some() || old.source_in != it.source_in || old.start != it.start || old.duration != it.duration || old.reverse != it.reverse {
        return false;
    }
    let want_out = old.source_out();
    // the clip still ends on the same media (a curve that changed elsewhere, or rounding)
    if (it.source_out() - want_out).0.abs() <= 1 {
        return false;
    }
    let room = Tick(next_start.0.saturating_sub(it.start.0));
    let mut len = it.timeline_span(it.source_in, want_out).round();
    // a curve's timing is exact to a few ticks: a length that close to whole frames is whole frames
    // (a ramp taken off again gives the clip its old length back, not one a tick short of it)
    let f = frame.0.max(1) as f64;
    let frames = (len / f).round() * f;
    if (len - frames).abs() <= f / 100_000.0 {
        len = frames;
    }
    let new_dur = Tick(if len.is_finite() { len as i64 } else { i64::MAX }).max(frame).min(room);
    if new_dur == it.duration {
        return false;
    }
    it.duration = new_dur;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditCtx, razor, through, trim};
    use filmcraft_project::{Interpolation, ItemId, Label, SequenceSettings};

    const S: i64 = filmcraft_time::TICKS_PER_SECOND;

    fn item(id: u64, start: i64, dur: i64, src: i64) -> TrackItem {
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: "c".into(),
            label: Label::Iris,
            start: Tick(start),
            duration: Tick(dur),
            source_in: Tick(src),
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: filmcraft_project::effect::intrinsic_video(),
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    fn seq_with(items: Vec<TrackItem>) -> Sequence {
        let mut p = filmcraft_project::Project::new("t");
        let id = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
        let mut q = p.sequence(id).unwrap().clone();
        q.video_tracks[0].items = items;
        q
    }

    fn ctx(next: &mut u64) -> EditCtx<'_> {
        EditCtx { next_id: next, media_duration: &|_| Some(Tick(100 * S)), media_start: &|_| Tick::ZERO, min_duration: Tick(1) }
    }

    fn speed(it: &mut TrackItem) -> &mut Param {
        it.effect_mut("time_remap").unwrap().param_mut("speed").unwrap()
    }

    /// The clip's speed ramps from 100 % (at 1 s of media) to 400 % (at 3 s) and stays there.
    fn ramp(it: &mut TrackItem) {
        let p = speed(it);
        p.put_keyframe(Tick(S), ParamValue::Float(100.0));
        p.put_keyframe(Tick(3 * S), ParamValue::Float(400.0));
        p.keyframes[0].interp = Interpolation::Linear;
    }

    fn followed(before: &Sequence, mut after: Sequence) -> Sequence {
        follow_time_remapping(Some(before), &mut after, Tick(1));
        after
    }

    #[test]
    fn keyframes_keep_the_media_and_change_the_length() {
        let before = seq_with(vec![item(1, 0, 10 * S, 0)]);
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        let it = &q.video_tracks[0].items[0];
        // still ten seconds of media, now played faster
        assert!((it.source_out() - Tick(10 * S)).0.abs() <= 1, "{:?}", it.source_out());
        assert!(it.duration < Tick(5 * S) && it.duration > Tick(3 * S), "{:?}", it.duration);
        // 1 s at 100 %, the ramp (2 s of media in 2·ln4/3 s) and 7 s at 400 %
        let want = S as f64 * (1.0 + 2.0 * 4f64.ln() / 3.0 + 7.0 / 4.0);
        assert!((it.duration.0 as f64 - want).abs() / want < 1e-6, "{:?} vs {want}", it.duration);
        assert!((it.speed - 10.0 * S as f64 / it.duration.0 as f64).abs() < 1e-9, "average speed");
        // media at the timeline: 100 % until 1 s, then faster
        assert_eq!(it.source_time_at(Tick(S / 2)), Tick(S / 2));
        assert!(it.source_time_at(Tick(2 * S)) > Tick(2 * S + S / 2));
    }

    #[test]
    fn a_slower_clip_stops_at_the_next_clip() {
        let before = seq_with(vec![item(1, 0, 4 * S, 0), item(2, 5 * S, 2 * S, 0)]);
        let mut after = before.clone();
        let p = speed(&mut after.video_tracks[0].items[0]);
        p.put_keyframe(Tick::ZERO, ParamValue::Float(50.0));
        let q = followed(&before, after);
        assert_eq!(q.video_tracks[0].items[0].duration, Tick(5 * S));
        assert!(q.check().is_ok());
    }

    #[test]
    fn a_static_value_is_the_clip_speed() {
        let before = seq_with(vec![item(1, 0, 10 * S, 0)]);
        let mut after = before.clone();
        speed(&mut after.video_tracks[0].items[0]).value = ParamValue::Float(200.0);
        let q = followed(&before, after);
        let it = &q.video_tracks[0].items[0];
        assert_eq!((it.speed, it.duration), (2.0, Tick(5 * S)));
        assert_eq!(it.source_out(), Tick(10 * S));
        // and a speed set elsewhere shows up in it
        let before = q.clone();
        let mut after = q;
        after.video_tracks[0].items[0].speed = 0.5;
        let q = followed(&before, after);
        assert_eq!(speed(&mut q.video_tracks[0].items[0].clone()).value, ParamValue::Float(50.0));
    }

    #[test]
    fn turning_keyframes_on_at_the_clip_speed_changes_nothing() {
        let mut it = item(1, 0, 7 * S + 3, 11);
        it.speed = 1.5;
        let before = seq_with(vec![it]);
        let before = followed(&before, before.clone());
        let mut after = before.clone();
        let p = speed(&mut after.video_tracks[0].items[0]);
        p.toggle_animation(Tick(2 * S));
        let q = followed(&before, after);
        let (a, b) = (&before.video_tracks[0].items[0], &q.video_tracks[0].items[0]);
        assert_eq!((a.start, a.duration, a.source_in), (b.start, b.duration, b.source_in));
        assert!((a.source_out() - b.source_out()).0.abs() <= 1);
        // and off again: the clip plays at the value it had at the playhead
        let before = q.clone();
        let mut after = q;
        speed(&mut after.video_tracks[0].items[0]).toggle_animation(Tick(2 * S));
        let q = followed(&before, after);
        assert_eq!(q.video_tracks[0].items[0].duration, a.duration);
        assert_eq!(q.video_tracks[0].items[0].speed, 1.5);
    }

    #[test]
    fn razor_and_trims_keep_the_ramp_on_its_media() {
        let before = seq_with(vec![item(1, 0, 10 * S, 0)]);
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        let whole = q.video_tracks[0].items[0].clone();
        // a razor cut shows every frame where the whole clip did
        let mut cut = q.clone();
        let mut n = 100;
        razor(&mut cut, &[], Tick(2 * S + 12_345), &mut ctx(&mut n));
        let probe = |q: &Sequence, t: Tick| q.video_tracks[0].items.iter().find(|i| i.start <= t && t < i.end()).map(|i| i.source_time_at(t));
        for i in 0..40 {
            let t = Tick(i * whole.duration.0 / 40);
            let (a, b) = (probe(&q, t).unwrap(), probe(&cut, t).unwrap());
            assert!((a - b).0.abs() <= 2, "{t:?}: {a:?} vs {b:?}");
        }
        assert_eq!(through::through_edits(&cut).len(), 1);
        assert_eq!(through::join_through_edits(&mut cut, &[]), 1);
        let joined = followed(&q, cut);
        let j = &joined.video_tracks[0].items[0];
        assert_eq!((j.duration, j.source_in), (whole.duration, whole.source_in));
        // trimming the head off keeps the later frames where they were
        let mut trimmed = q.clone();
        let mut n = 100;
        trim(&mut trimmed, ClipId(1), crate::Edge::In, crate::TrimMode::Regular, Tick(3 * S), &mut ctx(&mut n)).unwrap();
        for i in 0..20 {
            let t = Tick(3 * S + i * (whole.end() - Tick(3 * S)).0 / 20);
            assert!((probe(&q, t).unwrap() - probe(&trimmed, t).unwrap()).0.abs() <= 2, "{t:?}");
        }
    }

    #[test]
    fn constant_speed_is_refused_on_a_remapped_clip() {
        let before = seq_with(vec![item(1, 0, 10 * S, 0)]);
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let mut q = followed(&before, after);
        let mut n = 100;
        assert!(crate::set_speed(&mut q, ClipId(1), 2.0, false, false, &mut ctx(&mut n)).is_err());
        assert!(crate::rate_stretch(&mut q, ClipId(1), crate::Edge::Out, Tick(-S), &mut ctx(&mut n)).is_err());
    }

    #[test]
    fn reversed_clips_play_their_ramp_backwards() {
        let mut it = item(1, 0, 10 * S, 0);
        it.reverse = true;
        let before = seq_with(vec![it]);
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        let it = &q.video_tracks[0].items[0];
        assert!((it.source_out() - Tick(10 * S)).0.abs() <= 1);
        // the clip starts on its last frame and ends on its first, and media only goes back
        assert!((it.source_time_at(it.start) - Tick(10 * S)).0.abs() <= 2);
        assert!(it.source_time_at(it.end() - Tick(1)) < Tick(S / 100));
        let mut last = Tick::MAX;
        for i in 0..100 {
            let m = it.source_time_at(it.start + Tick(i * it.duration.0 / 100));
            assert!(m < last);
            last = m;
            assert!((it.timeline_time_of(m) - (it.start + Tick(i * it.duration.0 / 100))).0.abs() <= 2);
        }
    }

    /// A picture (V1) and its sound (A1) from one source, linked, ten seconds each.
    fn pair() -> Sequence {
        let mut q = seq_with(vec![item(1, 0, 10 * S, 0)]);
        q.video_tracks[0].items[0].link = Some(7);
        let mut sound = item(2, 0, 10 * S, 0);
        sound.effects = filmcraft_project::effect::intrinsic_audio();
        sound.link = Some(7);
        q.audio_tracks[0].items = vec![sound];
        q
    }

    fn sound(q: &Sequence) -> &TrackItem {
        &q.audio_tracks[0].items[0]
    }

    #[test]
    fn the_linked_sound_follows_the_ramp() {
        let before = pair();
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        assert!(a.is_remapped());
        assert_eq!((a.start, a.duration, a.source_in), (v.start, v.duration, v.source_in));
        for i in 0..50 {
            let t = Tick(i * v.duration.0 / 50);
            assert_eq!(a.source_time_at(t), v.source_time_at(t), "{t:?}");
        }
        // and back to a constant speed with the stopwatch
        let before = q.clone();
        let mut after = q;
        speed(&mut after.video_tracks[0].items[0]).toggle_animation(Tick(5 * S));
        let q = followed(&before, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        assert!(!a.is_remapped() && a.effect("time_remap").is_none());
        assert_eq!((a.speed, a.duration), (v.speed, v.duration));
    }

    #[test]
    fn a_static_speed_moves_the_sound_too() {
        let before = pair();
        let mut after = before.clone();
        speed(&mut after.video_tracks[0].items[0]).value = ParamValue::Float(50.0);
        let q = followed(&before, after);
        assert_eq!((sound(&q).speed, sound(&q).duration), (0.5, Tick(20 * S)));
    }

    #[test]
    fn remap_audio_off_leaves_the_sound_at_normal_speed() {
        let mut before = pair();
        before.video_tracks[0].items[0].effect_mut("time_remap").unwrap().params.insert("audio".into(), Param::new(ParamValue::Bool(false)));
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        assert!(q.video_tracks[0].items[0].is_remapped());
        assert_eq!(sound(&q), sound(&before));
        // turned off after the fact: the sound drops the curve and plays at 100 % again
        let before = pair();
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let ramped = followed(&before, after);
        assert!(sound(&ramped).is_remapped());
        let mut after = ramped.clone();
        after.video_tracks[0].items[0].effect_mut("time_remap").unwrap().params.insert("audio".into(), Param::new(ParamValue::Bool(false)));
        let q = followed(&ramped, after);
        let a = sound(&q);
        assert!(a.effect("time_remap").is_none());
        assert_eq!((a.speed, a.duration, a.source_in), (1.0, Tick(10 * S), Tick::ZERO));
    }

    #[test]
    fn a_picture_only_speed_change_leaves_the_sound_be() {
        // Speed/Duration on the picture alone shows up in its static Time Remapping speed, which is
        // not a Time Remapping edit
        let before = pair();
        let mut after = before.clone();
        let mut n = 100;
        crate::set_speed(&mut after, ClipId(1), 2.0, false, false, &mut ctx(&mut n)).unwrap();
        let q = followed(&before, after);
        assert_eq!(q.video_tracks[0].items[0].duration, Tick(5 * S));
        assert_eq!(sound(&q), sound(&before));
    }

    #[test]
    fn a_locked_sound_track_keeps_its_sound() {
        let mut before = pair();
        before.audio_tracks[0].locked = true;
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        assert_eq!(sound(&q), sound(&before));
    }
}
