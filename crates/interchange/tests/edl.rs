//! CMX 3600 EDL: golden documents (hand-written from the public CMX 3600 conventions) and round trips.

mod support;

use filmcraft_interchange::{ExportOptions, Format, ImportOptions, ReelMode, detect, export, export_edl_per_track, import, import_with};
use filmcraft_project::{ItemKind, MediaRef, TrackKind, TransitionAlign};
use filmcraft_time::{FrameRate, Tick, fields_to_frames};
use proptest::prelude::*;
use support::*;

const DF_DISSOLVES: &str = "TITLE: Harbour Cut
FCM: DROP FRAME

001  HARB01   B     C        00:10:00:00 00:10:04:00 01:00:00:00 01:00:04:00
* FROM CLIP NAME: harbour_wide.mov
* SOURCE FILE: media/harbour_wide.mov

002  HARB01   V     C        00:10:04:00 00:10:04:00 01:00:04:00 01:00:04:00
002  HARB02   V     D    030 00:20:00:00 00:20:05:00 01:00:04:00 01:00:09:00
* FROM CLIP NAME: harbour_wide.mov
* TO CLIP NAME: harbour_close.mov
* SOURCE FILE: media/harbour_wide.mov
* SOURCE FILE: media/harbour_close.mov

003  GULLS    V     C        00:30:00:00 00:30:02:00 01:00:09:00 01:00:11:00
M2   GULLS        060.0                00:30:00:00
* FROM CLIP NAME: gulls.mov
* LOC: 01:00:09;15 RED     gull lands

004  MUSIC    A2    C        00:00:00:00 00:00:11:00 01:00:00:00 01:00:11:00
* FROM CLIP NAME: score.wav

005  BL       V     C        00:00:00:00 00:00:00:00 01:00:11:00 01:00:11:00
005  HARB02   V     D    015 00:20:05:00 00:20:07:00 01:00:11:00 01:00:13:00
* TO CLIP NAME: harbour_close.mov
* SOURCE FILE: media/harbour_close.mov

006  HARB02   V     C        00:20:07:00 00:20:07:00 01:00:13:00 01:00:13:00
006  BL       V     D    010 00:00:00:00 00:00:00:10 01:00:13:00 01:00:13:10
* FROM CLIP NAME: harbour_close.mov
";

#[test]
fn golden_drop_frame_dissolves() {
    assert_eq!(detect(DF_DISSOLVES.as_bytes(), None), Some(Format::Edl));
    let (imp, rep) = import(DF_DISSOLVES.as_bytes(), Format::Edl, Some("/proj")).unwrap();
    assert!(rep.mentions("assumed 29.97"), "{rep}");
    let p = &imp.project;
    let sid = only_seq(&imp);
    let s = p.sequence(sid).unwrap();
    let r = FrameRate::FPS_29_97;
    assert_eq!(s.settings.frame_rate, r);
    assert!(s.settings.drop_frame);
    assert_eq!(p.item(sid).unwrap().name, "Harbour Cut");
    assert_eq!(s.start_timecode, 107_892); // 01:00:00;00 DF

    let v1 = &s.video_tracks[0];
    assert_eq!(v1.items.len(), 4, "{:#?}", v1.items);
    let [a, b, g, c] = [&v1.items[0], &v1.items[1], &v1.items[2], &v1.items[3]];
    assert_eq!((a.start, a.duration), (f(r, 0), f(r, 120)));
    assert_eq!(a.source_in, f(r, fields_to_frames(0, 10, 0, 0, r, true)));
    assert_eq!(a.source_in, f(r, 17_982));
    assert_eq!(media_key(p, a.item), "/proj/media/harbour_wide.mov");
    assert_eq!(a.name, "harbour_wide.mov");
    // dissolve event
    assert_eq!((b.start, b.duration, b.source_in), (f(r, 120), f(r, 150), f(r, 35_964)));
    assert_eq!(media_key(p, b.item), "/proj/media/harbour_close.mov");
    // M2 speed line
    assert_eq!(g.speed, 2.0);
    assert_eq!(g.start, f(r, 270));
    assert_eq!(media_key(p, g.item), "/proj/gulls.mov");
    // fade in from black, then fade out to black (clip extended through the transition)
    assert_eq!(c.item, b.item);
    assert_eq!((c.start, c.duration), (f(r, 330), f(r, 60 + 10)));
    let ts = &v1.transitions;
    assert_eq!(ts.len(), 3);
    assert_eq!((ts[0].start, ts[0].duration, ts[0].from, ts[0].to), (f(r, 120), f(r, 30), Some(a.id), Some(b.id)));
    assert_eq!(ts[0].align, TransitionAlign::StartAtCut);
    assert_eq!(ts[0].effect.effect, "cross_dissolve");
    assert_eq!((ts[1].from, ts[1].to, ts[1].duration), (None, Some(c.id), f(r, 15)));
    assert_eq!((ts[2].from, ts[2].to, ts[2].start, ts[2].duration), (Some(c.id), None, f(r, 390), f(r, 10)));
    assert_eq!(ts[2].align, TransitionAlign::EndAtCut);

    // B = V + A1, linked; A2 music.
    let a1 = &s.audio_tracks[0].items;
    assert_eq!(a1.len(), 1);
    assert_eq!(a1[0].link, a.link);
    assert!(a.link.is_some());
    let a2 = &s.audio_tracks[1].items;
    assert_eq!((a2[0].start, a2[0].duration), (f(r, 0), f(r, 330)));
    // LOC marker
    assert_eq!(s.markers.len(), 1);
    assert_eq!(s.markers[0].name, "gull lands");
    assert_eq!(s.markers[0].start, f(r, 285));
    assert_eq!(s.markers[0].color, filmcraft_project::Label::Rose);
    // media without a source file is offline with its reel as tape name
    let gm = p.item(g.item).unwrap();
    assert_eq!(gm.metadata.get("Tape Name").map(String::as_str), Some("GULLS"));
    assert!(gm.as_media().unwrap().offline);
}

#[test]
fn explicit_rate_and_wipes_and_keys() {
    let edl = "TITLE: Keys
FCM: NON-DROP FRAME

001  BG       V     C        00:00:00:00 00:00:02:00 00:00:00:00 00:00:02:00
002  BG       V     C        00:00:02:00 00:00:02:00 00:00:02:00 00:00:02:00
002  FG       V     W001 012 00:00:05:00 00:00:07:00 00:00:02:00 00:00:04:00
003  BG2      V     K B      00:01:00:00 00:01:02:00 00:00:04:00 00:00:06:00
003  LOGO     V     K        00:00:00:00 00:00:02:00 00:00:04:00 00:00:06:00
004  AUDX     A3    C        00:00:00:00 00:00:06:00 00:00:00:00 00:00:06:00
";
    let opts = ImportOptions { edl_frame_rate: Some(FrameRate::FPS_25), ..Default::default() };
    let (imp, rep) = import_with(edl.as_bytes(), Format::Edl, &opts).unwrap();
    let p = &imp.project;
    let s = p.sequence(only_seq(&imp)).unwrap();
    let r = FrameRate::FPS_25;
    assert_eq!(s.start_timecode, 0);
    assert_eq!(s.video_tracks[0].items.len(), 3);
    assert_eq!(s.video_tracks[0].transitions[0].effect.effect, "wipe");
    assert_eq!(s.video_tracks[0].transitions[0].duration, f(r, 12));
    assert_eq!(s.video_tracks[1].items.len(), 1, "key foreground on V2");
    assert_eq!(s.video_tracks[1].items[0].start, f(r, 100));
    assert_eq!(s.audio_tracks.len(), 3);
    assert_eq!(s.audio_tracks[2].items[0].duration, f(r, 150));
    assert!(rep.mentions("key"), "{rep}");
}

/// #711: a named audio dissolve keeps its audio transition instead of falling back to Constant
/// Power with a false "not supported" warning; an A/V event still resolves the name per kind.
#[test]
fn named_audio_dissolves_keep_their_effect() {
    let opts = ImportOptions { edl_frame_rate: Some(FrameRate::FPS_24), ..Default::default() };
    let edl = |chan: &str, name: &str| {
        format!(
            "TITLE: AudioDissolve
FCM: NON-DROP FRAME

001  AX       {chan}     C        00:00:00:00 00:00:02:00 01:00:00:00 01:00:02:00
* FROM CLIP NAME: A

002  AX       {chan}     C        00:00:02:00 00:00:02:00 01:00:02:00 01:00:02:00
002  AX       {chan}     D 024    00:00:02:00 00:00:04:00 01:00:02:00 01:00:04:00
* TO CLIP NAME: B
* EFFECT NAME: {name}
"
        )
    };
    for (name, id) in [("CONSTANT GAIN", "constant_gain"), ("EXPONENTIAL FADE", "exponential_fade"), ("CONSTANT POWER", "constant_power")] {
        let (imp, rep) = import_with(edl("A", name).as_bytes(), Format::Edl, &opts).unwrap();
        let s = imp.project.sequence(only_seq(&imp)).unwrap();
        assert_eq!(s.audio_tracks[0].transitions.len(), 1, "{name}");
        assert_eq!(s.audio_tracks[0].transitions[0].effect.effect, id, "{name}");
        assert!(!rep.mentions("not supported"), "{name}: {rep}");
    }
    // An unknown name still warns and falls back to Constant Power on audio.
    let (imp, rep) = import_with(edl("A", "UNRECOGNIZED TRANSITION").as_bytes(), Format::Edl, &opts).unwrap();
    let s = imp.project.sequence(only_seq(&imp)).unwrap();
    assert_eq!(s.audio_tracks[0].transitions[0].effect.effect, "constant_power");
    assert!(rep.mentions("not supported"), "{rep}");
    // On an A/V event the video track gets Cross Dissolve, the audio track Constant Gain.
    let (imp, _) = import_with(edl("B", "CONSTANT GAIN").as_bytes(), Format::Edl, &opts).unwrap();
    let s = imp.project.sequence(only_seq(&imp)).unwrap();
    assert_eq!(s.video_tracks[0].transitions[0].effect.effect, "cross_dissolve");
    assert_eq!(s.audio_tracks[0].transitions[0].effect.effect, "constant_gain");
}

fn demo_project(rate: FrameRate, df: bool) -> (filmcraft_project::Project, filmcraft_project::ItemId) {
    let mut p = filmcraft_project::Project::new("P");
    let a = media(&mut p, "/media/Beach Day.mov", true, true, rate);
    let b = media(&mut p, "/media/city.mp4", true, true, rate);
    let m = media(&mut p, "/media/score.wav", false, true, rate);
    let s = sequence(&mut p, "Edit 1", rate, df);
    let va = clip(&mut p, s, TrackKind::Video, 0, a, 0, 100, 10);
    let aa = clip(&mut p, s, TrackKind::Audio, 0, a, 0, 100, 10);
    link(&mut p, s, &[va, aa]);
    let vb = clip(&mut p, s, TrackKind::Video, 0, b, 100, 60, 200);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(va), Some(vb), 100, 20, TransitionAlign::StartAtCut);
    let vc = clip(&mut p, s, TrackKind::Video, 0, a, 200, 50, 500);
    transition(&mut p, s, TrackKind::Video, 0, "dip_to_black", None, Some(vc), 200, 10, TransitionAlign::StartAtCut);
    p.sequence_mut(s).unwrap().find_item_mut(vc).unwrap().1.speed = 2.0;
    clip(&mut p, s, TrackKind::Audio, 1, m, 0, 250, 0);
    (p, s)
}

#[test]
fn export_golden_text() {
    let (p, s) = demo_project(FrameRate::FPS_25, false);
    let (bytes, rep) = export(&p, s, Format::Edl, &ExportOptions::default()).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let expected = "TITLE: Edit 1
FCM: NON-DROP FRAME

001  BEACH_DA B     C        00:00:00:10 00:00:04:10 00:00:00:00 00:00:04:00
* FROM CLIP NAME: Beach Day.mov
* SOURCE FILE: /media/Beach Day.mov

002  SCORE    A2    C        00:00:00:00 00:00:10:00 00:00:00:00 00:00:10:00
* FROM CLIP NAME: score.wav
* SOURCE FILE: /media/score.wav

003  BEACH_DA V     C        00:00:04:10 00:00:04:10 00:00:04:00 00:00:04:00
003  CITY     V     D    020 00:00:08:00 00:00:10:10 00:00:04:00 00:00:06:10
* FROM CLIP NAME: Beach Day.mov
* TO CLIP NAME: city.mp4
* SOURCE FILE: /media/Beach Day.mov
* SOURCE FILE: /media/city.mp4
* EFFECT NAME: CROSS DISSOLVE

004  BL       V     C        00:00:00:00 00:00:00:00 00:00:08:00 00:00:08:00
004  BEACH_DA V     D    010 00:00:20:00 00:00:22:00 00:00:08:00 00:00:10:00
M2   BEACH_DA       050.0                00:00:20:00
* TO CLIP NAME: Beach Day.mov
* SOURCE FILE: /media/Beach Day.mov
* EFFECT NAME: DIP TO BLACK
";
    assert_eq!(text, expected, "{text}");
    assert!(rep.mentions("Dip to Black"), "{rep}");

    // …and it imports back to the same structure.
    let o = ImportOptions { edl_frame_rate: Some(FrameRate::FPS_25), ..Default::default() };
    let (imp, _) = import_with(text.as_bytes(), Format::Edl, &o).unwrap();
    let si = only_seq(&imp);
    assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video));
    assert_eq!(structure(&imp.project, si, TrackKind::Audio), structure(&p, s, TrackKind::Audio));
    assert_eq!(links(&imp.project, si), links(&p, s));
}

#[test]
fn reel_modes_and_per_track() {
    let (mut p, s) = demo_project(FrameRate::FPS_24, false);
    let a = p.items.values().find(|i| i.name == "city.mp4").unwrap().id;
    clip(&mut p, s, TrackKind::Video, 1, a, 10, 20, 0);
    let mut o = ExportOptions::default();
    o.edl.reel_mode = ReelMode::Ax;
    o.edl.reel_len = 32;
    let (bytes, _) = export(&p, s, Format::Edl, &o).unwrap();
    let t = String::from_utf8(bytes).unwrap();
    assert!(t.lines().filter(|l| l.starts_with("00")).all(|l| l[5..7] == *"AX" || l[5..7] == *"BL"), "{t}");
    let (docs, rep) = export_edl_per_track(&p, s, &ExportOptions::default()).unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[1].0, "V2");
    let v2 = String::from_utf8(docs[1].1.clone()).unwrap();
    assert!(v2.contains("CITY     V     C"), "{v2}");
    assert!(!v2.contains("SCORE"), "audio only in the first list");
    let _ = rep;
    // long file names are cut to 8 characters, distinct media keep distinct reels
    let mut q = filmcraft_project::Project::new("q");
    let m1 = media(&mut q, "/x/interview_cam_a.mov", true, false, FrameRate::FPS_24);
    let m2 = media(&mut q, "/x/interview_cam_b.mov", true, false, FrameRate::FPS_24);
    let sq = sequence(&mut q, "S", FrameRate::FPS_24, false);
    clip(&mut q, sq, TrackKind::Video, 0, m1, 0, 10, 0);
    clip(&mut q, sq, TrackKind::Video, 0, m2, 10, 10, 0);
    let (bytes, _) = export(&q, sq, Format::Edl, &ExportOptions::default()).unwrap();
    let t = String::from_utf8(bytes).unwrap();
    assert!(t.contains("001  INTERVIE V"), "{t}");
    assert!(t.contains("002  INTERVI2 V"), "{t}");
}

#[test]
fn drop_frame_roundtrip_59_94() {
    let r = FrameRate::FPS_59_94;
    let mut p = filmcraft_project::Project::new("P");
    let a = media(&mut p, "/m/a.mov", true, false, r);
    let s = sequence(&mut p, "S", r, true);
    p.sequence_mut(s).unwrap().start_timecode = fields_to_frames(1, 0, 0, 0, r, true);
    clip(&mut p, s, TrackKind::Video, 0, a, 3590, 30, 3590);
    clip(&mut p, s, TrackKind::Video, 0, a, 36_000, 7200, 0);
    let (imp, text, _) = roundtrip(&p, s, Format::Edl, &ExportOptions::default());
    assert!(text.contains("FCM: DROP FRAME"));
    assert!(text.contains("01;00;59;50 01;01;00;24"), "{text}");
    let si = only_seq(&imp);
    // 59.94 is not guessable from the list alone, pass it.
    let (imp2, _) = import_with(text.as_bytes(), Format::Edl, &ImportOptions { edl_frame_rate: Some(r), ..Default::default() }).unwrap();
    let si2 = only_seq(&imp2);
    assert_eq!(structure(&imp2.project, si2, TrackKind::Video), structure(&p, s, TrackKind::Video));
    assert_eq!(imp2.project.sequence(si2).unwrap().start_timecode, p.sequence(s).unwrap().start_timecode);
    // The guessed rate (59.94 because frame fields exceed 29) also works.
    assert_eq!(imp.project.sequence(si).unwrap().settings.frame_rate, r);
}

#[test]
fn freeze_reverse_and_fade_out() {
    let r = FrameRate::FPS_24;
    let mut p = filmcraft_project::Project::new("P");
    let a = media(&mut p, "/m/a.mov", true, false, r);
    let s = sequence(&mut p, "S", r, false);
    let c1 = clip(&mut p, s, TrackKind::Video, 0, a, 0, 48, 100);
    let c2 = clip(&mut p, s, TrackKind::Video, 0, a, 48, 24, 300);
    let c3 = clip(&mut p, s, TrackKind::Video, 0, a, 72, 24, 400);
    {
        let q = p.sequence_mut(s).unwrap();
        q.find_item_mut(c1).unwrap().1.reverse = true;
        q.find_item_mut(c2).unwrap().1.frame_hold = Some(r.tick_of(310));
    }
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(c3), None, 84, 12, TransitionAlign::EndAtCut);
    let (imp, text, _) = roundtrip(&p, s, Format::Edl, &ExportOptions::default());
    assert!(text.contains("-024.0"), "{text}");
    assert!(text.contains("000.0"), "{text}");
    let si = only_seq(&imp);
    assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video), "{text}");
}

#[test]
fn media_paths_relative_export() {
    let (p, s) = demo_project(FrameRate::FPS_24, false);
    let o = ExportOptions { relative_to: Some("/media/edl".into()), ..Default::default() };
    let (bytes, _) = export(&p, s, Format::Edl, &o).unwrap();
    let t = String::from_utf8(bytes).unwrap();
    assert!(t.contains("* SOURCE FILE: ../city.mp4"), "{t}");
    let (imp, _) = import(t.as_bytes(), Format::Edl, Some("/media/edl")).unwrap();
    let paths: Vec<String> = imp
        .project
        .items
        .values()
        .filter_map(|i| match &i.kind {
            ItemKind::Media(m) => match &m.media {
                MediaRef::File { path } => Some(path.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(paths.contains(&"/media/city.mp4".to_string()), "{paths:?}");
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    assert!(import(b"hello world", Format::Edl, None).is_err());
    assert!(detect(b"hello world", Some("txt")).is_none());
}

// ---------------------------------------------------------------------------------------------
// Property: cut lists with linked A/V, dissolves and fades survive export → import exactly.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Gen {
    gap: i64,
    dur: i64,
    media: usize,
    src: i64,
    with_audio: bool,
    dissolve: Option<i64>,
    speed: u8,
}

fn gen_clip() -> impl Strategy<Value = Gen> {
    (0i64..3, 12i64..200, 0usize..3, 30i64..5000, any::<bool>(), prop::option::of(1i64..10), 0u8..6).prop_map(
        |(gap, dur, media, src, with_audio, dissolve, speed)| Gen { gap: if gap == 0 { 0 } else { gap * 7 }, dur, media, src, with_audio, dissolve, speed },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn edl_roundtrip(clips in prop::collection::vec(gen_clip(), 1..14), ri in 0usize..4) {
        let rate = [FrameRate::FPS_24, FrameRate::FPS_25, FrameRate::FPS_29_97, FrameRate::FPS_23_976][ri];
        let df = rate == FrameRate::FPS_29_97;
        let mut p = filmcraft_project::Project::new("P");
        let ms = [media(&mut p, "/a/one.mov", true, true, rate), media(&mut p, "/a/two.mov", true, true, rate), media(&mut p, "/b/three.mxf", true, true, rate)];
        let s = sequence(&mut p, "Seq", rate, df);
        let mut t = 0i64;
        let mut prev: Option<(filmcraft_project::ClipId, i64)> = None;
        for g in &clips {
            t += g.gap;
            let v = clip(&mut p, s, TrackKind::Video, 0, ms[g.media], t, g.dur, g.src);
            if g.speed == 1 {
                p.sequence_mut(s).unwrap().find_item_mut(v).unwrap().1.speed = 2.0;
            }
            let mut has_tr = false;
            if let Some(d) = g.dissolve {
                let d = d.min(g.dur - 1);
                let from = prev.filter(|(_, end)| *end == t).map(|(c, _)| c);
                if g.src >= d * 2 {
                    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", from, Some(v), t, d, TransitionAlign::StartAtCut);
                    has_tr = true;
                }
            }
            if g.with_audio && g.speed != 1 && !has_tr {
                let a = clip(&mut p, s, TrackKind::Audio, 0, ms[g.media], t, g.dur, g.src);
                link(&mut p, s, &[v, a]);
            }
            t += g.dur;
            prev = Some((v, t));
        }
        let (imp, text, _) = roundtrip(&p, s, Format::Edl, &ExportOptions::default());
        let opts = ImportOptions { edl_frame_rate: Some(rate), ..Default::default() };
        let (imp2, _) = import_with(text.as_bytes(), Format::Edl, &opts).unwrap();
        let _ = imp;
        let si = only_seq(&imp2);
        prop_assert_eq!(structure(&imp2.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video), "{}", text);
        prop_assert_eq!(structure(&imp2.project, si, TrackKind::Audio), structure(&p, s, TrackKind::Audio), "{}", text);
        prop_assert_eq!(links(&imp2.project, si), links(&p, s));
        prop_assert_eq!(imp2.project.sequence(si).unwrap().settings.drop_frame, df);
        let _ = Tick::ZERO;
    }
}

/// A nested sequence is one event: reel AX, the nested sequence's name as the clip name, and its
/// own time as the source timecode. Premiere Pro 26.5.2 writes the same (seen in an EDL it
/// exported for a sequence with a nest):
/// `001  AX       V     C        00:00:00:00 00:00:06:00 00:00:00:00 00:00:06:00` and
/// `* FROM CLIP NAME: Nested Sequence 02`.
#[test]
fn a_nested_sequence_is_one_event_from_reel_ax() {
    let rate = FrameRate::FPS_24;
    let mut p = filmcraft_project::Project::new("n");
    let a = media(&mut p, "/media/a.mov", true, true, rate);
    let inner = sequence(&mut p, "Nested Sequence 02", rate, false);
    clip(&mut p, inner, TrackKind::Video, 0, a, 0, 144, 0);
    clip(&mut p, inner, TrackKind::Audio, 0, a, 0, 144, 0);
    let outer = sequence(&mut p, "clipHD", rate, false);
    // two seconds of the nest from its second 1, at second 3 of the sequence
    let v = clip(&mut p, outer, TrackKind::Video, 0, inner, 72, 48, 24);
    let s = clip(&mut p, outer, TrackKind::Audio, 0, inner, 72, 48, 24);
    link(&mut p, outer, &[v, s]);
    let (bytes, rep) = export(&p, outer, Format::Edl, &ExportOptions::default()).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    let events: Vec<&str> = text.lines().filter(|l| l.starts_with("00")).collect();
    assert_eq!(events.len(), 1, "{text}");
    let fields: Vec<&str> = events[0].split_whitespace().collect();
    assert_eq!(fields, ["001", "AX", "B", "C", "00:00:01:00", "00:00:03:00", "00:00:03:00", "00:00:05:00"], "{text}");
    assert!(text.lines().any(|l| l == "* FROM CLIP NAME: Nested Sequence 02"), "{text}");
    assert!(rep.mentions("nested sequences"), "the report says the nest's own edit is not in the EDL: {rep}");
    // read back, it is a clip of that name at the same place (an EDL cannot hold the nested edit)
    let (imp, _) = import(&bytes, Format::Edl, None).unwrap();
    let q = imp.project.sequence(only_seq(&imp)).unwrap();
    let c = &q.video_tracks[0].items[0];
    assert_eq!((c.name.as_str(), c.start, c.duration, c.source_in), ("Nested Sequence 02", rate.tick_of(72), rate.tick_of(48), rate.tick_of(24)));
}
