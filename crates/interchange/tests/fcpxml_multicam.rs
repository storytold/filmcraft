//! Synthetic multicam interchange regressions; no external media is required.
use filmcraft_interchange::{Format, import};
use filmcraft_project::Project;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn fixture() -> String {
    let mut resources = String::from(
        r#"<format id="f" frameDuration="1/30s" width="320" height="180"/>
<asset id="mix" name="Master Mix" hasAudio="1" duration="10s" src="mix.wav"/>"#,
    );
    let mut angles = String::new();
    for (camera, delay) in [0, 1, 0, 2].into_iter().enumerate() {
        let mut clips = String::new();
        let mut offset = delay;
        let mut segment = 0;
        while offset < 10 {
            let duration = (10 - offset).min(4);
            let id = format!("c{camera}s{segment}");
            resources.push_str(&format!(r#"<asset id="{id}" hasVideo="1" hasAudio="0" format="f" start="100s" duration="{duration}s" src="{id}.mov"/>"#));
            clips.push_str(&format!(r#"<asset-clip ref="{id}" offset="{offset}s" start="100s" duration="{duration}s" srcEnable="video"/>"#));
            offset += duration;
            segment += 1;
        }
        angles.push_str(&format!(r#"<mc-angle name="Camera{camera}" angleID="angle{camera}">{clips}</mc-angle>"#));
    }
    let mut cuts = String::new();
    for (index, (start, duration, angle)) in [(0, 3, 0), (3, 2, 1), (5, 1, 1), (6, 2, 3), (8, 2, 2)].into_iter().enumerate() {
        let mix = if index == 0 { r#"<asset-clip ref="mix" lane="-1" offset="0s" start="0s" duration="10s" srcEnable="audio"/>"# } else { "" };
        cuts.push_str(&format!(r#"<mc-clip ref="mc" offset="{start}s" start="{start}s" duration="{duration}s" srcEnable="video"><mc-source angleID="angle{angle}" srcEnable="video"/>{mix}</mc-clip>"#));
    }
    format!(
        r#"<fcpxml version="1.10"><resources>{resources}<media id="mc" name="Angles"><multicam format="f" tcStart="0s">{angles}</multicam></media></resources><event name="Test"><project name="Cuts"><sequence format="f"><spine>{cuts}</spine></sequence></project></event></fcpxml>"#
    )
}

fn seconds(n: i64) -> Tick {
    Tick(n * TICKS_PER_SECOND)
}

#[test]
fn multicam_preserves_segmented_angles_edits_and_continuous_master() {
    let (imported, report) = import(fixture().as_bytes(), Format::Fcpxml, None).unwrap();
    assert!(report.entries.is_empty(), "{report:?}");
    let project = imported.project;
    let main = project.sequence(imported.sequences[0]).unwrap();
    let cuts = &main.video_tracks[0].items;
    assert_eq!(cuts.len(), 5);
    for (clip, (start, duration, angle)) in cuts.iter().zip([(0, 3, 0), (3, 2, 1), (5, 1, 1), (6, 2, 3), (8, 2, 2)]) {
        assert_eq!((clip.start, clip.duration, clip.source_in), (seconds(start), seconds(duration), seconds(start)));
        assert_eq!(clip.multicam.unwrap().angle, angle);
    }
    let source = project.sequence(cuts[0].item).unwrap();
    let mc = source.multicam.as_ref().unwrap();
    assert_eq!(mc.cameras.len(), 4);
    assert_eq!(source.video_tracks.iter().map(|t| t.items.len()).collect::<Vec<_>>(), [3, 3, 3, 2]);
    for (track, delay) in source.video_tracks.iter().zip([0, 1, 0, 2]) {
        assert_eq!(track.items[0].start, seconds(delay));
        assert!(track.items.iter().all(|c| c.source_in == Tick::ZERO));
    }
    let audio: Vec<_> = main.audio_tracks.iter().flat_map(|t| &t.items).collect();
    assert_eq!(audio.len(), 1);
    assert_eq!((audio[0].start, audio[0].duration), (Tick::ZERO, seconds(10)));
    let reopened: Project = serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&reopened).unwrap(), serde_json::to_value(&project).unwrap());
    let metadata = &reopened.item(cuts[0].item).unwrap().metadata;
    assert_eq!(metadata.get("fcpxml.angleID.3").map(String::as_str), Some("angle3"));
}

#[test]
fn multicam_unknown_and_duplicate_angle_ids_warn_without_wrong_camera() {
    for xml in [
        fixture().replace("angleID=\"angle3\" srcEnable", "angleID=\"unknown\" srcEnable"),
        fixture().replace("name=\"Camera3\" angleID=\"angle3\"", "name=\"Camera3\" angleID=\"angle0\""),
    ] {
        let (imported, report) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
        assert!(!report.entries.is_empty());
        let main = imported.project.sequence(imported.sequences[0]).unwrap();
        assert_eq!(main.video_tracks[0].items.len(), 4);
        assert_eq!(main.audio_tracks[0].items[0].duration, seconds(10));
    }
}

#[test]
fn multicam_fractional_frame_times_are_exact() {
    let xml = fixture().replace("1/30s", "1001/30000s").replace("offset=\"3s\" start=\"3s\"", "offset=\"1001/30000s\" start=\"1001/30000s\"");
    let (imported, _) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
    let seq = imported.project.sequence(imported.sequences[0]).unwrap();
    assert!(seq.video_tracks.iter().flat_map(|t| &t.items).any(|c| c.start == Tick(TICKS_PER_SECOND / 30000 * 1001) && c.source_in == c.start));
}

#[test]
fn multicam_cycle_warns_instead_of_recursing() {
    let xml = fixture().replace(
        "<mc-angle name=\"Camera0\" angleID=\"angle0\">",
        "<mc-angle name=\"Camera0\" angleID=\"angle0\"><mc-clip ref=\"mc\" offset=\"0s\" duration=\"1s\"/>",
    );
    let (_, report) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
    assert!(report.entries.iter().any(|e| e.message.contains("cyclic")));
}

#[test]
fn multicam_separate_audio_selection_and_audio_only_angles() {
    let xml = r#"<fcpxml version="1.10"><resources>
    <format id="f" frameDuration="1/24s" width="320" height="180"/>
    <asset id="v" hasVideo="1" format="f" duration="10s" src="v.mov"/>
    <asset id="a" hasAudio="1" duration="10s" src="a.wav"/>
    <media id="mc"><multicam format="f">
      <mc-angle angleID="video"><asset-clip ref="v" offset="0s" duration="10s"/></mc-angle>
      <mc-angle angleID="audio"><asset-clip ref="a" offset="0s" duration="10s"/></mc-angle>
    </multicam></media></resources><project><sequence format="f"><spine>
    <mc-clip ref="mc" offset="0s" duration="10s"><mc-source angleID="video" srcEnable="video"/><mc-source angleID="audio" srcEnable="audio"/></mc-clip>
    </spine></sequence></project></fcpxml>"#;
    let (imp, report) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
    assert!(report.entries.is_empty(), "{report:?}");
    let main = imp.project.sequence(imp.sequences[0]).unwrap();
    assert_eq!(main.video_tracks[0].items[0].multicam.unwrap().angle, 0);
    assert_eq!(main.audio_tracks[0].items[0].multicam.unwrap().angle, 1);
    let source = imp.project.sequence(main.video_tracks[0].items[0].item).unwrap();
    let cameras = &source.multicam.as_ref().unwrap().cameras;
    assert!(cameras[1].video_track.is_none());
    assert_eq!(cameras[1].audio_tracks.len(), 1);
}

fn assert_damaged_multicam(xml: &str, expected: &[(i64, i64, u32)], warning: &str) {
    let result = std::panic::catch_unwind(|| import(xml.as_bytes(), Format::Fcpxml, None));
    let (imported, report) = result.expect("damaged input must not panic").expect("unaffected edits and master audio must still import");
    assert!(report.entries.iter().any(|e| e.message.contains(warning)), "expected actionable warning {warning}: {report:?}");
    let main = imported.project.sequence(imported.sequences[0]).unwrap();
    let mut actual: Vec<_> = main
        .video_tracks
        .iter()
        .flat_map(|t| &t.items)
        .map(|clip| {
            let selection = clip.multicam.expect("a surviving cut must retain its explicit camera selection");
            assert!(selection.enabled);
            (clip.start, clip.duration, clip.source_in, selection.angle)
        })
        .collect();
    actual.sort_by_key(|clip| clip.0);
    let expected: Vec<_> = expected.iter().map(|&(start, duration, angle)| (seconds(start), seconds(duration), seconds(start), angle)).collect();
    assert_eq!(actual, expected, "invalid clips must leave gaps, not move or remap surviving edits");
    let audio: Vec<_> = main.audio_tracks.iter().flat_map(|t| &t.items).collect();
    assert_eq!(audio.len(), 1, "the connected master must not be lost or duplicated");
    assert_eq!((audio[0].start, audio[0].duration, audio[0].source_in), (Tick::ZERO, seconds(10), Tick::ZERO));
    assert!(audio[0].multicam.is_none(), "the master mix must remain independent of angle selection");
    assert_eq!(imported.project.item(audio[0].item).unwrap().name, "Master Mix");
}

#[test]
fn multicam_missing_resource_reports_and_preserves_master() {
    assert_damaged_multicam(&fixture().replace("ref=\"mc\"", "ref=\"absent\""), &[], "media absent was not found");
}

#[test]
fn multicam_missing_angle_reports_and_preserves_unaffected_edits() {
    assert_damaged_multicam(
        &fixture().replace("angleID=\"angle3\" srcEnable", "srcEnable"),
        &[(0, 3, 0), (3, 2, 1), (5, 1, 1), (8, 2, 2)],
        "unknown video angleID (missing)",
    );
}

#[test]
fn multicam_invalid_duration_reports_and_preserves_unaffected_edits() {
    assert_damaged_multicam(
        &fixture().replace("duration=\"3s\"", "duration=\"1/0s\""),
        &[(3, 2, 1), (5, 1, 1), (6, 2, 3), (8, 2, 2)],
        "invalid or missing duration",
    );
}

#[test]
fn multicam_overflow_duration_reports_and_preserves_unaffected_edits() {
    assert_damaged_multicam(
        &fixture().replace("duration=\"3s\"", "duration=\"170141183460469231731687303715884105727/1s\""),
        &[(3, 2, 1), (5, 1, 1), (6, 2, 3), (8, 2, 2)],
        "invalid or missing duration",
    );
}
