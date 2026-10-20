//! Hand-written synthetic fixtures. MIT OR Apache-2.0; no private project data.
use super::*;

fn fixture(component: &str, extra: &str) -> String {
    format!(
        r#"<PremiereData>
<Sequence ObjectUID="s"><Name>Example</Name><TrackGroups><TrackGroup><Second ObjectRef="g"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="g"><TrackGroup><FrameRate>10584000000</FrameRate><Tracks><Track ObjectURef="t"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="t"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="a"/><TrackItem ObjectRef="b"/></TrackItems></ClipItems><TransitionItems><TrackItems><TrackItem ObjectRef="d"/></TrackItems></TransitionItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="a"><ClipTrackItem><ComponentOwner><Components ObjectRef="chain"/></ComponentOwner><TrackItem><Start>0</Start><End>{two}</End></TrackItem><SubClip ObjectRef="sa"/><TailTransition ObjectRef="d"/></ClipTrackItem></VideoClipTrackItem>
<VideoClipTrackItem ObjectID="b"><ClipTrackItem><TrackItem><Start>{two}</Start><End>{four}</End></TrackItem><SubClip ObjectRef="sb"/><HeadTransition ObjectRef="d"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sa"><Name>first.mov</Name><Clip ObjectRef="ca"/></SubClip><SubClip ObjectID="sb"><Name>second.mov</Name><Clip ObjectRef="cb"/></SubClip>
<VideoClip ObjectID="ca"><InPoint>0</InPoint><OutPoint>{two}</OutPoint></VideoClip><VideoClip ObjectID="cb"><InPoint>0</InPoint><OutPoint>{two}</OutPoint></VideoClip>
<Media><Title>first.mov</Title><FilePath>first.mov</FilePath></Media><Media><Title>second.mov</Title><FilePath>second.mov</FilePath></Media>
<VideoTransitionTrackItem ObjectID="d"><TransitionTrackItem><TrackItem><Start>{one}</Start><End>{three}</End></TrackItem><DisplayName>Dip to Black</DisplayName></TransitionTrackItem></VideoTransitionTrackItem>
<VideoComponentChain ObjectID="chain"><Components><Component ObjectRef="fx"/></Components></VideoComponentChain>
{component}{extra}</PremiereData>"#,
        one = filmcraft_time::TICKS_PER_SECOND,
        two = 2 * filmcraft_time::TICKS_PER_SECOND,
        three = 3 * filmcraft_time::TICKS_PER_SECOND,
        four = 4 * filmcraft_time::TICKS_PER_SECOND
    )
}

#[test]
fn transitions_are_not_media_and_shared_references_make_one_real_transition() {
    let (p, _, _) = import(fixture("", "").as_bytes(), &ImportOptions::default(), None).unwrap();
    let t = &p.project.sequence(p.sequences[0]).unwrap().video_tracks[0];
    assert_eq!(t.items.len(), 2);
    assert_eq!(t.transitions.len(), 1);
    let d = &t.transitions[0];
    assert_eq!(d.effect.effect, "dip_to_black");
    assert_eq!(d.start, Tick(filmcraft_time::TICKS_PER_SECOND));
    assert_eq!(d.duration, Tick(2 * filmcraft_time::TICKS_PER_SECOND));
    assert_eq!(d.from, Some(t.items[0].id));
    assert_eq!(d.to, Some(t.items[1].id));
}

fn lumetri(saturation: &str) -> String {
    fixture(
        r#"<VideoFilterComponent ObjectID="fx"><MatchName>AE.ADBE Lumetri</MatchName><Component><DisplayName>Localized Color</DisplayName><Params><Param ObjectRef="sat"/><Param ObjectRef="creative"/><Param ObjectRef="exposure"/></Params></Component></VideoFilterComponent>"#,
        &format!(
            r#"<VideoComponentParam ObjectID="sat"><ParameterID>20</ParameterID><Name>Duplicated label</Name><StartKeyframe>-100,{saturation},0,0</StartKeyframe></VideoComponentParam>
<VideoComponentParam ObjectID="creative"><ParameterID>31</ParameterID><Name>Duplicated label</Name><StartKeyframe>-100,115,0,0</StartKeyframe></VideoComponentParam>
<VideoComponentParam ObjectID="exposure"><ParameterID>11</ParameterID><StartKeyframe>-100,0.7,0,0</StartKeyframe></VideoComponentParam>"#
        ),
    )
}

#[test]
fn lumetri_uses_stable_ids_without_requiring_optional_control_type() {
    let (p, _, _) = import(lumetri("82").as_bytes(), &ImportOptions::default(), None).unwrap();
    let c = &p.project.sequence(p.sequences[0]).unwrap().video_tracks[0].items[0];
    let e = c.effect("lumetri").expect("color effect imported");
    assert_eq!(e.param("saturation").unwrap().value, ParamValue::Float(82.0));
    assert_eq!(e.param("creative_sat").unwrap().value, ParamValue::Float(115.0));
    assert_eq!(e.param("exposure").unwrap().value, ParamValue::Float(0.7));
}

#[test]
fn shape_components_create_native_graphics_instead_of_missing_files() {
    let xml = fixture(
        r#"<VideoFilterComponent ObjectID="fx"><MatchName>AE.ADBE Shape</MatchName><Component><DisplayName>Localized Shape</DisplayName></Component></VideoFilterComponent>"#,
        "",
    );
    let (p, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    let c = &p.project.sequence(p.sequences[0]).unwrap().video_tracks[0].items[0];
    assert!(matches!(p.project.item(c.item).unwrap().kind, ItemKind::Graphic { .. }));
    assert!(!report.entries.iter().any(|e| e.message.contains("media unresolved")));
}

#[test]
fn hostile_colors_and_keyframe_lists_return_errors_without_panicking() {
    for raw in ["NaN", "inf", "-inf", "1e999", "garbage"] {
        let xml = lumetri(raw);
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    for keys in ["-1,1;".to_string(), "9223372036854775808,1;".into(), "1,NaN;".into(), "1,1;1,2;".into(), "1,1;".repeat(10001)] {
        let xml = lumetri("80").replace("<Name>Duplicated label</Name>", &format!("<Keyframes>{keys}</Keyframes>"));
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}

#[test]
fn leading_transitions_are_cropped_at_zero_and_color_keys_use_source_ticks() {
    let xml = lumetri("80")
        .replace("<Start>254016000000</Start><End>762048000000</End>", "<Start>-254016000000</Start><End>762048000000</End>")
        .replace("<ParameterID>20</ParameterID>", "<ParameterID>20</ParameterID><Keyframes>508032000000,80;254016000000,70;</Keyframes>");
    let (p, _, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    let t = &p.project.sequence(p.sequences[0]).unwrap().video_tracks[0];
    assert_eq!(t.transitions[0].start, Tick::ZERO);
    assert_eq!(t.transitions[0].duration, Tick(3 * filmcraft_time::TICKS_PER_SECOND));
    let keys = &t.items[0].effect("lumetri").unwrap().param("saturation").unwrap().keyframes;
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].time, Tick(filmcraft_time::TICKS_PER_SECOND));
}

#[test]
fn old_picture_provenance_defaults_remain_readable() {
    let (_, doc, _) = import(lumetri("80").as_bytes(), &ImportOptions::default(), None).unwrap();
    let mut json = serde_json::to_value(doc).unwrap();
    for seq in json["sequences"].as_array_mut().unwrap() {
        for track in seq["tracks"].as_array_mut().unwrap() {
            for item in track["items"].as_array_mut().unwrap() {
                item.as_object_mut().unwrap().remove("video_effects");
                item.as_object_mut().unwrap().remove("graphic");
                for tr in item["transitions"].as_array_mut().unwrap() {
                    for key in ["object_id", "start", "end"] {
                        tr.as_object_mut().unwrap().remove(key);
                    }
                }
            }
        }
    }
    let old: Document = serde_json::from_value(json).unwrap();
    assert!(old.sequences[0].tracks[0].items[0].video_effects.is_empty());
    assert!(!old.sequences[0].tracks[0].items[0].graphic);
}
