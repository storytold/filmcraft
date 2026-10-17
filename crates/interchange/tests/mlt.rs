//! Kdenlive projects (MLT XML, #1089): synthetic `.kdenlive` documents written for these tests,
//! shaped like the files Kdenlive writes (the main one follows the snippet in the issue: a
//! `main_bin` playlist, a black background track, a tractor of two playlists per track flagged
//! `kdenlive:audio_track`, internal track filters). Nothing here comes from MLT or Kdenlive
//! source.

use std::panic::{AssertUnwindSafe, catch_unwind};

use filmcraft_interchange::{Error, ExportOptions, Format, Imported, Report, detect, export, import};
use filmcraft_media::Generator;
use filmcraft_project::{ItemKind, MediaRef, Sequence, TrackItem};
use filmcraft_time::FrameRate;

/// Two video tracks and one audio track over a black background, the sequence wrapped in a
/// project tractor. At 30 fps:
///
/// - V1 "Picture": River (frames 30–119 of `footage/river.mp4`) at 0, a 30-frame blank, City
///   (frames 30–89 of `footage/city.mov`, which carries a fade-in effect) at 120;
/// - V2 (locked): a 2 s blank, a red colour clip at 60 for 30 frames, a blank, the title
///   "Opening" at 120 for 45 frames;
/// - A1 (muted): the sound of River at 0 (the same file through its own chain), a 1 s blank, the
///   music `/library/music/theme.wav` (SMPTE in/out 00:00:00:00–00:00:04:29) at 120;
/// - a wipe between V1 and V2, two guides, a bin folder, and `unused.mp4` only in the bin.
const PROJECT: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<mlt LC_NUMERIC="C" producer="main_bin" root="/projects/river" version="7.37.0">
 <profile colorspace="709" description="HD 1080p 30 fps" display_aspect_den="9" display_aspect_num="16" frame_rate_den="1" frame_rate_num="30" height="1080" progressive="1" sample_aspect_den="1" sample_aspect_num="1" width="1920"/>
 <producer id="producer0" in="00:00:00.000" out="00:06:06.100">
  <property name="length">2147483647</property>
  <property name="eof">continue</property>
  <property name="resource">black</property>
  <property name="aspect_ratio">1</property>
  <property name="mlt_service">color</property>
  <property name="kdenlive:playlistid">black_track</property>
  <property name="mlt_image_format">rgba</property>
  <property name="set.test_audio">0</property>
 </producer>
 <playlist id="black_track">
  <entry producer="producer0" in="00:00:00.000" out="00:00:09.966"/>
 </playlist>
 <chain id="chain0" out="00:00:09.966">
  <property name="length">300</property>
  <property name="eof">pause</property>
  <property name="resource">footage/river.mp4</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="video_index">0</property>
  <property name="audio_index">1</property>
  <property name="kdenlive:clipname">River</property>
  <property name="kdenlive:id">2</property>
  <property name="kdenlive:folderid">3</property>
  <property name="kdenlive:markers">[{"comment":"kingfisher","pos":45,"type":2}]</property>
 </chain>
 <chain id="chain1" out="00:00:14.966">
  <property name="length">450</property>
  <property name="resource">/library/music/theme.wav</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="video_index">-1</property>
  <property name="audio_index">0</property>
  <property name="kdenlive:id">3</property>
 </chain>
 <chain id="chain2" out="00:00:04.966">
  <property name="length">150</property>
  <property name="resource">footage/city.mov</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="kdenlive:clipname">City</property>
  <property name="kdenlive:id">4</property>
  <filter id="filter3" out="00:00:00.966">
   <property name="mlt_service">brightness</property>
   <property name="kdenlive_id">fadein</property>
   <property name="kdenlive:collapsed">0</property>
  </filter>
 </chain>
 <producer id="producer1" in="00:00:00.000" out="00:00:04.966">
  <property name="length">2147483647</property>
  <property name="resource">0xff0000ff</property>
  <property name="mlt_service">color</property>
  <property name="kdenlive:clipname">Red</property>
  <property name="kdenlive:id">5</property>
 </producer>
 <producer id="producer2" in="00:00:00.000" out="00:00:04.966">
  <property name="length">150</property>
  <property name="resource"/>
  <property name="mlt_service">kdenlivetitle</property>
  <property name="kdenlive:clipname">Opening</property>
  <property name="kdenlive:id">6</property>
 </producer>
 <chain id="chain3" out="00:00:01.966">
  <property name="length">60</property>
  <property name="resource">footage/unused.mp4</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="kdenlive:id">7</property>
 </chain>
 <chain id="chain4" out="00:00:09.966">
  <property name="length">300</property>
  <property name="resource">footage/river.mp4</property>
  <property name="mlt_service">avformat-novalidate</property>
  <property name="set.test_image">1</property>
  <property name="kdenlive:clipname">River</property>
  <property name="kdenlive:id">2</property>
 </chain>
 <playlist id="playlist0">
  <property name="kdenlive:audio_track">1</property>
  <entry producer="chain4" in="30" out="119"/>
  <blank length="00:00:01.000"/>
  <entry producer="chain1" in="00:00:00:00" out="00:00:04:29"/>
 </playlist>
 <playlist id="playlist1">
  <property name="kdenlive:audio_track">1</property>
 </playlist>
 <tractor id="tractor0" in="00:00:00.000">
  <property name="kdenlive:audio_track">1</property>
  <property name="kdenlive:trackheight">72</property>
  <property name="kdenlive:timeline_active">1</property>
  <property name="kdenlive:collapsed">0</property>
  <property name="kdenlive:thumbs_format"/>
  <property name="kdenlive:audio_rec"/>
  <track hide="video" producer="playlist0"/>
  <track hide="video" producer="playlist1"/>
  <filter id="filter0">
   <property name="window">75</property>
   <property name="max_gain">20dB</property>
   <property name="channel_mask">-1</property>
   <property name="mlt_service">volume</property>
   <property name="internal_added">237</property>
   <property name="disable">1</property>
  </filter>
 </tractor>
 <playlist id="playlist2">
  <entry producer="chain0" in="00:00:01.000" out="00:00:03.966"/>
  <blank length="30"/>
  <entry producer="chain2" in="00:00:01.000" out="00:00:02.966"/>
 </playlist>
 <playlist id="playlist3"/>
 <tractor id="tractor1" in="00:00:00.000">
  <property name="kdenlive:trackheight">72</property>
  <property name="kdenlive:track_name">Picture</property>
  <track hide="audio" producer="playlist2"/>
  <track hide="audio" producer="playlist3"/>
 </tractor>
 <playlist id="playlist4">
  <blank length="00:00:02.000"/>
  <entry producer="producer1" in="0" out="29"/>
  <blank length="30"/>
  <entry producer="producer2" in="00:00:00.000" out="00:00:01.466"/>
 </playlist>
 <playlist id="playlist5"/>
 <tractor id="tractor2" in="00:00:00.000">
  <property name="kdenlive:trackheight">72</property>
  <property name="kdenlive:locked_track">1</property>
  <track hide="audio" producer="playlist4"/>
  <track hide="audio" producer="playlist5"/>
 </tractor>
 <tractor id="tractor3" in="00:00:00.000" out="00:00:08.966">
  <property name="kdenlive:clipname">Main Edit</property>
  <property name="kdenlive:uuid">{9a0e1c62-0000-4000-8000-000000000001}</property>
  <property name="kdenlive:sequenceproperties.guides">[{"comment":"Act 1","pos":0,"type":0},{"comment":"Act 2","pos":120,"type":0}]</property>
  <track producer="black_track"/>
  <track producer="tractor0" hide="both"/>
  <track producer="tractor1"/>
  <track producer="tractor2"/>
  <transition id="transition0">
   <property name="a_track">0</property>
   <property name="b_track">1</property>
   <property name="mlt_service">mix</property>
   <property name="kdenlive_id">mix</property>
   <property name="internal_added">237</property>
   <property name="always_active">1</property>
  </transition>
  <transition id="transition1">
   <property name="a_track">0</property>
   <property name="b_track">2</property>
   <property name="mlt_service">qtblend</property>
   <property name="internal_added">237</property>
   <property name="always_active">1</property>
  </transition>
  <transition id="transition2" in="00:00:02.000" out="00:00:02.966">
   <property name="a_track">2</property>
   <property name="b_track">3</property>
   <property name="mlt_service">luma</property>
   <property name="kdenlive_id">wipe</property>
  </transition>
  <filter id="filter5">
   <property name="mlt_service">volume</property>
   <property name="internal_added">237</property>
  </filter>
 </tractor>
 <playlist id="main_bin">
  <property name="kdenlive:folder.-1.3">Selects</property>
  <property name="kdenlive:docproperties.guides">[{"comment":"Act 1","pos":0,"type":0},{"comment":"Act 2","pos":120,"type":0}]</property>
  <property name="xml_retain">1</property>
  <entry producer="chain0" in="00:00:00.000" out="00:00:09.966"/>
  <entry producer="chain1" in="00:00:00.000" out="00:00:14.966"/>
  <entry producer="chain2" in="00:00:00.000" out="00:00:04.966"/>
  <entry producer="producer1" in="00:00:00.000" out="00:00:04.966"/>
  <entry producer="producer2" in="00:00:00.000" out="00:00:04.966"/>
  <entry producer="chain3" in="00:00:00.000" out="00:00:01.966"/>
  <entry producer="tractor3" in="00:00:00.000" out="00:00:08.966"/>
 </playlist>
 <tractor id="tractor4" in="00:00:00.000" out="00:00:08.966">
  <property name="kdenlive:projectTractor">1</property>
  <track producer="tractor3"/>
 </tractor>
</mlt>
"#;

fn import_mlt(text: &str) -> (Imported, Report) {
    import(text.as_bytes(), Format::Mlt, Some("/somewhere/else")).unwrap()
}

fn only_sequence(imp: &Imported) -> &Sequence {
    assert_eq!(imp.sequences.len(), 1);
    imp.project.sequence(imp.sequences[0]).unwrap()
}

/// `(clip name, start frame, duration in frames, source in frame)` of a track's clips.
fn clips(items: &[TrackItem], rate: FrameRate) -> Vec<(String, i64, i64, i64)> {
    items.iter().map(|c| (c.name.clone(), rate.frame_at(c.start), rate.frame_at(c.end()) - rate.frame_at(c.start), rate.frame_at(c.source_in))).collect()
}

fn media_path(imp: &Imported, c: &TrackItem) -> String {
    match &imp.project.item(c.item).unwrap().kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => path.clone(),
            MediaRef::Generator(g) => format!("{g:?}"),
        },
        other => panic!("not media: {other:?}"),
    }
}

fn c(name: &str, start: i64, dur: i64, src: i64) -> (String, i64, i64, i64) {
    (name.to_string(), start, dur, src)
}

#[test]
fn kdenlive_project_two_video_tracks_one_audio_track() {
    let (imp, report) = import_mlt(PROJECT);
    let seq = only_sequence(&imp);
    let seq_item = imp.project.item(imp.sequences[0]).unwrap();
    assert_eq!(seq_item.name, "Main Edit");
    // the profile
    let rate = FrameRate::FPS_30;
    assert_eq!(seq.settings.frame_rate, rate);
    assert_eq!((seq.settings.width, seq.settings.height, seq.settings.par), (1920, 1080, (1, 1)));
    // the black background is not a track; tractor0 is audio (kdenlive:audio_track)
    assert_eq!((seq.video_tracks.len(), seq.audio_tracks.len()), (2, 1), "{report}");

    let v1 = &seq.video_tracks[0];
    assert_eq!(v1.name, "Picture");
    assert!(v1.enabled && !v1.locked);
    assert_eq!(clips(&v1.items, rate), vec![c("River", 0, 90, 30), c("City", 120, 60, 30)]);
    assert_eq!(media_path(&imp, &v1.items[0]), "/projects/river/footage/river.mp4", "relative to <mlt root>");
    assert_eq!(media_path(&imp, &v1.items[1]), "/projects/river/footage/city.mov");

    let v2 = &seq.video_tracks[1];
    assert_eq!(v2.name, "Video 2");
    assert!(v2.locked, "kdenlive:locked_track");
    // the title at 120 is a gap
    assert_eq!(clips(&v2.items, rate), vec![c("Red", 60, 30, 0)]);
    assert_eq!(media_path(&imp, &v2.items[0]), format!("{:?}", Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }));

    let a1 = &seq.audio_tracks[0];
    assert_eq!(a1.name, "Audio 1");
    assert!(a1.muted, "hide=\"both\" on an audio track mutes it");
    assert_eq!(clips(&a1.items, rate), vec![c("River", 0, 90, 30), c("theme.wav", 120, 150, 0)]);
    assert_eq!(media_path(&imp, &a1.items[1]), "/library/music/theme.wav");

    // both halves of River: one media item, linked; the music is not linked to anything
    assert_eq!(a1.items[0].item, v1.items[0].item);
    assert!(v1.items[0].link.is_some());
    assert_eq!(a1.items[0].link, v1.items[0].link);
    assert_eq!(a1.items[1].link, None);
    assert_eq!(v1.items[1].link, None);

    // media: every file once (the bin's unused clip too), the red colour; no title
    let mut names: Vec<&str> = imp.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Red", "city.mov", "river.mp4", "theme.wav", "unused.mp4"]);
    let river = imp.project.item(v1.items[0].item).and_then(|i| i.as_media()).unwrap();
    assert!(river.info.video.is_some() && !river.info.audio_streams.is_empty());
    assert_eq!(river.info.duration, rate.tick_of(300), "the producer's length");
    let theme = imp.project.item(a1.items[1].item).and_then(|i| i.as_media()).unwrap();
    assert!(theme.info.video.is_none(), "video_index -1");

    // what was not imported is reported, Kdenlive's internal track plumbing is not
    for needle in [
        "transition \"wipe\" was not imported",
        "effect (filter) \"fadein\" was not imported",
        "title clip \"Opening\" at frame 120 was not imported",
        "title clip \"Opening\" in the bin was not imported",
        "2 timeline guide(s) were not imported",
        "the markers of 1 clip(s) were not imported",
        "1 bin folder(s) were not imported",
    ] {
        assert!(report.mentions(needle), "{needle:?} missing from:\n{report}");
    }
    for plumbing in ["mix", "qtblend", "volume"] {
        assert!(!report.mentions(&format!("\"{plumbing}\"")), "{plumbing} is reported:\n{report}");
    }
    assert_eq!(report.warnings().count(), 7, "{report}");
}

/// A document without Kdenlive properties: no profile (MLT's default, DV PAL), tracks as plain
/// playlists in a `<multitrack>`, audio by `hide="video"`, entries without in/out.
#[test]
fn plain_mlt_document() {
    let doc = r#"<mlt>
 <producer id="bg"><property name="mlt_service">color</property><property name="resource">#ff000000</property></producer>
 <producer id="a"><property name="resource">/m/a.mp4</property><property name="length">100</property></producer>
 <producer id="b" in="5" out="54"><property name="resource">/m/b.wav</property></producer>
 <playlist id="background"><entry producer="bg" in="0" out="99"/></playlist>
 <playlist id="v1"><entry producer="a"/></playlist>
 <playlist id="v2"><blank length="10"/><entry producer="a" in="10" out="19"/></playlist>
 <playlist id="a1"><entry producer="b"/></playlist>
 <playlist id="a2"><blank length="1:00.000"/><entry producer="b" in="00:00:00:00" out="00:00:00:24"/></playlist>
 <tractor id="t">
  <multitrack>
   <track producer="background"/>
   <track producer="v1"/>
   <track producer="v2" hide="both"/>
   <track producer="a1" hide="video"/>
   <track producer="a2" hide="video"/>
  </multitrack>
 </tractor>
</mlt>"#;
    let (imp, report) = import_mlt(doc);
    let seq = only_sequence(&imp);
    let rate = FrameRate::FPS_25;
    assert_eq!(seq.settings.frame_rate, rate);
    assert_eq!((seq.settings.width, seq.settings.height, seq.settings.par), (720, 576, (59, 54)));
    assert!(report.mentions("no <profile>"), "{report}");
    assert!(!report.has_warnings(), "{report}");
    assert_eq!(imp.project.item(imp.sequences[0]).unwrap().name, "Timeline");
    assert_eq!((seq.video_tracks.len(), seq.audio_tracks.len()), (2, 2));
    // an entry without in/out plays its producer from 0 to length - 1
    assert_eq!(clips(&seq.video_tracks[0].items, rate), vec![c("a.mp4", 0, 100, 0)]);
    assert_eq!(clips(&seq.video_tracks[1].items, rate), vec![c("a.mp4", 10, 10, 10)]);
    assert!(seq.video_tracks[0].enabled && !seq.video_tracks[1].enabled, "hide=\"both\" turns a video track off");
    // ... or its producer's own in/out; outside Kdenlive the audio tracks keep the document's order
    assert_eq!(clips(&seq.audio_tracks[0].items, rate), vec![c("b.wav", 0, 50, 5)]);
    assert_eq!(clips(&seq.audio_tracks[1].items, rate), vec![c("b.wav", 1500, 25, 0)]);
    assert!(!seq.audio_tracks[0].muted);
    // the first track, all opaque black (`#AARRGGBB`), was the background: no media for it
    assert!(imp.project.items.values().filter_map(|i| i.as_media()).all(|m| matches!(m.media, MediaRef::File { .. })));
}

/// Media that cannot be found or used leaves a gap and a report entry; the rest of the track
/// keeps its timing.
#[test]
fn missing_media_and_unusable_entries() {
    let doc = r#"<?xml version="1.0"?>
<mlt producer="main_bin">
 <profile width="1280" height="720" frame_rate_num="25" frame_rate_den="1"/>
 <chain id="c0"><property name="resource">/m/a.mp4</property><property name="kdenlive:clipname">A</property></chain>
 <chain id="c1"><property name="mlt_service">avformat</property></chain>
 <producer id="tw"><property name="mlt_service">timewarp</property><property name="resource">2:/m/a.mp4</property><property name="kdenlive:clipname">Fast</property></producer>
 <producer id="nest"><property name="resource">&lt;tractor&gt;</property><property name="kdenlive:clipname">Nest</property></producer>
 <playlist id="main_bin"><entry producer="c0"/><entry producer="c1"/></playlist>
 <playlist id="p0">
  <entry producer="ghost" in="0" out="24"/>
  <entry producer="c1" in="0" out="9"/>
  <entry producer="tw" in="0" out="9"/>
  <entry producer="c0" in="0" out="24"/>
  <entry producer="c0" in="10" out="5"/>
  <entry producer="c0"/>
  <blank length="soon"/>
  <entry producer="nest" in="0" out="4"/>
  <entry producer="c0" in="0" out="4"/>
 </playlist>
 <tractor id="t0"><track producer="p0"/><track producer="nowhere"/></tractor>
</mlt>"#;
    let (imp, report) = import_mlt(doc);
    let seq = only_sequence(&imp);
    let rate = FrameRate::FPS_25;
    assert_eq!(clips(&seq.video_tracks[0].items, rate), vec![c("A", 45, 25, 0), c("A", 75, 5, 0)], "{report}");
    for needle in [
        "a clip at frame 0 plays \"ghost\", which the document does not define: left as a gap",
        "clip without a media file \"c1\" at frame 25 was not imported",
        "clip without a media file \"c1\" in the bin was not imported",
        "speed-changed clip (timewarp) \"Fast\" at frame 35 was not imported",
        "a clip at frame 70 ends (out 5) before it starts (in 10) and was skipped",
        "a clip at frame 70 has no out point and was skipped",
        "a blank at frame 70 has no valid length",
        "nested timeline \"Nest\" at frame 70 was not imported",
        "a track plays \"nowhere\", which the document does not define",
    ] {
        assert!(report.mentions(needle), "{needle:?} missing from:\n{report}");
    }
}

#[test]
fn malformed_documents_are_errors() {
    let err = |text: &str| import(text.as_bytes(), Format::Mlt, None).unwrap_err();
    assert!(matches!(err("this is not XML"), Error::Parse { .. }));
    assert!(matches!(err("<mlt><producer id=\"a\"></mlt>"), Error::Parse { .. }));
    let e = err("<xmeml version=\"4\"/>");
    assert!(e.to_string().contains("not <mlt>"), "{e}");
    assert_eq!(err("<mlt><producer id=\"a\"/></mlt>"), Error::Empty);
    assert!(matches!(err(""), Error::Parse { format: "Kdenlive (MLT XML)", .. }));
}

/// Hostile numbers and structures: no panic, an import with warnings.
#[test]
fn hostile_values_and_cycles() {
    let hostile = r#"<mlt producer="a">
 <profile width="-5" height="99999999" frame_rate_num="0" frame_rate_den="0" sample_aspect_num="0" sample_aspect_den="-1"/>
 <producer id="p" in="99999999999999999999" out="-1"><property name="resource">/m/p.mp4</property><property name="length">999999999999999999</property></producer>
 <producer id="q"><property name="resource">/m/q.mp4</property><property name="mlt_service">color</property></producer>
 <playlist id="pl">
  <entry producer="p" in="00:00:99999999999999.5" out="1"/>
  <entry producer="p" in="00:59:59;29" out="99:99:99:99"/>
  <entry producer="q" in="0" out="1"/>
  <blank length="999999999999999999"/>
  <entry producer="p" in="0" out="9"/>
 </playlist>
 <playlist id="pl2">
  <entry producer="p" in="0" out="999999999999999999"/>
  <entry producer="p" in="0" out="9"/>
 </playlist>
 <tractor id="a"><track producer="b"/></tractor>
 <tractor id="b"><track producer="a"/></tractor>
 <tractor id="self"><track producer="self"/></tractor>
</mlt>"#;
    let (imp, report) = import_mlt(hostile);
    assert_eq!(imp.sequences.len(), 1);
    assert!(report.mentions("frame rate is missing or out of range") && report.mentions("frame size is missing or out of range"), "{report}");
    assert!(report.mentions("nested timeline"), "{report}");

    // the playlists on a real timeline: what lies past 30 days is dropped, the rest kept
    let long =
        hostile.replace(r#"<tractor id="a"><track producer="b"/></tractor>"#, r#"<tractor id="a"><track producer="pl"/><track producer="pl2"/></tractor>"#);
    let (imp, report) = import_mlt(&long);
    let seq = only_sequence(&imp);
    let rate = FrameRate::FPS_25;
    // 00:59:59;29 (no drop-frame at 25 fps) to 99:99:99:99, then the colour that is not one (black)
    assert_eq!(clips(&seq.video_tracks[0].items, rate), vec![c("p.mp4", 0, 8_971_071, 90_004), c("Color", 8_971_071, 2, 0)], "{report}");
    assert!(seq.video_tracks[1].items.is_empty(), "{report}");
    assert!(report.mentions("runs past 30 days") && report.mentions("is not understood: read as black"), "{report}");
    assert!(report.mentions("ends (out 1) before it starts"), "{report}");

    // a chain of wrapper tractors deeper than the importer looks: stops, reports, no recursion
    let mut deep = String::from(
        "<mlt><producer id=\"m\"><property name=\"resource\">/m/m.mp4</property></producer><playlist id=\"pl\"><entry producer=\"m\" in=\"0\" out=\"9\"/></playlist><tractor id=\"t0\"><track producer=\"pl\"/><track producer=\"pl\"/></tractor>",
    );
    for i in 1..=500 {
        deep.push_str(&format!("<tractor id=\"t{i}\"><track producer=\"t{}\"/></tractor>", i - 1));
    }
    deep.push_str("</mlt>");
    let (imp, report) = import_mlt(&deep);
    assert_eq!(imp.sequences.len(), 1);
    assert!(report.mentions("nested timeline"), "{report}");
}

/// Mutation fuzz: truncations and byte changes of the main document never panic.
#[test]
fn truncated_and_corrupted_documents_never_panic() {
    let bytes = PROJECT.as_bytes();
    let run = |b: &[u8]| {
        let r = catch_unwind(AssertUnwindSafe(|| import(b, Format::Mlt, Some("/p"))));
        assert!(r.is_ok(), "panicked on a {}-byte document", b.len());
    };
    for cut in (0..bytes.len()).step_by(37) {
        run(&bytes[..cut]);
    }
    // xorshift: reproducible positions and values
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let swaps: &[u8] = b"<>\"/=0123456789:;.,-x \n&";
    for _ in 0..400 {
        let mut b = bytes.to_vec();
        for _ in 0..1 + next() % 8 {
            let at = (next() % b.len() as u64) as usize;
            b[at] = swaps[(next() % swaps.len() as u64) as usize];
        }
        run(&b);
    }
}

#[test]
fn detection_and_import_only() {
    assert_eq!(detect(PROJECT.as_bytes(), Some("kdenlive")), Some(Format::Mlt));
    assert_eq!(detect(b"<?xml version='1.0'?>\n<mlt LC_NUMERIC=\"C\"/>", None), Some(Format::Mlt));
    assert_eq!(detect(b"<?xml version='1.0'?>\n<other/>", Some("kdenlive")), Some(Format::Mlt), "the extension decides for other XML");
    assert_eq!(Format::from_extension(".KDENLIVE"), Some(Format::Mlt));
    assert_eq!(Format::Mlt.extension(), "kdenlive");
    assert!(!Format::Mlt.can_export());
    assert!(Format::ALL.iter().filter(|f| **f != Format::Mlt).all(|f| f.can_export()));
    let (imp, _) = import_mlt(PROJECT);
    let e = export(&imp.project, imp.sequences[0], Format::Mlt, &ExportOptions::default()).unwrap_err();
    assert!(e.to_string().contains("import only"), "{e}");
}
