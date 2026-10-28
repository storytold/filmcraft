//! FCPXML: a hand-written document (from the public FCPXML element reference) with connected
//! clips, a transition, a retimed clip, a gap and a compound clip; plus export → import round trips.

use crate::support;

use filmcraft_interchange::{ExportOptions, FcpxmlVersion, Format, detect, import};
use filmcraft_project::{ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, Param, ParamValue, Project, Sequence, TrackKind, TransitionAlign};
use filmcraft_time::FrameRate;
use proptest::prelude::*;
use support::*;

/// Frame `n` at 23.976 as an FCPXML time, offset by `base` frames.
fn ft(n: i64) -> String {
    format!("{}/24000s", n * 1001)
}

fn doc() -> String {
    let tc = 86_400; // 01:00:00:00 at 23.976 NDF
    let tm_t = format!("{}/24000s", 3610 * 24000 + 48 * 1001);
    let tm_v = format!("{}/24000s", 3610 * 24000 + 96 * 1001);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.10">
  <resources>
    <format id="r1" frameDuration="1001/24000s" width="1920" height="1080"/>
    <asset id="r2" name="Pier" start="0s" duration="{d240}" hasVideo="1" hasAudio="1" format="r1" audioChannels="2" audioRate="48000">
      <media-rep kind="original-media" src="file:///Media/pier%20wide.mov"/>
    </asset>
    <asset id="r3" name="Boats" start="3600s" duration="120s" hasVideo="1" format="r1">
      <media-rep kind="original-media" src="boats.mov"/>
    </asset>
    <asset id="r4" name="Theme" start="0s" duration="60s" hasAudio="1" audioRate="44100">
      <media-rep kind="original-media" src="file:///Media/theme.wav"/>
    </asset>
    <effect id="r5" name="Cross Dissolve" uid="example.dissolve"/>
    <media id="r6" name="Compound">
      <sequence format="r1" tcStart="0s">
        <spine>
          <asset-clip ref="r3" name="Boats" offset="0s" start="3600s" duration="{d48}"/>
        </spine>
      </sequence>
    </media>
  </resources>
  <library>
    <event name="Day 1">
      <project name="Harbour">
        <sequence format="r1" tcStart="{tc0}" tcFormat="NDF" audioRate="48k">
          <spine>
            <asset-clip ref="r2" name="Pier" offset="{tc0}" start="{d24}" duration="{d72}">
              <adjust-transform position="10 -5" scale="1.5 1.5" rotation="90"/>
              <asset-clip ref="r4" lane="-1" name="Theme" offset="{d48}" start="0s" duration="{d96}">
                <adjust-volume amount="-6dB"/>
              </asset-clip>
              <marker start="{d48}" duration="{d1}" value="Seagull" note="nice"/>
            </asset-clip>
            <transition name="Cross Dissolve" offset="{t60}" duration="{d24}">
              <filter-video ref="r5" name="Cross Dissolve"/>
            </transition>
            <asset-clip ref="r3" name="Boats" offset="{t72}" start="3610s" duration="{d48}">
              <timeMap>
                <timept time="3610s" value="3610s" interp="linear"/>
                <timept time="{tm_t}" value="{tm_v}" interp="linear"/>
              </timeMap>
              <asset-clip ref="r2" lane="1" name="Pier Insert" offset="3610s" start="0s" duration="{d24}" srcEnable="video">
                <adjust-blend amount="0.5">
                  <param name="amount">
                    <keyframeAnimation>
                      <keyframe time="0s" value="0"/>
                      <keyframe time="{d12}" value="1"/>
                    </keyframeAnimation>
                  </param>
                </adjust-blend>
              </asset-clip>
            </asset-clip>
            <gap name="Gap" offset="{t120}" start="3600s" duration="{d48}">
              <ref-clip ref="r6" lane="1" name="Compound" offset="3600s" start="0s" duration="{d48}"/>
            </gap>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>
"#,
        tc0 = ft(tc),
        t60 = ft(tc + 60),
        t72 = ft(tc + 72),
        t120 = ft(tc + 120),
        d1 = ft(1),
        d12 = ft(12),
        d24 = ft(24),
        d48 = ft(48),
        d72 = ft(72),
        d96 = ft(96),
        d240 = ft(240),
    )
}

#[test]
fn self_referencing_compound_media_is_ignored_without_recursing() {
    let xml = r#"<fcpxml version="1.10">
      <resources>
        <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
        <media id="r2" name="Loop"><sequence format="r1"><spine>
          <ref-clip ref="r2" offset="0s" start="0s" duration="1s"/>
        </spine></sequence></media>
      </resources>
      <library><event><project name="P"><sequence format="r1"><spine>
        <ref-clip ref="r2" offset="0s" start="0s" duration="1s"/>
      </spine></sequence></project></event></library>
    </fcpxml>"#;
    let (imported, report) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
    assert!(!imported.sequences.is_empty());
    assert!(report.mentions("recursive FCPXML media"), "{report:?}");
}

#[test]
fn golden_connected_clips() {
    let d = doc();
    assert_eq!(detect(d.as_bytes(), Some("fcpxml")), Some(Format::Fcpxml));
    let (imp, rep) = import(d.as_bytes(), Format::Fcpxml, Some("/proj")).unwrap();
    let p = &imp.project;
    let sid = only_seq(&imp);
    assert_eq!(p.item(sid).unwrap().name, "Harbour");
    let s = p.sequence(sid).unwrap();
    let r = FrameRate::FPS_23_976;
    assert_eq!(s.settings.frame_rate, r);
    assert_eq!(s.start_timecode, 86_400);

    assert_connected_primary_video(p, s, r);
    assert_connected_secondary_video(p, s, r);
    assert_connected_audio_markers_and_bins(p, s, r, &rep);
}

fn assert_connected_primary_video(p: &Project, s: &Sequence, r: FrameRate) {
    let v1 = &s.video_tracks[0];
    assert_eq!(v1.items.len(), 2);
    let (pier, boats) = (&v1.items[0], &v1.items[1]);
    assert_eq!((pier.start, pier.duration, pier.source_in), (r.tick_of(0), r.tick_of(72), r.tick_of(24)));
    assert_eq!(media_key(p, pier.item), "/Media/pier wide.mov");
    let pos = pier.effect("motion").unwrap().param("position").unwrap().value.as_vec2().unwrap();
    assert!((pos.x - 1068.0).abs() < 1e-9 && (pos.y - 594.0).abs() < 1e-9, "{pos:?}");
    assert_eq!(pier.effect("motion").unwrap().param("scale").unwrap().value, ParamValue::Float(150.0));
    assert_eq!(pier.effect("motion").unwrap().param("rotation").unwrap().value, ParamValue::Float(-90.0));
    // retimed clip: 2x, source in = 10 s into an asset whose timecode starts at 3600 s
    assert_eq!(boats.speed, 2.0);
    assert_eq!(boats.source_in, filmcraft_time::Tick(10 * filmcraft_time::TICKS_PER_SECOND));
    assert_eq!(media_key(p, boats.item), "/proj/boats.mov");
    let t = &v1.transitions[0];
    assert_eq!((t.start, t.duration, t.from, t.to, t.align), (r.tick_of(60), r.tick_of(24), Some(pier.id), Some(boats.id), TransitionAlign::CenterAtCut));
}

fn assert_connected_secondary_video(p: &Project, s: &Sequence, r: FrameRate) {
    // connected video on lane 1 (V2) with keyframed opacity; compound clip in a gap
    let v2 = &s.video_tracks[1];
    assert_eq!(v2.items.len(), 2);
    assert_eq!((v2.items[0].start, v2.items[0].duration), (r.tick_of(72), r.tick_of(24)));
    let op = v2.items[0].effect("opacity").unwrap().param("opacity").unwrap();
    assert_eq!(op.keyframes.len(), 2);
    assert_eq!(op.keyframes[1].value, ParamValue::Float(100.0));
    assert_eq!(op.keyframes[1].time, r.tick_of(12));
    assert_eq!(v2.items[1].start, r.tick_of(120));
    assert!(matches!(p.item(v2.items[1].item).unwrap().kind, ItemKind::Sequence(_)));
    assert_eq!(p.item(v2.items[1].item).unwrap().name, "Compound");
}

fn assert_connected_audio_markers_and_bins(p: &Project, s: &Sequence, r: FrameRate, rep: &filmcraft_interchange::Report) {
    // audio: pier's own audio on A1 (linked), the lane -1 music pushed to A2 by the overlap
    let a1 = &s.audio_tracks[0].items;
    let pier_link = s.video_tracks[0].items[0].link;
    assert_eq!(a1.len(), 1);
    assert_eq!(a1[0].link, pier_link);
    assert!(pier_link.is_some());
    let theme = &s.audio_tracks[1].items[0];
    assert_eq!((theme.start, theme.duration), (r.tick_of(24), r.tick_of(96)));
    assert_eq!(theme.effect("volume").unwrap().param("level").unwrap().value, ParamValue::Float(-6.0));
    assert!(p.item(theme.item).unwrap().as_media().unwrap().info.video.is_none());
    // the boats asset has no audio: nothing on audio from it
    let boats = s.video_tracks[0].items[1].item;
    assert!(s.audio_tracks.iter().flat_map(|t| &t.items).all(|i| i.item != boats));
    // marker on a primary element is a timeline marker
    assert_eq!(s.markers.len(), 1);
    assert_eq!(
        (s.markers[0].name.as_str(), s.markers[0].comment.as_str(), s.markers[0].start, s.markers[0].duration),
        ("Seagull", "nice", r.tick_of(24), filmcraft_time::Tick::ZERO)
    );
    // event became a bin
    assert!(p.root.children.iter().any(|c| matches!(c, filmcraft_project::BinEntry::Bin(b) if b.name == "Day 1")));
    assert!(!rep.has_warnings(), "{rep}");
    let m = p.items.values().find(|i| i.name == "Boats").unwrap().as_media().unwrap();
    assert!(matches!(&m.media, MediaRef::File { path } if path == "/proj/boats.mov"));
}

#[test]
fn hostile_lane_numbers_do_not_create_sparse_tracks() {
    let d = doc().replace("lane=\"-1\"", "lane=\"-2147483648\"").replace("lane=\"1\"", "lane=\"2147483647\"");
    let (imp, rep) = import(d.as_bytes(), Format::Fcpxml, Some("/proj")).unwrap();
    let s = imp.project.sequence(only_seq(&imp)).unwrap();
    assert!(s.video_tracks.len() <= 256);
    assert!(s.audio_tracks.len() <= 256);
    assert!(rep.has_warnings());
}

fn project() -> (Project, filmcraft_project::ItemId) {
    let r = FrameRate::FPS_29_97;
    let mut p = Project::new("Lib");
    let a = media(&mut p, "/m/a.mov", true, true, r);
    let b = media(&mut p, "/m/b.mov", true, false, r);
    let mus = media(&mut p, "/m/mus.wav", false, true, r);
    let nest = sequence(&mut p, "Nest", r, false);
    clip(&mut p, nest, TrackKind::Video, 0, b, 0..60, 0);
    let s = sequence(&mut p, "Cut", r, true);
    p.sequence_mut(s).unwrap().start_timecode = 107_892;
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0..90, 30);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0..90, 30);
    link(&mut p, s, &[v1, a1]);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, b, 90..90 + (60), 0);
    transition(&mut p, s, (TrackKind::Video, 0), "cross_dissolve", (Some(v1), Some(v2)), 80..80 + (20), TransitionAlign::CenterAtCut);
    let v3 = clip(&mut p, s, TrackKind::Video, 0, a, 200..200 + (40), 500);
    transition(&mut p, s, (TrackKind::Video, 0), "dip_to_black", (None, Some(v3)), 200..200 + (10), TransitionAlign::StartAtCut);
    transition(&mut p, s, (TrackKind::Video, 0), "dip_to_black", (Some(v3), None), 230..230 + (10), TransitionAlign::EndAtCut);
    let up = clip(&mut p, s, TrackKind::Video, 1, nest, 30..30 + (60), 0);
    clip(&mut p, s, TrackKind::Video, 2, b, 160..160 + (30), 10);
    clip(&mut p, s, TrackKind::Audio, 2, mus, 0..260, 0);
    {
        let q = p.sequence_mut(s).unwrap();
        let (_, it) = q.find_item_mut(v2).unwrap();
        it.speed = 2.0;
        let mut sc = Param::new(ParamValue::Float(100.0));
        sc.keyframes.push(Keyframe::new(r.tick_of(0), ParamValue::Float(100.0)));
        sc.keyframes.push(Keyframe::new(r.tick_of(60), ParamValue::Float(120.0)));
        it.effect_mut("motion").unwrap().params.insert("scale".into(), sc);
        it.effect_mut("motion").unwrap().params.insert("rotation".into(), Param::new(ParamValue::Float(15.0)));
        let (_, u) = q.find_item_mut(up).unwrap();
        u.enabled = false;
        u.effect_mut("opacity").unwrap().params.insert("opacity".into(), Param::new(ParamValue::Float(40.0)));
        u.markers.push(Marker {
            id: MarkerId(77),
            start: r.tick_of(10),
            duration: filmcraft_time::Tick::ZERO,
            name: "in nest".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: Label::Blue,
        });
        q.markers.push(Marker {
            id: MarkerId(78),
            start: r.tick_of(100),
            duration: r.tick_of(5),
            name: "chapter".into(),
            comment: "c".into(),
            kind: MarkerKind::Chapter,
            color: Label::Mango,
        });
        let (_, ai) = q.find_item_mut(a1).unwrap();
        ai.effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-3.5)));
    }
    (p, s)
}

#[test]
fn roundtrip_all_versions() {
    for v in [FcpxmlVersion::V1_9, FcpxmlVersion::V1_10, FcpxmlVersion::V1_11] {
        let (p, s) = project();
        let o = ExportOptions { fcpxml_version: v, ..Default::default() };
        let (imp, text, rep) = roundtrip(&p, s, Format::Fcpxml, &o);
        assert!(text.contains(&format!("<fcpxml version=\"{}\">", v.as_str())));
        assert!(text.contains("tcFormat=\"DF\""));
        assert!(!rep.has_warnings(), "{rep}");
        let si = only_seq(&imp);
        let q = &imp.project;
        for kind in [TrackKind::Video, TrackKind::Audio] {
            assert_eq!(structure(q, si, kind), structure(&p, s, kind), "{text}");
        }
        assert_eq!(links(q, si), links(&p, s));
        let qs = q.sequence(si).unwrap();
        let ps = p.sequence(s).unwrap();
        assert_eq!(qs.start_timecode, 107_892);
        assert_eq!(
            qs.markers.iter().map(|m| (&m.name, m.start, m.duration, m.kind)).collect::<Vec<_>>(),
            ps.markers.iter().map(|m| (&m.name, m.start, m.duration, m.kind)).collect::<Vec<_>>()
        );
        let pv = &ps.video_tracks[0].items[1];
        let qv = &qs.video_tracks[0].items[1];
        assert_eq!(qv.effect("motion").unwrap().param("scale"), pv.effect("motion").unwrap().param("scale"), "{text}");
        assert_eq!(qv.effect("motion").unwrap().param("rotation").unwrap().value, ParamValue::Float(15.0));
        let up = &qs.video_tracks[1].items[0];
        assert_eq!(up.effect("opacity").unwrap().param("opacity").unwrap().value, ParamValue::Float(40.0));
        assert_eq!(up.markers.iter().map(|m| (&m.name, m.start)).collect::<Vec<_>>(), vec![(&"in nest".to_string(), FrameRate::FPS_29_97.tick_of(10))]);
        let lvl = qs.audio_tracks[0].items[0].effect("volume").unwrap().param("level").unwrap().value.as_f64().unwrap();
        assert!((lvl + 3.5).abs() < 1e-9);
    }
}

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
    audio: u8,
}

fn gen_clip() -> impl Strategy<Value = G> {
    (0usize..3, 0i64..3, 20i64..150, 0usize..3, 0i64..2000, 0u8..5, prop::bool::weighted(0.9), 0u8..6, 1i64..9, 0u8..3).prop_map(
        |(track, gap, dur, media, src, speed, enabled, tr, tr_len, audio)| G { track, gap: gap * 5, dur, media, src, speed, enabled, tr, tr_len, audio },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn fcpxml_roundtrip(clips in prop::collection::vec(gen_clip(), 1..20), ri in 0usize..5) {
        let rate = [FrameRate::FPS_23_976, FrameRate::FPS_25, FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_24][ri];
        let mut p = Project::new("P");
        let ms = [media(&mut p, "/x/one.mov", true, true, rate), media(&mut p, "/x/two.mov", true, true, rate), media(&mut p, "/y/three four.mxf", true, true, rate)];
        let wav = media(&mut p, "/y/vo.wav", false, true, rate);
        let s = sequence(&mut p, "S", rate, false);
        let mut ends = [0i64; 3];
        let mut prev: [Option<filmcraft_project::ClipId>; 3] = [None; 3];
        let mut aends = [0i64; 3];
        for g in &clips {
            let t = ends[g.track] + g.gap;
            let v = clip(&mut p, s, TrackKind::Video, g.track, ms[g.media], t..t + (g.dur), g.src);
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
            if g.track == 0 {
                match g.tr {
                    0 if adjacent && d >= 2 => transition(&mut p, s, (TrackKind::Video, 0), "cross_dissolve", (prev[0], Some(v)), t - d / 2..t - d / 2 + (d), TransitionAlign::CenterAtCut),
                    1 if adjacent => transition(&mut p, s, (TrackKind::Video, 0), "dip_to_white", (prev[0], Some(v)), t..t + (d), TransitionAlign::StartAtCut),
                    2 if adjacent => transition(&mut p, s, (TrackKind::Video, 0), "push", (prev[0], Some(v)), t - d..t - d + (d), TransitionAlign::EndAtCut),
                    3 if !adjacent => transition(&mut p, s, (TrackKind::Video, 0), "cross_dissolve", (None, Some(v)), t..t + (d), TransitionAlign::StartAtCut),
                    _ => {}
                }
            }
            if aends[g.track] <= t {
                match g.audio {
                    // linked, same-numbered audio track, identical timing
                    1 => {
                        let a = clip(&mut p, s, TrackKind::Audio, g.track, ms[g.media], t..t + (g.dur), g.src);
                        {
                            let q = p.sequence_mut(s).unwrap();
                            let (sp, rv, en) = { let (_, it) = q.find_item(v).unwrap(); (it.speed, it.reverse, it.enabled) };
                            let (_, ai) = q.find_item_mut(a).unwrap();
                            ai.speed = sp;
                            ai.reverse = rv;
                            ai.enabled = en;
                        }
                        link(&mut p, s, &[v, a]);
                        aends[g.track] = t + g.dur;
                    }
                    // independent audio-only clip
                    2 => {
                        clip(&mut p, s, TrackKind::Audio, g.track, wav, t..t + (g.dur), g.src);
                        aends[g.track] = t + g.dur;
                    }
                    _ => {}
                }
            }
            ends[g.track] = t + g.dur;
            prev[g.track] = Some(v);
        }
        let (imp, text, _) = roundtrip(&p, s, Format::Fcpxml, &ExportOptions::default());
        let si = only_seq(&imp);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video), "{}", text);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Audio), structure(&p, s, TrackKind::Audio), "{}", text);
        prop_assert_eq!(links(&imp.project, si), links(&p, s));
    }
}
