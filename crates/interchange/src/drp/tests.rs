//! A hand-written `.drp` in the layout Resolve 20 exports: a project, a Media Pool with a
//! sub-folder, one 30 fps UHD timeline starting at 01:00:00:00 with video, audio, a transition,
//! a title, fades, transform and volume.

use std::panic::{AssertUnwindSafe, catch_unwind};

use filmcraft_project::{Bin, BinEntry, ItemKind, MediaRef, ParamValue, TrackKind, TransitionAlign};
use filmcraft_time::{FrameRate, Tick};

use super::blob::{build, params as P};
use super::{fix_tag_names, sniff, zip};
use crate::{Format, ImportOptions, detect, import, import_with};

const SEQ: &str = "20345951-7336-4d98-8e69-9ca48a155d7d";

fn project_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!--DbAppVer="20.1.0.0020" DbPrjVer="15"-->
<SM_Project DbId="p1">
 <ProjectName>Nordschleife Cut</ProjectName>
 <TimelineVec/>
</SM_Project>"#
        .to_string()
}

fn media_pool_xml() -> String {
    let rate30 = format!("{}0000000000000000", build::f64_le(30.0));
    let ext = format!("{}{}", build::f64_le(3600.0), build::f64_le(20.0));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Sm2MpFolder DbId="master">
 <Name>Master</Name>
 <MediaVec>
  <Element>
   <Sm2MpVideoClip DbId="media-a">
    <Name>A001.mov</Name>
    <MpFolder>master</MpFolder>
    <Video><BtVideoInfo DbId="v1"><Clip>{clip_a}</Clip></BtVideoInfo></Video>
   </Sm2MpVideoClip>
  </Element>
  <Element>
   <Sm2MpVideoClip DbId="media-unused">
    <Name>Unused.mp4</Name>
    <MpFolder>selects</MpFolder>
    <Video><BtVideoInfo DbId="v2"><Clip>{clip_u}</Clip></BtVideoInfo></Video>
   </Sm2MpVideoClip>
  </Element>
  <Element>
   <Sm2MpTimelineClip DbId="tlc">
    <Name>Edit 1</Name>
    <MpFolder>master</MpFolder>
    <TimelineSharedHandle>
     <Sm2Timeline DbId="tl">
      <Name>Edit 1</Name>
      <Sequence>
       <Sm2Sequence DbId="{SEQ}">
        <MediaExtents>{ext}</MediaExtents>
        <FrameRate>{rate30}</FrameRate>
        <Resolution>0000000000000f000000000000000870</Resolution>
        <VideoTrackVec/>
        <AudioTrackVec/>
        <pLmVerTable><ListMgt::LmVersionTable DbId="x"><Locals><Element><ListMgt::LmVersion DbId="y"><HasCorrection>false</HasCorrection></ListMgt::LmVersion></Element></Locals></ListMgt::LmVersionTable></pLmVerTable>
       </Sm2Sequence>
      </Sequence>
     </Sm2Timeline>
    </TimelineSharedHandle>
   </Sm2MpTimelineClip>
  </Element>
 </MediaVec>
 <SubFolderVec>
  <Element>
   <Sm2MpFolder DbId="selects">
    <Name>Selects</Name>
    <MpFolder>master</MpFolder>
   </Sm2MpFolder>
  </Element>
 </SubFolderVec>
</Sm2MpFolder>"#,
        clip_a = build::clip("/Media/Day 1", "A001.mov"),
        clip_u = build::clip("/Media/Day 1", "Unused.mp4"),
    )
}

#[allow(clippy::too_many_arguments)]
fn item(class: &str, name: &str, start: &str, dur: &str, inp: &str, media: &str, path: &str, effects: &str, graded: bool) -> String {
    let rate30 = format!("{}0000000000000000", build::f64_le(30.0));
    format!(
        r#"<Element>
      <{class} DbId="{name}-{start}">
       <PrettyType/>
       <Name>{name}</Name>
       <Start>{start}</Start>
       <Duration>{dur}</Duration>
       <EffectFiltersBA>{effects}</EffectFiltersBA>
       <In>{inp}</In>
       <MediaRef>{media}</MediaRef>
       <MediaFilePath>{path}</MediaFilePath>
       <MediaFrameRate>{rate30}</MediaFrameRate>
       <MediaTimemapBA>024024000000000000</MediaTimemapBA>
       <pLmVerTable>
        <ListMgt::LmVersionTable DbId="t-{name}-{start}">
         <Locals><Element><ListMgt::LmVersion DbId="v-{name}-{start}"><HasCorrection>{graded}</HasCorrection><VerType>0</VerType></ListMgt::LmVersion></Element></Locals>
        </ListMgt::LmVersionTable>
       </pLmVerTable>
      </{class}>
     </Element>"#
    )
}

fn seq_container_xml() -> String {
    let transform = build::effects(&[(P::TRANSFORM, &[(P::ZOOM_X, 1.5), (P::ZOOM_Y, 1.5), (P::POSITION_X, 0.25), (P::POSITION_Y, 0.1), (P::ROTATION, 2.0)])]);
    let fade_out = build::effects(&[(P::VIDEO_FADE, &[(P::VIDEO_FADE_OUT, 15.0)])]);
    let volume = build::effects(&[(P::AUDIO, &[(P::AUDIO_VOLUME_DB, -6.0), (P::AUDIO_FADE_IN, 10.0)])]);
    let title = build::title(&["Nürburgring ", "2026"], "Bebas Neue", "Regular", "#00ffff");
    // 64 frames and 0.064 of one: a 29.97 fps cut point expressed in 30 fps timeline frames.
    let frac_in = format!("64|{}", build::f64_le(0.064));
    let v1 = [
        item("Sm2TiVideoClip", "A001.mov", "108000", "120", &frac_in, "media-a", "/Media/Day 1/A001.mov", &transform, true),
        item("Sm2TiTransition", "Cross Dissolve", "108108", "24", "", "", "", "", false),
        item("Sm2TiVideoClip", "B002.mov", "108120", "90", "30", "", "/Media/Day 1/B002.mov", &fade_out, false),
        item("Sm2TiFusionClip", "Fusion Clip 1", "108300", "30", "", "", "", "", false),
    ]
    .join("\n");
    let v2 = item("Sm2TiGenerator", "Rich", "108030", "60", "", "", "", &title, false);
    let a1 = [
        item("Sm2TiAudioClip", "A001.mov", "108000", "120", &frac_in, "media-a", "/Media/Day 1/A001.mov", &volume, false),
        item("Sm2TiAudioClip", "B002.mov", "108120", "90", "30", "", "/Media/Day 1/B002.mov", "", false),
    ]
    .join("\n");
    let track = |kind: u8, name: &str, items: &str| {
        format!(
            r#"<Element>
   <Sm2TiTrack DbId="track-{kind}-{name}">
    <Type>{kind}</Type>
    <Sequence>{SEQ}</Sequence>
    <Items>
     {items}
    </Items>
    <UserDefinedName>{name}</UserDefinedName>
   </Sm2TiTrack>
  </Element>"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Sm2SequenceContainer DbId="container">
 <VideoTrackVec>
  {v1}
  {v2}
 </VideoTrackVec>
 <AudioTrackVec>
  {a1}
 </AudioTrackVec>
</Sm2SequenceContainer>"#,
        v1 = track(0, "", &v1),
        v2 = track(0, "Titles", &v2),
        a1 = track(1, "Dialogue", &a1),
    )
}

fn drp(deflate: bool) -> Vec<u8> {
    let (p, m, s) = (project_xml(), media_pool_xml(), seq_container_xml());
    zip::write(
        &[
            ("project.xml", p.as_bytes()),
            ("MediaPool/Master/MpFolder.xml", m.as_bytes()),
            (&format!("SeqContainer/{SEQ}.xml"), s.as_bytes()),
            ("Gallery.xml", b"<Gallery/>"),
        ],
        deflate,
    )
}

fn find_bin<'a>(b: &'a Bin, name: &str) -> Option<&'a Bin> {
    if b.name == name {
        return Some(b);
    }
    b.children.iter().find_map(|e| match e {
        BinEntry::Bin(c) => find_bin(c, name),
        _ => None,
    })
}

#[test]
fn detects_drp() {
    let b = drp(true);
    assert!(sniff(&b));
    assert_eq!(detect(&b, Some("drp")), Some(Format::Drp));
    assert_eq!(detect(&b, None), Some(Format::Drp));
    assert_eq!(Format::from_extension(".DRP"), Some(Format::Drp));
    // a ZIP that is not a Resolve project
    let other = zip::write(&[("readme.txt", b"hello")], false);
    assert!(!sniff(&other));
    assert_eq!(detect(&other, Some("zip")), None);
    assert!(!Format::Drp.can_export());
    assert!(Format::ALL.iter().all(|f| f.can_export()));
    assert!(Format::IMPORTABLE.contains(&Format::Drp));
}

#[test]
fn imports_a_timeline() {
    for deflate in [false, true] {
        let (imp, report) = import(&drp(deflate), Format::Drp, Some("/proj")).unwrap();
        let p = &imp.project;
        assert_eq!(p.name, "Nordschleife Cut");
        assert_eq!(imp.sequences.len(), 1);
        let seq = p.sequence(imp.sequences[0]).unwrap();
        assert_eq!(p.item(imp.sequences[0]).unwrap().name, "Edit 1");
        assert_eq!(seq.settings.frame_rate, FrameRate::FPS_30);
        assert_eq!((seq.settings.width, seq.settings.height), (3840, 2160));
        assert_eq!(seq.start_timecode, 108_000);
        seq.check().unwrap();

        let r = FrameRate::FPS_30;
        // V1: two clips, a centred dissolve on the cut, the Fusion clip left as a gap
        assert_eq!(seq.video_tracks.len(), 2);
        assert_eq!(seq.video_tracks[1].name, "Titles");
        assert_eq!(seq.audio_tracks[0].name, "Dialogue");
        let v1 = &seq.video_tracks[0];
        assert_eq!(v1.items.len(), 2);
        let (a, b) = (&v1.items[0], &v1.items[1]);
        assert_eq!((a.start, a.duration), (Tick::ZERO, r.tick_of(120)));
        assert_eq!(a.source_in, r.tick_of(64) + Tick((r.frame_duration().0 as f64 * 0.064).round() as i64));
        assert_eq!((b.start, b.duration, b.source_in), (r.tick_of(120), r.tick_of(90), r.tick_of(30)));
        assert!(a.scale_to_frame);
        let tr = &v1.transitions[0];
        assert_eq!((tr.start, tr.duration, tr.align), (r.tick_of(108), r.tick_of(24), TransitionAlign::CenterAtCut));
        assert_eq!((tr.from, tr.to), (Some(a.id), Some(b.id)));
        assert_eq!(tr.effect.effect, "cross_dissolve");
        // fade out on the last clip
        let fade = v1.transitions.iter().find(|t| t.to.is_none()).unwrap();
        assert_eq!((fade.from, fade.start, fade.duration, fade.align), (Some(b.id), r.tick_of(195), r.tick_of(15), TransitionAlign::EndAtCut));

        // transform → Motion
        let m = a.effect("motion").unwrap();
        assert_eq!(m.param("scale").unwrap().value, ParamValue::Float(150.0));
        assert_eq!(m.param("rotation").unwrap().value, ParamValue::Float(-2.0));
        match m.param("position").unwrap().value {
            ParamValue::Vec2(v) => assert!((v.x - 2880.0).abs() < 1e-6 && (v.y - 864.0).abs() < 1e-6, "{v:?}"),
            ref other => panic!("{other:?}"),
        }

        // media: one item per file, the media pool clip (and the unused one, in its bin) kept
        match &p.item(a.item).unwrap().kind {
            ItemKind::Media(mc) => assert_eq!(mc.media, MediaRef::File { path: "/Media/Day 1/A001.mov".into() }),
            other => panic!("{other:?}"),
        }
        let unused = p.items.values().find(|i| i.name == "Unused.mp4").unwrap();
        let selects = find_bin(&p.root, "Selects").unwrap();
        assert!(selects.children.iter().any(|e| matches!(e, BinEntry::Item(i) if *i == unused.id)));
        assert!(find_bin(&p.root, "Nordschleife Cut").is_some());

        // title → text graphic
        let t = &seq.video_tracks[1].items[0];
        assert_eq!((t.start, t.duration), (r.tick_of(30), r.tick_of(60)));
        assert!(matches!(p.item(t.item).unwrap().kind, ItemKind::Graphic { width: 3840, height: 2160, .. }));
        let layer = t.effects.iter().find(|e| e.effect == filmcraft_project::graphic::TEXT_LAYER).unwrap();
        assert_eq!(layer.param("text").unwrap().value, ParamValue::Text("Nürburgring 2026".into()));
        assert_eq!(layer.param("font").unwrap().value, ParamValue::Text("Bebas Neue".into()));
        assert_eq!(layer.param("fill_color").unwrap().value, ParamValue::Color([0.0, 1.0, 1.0, 1.0]));

        // audio: volume, fade in, linked to the video of the same take
        let a1 = &seq.audio_tracks[0];
        assert_eq!(a1.items.len(), 2);
        assert_eq!(a1.items[0].effect("volume").unwrap().param("level").unwrap().value, ParamValue::Float(-6.0));
        assert!(a1.items[0].link.is_some() && a1.items[0].link == a.link);
        assert!(a1.items[1].link.is_some() && a1.items[1].link == b.link);
        assert_ne!(a.link, b.link);
        let afade = a1.transitions.iter().find(|t| t.from.is_none()).unwrap();
        assert_eq!((afade.to, afade.start, afade.duration), (Some(a1.items[0].id), Tick::ZERO, r.tick_of(10)));
        assert_eq!(afade.effect.effect, "constant_power");

        // what was not imported is reported
        assert!(report.mentions("colour grades are not imported (1 graded clip)"), "{report}");
        assert!(report.mentions("Fusion"), "{report}");
        let _ = TrackKind::Video;
    }
}

#[test]
fn imports_a_lone_timeline_document() {
    // detected as Resolve, not FCP7 XML, from a `.xml` file name
    assert_eq!(crate::detect(seq_container_xml().as_bytes(), Some("xml")), Some(Format::Drp));
    assert_eq!(crate::detect(media_pool_xml().as_bytes(), Some("xml")), Some(Format::Drp));
    assert_eq!(crate::detect(project_xml().as_bytes(), None), Some(Format::Drp));
    // the root is found past a long preamble, and only a Resolve root counts
    let long = format!(
        "<?xml version=\"1.0\"?>\n<!-- {} -->\n{}",
        "x".repeat(8000),
        seq_container_xml().trim_start_matches("<?xml version=\"1.0\" encoding=\"UTF-8\"?>")
    );
    assert_eq!(crate::detect(long.as_bytes(), Some("xml")), Some(Format::Drp));
    assert_eq!(crate::detect(b"<!-- <Sm2MpFolder> --><other/>", Some("xml")), Some(Format::Fcp7Xml));
    assert_eq!(crate::detect(b"<!-- unterminated", Some("xml")), Some(Format::Fcp7Xml));
    let (imp, _) = import_with(seq_container_xml().as_bytes(), Format::Drp, &ImportOptions { name: Some("Loose".into()), ..Default::default() }).unwrap();
    assert_eq!(imp.project.name, "Loose");
    assert_eq!(imp.sequences.len(), 1);
    let seq = imp.project.sequence(imp.sequences[0]).unwrap();
    // no extents: the clips at 01:00:00:00 still start at zero
    assert_eq!(seq.video_tracks[0].items[0].start, Tick::ZERO);
}

#[test]
fn renames_namespaced_tags() {
    assert_eq!(fix_tag_names("<a><ListMgt::Lm x=\"a::b\">t::u</ListMgt::Lm><B::C/></a>"), "<a><ListMgt__Lm x=\"a::b\">t::u</ListMgt__Lm><B__C/></a>");
    assert_eq!(fix_tag_names("<?xml version=\"1.0\"?><!--a::b--><r/>"), "<?xml version=\"1.0\"?><!--a::b--><r/>");
}

#[test]
fn rejects_what_is_not_a_project() {
    assert!(import(b"", Format::Drp, None).is_err());
    assert!(import(b"<x/>", Format::Drp, None).is_err());
    assert!(import(&zip::write(&[("readme.txt", b"hi")], false), Format::Drp, None).is_err());
    assert!(import(&zip::write(&[("project.xml", b"<SM_Project/>")], true), Format::Drp, None).is_err());
}

#[test]
fn hostile_archives_never_panic() {
    let seeds = [drp(false), drp(true)];
    for seed in seeds {
        let step = (seed.len() / 300).max(1);
        for cut in (0..seed.len()).step_by(step) {
            let r = catch_unwind(AssertUnwindSafe(|| import(&seed[..cut], Format::Drp, None)));
            assert!(r.is_ok(), "panic on truncation at {cut}");
        }
        for i in (0..seed.len()).step_by(step) {
            for bit in [0x01u8, 0x10, 0x80, 0xff] {
                let mut m = seed.clone();
                m[i] ^= bit;
                let r = catch_unwind(AssertUnwindSafe(|| import(&m, Format::Drp, None)));
                assert!(r.is_ok(), "panic on flip {bit:#x} at {i}");
            }
        }
    }
    // hostile numbers in an otherwise valid document
    for (from, to) in [("108120", "-99999999999"), ("108120", "9223372036854775807"), ("<Duration>90", "<Duration>-5"), ("1.5", "1e308")] {
        let doc = seq_container_xml().replacen(from, to, 1);
        let r = catch_unwind(AssertUnwindSafe(|| import(doc.as_bytes(), Format::Drp, None)));
        assert!(r.is_ok(), "panic with {to}");
    }
}

/// Compound, multicam and Fusion clips, in one XML document.
fn nested_xml() -> String {
    let r30 = format!("{}0000000000000000", build::f64_le(30.0));
    let ext = |s: f64| format!("{}{}", build::f64_le(s), build::f64_le(60.0));
    let seq =
        |id: &str, start: f64| format!(r#"<Sm2Sequence DbId="{id}"><FrameRate>{r30}</FrameRate><MediaExtents>{}</MediaExtents></Sm2Sequence>"#, ext(start));
    let clip = |class: &str, name: &str, start: i64, dur: i64, inp: i64, extra: &str| {
        format!(
            r#"<Element><{class} DbId="{name}-{start}"><Name>{name}</Name><Start>{start}</Start><Duration>{dur}</Duration><In>{inp}</In>{extra}</{class}></Element>"#
        )
    };
    let track = |seq: &str, name: &str, items: &[String]| {
        format!(
            r#"<Element><Sm2TiTrack DbId="t-{seq}-{name}"><Sequence>{seq}</Sequence><Items>{}</Items><UserDefinedName>{name}</UserDefinedName></Sm2TiTrack></Element>"#,
            items.concat()
        )
    };
    let media = |p: &str| format!("<MediaFilePath>{p}</MediaFilePath><MediaFrameRate>{r30}</MediaFrameRate>");
    let main_v1 = [
        clip("Sm2TiVideoClip", "Compound Clip 1", 108000, 60, 10, "<MediaRef>cc</MediaRef>"),
        clip("Sm2TiVideoClip", "Show Multicam", 108060, 30, 100, &format!("<MediaRef>mc</MediaRef><FieldsBlob>{}</FieldsBlob>", build::angle("Camera 2"))),
        clip(
            "Sm2TiVideoClip",
            "Fusion Title",
            108090,
            30,
            0,
            &format!(
                "<CompositionTable><Sm2TiCompositionTable><CompositionBA>{}</CompositionBA></Sm2TiCompositionTable></CompositionTable>",
                build::fusion("Sequoia", "Brush Script MT")
            ),
        ),
        clip("Sm2TiVideoClip", "Fusion Composition", 108120, 30, 0, ""),
    ];
    format!(
        r#"<Doc>
<SM_Project><ProjectName>Konzert</ProjectName></SM_Project>
<Sm2MpFolder DbId="m"><Name>Master</Name></Sm2MpFolder>
<Sm2MpCompoundClip DbId="cc"><Name>Compound Clip 1</Name><MpFolder>m</MpFolder><Sequence>cseq</Sequence></Sm2MpCompoundClip>
<Sm2MpCompoundClip DbId="cc2"><Name>Unused Compound</Name><MpFolder>m</MpFolder><Sequence>cseq2</Sequence></Sm2MpCompoundClip>
<Sm2MpMulticamClip DbId="mc"><Name>Show Multicam</Name><MpFolder>m</MpFolder><Sequence>mseq</Sequence></Sm2MpMulticamClip>
<Sm2MpTimelineClip DbId="tc"><Name>Main</Name><MpFolder>m</MpFolder><TimelineSharedHandle><Sm2Timeline DbId="tl"><Name>Main</Name><Sequence>{main}</Sequence></Sm2Timeline></TimelineSharedHandle></Sm2MpTimelineClip>
{cseq}{mseq}{cseq2}
<Sm2SequenceContainer>
<VideoTrackVec>{v_main}{v_c}{v_m1}{v_m2}{v_c2}</VideoTrackVec>
<AudioTrackVec/>
</Sm2SequenceContainer>
</Doc>"#,
        main = seq("main", 3600.0),
        cseq = seq("cseq", 0.0),
        mseq = seq("mseq", 3600.0),
        cseq2 = seq("cseq2", 0.0),
        v_main = track("main", "", &main_v1),
        v_c = track("cseq", "", &[clip("Sm2TiVideoClip", "x.mov", 0, 100, 0, &media("/m/x.mov"))]),
        v_m1 = track("mseq", "Camera 1", &[clip("Sm2TiVideoClip", "a.mov", 108000, 1000, 0, &media("/m/a.mov"))]),
        v_c2 = track("cseq2", "", &[clip("Sm2TiVideoClip", "y.mov", 0, 50, 0, &media("/m/y.mov"))]),
        v_m2 = track("mseq", "Camera 2", &[clip("Sm2TiVideoClip", "b.mov", 108050, 1000, 20, &media("/m/b.mov"))]),
    )
}

#[test]
fn imports_compound_multicam_and_fusion_clips() {
    let (imp, report) = import(nested_xml().as_bytes(), Format::Drp, None).unwrap();
    let p = &imp.project;
    // the compound clip's timeline is a sequence of its own, not a top-level timeline
    assert_eq!(imp.sequences.len(), 1, "{report}");
    let seq = p.sequence(imp.sequences[0]).unwrap();
    seq.check().unwrap();
    let r = FrameRate::FPS_30;
    let v1 = &seq.video_tracks[0];
    assert_eq!(v1.items.len(), 4, "{report}");

    let compound = &v1.items[0];
    let nested = p.sequence(compound.item).expect("a nested sequence");
    assert_eq!(p.item(compound.item).unwrap().name, "Compound Clip 1");
    // an unused compound clip is still a sequence in the project
    assert!(p.items.values().any(|i| i.name == "Unused Compound" && i.as_sequence().is_some()));
    assert_eq!(nested.video_tracks[0].items[0].name, "x.mov");
    assert_eq!((compound.start, compound.duration, compound.source_in), (Tick::ZERO, r.tick_of(60), r.tick_of(10)));

    // multicam: Camera 2 at 100 frames into the multicam timeline = b.mov from frame 20 + 50
    let mc = &v1.items[1];
    match &p.item(mc.item).unwrap().kind {
        ItemKind::Media(m) => assert_eq!(m.media, MediaRef::File { path: "/m/b.mov".into() }),
        other => panic!("{other:?}"),
    }
    assert_eq!((mc.start, mc.source_in), (r.tick_of(60), r.tick_of(70)));
    assert!(report.mentions("flattened"), "{report}");

    // Fusion title with text; a Fusion clip without one is a placeholder
    let title = &v1.items[2];
    let layer = title.effects.iter().find(|e| e.effect == filmcraft_project::graphic::TEXT_LAYER).unwrap();
    assert_eq!(layer.param("text").unwrap().value, ParamValue::Text("Sequoia".into()));
    assert_eq!(layer.param("size").unwrap().value, ParamValue::Float(0.09 * 1920.0));
    assert!(matches!(&p.item(v1.items[3].item).unwrap().kind, ItemKind::Media(m) if !matches!(m.media, MediaRef::File { .. })));
    assert!(report.mentions("Fusion clip \"Fusion Composition\""), "{report}");
}

#[test]
fn a_compound_clip_inside_itself_is_a_gap() {
    let doc = nested_xml().replace("<Sequence>cseq</Sequence></Sm2MpCompoundClip>", "<Sequence>main</Sequence></Sm2MpCompoundClip>");
    let r = catch_unwind(AssertUnwindSafe(|| import(doc.as_bytes(), Format::Drp, None)));
    let (imp, report) = r.expect("no panic").unwrap();
    assert!(report.mentions("contains itself"), "{report}");
    imp.project.sequence(imp.sequences[0]).unwrap().check().unwrap();
}

#[test]
fn overlapping_clips_beyond_the_track_limit_are_skipped() {
    // 300 clips stacked at one time on a single Resolve track: each overlap moves to the next
    // free track, until the import limit, where the rest are reported and skipped.
    let items: String =
        (0..300).map(|i| item("Sm2TiVideoClip", &format!("C{i:03}.mov"), "108000", "30", "0", "", &format!("/Media/C{i:03}.mov"), "", false)).collect();
    let doc = format!(
        r#"<Sm2SequenceContainer DbId="container"><VideoTrackVec><Element><Sm2TiTrack DbId="t"><Type>0</Type><Sequence>{SEQ}</Sequence><Items>{items}</Items></Sm2TiTrack></Element></VideoTrackVec></Sm2SequenceContainer>"#
    );
    let (imp, report) = import_with(doc.as_bytes(), Format::Drp, &ImportOptions::default()).unwrap();
    let seq = imp.project.sequence(imp.sequences[0]).unwrap();
    assert!(seq.video_tracks.len() <= crate::common::MAX_IMPORTED_TRACKS);
    let placed: usize = seq.video_tracks.iter().map(|t| t.items.len()).sum();
    assert!(placed < 300 && placed > 0, "{placed}");
    assert!(report.mentions("track import limit"), "{report}");
}

#[test]
fn deeply_nested_compound_clips_stop_at_the_limit() {
    // a chain of distinct compound clips, each holding the next: without a depth limit the walk
    // recurses once per level and overflows the stack (an abort, not a catchable panic)
    const LEVELS: usize = 5000;
    let r30 = format!("{}0000000000000000", build::f64_le(30.0));
    let mut pool = String::new();
    let mut seqs = String::new();
    let mut tracks = String::new();
    for i in 0..LEVELS {
        pool.push_str(&format!(r#"<Sm2MpCompoundClip DbId="cc{i}"><Name>C{i}</Name><MpFolder>m</MpFolder><Sequence>s{i}</Sequence></Sm2MpCompoundClip>"#));
        seqs.push_str(&format!(r#"<Sm2Sequence DbId="s{i}"><FrameRate>{r30}</FrameRate></Sm2Sequence>"#));
        let next = i + 1;
        tracks.push_str(&format!(
            r#"<Element><Sm2TiTrack DbId="t{i}"><Sequence>s{i}</Sequence><Items><Element><Sm2TiVideoClip DbId="c{i}"><Name>C{next}</Name><Start>0</Start><Duration>30</Duration><In>0</In><MediaRef>cc{next}</MediaRef></Sm2TiVideoClip></Element></Items></Sm2TiTrack></Element>"#
        ));
    }
    let doc = format!(
        r#"<Doc><Sm2MpFolder DbId="m"><Name>Master</Name></Sm2MpFolder>{pool}<Sm2MpTimelineClip DbId="tc"><Name>Main</Name><MpFolder>m</MpFolder><TimelineSharedHandle><Sm2Timeline DbId="tl"><Name>Main</Name><Sequence>s0</Sequence></Sm2Timeline></TimelineSharedHandle></Sm2MpTimelineClip>{seqs}<Sm2SequenceContainer><VideoTrackVec>{tracks}</VideoTrackVec><AudioTrackVec/></Sm2SequenceContainer></Doc>"#
    );
    let (imp, report) = import(doc.as_bytes(), Format::Drp, None).unwrap();
    assert!(report.mentions("compound clips deep"), "{report}");
    assert!(!imp.sequences.is_empty());
}
