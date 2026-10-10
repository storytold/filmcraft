//! Final Cut Pro 7 XML (xmeml): a hand-written Premiere-style document (written from the public
//! xmeml element descriptions) and export → import round trips.

mod support;

use filmcraft_interchange::{ExportOptions, Format, detect, export, import};
use filmcraft_media::Generator;
use filmcraft_project::{ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, Param, ParamValue, Project, TrackKind, TransitionAlign};
use filmcraft_time::{FrameRate, Tick};
use proptest::prelude::*;
use support::*;

const RATE: &str = "<rate><timebase>24</timebase><ntsc>TRUE</ntsc></rate>";

fn premiere_style_doc() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="4">
  <project>
    <name>Lighthouse</name>
    <children>
      <bin>
        <name>Footage</name>
        <children>
          <clip id="masterclip-1">
            <name>tower.mov</name>
            <duration>2400</duration>
            {RATE}
            <labels><label2>Mango</label2></labels>
            <media><video><track><clipitem id="clipitem-m1"><name>tower.mov</name>
              <file id="file-1">
                <name>tower.mov</name>
                <pathurl>file://localhost/Volumes/Shoot/tower%20wide.mov</pathurl>
                {RATE}
                <duration>2400</duration>
                <timecode>{RATE}<string>01:00:00:00</string><frame>86400</frame><displayformat>NDF</displayformat></timecode>
                <media>
                  <video><samplecharacteristics>{RATE}<width>3840</width><height>2160</height></samplecharacteristics></video>
                  <audio><samplecharacteristics><depth>24</depth><samplerate>48000</samplerate></samplecharacteristics><channelcount>2</channelcount></audio>
                </media>
              </file>
            </clipitem></track></video></media>
          </clip>
        </children>
      </bin>
      <sequence id="sequence-1">
        <name>Main Edit</name>
        <duration>130</duration>
        {RATE}
        <timecode>{RATE}<string>01:00:00:00</string><frame>86400</frame><displayformat>NDF</displayformat></timecode>
        <media>
          <video>
            <format><samplecharacteristics>{RATE}<width>1920</width><height>1080</height><pixelaspectratio>square</pixelaspectratio></samplecharacteristics></format>
            <track>
              <clipitem id="clipitem-1">
                <masterclipid>masterclip-1</masterclipid>
                <name>tower.mov</name>
                <enabled>TRUE</enabled>
                {RATE}
                <start>0</start><end>-1</end><in>0</in><out>48</out>
                <file id="file-1"/>
                <link><linkclipref>clipitem-1</linkclipref><mediatype>video</mediatype><trackindex>1</trackindex><clipindex>1</clipindex></link>
                <link><linkclipref>clipitem-3</linkclipref><mediatype>audio</mediatype><trackindex>1</trackindex><clipindex>1</clipindex></link>
                <labels><label2>Cerulean</label2></labels>
              </clipitem>
              <transitionitem>
                {RATE}
                <start>36</start><end>60</end><alignment>center</alignment>
                <effect><name>Cross Dissolve</name><effectid>Cross Dissolve</effectid><effectcategory>Dissolve</effectcategory><effecttype>transition</effecttype><mediatype>video</mediatype></effect>
              </transitionitem>
              <clipitem id="clipitem-2">
                <name>waves</name>
                {RATE}
                <start>-1</start><end>120</end><in>100</in><out>172</out>
                <file id="file-2">
                  <name>waves.mov</name>
                  <pathurl>media/waves.mov</pathurl>
                  {RATE}
                  <duration>1000</duration>
                  <media><video><samplecharacteristics><width>1920</width><height>1080</height></samplecharacteristics></video></media>
                </file>
                <filter><effect>
                  <name>Basic Motion</name><effectid>basic</effectid><effectcategory>motion</effectcategory><effecttype>motion</effecttype><mediatype>video</mediatype>
                  <parameter><parameterid>scale</parameterid><name>Scale</name><valuemin>0</valuemin><valuemax>1000</valuemax><value>100</value>
                    <keyframe><when>100</when><value>100</value></keyframe>
                    <keyframe><when>160</when><value>150</value></keyframe>
                  </parameter>
                  <parameter><parameterid>rotation</parameterid><name>Rotation</name><value>-12.5</value></parameter>
                  <parameter><parameterid>center</parameterid><name>Center</name><value><horiz>0.25</horiz><vert>-0.1</vert></value></parameter>
                </effect></filter>
                <filter><effect>
                  <name>Opacity</name><effectid>opacity</effectid><effectcategory>motion</effectcategory><effecttype>motion</effecttype><mediatype>video</mediatype>
                  <parameter><parameterid>opacity</parameterid><name>opacity</name><valuemin>0</valuemin><valuemax>100</valuemax><value>50</value></parameter>
                </effect></filter>
                <filter><effect>
                  <name>Gaussian Blur</name><effectid>GaussianBlur</effectid><effecttype>filter</effecttype><mediatype>video</mediatype>
                </effect></filter>
                <filter><effect>
                  <name>Mystery Glow Pro</name><effectid>mystery</effectid><effecttype>filter</effecttype><mediatype>video</mediatype>
                </effect></filter>
                <marker><name>wave peak</name><comment>use this</comment><in>130</in><out>-1</out></marker>
              </clipitem>
            </track>
            <track>
              <clipitem id="clipitem-4">
                <name>Titles Nest</name>
                {RATE}
                <start>24</start><end>72</end><in>0</in><out>48</out>
                <sequence id="sequence-2"/>
              </clipitem>
              <generatoritem id="clipitem-7">
                <name>Red Matte</name>
                {RATE}
                <start>100</start><end>130</end><in>0</in><out>30</out>
                <effect><name>Color</name><effectid>Color</effectid><effectcategory>Matte</effectcategory><effecttype>generator</effecttype><mediatype>video</mediatype>
                  <parameter><parameterid>fillcolor</parameterid><name>Color</name><value><alpha>255</alpha><red>255</red><green>0</green><blue>0</blue></value></parameter>
                </effect>
              </generatoritem>
              <enabled>TRUE</enabled>
              <locked>TRUE</locked>
            </track>
          </video>
          <audio>
            <track>
              <clipitem id="clipitem-3">
                <name>tower.mov</name>
                {RATE}
                <start>0</start><end>48</end><in>0</in><out>48</out>
                <file id="file-1"/>
                <sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>
                <filter><effect>
                  <name>Audio Levels</name><effectid>audiolevels</effectid><effectcategory>audiolevels</effectcategory><effecttype>audiolevels</effecttype><mediatype>audio</mediatype>
                  <parameter><parameterid>level</parameterid><name>Level</name><valuemin>0</valuemin><valuemax>3.98109</valuemax><value>0.5</value>
                    <keyframe><when>0</when><value>1</value></keyframe>
                    <keyframe><when>24</when><value>0.5</value></keyframe>
                  </parameter>
                </effect></filter>
                <link><linkclipref>clipitem-1</linkclipref><mediatype>video</mediatype></link>
                <link><linkclipref>clipitem-3</linkclipref><mediatype>audio</mediatype></link>
              </clipitem>
            </track>
            <track>
              <clipitem id="clipitem-5">
                <name>music.wav</name>
                <enabled>FALSE</enabled>
                {RATE}
                <start>0</start><end>120</end><in>0</in><out>120</out>
                <file id="file-3"><name>music.wav</name><pathurl>file:///Volumes/Audio/music.wav</pathurl>
                  <media><audio><samplecharacteristics><samplerate>44100</samplerate></samplecharacteristics><channelcount>2</channelcount></audio></media>
                </file>
              </clipitem>
            </track>
          </audio>
        </media>
        <marker><name>Check sky</name><comment></comment><in>30</in><out>-1</out></marker>
      </sequence>
      <sequence id="sequence-2">
        <name>Titles Nest</name>
        {RATE}
        <media>
          <video>
            <track>
              <clipitem id="clipitem-6">
                <name>waves fast</name>
                {RATE}
                <start>0</start><end>48</end><in>0</in><out>96</out>
                <file id="file-2"/>
                <filter><effect><name>Time Remap</name><effectid>timeremap</effectid><effectcategory>motion</effectcategory><effecttype>motion</effecttype><mediatype>video</mediatype>
                  <parameter><parameterid>speed</parameterid><name>speed</name><value>200</value></parameter>
                  <parameter><parameterid>reverse</parameterid><name>reverse</name><value>FALSE</value></parameter>
                </effect></filter>
              </clipitem>
            </track>
          </video>
        </media>
      </sequence>
    </children>
  </project>
</xmeml>
"#
    )
}

#[test]
fn golden_premiere_style_nested_and_transitions() {
    let doc = premiere_style_doc();
    assert_eq!(detect(doc.as_bytes(), Some("xml")), Some(Format::Fcp7Xml));
    let (imp, rep) = import(doc.as_bytes(), Format::Fcp7Xml, Some("/proj")).unwrap();
    let p = &imp.project;
    assert_eq!(p.name, "Lighthouse");
    assert_eq!(imp.sequences.len(), 2);
    let main = imp.sequences.iter().copied().find(|s| p.item(*s).unwrap().name == "Main Edit").unwrap();
    let nest = imp.sequences.iter().copied().find(|s| p.item(*s).unwrap().name == "Titles Nest").unwrap();
    let s = p.sequence(main).unwrap();
    let r = FrameRate::FPS_23_976;
    assert_eq!(s.settings.frame_rate, r);
    assert_eq!(s.start_timecode, 86_400);

    // Master clip in the Footage bin, with its file characteristics.
    let tower = p.items.values().find(|i| i.name == "tower.mov").unwrap();
    let tm = tower.as_media().unwrap();
    assert_eq!(tm.media, MediaRef::File { path: "/Volumes/Shoot/tower wide.mov".into() });
    assert_eq!(tm.info.video.as_ref().unwrap().width, 3840);
    assert_eq!(tm.info.start_timecode, Some(86_400));
    assert_eq!(tm.info.duration, r.tick_of(2400));
    assert_eq!(tower.label, Label::Mango);
    let bin = p.root.children.iter().find_map(|c| if let filmcraft_project::BinEntry::Bin(b) = c { Some(b) } else { None }).unwrap();
    assert_eq!(bin.name, "Footage");
    assert_eq!(p.root.parent_of(tower.id), Some(bin.id));

    // V1: -1 start/end resolved from in/out, centred dissolve.
    let v1 = &s.video_tracks[0];
    assert_eq!(v1.items.len(), 2);
    let (c1, c2) = (&v1.items[0], &v1.items[1]);
    assert_eq!((c1.start, c1.duration), (r.tick_of(0), r.tick_of(48)));
    assert_eq!((c2.start, c2.duration, c2.source_in), (r.tick_of(48), r.tick_of(72), r.tick_of(100)));
    assert_eq!(c1.label, Label::Cerulean);
    assert_eq!(media_key(p, c2.item), "/proj/media/waves.mov");
    let t = &v1.transitions[0];
    assert_eq!((t.start, t.duration, t.from, t.to, t.align), (r.tick_of(36), r.tick_of(24), Some(c1.id), Some(c2.id), TransitionAlign::CenterAtCut));
    assert_eq!(t.effect.effect, "cross_dissolve");
    // Motion / opacity / generic effect by name.
    let scale = c2.effect("motion").unwrap().param("scale").unwrap();
    assert_eq!(scale.keyframes.len(), 2);
    assert_eq!(scale.keyframes[1].time, r.tick_of(160));
    assert_eq!(scale.keyframes[1].value, ParamValue::Float(150.0));
    assert_eq!(c2.effect("motion").unwrap().param("rotation").unwrap().value, ParamValue::Float(-12.5));
    let pos = c2.effect("motion").unwrap().param("position").unwrap().value.as_vec2().unwrap();
    assert!((pos.x - (960.0 + 480.0)).abs() < 1e-9 && (pos.y - (540.0 - 108.0)).abs() < 1e-9, "{pos:?}");
    assert_eq!(c2.effect("opacity").unwrap().param("opacity").unwrap().value, ParamValue::Float(50.0));
    assert!(c2.effect("gaussian_blur").is_some());
    assert!(rep.mentions("Mystery Glow Pro"), "{rep}");
    assert_eq!(c2.markers.len(), 1);
    assert_eq!((c2.markers[0].name.as_str(), c2.markers[0].start), ("wave peak", r.tick_of(130)));

    // V2: nested sequence clip, colour matte generator, locked track.
    let v2 = &s.video_tracks[1];
    assert!(v2.locked);
    assert_eq!(v2.items[0].item, nest);
    assert!(matches!(p.item(v2.items[0].item).unwrap().kind, ItemKind::Sequence(_)));
    let g = p.item(v2.items[1].item).unwrap().as_media().unwrap();
    assert_eq!(g.media, MediaRef::Generator(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }));
    let ns = p.sequence(nest).unwrap();
    assert_eq!(ns.video_tracks[0].items[0].speed, 2.0);
    assert_eq!(ns.video_tracks[0].items[0].item, c2.item, "file-2 reference shared across sequences");

    // Audio: levels as dB with keyframes, link to the video clip, disabled music.
    let a1 = &s.audio_tracks[0].items[0];
    let lvl = a1.effect("volume").unwrap().param("level").unwrap();
    assert!((lvl.keyframes[1].value.as_f64().unwrap() + 6.0206).abs() < 1e-3);
    assert_eq!(lvl.keyframes[0].value, ParamValue::Float(0.0));
    assert!(a1.link.is_some());
    assert_eq!(a1.link, c1.link);
    let music = &s.audio_tracks[1].items[0];
    assert!(!music.enabled);
    assert!(p.item(music.item).unwrap().as_media().unwrap().info.video.is_none());
    assert_eq!(s.markers[0].name, "Check sky");
    assert_eq!(s.markers[0].start, r.tick_of(30));
}

fn rich_project() -> (Project, filmcraft_project::ItemId) {
    let r = FrameRate::FPS_29_97;
    let mut p = Project::new("P");
    let a = media(&mut p, "/m/a b.mov", true, true, r);
    let b = media(&mut p, "/m/b.mov", true, true, r);
    let m = media(&mut p, "/m/music.wav", false, true, r);
    let nest = sequence(&mut p, "Nested", r, true);
    clip(&mut p, nest, TrackKind::Video, 0, b, 0, 90, 30);
    let s = sequence(&mut p, "Main", r, true);
    p.sequence_mut(s).unwrap().start_timecode = 107_892;
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0, 60, 10);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0, 60, 10);
    link(&mut p, s, &[v1, a1]);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, b, 60, 45, 300);
    transition(&mut p, s, TrackKind::Video, 0, "dip_to_black", Some(v1), Some(v2), 50, 20, TransitionAlign::CenterAtCut);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(v2), None, 90, 15, TransitionAlign::EndAtCut);
    let n = clip(&mut p, s, TrackKind::Video, 1, nest, 20, 40, 0);
    let mm = clip(&mut p, s, TrackKind::Audio, 1, m, 0, 105, 0);
    transition(&mut p, s, TrackKind::Audio, 1, "constant_power", None, Some(mm), 0, 10, TransitionAlign::StartAtCut);
    {
        let q = p.sequence_mut(s).unwrap();
        let (_, it) = q.find_item_mut(v2).unwrap();
        it.speed = 1.5;
        it.label = Label::Rose;
        let mut sc = Param::new(ParamValue::Float(100.0));
        sc.keyframes.push(Keyframe::new(r.tick_of(300), ParamValue::Float(100.0)));
        sc.keyframes.push(Keyframe::new(r.tick_of(330), ParamValue::Float(80.0)));
        it.effect_mut("motion").unwrap().params.insert("scale".into(), sc);
        it.effect_mut("opacity").unwrap().params.insert("opacity".into(), Param::new(ParamValue::Float(75.0)));
        it.effects.push(filmcraft_project::find_effect("gaussian_blur").unwrap().instance());
        it.markers.push(Marker {
            id: MarkerId(900),
            start: r.tick_of(310),
            duration: r.tick_of(5),
            name: "hit".into(),
            comment: "c".into(),
            kind: MarkerKind::Comment,
            color: Label::Yellow,
        });
        let (_, ni) = q.find_item_mut(n).unwrap();
        ni.enabled = false;
        ni.reverse = true;
        let (_, ai) = q.find_item_mut(a1).unwrap();
        ai.effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-6.0)));
        q.markers.push(Marker {
            id: MarkerId(901),
            start: r.tick_of(12),
            duration: Tick::ZERO,
            name: "seq m".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: Label::Blue,
        });
        q.video_tracks[1].locked = true;
    }
    (p, s)
}

#[test]
fn rich_roundtrip() {
    let (p, s) = rich_project();
    let (imp, text, rep) = roundtrip(&p, s, Format::Fcp7Xml, &ExportOptions::default());
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\">"));
    assert!(text.contains("<pathurl>file://localhost/m/a%20b.mov</pathurl>"), "{text}");
    assert!(!rep.has_warnings(), "{rep}");
    let si = only_seq(&imp);
    let q = &imp.project;
    for kind in [TrackKind::Video, TrackKind::Audio] {
        assert_eq!(structure(q, si, kind), structure(&p, s, kind), "{text}");
    }
    assert_eq!(links(q, si), links(&p, s));
    let qs = q.sequence(si).unwrap();
    let ps = p.sequence(s).unwrap();
    assert_eq!(qs.start_timecode, ps.start_timecode);
    assert!(qs.settings.drop_frame);
    assert_eq!(
        qs.markers.iter().map(|m| (&m.name, m.start, m.color)).collect::<Vec<_>>(),
        ps.markers.iter().map(|m| (&m.name, m.start, m.color)).collect::<Vec<_>>()
    );
    assert!(qs.video_tracks[1].locked);
    let v2 = &qs.video_tracks[0].items[1];
    let pv2 = &ps.video_tracks[0].items[1];
    assert_eq!(v2.label, Label::Rose);
    assert_eq!(v2.effect("motion").unwrap().param("scale"), pv2.effect("motion").unwrap().param("scale"));
    assert_eq!(v2.effect("opacity").unwrap().param("opacity").unwrap().value, ParamValue::Float(75.0));
    assert!(v2.effect("gaussian_blur").is_some());
    assert_eq!(
        v2.markers.iter().map(|m| (&m.name, m.start, m.duration)).collect::<Vec<_>>(),
        pv2.markers.iter().map(|m| (&m.name, m.start, m.duration)).collect::<Vec<_>>()
    );
    let lvl = qs.audio_tracks[0].items[0].effect("volume").unwrap().param("level").unwrap().value.as_f64().unwrap();
    assert!((lvl + 6.0).abs() < 1e-9);
    // nested sequence survived as a sequence item with its own clip
    let nested = qs.video_tracks[1].items[0].item;
    assert_eq!(q.item(nested).unwrap().name, "Nested");
    assert_eq!(q.sequence(nested).unwrap().video_tracks[0].items[0].source_in, FrameRate::FPS_29_97.tick_of(30));
    // Re-export is stable.
    let (again, _) = export(q, si, Format::Fcp7Xml, &ExportOptions::default()).unwrap();
    let (imp2, _) = import(&again, Format::Fcp7Xml, None).unwrap();
    assert_eq!(structure(&imp2.project, imp2.sequences[0], TrackKind::Video), structure(&p, s, TrackKind::Video));
}

#[test]
fn version_5_and_generators() {
    let (mut p, s) = rich_project();
    let g = generator(&mut p, "Bars", Generator::BarsAndTone);
    let k = generator(&mut p, "Black", Generator::BlackVideo);
    clip(&mut p, s, TrackKind::Video, 2, g, 0, 30, 0);
    clip(&mut p, s, TrackKind::Video, 2, k, 30, 30, 0);
    let o = ExportOptions { xmeml_version: 5, ..Default::default() };
    let (imp, text, _) = roundtrip(&p, s, Format::Fcp7Xml, &o);
    assert!(text.contains("<xmeml version=\"5\">"));
    assert!(text.contains("<effectid>slug</effectid>"));
    let si = only_seq(&imp);
    assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video));
}

// ---------------------------------------------------------------------------------------------
// Property round trip over multi-track timelines with transitions, speed, links, enable flags.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct G {
    track: usize,
    gap: i64,
    dur: i64,
    media: usize,
    src: i64,
    speed: u8,
    enabled: bool,
    tr: u8,
    tr_len: i64,
    audio: bool,
}

fn gen_clip() -> impl Strategy<Value = G> {
    (0usize..3, 0i64..3, 20i64..150, 0usize..3, 0i64..2000, 0u8..5, prop::bool::weighted(0.9), 0u8..6, 1i64..9, any::<bool>()).prop_map(
        |(track, gap, dur, media, src, speed, enabled, tr, tr_len, audio)| G { track, gap: gap * 5, dur, media, src, speed, enabled, tr, tr_len, audio },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn xmeml_roundtrip(clips in prop::collection::vec(gen_clip(), 1..20), ri in 0usize..5) {
        let rate = [FrameRate::FPS_23_976, FrameRate::FPS_25, FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_24][ri];
        let mut p = Project::new("P");
        let ms = [media(&mut p, "/x/one.mov", true, true, rate), media(&mut p, "/x/two.mov", true, true, rate), media(&mut p, "/y/three four.mxf", true, true, rate)];
        let s = sequence(&mut p, "S", rate, false);
        let mut ends = [0i64; 3];
        let mut prev: [Option<filmcraft_project::ClipId>; 3] = [None; 3];
        let mut aends = [0i64; 3];
        for g in &clips {
            let t = ends[g.track] + g.gap;
            let v = clip(&mut p, s, TrackKind::Video, g.track, ms[g.media], t, g.dur, g.src);
            {
                let q = p.sequence_mut(s).unwrap();
                let (_, it) = q.find_item_mut(v).unwrap();
                it.enabled = g.enabled;
                match g.speed {
                    1 => it.speed = 2.0,
                    2 => it.speed = 0.5,
                    3 => it.reverse = true,
                    _ => {}
                }
            }
            let d = g.tr_len;
            let adjacent = g.gap == 0 && prev[g.track].is_some();
            match g.tr {
                0 if adjacent => transition(&mut p, s, TrackKind::Video, g.track, "cross_dissolve", prev[g.track], Some(v), t - d / 2, d, TransitionAlign::CenterAtCut),
                1 if adjacent => transition(&mut p, s, TrackKind::Video, g.track, "dip_to_white", prev[g.track], Some(v), t, d, TransitionAlign::StartAtCut),
                2 if adjacent => transition(&mut p, s, TrackKind::Video, g.track, "push", prev[g.track], Some(v), t - d, d, TransitionAlign::EndAtCut),
                3 if !adjacent => transition(&mut p, s, TrackKind::Video, g.track, "cross_dissolve", None, Some(v), t, d, TransitionAlign::StartAtCut),
                _ => {}
            }
            if g.audio && aends[g.track] <= t {
                let a = clip(&mut p, s, TrackKind::Audio, g.track, ms[g.media], t, g.dur, g.src);
                link(&mut p, s, &[v, a]);
                aends[g.track] = t + g.dur;
            }
            ends[g.track] = t + g.dur;
            prev[g.track] = Some(v);
        }
        let (imp, text, _) = roundtrip(&p, s, Format::Fcp7Xml, &ExportOptions::default());
        let si = only_seq(&imp);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video), "{}", text);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Audio), structure(&p, s, TrackKind::Audio));
        prop_assert_eq!(links(&imp.project, si), links(&p, s));
    }
}

#[test]
fn a_master_clip_used_by_an_earlier_sequence_still_lands_in_its_bin() {
    // Premiere and other writers may put a stringout sequence ahead of the bins that hold its clips.
    let file = format!(
        r#"<file id="file-1"><name>take.mov</name><pathurl>file://localhost/media/take.mov</pathurl>{RATE}<duration>120</duration><media><video><samplecharacteristics><width>1920</width><height>1080</height></samplecharacteristics></video></media></file>"#
    );
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="4"><project><name>Order</name><children>
  <bin><name>Stringouts</name><children>
    <sequence id="sequence-1"><name>All takes</name><duration>120</duration>{RATE}
      <media><video><track><clipitem id="clipitem-1"><masterclipid>masterclip-1</masterclipid><name>take</name>{RATE}
        <start>0</start><end>120</end><in>0</in><out>120</out>{file}</clipitem></track></video></media>
    </sequence>
  </children></bin>
  <bin><name>Footage</name><children><bin><name>Comedy</name><children>
    <clip id="masterclip-1"><name>take</name><duration>120</duration>{RATE}
      <media><video><track><clipitem id="clipitem-2"><masterclipid>masterclip-1</masterclipid><name>take</name>{RATE}<file id="file-1"/></clipitem></track></video></media>
    </clip>
  </children></bin></children></bin>
</children></project></xmeml>"#
    );
    let (imp, _) = import(doc.as_bytes(), Format::Fcp7Xml, None).unwrap();
    let p = &imp.project;
    let mut items = Vec::new();
    p.root.all_items(&mut items);
    let take = items.iter().copied().find(|i| p.item(*i).unwrap().name == "take.mov").unwrap();
    assert_eq!(items.iter().filter(|i| **i == take).count(), 1);
    let bin = p.root.parent_of(take).unwrap();
    assert_ne!(bin, p.root.id, "the take was left loose at the top");
    assert_eq!(p.root.find_bin(bin).unwrap().name, "Comedy");
}

/// `start`/`end`/`in`/`out` of every sequence clip item in an exported document.
fn clip_spans(xml: &str) -> Vec<(i64, i64, i64, i64)> {
    let tag = |block: &str, name: &str| -> i64 {
        let open = format!("<{name}>");
        let at = block.find(&open).unwrap_or_else(|| panic!("no <{name}> in {block}")) + open.len();
        block[at..].split('<').next().unwrap().trim().parse().unwrap()
    };
    xml.split("<clipitem").skip(1).filter(|b| b.contains("<start>")).map(|b| (tag(b, "start"), tag(b, "end"), tag(b, "in"), tag(b, "out"))).collect()
}

/// #340: a clip placed by seconds in a 23.976 sequence has sub-frame start, duration and source in.
/// Its record span and source span were rounded to frames independently, so `out - in` could come
/// out a frame shorter than `end - start` (Premiere then reads a different source range).
#[test]
fn sub_frame_clips_keep_equal_source_and_record_spans() {
    const SECOND: i64 = 254_016_000_000;
    let r = FrameRate::FPS_23_976;
    let mut p = Project::new("P");
    let a = media(&mut p, "/m/a.mov", true, true, r);
    let s = sequence(&mut p, "t", r, false);
    let c = clip(&mut p, s, TrackKind::Video, 0, a, 0, 24, 0);
    // the issue's case first (start 0, source in 20 s, 3 s long), then starts, ins and lengths in tenths of a second
    let mut cases = vec![(0, 200, 30)];
    for start in [0, 1, 5, 17, 33] {
        for src in [0, 3, 7, 200, 413] {
            for dur in [1, 4, 30, 71, 125] {
                cases.push((start, src, dur));
            }
        }
    }
    for (start, src, dur) in cases {
        let (_, it) = p.sequence_mut(s).unwrap().find_item_mut(c).unwrap();
        it.start = Tick(start * SECOND / 10);
        it.source_in = Tick(src * SECOND / 10);
        it.duration = Tick(dur * SECOND / 10);
        let (bytes, _) = export(&p, s, Format::Fcp7Xml, &ExportOptions::default()).expect("export");
        let xml = String::from_utf8(bytes).unwrap();
        let spans = clip_spans(&xml);
        assert_eq!(spans.len(), 1, "{xml}");
        let (start_f, end_f, in_f, out_f) = spans[0];
        assert_eq!(out_f - in_f, end_f - start_f, "start {start}, in {src}, duration {dur} (tenths of a second): {spans:?}");
    }
}
