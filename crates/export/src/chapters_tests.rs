//! #2747: chapter markers are written into MP4, MOV and M4A exports (QuickTime chapter track and
//! Nero `chpl`), timed from the start of the export range. ffprobe is an external test oracle only.

use filmcraft_isobmff::Chapter;
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind};
use filmcraft_time::TICKS_PER_SECOND;

use super::*;
use crate::settings_tests::{Scratch, ffprobe_json, matte};

fn ms(v: i64) -> Tick {
    Tick(v * TICKS_PER_SECOND / 1000)
}

fn marker(id: u64, at: Tick, name: &str, kind: MarkerKind) -> Marker {
    Marker { id: MarkerId(id), start: at, duration: Tick::ZERO, name: name.into(), comment: String::new(), kind, color: Label::Iris }
}

/// The matte sequence (1 s of picture, a tone) with these markers.
fn with_markers(markers: Vec<Marker>) -> (Arc<Project>, ItemId, filmcraft_render::SourceMap) {
    let (p, seq, m) = matte([0.2, 0.4, 0.6, 1.0], 160, 90, Some(-12.0));
    let mut p = Arc::unwrap_or_clone(p);
    p.sequence_mut(seq).unwrap().markers = markers;
    (Arc::new(p), seq, m)
}

fn standard_markers() -> Vec<Marker> {
    vec![
        marker(1, ms(0), "Before the range", MarkerKind::Chapter),
        marker(2, ms(250), "Opening", MarkerKind::Chapter),
        marker(3, ms(600), "a comment, not a chapter", MarkerKind::Comment),
        marker(4, ms(500), "", MarkerKind::Chapter),
        marker(5, ms(750), "Schluss — 日本", MarkerKind::Chapter),
        marker(6, ms(1000), "At the range end", MarkerKind::Chapter),
    ]
}

/// Range 125 ms – 1 s (frames 3–24 at 24 fps).
fn range() -> TimeRange {
    TimeRange::new(ms(125), ms(875))
}

fn want() -> Vec<Chapter> {
    vec![
        Chapter { start: 125, title: "Opening".into() },
        Chapter { start: 375, title: "Chapter 2".into() },
        Chapter { start: 625, title: "Schluss — 日本".into() },
    ]
}

fn read_back(path: &str) -> (Vec<Chapter>, Vec<Chapter>) {
    let bytes = std::fs::read(path).unwrap();
    let f = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
    assert_eq!(f.timescale, CHAPTER_TIMESCALE);
    (f.read_chapters(bytes.as_slice()).unwrap(), f.chpl_chapters.clone())
}

#[test]
fn chapter_markers_in_the_range_are_written_into_mp4_mov_and_m4a() {
    let (p, seq, m) = with_markers(standard_markers());
    let dir = Scratch::new("chapters");
    for (name, format, mux) in [("c.mp4", Format::H264, Multiplexer::Mp4), ("c.mov", Format::H264, Multiplexer::Mov), ("c.m4a", Format::Aac, Multiplexer::Mp4)]
    {
        let path = dir.path(name);
        let s = ExportSettings { format, path: path.clone(), multiplexer: mux, range: Some(range()), ..Default::default() };
        assert!(s.include_chapters, "on by default");
        export(&p, seq, &s, &m, &Progress::default()).unwrap();
        let (track, chpl) = read_back(&path);
        assert_eq!(track, want(), "{name}: QuickTime chapter track");
        assert_eq!(chpl, want(), "{name}: chpl");
        if let Some(j) = ffprobe_json(&["-show_chapters"], &path) {
            let ch = j["chapters"].as_array().unwrap();
            let got: Vec<(i64, &str)> = ch
                .iter()
                .map(|c| ((c["start_time"].as_str().unwrap().parse::<f64>().unwrap() * 1000.0).round() as i64, c["tags"]["title"].as_str().unwrap()))
                .collect();
            assert_eq!(got, vec![(125, "Opening"), (375, "Chapter 2"), (625, "Schluss — 日本")], "{name}: ffprobe -show_chapters");
        }
    }
}

#[test]
fn chapters_can_be_switched_off_and_sequences_without_chapter_markers_get_none() {
    let dir = Scratch::new("chapters-off");
    let (p, seq, m) = with_markers(standard_markers());
    let path = dir.path("off.mp4");
    let s = ExportSettings { path: path.clone(), range: Some(range()), include_chapters: false, ..Default::default() };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let (track, chpl) = read_back(&path);
    assert!(track.is_empty() && chpl.is_empty());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(filmcraft_isobmff::open(bytes.as_slice()).unwrap().tracks.len(), 2, "video + audio, no chapter track");

    let (p, seq, m) = with_markers(vec![marker(1, ms(250), "just a comment", MarkerKind::Comment)]);
    let path = dir.path("none.mp4");
    export(&p, seq, &ExportSettings { path: path.clone(), ..Default::default() }, &m, &Progress::default()).unwrap();
    let (track, chpl) = read_back(&path);
    assert!(track.is_empty() && chpl.is_empty());
}

#[test]
fn hostile_chapter_markers_do_not_panic() {
    let long = "ü".repeat(5000);
    let mut markers = vec![
        marker(1, ms(100), "dup a", MarkerKind::Chapter),
        marker(2, ms(100), "dup b", MarkerKind::Chapter),
        marker(3, Tick(-5), "negative", MarkerKind::Chapter),
        marker(4, Tick::MAX, "max", MarkerKind::Chapter),
        marker(5, Tick(i64::MIN), "min", MarkerKind::Chapter),
        marker(6, ms(200), &long, MarkerKind::Chapter),
        marker(7, ms(300), "\0\u{1b}[31m\n", MarkerKind::Chapter),
        marker(8, ms(0), "   ", MarkerKind::Chapter),
    ];
    // far more markers than a file can carry
    markers.extend((0..2000).map(|i| marker(100 + i, ms(400 + i as i64), "many", MarkerKind::Chapter)));
    let (p, seq, m) = with_markers(markers);
    let dir = Scratch::new("chapters-hostile");
    let path = dir.path("h.mov");
    let s = ExportSettings { path: path.clone(), multiplexer: Multiplexer::Mov, ..Default::default() };
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
    let (track, chpl) = read_back(&path);
    assert_eq!(track, chpl);
    assert_eq!(track.len(), filmcraft_isobmff::MAX_CHAPTERS);
    assert_eq!(track[0], Chapter { start: 0, title: "Chapter 1".into() });
    assert_eq!(track[1], Chapter { start: 100, title: "dup a".into() });
    assert!(track[2].title.len() <= filmcraft_isobmff::MAX_CHAPTER_TITLE_BYTES && track[2].title.starts_with('ü'));
    assert_eq!(track[3].title, "[31m");
    // the helper alone, with an origin after every marker and a zero timescale
    let s = ExportSettings::default();
    for c in export_chapters(&p, seq, &s, TimeRange::new(Tick::ZERO, Tick::MAX), Tick::MAX, 0) {
        assert_eq!(c.start, 0);
    }
}

#[test]
fn include_chapters_round_trips_through_json() {
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264"})).unwrap();
    assert!(s.include_chapters);
    let s: ExportSettings = serde_json::from_value(serde_json::json!({"chapters": false})).unwrap();
    assert!(!s.include_chapters);
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["includeChapters"], false);
    let back: ExportSettings = serde_json::from_value(v).unwrap();
    assert!(!back.include_chapters);
}
