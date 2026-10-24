//! Original synthetic format fixtures, not private project data. MIT OR Apache-2.0.
use super::*;

fn fixture() -> String {
    let t = filmcraft_time::TICKS_PER_SECOND;
    format!(
        r#"<PremiereData>
<Sequence ObjectUID="s"><Name>Example</Name><TrackGroups><TrackGroup><Second ObjectRef="vg"/></TrackGroup><TrackGroup><Second ObjectRef="ag"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="vg"><TrackGroup><FrameRate>10584000000</FrameRate><Tracks><Track ObjectURef="vt"/></Tracks></TrackGroup></VideoTrackGroup>
<AudioTrackGroup ObjectID="ag"><TrackGroup><FrameRate>7938000</FrameRate><Tracks><Track ObjectURef="at"/></Tracks></TrackGroup></AudioTrackGroup>
<VideoClipTrack ObjectUID="vt"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="v"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<AudioClipTrack ObjectUID="at"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="a"/></TrackItems></ClipItems></ClipTrack></AudioClipTrack>
<VideoClipTrackItem ObjectID="v"><ClipTrackItem><TrackItem><Start>{start}</Start><End>{end}</End></TrackItem><SubClip ObjectRef="sv"/></ClipTrackItem></VideoClipTrackItem>
<AudioClipTrackItem ObjectID="a"><ClipTrackItem><ComponentOwner><Components ObjectRef="chain"/></ComponentOwner><TrackItem><Start>{start}</Start><End>{end}</End></TrackItem><SubClip ObjectRef="sa"/></ClipTrackItem></AudioClipTrackItem>
<SubClip ObjectID="sv"><Name>example.mov</Name><Clip ObjectRef="vc"/></SubClip><SubClip ObjectID="sa"><Name>example.mov</Name><Clip ObjectRef="ac"/></SubClip>
<VideoClip ObjectID="vc"><InPoint>{t}</InPoint><OutPoint>{start}</OutPoint><PlaybackSpeed>1</PlaybackSpeed></VideoClip>
<AudioClip ObjectID="ac"><InPoint>{t}</InPoint><OutPoint>{start}</OutPoint><PlaybackSpeed>1</PlaybackSpeed></AudioClip>
<Media><Title>example.mov</Title><FilePath>example.mov</FilePath></Media>
<Link ObjectID="link"><TrackItemGroup><TrackItems><TrackItem ObjectRef="v"/><TrackItem ObjectRef="a"/></TrackItems></TrackItemGroup></Link>
<AudioComponentChain ObjectID="chain"><Components><Component ObjectRef="volume"/><Component ObjectRef="channels"/></Components></AudioComponentChain>
<AudioFilterComponent ObjectID="volume"><AudioComponent><Component><Params><Param ObjectRef="level"/></Params></Component></AudioComponent><FilterMatchName>Internal Volume Stereo</FilterMatchName></AudioFilterComponent>
<AudioComponentParam ObjectID="level"><Name>Level</Name><CurrentValue>0.1778279410038923</CurrentValue><Keyframes>{t},0.1778279410038923,0,0;{two},0,0,0;{start},0.1778279410038923,0,0;</Keyframes></AudioComponentParam>
<AudioFilterComponent ObjectID="channels"><AudioComponent><Component><Params><Param ObjectRef="left"/><Param ObjectRef="right"/></Params></Component></AudioComponent><FilterMatchName>Internal Channel Volume Stereo</FilterMatchName></AudioFilterComponent>
<AudioComponentParam ObjectID="left"><Name>Left</Name><CurrentValue>0.08912509381337455</CurrentValue></AudioComponentParam>
<AudioComponentParam ObjectID="right"><Name>Right</Name><CurrentValue>0.1778279410038923</CurrentValue></AudioComponentParam>
</PremiereData>"#,
        start = 3 * t,
        end = 5 * t,
        t = t,
        two = 2 * t
    )
}

#[test]
fn sequence_audio_rate_comes_from_audio_group_not_video_or_default() {
    let (p, _, _) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let s = p.project.sequence(p.sequences[0]).unwrap();
    assert_eq!(s.settings.sample_rate, 32000);
    assert_eq!(s.settings.frame_rate, FrameRate::FPS_24);
    let absent = fixture().replace("<FrameRate>7938000</FrameRate>", "");
    let (p, _, _) = import(absent.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert_eq!(p.project.sequence(p.sequences[0]).unwrap().settings.sample_rate, 48000);
}

#[test]
fn explicit_links_survive_without_linking_unrelated_aligned_items() {
    let (p, _, _) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let s = p.project.sequence(p.sequences[0]).unwrap();
    let v = &s.video_tracks[0].items[0];
    let a = &s.audio_tracks[0].items[0];
    assert!(v.link.is_some());
    assert_eq!(v.link, a.link);
    assert_eq!(
        (v.start, v.source_in, v.duration),
        (Tick(3 * filmcraft_time::TICKS_PER_SECOND), Tick(filmcraft_time::TICKS_PER_SECOND), Tick(2 * filmcraft_time::TICKS_PER_SECOND))
    );
    let xml = fixture().replace(
        r#"<Link ObjectID="link"><TrackItemGroup><TrackItems><TrackItem ObjectRef="v"/><TrackItem ObjectRef="a"/></TrackItems></TrackItemGroup></Link>"#,
        "",
    );
    let (p, _, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert!(p.project.sequence(p.sequences[0]).unwrap().all_tracks().flat_map(|t| &t.items).all(|c| c.link.is_none()));
}

#[test]
fn intrinsic_audio_keeps_source_time_keys_silence_and_channel_levels() {
    let (p, _, report) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let c = &p.project.sequence(p.sequences[0]).unwrap().audio_tracks[0].items[0];
    let v = c.effect("volume").unwrap();
    let param = v.param("level").unwrap();
    assert_eq!(param.keyframes.len(), 3);
    assert_eq!(param.keyframes[0].time, Tick(filmcraft_time::TICKS_PER_SECOND));
    assert_eq!(param.keyframes[1].value, ParamValue::Float(-287.5));
    assert!(param.keyframes[0].value.as_f64().unwrap().abs() < 1e-6);
    let ch = c.effect("channel_volume").unwrap();
    assert!((ch.param("left").unwrap().value.as_f64().unwrap() + 6.0).abs() < 1e-6);
    assert!(!report.entries.iter().any(|e| e.message.contains("Internal Volume Stereo")));
}

#[test]
fn hostile_audio_values_and_link_graphs_fail_without_panicking() {
    for rate in ["0", "-1", "NaN", "1.5", "9223372036854775808", "1"] {
        let xml = fixture().replace("<FrameRate>7938000</FrameRate>", &format!("<FrameRate>{rate}</FrameRate>"));
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err(), "{rate}");
    }
    for bad in ["NaN", "inf", "-1", "2", "1e999"] {
        let xml = fixture().replace("<CurrentValue>0.1778279410038923</CurrentValue>", &format!("<CurrentValue>{bad}</CurrentValue>"));
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err(), "{bad}");
    }
    let xml = fixture().replace(
        "</PremiereData>",
        r#"<Link ObjectID="another-link"><TrackItemGroup><TrackItems><TrackItem ObjectRef="a"/></TrackItems></TrackItemGroup></Link></PremiereData>"#,
    );
    assert!(import(xml.as_bytes(), &ImportOptions::default(), None).is_err());
    for key in ["-1,0;", "9223372036854775808,0;", "1,NaN;", "1,0;1,0;", "1,0;".repeat(10001).as_str()] {
        let xml = fixture();
        let a = xml.find("<Keyframes>").unwrap() + "<Keyframes>".len();
        let b = xml.find("</Keyframes>").unwrap();
        let xml = format!("{}{}{}", &xml[..a], key, &xml[b..]);
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}

#[test]
fn old_provenance_defaults_and_new_audio_controls_round_trip() {
    let (_, tree, _) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let mut json = serde_json::to_value(&tree).unwrap();
    let read: Document = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(read.sequences[0].sample_rate, Some(32000));
    assert!(read.sequences[0].tracks[1].items[0].audio["volume.level"].is_animated());
    for seq in json["sequences"].as_array_mut().unwrap() {
        seq.as_object_mut().unwrap().remove("sample_rate");
        for track in seq["tracks"].as_array_mut().unwrap() {
            for item in track["items"].as_array_mut().unwrap() {
                item.as_object_mut().unwrap().remove("link");
                item.as_object_mut().unwrap().remove("audio");
            }
        }
    }
    let old: Document = serde_json::from_value(json).unwrap();
    assert_eq!(old.sequences[0].sample_rate, None);
    assert!(old.sequences[0].tracks[1].items[0].audio.is_empty());
}

#[test]
fn oversized_link_identifiers_are_rejected_before_per_clip_copying() {
    let xml = fixture().replace("ObjectID=\"link\"", &format!("ObjectID=\"{}\"", "x".repeat(4097)));
    let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
    assert!(result.is_ok());
    match result.unwrap() {
        Err(error) => assert!(error.to_string().contains("link group identifier is too long")),
        Ok(_) => panic!("an oversized shared link identifier was accepted"),
    }
}
