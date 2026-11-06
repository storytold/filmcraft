use super::*;

/// Apply an Out-edge ripple stretch with the fixture’s edit context.
fn stretch(fx: &mut Fx, clips: &[ClipId], delta: Tick) -> Result<f64> {
    let mut next = fx.next;
    ripple_rate_stretch_items(&mut fx.seq, clips, delta, &mut Fx::ctx(&mut next))
}

/// Opening and closing the cut preserve source frames, existing gaps and excluded tracks.
#[test]
fn both_directions_keep_adjacency_gaps_source_and_locked_music() {
    for delta in [-17, 23] {
        for reverse in [false, true] {
            let mut fx = Fx::new();
            let (v1, v2, v3, a1, a2) = (fx.v(0), fx.v(1), fx.v(2), fx.a(0), fx.a(1));
            let pic = fx.put(v1, 10, 60, 30);
            let sound = fx.put(a1, 10, 60, 30);
            fx.seq.find_item_mut(pic).unwrap().1.reverse = reverse;
            fx.seq.find_item_mut(sound).unwrap().1.reverse = reverse;
            fx.put(v1, 70, 20, 100);
            fx.put(v1, 95, 20, 120);
            fx.put(a1, 70, 20, 100);
            fx.put(v2, 80, 20, 100);
            fx.put(v3, 80, 20, 100);
            fx.seq.track_mut(v3).unwrap().sync_lock = false;
            fx.put(a2, 0, 300, 0);
            fx.seq.track_mut(a2).unwrap().locked = true;
            let before = fx.seq.clone();
            let speed = stretch(&mut fx, &[pic, sound], f(delta)).unwrap();
            assert!((speed - 60.0 / (60 + delta) as f64).abs() < 1e-12);
            assert_eq!(fx.spans(v1), vec![(10, 60 + delta), (70 + delta, 20), (95 + delta, 20)]);
            assert_eq!(fx.spans(a1), vec![(10, 60 + delta), (70 + delta, 20)]);
            assert_eq!(fx.spans(v2), vec![(80 + delta, 20)]);
            assert_eq!(fx.seq.track(a2), before.track(a2), "locked music unchanged");
            assert_eq!(fx.seq.track(v3), before.track(v3), "sync lock off unchanged");
            for c in [pic, sound] {
                let old = before.find_item(c).unwrap().1;
                let new = fx.seq.find_item(c).unwrap().1;
                assert_eq!((new.source_in, new.source_out(), new.reverse), (old.source_in, old.source_out(), old.reverse));
            }
            fx.seq.check().unwrap();
        }
    }
}

/// A linked split-edit follower moves once, or a collision rejects the whole edit.
#[test]
fn split_edit_follower_moves_once_or_refuses_atomically() {
    for blocked in [false, true] {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        let main = fx.put(v1, 0, 60, 20);
        let later = fx.put(v1, 60, 30, 80);
        let lead = fx.put(a1, 50, 40, 70);
        for c in [later, lead] {
            fx.seq.find_item_mut(c).unwrap().1.link = Some(999);
        }
        if blocked {
            fx.put(a1, 0, 49, 0);
        }
        let before = fx.seq.clone();
        let r = stretch(&mut fx, &[main], -f(10));
        if blocked {
            assert!(r.is_err());
            assert_eq!(fx.seq, before);
        } else {
            r.unwrap();
            assert_eq!(fx.spans(v1), vec![(0, 50), (50, 30)]);
            assert_eq!(fx.spans(a1), vec![(40, 40)]);
        }
    }
}

/// Closing across an independent sync-locked clip refuses without changing the sequence.
#[test]
fn sync_locked_material_across_closing_region_blocks_without_mutation() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let clip = fx.put(v1, 0, 60, 20);
    fx.put(v1, 60, 30, 80);
    fx.put(a1, 0, 100, 0);
    let before = fx.seq.clone();
    assert!(matches!(stretch(&mut fx, &[clip], -f(10)), Err(EditError::SyncLockConflict(_))));
    assert_eq!(fx.seq, before);
}

/// Locked members stay fixed; a selection containing only locked members cannot stretch.
#[test]
fn locked_partners_stay_fixed_and_all_locked_refuses() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let pic = fx.put(v1, 0, 60, 20);
    let sound = fx.put(a1, 0, 60, 20);
    fx.seq.track_mut(a1).unwrap().locked = true;
    let locked = fx.seq.track(a1).unwrap().clone();
    stretch(&mut fx, &[pic, sound], f(10)).unwrap();
    assert_eq!(fx.seq.track(a1).unwrap(), &locked);
    fx.seq.track_mut(v1).unwrap().locked = true;
    let before = fx.seq.clone();
    assert_eq!(stretch(&mut fx, &[pic, sound], f(10)), Err(EditError::Locked));
    assert_eq!(fx.seq, before);
}

/// Differently edited linked members cannot be retimed as one aligned group.
#[test]
fn unequal_or_offset_linked_members_are_refused() {
    for change in [0, 1, 2, 3] {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        let pic = fx.put(v1, 20, 60, 20);
        let snd = fx.put(a1, 20, 60, 20);
        let i = fx.seq.find_item_mut(snd).unwrap().1;
        match change {
            0 => i.start += f(1),
            1 => i.duration -= f(1),
            2 => i.speed = 2.0,
            _ => i.reverse = true,
        }
        let before = fx.seq.clone();
        assert!(stretch(&mut fx, &[pic, snd], f(10)).is_err());
        assert_eq!(fx.seq, before);
    }
}

/// Invalid ids and overflow refuse, while repeated bounded edits retain precise source ranges.
#[test]
fn bounds_and_missing_ids_are_safe_and_rate_keeps_precision() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let clip = fx.put(v1, 0, 60, 10);
    fx.put(v1, 60, 10, 0);
    let before = fx.seq.clone();
    assert!(stretch(&mut fx, &[clip, ClipId(999)], f(1)).is_err());
    assert!(stretch(&mut fx, &[clip], Tick(i64::MAX)).is_err());
    assert_eq!(fx.seq, before);
    stretch(&mut fx, &[clip], Tick(i64::MIN)).unwrap();
    assert_eq!(fx.seq.find_item(clip).unwrap().1.duration, f(1));
    assert_eq!(fx.spans(v1), vec![(0, 1), (1, 10)]);
    for _ in 0..50 {
        stretch(&mut fx, &[clip], f(6)).unwrap();
        stretch(&mut fx, &[clip], -f(6)).unwrap();
    }
    let i = fx.seq.find_item(clip).unwrap().1;
    assert_eq!((i.source_in, i.source_out(), i.duration), (f(10), f(70), f(1)));
}

/// Transitions follow the moved cut and fades fit within a shortened clip.
#[test]
fn transitions_follow_changed_cut_and_fades_fit_new_duration() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let main = fx.put(v1, 0, 60, 30);
    let next = fx.put(v1, 60, 40, 100);
    let transition = |id, start, from, to| Transition {
        id: TransitionId(id),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(start),
        duration: f(6),
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    fx.seq.track_mut(v1).unwrap().transitions = vec![transition(1, 57, Some(main), Some(next)), transition(2, 54, Some(main), None)];
    stretch(&mut fx, &[main], -f(58)).unwrap();
    let tr = fx.seq.track(v1).unwrap();
    let cut = tr.transitions.iter().find(|t| t.id == TransitionId(1)).unwrap();
    let fade = tr.transitions.iter().find(|t| t.id == TransitionId(2)).unwrap();
    assert_eq!((cut.start, cut.duration), (f(0), f(6)), "transition stays within its clips when the head gets shorter");
    assert_eq!((fade.start, fade.duration), (f(0), f(2)), "fade capped to new clip");
}

/// Affected caption tracks block the edit unless locked or excluded from sync lock.
#[test]
fn affected_sync_locked_captions_block_until_explicitly_excluded() {
    for delta in [-10, 10] {
        for (caption_start, locked, sync_lock, allowed) in
            [(0, false, true, true), (25, false, true, false), (85, false, true, false), (85, true, true, true), (85, false, false, true)]
        {
            let mut fx = Fx::new();
            let v1 = fx.v(0);
            let main = fx.put(v1, 20, 60, 20);
            let mut tr = filmcraft_project::CaptionTrack::new(TrackId(20000), "Subtitles".into(), Default::default());
            tr.locked = locked;
            tr.sync_lock = sync_lock;
            tr.captions.push(filmcraft_project::Caption {
                id: ClipId(20001),
                start: f(caption_start),
                duration: f(10),
                text: "Synthetic".into(),
                speaker: None,
                cue_id: None,
                settings: String::new(),
            });
            fx.seq.caption_tracks.push(tr);
            let before = fx.seq.clone();
            let result = stretch(&mut fx, &[main], f(delta));
            assert_eq!(result.is_ok(), allowed);
            assert_eq!(fx.seq.caption_tracks, before.caption_tracks);
            if !allowed {
                assert_eq!(fx.seq, before);
            }
        }
    }
}
