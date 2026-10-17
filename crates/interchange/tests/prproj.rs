//! Synthetic Premiere-shaped data only; no Adobe project or media is embedded.
use filmcraft_interchange::{Format, import};
use filmcraft_time::Tick;

fn gzip(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let n = u16::try_from(bytes.len()).unwrap();
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255, 1];
    out.extend(n.to_le_bytes());
    out.extend((!n).to_le_bytes());
    out.extend(bytes);
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    out.extend((!crc).to_le_bytes());
    out.extend((bytes.len() as u32).to_le_bytes());
    out
}

fn project() -> String {
    r#"<PremiereData>
<RootProjectItem ObjectID="1"><ProjectItemContainer><Items><Item ObjectRef="2"/></Items></ProjectItemContainer></RootProjectItem>
<ClipProjectItem ObjectID="2"><ProjectItem><Name>Synthetic edit</Name></ProjectItem><MasterClip ObjectRef="3"/></ClipProjectItem>
<MasterClip ObjectID="3"><Clips><Clip ObjectRef="4"/></Clips></MasterClip>
<VideoClip ObjectID="4"><Clip><Source ObjectRef="5"/></Clip></VideoClip>
<VideoSequenceSource ObjectID="5"><SequenceSource><Sequence ObjectRef="6"/></SequenceSource></VideoSequenceSource>
<Sequence ObjectID="6"><TrackGroups><TrackGroup><Second ObjectRef="7"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="7"><TrackGroup><FrameRate>10584000000</FrameRate><Tracks><Track ObjectRef="8"/></Tracks></TrackGroup><FrameRect>0,0,1920,1080</FrameRect></VideoTrackGroup>
<VideoTrack ObjectID="8"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="9"/></TrackItems></ClipItems></ClipTrack></VideoTrack>
<VideoClipTrackItem ObjectID="9"><ClipTrackItem><SubClip ObjectRef="10"/><TrackItem><Start>254016000000</Start><End>762048000000</End></TrackItem></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="10"><Name>Shot</Name><MasterClip ObjectRef="11"/><Clip ObjectRef="12"/></SubClip>
<MasterClip ObjectID="11"><Name>Source</Name><Clips><Clip ObjectRef="12"/></Clips></MasterClip>
<VideoClip ObjectID="12"><Clip><Source ObjectRef="13"/><InPoint>127008000000</InPoint><PlaybackSpeed>2</PlaybackSpeed><PlayBackwards>true</PlayBackwards></Clip></VideoClip>
<VideoMediaSource ObjectID="13"><MediaSource><Media ObjectRef="14"/></MediaSource></VideoMediaSource>
<Media ObjectID="14"><FilePath>/synthetic/shot.mov</FilePath><VideoStream ObjectRef="15"/></Media>
<VideoStream ObjectID="15"><FrameRate>10584000000</FrameRate><FrameRect>0,0,1920,1080</FrameRect><Duration>2540160000000</Duration></VideoStream>
</PremiereData>"#.into()
}

#[test]
fn prproj_import_preserves_sequence_clip_timing_and_retime() {
    let (imported, _) = import(&gzip(&project()), Format::Prproj, None).unwrap();
    let sequence = imported.project.sequence(imported.sequences[0]).unwrap();
    assert_eq!((sequence.settings.width, sequence.settings.height), (1920, 1080));
    assert_eq!(sequence.settings.frame_rate.as_f64(), 24.0);
    let clip = &sequence.video_tracks[0].items[0];
    assert_eq!(clip.start, Tick(254_016_000_000));
    assert_eq!(clip.duration, Tick(508_032_000_000));
    assert_eq!(clip.source_in, Tick(127_008_000_000));
    assert_eq!(clip.speed, 2.0);
    assert!(clip.reverse);
}

#[test]
fn prproj_import_uses_sequence_name_when_project_item_name_is_stale() {
    let text = project().replace("</Sequence>", "<Name>test</Name></Sequence>");
    let (imported, _) = import(&gzip(&text), Format::Prproj, None).unwrap();
    let id = imported.sequences[0];
    assert_eq!(imported.project.item(id).unwrap().name, "test");
}

#[test]
fn prproj_imports_time_remapping_keyframes_as_speed_segments() {
    let text = project()
        .replace("<PlayBackwards>true</PlayBackwards>", "<PlayBackwards>false</PlayBackwards>")
        .replace("<InPoint>127008000000</InPoint>", "<TimeRemapping ObjectRef=\"21\"/><InPoint>127008000000</InPoint>")
        .replace(
            "</PremiereData>",
            "<TimeRemapping ObjectID=\"21\"><Keyframes ObjectRef=\"22\"/></TimeRemapping>\
             <TimeComponentParam ObjectID=\"22\"><Name>Speed</Name><Keyframes>0,0,6,0,0,0,0,0;\
             254016000000,2,7,0,0,0,0,0;508032000000,5,8,0,0,0,0,0;</Keyframes></TimeComponentParam></PremiereData>",
        );
    let (imported, report) = import(&gzip(&text), Format::Prproj, None).unwrap();
    let sequence = imported.project.sequence(imported.sequences[0]).unwrap();
    let clip = &sequence.video_tracks[0].items[0];
    let speed = clip.effect("time_remap").unwrap().param("speed").unwrap();
    assert_eq!(speed.keyframes.len(), 3);
    assert!((speed.keyframes[0].value.as_f64().unwrap() - 100.0).abs() < 0.001);
    assert!((speed.keyframes[1].value.as_f64().unwrap() - 150.0).abs() < 0.001);
    assert_eq!(clip.source_time_at(clip.start + Tick(127_008_000_000)), clip.source_in + Tick(254_016_000_000));
    assert!((clip.source_time_at(clip.start + Tick(381_024_000_000)) - (clip.source_in + Tick(889_056_000_000))).abs() <= Tick(4));
    assert!(report.mentions("Shot\" imported 3 Time Remapping keyframes"));
}

#[test]
fn malformed_time_remapping_keeps_clip_and_reports_error() {
    let text = project()
        .replace("<PlayBackwards>true</PlayBackwards>", "<PlayBackwards>false</PlayBackwards>")
        .replace("<InPoint>127008000000</InPoint>", "<TimeRemapping ObjectRef=\"21\"/><InPoint>127008000000</InPoint>")
        .replace(
            "</PremiereData>",
            "<TimeRemapping ObjectID=\"21\"><Keyframes ObjectRef=\"22\"/></TimeRemapping>\
             <TimeComponentParam ObjectID=\"22\"><Keyframes>0,0,6;1,NaN,7;</Keyframes></TimeComponentParam></PremiereData>",
        );
    let result = std::panic::catch_unwind(|| import(&gzip(&text), Format::Prproj, None));
    let (imported, report) = result.unwrap().unwrap();
    let sequence = imported.project.sequence(imported.sequences[0]).unwrap();
    assert_eq!(sequence.video_tracks[0].items.len(), 1);
    assert!(report.mentions("Time Remapping was not imported: invalid source position"));
}

#[test]
fn prproj_truncation_and_invalid_documents_return_errors() {
    let bytes = gzip(&project());
    for n in 0..bytes.len() {
        assert!(import(&bytes[..n], Format::Prproj, None).is_err(), "accepted truncation {n}");
    }
    for text in ["not XML", "<Other/>", "<PremiereData/>"] {
        assert!(import(&gzip(text), Format::Prproj, None).is_err());
    }
}

#[test]
fn prproj_cyclic_bins_are_bounded_and_reported() {
    let text = project().replace("<Items><Item ObjectRef=\"2\"/></Items>", "<Items><Item ObjectRef=\"2\"/><Item ObjectRef=\"20\"/></Items>")
        .replace("</PremiereData>", "<BinProjectItem ObjectID=\"20\"><ProjectItem><Name>Cycle</Name></ProjectItem><ProjectItemContainer><Items><Item ObjectRef=\"20\"/><Item ObjectRef=\"20\"/></Items></ProjectItemContainer></BinProjectItem></PremiereData>");
    let (_, report) = import(&gzip(&text), Format::Prproj, None).unwrap();
    assert!(report.entries.iter().any(|e| e.message.contains("cyclic bin")));
}
