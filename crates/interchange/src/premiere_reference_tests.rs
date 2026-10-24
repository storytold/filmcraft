//! Hand-written synthetic object graphs; no XML, names, IDs or paths copied from private projects.
//! MIT OR Apache-2.0.
use super::*;

fn media_fixture() -> &'static str {
    r#"<PremiereData>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="group"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="one"/><TrackItem ObjectRef="two"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="one"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="sub-one"/></ClipTrackItem></VideoClipTrackItem>
<VideoClipTrackItem ObjectID="two"><ClipTrackItem><TrackItem><Start>254016000000</Start><End>508032000000</End></TrackItem><SubClip ObjectRef="sub-two"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub-one"><Name>FIRST.MOV</Name><Clip ObjectRef="clip-one"/></SubClip>
<SubClip ObjectID="sub-two"><Name>SECOND.MOV</Name><Clip ObjectRef="clip-two"/></SubClip>
<VideoClip ObjectID="clip-one"><Clip><InPoint>0</InPoint><OutPoint>254016000000</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="source-one"/></Clip></VideoClip>
<VideoClip ObjectID="clip-two"><Clip><InPoint>0</InPoint><OutPoint>254016000000</OutPoint><PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef="source-two"/></Clip></VideoClip>
<VideoMediaSource ObjectID="source-one"><MediaSource><Media ObjectURef="media-one"/></MediaSource></VideoMediaSource>
<VideoMediaSource ObjectID="source-two"><MediaSource><Media ObjectURef="media-two"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media-one"><Title>first.mov</Title><FilePath>first.mov</FilePath></Media>
<Media ObjectUID="media-two"><Title>second.mov</Title><ActualMediaFilePath>second.mov</ActualMediaFilePath></Media>
</PremiereData>"#
}

fn nested_fixture() -> String {
    media_fixture()
        .replace("<Name>FIRST.MOV</Name>", "<Name>Renamed timeline nest</Name>")
        .replace(
            "<VideoMediaSource ObjectID=\"source-one\"><MediaSource><Media ObjectURef=\"media-one\"/></MediaSource></VideoMediaSource>",
            "<VideoSequenceSource ObjectID=\"source-one\"><SequenceSource><Sequence ObjectURef=\"child\"/></SequenceSource></VideoSequenceSource>",
        )
        .replace("</PremiereData>", "<Sequence ObjectUID=\"child\"><Name>Child</Name></Sequence></PremiereData>")
}

fn localized_motion_fixture() -> String {
    media_fixture()
        .replace("FIRST.MOV", "first.mov")
        .replace("SECOND.MOV", "second.mov")
        .replace(
            "<SubClip ObjectRef=\"sub-one\"/>",
            "<SubClip ObjectRef=\"sub-one\"/><ComponentOwner><Components ObjectRef=\"chain\"/></ComponentOwner>",
        )
        .replace(
            "</PremiereData>",
            r#"<VideoComponentChain ObjectID="chain"><Components><Component ObjectRef="motion"/></Components></VideoComponentChain>
<VideoFilterComponent ObjectID="motion"><Component><DisplayName>Movimiento</DisplayName><Params><Param ObjectRef="position"/><Param ObjectRef="scale"/><Param ObjectRef="rotation"/></Params></Component><MatchName>AE.ADBE Motion</MatchName></VideoFilterComponent>
<PointComponentParam ObjectID="position"><ParameterID>1</ParameterID><Name>Posición</Name><IsTimeVarying>false</IsTimeVarying><StartKeyframe>-254016000000,0.25:0.75,0,0</StartKeyframe></PointComponentParam>
<VideoComponentParam ObjectID="scale"><ParameterID>2</ParameterID><Name>Escala</Name><CurrentValue>75</CurrentValue></VideoComponentParam>
<VideoComponentParam ObjectID="rotation"><ParameterID>5</ParameterID><Name>Rotación</Name><IsTimeVarying>false</IsTimeVarying><StartKeyframe>-254016000000,12.5,0,0</StartKeyframe></VideoComponentParam>
</PremiereData>"#,
        )
}

#[test]
fn media_references_resolve_two_case_only_display_name_mismatches() {
    let (fragment, tree, report) = import(media_fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let clips = &tree.sequences[0].tracks[0].items;
    assert_eq!(clips[0].media_path.as_deref(), Some("first.mov"));
    assert_eq!(clips[1].media_path.as_deref(), Some("second.mov"));
    let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
    assert_ne!(seq.video_tracks[0].items[0].item, seq.video_tracks[0].items[1].item);
    assert!(!report.mentions("fallback"), "{report:?}");
}

#[test]
fn renamed_nested_clip_resolves_sequence_source_uid() {
    let (fragment, _, report) = import(nested_fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let parent = fragment.project.sequence(fragment.sequences[0]).unwrap();
    assert_eq!(parent.video_tracks[0].items[0].item, fragment.sequences[1]);
    assert_eq!(parent.video_tracks[0].items[0].name, "Renamed timeline nest");
    assert!(!report.mentions("title fallback"), "{report:?}");
}

#[test]
fn duplicate_sequence_display_names_keep_distinct_uid_targets() {
    let xml = nested_fixture()
        .replace("ObjectURef=\"child\"", "ObjectURef=\"other-child\"")
        .replace("</PremiereData>", "<Sequence ObjectUID=\"other-child\"><Name>Child</Name></Sequence></PremiereData>");
    let (fragment, _, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert_eq!(fragment.sequences.len(), 3);
    let parent = fragment.project.sequence(fragment.sequences[0]).unwrap();
    assert_eq!(parent.video_tracks[0].items[0].item, fragment.sequences[2]);
    assert_ne!(fragment.sequences[1], fragment.sequences[2]);
}

#[test]
fn localized_motion_uses_match_name_parameter_ids_and_static_values() {
    let (fragment, tree, report) = import(localized_motion_fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
    let motion = &tree.sequences[0].tracks[0].items[0].motion;
    assert_eq!(motion.get("Position").map(String::as_str), Some("0.25:0.75"));
    assert_eq!(motion.get("Scale").map(String::as_str), Some("75"));
    assert_eq!(motion.get("Rotation").map(String::as_str), Some("12.5"));
    let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
    let effect = clip.effects.iter().find(|e| e.effect == "motion").unwrap();
    assert_eq!(effect.params["scale"].value, ParamValue::Float(75.0));
    assert_eq!(effect.params["rotation"].value, ParamValue::Float(12.5));
    assert_eq!(effect.params["position"].value, ParamValue::Vec2(Vec2::new(480.0, 810.0)));
    assert!(!report.mentions("not rendered"), "{report:?}");
}

#[test]
fn audio_media_and_sequence_sources_follow_the_same_reference_chain() {
    for xml in [media_fixture().to_string(), nested_fixture()] {
        // Audio groups store ticks per sample, not the video-frame duration.
        let xml = xml.replace("Video", "Audio").replace("<FrameRate>8467200000</FrameRate>", "<FrameRate>5292000</FrameRate>");
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), Some(FrameRate::FPS_30)).unwrap();
        let first = &tree.sequences[0].tracks[0].items[0];
        if fragment.sequences.len() == 1 {
            assert_eq!(first.media_path.as_deref(), Some("first.mov"));
        } else {
            let parent = fragment.project.sequence(fragment.sequences[0]).unwrap();
            assert_eq!(parent.audio_tracks[0].items[0].item, fragment.sequences[1]);
        }
        assert!(!report.mentions("title fallback"), "{report:?}");
    }
}

#[test]
fn resolved_media_ignores_duplicate_titles_and_sequence_name_collisions() {
    let xml = media_fixture()
        .replace("<Title>first.mov</Title>", "<Title>FIRST.MOV</Title>")
        .replace("<Title>second.mov</Title>", "<Title>FIRST.MOV</Title>")
        .replace("<Name>Parent</Name>", "<Name>FIRST.MOV</Name>");
    let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    let items = &tree.sequences[0].tracks[0].items;
    assert_eq!(items[0].media_path.as_deref(), Some("first.mov"));
    assert_eq!(items[1].media_path.as_deref(), Some("second.mov"));
    assert!(items.iter().all(|i| i.nested_sequence.is_none()));
    assert!(!report.has_warnings(), "{report:?}");
}

#[test]
fn unresolved_references_use_only_warned_unambiguous_title_fallbacks() {
    let xml = media_fixture().replace("FIRST.MOV", "first.mov").replace("ObjectRef=\"source-one\"", "ObjectRef=\"missing\"");
    let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert_eq!(tree.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("first.mov"));
    assert!(report.mentions("using title fallback"));
    let xml = xml.replace("</PremiereData>", "<Media><Title>first.mov</Title><FilePath>other.mov</FilePath></Media></PremiereData>");
    let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert!(tree.sequences[0].tracks[0].items[0].media_path.is_none());
    assert!(report.mentions("ambiguous title fallback refused"));
}

#[test]
fn duplicate_sequence_names_cannot_be_used_as_a_fallback() {
    let xml = nested_fixture()
        .replace("<Name>Renamed timeline nest</Name>", "<Name>Child</Name>")
        .replace("ObjectURef=\"child\"", "ObjectURef=\"missing\"")
        .replace("</PremiereData>", "<Sequence ObjectUID=\"other-child\"><Name>Child</Name></Sequence></PremiereData>");
    let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert_eq!(fragment.sequences.len(), 3);
    assert!(tree.sequences[0].tracks[0].items[0].nested_sequence.is_none());
    assert!(report.mentions("ambiguous title fallback refused"));
}

#[test]
fn resolved_uid_cycles_are_rejected_even_after_renaming_the_clip() {
    let xml = nested_fixture().replace("ObjectURef=\"child\"", "ObjectURef=\"parent\"");
    assert!(import(xml.as_bytes(), &ImportOptions::default(), None).unwrap_err().to_string().contains("cyclic"));
}

#[test]
fn malformed_reference_graphs_never_panic() {
    for (from, to) in [
        ("ObjectRef=\"source-one\"", "ObjectRef=\"clip-one\""),
        ("ObjectURef=\"media-one\"", "ObjectURef=\"track\""),
        ("ObjectURef=\"media-one\"", "ObjectRef=\"source-one\""),
        ("ObjectUID=\"media-two\"", "ObjectUID=\"media-one\""),
    ] {
        let xml = media_fixture().replace(from, to);
        assert!(std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None)).is_ok());
    }
}

#[test]
fn motion_ids_override_display_names_and_animation_is_not_guessed() {
    let xml = localized_motion_fixture().replace("<Name>Escala</Name>", "<Name>Rotation</Name>");
    let (_, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert_eq!(tree.sequences[0].tracks[0].items[0].motion["Scale"], "75");
    let xml = xml.replace("<MatchName>AE.ADBE Motion</MatchName>", "<MatchName>custom.effect</MatchName>").replace("Movimiento", "Motion");
    let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert!(tree.sequences[0].tracks[0].items[0].motion.is_empty());
    assert!(report.mentions("not rendered"));
    let xml = localized_motion_fixture().replace("<IsTimeVarying>false", "<IsTimeVarying>true");
    let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
    assert!(!tree.sequences[0].tracks[0].items[0].motion.contains_key("Position"));
    assert!(!tree.sequences[0].tracks[0].items[0].motion.contains_key("Rotation"));
    assert!(report.mentions("animated Motion"));
}
