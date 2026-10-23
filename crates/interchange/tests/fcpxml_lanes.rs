//! FCPXML connected-clip lanes are track indexes, so a hostile `lane` (or a pile of overlapping clips) must
//! not grow a sequence past the 999 tracks of a kind the engine allows. Values stay moderate so that
//! reverting the fix fails these tests instead of exhausting memory.

use filmcraft_interchange::{Format, import};
use filmcraft_project::TrackKind;

/// The engine's limit of tracks of one kind (`sequence_tools::MAX_TRACKS`).
const LIMIT: usize = 999;

/// A connected clip's `lane` became that many tracks (lane 100000 made 200 002 of them, and a lane in
/// the billions allocated until the machine ran out of memory). Lanes beyond the track limit are
/// skipped with a warning, nested lanes included (they add up). The values stay moderate so that
/// reverting the fix fails this test instead of exhausting memory.
#[test]
fn huge_lanes_are_skipped_not_turned_into_tracks() {
    let doc = |inner: &str| {
        format!(
            r#"<?xml version="1.0"?><fcpxml version="1.10"><resources><format id="f1" frameDuration="100/2500s" width="1920" height="1080"/>
<asset id="a1" name="c" start="0s" duration="10s" hasVideo="1" hasAudio="1" format="f1"><media-rep kind="original-media" src="file:///tmp/c.mov"/></asset></resources>
<library><event name="E"><project name="P"><sequence format="f1" tcStart="0s"><spine>
<asset-clip ref="a1" offset="0s" duration="5s">{inner}</asset-clip></spine></sequence></project></event></library></fcpxml>"#
        )
    };
    let clip = |lane: i64| format!(r#"<asset-clip ref="a1" lane="{lane}" offset="0s" duration="1s"/>"#);
    for inner in [clip(100_000), clip(-100_000), format!(r#"<asset-clip ref="a1" lane="600" offset="0s" duration="1s">{}</asset-clip>"#, clip(600))] {
        let (imported, report) = import(doc(&inner).as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        assert!(report.mentions("lanes a sequence can have"), "{inner}: {report}");
        for seq in imported.sequences.iter().filter_map(|s| imported.project.sequence(*s)) {
            let tracks = seq.tracks(TrackKind::Video).len() + seq.tracks(TrackKind::Audio).len();
            assert!(tracks <= 2 * LIMIT, "{inner}: {tracks} tracks");
        }
    }
    // the largest lanes that fit are imported, and no sequence is left with more tracks than the engine allows
    for lane in [LIMIT as i64 - 1, -(LIMIT as i64)] {
        let (imported, report) = import(doc(&clip(lane)).as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        assert!(!report.mentions("lanes a sequence can have"), "lane {lane}: {report}");
        let q = imported.sequences.iter().filter_map(|s| imported.project.sequence(*s)).next().expect("a sequence");
        assert_eq!(q.tracks(TrackKind::Video).len().max(q.tracks(TrackKind::Audio).len()), LIMIT, "lane {lane}");
    }
    // one past either end is skipped
    for lane in [LIMIT as i64, -(LIMIT as i64) - 1] {
        let (_, report) = import(doc(&clip(lane)).as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        assert!(report.mentions("lanes a sequence can have"), "lane {lane}: {report}");
    }
}

/// `Builder::place` moves an overlapping clip to the next free track without a limit, so clips piled
/// at the last lane (or just on one lane) grew a sequence past the engine's 999 tracks. A clip that
/// finds no room is skipped with a warning. A `lane` that is not an integer that fits is reported too.
#[test]
fn overlapping_clips_cannot_pass_the_track_limit() {
    let doc = |inner: &str| {
        format!(
            r#"<?xml version="1.0"?><fcpxml version="1.10"><resources><format id="f1" frameDuration="100/2500s" width="1920" height="1080"/>
<asset id="a1" name="c" start="0s" duration="10s" hasVideo="1" hasAudio="1" format="f1"><media-rep kind="original-media" src="file:///tmp/c.mov"/></asset></resources>
<library><event name="E"><project name="P"><sequence format="f1" tcStart="0s"><spine>
<asset-clip ref="a1" offset="0s" duration="5s">{inner}</asset-clip></spine></sequence></project></event></library></fcpxml>"#
        )
    };
    let piled = |lane: i64, n: usize| (0..n).map(|_| format!(r#"<asset-clip ref="a1" lane="{lane}" offset="0s" duration="1s"/>"#)).collect::<String>();
    for (lane, n) in [(998, 3), (1, LIMIT + 5), (-1, LIMIT + 5)] {
        let (imported, report) = import(doc(&piled(lane, n)).as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        assert!(report.mentions("tracks a sequence can have; skipped"), "lane {lane}: {report}");
        for q in imported.sequences.iter().filter_map(|s| imported.project.sequence(*s)) {
            assert!(q.tracks(TrackKind::Video).len() <= LIMIT && q.tracks(TrackKind::Audio).len() <= LIMIT, "lane {lane}");
        }
    }
    for lane in ["99999999999", "1.5", "x"] {
        let inner = format!(r#"<asset-clip ref="a1" lane="{lane}" offset="0s" duration="1s"/>"#);
        let (_, report) = import(doc(&inner).as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        assert!(report.mentions("not a lane a sequence can have"), "lane {lane}: {report}");
    }
}
