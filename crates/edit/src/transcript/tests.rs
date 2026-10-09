//! Deterministic tests of the transcript → sequence mapping and the text-based edits, on the
//! hand-made interview transcript (`tests/fixtures/interview.transcript.json`, 10 s, two speakers,
//! a 2 s pause after "show.", fillers "Um," and "uh", phrase "you know.").

use super::*;
use filmcraft_project::{Label, Project, SequenceSettings, TrackItem};
use filmcraft_time::FrameRate;

const R: FrameRate = FrameRate { num: 25, den: 1 };
const MEDIA: ItemId = ItemId(1);

fn s(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64).round() as i64)
}

fn fixture() -> Transcript {
    let mut t: Transcript = serde_json::from_str(include_str!("../../tests/fixtures/interview.transcript.json")).unwrap();
    t.normalize();
    t.check().unwrap();
    t
}

fn transcripts() -> Transcripts {
    let mut m = Transcripts::new();
    m.insert(MEDIA, Arc::new(fixture()));
    m
}

fn seq() -> Sequence {
    let mut p = Project::new("t");
    let id = p.new_sequence("s", SequenceSettings { frame_rate: R, ..Default::default() }, 1, 2, None);
    p.sequence(id).unwrap().clone()
}

fn clip(id: u64, start: f64, dur: f64, src_in: f64) -> TrackItem {
    TrackItem {
        id: ClipId(id),
        item: MEDIA,
        name: "interview".into(),
        label: Label::Iris,
        start: s(start),
        duration: s(dur),
        source_in: s(src_in),
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
        graphic: None,
    }
}

fn texts(w: &[SeqWord]) -> Vec<&str> {
    w.iter().map(|w| w.text.as_str()).collect()
}

fn media_dur(_: ItemId) -> Option<Tick> {
    Some(s(10.0))
}

fn whole() -> Sequence {
    let mut q = seq();
    q.audio_tracks[0].items.push(clip(100, 0.0, 10.0, 0.0));
    q
}

#[test]
fn words_map_through_the_clip() {
    let mut q = seq();
    // media 3.5 s – 6.5 s at timeline 2 s
    q.audio_tracks[0].items.push(clip(100, 2.0, 3.0, 3.5));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["Um,", "today", "we", "talk", "about", "rivers."]);
    assert_eq!(w[0].start, s(2.2));
    assert_eq!(w[0].end, s(2.5));
    assert_eq!(w[5].end, s(4.4));
    assert!(w.iter().all(|x| x.clip == ClipId(100) && x.item == MEDIA && x.track == 0));
    assert_eq!(w[0].index, 4);
    assert_eq!(w[0].speaker.as_deref(), Some("Speaker 1"));
}

#[test]
fn a_cut_word_is_kept_by_its_midpoint_and_clamped() {
    let mut q = seq();
    // cut inside "show." (1.20–1.70, midpoint 1.45): the clip starts at media 1.4
    q.audio_tracks[0].items.push(clip(100, 0.0, 1.0, 1.4));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["show."]);
    assert_eq!(w[0].start, Tick::ZERO);
    assert_eq!(w[0].end, s(0.3));
}

#[test]
fn speed_scales_word_times() {
    let mut q = seq();
    let mut c = clip(100, 0.0, 5.0, 0.0);
    c.speed = 2.0;
    q.audio_tracks[0].items.push(c);
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert_eq!((w[3].start, w[3].end), (s(0.6), s(0.85)));
}

#[test]
fn duplicate_tracks_read_once_and_disabled_or_reversed_clips_are_silent() {
    let mut q = whole();
    q.audio_tracks[1].items.push(clip(101, 0.0, 10.0, 0.0));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert!(w.iter().all(|x| x.track == 0));
    q.audio_tracks[0].items[0].enabled = false;
    let w = sequence_words(&q, &transcripts());
    assert_eq!(w.len(), 20);
    assert!(w.iter().all(|x| x.track == 1));
    q.audio_tracks[1].items[0].reverse = true;
    assert!(sequence_words(&q, &transcripts()).is_empty());
}

#[test]
fn two_clips_of_the_same_media_repeat_words_in_edit_order() {
    let mut q = seq();
    // "rivers." twice: media 5.3–5.9 at 0 s and again at 1 s
    q.audio_tracks[0].items.push(clip(100, 0.0, 1.0, 5.2));
    q.audio_tracks[0].items.push(clip(101, 1.0, 1.0, 5.2));
    let w = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w), ["rivers.", "rivers."]);
    assert_eq!(w[1].start, s(1.1));
    assert_eq!(w[1].clip, ClipId(101));
}

#[test]
fn search_word_at_and_paragraphs() {
    let w = sequence_words(&whole(), &transcripts());
    assert_eq!(search(&w, "rivers"), vec![9..10, 17..18]);
    assert_eq!(search(&w, "YOU kn"), vec![18..20]);
    assert_eq!(search(&w, "talk about"), vec![7..9]);
    assert!(search(&w, "  ").is_empty());
    assert_eq!(word_at(&w, s(4.2)), Some(5));
    assert_eq!(word_at(&w, s(2.5)), None); // in the pause
    assert_eq!(word_at(&w, s(0.1)), None);
    // pause ≥ 1.5 s after "show.", speaker change before "Thanks"
    assert_eq!(paragraphs(&w, s(1.5)), vec![0..4, 4..10, 10..20]);
}

#[test]
fn word_ranges_snap_outward() {
    let w = sequence_words(&whole(), &transcripts());
    // "Welcome to the show." 0.50–1.70 → frames 12..43 (0.48–1.72)
    let r = word_range(&w, 3, 0, R).unwrap();
    assert_eq!((r.start, r.end()), (s(0.48), s(1.72)));
    let m = media_word_range(&fixture(), 4, 9, R).unwrap();
    assert_eq!((m.start, m.end()), (s(3.68), s(5.92)));
    assert!(word_range(&w, 0, 99, R).is_none());
}

#[test]
fn extract_text_closes_the_gap() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    let r = word_range(&w, 0, 3, R).unwrap();
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    let tracks: Vec<TrackId> = q.all_tracks().map(|t| t.id).collect();
    crate::extract(&mut q, &tracks, r, &mut ctx);
    q.check().unwrap();
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(w2.len(), 16);
    assert_eq!(w2[0].text, "Um,");
    assert_eq!(w2[0].start, s(3.70) - r.duration);
}

#[test]
fn pauses_are_found_and_removed_in_one_pass() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    // only the 2 s pause after "show." is ≥ 1 s; 0.1 s is kept on each side
    let p = find_pauses(&w, s(1.0), s(0.1), R);
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].start, p[0].end()), (s(1.80), s(3.60)));
    // 0.4 s threshold also finds 5.90→6.40 and 7.60→8.00
    let p2 = find_pauses(&w, s(0.4), s(0.1), R);
    assert_eq!(p2.len(), 3);
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    let removed = ripple_delete_ranges(&mut q, p2.clone(), &mut ctx);
    q.check().unwrap();
    assert_eq!(removed, p2.iter().map(|r| r.duration).fold(Tick::ZERO, |a, b| a + b));
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(texts(&w2), texts(&w));
    // the long pause is now 0.2 s
    assert_eq!(w2[4].start - w2[3].end, s(0.2));
    assert_eq!(w2.last().unwrap().end, s(10.0) - removed);
    assert!(find_pauses(&w2, s(0.4), s(0.1), R).is_empty());
}

#[test]
fn fillers_are_found_with_phrases_and_removed() {
    let mut q = whole();
    let w = sequence_words(&q, &transcripts());
    let defaults: Vec<String> = DEFAULT_FILLERS.iter().map(|x| x.to_string()).collect();
    let hits = find_fillers(&w, &defaults);
    assert_eq!(hits, vec![4..5, 15..16]);
    let mut more = defaults.clone();
    more.push("You know".into());
    more.push("you".into());
    assert_eq!(find_fillers(&w, &more), vec![4..5, 15..16, 18..20]);
    let ranges = filler_ranges(&w, &hits, R);
    // "Um," 3.70–4.00 → nearest frames 3.68–4.00; "uh" 8.20–8.50 → 8.20–8.48
    assert_eq!(ranges.iter().map(|r| (r.start, r.end())).collect::<Vec<_>>(), vec![(s(3.68), s(4.00)), (s(8.20), s(8.48))]);
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    ripple_delete_ranges(&mut q, ranges, &mut ctx);
    q.check().unwrap();
    let w2 = sequence_words(&q, &transcripts());
    assert_eq!(w2.len(), 18);
    assert!(find_fillers(&w2, &defaults).is_empty());
    assert_eq!(q.audio_tracks[0].items.len(), 3);
}

#[test]
fn merge_ranges_joins_overlaps() {
    let r = |a: f64, b: f64| TimeRange::from_bounds(s(a), s(b));
    let m = merge_ranges(vec![r(5.0, 6.0), r(1.0, 2.0), r(1.5, 3.0), r(3.0, 4.0)]);
    assert_eq!(m, vec![r(1.0, 4.0), r(5.0, 6.0)]);
}

fn check_blocks(b: &[CaptionBlock], rules: &CaptionRules) {
    for (i, x) in b.iter().enumerate() {
        assert!(x.end > x.start, "{x:?}");
        assert_eq!(R.snap(x.start), x.start);
        assert_eq!(R.snap(x.end), x.end);
        let lines: Vec<&str> = x.text.lines().collect();
        assert!(lines.len() <= rules.lines, "{x:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= rules.max_chars), "{x:?}");
        if i > 0 {
            assert!(x.start >= b[i - 1].end + Tick(R.frame_duration().0 * rules.gap_frames), "{:?} overlaps {:?}", b[i - 1], x);
        }
    }
}

#[test]
fn captions_follow_line_length_speaker_and_pause_rules() {
    let w = sequence_words(&whole(), &transcripts());
    let rules = CaptionRules::default();
    let b = caption_blocks(&w, &rules, R);
    check_blocks(&b, &rules);
    assert_eq!(
        b.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
        ["Welcome to the show.", "Um, today we talk about rivers.", "Thanks for having me. I uh love rivers,\nyou know."]
    );
    assert_eq!((b[0].start, b[0].end), (s(0.48), s(1.72)));
    assert_eq!(b[2].speaker.as_deref(), Some("Speaker 2"));
    assert_eq!(b[2].words, 10..20);

    // single 20-character lines, two-frame gaps, 3 s minimum
    let tight = CaptionRules { max_chars: 20, lines: 1, gap_frames: 2, min_duration: s(3.0), ..Default::default() };
    let b = caption_blocks(&w, &tight, R);
    check_blocks(&b, &tight);
    assert!(b.len() >= 5);
    // the first block is extended to 3 s (room before "Um,")
    assert_eq!(b[0].end - b[0].start, s(3.0));
    // every word is shown exactly once, in order
    let all: Vec<usize> = b.iter().flat_map(|x| x.words.clone()).collect();
    assert_eq!(all, (0..20).collect::<Vec<_>>());
}

#[test]
fn captions_never_overlap_with_dense_words() {
    // 30 one-frame words back to back
    let words: Vec<SeqWord> = (0..30)
        .map(|i| SeqWord {
            text: format!("w{i}."),
            start: R.tick_of(i),
            end: R.tick_of(i + 1),
            clip: ClipId(1),
            item: MEDIA,
            index: i as usize,
            track: 0,
            speaker: if i % 3 == 0 { Some("A".into()) } else { Some("B".into()) },
            confidence: 1.0,
        })
        .collect();
    let rules = CaptionRules { gap_frames: 1, ..Default::default() };
    let b = caption_blocks(&words, &rules, R);
    check_blocks(&b, &rules);
}

// ---- voice and pauses ----------------------------------------------------------------------

const VOICED: ItemId = ItemId(2);

/// "so but then", with voice measured from a waveform: lead-in silence to 0.5 s, "so" 0.5–0.8,
/// a 60 ms flicker, "but" 0.86–1.30 with a 100 ms stop closure inside it (1.00–1.10), a 0.4 s
/// pause, "then" 1.70–2.00, then a 0.3 s voiced stretch with no word (a tidied-away restart),
/// silence to 3.0 s.
fn voiced() -> Transcript {
    let w = |t: &str, a: f64, b: f64| filmcraft_project::Word::new(t, s(a), s(b));
    let mut t = Transcript {
        language: "en".into(),
        words: vec![w("so", 0.50, 0.80), w("but", 0.86, 1.30), w("then", 1.70, 2.00)],
        voice: vec![(s(0.50), s(0.80)), (s(0.86), s(1.00)), (s(1.10), s(1.30)), (s(1.70), s(2.00)), (s(2.20), s(2.50))],
        ..Default::default()
    };
    t.normalize();
    t.check().unwrap();
    t
}

fn voiced_seq(clips: Vec<TrackItem>) -> (Sequence, Transcripts) {
    let mut q = seq();
    q.audio_tracks[0].items = clips
        .into_iter()
        .map(|mut c| {
            c.item = VOICED;
            c
        })
        .collect();
    let mut m = Transcripts::new();
    m.insert(VOICED, Arc::new(voiced()));
    (q, m)
}

#[test]
fn voice_maps_through_the_clip_like_words() {
    // the clip shows media 0.6–3.0 at timeline 10.0, at double speed
    let mut c = clip(1, 10.0, 1.2, 0.6);
    c.speed = 2.0;
    let (q, m) = voiced_seq(vec![c]);
    let v = sequence_voice(&q, &m);
    assert_eq!(v.covered, vec![TimeRange::from_bounds(s(10.0), s(11.2))]);
    // 0.5–0.8 is clipped to the In point (0.6) and halved: 10.00–10.10
    assert_eq!(v.spans.first(), Some(&TimeRange::from_bounds(s(10.0), s(10.1))));
    assert_eq!(v.spans.last(), Some(&TimeRange::from_bounds(s(10.8), s(10.95))));
    // a transcript without a voice map: its words are the voice
    let mut plain = voiced();
    plain.voice.clear();
    let mut m2 = Transcripts::new();
    m2.insert(VOICED, Arc::new(plain));
    let (q2, _) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0)]);
    let v2 = sequence_voice(&q2, &m2);
    assert_eq!(v2.spans.len(), 3);
    assert_eq!(v2.spans[1], TimeRange::from_bounds(s(0.86), s(1.30)));
}

#[test]
fn pauses_come_from_the_voice_not_the_words() {
    let (q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0)]);
    let w = sequence_words(&q, &m);
    let v = sequence_voice(&q, &m);
    let p = find_voice_pauses(&w, &v, s(0.08));
    let ranges: Vec<(Tick, Tick)> = p.iter().map(|p| (p.range.start, p.range.end())).collect();
    // lead-in, the 0.4 s pause, the gap before the wordless voice and the tail; not the 60 ms
    // flicker (below the minimum) nor the stop closure inside "but"
    assert_eq!(ranges, vec![(s(0.0), s(0.5)), (s(1.3), s(1.7)), (s(2.0), s(2.2)), (s(2.5), s(3.0))]);
    assert!(!p[0].voiced_before && p[0].voiced_after && p[0].before.is_none() && p[0].after == Some(0));
    assert_eq!((p[1].before, p[1].after), (Some(1), Some(2)));
    assert!(p[3].voiced_before && !p[3].voiced_after && p[3].after.is_none());
    // with a 0.3 s pause length only the lead-in, the 0.4 s pause and the tail are pauses
    assert_eq!(find_voice_pauses(&w, &v, s(0.3)).len(), 3);
    // a word is never cut, even where the waveform is quiet all through it (a soft syllable
    // below the gate): "but" spans 0.86–1.30 but has voice only at its edges
    let mut quiet = voiced();
    quiet.voice = vec![(s(0.50), s(0.80)), (s(0.86), s(0.90)), (s(1.25), s(1.30)), (s(1.70), s(2.00))];
    let mut m3 = Transcripts::new();
    m3.insert(VOICED, Arc::new(quiet));
    let p3 = find_voice_pauses(&sequence_words(&q, &m3), &sequence_voice(&q, &m3), s(0.08));
    assert!(p3.iter().all(|p| p.range.end() <= s(0.86) || p.range.start >= s(1.30)), "{p3:?}");
    // a pause that runs into a word stops at the word
    let mut early = voiced();
    early.voice = vec![(s(0.50), s(0.80)), (s(0.86), s(1.30)), (s(1.80), s(2.00))];
    let mut m4 = Transcripts::new();
    m4.insert(VOICED, Arc::new(early));
    let p4 = find_voice_pauses(&sequence_words(&q, &m4), &sequence_voice(&q, &m4), s(0.08));
    assert!(p4.iter().any(|p| p.range == TimeRange::from_bounds(s(1.30), s(1.70))), "{p4:?}");
}

#[test]
fn an_untranscribed_clip_is_never_a_pause() {
    // transcribed 0–3 s, an untranscribed clip 3–5 s, transcribed again 5–8 s
    let (mut q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0), clip(3, 5.0, 3.0, 0.0)]);
    let mut other = clip(2, 3.0, 2.0, 0.0);
    other.item = ItemId(99);
    q.audio_tracks[0].items.insert(1, other);
    let v = sequence_voice(&q, &m);
    assert_eq!(v.covered, vec![TimeRange::from_bounds(s(0.0), s(3.0)), TimeRange::from_bounds(s(5.0), s(8.0))]);
    let p = find_voice_pauses(&sequence_words(&q, &m), &v, s(0.08));
    assert!(p.iter().all(|p| p.range.end() <= s(3.0) || p.range.start >= s(5.0)), "{p:?}");
}

#[test]
fn an_empty_timeline_gap_joins_the_silence_around_it() {
    // the same take twice with 1.5 s of empty timeline between: its tail (2.0–3.0), the gap
    // (3.0–4.5) and the second copy's lead-in (4.5–5.0) are one pause
    let (q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0), clip(2, 4.5, 3.0, 0.0)]);
    let v = sequence_voice(&q, &m);
    assert_eq!(v.covered, vec![TimeRange::from_bounds(s(0.0), s(7.5))]);
    let p = find_voice_pauses(&sequence_words(&q, &m), &v, s(0.3));
    assert!(p.iter().any(|p| p.range == TimeRange::from_bounds(s(2.5), s(5.0)) && p.voiced_before && p.voiced_after), "{p:?}");
    // with anything on any track in the gap (here a video clip) it is not silence we may cut
    let (mut q2, m2) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0), clip(2, 4.5, 3.0, 0.0)]);
    q2.video_tracks[0].items.push(clip(9, 3.2, 0.5, 0.0));
    assert_eq!(sequence_voice(&q2, &m2).covered.len(), 2);
}

#[test]
fn pause_cuts_keep_pats_margins_and_round_inward_to_frames() {
    let (q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0)]);
    let w = sequence_words(&q, &m);
    let p = find_voice_pauses(&w, &sequence_voice(&q, &m), s(0.08));
    let fd = R.frame_duration(); // 40 ms at 25 fps
    let (after, before) = (s(0.030), s(0.035));
    // 1.30–1.70: 1.33 rounds up to 1.36, 1.665 down to 1.64
    assert_eq!(pause_cut(&p[1], after, before, R), Some(TimeRange::from_bounds(s(1.36), s(1.64))));
    // the lead-in keeps nothing before the take starts: 0.0 to 0.465 → 0.44
    assert_eq!(pause_cut(&p[0], after, before, R), Some(TimeRange::from_bounds(s(0.0), s(0.44))));
    // the tail runs to the end: 2.53 → 2.56 to 3.0
    assert_eq!(pause_cut(&p[3], after, before, R), Some(TimeRange::from_bounds(s(2.56), s(3.0))));
    // 0.2 s less two 80 ms margins is exactly one frame; with 90 ms margins nothing is left
    assert_eq!(pause_cut(&p[2], s(0.08), s(0.08), R), Some(TimeRange::from_bounds(s(2.08), s(2.12))));
    assert_eq!(pause_cut(&p[2], s(0.09), s(0.09), R), None);
    for c in p.iter().filter_map(|p| pause_cut(p, after, before, R)) {
        assert_eq!(R.snap(c.start), c.start);
        assert_eq!(R.snap(c.end()), c.end());
        assert!(c.duration >= fd);
    }
}

#[test]
fn wordless_voice_is_shown_and_the_layout_is_in_time_order() {
    let (q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0)]);
    let w = sequence_words(&q, &m);
    let v = sequence_voice(&q, &m);
    let speech = unlabelled_speech(&w, &v, s(0.1));
    assert_eq!(speech, vec![TimeRange::from_bounds(s(2.20), s(2.50))]);
    let p = find_voice_pauses(&w, &v, s(0.3));
    assert_eq!(
        layout(&w, &p, &speech),
        vec![Token::Pause(0), Token::Word(0), Token::Word(1), Token::Pause(1), Token::Word(2), Token::Speech(0), Token::Pause(2)]
    );
}

#[test]
fn lift_leaves_the_gaps_extract_closes_them() {
    let (q, m) = voiced_seq(vec![clip(1, 0.0, 3.0, 0.0)]);
    let w = sequence_words(&q, &m);
    let p = find_voice_pauses(&w, &sequence_voice(&q, &m), s(0.3));
    let cuts: Vec<TimeRange> = p.iter().filter_map(|p| pause_cut(p, s(0.03), s(0.035), R)).collect();
    let total = cuts.iter().fold(Tick::ZERO, |a, c| a + c.duration);
    let mut next = 1000;
    let mut ctx = EditCtx { next_id: &mut next, media_duration: &media_dur, media_start: &|_| Tick::ZERO, min_duration: R.frame_duration() };
    let mut lifted = q.clone();
    assert_eq!(lift_ranges(&mut lifted, cuts.clone(), &mut ctx), total);
    lifted.check().unwrap();
    // lifting leaves every word where it was (only the tail after the last voice is gone)
    let lw = sequence_words(&lifted, &m);
    assert_eq!(lw.iter().map(|x| (x.start, x.end)).collect::<Vec<_>>(), w.iter().map(|x| (x.start, x.end)).collect::<Vec<_>>());
    let mut extracted = q.clone();
    assert_eq!(ripple_delete_ranges(&mut extracted, cuts, &mut ctx), total);
    extracted.check().unwrap();
    assert_eq!(extracted.duration(), q.duration() - total);
    // the words all survive, closer together
    assert_eq!(texts(&sequence_words(&extracted, &m)), texts(&w));
}

/// An hour of speech (a word and its voice every 0.5 s, 0.2 s pauses): finding the pauses stays
/// fast (it used to compare every span with every other).
#[test]
fn pauses_of_an_hour_long_transcript_are_found_quickly() {
    let n = 7200;
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for i in 0..n {
        let a = i as f64 * 0.5;
        t.words.push(filmcraft_project::Word::new("w", s(a), s(a + 0.3)));
        t.voice.push((s(a), s(a + 0.3)));
    }
    let mut q = seq();
    let mut c = clip(1, 0.0, n as f64 * 0.5, 0.0);
    c.item = VOICED;
    q.audio_tracks[0].items.push(c);
    let mut m = Transcripts::new();
    m.insert(VOICED, Arc::new(t));
    let t0 = std::time::Instant::now();
    let w = sequence_words(&q, &m);
    let v = sequence_voice(&q, &m);
    let p = find_voice_pauses(&w, &v, s(0.15));
    let sp = unlabelled_speech(&w, &v, s(0.1));
    let _ = layout(&w, &p, &sp);
    assert_eq!(p.len(), n, "a pause after every word (the last one is the tail)");
    assert!(t0.elapsed() < std::time::Duration::from_secs(2), "{:?}", t0.elapsed());
}
