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
//! **Sound.** The clip's linked sound from the same source follows the picture's length, never its
//! curve. A static speed is the clip's speed, so the sound is retimed with the picture: the same
//! media played at the same speed, its pitch kept only when Maintain Audio Pitch is on
//! (Speed/Duration…, off until the user sets it). A keyframed ramp leaves the sound at 100 %: it
//! stays linked and plays its own media at its own rate, in step at the clip's head and drifting out
//! of step where the ramp speeds the picture up or slows it down, so a faster ramp cuts the sound's
//! tail and a slower one leaves silence after its media ends. That is Premiere's Time Remapping, and
//! it keeps the sound audible where muting it would lose it.
//!
//! [`follow_time_remapping`] applies these rules after an edit, whichever command made it (the
//! parameter itself, its keyframes and their interpolation, Paste/Remove Attributes, presets).

use filmcraft_project::{ClipId, ItemId, Param, ParamValue, Sequence, Track, TrackItem, time_remap};
use filmcraft_time::Tick;

/// Bring every clip's Time Remapping and timing in step after an edit. `before` is the sequence as
/// it was before the edit (None for a new or just-opened one: its clips' static speeds are only
/// brought in line with their speeds). `frame` is the sequence's frame duration: the shortest a
/// clip may become, and the grid a new length snaps to when it is only rounding away from it.
pub fn follow_time_remapping(before: Option<&Sequence>, seq: &mut Sequence, frame: Tick) {
    // pictures whose Time Remapping the edit changed, for their linked sound: (link, source, the
    // picture, the picture's length before the edit). Only the edit's own change counts: the static
    // speed showing a new clip speed (Speed/Duration of the picture alone) leaves the sound be.
    let mut changed: Vec<(u64, ItemId, TrackItem, Tick)> = Vec::new();
    for tr in seq.video_tracks.iter_mut() {
        follow_track(before, tr, frame, |it, old, edited| {
            if let (Some(link), true) = (it.link, edited) {
                // a picture the edit added has no earlier length: its sound has nothing to follow
                changed.push((link, it.item, it.clone(), old.map_or(it.duration, |o| o.duration)));
            }
        });
    }
    for tr in seq.audio_tracks.iter_mut() {
        follow_track(before, tr, frame, |_, _, _| {});
        if tr.locked {
            continue;
        }
        let mut moved: Vec<ClipId> = Vec::new();
        for i in 0..tr.items.len() {
            let next_start = tr.items.get(i + 1).map_or(Tick::MAX, |n| n.start);
            let Some(it) = tr.items.get_mut(i) else { continue };
            let Some((_, _, picture, was)) = changed.iter().find(|(link, item, _, _)| it.link == Some(*link) && it.item == *item) else { continue };
            if sync_sound(it, picture, *was, next_start, frame) {
                moved.push(it.id);
            }
        }
        for id in moved {
            crate::fades_follow_edges(tr, id);
        }
    }
}

/// [`follow`] every clip of a track; `seen` gets each clip (after), the same clip before the edit
/// (None when it is new) and whether the edit changed its Time Remapping.
fn follow_track(before: Option<&Sequence>, tr: &mut Track, frame: Tick, mut seen: impl FnMut(&TrackItem, Option<&TrackItem>, bool)) {
    let mut moved: Vec<ClipId> = Vec::new();
    for i in 0..tr.items.len() {
        let next_start = tr.items.get(i + 1).map_or(Tick::MAX, |n| n.start);
        let Some(it) = tr.items.get_mut(i) else { continue };
        let old = before.and_then(|b| b.find_item(it.id)).map(|(_, o)| o);
        let edited = old.is_some_and(|o| o.effect("time_remap").is_some() && o.effect("time_remap") != it.effect("time_remap"));
        if follow(it, old, next_start, frame) {
            moved.push(it.id);
        }
        seen(it, old, edited);
    }
    for id in moved {
        crate::fades_follow_edges(tr, id);
    }
}

/// A linked sound follows its picture's new Time Remapping (see the module docs). `was` is the
/// picture's length before the edit. True when the sound's length changed.
fn sync_sound(it: &mut TrackItem, picture: &TrackItem, was: Tick, next_start: Tick, frame: Tick) -> bool {
    // a held picture shows its one frame whatever its speed says: its sound has nothing to follow
    if picture.frame_hold.is_some() {
        return false;
    }
    // the curve stays on the picture: a sound plays at one speed, and the keyframes sit on the
    // source's media, which the sound only shares at 100 %
    it.effects.retain(|e| e.effect != "time_remap");
    it.speed = if picture.is_remapped() { 1.0 } else { picture.speed.abs() };
    // the sound ends where the picture's edit moved its end: a ramp cuts its tail (or runs it into
    // silence) while it plays at 100 %, and a static speed scales it with the picture
    let delta = picture.duration.0.saturating_sub(was.0);
    grow(it, delta, next_start, frame)
}

/// Add `delta` to the clip's length, kept inside the track's room before the next clip, at least one
/// frame, and inside the time bounds. True when the length changed.
fn grow(it: &mut TrackItem, delta: i64, next_start: Tick, frame: Tick) -> bool {
    let least = frame.max(Tick(1));
    let room = Tick(next_start.0.saturating_sub(it.start.0)).max(least);
    let dur = Tick(it.duration.0.saturating_add(delta)).max(least).min(room);
    if dur == it.duration {
        return false;
    }
    it.duration = dur;
    true
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
        let mut effects = filmcraft_project::effect::intrinsic_video();
        // Real placed media resolves auto-position NaNs before attributes are compared for a join.
        for effect in &mut effects {
            filmcraft_project::resolve_auto_points(effect, (1920, 1080), (1920, 1080));
        }
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: "c".into(),
            label: Label::Iris,
            custom_label_color: None,
            start: Tick(start),
            duration: Tick(dur),
            source_in: Tick(src),
            speed: 1.0,
            reverse: false,
            maintain_pitch: false,
            enabled: true,
            link: None,
            group: None,
            effects,
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

    /// The media a clip plays over its length: 1.0 plays its source at its own rate.
    fn rate(a: &TrackItem) -> f64 {
        (a.source_out() - a.source_in).0 as f64 / a.duration.0 as f64
    }

    #[test]
    fn a_ramp_leaves_the_linked_sound_at_full_speed() {
        let before = pair();
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let q = followed(&before, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        // the picture ramps shorter; the sound takes no curve and plays at 100 %
        assert!(v.is_remapped() && v.duration < Tick(10 * S), "{:?}", v.duration);
        assert!(!a.is_remapped() && a.effect("time_remap").is_none());
        assert_eq!(a.speed, 1.0);
        assert!((rate(a) - 1.0).abs() < 1e-9, "the sound plays at {}", rate(a));
        // still linked to the picture, enabled and unmuted
        assert!(a.link.is_some() && a.link == v.link);
        assert!(a.enabled && !a.essential.as_ref().is_some_and(|e| e.mute) && a.gain_db == 0.0);
        // its length follows the picture's, and its media plays from the clip's head
        assert_eq!((a.start, a.duration), (v.start, v.duration));
        for i in 0..=20 {
            let t = a.start + Tick(i * a.duration.0 / 20);
            assert_eq!(a.source_time_at(t), a.source_in + (t - a.start), "{t:?}");
        }
        // the faster the ramp plays the picture, the further it runs ahead of the sound
        let late = a.start + Tick(a.duration.0 * 9 / 10);
        assert!(v.source_time_at(late) > a.source_time_at(late) + Tick(S / 2));
        // so the sound plays only the media its length covers: the ramp cuts its tail
        assert_eq!(a.source_out(), a.source_in + a.duration);
        assert!(a.source_out() < v.source_out());
        assert!(q.check().is_ok(), "{}", q.check().unwrap_err());
    }

    #[test]
    fn a_ramp_taken_off_puts_the_sound_back_in_step() {
        let before = pair();
        let mut after = before.clone();
        ramp(&mut after.video_tracks[0].items[0]);
        let ramped = followed(&before, after);
        let mut after = ramped.clone();
        speed(&mut after.video_tracks[0].items[0]).toggle_animation(Tick(5 * S));
        let q = followed(&ramped, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        // a constant speed again: the pair plays the same media at the same speed
        assert!(!a.is_remapped() && a.effect("time_remap").is_none());
        assert!((a.speed - v.speed).abs() < 1e-9, "{} vs {}", a.speed, v.speed);
        assert_eq!((a.start, a.duration, a.source_in), (v.start, v.duration, v.source_in));
        assert_eq!(a.source_out(), v.source_out());
        assert!(q.check().is_ok(), "{}", q.check().unwrap_err());
    }

    #[test]
    fn a_static_speed_moves_the_sound_too() {
        let before = pair();
        let mut after = before.clone();
        speed(&mut after.video_tracks[0].items[0]).value = ParamValue::Float(50.0);
        let q = followed(&before, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        // the same media played for twice as long, in step with the picture
        assert_eq!((a.speed, a.duration), (0.5, Tick(20 * S)));
        assert_eq!((a.start, a.source_in, a.source_out()), (v.start, v.source_in, v.source_out()));
        assert!((rate(a) - 0.5).abs() < 1e-9, "the sound plays at {}", rate(a));
        // pitch preservation is off until it is asked for, and a retime keeps the pair's setting
        assert!(!a.maintain_pitch);
        let mut after = q.clone();
        after.video_tracks[0].items[0].maintain_pitch = true;
        after.audio_tracks[0].items[0].maintain_pitch = true;
        speed(&mut after.video_tracks[0].items[0]).value = ParamValue::Float(200.0);
        let q = followed(&q, after);
        assert!(sound(&q).maintain_pitch, "Maintain Audio Pitch goes with the pair");
        assert!((sound(&q).speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_ramp_that_starts_or_ends_at_no_speed_keeps_the_sound_whole() {
        for keys in [[(0, 0.0), (2 * S, 400.0)], [(0, 400.0), (2 * S, 0.0)], [(0, 0.0), (2 * S, 0.0)]] {
            let before = pair();
            let mut after = before.clone();
            let p = speed(&mut after.video_tracks[0].items[0]);
            for (t, v) in keys {
                p.put_keyframe(Tick(t), ParamValue::Float(v));
            }
            p.keyframes[0].interp = Interpolation::Linear;
            let q = followed(&before, after);
            let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
            // the slowest speed is 1 %, so the picture and its sound stay representable and whole
            assert!(v.duration > Tick::ZERO, "{keys:?}");
            assert!(a.duration > Tick::ZERO, "{keys:?}: {:?}", a.duration);
            assert!(a.start.0.checked_add(a.duration.0).is_some(), "{keys:?}");
            assert_eq!(a.speed, 1.0);
            assert!((rate(a) - 1.0).abs() < 1e-9, "{keys:?}: {}", rate(a));
            assert!(q.check().is_ok(), "{keys:?}: {}", q.check().unwrap_err());
        }
    }

    #[test]
    fn a_single_keyframe_is_a_constant_speed_for_the_sound() {
        let before = pair();
        let mut after = before.clone();
        speed(&mut after.video_tracks[0].items[0]).put_keyframe(Tick(S), ParamValue::Float(400.0));
        let q = followed(&before, after);
        let (v, a) = (&q.video_tracks[0].items[0], sound(&q));
        // one keyframe is one speed everywhere: the picture gets shorter, the sound plays under it
        assert!(v.is_remapped() && v.duration < Tick(3 * S), "{:?}", v.duration);
        assert_eq!((a.speed, a.start, a.duration), (1.0, v.start, v.duration));
        assert!((rate(a) - 1.0).abs() < 1e-9);
        assert!(q.check().is_ok(), "{}", q.check().unwrap_err());
    }

    #[test]
    fn hostile_speeds_keep_the_sounds_length_representable() {
        for value in [time_remap::MIN_SPEED_PERCENT, time_remap::MAX_SPEED_PERCENT, -1.0, f64::NAN, f64::INFINITY] {
            let before = pair();
            let mut after = before.clone();
            let p = speed(&mut after.video_tracks[0].items[0]);
            p.put_keyframe(Tick::ZERO, ParamValue::Float(value));
            p.put_keyframe(Tick(S), ParamValue::Float(100.0));
            let q = followed(&before, after);
            let a = sound(&q);
            assert!(a.duration > Tick::ZERO, "{value}: {:?}", a.duration);
            assert!(a.start.0.checked_add(a.duration.0).is_some(), "{value}");
            assert_eq!(a.speed, 1.0, "{value}");
            assert!((rate(a) - 1.0).abs() < 1e-9, "{value}: {}", rate(a));
            assert!(q.check().is_ok(), "{value}: {}", q.check().unwrap_err());
        }
    }

    #[test]
    fn a_slower_ramp_stops_the_sound_at_the_next_sound() {
        let mut before = pair();
        before.audio_tracks[0].items.push(item(3, 12 * S, 5 * S, 0));
        let mut after = before.clone();
        speed(&mut after.video_tracks[0].items[0]).put_keyframe(Tick::ZERO, ParamValue::Float(50.0));
        let q = followed(&before, after);
        assert!(q.video_tracks[0].items[0].duration > Tick(12 * S));
        assert_eq!(sound(&q).duration, Tick(12 * S), "the room up to the next sound");
        assert!(q.check().is_ok(), "{}", q.check().unwrap_err());
    }

    /// A project file can hold speed keyframes on a sound of its own (no picture to copy them
    /// from): it is played along them like any other clip, and nothing else is retimed.
    #[test]
    fn a_sound_with_its_own_curve_is_retimed_alone() {
        let mut before = seq_with(vec![item(1, 0, 10 * S, 0)]);
        before.audio_tracks[0].items = vec![{
            let mut a = item(2, 0, 10 * S, 0);
            a.effects = filmcraft_project::effect::intrinsic_audio();
            a.effects.push(filmcraft_project::effect::find_effect("time_remap").unwrap().instance());
            a
        }];
        let mut after = before.clone();
        let p = after.audio_tracks[0].items[0].effect_mut("time_remap").unwrap().param_mut("speed").unwrap();
        p.put_keyframe(Tick::ZERO, ParamValue::Float(100.0));
        p.put_keyframe(Tick(2 * S), ParamValue::Float(200.0));
        let q = followed(&before, after);
        let a = sound(&q);
        assert!(a.is_remapped());
        // 100 % up to 2 s of media, then 200 %: 2·ln2 + 8/2 of timeline for the ten seconds
        let want = S as f64 * (2.0 * 2f64.ln() + 4.0);
        assert!((a.duration.0 as f64 - want).abs() / want < 1e-6, "{:?} vs {want}", a.duration);
        assert_eq!(q.video_tracks[0].items[0], before.video_tracks[0].items[0], "the picture is its own");
        assert!(q.check().is_ok(), "{}", q.check().unwrap_err());
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
