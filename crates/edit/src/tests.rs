use super::*;
use filmcraft_project::{Label, Project, SequenceSettings, TrackKind};
use filmcraft_time::FrameRate;
use proptest::prelude::*;

const R: FrameRate = FrameRate::FPS_24;

fn f(n: i64) -> Tick {
    R.tick_of(n)
}

struct Fx {
    seq: Sequence,
    next: u64,
}

fn media(_: ItemId) -> Option<Tick> {
    Some(f(1000))
}

impl Fx {
    fn new() -> Self {
        let mut p = Project::new("t");
        let s = p.new_sequence("s", SequenceSettings { frame_rate: R, ..Default::default() }, 3, 2, None);
        let seq = p.sequence(s).unwrap().clone();
        Self { seq, next: 10_000 }
    }
    fn ctx<'a>(next: &'a mut u64) -> EditCtx<'a> {
        EditCtx { next_id: next, media_duration: &media, media_start: &|_| Tick::ZERO, min_duration: f(1) }
    }
    fn v(&self, i: usize) -> TrackId {
        self.seq.video_tracks[i].id
    }
    fn a(&self, i: usize) -> TrackId {
        self.seq.audio_tracks[i].id
    }
    fn item(&mut self, start: i64, dur: i64, src_in: i64) -> TrackItem {
        self.next += 1;
        TrackItem {
            id: ClipId(self.next),
            item: ItemId(1),
            name: format!("c{}", self.next),
            label: Label::Iris,
            start: f(start),
            duration: f(dur),
            source_in: f(src_in),
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: vec![],
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
    fn put(&mut self, track: TrackId, start: i64, dur: i64, src_in: i64) -> ClipId {
        let it = self.item(start, dur, src_in);
        let id = it.id;
        let mut n = self.next;
        overwrite(&mut self.seq, vec![(track, it)], &mut Self::ctx(&mut n)).unwrap();
        self.next = n;
        id
    }
    fn spans(&self, track: TrackId) -> Vec<(i64, i64)> {
        self.seq.track(track).unwrap().items.iter().map(|i| (R.frame_at(i.start), R.frame_at(i.duration))).collect()
    }
}

#[test]
fn overwrite_splits_underlying() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    fx.put(v1, 0, 100, 0);
    fx.put(v1, 40, 20, 500);
    assert_eq!(fx.spans(v1), vec![(0, 40), (40, 20), (60, 40)]);
    let right = &fx.seq.track(v1).unwrap().items[2];
    assert_eq!(right.source_in, f(60), "right remainder keeps media continuity");
    fx.seq.check().unwrap();
}

#[test]
fn insert_ripples_sync_locked_tracks() {
    let mut fx = Fx::new();
    let (v1, v2, a1) = (fx.v(0), fx.v(1), fx.a(0));
    fx.put(v1, 0, 50, 0);
    fx.put(v2, 60, 10, 0);
    fx.put(a1, 0, 100, 0);
    fx.seq.track_mut(v2).unwrap().sync_lock = false;
    let it = fx.item(20, 10, 0);
    let mut n = fx.next;
    insert(&mut fx.seq, vec![(v1, it)], &mut Fx::ctx(&mut n)).unwrap();
    fx.next = n;
    assert_eq!(fx.spans(v1), vec![(0, 20), (20, 10), (30, 30)]);
    assert_eq!(fx.spans(a1), vec![(0, 20), (30, 80)], "sync-locked audio split and shifted");
    assert_eq!(fx.spans(v2), vec![(60, 10)], "not sync-locked: untouched");
}

#[test]
fn razor_keeps_content_continuous() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let c = fx.put(v1, 10, 100, 200);
    let before = fx.seq.find_item(c).unwrap().1.source_time_at(f(70));
    let mut n = fx.next;
    let new = razor(&mut fx.seq, &[], f(50), &mut Fx::ctx(&mut n));
    assert_eq!(new.len(), 1);
    let right = fx.seq.find_item(new[0]).unwrap().1;
    assert_eq!(right.source_time_at(f(70)), before);
    assert_eq!(fx.spans(v1), vec![(10, 40), (50, 60)]);
}

#[test]
fn extract_and_lift() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    fx.put(v1, 0, 100, 0);
    fx.put(a1, 0, 100, 0);
    let mut n = fx.next;
    lift(&mut fx.seq, &[v1], TimeRange::new(f(10), f(10)), &mut Fx::ctx(&mut n));
    assert_eq!(fx.spans(v1), vec![(0, 10), (20, 80)]);
    extract(&mut fx.seq, &[v1], TimeRange::new(f(30), f(10)), &mut Fx::ctx(&mut n));
    assert_eq!(fx.spans(v1), vec![(0, 10), (20, 10), (30, 60)]);
    assert_eq!(fx.spans(a1), vec![(0, 30), (30, 60)]);
}

#[test]
fn ripple_delete_and_conflict() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let a = fx.put(v1, 0, 10, 0);
    fx.put(v1, 10, 10, 0);
    ripple_delete_items(&mut fx.seq, &[a]).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 10)]);
    // audio under the gap blocks the ripple
    let b = fx.put(v1, 20, 10, 0);
    fx.put(a1, 22, 3, 0);
    assert_eq!(ripple_delete_items(&mut fx.seq, &[b]), Err(EditError::SyncLockConflict("A1".into())), "names the track in the way");
    assert_eq!(fx.spans(v1), vec![(0, 10), (20, 10)], "unchanged on failure");
}

#[test]
fn ripple_delete_closes_the_time_free_on_every_track() {
    // four clips on V1 with their sounds on A1; the second sound starts 2 early and ends 2 late
    let build = || {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        let v: Vec<ClipId> = [(0, 10), (10, 10), (20, 10), (30, 10)].iter().map(|(s, d)| fx.put(v1, *s, *d, 0)).collect();
        let a: Vec<ClipId> = [(0, 8), (8, 14), (22, 8), (30, 10)].iter().map(|(s, d)| fx.put(a1, *s, *d, 0)).collect();
        (fx, v, a)
    };
    let span = |start, len| TimeRange::new(f(start), f(len));
    // one clip with a split edit on both sides: the picture's stretch closes, everything later stays in sync
    let (mut fx, v, a) = build();
    assert_eq!(ripple_delete_items(&mut fx.seq, &[v[1], a[1]]), Ok(vec![span(10, 10)]));
    assert_eq!(fx.spans(fx.v(0)), vec![(0, 10), (10, 10), (20, 10)]);
    assert_eq!(fx.spans(fx.a(0)), vec![(0, 8), (12, 8), (20, 10)]);
    fx.seq.check().unwrap();
    // two neighbours with a split edit between them close as one stretch
    let (mut fx, v, a) = build();
    assert_eq!(ripple_delete_items(&mut fx.seq, &[v[1], a[1], v[2], a[2]]), Ok(vec![span(10, 20)]));
    assert_eq!(fx.spans(fx.v(0)), vec![(0, 10), (10, 10)]);
    assert_eq!(fx.spans(fx.a(0)), vec![(0, 8), (10, 10)]);
    // clips apart in time each close their own gap
    let (mut fx, v, a) = build();
    assert_eq!(ripple_delete_items(&mut fx.seq, &[v[0], a[0], v[3], a[3]]), Ok(vec![span(0, 8), span(30, 10)]));
    fx.seq.check().unwrap();

    // straight cuts with the sounds on alternating tracks: V1 [0,10) [10,20) [20,30), A1 [0,10) [20,30), A2 [10,20)
    let mut fx = Fx::new();
    let (v1, a1, a2) = (fx.v(0), fx.a(0), fx.a(1));
    let v: Vec<ClipId> = [0, 10, 20].iter().map(|s| fx.put(v1, *s, 10, 0)).collect();
    let (s0, s1) = (fx.put(a1, 0, 10, 0), fx.put(a2, 10, 10, 0));
    fx.put(a1, 20, 10, 0);
    assert_eq!(ripple_delete_items(&mut fx.seq, &[v[0], s0, v[1], s1]), Ok(vec![span(0, 20)]));
    assert_eq!((fx.spans(v1), fx.spans(a1), fx.spans(a2)), (vec![(0, 10)], vec![(0, 10)], vec![]));

    // a sound shorter than its picture with nothing round it: the whole picture closes
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let (p, s) = (fx.put(v1, 10, 10, 0), fx.put(a1, 12, 6, 0));
    fx.put(v1, 20, 10, 0);
    fx.put(a1, 20, 10, 0);
    assert_eq!(ripple_delete_items(&mut fx.seq, &[p, s]), Ok(vec![span(10, 10)]));
    // with the neighbours' sound right up against it, only the sound's stretch can close
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    fx.put(a1, 0, 12, 0);
    let (p, s) = (fx.put(v1, 10, 10, 0), fx.put(a1, 12, 6, 0));
    fx.put(v1, 20, 10, 0);
    fx.put(a1, 18, 12, 0);
    assert_eq!(ripple_delete_items(&mut fx.seq, &[p, s]), Ok(vec![span(12, 6)]));
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(14, 10)], vec![(0, 12), (12, 12)]));

    // a linked sound on a locked track stays, and does not shrink the gap that closes
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let (p, s) = (fx.put(v1, 10, 10, 0), fx.put(a1, 12, 6, 0));
    fx.put(v1, 20, 10, 0);
    fx.seq.track_mut(a1).unwrap().locked = true;
    fx.seq.track_mut(a1).unwrap().sync_lock = false;
    assert_eq!(ripple_delete_items(&mut fx.seq, &[p, s]), Ok(vec![span(10, 10)]));
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(10, 10)], vec![(12, 6)]));

    // nothing can close: what stays on each track covers the other track's deleted time. Refused, unchanged.
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let x = fx.put(v1, 10, 10, 0);
    fx.put(v1, 30, 10, 0);
    fx.put(a1, 10, 10, 0);
    let y = fx.put(a1, 30, 10, 0);
    let before = fx.seq.clone();
    assert!(matches!(ripple_delete_items(&mut fx.seq, &[x, y]), Err(EditError::Other(_))));
    assert_eq!(fx.seq, before);
}

#[test]
fn close_gap_works() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    fx.put(v1, 0, 10, 0);
    fx.put(v1, 25, 10, 0);
    close_gap(&mut fx.seq, v1, f(15)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 10), (10, 10)]);
}

#[test]
fn reversed_trim_uses_the_handle_its_playback_direction_consumes() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    // source frames [5, 15) played backwards; the media is 1000 frames long
    let a = fx.put(v1, 30, 10, 5);
    fx.seq.find_item_mut(a).unwrap().1.reverse = true;
    let mut n = fx.next;
    // out extends into the 5 frames before source_in, not into the 985 after source_out
    let d = trim(&mut fx.seq, a, Edge::Out, TrimMode::Regular, f(20), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, f(5));
    let it = fx.seq.find_item(a).unwrap().1;
    assert_eq!((it.source_in, it.source_out()), (f(0), f(15)));
    // in extends into the media after source_out
    let d = trim(&mut fx.seq, a, Edge::In, TrimMode::Regular, -f(10), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, -f(10));
    let it = fx.seq.find_item(a).unwrap().1;
    assert_eq!((it.source_in, it.source_out()), (f(0), f(25)));
    fx.seq.check().unwrap();
}

#[test]
fn regular_trim_respects_neighbours_and_handles() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 10, 5);
    fx.put(v1, 12, 10, 0);
    let mut n = fx.next;
    // out: only 2 frames of space before the neighbour
    let d = trim(&mut fx.seq, a, Edge::Out, TrimMode::Regular, f(5), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, f(2));
    // in: 5 frames of head handle, but item starts at 0 so no space to the left
    let d = trim(&mut fx.seq, a, Edge::In, TrimMode::Regular, -f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, Tick::ZERO);
    let d = trim(&mut fx.seq, a, Edge::In, TrimMode::Regular, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, f(3));
    assert_eq!(fx.seq.find_item(a).unwrap().1.source_in, f(8));
    fx.seq.check().unwrap();
}

#[test]
fn held_regular_in_trim_stops_at_previous_clip() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 15, 10, 5);
    fx.seq.find_item_mut(b).unwrap().1.frame_hold = Some(f(5));
    let mut n = fx.next;
    // 5 frames of gap: a 20 frame extension is clamped to 5 instead of overlapping the previous clip
    let d = trim(&mut fx.seq, b, Edge::In, TrimMode::Regular, -f(20), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, -f(5));
    assert_eq!(fx.spans(v1), vec![(0, 10), (10, 15)]);
    // no space left: nothing to do, and no error
    let d = trim(&mut fx.seq, b, Edge::In, TrimMode::Regular, -f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, Tick::ZERO);
    fx.seq.check().unwrap();
}

#[test]
fn ripple_trim_shifts_following() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let a = fx.put(v1, 0, 10, 0);
    fx.put(v1, 10, 10, 0);
    fx.put(a1, 30, 5, 0);
    let mut n = fx.next;
    trim(&mut fx.seq, a, Edge::Out, TrimMode::Ripple, f(4), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 14), (14, 10)]);
    assert_eq!(fx.spans(a1), vec![(34, 5)]);
    trim(&mut fx.seq, a, Edge::In, TrimMode::Ripple, f(2), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 12), (12, 10)]);
}

#[test]
fn ripple_trim_group_takes_a_split_edit_along() {
    // V1: a [0,10) then b [10,20). b's linked sound leads it by 4 on sync-locked A1: [6,20).
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let a = fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    let sound = fx.put(a1, 6, 14, 0);
    for c in [b, sound] {
        fx.seq.find_item_mut(c).unwrap().1.link = Some(7);
    }
    let mut n = fx.next;
    ripple_trim_group(&mut fx.seq, &[a], Edge::Out, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 13), (13, 10)], vec![(9, 14)]), "longer: the early sound follows its picture");
    ripple_trim_group(&mut fx.seq, &[a], Edge::Out, -f(5), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 8), (8, 10)], vec![(4, 14)]), "shorter: it follows too");
    // the sound would start before the sequence does: refused, nothing changes
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[a], Edge::Out, -f(5), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));
    // another clip right before the sound leaves it no room either
    fx.put(a1, 0, 4, 0);
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[a], Edge::Out, -f(2), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 8), (8, 10)], vec![(0, 4), (4, 14)]), "unchanged on failure");
    // a linked clip that ends before the cut is not a split edit and stays where it is
    let early = fx.put(fx.a(1), 0, 3, 0);
    fx.seq.find_item_mut(early).unwrap().1.link = Some(7);
    ripple_trim_group(&mut fx.seq, &[a], Edge::Out, f(2), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(fx.a(1)), vec![(0, 3)]);
    fx.seq.check().unwrap();

    // In edge: V1 b [10,20), c [20,30); c's sound starts 12 early on A1: [8,30). b loses 3 at its head.
    let mut fx = Fx::new();
    let (v1, v2, a1, a2) = (fx.v(0), fx.v(1), fx.a(0), fx.a(1));
    fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    let c = fx.put(v1, 20, 10, 0);
    let c_sound = fx.put(a1, 8, 22, 0);
    // two more clips linked to b that the caller did not put in the group
    let b_sound = fx.put(a2, 10, 10, 0);
    let b_top = fx.put(v2, 12, 8, 0);
    for (x, link) in [(c, 7), (c_sound, 7), (b, 8), (b_sound, 8), (b_top, 8)] {
        fx.seq.find_item_mut(x).unwrap().1.link = Some(link);
    }
    let mut n = fx.next;
    ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 10), (10, 7), (17, 10)], vec![(5, 22)]), "c and its early sound move up together");
    assert_eq!((fx.spans(v2), fx.spans(a2)), (vec![(9, 8)], vec![(10, 10)]), "the trimmed clip's own partners never follow");

    // a crossfade into the early sound from a clip that stays behind no longer sits on a cut: it goes
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let a = fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    let stays = fx.put(a1, 0, 6, 0);
    let sound = fx.put(a1, 6, 14, 0);
    for x in [b, sound] {
        fx.seq.find_item_mut(x).unwrap().1.link = Some(7);
    }
    fx.seq.track_mut(a1).unwrap().transitions = vec![Transition {
        id: TransitionId(1),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(4),
        duration: f(4),
        from: Some(stays),
        to: Some(sound),
        align: Default::default(),
        reverse: false,
    }];
    let mut n = fx.next;
    ripple_trim_group(&mut fx.seq, &[a], Edge::Out, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(a1), vec![(0, 6), (9, 14)]);
    assert!(fx.seq.track(a1).unwrap().transitions.is_empty());
}

#[test]
fn roll_slip_slide() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 10, 10);
    let b = fx.put(v1, 10, 10, 10);
    let c = fx.put(v1, 20, 10, 10);
    let mut n = fx.next;
    roll(&mut fx.seq, a, b, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 13), (13, 7), (20, 10)]);
    assert_eq!(fx.seq.find_item(b).unwrap().1.source_in, f(13));
    let d = slip(&mut fx.seq, c, -f(50), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(d, -f(10), "slip clamps at media start");
    slide(&mut fx.seq, b, f(2), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 15), (15, 7), (22, 8)]);
    fx.seq.check().unwrap();
}

#[test]
fn slide_keeps_reversed_neighbours_content() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 30, 20);
    let b = fx.put(v1, 30, 30, 50);
    let c = fx.put(v1, 60, 30, 70);
    for id in [a, c] {
        fx.seq.find_item_mut(id).unwrap().1.reverse = true;
    }
    let mut n = fx.next;
    slide(&mut fx.seq, b, f(10), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 40), (40, 30), (70, 20)]);
    assert_eq!(fx.seq.find_item(a).unwrap().1.source_in, f(10));
    assert_eq!(fx.seq.find_item(b).unwrap().1.source_in, f(50));
    assert_eq!(fx.seq.find_item(c).unwrap().1.source_in, f(70));
    fx.seq.check().unwrap();
}

#[test]
fn transitions_stay_on_their_cuts() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 10, 10);
    let b = fx.put(v1, 10, 10, 10);
    let c = fx.put(v1, 20, 10, 10);
    let cross = |id, start, from, to| Transition {
        id: TransitionId(id),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(start),
        duration: f(4),
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    let t = fx.seq.track_mut(v1).unwrap();
    t.transitions = vec![cross(1, 8, Some(a), Some(b)), cross(2, 18, Some(b), Some(c)), cross(3, 26, Some(c), None)];
    // every transition is centred on its cut (or ends with the clip it fades out)
    let check = |fx: &Fx, what: &str| {
        let t = fx.seq.track(v1).unwrap();
        for tr in &t.transitions {
            let cut = match tr.to {
                Some(to) => t.item(to).unwrap().start,
                None => t.item(tr.from.unwrap()).unwrap().end() - f(2),
            };
            assert_eq!(tr.start + f(2), cut, "{what}: transition {:?} left its cut", tr.id);
        }
        assert_eq!(t.transitions.len(), 3, "{what}");
    };
    let mut n = fx.next;
    trim(&mut fx.seq, a, Edge::Out, TrimMode::Ripple, f(4), &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "ripple trim out");
    trim(&mut fx.seq, b, Edge::In, TrimMode::Ripple, f(3), &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "ripple trim in");
    ripple_trim_group(&mut fx.seq, &[b], Edge::Out, -f(2), &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "ripple trim group");
    roll(&mut fx.seq, b, c, f(2), &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "roll");
    slide(&mut fx.seq, b, -f(3), &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "slide");
    set_speed(&mut fx.seq, b, 0.5, false, true, &mut Fx::ctx(&mut n)).unwrap();
    check(&fx, "ripple speed change");
    fx.seq.check().unwrap();
}

#[test]
fn fades_follow_a_regular_trim() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 10, 20, 10);
    let fade = |id, start, from, to| Transition {
        id: TransitionId(id),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(start),
        duration: f(6),
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    fx.seq.track_mut(v1).unwrap().transitions = vec![fade(1, 10, None, Some(a)), fade(2, 24, Some(a), None)];
    let spans = |fx: &Fx| fx.seq.track(v1).unwrap().transitions.iter().map(|t| (t.start, t.duration)).collect::<Vec<_>>();
    let mut n = fx.next;
    trim(&mut fx.seq, a, Edge::In, TrimMode::Regular, f(2), &mut Fx::ctx(&mut n)).unwrap();
    trim(&mut fx.seq, a, Edge::Out, TrimMode::Regular, f(4), &mut Fx::ctx(&mut n)).unwrap();
    // the fade in starts with the clip (12), the fade out ends with it (34)
    assert_eq!(spans(&fx), vec![(f(12), f(6)), (f(28), f(6))]);
    // a clip trimmed shorter than its fades keeps them inside it
    trim(&mut fx.seq, a, Edge::Out, TrimMode::Regular, -f(18), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(spans(&fx), vec![(f(12), f(4)), (f(12), f(4))]);
    fx.seq.check().unwrap();
}

#[test]
fn transitions_travel_with_moved_clips() {
    // V1: a [0,10) fading in, b [10,20), c [20,30); b→c crossfade, c fading out
    let mut fx = Fx::new();
    let (v1, v2) = (fx.v(0), fx.v(1));
    let a = fx.put(v1, 0, 10, 10);
    let b = fx.put(v1, 10, 10, 10);
    let c = fx.put(v1, 20, 10, 10);
    let tr = |id, start, from, to| Transition {
        id: TransitionId(id),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(start),
        duration: f(4),
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    fx.seq.track_mut(v1).unwrap().transitions = vec![tr(1, 0, None, Some(a)), tr(2, 18, Some(b), Some(c)), tr(3, 26, Some(c), None)];
    let at = |fx: &Fx, track: TrackId| -> Vec<(u64, i64)> { fx.seq.track(track).unwrap().transitions.iter().map(|t| (t.id.0, R.frame_at(t.start))).collect() };
    let mut n = fx.next;
    // a fade at a clip's edge goes with it, to another track too
    move_items(&mut fx.seq, &[(a, v2, f(40))], false, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((at(&fx, v1), at(&fx, v2)), (vec![(2, 18), (3, 26)], vec![(1, 40)]));
    // a cut whose two clips move together keeps its transition, in an insert move as well
    move_items(&mut fx.seq, &[(b, v1, f(60)), (c, v1, f(70))], true, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(at(&fx, v1), vec![(2, 68), (3, 76)]);
    // a crossfade whose partner stays behind is dropped; the other clip's fade stays with it
    move_items(&mut fx.seq, &[(c, v1, f(90))], false, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(at(&fx, v1), vec![(3, 96)]);
    fx.seq.check().unwrap();
}

#[test]
fn rate_stretch_and_speed() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 10, 0);
    let mut n = fx.next;
    let s = rate_stretch(&mut fx.seq, a, Edge::Out, f(10), &mut Fx::ctx(&mut n)).unwrap();
    assert!((s - 0.5).abs() < 1e-9);
    set_speed(&mut fx.seq, a, 2.0, false, true, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![(0, 5)]);
}

#[test]
fn speed_on_a_linked_group_ripples_once() {
    // V1: a [0,10), b [10,20). A1: a's sound [0,10), b's sound [10,20).
    let build = |sound_len: i64| {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        let a = fx.put(v1, 0, 10, 0);
        fx.put(v1, 10, 10, 0);
        let sound = fx.put(a1, 0, sound_len, 0);
        fx.put(a1, sound_len, 10, 0);
        (fx, a, sound)
    };
    for (speed, picture, later) in [(0.5, (0, 20), (20, 10)), (2.0, (0, 5), (5, 10))] {
        let (mut fx, a, sound) = build(10);
        let mut n = fx.next;
        set_speed_group(&mut fx.seq, &[a, sound], speed, false, true, &mut Fx::ctx(&mut n)).unwrap();
        assert_eq!((fx.spans(fx.v(0)), fx.spans(fx.a(0))), (vec![picture, later], vec![picture, later]), "{speed}");
    }
    // a split edit: the sound is 4 longer. Later clips move by the larger change, on both tracks.
    let (mut fx, a, sound) = build(14);
    let mut n = fx.next;
    set_speed_group(&mut fx.seq, &[a, sound], 0.5, false, true, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(fx.v(0)), fx.spans(fx.a(0))), (vec![(0, 20), (24, 10)], vec![(0, 28), (28, 10)]));
    let (mut fx, a, sound) = build(14);
    let mut n = fx.next;
    set_speed_group(&mut fx.seq, &[a, sound], 2.0, false, true, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(fx.v(0)), fx.spans(fx.a(0))), (vec![(0, 5), (5, 10)], vec![(0, 7), (9, 10)]));
    fx.seq.check().unwrap();
    // two clips of one track are not one group; a missing clip changes nothing
    let (mut fx, a, _) = build(10);
    let b = fx.seq.video_tracks[0].items[1].id;
    let before = fx.seq.clone();
    let mut n = fx.next;
    assert!(set_speed_group(&mut fx.seq, &[a, b], 2.0, false, true, &mut Fx::ctx(&mut n)).is_err());
    assert!(set_speed_group(&mut fx.seq, &[a, ClipId(1)], 2.0, false, true, &mut Fx::ctx(&mut n)).is_err());
    assert_eq!(fx.seq, before);
}

#[test]
fn move_with_insert_and_overwrite() {
    let mut fx = Fx::new();
    let (v1, v2) = (fx.v(0), fx.v(1));
    let a = fx.put(v1, 0, 10, 0);
    fx.put(v2, 0, 30, 0);
    let mut n = fx.next;
    move_items(&mut fx.seq, &[(a, v2, f(5))], false, &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!(fx.spans(v1), vec![]);
    assert_eq!(fx.spans(v2), vec![(0, 5), (5, 10), (15, 15)]);
}

#[derive(Debug, Clone)]
enum Op {
    Over(usize, i64, i64),
    Ins(usize, i64, i64),
    Razor(i64),
    Extract(i64, i64),
    TrimOut(usize, i64),
    Ripple(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..3, 0i64..200, 1i64..50).prop_map(|(t, s, d)| Op::Over(t, s, d)),
        (0usize..3, 0i64..200, 1i64..50).prop_map(|(t, s, d)| Op::Ins(t, s, d)),
        (0i64..250).prop_map(Op::Razor),
        (0i64..200, 1i64..30).prop_map(|(s, d)| Op::Extract(s, d)),
        (0usize..20, -20i64..20).prop_map(|(i, d)| Op::TrimOut(i, d)),
        (0usize..20).prop_map(Op::Ripple),
    ]
}

proptest! {
    #[test]
    fn never_overlaps(ops in proptest::collection::vec(op(), 1..40)) {
        let mut fx = Fx::new();
        let tracks = [fx.v(0), fx.v(1), fx.a(0)];
        for o in ops {
            let mut n = fx.next;
            match o {
                Op::Over(t, s, d) => { let it = fx.item(s, d, 0); n = fx.next; let _ = overwrite(&mut fx.seq, vec![(tracks[t], it)], &mut Fx::ctx(&mut n)); }
                Op::Ins(t, s, d) => { let it = fx.item(s, d, 0); n = fx.next; let _ = insert(&mut fx.seq, vec![(tracks[t], it)], &mut Fx::ctx(&mut n)); }
                Op::Razor(t) => { razor(&mut fx.seq, &[], f(t), &mut Fx::ctx(&mut n)); }
                Op::Extract(s, d) => { extract(&mut fx.seq, &[tracks[0]], TimeRange::new(f(s), f(d)), &mut Fx::ctx(&mut n)); }
                Op::TrimOut(i, d) => {
                    let ids: Vec<ClipId> = fx.seq.all_tracks().flat_map(|t| t.items.iter().map(|x| x.id)).collect();
                    if let Some(c) = ids.get(i) { let _ = trim(&mut fx.seq, *c, Edge::Out, TrimMode::Regular, f(d), &mut Fx::ctx(&mut n)); }
                }
                Op::Ripple(i) => {
                    let ids: Vec<ClipId> = fx.seq.all_tracks().flat_map(|t| t.items.iter().map(|x| x.id)).collect();
                    if let Some(c) = ids.get(i) { let _ = ripple_delete_items(&mut fx.seq, &[*c]); }
                }
            }
            fx.next = fx.next.max(n);
            prop_assert!(fx.seq.check().is_ok(), "{:?}", fx.seq.check());
            for t in fx.seq.all_tracks() { for i in &t.items { prop_assert!(i.start >= Tick::ZERO); } }
        }
    }

    #[test]
    fn insert_then_extract_is_identity(s in 0i64..100, d in 1i64..40) {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        fx.put(v1, 0, 60, 0);
        fx.put(v1, 70, 30, 100);
        fx.put(a1, 10, 80, 0);
        let before: Vec<Vec<(i64, i64)>> = [v1, a1].iter().map(|t| fx.spans(*t)).collect();
        let it = fx.item(s, d, 0);
        let mut n = fx.next;
        insert(&mut fx.seq, vec![(v1, it)], &mut Fx::ctx(&mut n)).unwrap();
        extract(&mut fx.seq, &[v1], TimeRange::new(f(s), f(d)), &mut Fx::ctx(&mut n));
        // content ranges are identical after merging split pieces
        for (k, t) in [v1, a1].iter().enumerate() {
            let merged = merge(fx.spans(*t));
            prop_assert_eq!(merged, merge(before[k].clone()));
        }
    }
}

fn merge(v: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, d) in v {
        if let Some(last) = out.last_mut()
            && last.0 + last.1 == s
        {
            last.1 += d;
            continue;
        }
        out.push((s, d));
    }
    out
}

#[test]
fn unused_kind_import() {
    let _ = TrackKind::Video;
}

#[test]
fn hostile_speeds_are_refused_or_clamped_never_overflow() {
    // a tiny speed used to saturate the new duration to i64::MAX, then the ripple shift overflowed
    for speed in [1e-7, 1e-300, f64::MIN_POSITIVE, f64::NAN, f64::INFINITY, -1.0, 0.0] {
        let mut fx = Fx::new();
        let v1 = fx.v(0);
        let a = fx.put(v1, 0, 10, 10);
        let _b = fx.put(v1, 10, 10, 10);
        let before = fx.seq.clone();
        let mut n = fx.next;
        let r = set_speed_group(&mut fx.seq, &[a], speed, false, true, &mut Fx::ctx(&mut n));
        match r {
            Ok(()) => fx.seq.check().unwrap(),
            Err(_) => assert_eq!(format!("{:?}", fx.seq), format!("{before:?}"), "{speed}: a refused edit changes nothing"),
        }
    }
}

#[test]
fn shorter_head_into_an_l_cut_keeps_the_earlier_sound_where_it_is() {
    // V1: a [0,10), b [10,20), c [20,30). a's sound on sync-locked A1 runs 3 into b: [0,13) (an L
    // cut). b loses 3 at its head: a never moves, so its sound stays and the L cut still ends at 13.
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    let a = fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    fx.put(v1, 20, 10, 0);
    let a_sound = fx.put(a1, 0, 13, 0);
    for x in [a, a_sound] {
        fx.seq.find_item_mut(x).unwrap().1.link = Some(7);
    }
    let mut n = fx.next;
    ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 10), (10, 7), (17, 10)], vec![(0, 13)]));
    fx.seq.check().unwrap();

    // later material on A1 moves up with the pictures; landing on the L cut's sound refuses
    let later = fx.put(a1, 15, 5, 0);
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));
    assert_eq!(fx.spans(a1), vec![(0, 13), (15, 5)], "unchanged on failure");
    fx.seq.find_item_mut(later).unwrap().1.start = f(20);
    ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(2), &mut Fx::ctx(&mut n)).unwrap();
    assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 10), (10, 5), (15, 10)], vec![(0, 13), (18, 5)]));

    // unlinked material across the cut (a music bed) still refuses
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    fx.put(a1, 0, 13, 0);
    let mut n = fx.next;
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));
    // so does a link with no partner left
    fx.seq.track_mut(a1).unwrap().items[0].link = Some(9);
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));

    // a linked cutaway across the cut whose own partner reaches past it too is no L cut: it refuses
    let mut fx = Fx::new();
    let (v1, v2, a1) = (fx.v(0), fx.v(1), fx.a(0));
    fx.put(v1, 0, 10, 0);
    let b = fx.put(v1, 10, 10, 0);
    fx.put(v1, 20, 10, 0);
    let x = fx.put(v2, 5, 20, 0);
    let xs = fx.put(a1, 5, 20, 0);
    for c in [x, xs] {
        fx.seq.find_item_mut(c).unwrap().1.link = Some(7);
    }
    let mut n = fx.next;
    assert!(matches!(ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)), Err(EditError::SyncLockConflict(_))));
}

#[test]
fn shorter_head_closes_the_stretch_after_the_cut_on_sync_locked_tracks() {
    // V1: a [0,10), b [10,20). a's sound on sync-locked A1 runs up to the cut: [0,10). b loses 3 at
    // its head; the stretch that closes is [10,13), where A1 is empty, so nothing refuses.
    for group in [false, true] {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        fx.put(v1, 0, 10, 0);
        let b = fx.put(v1, 10, 10, 0);
        fx.put(v1, 20, 10, 0);
        fx.put(a1, 0, 10, 0);
        let later = fx.put(a1, 20, 5, 0);
        let mut n = fx.next;
        if group {
            ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n)).unwrap();
        } else {
            trim(&mut fx.seq, b, Edge::In, TrimMode::Ripple, f(3), &mut Fx::ctx(&mut n)).unwrap();
        }
        assert_eq!((fx.spans(v1), fx.spans(a1)), (vec![(0, 10), (10, 7), (17, 10)], vec![(0, 10), (17, 5)]), "group {group}");
        // material inside the stretch that closes still refuses
        fx.seq.find_item_mut(later).unwrap().1.start = f(11);
        let r = if group {
            ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(2), &mut Fx::ctx(&mut n))
        } else {
            trim(&mut fx.seq, b, Edge::In, TrimMode::Ripple, f(2), &mut Fx::ctx(&mut n))
        };
        assert!(matches!(r, Err(EditError::SyncLockConflict(_))), "group {group}");
    }
    // a clip on a sync-locked track that starts at the cut stays put while the pictures after it move
    // up: only the stretch after the cut sees it (the old stretch, before the cut, let it through)
    for group in [false, true] {
        let mut fx = Fx::new();
        let (v1, a1) = (fx.v(0), fx.a(0));
        fx.put(v1, 0, 10, 0);
        let b = fx.put(v1, 10, 10, 0);
        fx.put(v1, 20, 10, 0);
        fx.put(a1, 10, 5, 0);
        let mut n = fx.next;
        let r = if group {
            ripple_trim_group(&mut fx.seq, &[b], Edge::In, f(3), &mut Fx::ctx(&mut n))
        } else {
            trim(&mut fx.seq, b, Edge::In, TrimMode::Ripple, f(3), &mut Fx::ctx(&mut n))
        };
        assert!(matches!(r, Err(EditError::SyncLockConflict(_))), "group {group}");
        assert_eq!(fx.spans(v1), vec![(0, 10), (10, 10), (20, 10)], "unchanged on failure");
    }
}

#[test]
fn roll_reversed_clips_keeps_media_mapping() {
    let mut fx = Fx::new();
    let v = fx.v(0);
    let mut a = fx.item(0, 30, 20);
    a.reverse = true;
    let mut b = fx.item(30, 30, 50);
    b.reverse = true;
    let (ia, ib) = (a.id, b.id);
    fx.seq.track_mut(v).unwrap().items.extend([a, b]);
    let mut n = 0;
    roll(&mut fx.seq, ia, ib, f(10), &mut Fx::ctx(&mut n)).unwrap();
    let (_, l) = fx.seq.find_item(ia).unwrap();
    assert_eq!((l.start, l.duration, l.source_in), (f(0), f(40), f(10)));
    let (_, r) = fx.seq.find_item(ib).unwrap();
    assert_eq!((r.start, r.duration, r.source_in), (f(40), f(20), f(50)));
}

/// #374: moving an edge must not change which media sits under any timeline time the clip still
/// covers (the timeline's live trim preview draws its waveform from this item), and must leave the
/// item as the trim itself does.
#[test]
fn moving_an_edge_keeps_the_media_under_the_timeline() {
    let fx = Fx::new();
    let v1 = fx.v(0);
    for (speed, reverse, hold) in [(1.0, false, None), (2.0, false, None), (0.5, false, None), (1.0, true, None), (2.0, true, None), (1.0, false, Some(f(50)))]
    {
        for (edge, d) in [(Edge::In, f(7)), (Edge::In, -f(4)), (Edge::Out, -f(9)), (Edge::Out, f(3))] {
            let mut fresh = Fx::new();
            let c = fresh.put(v1, 100, 40, 200);
            {
                let it = fresh.seq.find_item_mut(c).unwrap().1;
                (it.speed, it.reverse, it.frame_hold) = (speed, reverse, hold);
            }
            let before = fresh.seq.find_item(c).unwrap().1.clone();
            let mut moved = before.clone();
            move_edge(&mut moved, edge, d, true);
            let label = format!("speed {speed} reverse {reverse} hold {hold:?} {edge:?} {d:?}");
            // every frame still covered shows the same media as before the move
            let mut t = moved.start.max(before.start);
            while t < moved.end().min(before.end()) {
                let (a, b) = (before.source_time_at(t), moved.source_time_at(t));
                assert!((a - b).0.abs() <= 1, "{label}: at {t:?} {a:?} became {b:?}");
                t += f(1);
            }
            // and the item is what the real (unclamped) trim makes of it
            let mut n = fresh.next;
            let got = trim(&mut fresh.seq, c, edge, TrimMode::Regular, d, &mut Fx::ctx(&mut n)).unwrap();
            assert_eq!(got, d, "{label}: not clamped");
            let trimmed = fresh.seq.find_item(c).unwrap().1;
            assert_eq!((trimmed.start, trimmed.duration, trimmed.source_in), (moved.start, moved.duration, moved.source_in), "{label}");
        }
    }
}
