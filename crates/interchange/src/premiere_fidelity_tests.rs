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
<AudioFilterComponent ObjectID="volume"><AudioComponent><Component><Params><Param Index="1" ObjectRef="level"/></Params></Component></AudioComponent><FilterMatchName>Internal Volume Stereo</FilterMatchName></AudioFilterComponent>
<AudioComponentParam ObjectID="level"><Name>Level</Name><CurrentValue>0.1778279410038923</CurrentValue><Keyframes>{t},0.1778279410038923,0,0;{two},0,0,0;{start},0.1778279410038923,0,0;</Keyframes></AudioComponentParam>
<AudioFilterComponent ObjectID="channels"><AudioComponent><Component><Params><Param Index="1" ObjectRef="left"/><Param Index="2" ObjectRef="right"/></Params></Component></AudioComponent><FilterMatchName>Internal Channel Volume Stereo</FilterMatchName></AudioFilterComponent>
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

#[test]
fn intrinsic_audio_is_matched_by_index_not_by_localised_name() {
    // Hand-written: a Spanish Premiere names the parameters Nivel / Izquierda / Derecha, and a
    // mono clip's volume is `Internal Volume Mono`; the `Param@Index` layout is the same.
    let english = fixture();
    let localised = english
        .replace("<Name>Level</Name>", "<Name>Nivel</Name>")
        .replace("<Name>Left</Name>", "<Name>Izquierda</Name>")
        .replace("<Name>Right</Name>", "<Name>Derecha</Name>")
        .replace("Internal Volume Stereo", "Internal Volume Mono");
    let params = |xml: &str| {
        let (p, _, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let c = p.project.sequence(p.sequences[0]).unwrap().audio_tracks[0].items[0].clone();
        let level = c.effect("volume").unwrap().param("level").unwrap().clone();
        let left = c.effect("channel_volume").unwrap().param("left").unwrap().value.as_f64().unwrap();
        (level.keyframes.len(), left)
    };
    let (keys, left) = params(&localised);
    assert_eq!((keys, left), params(&english));
    assert_eq!(keys, 3);
    assert!((left + 6.0).abs() < 1e-6);
}

fn with_fader(mute: &str) -> String {
    fixture().replace(
        "<ClipItems><TrackItems><TrackItem ObjectRef=\"a\"/></TrackItems></ClipItems></ClipTrack>",
        "<ClipItems><TrackItems><TrackItem ObjectRef=\"a\"/></TrackItems></ClipItems></ClipTrack><AudioTrack><ComponentOwner><Components ObjectRef=\"tchain\"/></ComponentOwner></AudioTrack>",
    )
    .replace(
        "</PremiereData>",
        &format!(
            r#"<AudioComponentChain ObjectID="tchain"><ComponentChain><Components><Component Index="0" ObjectRef="fader"/></Components></ComponentChain></AudioComponentChain>
<AudioFader ObjectID="fader"><AudioComponent><Component><Params><Param Index="0" ObjectRef="fvol"/><Param Index="1" ObjectRef="fmute"/></Params></Component></AudioComponent></AudioFader>
<AudioComponentParam ObjectID="fvol"><Name>Volumen</Name></AudioComponentParam>
<AudioComponentParam ObjectID="fmute"><Name>Silenciar</Name>{mute}</AudioComponentParam></PremiereData>"#
        ),
    )
}

#[test]
fn a_track_muted_on_its_fader_imports_muted() {
    // Hand-written after the shape Premiere writes: the mute is the fader's second parameter,
    // whose name is localised; `IsMuted` stays false.
    let muted = |xml: &str| {
        let (p, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        (p.project.sequence(p.sequences[0]).unwrap().audio_tracks[0].muted, report)
    };
    assert!(muted(&with_fader("<StartKeyframe>-91445760000000000,true,0,0,0,0,0,0</StartKeyframe><CurrentValue>true</CurrentValue>")).0);
    assert!(muted(&with_fader("<StartKeyframe>-91445760000000000,true,0,0,0,0,0,0</StartKeyframe>")).0);
    assert!(!muted(&with_fader("")).0);
    assert!(!muted(&with_fader("<CurrentValue>false</CurrentValue>")).0);
    assert!(!muted(&fixture()).0);
    let (on, report) = muted(&with_fader("<CurrentValue>true</CurrentValue><Keyframes>0,true;254016000000,false;</Keyframes>"));
    assert!(on);
    assert!(report.entries.iter().any(|e| e.message.contains("track mute automation")));
}

#[test]
fn the_sequence_frame_is_the_video_frame_rect_not_the_preview_size() {
    let rect = |r: &str| fixture().replace("<FrameRate>10584000000</FrameRate>", &format!("<FrameRate>10584000000</FrameRate>{r}"));
    let size = |xml: &str| {
        let xml = xml.replace(
            "<Name>Example</Name>",
            "<Name>Example</Name><Node><Properties><MZ.Sequence.PreviewFrameSizeWidth>1920</MZ.Sequence.PreviewFrameSizeWidth><MZ.Sequence.PreviewFrameSizeHeight>1080</MZ.Sequence.PreviewFrameSizeHeight></Properties></Node>",
        );
        import(xml.as_bytes(), &ImportOptions::default(), None).map(|(p, _, _)| {
            let s = p.project.sequence(p.sequences[0]).unwrap();
            (s.settings.width, s.settings.height)
        })
    };
    // FrameRect is a sibling of the inner TrackGroup in the files Premiere writes.
    let outside = |r: &str| rect("").replace("</TrackGroup></VideoTrackGroup>", &format!("</TrackGroup>{r}</VideoTrackGroup>"));
    assert_eq!(size(&outside("<FrameRect>0,0,2560,1440</FrameRect>")).unwrap(), (2560, 1440));
    assert_eq!(size(&fixture()).unwrap(), (1920, 1080));
    for bad in ["0,0,2560", "10,0,0,1440", "0,0,x,1440", "0,0,0,0", "0,0,99999999,1"] {
        let result = std::panic::catch_unwind(|| size(&outside(&format!("<FrameRect>{bad}</FrameRect>"))));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err(), "{bad}");
    }
}

#[test]
fn a_volume_component_named_only_by_match_name_is_still_volume() {
    // The caller accepts `MatchName` before `FilterMatchName`; the reader must agree, or the
    // level would land on Channel Volume's Left.
    let xml = fixture().replace(
        "<FilterMatchName>Internal Volume Stereo</FilterMatchName></AudioFilterComponent>",
        "<MatchName>Internal Volume Stereo</MatchName></AudioFilterComponent>",
    );
    let (p, _, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    let c = &p.project.sequence(p.sequences[0]).unwrap().audio_tracks[0].items[0];
    assert_eq!(c.effect("volume").unwrap().param("level").unwrap().keyframes.len(), 3);
    assert!((c.effect("channel_volume").unwrap().param("left").unwrap().value.as_f64().unwrap() + 6.0).abs() < 1e-6);
}
