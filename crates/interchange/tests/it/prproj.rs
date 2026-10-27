//! Synthetic original project structures: no Adobe media, presets or bundled assets.

use std::io::Write;

use filmcraft_interchange::{Format, ImportOptions, detect, import_with, prproj};
use filmcraft_project::MediaRef;
use filmcraft_time::{FrameRate, Tick};

fn fixture() -> String {
    r#"<PremiereData Version="3">
<Sequence ObjectUID="seq"><Name>Test sequence</Name><TrackGroups><TrackGroup Index="0"><Second ObjectRef="vgroup"/></TrackGroup><TrackGroup Index="1"><Second ObjectRef="agroup"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="vgroup"><FrameRect>0,0,1920,1080</FrameRect><TrackGroup><FrameRate>8475667200</FrameRate><Tracks><Track Index="0" ObjectURef="vtrack"/></Tracks></TrackGroup></VideoTrackGroup>
<AudioTrackGroup ObjectID="agroup"><TrackGroup><FrameRate>5292000</FrameRate><Tracks><Track Index="0" ObjectURef="atrack"/></Tracks></TrackGroup></AudioTrackGroup>
<VideoClipTrack ObjectUID="vtrack"><ClipTrack><Track/><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="vi"/></TrackItems></ClipItems><TransitionItems><TrackItems><TrackItem Index="0" ObjectRef="tr"/></TrackItems></TransitionItems></ClipTrack></VideoClipTrack>
<AudioClipTrack ObjectUID="atrack"><ClipTrack><Track/><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="ai"/></TrackItems></ClipItems></ClipTrack></AudioClipTrack>
<VideoClipTrackItem ObjectID="vi"><ClipTrackItem><TrackItem><Start>254016000000</Start><End>762048000000</End></TrackItem><SubClip ObjectRef="vs"/><ComponentOwner><Components ObjectRef="chain"/></ComponentOwner></ClipTrackItem></VideoClipTrackItem>
<AudioClipTrackItem ObjectID="ai"><ClipTrackItem><TrackItem><Start>254016000000</Start><End>762048000000</End></TrackItem><SubClip ObjectRef="as"/></ClipTrackItem></AudioClipTrackItem>
<SubClip ObjectID="vs"><Name>Video cut</Name><Clip ObjectRef="vc"/></SubClip><SubClip ObjectID="as"><Name>Audio cut</Name><Clip ObjectRef="ac"/></SubClip>
<VideoClip ObjectID="vc"><Clip><Source ObjectRef="vsource"/><InPoint>508032000000</InPoint><PlaybackSpeed>1.5</PlaybackSpeed></Clip></VideoClip>
<AudioClip ObjectID="ac"><Clip><Source ObjectRef="asource"/><InPoint>508032000000</InPoint></Clip></AudioClip>
<VideoMediaSource ObjectID="vsource"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource><AudioMediaSource ObjectID="asource"><MediaSource><Media ObjectURef="media"/></MediaSource></AudioMediaSource>
<Media ObjectUID="media"><FilePath>footage/日本語.mov</FilePath><Title>Footage</Title><VideoStream ObjectRef="video"/><AudioStream ObjectRef="audio"/></Media>
<VideoStream ObjectID="video"><FrameRate>10594584000</FrameRate><FrameRect>0,0,1280,720</FrameRect><Duration>2540160000000</Duration></VideoStream>
<AudioStream ObjectID="audio"><FrameRate>5292000</FrameRate><Duration>2540160000000</Duration><AudioChannelLayout>[{},{}]</AudioChannelLayout></AudioStream>
<Link ObjectID="link"><TrackItemGroup><TrackItems><TrackItem Index="0" ObjectRef="vi"/><TrackItem Index="1" ObjectRef="ai"/></TrackItems></TrackItemGroup></Link>
<VideoComponentChain ObjectID="chain"><ComponentChain><Components><Component Index="0" ObjectRef="motion"/></Components></ComponentChain></VideoComponentChain>
<VideoFilterComponent ObjectID="motion"><MatchName>AE.ADBE Motion</MatchName><Component><Params><Param Index="0" ObjectRef="scale"/><Param Index="1" ObjectRef="position"/></Params></Component></VideoFilterComponent>
<VideoComponentParam ObjectID="scale"><ParameterID>2</ParameterID><Name>Scale</Name><StartKeyframe>-91445760000000000,175.,0</StartKeyframe></VideoComponentParam>
<PointComponentParam ObjectID="position"><ParameterID>1</ParameterID><StartKeyframe>-91445760000000000,0.25:0.75,0</StartKeyframe></PointComponentParam>
<VideoTransitionTrackItem ObjectID="tr"><TransitionTrackItem><TrackItem><Start>254016000000</Start><End>381024000000</End></TrackItem><Alignment>0</Alignment><HasOutgoingClip>false</HasOutgoingClip><HasIncomingClip>true</HasIncomingClip><MatchName>third.party.dissolve</MatchName><DisplayName>Fancy dissolve</DisplayName></TransitionTrackItem></VideoTransitionTrackItem>
</PremiereData>"#.into()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}

fn saved_tabs(properties: &str) -> String {
    fixture().replace(
        "<PremiereData Version=\"3\">",
        &format!(r#"<PremiereData Version="3"><Project ObjectID="project"><Node><Properties>{properties}</Properties></Node></Project><Sequence ObjectUID="trailer"><Name>Trailer</Name><TrackGroups><TrackGroup Index="0"><Second ObjectRef="emptygroup"/></TrackGroup></TrackGroups></Sequence><VideoTrackGroup ObjectID="emptygroup"><FrameRect>0,0,1920,1080</FrameRect><TrackGroup><FrameRate>8475667200</FrameRate><Tracks/></TrackGroup></VideoTrackGroup>"#),
    )
}

#[test]
fn saved_open_timeline_is_separate_from_sequence_storage_order() {
    let xml = saved_tabs("<MZ.PrefixKey.OpenSequenceGuidList.1>seq</MZ.PrefixKey.OpenSequenceGuidList.1>");
    let (imported, view, _) = prproj::import_with_view(&gzip(xml.as_bytes()), &Default::default()).unwrap();
    let trailer = imported.sequences[0];
    let edit = imported.sequences[1];
    assert_eq!(imported.project.item(trailer).unwrap().name, "Trailer");
    assert_eq!(view.active_sequence, Some(edit));
    assert_eq!(view.open_sequences, [edit]);
    assert_eq!(imported.project.sequence(trailer).unwrap().all_tracks().map(|t| t.items.len()).sum::<usize>(), 0);
    assert_eq!(imported.project.sequence(edit).unwrap().all_tracks().map(|t| t.items.len()).sum::<usize>(), 2);
}

#[test]
fn saved_tabs_are_ordered_deduplicated_and_ignore_stale_non_sequence_uids() {
    let props = r#"<MZ.PrefixKey.OpenSequenceGuidList.8>trailer</MZ.PrefixKey.OpenSequenceGuidList.8>
<MZ.PrefixKey.OpenSequenceGuidList.2>seq</MZ.PrefixKey.OpenSequenceGuidList.2>
<MZ.PrefixKey.OpenSequenceGuidList.3>seq</MZ.PrefixKey.OpenSequenceGuidList.3>
<MZ.PrefixKey.OpenSequenceGuidList.0>gone</MZ.PrefixKey.OpenSequenceGuidList.0>
<MZ.PrefixKey.OpenSequenceGuidList.1>media</MZ.PrefixKey.OpenSequenceGuidList.1>
<MZ.PrefixKey.OpenSequenceGuidList.999999999999999999999999>trailer</MZ.PrefixKey.OpenSequenceGuidList.999999999999999999999999>"#;
    let (imported, view, _) = prproj::import_with_view(saved_tabs(props).as_bytes(), &Default::default()).unwrap();
    assert_eq!(view.open_sequences, [imported.sequences[1], imported.sequences[0]]);
    assert_eq!(view.active_sequence, Some(imported.sequences[1]));
    for properties in ["", "<MZ.PrefixKey.OpenSequenceGuidList.1>gone</MZ.PrefixKey.OpenSequenceGuidList.1>"] {
        let (p, view, _) = prproj::import_with_view(saved_tabs(properties).as_bytes(), &Default::default()).unwrap();
        assert_eq!(view.open_sequences, [p.sequences[0]]);
        assert_eq!(view.active_sequence, Some(p.sequences[0]));
    }
}

#[test]
fn hostile_saved_tab_metadata_is_bounded() {
    let property = "<MZ.PrefixKey.OpenSequenceGuidList.1>seq</MZ.PrefixKey.OpenSequenceGuidList.1>";
    let too_many: String =
        (0..=4096).map(|index| format!("<MZ.PrefixKey.OpenSequenceGuidList.{index}>seq</MZ.PrefixKey.OpenSequenceGuidList.{index}>")).collect();
    for properties in [too_many, property.replace("seq", &"x".repeat(257)), property.repeat(2)] {
        assert!(prproj::import_with_view(saved_tabs(&properties).as_bytes(), &Default::default()).is_err());
    }
}

#[test]
fn compressed_and_plain_import_preserve_the_edit() {
    let xml = fixture();
    let compressed = gzip(xml.as_bytes());
    let opts = ImportOptions { base_dir: Some("/work".into()), name: Some("Conversion".into()), ..Default::default() };
    let (plain, report) = import_with(xml.as_bytes(), Format::Premiere, &opts).unwrap();
    let (zipped, _) = prproj::import(&compressed, &opts).unwrap();
    assert_eq!(plain, zipped);
    assert_eq!(plain.project.name, "Conversion");
    let seq = plain.project.sequence(plain.sequences[0]).unwrap();
    assert_eq!(seq.settings.frame_rate, FrameRate::FPS_29_97);
    assert_eq!(seq.settings.sample_rate, 48000);
    assert_eq!((seq.settings.width, seq.settings.height), (1920, 1080));
    let video = &seq.video_tracks[0].items[0];
    let audio = &seq.audio_tracks[0].items[0];
    assert_eq!((video.start, video.duration, video.source_in), (Tick(254016000000), Tick(508032000000), Tick(508032000000)));
    assert_eq!(video.speed, 1.5);
    assert_eq!(video.item, audio.item);
    assert!(video.link.is_some());
    assert_eq!(video.link, audio.link);
    assert_eq!(video.effect("motion").unwrap().param("scale").unwrap().value.as_f64(), Some(175.0));
    let pos = video.effect("motion").unwrap().param("position").unwrap().value.as_vec2().unwrap();
    assert_eq!((pos.x, pos.y), (480.0, 810.0));
    let transition = &seq.video_tracks[0].transitions[0];
    assert_eq!(transition.from, None);
    assert_eq!(transition.to, Some(video.id));
    assert_eq!(transition.effect.effect, "cross_dissolve");
    assert!(report.mentions("third.party.dissolve"));
    assert!(report.mentions("not transferred"));
    let m = plain.project.item(video.item).unwrap().as_media().unwrap();
    assert_eq!(m.media, MediaRef::File { path: "/work/footage/日本語.mov".into() });
    assert_eq!(m.info.video.as_ref().unwrap().frame_rate, FrameRate::FPS_23_976);
}

#[test]
fn detects_import_only_format_and_refuses_export() {
    assert_eq!(Format::from_extension(".PRPROJ"), Some(Format::Premiere));
    assert_eq!(detect(b"corrupt", Some("prproj")), Some(Format::Premiere));
    assert_eq!(detect(fixture().as_bytes(), None), Some(Format::Premiere));
    assert!(!Format::ALL.contains(&Format::Premiere));
    let (p, _) = prproj::import(fixture().as_bytes(), &ImportOptions::default()).unwrap();
    let err = filmcraft_interchange::export(&p.project, p.sequences[0], Format::Premiere, &Default::default()).unwrap_err();
    assert!(err.to_string().contains("unsupported"));
}

#[test]
fn static_stereo_volume_and_channel_volume_are_preserved() {
    let xml = fixture().replace("<SubClip ObjectRef=\"as\"/>", "<SubClip ObjectRef=\"as\"/><ComponentOwner><Components ObjectRef=\"audiochain\"/></ComponentOwner>")
        .replace("</PremiereData>", r#"
<AudioComponentChain ObjectID="audiochain"><ComponentChain><Components><Component Index="0" ObjectRef="volume"/><Component Index="1" ObjectRef="channels"/></Components></ComponentChain></AudioComponentChain>
<AudioFilterComponent ObjectID="volume"><FilterMatchName>Internal Volume Stereo</FilterMatchName><AudioComponent><Component><Params><Param Index="0" ObjectRef="level"/></Params></Component></AudioComponent></AudioFilterComponent>
<AudioFilterComponent ObjectID="channels"><FilterMatchName>Internal Channel Volume Stereo</FilterMatchName><AudioComponent><Component><Params><Param Index="0" ObjectRef="left"/><Param Index="1" ObjectRef="right"/></Params></Component></AudioComponent></AudioFilterComponent>
<AudioComponentParam ObjectID="level"><Name>Level</Name><StartKeyframe>-91445760000000000,0.5,0</StartKeyframe></AudioComponentParam>
<AudioComponentParam ObjectID="left"><Name>Left</Name><CurrentValue>0.1778279410038923</CurrentValue></AudioComponentParam>
<AudioComponentParam ObjectID="right"><Name>Right</Name><CurrentValue>0</CurrentValue></AudioComponentParam>
</PremiereData>"#);
    let (p, report) = prproj::import(xml.as_bytes(), &Default::default()).unwrap();
    let audio = &p.project.sequence(p.sequences[0]).unwrap().audio_tracks[0].items[0];
    let level = audio.effect("volume").unwrap().f64_at("level", Tick::ZERO);
    assert!((level + 6.020599913).abs() < 1e-8);
    assert!((audio.effect("channel_volume").unwrap().f64_at("left", Tick::ZERO) + 15.0).abs() < 1e-8);
    assert_eq!(audio.effect("channel_volume").unwrap().f64_at("right", Tick::ZERO), -287.5);
    assert!(!report.mentions("was not imported"));
}

#[test]
fn end_aligned_transition_uses_integer_tick_offset() {
    let xml = fixture()
        .replace("<Start>254016000000</Start><End>381024000000</End>", "<Start>635040000000</Start><End>762048000000</End>")
        .replace("<Alignment>0</Alignment>", "<Alignment>127008000000</Alignment>")
        .replace(
            "<HasOutgoingClip>false</HasOutgoingClip><HasIncomingClip>true</HasIncomingClip>",
            "<HasOutgoingClip>true</HasOutgoingClip><HasIncomingClip>false</HasIncomingClip>",
        );
    let (p, _) = prproj::import(xml.as_bytes(), &Default::default()).unwrap();
    let track = &p.project.sequence(p.sequences[0]).unwrap().video_tracks[0];
    let transition = &track.transitions[0];
    assert_eq!(transition.start, Tick(635040000000));
    assert_eq!(transition.duration, Tick(127008000000));
    assert_eq!(transition.from, Some(track.items[0].id));
    assert_eq!(transition.to, None);
    assert_eq!(transition.align, filmcraft_project::TransitionAlign::EndAtCut);
}

#[test]
fn rejects_hostile_numbers_missing_references_and_duplicates() {
    for (from, to) in [
        ("<PlaybackSpeed>1.5</PlaybackSpeed>", "<PlaybackSpeed>NaN</PlaybackSpeed>"),
        ("<PlaybackSpeed>1.5</PlaybackSpeed>", "<PlaybackSpeed>-1</PlaybackSpeed>"),
        ("<PlaybackSpeed>1.5</PlaybackSpeed>", "<PlaybackSpeed>0</PlaybackSpeed>"),
        ("<FrameRate>8475667200</FrameRate>", "<FrameRate>0</FrameRate>"),
        ("<FrameRate>5292000</FrameRate>", "<FrameRate>0</FrameRate>"),
        ("<FrameRect>0,0,1920,1080</FrameRect>", "<FrameRect>0,0,4294967295,4294967295</FrameRect>"),
        ("<FrameRect>0,0,1920,1080</FrameRect>", "<FrameRect>-9223372036854775808,0,9223372036854775807,1080</FrameRect>"),
        ("<Start>254016000000</Start>", "<Start>-1</Start>"),
        ("<End>762048000000</End>", "<End>0</End>"),
        ("<End>762048000000</End>", "<End>9223372036854775807</End>"),
        ("ObjectRef=\"vsource\"", "ObjectRef=\"missing\""),
        ("ObjectID=\"asource\"", "ObjectID=\"vsource\""),
        ("<Alignment>0</Alignment>", "<Alignment>NaN</Alignment>"),
        ("<Alignment>0</Alignment>", "<Alignment>999999999999</Alignment>"),
        ("175.,0", "NaN,0"),
        ("Version=\"3\"", "Version=\"4\""),
    ] {
        let xml = fixture().replace(from, to);
        let result = std::panic::catch_unwind(|| prproj::import(xml.as_bytes(), &ImportOptions::default()));
        assert!(result.is_ok(), "panicked for {to}");
        assert!(result.unwrap().is_err(), "accepted {to}");
    }
}

#[test]
fn rejects_sequence_cycles_and_deep_xml() {
    let xml = fixture().replace(
        "<VideoMediaSource ObjectID=\"vsource\"><MediaSource><Media ObjectURef=\"media\"/></MediaSource></VideoMediaSource>",
        "<VideoSequenceSource ObjectID=\"vsource\"><SequenceSource><Sequence ObjectURef=\"seq\"/></SequenceSource></VideoSequenceSource>",
    );
    assert!(prproj::import(xml.as_bytes(), &Default::default()).unwrap_err().to_string().contains("cyclic"));
    let xml = format!("<PremiereData Version=\"3\">{}{}</PremiereData>", "<x>".repeat(5000), "</x>".repeat(5000));
    let result = std::panic::catch_unwind(|| prproj::import(xml.as_bytes(), &Default::default()));
    assert!(result.is_ok());
    assert!(result.unwrap().unwrap_err().to_string().contains("nesting"));
    let xml = "<!DOCTYPE PremiereData [<!ENTITY x 'test'>]><PremiereData Version=\"3\">&x;</PremiereData>";
    assert!(prproj::import(xml.as_bytes(), &Default::default()).is_err());
}

#[test]
fn a_nested_sequence_is_retained_but_is_not_a_top_level_timeline() {
    let xml = fixture().replace("<VideoMediaSource ObjectID=\"vsource\"><MediaSource><Media ObjectURef=\"media\"/></MediaSource></VideoMediaSource>",
        "<VideoSequenceSource ObjectID=\"vsource\"><SequenceSource><Sequence ObjectURef=\"nest\"/></SequenceSource></VideoSequenceSource>")
        .replace("</PremiereData>", r#"<Sequence ObjectUID="nest"><Name>Nested</Name><TrackGroups><TrackGroup Index="0"><Second ObjectRef="nested-group"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="nested-group"><FrameRect>0,0,1280,720</FrameRect><TrackGroup><FrameRate>10160640000</FrameRate><Tracks/></TrackGroup></VideoTrackGroup></PremiereData>"#);
    let xml = xml.replace("<ParameterID>1</ParameterID>", "<ParameterID>6</ParameterID>");
    let (p, _) = prproj::import(xml.as_bytes(), &Default::default()).unwrap();
    assert_eq!(p.sequences.len(), 1);
    assert_eq!(p.project.sequences().count(), 2);
    let parent = p.project.sequence(p.sequences[0]).unwrap();
    assert_eq!(p.project.item(parent.video_tracks[0].items[0].item).unwrap().name, "Nested");
    let anchor = parent.video_tracks[0].items[0].effect("motion").unwrap().param("anchor").unwrap().value.as_vec2().unwrap();
    assert_eq!((anchor.x, anchor.y), (320.0, 540.0));
}

#[test]
fn truncation_bit_flips_and_bad_gzip_never_panic() {
    let xml = fixture();
    for bytes in [xml.as_bytes().to_vec(), gzip(xml.as_bytes())] {
        for end in 0..bytes.len() {
            let result = std::panic::catch_unwind(|| prproj::import(&bytes[..end], &Default::default()));
            assert!(result.is_ok(), "truncation {end} panicked");
            assert!(result.unwrap().is_err(), "truncation {end} was accepted");
        }
        for i in 0..bytes.len() {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= 0x80;
            assert!(std::panic::catch_unwind(|| prproj::import(&corrupt, &Default::default())).is_ok(), "bit flip {i} panicked");
        }
    }
}

#[test]
fn repeated_references_and_oversized_fields_have_global_budgets() {
    let references: String = (0..1000).map(|i| format!("<Component Index=\"{i}\" ObjectRef=\"motion\"/>")).collect();
    let clips: String = (0..1000).map(|i| format!("<TrackItem Index=\"{i}\" ObjectRef=\"vi\"/>")).collect();
    let xml = fixture().replace("<Component Index=\"0\" ObjectRef=\"motion\"/>", &references).replace(
        "<ClipItems><TrackItems><TrackItem Index=\"0\" ObjectRef=\"vi\"/></TrackItems></ClipItems>",
        &format!("<ClipItems><TrackItems>{clips}</TrackItems></ClipItems>"),
    );
    let result = std::panic::catch_unwind(|| prproj::import(xml.as_bytes(), &Default::default()));
    assert!(result.is_ok());
    let error = result.unwrap().unwrap_err().to_string();
    assert!(error.contains("budget"), "{error}");
    let xml = fixture().replace("<Name>Video cut</Name>", &format!("<Name>{}</Name>", "x".repeat(1025)));
    assert!(prproj::import(xml.as_bytes(), &Default::default()).unwrap_err().to_string().contains("text budget"));
    let opts = ImportOptions { base_dir: Some("x".repeat(32769)), ..Default::default() };
    assert!(prproj::import(fixture().as_bytes(), &opts).is_err());
    let media: String = (0..4100).map(|i| format!("<Media ObjectID=\"missing-{i}\"/>")).collect();
    let xml = fixture().replace("</PremiereData>", &(media + "</PremiereData>"));
    assert!(prproj::import(xml.as_bytes(), &Default::default()).unwrap_err().to_string().contains("warning budget"));
}
