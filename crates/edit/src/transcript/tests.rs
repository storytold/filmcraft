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
        custom_label_color: None,
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
        audio_stream: 0,
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
        // no word in these tests spans more than the maximum once snapped to frames, so no caption
        // may last longer either
        assert!(x.end - x.start <= rules.max_duration, "longer than {:?}: {x:?}", rules.max_duration);
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

/// Captions keep to `max_duration` on their frame-snapped times. "one" to "seven" spans 6.999 s as
/// spoken (0.01 s to 7.009 s) but 7.04 s once its end is rounded up to a frame at 25 fps, so the
/// caption breaks before "seven". A short caption stretched toward `min_duration` stops at the
/// maximum too, also when the minimum is the longer of the two.
#[test]
fn captions_keep_to_the_maximum_once_snapped_to_frames() {
    let spoken = [
        ("one", 0.01, 0.9),
        ("two", 1.0, 1.9),
        ("three", 2.0, 2.9),
        ("four", 3.0, 3.9),
        ("five", 4.0, 4.9),
        ("six", 5.0, 5.9),
        ("seven", 6.0, 7.009),
        ("eight", 7.1, 7.9),
    ];
    let words: Vec<SeqWord> = spoken
        .iter()
        .enumerate()
        .map(|(i, &(text, a, b))| SeqWord {
            text: text.into(),
            start: s(a),
            end: s(b),
            clip: ClipId(1),
            item: MEDIA,
            index: i,
            track: 0,
            speaker: None,
            confidence: 1.0,
        })
        .collect();
    let spans = |rules: &CaptionRules| {
        let b = caption_blocks(&words, rules, R);
        check_blocks(&b, rules);
        b.iter().map(|x| (x.start, x.end, x.text.clone())).collect::<Vec<_>>()
    };
    let one_to_six = "one two three four five six".to_string();
    assert_eq!(spans(&CaptionRules::default()), [(s(0.0), s(5.92), one_to_six), (s(6.0), s(7.92), "seven eight".to_string())]);
    // a 2.01 s minimum rounds up to 2.04 s; the last caption, with nothing after it, stops at 2 s
    let even = CaptionRules { min_duration: s(2.01), max_duration: s(2.01), ..Default::default() };
    assert_eq!(spans(&even).last().map(|x| (x.0, x.1)), Some((s(6.0), s(8.0))));
    // a minimum longer than the maximum gives way to it
    let crossed = CaptionRules { min_duration: s(3.0), max_duration: s(2.0), ..Default::default() };
    let b = spans(&crossed);
    assert_eq!(b.len(), 4, "{b:?}");
    assert!(b.iter().all(|x| x.1 - x.0 == s(2.0)), "{b:?}");
}

/// A single word is never split, so a word whose frame-snapped span is longer than the maximum
/// gets a caption of its own over that span, even when the word itself is shorter: at 25 fps a
/// word from 0.01 s to 2.009 s lasts 1.999 s and spans 0 to 2.04 s.
#[test]
fn a_single_word_keeps_its_snapped_span_past_the_maximum() {
    let word = |text: &str, a: f64, b: f64, index: usize| SeqWord {
        text: text.into(),
        start: s(a),
        end: s(b),
        clip: ClipId(1),
        item: MEDIA,
        index,
        track: 0,
        speaker: None,
        confidence: 1.0,
    };
    let rules = CaptionRules { max_duration: s(2.0), ..Default::default() };
    let alone = caption_blocks(&[word("long", 0.01, 2.009, 0)], &rules, R);
    assert_eq!(alone.iter().map(|x| (x.start, x.end)).collect::<Vec<_>>(), [(s(0.0), s(2.04))]);
    // a word close behind it starts the next caption: together they would pass the maximum
    let both = caption_blocks(&[word("long", 0.01, 2.009, 0), word("next", 2.1, 2.5, 1)], &rules, R);
    let spans: Vec<(Tick, Tick, &str)> = both.iter().map(|x| (x.start, x.end, x.text.as_str())).collect();
    // "next" is stretched to the 1 s default minimum
    assert_eq!(spans, [(s(0.0), s(2.04), "long"), (s(2.08), s(3.08), "next")]);
}

/// Higher-track coverage merges into sorted disjoint ranges and answers like the ranges it was
/// built from (a binary search, not a scan of every clip per word).
#[test]
fn coverage_merges_ranges_and_matches_a_linear_scan() {
    let r = |a: i64, b: i64| TimeRange::from_bounds(Tick(a), Tick(b));
    let ranges = [r(10, 20), r(15, 30), r(30, 35), r(50, 60), r(70, 70), r(90, 80), r(-5, 0)];
    let mut c = Coverage::default();
    c.add(ranges[..3].to_vec());
    c.add(ranges[3..].to_vec());
    assert_eq!(c.0, vec![(Tick(-5), Tick(0)), (Tick(10), Tick(35)), (Tick(50), Tick(60))]);
    for t in -10..100 {
        assert_eq!(c.contains(Tick(t)), ranges.iter().any(|x| x.contains(Tick(t))), "t = {t}");
    }
}
