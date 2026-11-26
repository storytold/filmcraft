//! Tests of [`crate::prompter`]: talking points and script from a marker comment, their timing in
//! the section, sections and the next one around overlapping range markers, the caption line, the
//! `prompter.inspect` query, and hostile comments and markers.

use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, Label, Marker, MarkerId, MarkerKind, Project, Sequence, SequenceSettings, TrackId};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::prompter::{MAX_POINTS, Note, PointState, allocate, parse_notes, prompt_at, section_at};
use crate::{EngineError, Session};

fn sec(s: f64) -> Tick {
    Tick::from_seconds_f64(s)
}

fn sequence() -> Sequence {
    let mut p = Project::default();
    let id = p.new_sequence("Narration", SequenceSettings::default(), 1, 1, None);
    p.sequence(id).expect("the new sequence").clone()
}

fn marker(id: u64, start: f64, duration: f64, name: &str, comment: &str) -> Marker {
    Marker {
        url: String::new(),
        author: String::new(),
        custom_color: None,
        id: MarkerId(id),
        start: sec(start),
        duration: sec(duration),
        name: name.into(),
        comment: comment.into(),
        kind: MarkerKind::Comment,
        color: Label::Green,
    }
}

fn with_markers(markers: Vec<Marker>) -> Sequence {
    let mut q = sequence();
    q.markers = markers;
    q
}

#[test]
fn list_lines_are_talking_points_with_their_durations_and_the_rest_is_script() {
    let notes = parse_notes(
        "Welcome back.\n- Third line of the graph [0:30]\n  * Growth since 2019\n• Bad time [0:75]\n- Not a time [soon]\n-not a point\n\n\nSecond paragraph.",
    );
    let texts: Vec<&str> = notes.points.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(texts, ["Third line of the graph", "Growth since 2019", "Bad time [0:75]", "Not a time [soon]"]);
    assert_eq!(notes.points[0].duration, Some(sec(30.0)), "a written m:ss duration is taken off the text");
    assert!(notes.points[1..].iter().all(|p| p.duration.is_none()), "no duration, or a malformed one kept in the text");
    assert!(notes.script.starts_with("Welcome back.") && notes.script.ends_with("Second paragraph."), "script keeps its order: {:?}", notes.script);
    assert!(notes.script.contains("-not a point"), "a dash without a space is script");
    assert!(!notes.script.contains("\n\n\n"), "blank runs collapse: {:?}", notes.script);
    assert!(!notes.script.contains("Third line"), "points are not script");
}

#[test]
fn points_without_a_duration_share_what_the_timed_ones_leave_and_end_with_the_section() {
    let points = [
        Note { text: "a".into(), duration: None },
        Note { text: "b".into(), duration: Some(sec(10.0)) },
        Note { text: "c".into(), duration: None },
        Note { text: "d".into(), duration: None },
    ];
    let (start, length) = (sec(5.0), Tick(sec(40.0).0 + 7));
    let (spans, overrun) = allocate(&points, start, length);
    assert!(overrun.is_none());
    assert_eq!(spans.first().map(|s| s.0), Some(start), "the first point starts the section");
    assert_eq!(spans.last().map(|s| s.1), Some(start + length), "the last point ends it");
    assert!(spans.windows(2).all(|w| w[0].1 == w[1].0), "points are laid end to end: {spans:?}");
    assert_eq!(spans[1].1 - spans[1].0, sec(10.0), "a written duration is kept");
    let shares: Vec<i64> = [0, 2, 3].iter().map(|&i| (spans[i].1 - spans[i].0).0).collect();
    assert!(shares.iter().all(|d| (d - shares[0]).abs() <= 7), "untimed points share equally: {shares:?}");
}

#[test]
fn points_that_need_more_time_than_the_section_are_reported() {
    let long =
        [Note { text: "a".into(), duration: Some(sec(30.0)) }, Note { text: "b".into(), duration: Some(sec(20.0)) }, Note { text: "c".into(), duration: None }];
    let (spans, overrun) = allocate(&long, Tick::ZERO, sec(40.0));
    let o = overrun.expect("50 s of points in a 40 s section");
    assert_eq!(o.over, o.points - o.section, "over is what the points run past the section");
    assert!(o.over > Tick::ZERO && o.untimed == 1);
    assert!(spans[1].1 > sec(40.0), "the points keep their written length, past the section");

    let exact = [Note { text: "a".into(), duration: Some(sec(40.0)) }, Note { text: "b".into(), duration: None }];
    let o = allocate(&exact, Tick::ZERO, sec(40.0)).1.expect("nothing left for the untimed point");
    assert!(o.over == Tick::ZERO && o.untimed == 1);

    let fits = [Note { text: "a".into(), duration: Some(sec(20.0)) }];
    assert!(allocate(&fits, Tick::ZERO, sec(40.0)).1.is_none(), "points shorter than the section are fine");
}

#[test]
fn the_current_point_and_the_script_position_follow_the_time() {
    let q = with_markers(vec![marker(1, 10.0, 60.0, "Graph", "Look at this chart.\n- First [0:10]\n- Second\n- Third [0:20]")]);
    let mut last = (0usize, 0usize);
    let mut seen = Vec::new();
    for s in [10.0, 15.0, 25.0, 39.0, 45.0, 60.0, 69.9] {
        let p = prompt_at(&q, sec(s));
        let section = p.section.expect("inside the marker");
        let current = section.current.expect("a point is current while the points fill the section");
        assert!(section.points[..current].iter().all(|x| x.state == PointState::Done), "earlier points are done at {s}");
        assert!(section.points[current + 1..].iter().all(|x| x.state == PointState::Upcoming), "later points are upcoming at {s}");
        let offset = section.script.as_ref().map(|x| x.offset).unwrap_or(0);
        assert!(current >= last.0 && offset >= last.1, "position only moves forward at {s}");
        last = (current, offset);
        seen.push(current);
    }
    seen.dedup();
    assert_eq!(seen, [0, 1, 2], "every point is current in turn");
    let end = prompt_at(&q, Tick(sec(70.0).0 - 1)).section.and_then(|x| x.script).expect("script");
    assert!(end.offset + 1 >= end.chars, "the script finishes with the section: {} of {}", end.offset, end.chars);
    assert!(prompt_at(&q, sec(70.0)).section.is_none(), "a section ends at its out point");
}

#[test]
fn a_nested_section_interrupts_its_parent_and_next_names_what_is_shown_after() {
    let q = with_markers(vec![
        marker(1, 0.0, 100.0, "Outer", ""),
        marker(2, 30.0, 10.0, "Inner", ""),
        marker(3, 30.0, 40.0, "Same start, longer", ""),
        marker(4, 120.0, 10.0, "After the gap", ""),
        marker(5, 50.0, 0.0, "Point marker", ""),
    ]);
    let shown = |s: f64| section_at(&q, sec(s)).map(|m| m.id.0);
    assert_eq!(shown(10.0), Some(1));
    assert_eq!(shown(35.0), Some(2), "the later start wins; of two equal starts the shorter");
    assert_eq!(shown(45.0), Some(3), "the longer one of the pair takes over when the shorter ends");
    assert_eq!(shown(75.0), Some(1), "the outer section resumes");
    assert_eq!(shown(110.0), None, "between sections");
    assert_eq!(shown(50.0), Some(3), "a point marker is not a section");

    let next = |s: f64| prompt_at(&q, sec(s)).next.map(|n| (n.marker.0, n.at));
    assert_eq!(next(10.0), Some((2, sec(30.0))), "a section starting inside the current one is next");
    assert_eq!(next(35.0), Some((3, sec(40.0))));
    assert_eq!(next(60.0), Some((1, sec(70.0))), "the resumed outer section is next");
    assert_eq!(next(90.0), Some((4, sec(120.0))), "across a gap");
    assert_eq!(next(125.0), None, "after the last section");
    assert_eq!(prompt_at(&q, sec(10.0)).sections, 4);
}

#[test]
fn the_next_section_is_announced_near_the_end_and_between_sections() {
    let q = with_markers(vec![marker(1, 0.0, 60.0, "A", ""), marker(2, 80.0, 10.0, "B", ""), marker(3, 90.0, 4.0, "C", "")]);
    let soon = |s: f64| prompt_at(&q, sec(s)).next.map(|n| n.soon);
    assert_eq!(soon(5.0), Some(false), "not at the start of a long section");
    assert_eq!(soon(55.0), Some(true), "near its end");
    assert_eq!(soon(70.0), Some(true), "always between sections");
    assert_eq!(soon(81.0), Some(false), "a 10 s section waits until its second half");
    assert_eq!(soon(86.0), Some(true));
    assert!(prompt_at(&q, sec(55.0)).next.is_some_and(|n| n.starts_in == sec(80.0) - sec(55.0)));
}

#[test]
fn the_caption_line_comes_from_the_top_enabled_track_with_a_caption_there() {
    let mut q = sequence();
    let caption = |id: u64, start: f64, text: &str| Caption {
        id: ClipId(id),
        start: sec(start),
        duration: sec(4.0),
        text: text.into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    };
    let mut hidden = CaptionTrack::new(TrackId(900), "Hidden".into(), CaptionFormat::default());
    hidden.enabled = false;
    hidden.captions = vec![caption(1, 0.0, "hidden")];
    let mut top = CaptionTrack::new(TrackId(901), "Top".into(), CaptionFormat::default());
    top.captions = vec![caption(2, 10.0, "<i>top</i>\n second line")];
    let mut lower = CaptionTrack::new(TrackId(902), "Lower".into(), CaptionFormat::default());
    lower.captions = vec![caption(3, 0.0, "lower"), caption(4, 10.0, "lower too")];
    q.caption_tracks = vec![hidden, top, lower];
    let at = |s: f64| prompt_at(&q, sec(s)).caption.map(|c| (c.caption.0, c.lines));
    assert_eq!(at(1.0), Some((3, vec!["lower".to_string()])), "a turned-off track is skipped");
    assert_eq!(at(11.0), Some((2, vec!["top".to_string(), "second line".to_string()])), "the top track wins, markup removed");
    assert_eq!(at(20.0), None);
}

#[test]
fn prompter_inspect_reports_the_section_at_a_time_and_changes_nothing() {
    let mut s = Session::default();
    let none = s.execute("prompter.inspect", json!({}));
    assert!(matches!(none, Err(EngineError::Disabled(..))), "disabled without a sequence: {none:?}");

    s.execute("file.openDemoProject", json!({})).unwrap();
    let empty = s.execute("prompter.inspect", json!({})).unwrap();
    assert!(empty["section"].is_null() && empty["sections"] == 0, "the demo's markers are all point markers: {empty}");

    let rate = s.sequence_rate();
    let fps = rate.frame_at(Tick(TICKS_PER_SECOND));
    s.execute("markers.add", json!({"time": 0, "name": "Intro", "comment": "- Hello [0:02]\n- Topic", "durationFrames": 6 * fps})).unwrap();
    s.execute("markers.add", json!({"time": rate.tick_of(6 * fps).0, "name": "Outro", "durationFrames": 3 * fps})).unwrap();
    let before = (s.project.clone(), s.history.undo.len(), s.playhead());
    let r = s.execute("prompter.inspect", json!({"seconds": 3.0})).unwrap();
    assert_eq!(r["section"]["name"], "Intro");
    assert_eq!(r["section"]["current"], 1, "the second point after the first's two seconds");
    assert_eq!(r["next"]["name"], "Outro");
    let by_frame = s.execute("prompter.inspect", json!({"frame": 7 * fps})).unwrap();
    assert_eq!(by_frame["section"]["name"], "Outro", "frames work as well as seconds");
    s.execute("playhead.set", json!({"frame": 7 * fps})).unwrap();
    let at_playhead = s.execute("prompter.inspect", json!({})).unwrap();
    assert_eq!(at_playhead["section"]["name"], "Outro", "without a time it answers for the playhead");
    assert!(s.execute("prompter.inspect", json!({"seconds": -1.0})).is_err(), "a negative time is refused");
    assert_eq!(*s.project, *before.0, "the query changes nothing");
    assert_eq!(s.history.undo.len(), before.1);
}

#[test]
fn hostile_comments_markers_and_times_never_panic() {
    let long_line = "x".repeat(200_000);
    let many_points = "- p\n".repeat(MAX_POINTS * 3);
    let sample = "Script ü 😀 e\u{301}\n- point [1:00]\n- [0:05]\n- \u{202e}rtl\u{200b} [99:59:59]\n* [999999999:00]\n• [::]\n- []\n- [0:0x]\n\r\n";
    let mut comments: Vec<String> = vec![
        String::new(),
        "\n\n\n".into(),
        "-".into(),
        "- ".into(),
        "[".into(),
        "]".into(),
        "- [".into(),
        "- ]".into(),
        "- [1:00".into(),
        "- 1:00]".into(),
        "- [١:٠٠]".into(),
        "\u{0}\u{feff}\u{fffd}".into(),
        long_line.clone(),
        format!("- {long_line} [0:10]"),
        many_points.clone(),
        format!("{long_line}é"),
    ];
    // every prefix of a mixed sample, cut at each character boundary
    comments.extend(sample.char_indices().map(|(i, _)| sample[..i].to_string()));
    let times = [i64::MIN, Tick::MIN.0, -1, 0, 1, TICKS_PER_SECOND, Tick::MAX.0, i64::MAX];
    for (i, c) in comments.iter().enumerate() {
        let notes = std::panic::catch_unwind(|| parse_notes(c)).unwrap_or_else(|_| panic!("comment {i} panicked"));
        assert!(notes.points.len() <= MAX_POINTS);
        let markers = vec![
            marker(1, 0.0, 30.0, "A", c),
            Marker { start: Tick::MAX, duration: Tick::MAX, ..marker(2, 0.0, 0.0, c, c) },
            Marker { start: Tick(i64::MIN), duration: Tick(i64::MAX), ..marker(3, 0.0, 0.0, "", c) },
            Marker { duration: Tick(-5), ..marker(4, 10.0, 0.0, "negative", c) },
            marker(5, 10.0, 0.0, "point", c),
            marker(6, 10.0, 30.0, "overlap", c),
            marker(7, 10.0, 30.0, "same", c),
        ];
        let q = with_markers(markers);
        for &t in &times {
            let r = std::panic::catch_unwind(|| prompt_at(&q, Tick(t)));
            let p = r.unwrap_or_else(|_| panic!("comment {i} at {t} panicked"));
            if let Some(s) = p.section {
                assert!((0.0..=1.0).contains(&s.progress));
                assert!(s.script.is_none_or(|x| x.offset <= x.chars));
            }
        }
    }
    assert!(parse_notes(&long_line).truncated && parse_notes(&many_points).truncated, "a cut is reported");
    assert!(prompt_at(&sequence(), Tick::ZERO).section.is_none(), "a sequence with no markers");
}
