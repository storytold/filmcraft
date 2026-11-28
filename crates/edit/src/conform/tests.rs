//! Tests of [`crate::conform`]: holds on extension, removal on trim, the kept clip, locked and
//! sync-lock-off tracks, transitions in the way, and the whole-operation invariants as properties.

use super::*;
use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ItemId, Label, Project, SequenceSettings, TrackId, TransitionId};
use filmcraft_time::FrameRate;
use proptest::prelude::*;

const R: FrameRate = FrameRate::FPS_24;

fn f(n: i64) -> Tick {
    R.tick_of(n)
}

fn media(_: ItemId) -> Option<Tick> {
    Some(f(100_000))
}

struct Fx {
    seq: Sequence,
    next: u64,
}

impl Fx {
    /// Three video tracks, three audio tracks and one caption track.
    fn new() -> Self {
        let mut p = Project::new("t");
        let s = p.new_sequence("s", SequenceSettings { frame_rate: R, ..Default::default() }, 3, 3, None);
        let mut seq = p.sequence(s).unwrap().clone();
        seq.caption_tracks.push(CaptionTrack::new(TrackId(9_000), "Subtitles".into(), CaptionFormat::Subtitle));
        Self { seq, next: 10_000 }
    }
    fn ctx(next: &mut u64) -> EditCtx<'_> {
        EditCtx { next_id: next, media_duration: &media, media_start: &|_| Tick::ZERO, min_duration: f(1) }
    }
    fn v(&self, i: usize) -> TrackId {
        self.seq.video_tracks[i].id
    }
    fn a(&self, i: usize) -> TrackId {
        self.seq.audio_tracks[i].id
    }
    fn put(&mut self, track: TrackId, start: i64, dur: i64, src_in: i64) -> ClipId {
        let id = ClipId(self.next);
        self.next += 1;
        let it = TrackItem {
            custom_label_color: None,
            id,
            item: ItemId(1),
            name: format!("c{}", id.0),
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
            maintain_pitch: false,
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        };
        let t = self.seq.track_mut(track).unwrap();
        t.items.push(it);
        t.sort();
        id
    }
    fn caption(&mut self, start: i64, dur: i64, text: &str) {
        let id = ClipId(self.next);
        self.next += 1;
        let c = Caption { id, start: f(start), duration: f(dur), text: text.into(), speaker: None, cue_id: None, settings: String::new() };
        let t = &mut self.seq.caption_tracks[0];
        t.captions.push(c);
        t.sort();
    }
    fn conform(&mut self, fits: &[(i64, i64)], keep: Option<ClipId>) -> Result<()> {
        let fits: Vec<Fit> = fits.iter().map(|&(end, d)| Fit { end: f(end), delta: f(d) }).collect();
        let mut n = self.next;
        let r = conform(&mut self.seq, &fits, keep, &mut Self::ctx(&mut n));
        self.next = n;
        r
    }
    /// (start, duration, source in, held frame) of each item on a track, in frames.
    fn spans(&self, track: TrackId) -> Vec<(i64, i64, i64, Option<i64>)> {
        self.seq
            .track(track)
            .unwrap()
            .items
            .iter()
            .map(|i| (R.frame_at(i.start), R.frame_at(i.duration), R.frame_at(i.source_in), i.frame_hold.map(|h| R.frame_at(h))))
            .collect()
    }
}

/// A sequence's content without clip ids, which depend on the order edits allocate them in.
fn shape(seq: &Sequence) -> Vec<Vec<(Tick, Tick, Tick, Option<Tick>, bool, Option<u64>)>> {
    let mut v: Vec<_> = seq
        .all_tracks()
        .map(|t| t.items.iter().map(|i| (i.start, i.duration, i.source_in, i.frame_hold, i.hold_filters, i.link.map(|_| 1))).collect())
        .collect();
    v.extend(seq.caption_tracks.iter().map(|t| t.captions.iter().map(|c| (c.start, c.duration, Tick::ZERO, None, false, None)).collect()));
    v
}

#[test]
fn a_longer_section_holds_each_video_tracks_last_frame_between_the_halves_of_a_split_clip() {
    let mut fx = Fx::new();
    let (v1, v2) = (fx.v(0), fx.v(1));
    fx.put(v1, 0, 100, 500); // spans the section end at 40
    fx.put(v2, 20, 20, 900); // ends at the section end
    fx.put(v2, 60, 10, 0); // later: moves
    fx.conform(&[(40, 12)], None).unwrap();

    let v1s = fx.spans(v1);
    assert_eq!(v1s.len(), 3, "the clip is split and the hold goes between the halves: {v1s:?}");
    assert_eq!((v1s[0].0, v1s[0].1), (0, 40), "the first half ends at the section end");
    assert_eq!((v1s[1].0, v1s[1].1, v1s[1].3), (40, 12, Some(539)), "the hold fills the added time with the last frame before the end");
    assert_eq!((v1s[2].0, v1s[2].2), (52, 540), "the second half moves on by the added time and shows what it showed before");
    let v2s = fx.spans(v2);
    assert_eq!((v2s[1].0, v2s[1].1, v2s[1].3), (40, 12, Some(919)), "a clip ending at the section end is held after it");
    assert_eq!(v2s[2].0, 72, "later clips ripple");
    let hold = &fx.seq.track(v1).unwrap().items[1];
    assert!(hold.hold_filters && hold.link.is_none(), "the hold freezes its effects and is linked to nothing");
}

#[test]
fn other_tracks_get_empty_time_and_split_clips_and_captions_move_on() {
    let mut fx = Fx::new();
    let (v3, a1) = (fx.v(2), fx.a(0));
    fx.put(v3, 50, 10, 0); // a gap at the section end on this video track
    fx.put(a1, 30, 30, 0); // music across the end
    fx.caption(35, 10, "across");
    fx.conform(&[(40, 6)], None).unwrap();
    assert_eq!(fx.spans(v3), vec![(56, 10, 0, None)], "a gap gets no hold; the later clip moves");
    let a1s = fx.spans(a1);
    assert_eq!((a1s[0].0, a1s[0].1, a1s[1].0, a1s[1].2), (30, 10, 46, 10), "sound is split and its later part moves on: silence between");
    let caps: Vec<(i64, i64)> = fx.seq.caption_tracks[0].captions.iter().map(|c| (R.frame_at(c.start), R.frame_at(c.end()))).collect();
    assert_eq!(caps, vec![(35, 40), (46, 51)], "a caption across the end is split like a clip");
}

#[test]
fn a_clip_that_already_holds_a_frame_keeps_holding_it() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let c = fx.put(v1, 0, 60, 100);
    let it = fx.seq.find_item_mut(c).unwrap().1;
    it.frame_hold = Some(f(130));
    fx.conform(&[(40, 5)], None).unwrap();
    let holds: Vec<Option<i64>> = fx.spans(v1).iter().map(|s| s.3).collect();
    assert!(holds.iter().all(|h| *h == Some(130)), "every piece shows the clip's held frame: {holds:?}");
    assert!(!fx.seq.track(v1).unwrap().items[1].hold_filters, "and keeps its Hold Filters setting");
}

#[test]
fn a_shorter_section_loses_the_range_at_its_end_exactly_as_extract_removes_it() {
    let mut fx = Fx::new();
    let (v1, a1) = (fx.v(0), fx.a(0));
    fx.put(v1, 0, 100, 0);
    fx.put(a1, 10, 25, 0);
    fx.put(a1, 38, 20, 0);
    fx.caption(30, 20, "x");
    let mut expected = fx.seq.clone();
    let mut n = fx.next;
    crate::extract(&mut expected, &[], TimeRange::from_bounds(f(32), f(40)), &mut Fx::ctx(&mut n));
    fx.conform(&[(40, -8)], None).unwrap();
    assert_eq!(shape(&fx.seq), shape(&expected));
}

#[test]
fn the_kept_clip_stays_whole_and_in_place_while_the_rest_of_its_track_ripples() {
    let mut fx = Fx::new();
    let (v1, a2) = (fx.v(0), fx.a(1));
    fx.put(v1, 0, 200, 0);
    fx.put(a2, 0, 5, 0);
    let take = fx.put(a2, 10, 100, 0);
    let after = fx.put(a2, 150, 10, 0);
    let before = fx.seq.find_item(take).unwrap().1.clone();
    fx.conform(&[(40, 8), (80, -6)], Some(take)).unwrap();
    assert_eq!(fx.seq.find_item(take).unwrap().1, &before, "the take is bit for bit what it was");
    assert_eq!(fx.seq.find_item(after).unwrap().1.start, f(152), "a clip after it on its track ripples by the net change");
    assert_eq!(fx.seq.track(a2).unwrap().items.len(), 3, "nothing on the take's track was split");
}

#[test]
fn a_clip_that_would_slide_under_the_kept_clip_refuses_the_conform() {
    let mut fx = Fx::new();
    let a2 = fx.a(1);
    let take = fx.put(a2, 10, 100, 0);
    fx.put(a2, 112, 10, 0);
    let before = fx.seq.clone();
    let e = fx.conform(&[(40, -6)], Some(take)).unwrap_err();
    assert!(e.to_string().contains("overlap") && e.to_string().contains("A2"), "{e}");
    assert_eq!(fx.seq, before, "and changes nothing");
}

#[test]
fn a_locked_track_in_the_way_refuses_and_is_named_but_sync_lock_off_tracks_stay_put() {
    let mut fx = Fx::new();
    let (v1, v2, a3) = (fx.v(0), fx.v(1), fx.a(2));
    fx.put(v1, 0, 100, 0);
    fx.put(v2, 60, 10, 0);
    fx.put(a3, 60, 10, 0);
    fx.seq.track_mut(v2).unwrap().locked = true;
    let before = fx.seq.clone();
    let e = fx.conform(&[(40, 4)], None).unwrap_err();
    assert!(e.to_string().contains("V2"), "the locked track is named: {e}");
    assert_eq!(fx.seq, before);

    // with its sync lock off the locked track would not move anyway: no refusal
    fx.seq.track_mut(v2).unwrap().sync_lock = false;
    fx.seq.track_mut(a3).unwrap().sync_lock = false;
    fx.conform(&[(40, 4)], None).unwrap();
    assert_eq!(fx.spans(v2)[0].0, 60, "a track with sync lock off stays where it is");
    assert_eq!(fx.spans(a3)[0].0, 60);
    assert_eq!(fx.spans(v1).len(), 3, "the sync-locked picture is conformed");
}

#[test]
fn a_transition_in_progress_at_the_section_end_refuses() {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    let a = fx.put(v1, 0, 40, 0);
    let b = fx.put(v1, 40, 40, 0);
    let dissolve = |start| Transition {
        id: TransitionId(1),
        effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
        start: f(start),
        duration: f(4),
        from: Some(a),
        to: Some(b),
        align: Default::default(),
        reverse: false,
    };
    fx.seq.track_mut(v1).unwrap().transitions = vec![dissolve(38)];
    let before = fx.seq.clone();
    for fit in [(40, 5), (40, -5)] {
        let e = fx.conform(&[fit], None).unwrap_err();
        assert!(e.to_string().contains("transition on V1"), "{fit:?}: {e}");
        assert_eq!(fx.seq, before);
    }
    // one that has finished a frame before the end leaves the last frame clean
    fx.seq.track_mut(v1).unwrap().transitions = vec![Transition { duration: f(1), ..dissolve(38) }];
    assert!(obstacle(&fx.seq, Fit { end: f(40), delta: f(5) }, None).is_none());
}

/// A random sequence: a background clip on V1 that outlasts every change, clips on the other
/// tracks, captions, and a kept clip on A3 alone on its track.
fn random_sequence(clips: &[(usize, i64, i64)], take: (i64, i64)) -> (Fx, ClipId) {
    let mut fx = Fx::new();
    let v1 = fx.v(0);
    fx.put(v1, 0, 2_000, 0);
    let tracks = [fx.v(1), fx.v(2), fx.a(0), fx.a(1)];
    let mut ends = [0i64; 4];
    for &(t, gap, dur) in clips {
        let t = t % 4;
        let start = ends[t] + gap;
        fx.put(tracks[t], start, dur, 0);
        ends[t] = start + dur;
    }
    for k in 0..10 {
        fx.caption(k * 37, 20, "c");
    }
    let kept = fx.put(fx.a(2), take.0, take.1, 0);
    (fx, kept)
}

/// Fits at increasing section ends: each section `len` frames, changed by `delta` (a trim never
/// removes the whole section).
fn fits_of(sections: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut at = 10;
    let mut out = Vec::new();
    for &(len, delta) in sections {
        at += len;
        let delta = if delta < 0 { delta.max(1 - len) } else { delta };
        if delta != 0 {
            out.push((at, delta));
        }
    }
    out
}

proptest! {
    #[test]
    fn a_conform_changes_the_length_by_the_sum_of_its_fits_and_nothing_before_or_the_kept_clip(
        clips in proptest::collection::vec((0usize..4, 0i64..30, 1i64..60), 0..20),
        sections in proptest::collection::vec((2i64..80, -40i64..40), 1..8),
        take in (0i64..100, 1i64..400),
    ) {
        let (mut fx, kept) = random_sequence(&clips, take);
        let fits = fits_of(&sections);
        let before = fx.seq.clone();
        let take_before = before.find_item(kept).unwrap().1.clone();
        match fx.conform(&fits, Some(kept)) {
            Ok(()) => {
                let sum: i64 = fits.iter().map(|f| f.1).sum();
                prop_assert_eq!(fx.seq.duration(), before.duration() + f(sum), "the length changes by exactly the sum of the fits");
                prop_assert!(fx.seq.check().is_ok(), "{:?}", fx.seq.check());
                prop_assert_eq!(fx.seq.find_item(kept).unwrap().1, &take_before, "the kept clip is unchanged");
                let first = fits.first().map(|f| f.0 + f.1.min(0)).unwrap_or(i64::MAX);
                for (old, new) in before.all_tracks().zip(fx.seq.all_tracks()) {
                    for i in old.items.iter().filter(|i| i.end() <= f(first)) {
                        prop_assert!(new.items.contains(i), "content before the first change is untouched: {:?}", i.id);
                    }
                }
            }
            Err(e) => {
                prop_assert!(e.to_string().contains("overlap"), "the only refusal here is the kept clip's track: {}", e);
                prop_assert_eq!(&fx.seq, &before, "a refused conform changes nothing");
            }
        }
    }

    #[test]
    fn fits_applied_together_give_the_same_sequence_as_one_at_a_time(
        clips in proptest::collection::vec((0usize..4, 0i64..30, 1i64..60), 0..20),
        sections in proptest::collection::vec((2i64..80, -40i64..40), 1..8),
    ) {
        let (mut together, _) = random_sequence(&clips, (3_000, 10));
        let mut single = Fx { seq: together.seq.clone(), next: together.next };
        let fits = fits_of(&sections);
        together.conform(&fits, None).unwrap();
        // first to last, each at its end as the earlier fits have moved it
        let mut offset = 0;
        for &(end, delta) in &fits {
            single.conform(&[(end + offset, delta)], None).unwrap();
            offset += delta;
        }
        prop_assert_eq!(shape(&together.seq), shape(&single.seq));
    }

    #[test]
    fn hostile_fits_are_refused_or_applied_and_never_panic(
        ends in proptest::collection::vec(prop_oneof![any::<i64>(), -100i64..3_000], 0..6),
        deltas in proptest::collection::vec(prop_oneof![any::<i64>(), -100i64..100], 0..6),
        keep in prop_oneof![Just(None), any::<u64>().prop_map(|c| Some(ClipId(c)))],
    ) {
        let (mut fx, _) = random_sequence(&[(1, 5, 30), (2, 0, 50)], (0, 50));
        let fits: Vec<Fit> = ends.iter().zip(&deltas).map(|(&e, &d)| Fit { end: Tick(e), delta: Tick(d) }).collect();
        let before = fx.seq.clone();
        let mut n = fx.next;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| conform(&mut fx.seq, &fits, keep, &mut Fx::ctx(&mut n))));
        prop_assert!(r.is_ok(), "never panics");
        if let Ok(Err(_)) = r {
            prop_assert_eq!(&fx.seq, &before, "a refusal changes nothing");
        } else {
            prop_assert!(fx.seq.check().is_ok());
        }
    }
}
