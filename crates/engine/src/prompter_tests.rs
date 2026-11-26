//! Tests of [`crate::prompter`]: talking points and script from a marker comment, their timing in
//! the section, sections and the next one around overlapping range markers, the caption line, the
//! `prompter.inspect` query, and hostile comments and markers; Next and Previous through points,
//! paragraphs and sections, the ahead/behind gap, and the prompter log of a voice-over take.

use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, Label, Marker, MarkerId, MarkerKind, Project, Sequence, SequenceSettings, TrackId};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::prompter::{Anchor, MAX_POINTS, Note, PointState, allocate, next_step, parse_notes, previous_step, prompt_at, section_at};
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
fn prompter_inspect_refuses_a_time_it_cannot_read_instead_of_answering_for_the_playhead() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let fps = s.sequence_rate().frame_at(Tick(TICKS_PER_SECOND));
    s.execute("markers.add", json!({"time": 0, "name": "Intro", "durationFrames": 6 * fps})).unwrap();
    for bad in [json!({"timecode": "invalid"}), json!({"seconds": "three"}), json!({"frame": "x"}), json!({"time": 1.5})] {
        let r = s.execute("prompter.inspect", bad.clone());
        assert!(matches!(r, Err(EngineError::BadParams { .. })), "{bad} is refused: {r:?}");
    }
    assert!(s.execute("prompter.inspect", json!({"timecode": "00:00:01:00"})).is_ok(), "a valid timecode still works");
    assert!(s.execute("prompter.inspect", json!({"timecode": null})).is_ok(), "null is no time: the playhead");
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

/// What is shown at `t`: the section and its current point or script paragraph.
fn shown(q: &Sequence, t: Tick) -> Option<(u64, Option<usize>, Option<usize>)> {
    prompt_at(q, t).section.map(|s| (s.marker.0, s.current, s.script.map(|x| x.paragraph)))
}

fn three_sections() -> Sequence {
    with_markers(vec![
        marker(1, 0.0, 30.0, "Points", "- One [0:10]\n- Two\n- Three"),
        marker(2, 40.0, 30.0, "Script", "First paragraph, a long one.\nStill the first.\n\nSecond.\n\n\nThird and last paragraph."),
        marker(3, 70.0, 10.0, "Bare", ""),
    ])
}

#[test]
fn next_visits_every_point_and_paragraph_in_order_across_sections_and_stops_at_the_last() {
    let q = three_sections();
    let mut t = Tick::ZERO;
    let mut visited = vec![shown(&q, t)];
    while let Some(n) = next_step(&q, t) {
        assert!(n > t, "Next moves forward: {t:?} -> {n:?}");
        assert_ne!(shown(&q, n), shown(&q, Tick(n.0 - 1)), "a step is where the point, paragraph or section changes ({n:?})");
        visited.push(shown(&q, n));
        t = n;
        assert!(visited.len() < 20, "Next must end");
    }
    let expected = vec![
        Some((1, Some(0), None)),
        Some((1, Some(1), None)),
        Some((1, Some(2), None)),
        Some((2, None, Some(0))),
        Some((2, None, Some(1))),
        Some((2, None, Some(2))),
        Some((3, None, None)),
    ];
    assert_eq!(visited, expected, "each point, then each paragraph, then the section without notes; the gap is skipped");

    // back again, pressing Previous at each step's start
    let mut back = vec![shown(&q, t)];
    while let Some(p) = previous_step(&q, t) {
        assert!(p < t);
        back.push(shown(&q, p));
        t = p;
    }
    back.reverse();
    assert_eq!(back, expected, "Previous at a step's start goes to the step before, across sections");
    assert_eq!(t, Tick::ZERO, "and stops at the first section's start");
}

#[test]
fn previous_restarts_the_current_point_unless_it_has_just_started() {
    let q = three_sections();
    let grace = sec(crate::prompter::PREVIOUS_GRACE_SECONDS as f64);
    let second = sec(10.0);
    assert_eq!(previous_step(&q, second + grace + sec(1.0)), Some(second), "well into a point: back to its start");
    assert_eq!(previous_step(&q, second + sec(0.5)), Some(Tick::ZERO), "just after its start: the point before");
    assert_eq!(previous_step(&q, sec(0.5)), Some(Tick::ZERO), "just into the first point: its start");
    assert_eq!(previous_step(&q, Tick::ZERO), None, "at the first step there is nothing before");
    let gap = sec(35.0);
    assert!(section_at(&q, gap).is_none());
    assert_eq!(previous_step(&q, gap).and_then(|t| shown(&q, t)), Some((1, Some(2), None)), "from a gap: the previous section's last point");
    assert_eq!(next_step(&q, gap), Some(sec(40.0)), "and Next: the next section's start");
    let late = with_markers(vec![marker(1, 10.0, 10.0, "Late", "")]);
    assert_eq!(previous_step(&late, sec(5.0)), None, "before the first section");
    assert_eq!(next_step(&late, sec(5.0)), Some(sec(10.0)));
    assert_eq!(next_step(&late, sec(25.0)), None, "after the last section");
}

#[test]
fn next_follows_what_is_shown_where_sections_overlap() {
    let q = with_markers(vec![marker(1, 0.0, 100.0, "Outer", "- o1\n- o2\n- o3\n- o4"), marker(2, 20.0, 20.0, "Inner", "- i1\n- i2")]);
    let mut t = Tick::ZERO;
    let mut steps = vec![t];
    while let Some(n) = next_step(&q, t) {
        steps.push(n);
        t = n;
    }
    for &s in &steps {
        let p = prompt_at(&q, s);
        assert_eq!(p.section.map(|x| x.marker), section_at(&q, s).map(|m| m.id), "a step shows the section the overlay shows");
    }
    assert!(!steps.contains(&sec(25.0)), "the outer point hidden under the nested section is skipped: {steps:?}");
    assert!(steps.contains(&sec(20.0)) && steps.contains(&sec(30.0)), "the nested section's points are visited");
    assert!(steps.contains(&sec(40.0)), "the outer section is a step where it resumes, mid-point: {steps:?}");
    assert_eq!(previous_step(&q, sec(40.0)), Some(sec(30.0)), "Previous from the resumed section goes into the nested one");
}

/// The demo project with two sections at frame boundaries; returns the playhead to start from.
fn session_with_sections() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();
    let fps = rate.frame_at(Tick(TICKS_PER_SECOND));
    s.execute("markers.add", json!({"time": 0, "name": "Intro", "comment": "- Hello [0:02]\n- Topic [0:02]\n- Plan", "durationFrames": 6 * fps})).unwrap();
    s.execute("markers.add", json!({"time": rate.tick_of(8 * fps).0, "name": "Outro", "durationFrames": 3 * fps})).unwrap();
    s
}

fn prompter(s: &mut Session) -> serde_json::Value {
    s.execute("prompter.inspect", json!({})).unwrap()
}

#[test]
fn moving_the_prompter_changes_neither_the_playhead_nor_the_project_nor_the_history() {
    let mut s = session_with_sections();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let (project, undo, playhead) = (s.project.clone(), s.history.undo.len(), s.playhead());
    assert!(prompter(&mut s)["direction"].is_null() && prompter(&mut s)["gap"] == 0, "in step to begin with");

    let r = s.execute("prompter.next", json!({})).unwrap();
    assert!(r["gap"].as_i64().unwrap() > 0 && r["direction"] == "ahead", "Next before the picture gets there: ahead: {r}");
    assert_eq!(r["point"], 1, "on the next point");
    let q = prompter(&mut s);
    assert_eq!(q["section"]["current"], 1, "the query answers for the prompt time, not the playhead");
    assert_eq!(q["promptTime"].as_i64().unwrap() - q["playhead"].as_i64().unwrap(), q["gap"].as_i64().unwrap());

    // the prompter keeps moving with the playhead from its new place
    let gap = q["gap"].as_i64().unwrap();
    s.execute("playhead.set", json!({"seconds": 2.5})).unwrap();
    assert_eq!(prompter(&mut s)["gap"].as_i64(), Some(gap), "the gap stays as the playhead moves");
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();

    // two steps back: behind the picture
    s.execute("prompter.previous", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 4.5})).unwrap();
    let r = s.execute("prompter.previous", json!({})).unwrap();
    assert!(r["gap"].as_i64().unwrap() < 0 && r["direction"] == "behind", "the picture moved on without the prompter: {r}");

    let r = s.execute("prompter.toPlayhead", json!({})).unwrap();
    assert!(r["gap"] == 0 && r["direction"].is_null(), "back in step: {r}");
    assert!(matches!(s.execute("prompter.toPlayhead", json!({})), Err(EngineError::Disabled(..))), "nothing to return from");
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    assert_eq!(s.playhead(), playhead);
    assert_eq!(*s.project, *project, "no command changed the project");
    assert_eq!(s.history.undo.len(), undo, "or made an undo step");
}

#[test]
fn next_and_previous_are_disabled_with_a_reason_where_they_cannot_move() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    for id in ["prompter.next", "prompter.previous"] {
        match s.execute(id, json!({})) {
            Err(EngineError::Disabled(_, why)) => assert!(why.contains("range markers"), "{id} says how to make sections: {why}"),
            other => panic!("{id} without sections: {other:?}"),
        }
    }
    let mut s = session_with_sections();
    s.execute("playhead.set", json!({"seconds": 0})).unwrap();
    assert!(matches!(s.execute("prompter.previous", json!({})), Err(EngineError::Disabled(..))), "at the first section's start");
    while s.execute("prompter.next", json!({})).is_ok() {}
    let last = prompter(&mut s);
    assert_eq!(last["section"]["name"], "Outro", "Next ends on the last section");
    assert!(matches!(s.execute("prompter.next", json!({})), Err(EngineError::Disabled(..))), "and is then disabled");
}

#[test]
fn a_take_logs_every_prompter_move_relative_to_its_record_point() {
    let mut s = session_with_sections();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("prompter.next", json!({})).unwrap();
    let record = s.playhead();
    s.execute("audio.voiceover.start", json!({"preroll": 0})).unwrap();
    assert_eq!(prompter(&mut s)["gap"], 0, "a recording starts with the prompter at the playhead");

    for (at, id) in [(1.5, "prompter.next"), (3.5, "prompter.previous"), (3.75, "prompter.next"), (4.0, "prompter.toPlayhead")] {
        s.execute("playhead.set", json!({"seconds": at})).unwrap();
        s.execute(id, json!({})).unwrap();
    }
    let live = s.execute("prompter.takeLog", json!({})).unwrap();
    assert_eq!(live["recording"], true);
    let log = live["inProgress"].clone();
    let entries = log["entries"].as_array().unwrap();
    let kinds: Vec<&str> = entries.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["start", "next", "previous", "next", "toPlayhead"], "a starting entry, then each move in order");
    let n = |e: &serde_json::Value, k: &str| e[k].as_i64().unwrap();
    assert_eq!(n(&entries[0], "takeTime"), 0, "the take starts at its record point");
    for e in entries {
        assert_eq!(n(e, "takeTime"), n(e, "playhead") - record.0, "take times count from the record point");
        let at = s.execute("prompter.inspect", json!({"time": n(e, "promptAfter")})).unwrap();
        assert_eq!(e["marker"], at["section"]["marker"], "the entry names the section the prompt landed in");
        assert_eq!(e["point"], at["section"]["current"], "and its point");
    }
    for w in entries.windows(2) {
        let ran = n(&w[0], "promptAfter") + (n(&w[1], "playhead") - n(&w[0], "playhead"));
        assert_eq!(n(&w[1], "promptBefore"), ran, "between moves the prompt runs with the playhead, so the log replays");
    }
    assert_eq!(n(&entries[4], "promptAfter"), n(&entries[4], "playhead"), "return-to-playhead lands on the playhead");

    let project_before_stop = s.project.clone();
    let dir = std::env::temp_dir().join(format!("filmcraft-prompter-{}", std::process::id()));
    let stop = s.execute("audio.voiceover.stop", json!({"time": sec(5.0).0, "dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(stop["promptLog"], log, "stop returns the finished take's log");
    let after = s.execute("prompter.takeLog", json!({})).unwrap();
    assert!(after["recording"] == false && after["inProgress"].is_null() && after["last"] == log, "and keeps it as the last take's");
    assert!(!std::sync::Arc::ptr_eq(&project_before_stop, &s.project), "the take was placed");

    // a take without moves logs only where it started, during the pre-roll
    s.execute("playhead.set", json!({"seconds": 4.0})).unwrap();
    let start = s.execute("audio.voiceover.start", json!({"preroll": 1.0})).unwrap();
    let quiet = s.execute("audio.voiceover.stop", json!({"discard": true})).unwrap();
    let entries = quiet["promptLog"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{quiet}");
    assert!(n(&entries[0], "takeTime") < 0, "the capture starts before the record point");
    assert_eq!(n(&entries[0], "playhead"), start["captureStart"].as_i64().unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hostile_prompter_positions_never_panic() {
    let q = with_markers(vec![
        marker(1, 0.0, 30.0, "A", "x\n\ny\n\nz"),
        Marker { start: Tick::MAX, duration: Tick::MAX, ..marker(2, 0.0, 0.0, "", "- a\n- b") },
        Marker { start: Tick(i64::MIN), duration: Tick(i64::MAX), ..marker(3, 0.0, 0.0, "", "p\n\nq") },
        Marker { duration: Tick(1), ..marker(4, 10.0, 0.0, "one tick", "a\n\nb\n\nc") },
        marker(5, 10.0, 30.0, "overlap", "- [99:59:59]\n- b"),
    ]);
    let times = [i64::MIN, Tick::MIN.0, -1, 0, 1, sec(10.0).0, sec(25.0).0, Tick::MAX.0, i64::MAX];
    for &t in &times {
        let r = std::panic::catch_unwind(|| (next_step(&q, Tick(t)), previous_step(&q, Tick(t))));
        let (n, p) = r.unwrap_or_else(|_| panic!("steps from {t} panicked"));
        assert!(n.is_none_or(|n| n > Tick(t)) && p.is_none_or(|p| p < Tick(t)), "steps move the right way from {t}");
    }
    let mut s = session_with_sections();
    let sequence = s.state.active_sequence.unwrap();
    for &prompt in &times {
        for &playhead in &[i64::MIN, 0, i64::MAX] {
            s.prompter.anchor = Some(Anchor { sequence, epoch: s.document_epoch, prompt: Tick(prompt), playhead: Tick(playhead) });
            for id in ["prompter.next", "prompter.previous", "prompter.toPlayhead", "prompter.inspect", "prompter.takeLog"] {
                let mut s2 = std::panic::AssertUnwindSafe(&mut s);
                let r = std::panic::catch_unwind(move || s2.execute(id, json!({})).map(|_| ()));
                assert!(r.is_ok(), "{id} at prompt {prompt}, anchor playhead {playhead} panicked");
            }
        }
    }
}
