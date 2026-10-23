//! An FCP7 `<sequence>` listing more tracks than the engine allows (999 of a kind) is cut there, with a
//! warning, so the import is a sequence the engine itself could have built.

use filmcraft_interchange::{Format, import};
use filmcraft_project::TrackKind;

/// The engine's limit of tracks of one kind (`sequence_tools::MAX_TRACKS`).
const LIMIT: usize = 999;

fn doc(video: usize, audio: usize) -> String {
    let tracks = |n: usize| "<track/>".repeat(n);
    format!(
        r#"<?xml version="1.0"?><xmeml version="4"><sequence id="s"><name>S</name><rate><timebase>25</timebase><ntsc>FALSE</ntsc></rate>
<media><video>{}</video><audio>{}</audio></media></sequence></xmeml>"#,
        tracks(video),
        tracks(audio)
    )
}

fn counts(video: usize, audio: usize) -> (usize, usize, bool) {
    let (imp, rep) = import(doc(video, audio).as_bytes(), Format::Fcp7Xml, None).expect("import succeeds");
    let q = imp.project.sequence(imp.sequences[0]).unwrap();
    (q.tracks(TrackKind::Video).len(), q.tracks(TrackKind::Audio).len(), rep.mentions("tracks; the rest were skipped"))
}

#[test]
fn tracks_beyond_the_limit_are_skipped_with_a_warning() {
    // moderate counts, so reverting the cap fails this test instead of exhausting memory
    assert_eq!(counts(LIMIT + 50, 3), (LIMIT, 3, true));
    assert_eq!(counts(2, LIMIT + 50), (2, LIMIT, true));
}

#[test]
fn the_limit_itself_is_not_reported() {
    assert_eq!(counts(LIMIT, LIMIT), (LIMIT, LIMIT, false));
    assert_eq!(counts(3, 2), (3, 2, false));
}

/// Reviewer's case: 998 empty tracks, then a 999th holding two overlapping clips. The second one used to be
/// moved to a 1 000th track by `Builder::place`; now it is skipped with a warning.
#[test]
fn an_overlap_on_the_last_allowed_track_is_skipped() {
    let clip = |id: &str| {
        format!(
            r#"<clipitem id="{id}"><name>A</name><start>0</start><end>25</end><in>0</in><out>25</out><file id="f"><name>a.mov</name><pathurl>file:///tmp/a.mov</pathurl><media><video/></media></file></clipitem>"#
        )
    };
    let xml = format!(
        r#"<?xml version="1.0"?><xmeml version="4"><sequence id="s"><name>S</name><rate><timebase>25</timebase><ntsc>FALSE</ntsc></rate>
<media><video>{}<track>{}{}</track></video></media></sequence></xmeml>"#,
        "<track/>".repeat(LIMIT - 1),
        clip("c1"),
        clip("c2")
    );
    let (imp, rep) = import(xml.as_bytes(), Format::Fcp7Xml, None).expect("import succeeds");
    let q = imp.project.sequence(imp.sequences[0]).unwrap();
    assert_eq!(q.tracks(TrackKind::Video).len(), LIMIT);
    assert_eq!(q.tracks(TrackKind::Video)[LIMIT - 1].items.len(), 1, "the overlapping clip was not kept");
    assert!(rep.mentions("overlaps clips on all 999 tracks"), "{rep}");
}
