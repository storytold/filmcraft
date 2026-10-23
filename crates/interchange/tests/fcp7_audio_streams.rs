//! Regression for issue #2872 using the reporter's xmeml layout, without opening media.
mod support;
use filmcraft_interchange::{ExportOptions, Format, Imported, export, import};
use filmcraft_project::{TrackItem, TrackKind};
use support::*;

const REPRO: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="5">
  <sequence>
    <name>trackindex-repro</name>
    <duration>45</duration>
    <rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate>
    <media>
      <video>
        <format><samplecharacteristics><width>1920</width><height>1080</height><pixelaspectratio>square</pixelaspectratio><rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate></samplecharacteristics></format>
        <track>
          <clipitem id="v-0">
            <name>obs-recording.mov</name>
            <enabled>TRUE</enabled>
            <start>0</start><end>45</end><in>64</in><out>109</out>
            <file id="file-1">
              <name>obs-recording.mov</name>
              <pathurl>file:///path/to/obs-recording.mov</pathurl>
              <rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate>
              <duration>18911</duration>
              <timecode><string>00:00:00:00</string><displayformat>NDF</displayformat><rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate></timecode>
              <media>
                <video><samplecharacteristics><rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate><width>1920</width><height>1080</height><pixelaspectratio>square</pixelaspectratio></samplecharacteristics></video>
                <!-- two separate stereo audio streams (OBS "Track 1" and "Track 2") -->
                <audio><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate></samplecharacteristics><channelcount>2</channelcount></audio>
                <audio><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate></samplecharacteristics><channelcount>2</channelcount></audio>
              </media>
            </file>
            <compositemode>normal</compositemode>
            <link><linkclipref>v-0</linkclipref></link>
            <link><linkclipref>a-0</linkclipref></link>
          </clipitem>
        </track>
      </video>
      <audio>
        <numOutputChannels>2</numOutputChannels>
        <track>
          <!-- intended: the file's second audio stream (OBS "Track 2"); no pan filter authored -->
          <clipitem id="a-0">
            <name>obs-recording.mov</name>
            <enabled>TRUE</enabled>
            <start>0</start><end>45</end><in>64</in><out>109</out>
            <file id="file-1"/>
            <sourcetrack><mediatype>audio</mediatype><trackindex>2</trackindex></sourcetrack>
            <link><linkclipref>v-0</linkclipref></link>
            <link><linkclipref>a-0</linkclipref></link>
          </clipitem>
        </track>
      </audio>
    </media>
  </sequence>
</xmeml>"##;
const PAN: &str = r#"<filter><effect><effectid>audiopan</effectid><parameter><parameterid>pan</parameterid><value>0</value></parameter></effect></filter>"#;
fn read(xml: &str) -> Imported {
    import(xml.as_bytes(), Format::Fcp7Xml, None).unwrap().0
}
fn audio(imp: &Imported) -> &TrackItem {
    &imp.project.sequence(only_seq(imp)).unwrap().audio_tracks[0].items[0]
}
fn balance(clip: &TrackItem) -> f64 {
    clip.effect("panner").unwrap().param("balance").unwrap().value.as_f64().unwrap()
}
fn streams(imp: &Imported) -> &[filmcraft_media::AudioStreamInfo] {
    &imp.project.item(audio(imp).item).unwrap().as_media().unwrap().info.audio_streams
}
#[test]
fn reporter_file_keeps_both_stereo_streams() {
    let imp = read(REPRO);
    assert_eq!(streams(&imp).len(), 2);
    for stream in streams(&imp) {
        assert_eq!((stream.sample_rate, stream.channels, stream.bits_per_sample), (48000, 2, Some(16)));
    }
}
#[test]
fn reporter_trackindex_selects_second_stream() {
    let imp = read(REPRO);
    assert_eq!(audio(&imp).audio_stream, 1);
    assert!(audio(&imp).source_channels.is_empty());
}
#[test]
fn reporter_stream_selection_does_not_add_hard_pan() {
    assert_eq!(balance(audio(&read(REPRO))), 0.0);
}
#[test]
fn authored_pan_does_not_mask_wrong_stream_selection() {
    for value in ["0", "-0.35"] {
        let pan = PAN.replace("<value>0</value>", &format!("<value>{value}</value>"));
        let xml = REPRO.replace(
            "<sourcetrack><mediatype>audio</mediatype><trackindex>2</trackindex></sourcetrack>",
            &format!("<sourcetrack><mediatype>audio</mediatype><trackindex>2</trackindex></sourcetrack>{pan}"),
        );
        let imp = read(&xml);
        assert_eq!(audio(&imp).audio_stream, 1);
        assert!(audio(&imp).source_channels.is_empty());
        assert!((balance(audio(&imp)) - value.parse::<f64>().unwrap() * 100.0).abs() < 1e-9);
    }
}
fn multi_stream_project() -> (filmcraft_project::Project, filmcraft_project::ItemId) {
    let mut p = filmcraft_project::Project::new("multistream");
    let rate = filmcraft_time::FrameRate::FPS_30;
    let source = media(&mut p, "/media/obs.mov", true, true, rate);
    let info = &mut p.item_mut(source).unwrap().as_media_mut().unwrap().info;
    let mut second = info.audio_streams[0].clone();
    second.sample_rate = 44100;
    second.bits_per_sample = Some(24);
    info.audio_streams.push(second);
    let seq = sequence(&mut p, "timeline", rate, false);
    let id = clip(&mut p, seq, TrackKind::Audio, 0, source, 0, 45, 64);
    p.sequence_mut(seq).unwrap().find_item_mut(id).unwrap().1.audio_stream = 1;
    (p, seq)
}
#[test]
fn exported_stream_selection_and_all_descriptions_roundtrip() {
    let (p, seq) = multi_stream_project();
    let (bytes, _) = export(&p, seq, Format::Fcp7Xml, &ExportOptions::default()).unwrap();
    let imp = read(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(audio(&imp).audio_stream, 1);
    assert!(audio(&imp).source_channels.is_empty());
    assert_eq!(streams(&imp).len(), 2);
    assert_eq!((streams(&imp)[1].sample_rate, streams(&imp)[1].bits_per_sample), (44100, Some(24)));
    assert_eq!(balance(audio(&imp)), 0.0);
}
#[test]
fn exporting_unrepresentable_stream_channel_mapping_reports_error() {
    let (mut p, seq) = multi_stream_project();
    p.sequence_mut(seq).unwrap().audio_tracks[0].items[0].source_channels = vec![1];
    assert!(export(&p, seq, Format::Fcp7Xml, &ExportOptions::default()).is_err());
}

const AUDIO: &str =
    "<audio><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate></samplecharacteristics><channelcount>2</channelcount></audio>";
fn with_index(index: &str) -> String {
    REPRO.replace("<trackindex>2</trackindex>", &format!("<trackindex>{index}</trackindex>"))
}
#[test]
fn stream_one_and_unspecified_stream_keep_full_stereo() {
    for xml in [with_index("1"), REPRO.replace("<sourcetrack><mediatype>audio</mediatype><trackindex>2</trackindex></sourcetrack>", "")] {
        let imp = read(&xml);
        assert_eq!(audio(&imp).audio_stream, 0);
        assert!(audio(&imp).source_channels.is_empty());
        assert_eq!(balance(audio(&imp)), 0.0);
    }
}
#[test]
fn single_stream_index_two_still_splits_right_channel() {
    let imp = read(&REPRO.replacen(AUDIO, "", 1));
    assert_eq!(audio(&imp).audio_stream, 0);
    assert_eq!(audio(&imp).source_channels, vec![1]);
    assert_eq!(balance(audio(&imp)), 100.0);
}
#[test]
fn explicit_exploded_track_keeps_its_channel_semantics() {
    let xml = REPRO.replace(
        "<track>\n          <!-- intended",
        "<track currentExplodedTrackIndex=\"1\" totalExplodedTrackCount=\"2\" premiereTrackType=\"Stereo\">\n          <!-- intended",
    );
    assert_ne!(xml, REPRO);
    let imp = read(&xml);
    assert_eq!(audio(&imp).audio_stream, 0);
    assert_eq!(audio(&imp).source_channels, vec![1]);
    assert_eq!(balance(audio(&imp)), 100.0);
}
#[test]
fn invalid_stream_selectors_skip_audio_with_a_warning() {
    for index in ["0", "-1", "3", "18446744073709551615", "invalid"] {
        let (imp, report) = import(with_index(index).as_bytes(), Format::Fcp7Xml, None).unwrap();
        assert!(imp.project.sequence(only_seq(&imp)).unwrap().audio_tracks.iter().all(|t| t.items.is_empty()), "{index}");
        assert!(report.entries.iter().any(|e| e.message.contains("source audio stream")), "{index}: {report:?}");
    }
}
#[test]
fn malformed_stream_descriptions_return_errors_without_panicking() {
    let mut cases = vec![REPRO.replacen(AUDIO, &AUDIO.repeat(filmcraft_media::MAX_AUDIO_STREAMS + 1), 1)];
    for (key, valid) in [("samplerate", "48000"), ("channelcount", "2"), ("depth", "16")] {
        for invalid in ["0", "-1", "4294967296"] {
            cases.push(REPRO.replace(&format!("<{key}>{valid}</{key}>"), &format!("<{key}>{invalid}</{key}>")));
        }
    }
    for xml in cases {
        assert!(std::panic::catch_unwind(|| import(xml.as_bytes(), Format::Fcp7Xml, None)).unwrap().is_err());
    }
}
#[test]
fn reference_before_definition_preserves_stream_selection() {
    let start = REPRO.find("      <video>").unwrap();
    let end = REPRO.rfind("      </video>").unwrap() + "      </video>".len();
    let video = &REPRO[start..end];
    let xml = REPRO.replacen(video, "", 1).replace("    </media>\n  </sequence>", &format!("{video}\n    </media>\n  </sequence>"));
    assert!(xml.find("<file id=\"file-1\"/>").unwrap() < xml.find("<file id=\"file-1\">").unwrap());
    let imp = read(&xml);
    assert_eq!(audio(&imp).audio_stream, 1);
    assert_eq!(streams(&imp).len(), 2);
}
#[test]
fn authored_pan_keyframes_survive_import_and_roundtrip() {
    let pan = PAN.replace("<value>0</value>", "<keyframe><when>64</when><value>0.2</value></keyframe><keyframe><when>100</when><value>-0.4</value></keyframe>");
    let xml = REPRO.replace("</sourcetrack>", &format!("</sourcetrack>{pan}"));
    let imp = read(&xml);
    let before = audio(&imp).effect("panner").unwrap().param("balance").unwrap().clone();
    assert_eq!(before.keyframes.len(), 2);
    assert_eq!(before.keyframes[0].value.as_f64(), Some(20.0));
    assert_eq!(before.keyframes[1].value.as_f64(), Some(-40.0));
    let (bytes, _) = export(&imp.project, only_seq(&imp), Format::Fcp7Xml, &ExportOptions::default()).unwrap();
    let after = read(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(audio(&after).audio_stream, 1);
    assert_eq!(audio(&after).effect("panner").unwrap().param("balance").unwrap().keyframes, before.keyframes);
}
#[test]
fn project_serialization_preserves_stream_selection() {
    let imp = read(REPRO);
    let json = serde_json::to_vec(&imp.project).unwrap();
    let p: filmcraft_project::Project = serde_json::from_slice(&json).unwrap();
    let clip = &p.sequence(only_seq(&imp)).unwrap().audio_tracks[0].items[0];
    assert_eq!(clip.audio_stream, 1);
    assert!(clip.source_channels.is_empty());
}
#[test]
fn aliased_file_definition_does_not_replace_existing_stream_metadata() {
    let alias = format!(
        "<clipitem><name>alias</name><start>45</start><end>90</end><in>0</in><out>45</out><file id=\"alias\"><name>obs-recording.mov</name><pathurl>file:///path/to/obs-recording.mov</pathurl><media>{AUDIO}</media></file><sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack></clipitem>"
    );
    let xml = REPRO.replace("      </audio>", &format!("<track>{alias}</track>      </audio>"));
    let imp = read(&xml);
    assert_eq!(audio(&imp).audio_stream, 1);
    assert_eq!(streams(&imp).len(), 2);
}
#[test]
fn exporting_invalid_audio_stream_returns_error() {
    let (mut p, seq) = multi_stream_project();
    p.sequence_mut(seq).unwrap().audio_tracks[0].items[0].audio_stream = usize::MAX;
    assert!(export(&p, seq, Format::Fcp7Xml, &ExportOptions::default()).is_err());
}

#[test]
fn mutated_reporter_xml_never_panics() {
    let bytes = REPRO.as_bytes();
    for offset in (0..bytes.len()).step_by(31) {
        assert!(std::panic::catch_unwind(|| import(&bytes[..offset], Format::Fcp7Xml, None)).is_ok());
        let mut changed = bytes.to_vec();
        changed[offset] ^= 0x80;
        assert!(std::panic::catch_unwind(|| import(&changed, Format::Fcp7Xml, None)).is_ok());
    }
}
