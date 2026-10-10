//! AAF and OMF: export → import round trips (no external oracle exists for these formats).

mod support;

use filmcraft_interchange::aaf::{self, AafOptions};
use filmcraft_interchange::essence::{AudioEssence, EssenceData, EssenceKey, MediaOptions, MixdownVideo, NeedOptions, NestNeeds, audio_needs};
use filmcraft_interchange::omf::{self, OmfOptions};
use filmcraft_interchange::{Format, ImportOptions, Imported, detect};
use filmcraft_project::{
    AudioChannels, Interpolation, ItemId, ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, Param, ParamValue, Project, TrackKind,
    TransitionAlign,
};
use filmcraft_time::{FrameRate, Tick};
use support::*;

/// V1: a fade-in from black, a cross dissolve, a gap, a clip with a dip-to-black fade-out.
/// V2: one clip. A1/A2: the linked audio with a crossfade, constant gain and volume keyframes.
fn sample(rate: FrameRate, df: bool) -> (Project, ItemId) {
    let mut p = Project::new("p");
    let a = media(&mut p, "/media/A001.mov", true, true, rate);
    let b = media(&mut p, "/media/B002.mov", true, true, rate);
    let m = media(&mut p, "/media/music.wav", false, true, rate);
    if let Some(mm) = p.item_mut(a).and_then(|i| i.as_media_mut()) {
        mm.info.start_timecode = Some(rate.timecode_base() * 3600 + 12);
        mm.markers.push(Marker {
            id: MarkerId(9000),
            start: rate.tick_of(40),
            duration: Tick::ZERO,
            name: "slate".into(),
            comment: "take 3".into(),
            kind: MarkerKind::Comment,
            color: Label::Rose,
        });
    }
    let s = sequence(&mut p, "Edit 1", rate, df);
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0, 100, 20);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, b, 100, 80, 50);
    let v3 = clip(&mut p, s, TrackKind::Video, 0, a, 220, 60, 300);
    clip(&mut p, s, TrackKind::Video, 1, b, 30, 40, 0);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", None, Some(v1), 0, 12, TransitionAlign::StartAtCut);
    transition(&mut p, s, TrackKind::Video, 0, "cross_dissolve", Some(v1), Some(v2), 90, 20, TransitionAlign::CenterAtCut);
    transition(&mut p, s, TrackKind::Video, 0, "dip_to_black", Some(v3), None, 266, 14, TransitionAlign::EndAtCut);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0, 100, 20);
    let a2 = clip(&mut p, s, TrackKind::Audio, 0, b, 100, 80, 50);
    let a3 = clip(&mut p, s, TrackKind::Audio, 0, a, 220, 60, 300);
    transition(&mut p, s, TrackKind::Audio, 0, "constant_gain", Some(a1), Some(a2), 95, 10, TransitionAlign::CenterAtCut);
    let mu = clip(&mut p, s, TrackKind::Audio, 1, m, 10, 200, 0);
    link(&mut p, s, &[v1, a1]);
    link(&mut p, s, &[v2, a2]);
    link(&mut p, s, &[v3, a3]);
    {
        let q = p.sequence_mut(s).unwrap();
        q.start_timecode = rate.timecode_base() * 3600;
        q.markers.push(Marker {
            id: MarkerId(9001),
            start: rate.tick_of(50),
            duration: rate.tick_of(10),
            name: "Fix this".into(),
            comment: "colour".into(),
            kind: MarkerKind::Comment,
            color: Label::Mango,
        });
        let (_, c) = q.find_item_mut(a2).unwrap();
        c.effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-6.0)));
        let (_, c) = q.find_item_mut(mu).unwrap();
        let mut prm = Param::new(ParamValue::Float(0.0));
        for (f, db) in [(0, -12.0), (48, 0.0), (150, -3.0)] {
            let mut k = Keyframe::new(rate.tick_of(f), ParamValue::Float(db));
            k.interp = Interpolation::Linear;
            prm.keyframes.push(k);
        }
        c.effect_mut("volume").unwrap().params.insert("level".into(), prm);
    }
    (p, s)
}

fn level(p: &Project, seq: ItemId, track: usize, clip: usize) -> Param {
    p.sequence(seq).unwrap().audio_tracks[track].items[clip]
        .effect("volume")
        .and_then(|e| e.param("level"))
        .cloned()
        .unwrap_or(Param::new(ParamValue::Float(0.0)))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn aaf_rt(p: &Project, s: ItemId, opts: &AafOptions) -> (Imported, Vec<filmcraft_interchange::ExtractedMedia>, filmcraft_interchange::Report) {
    let (bytes, _) = aaf::export(p, s, opts).expect("export");
    assert!(aaf::sniff(&bytes));
    assert_eq!(detect(&bytes, Some("aaf")), Some(Format::Aaf));
    aaf::import(&bytes, &ImportOptions { base_dir: Some("/proj".into()), name: Some("rt".into()), ..Default::default() }).expect("import")
}

#[test]
fn aaf_round_trip_preserves_the_edit() {
    for (rate, df) in [(FrameRate::FPS_25, false), (FrameRate::FPS_29_97, true), (FrameRate::FPS_23_976, false), (FrameRate::FPS_59_94, true)] {
        let (p, s) = sample(rate, df);
        let (imp, extracted, rep) = aaf_rt(&p, s, &AafOptions::default());
        assert!(extracted.is_empty());
        let n = only_seq(&imp);
        assert_eq!(structure(&imp.project, n, TrackKind::Video), structure(&p, s, TrackKind::Video), "{rate:?}: {rep}");
        assert_eq!(structure(&imp.project, n, TrackKind::Audio), structure(&p, s, TrackKind::Audio), "{rate:?}: {rep}");
        assert_eq!(links(&imp.project, n), links(&p, s));
        let q = imp.project.sequence(n).unwrap();
        assert_eq!(q.settings.frame_rate, rate);
        assert_eq!(q.settings.drop_frame, df);
        assert_eq!(q.start_timecode, rate.timecode_base() * 3600);
        assert_eq!((q.settings.width, q.settings.height), (1920, 1080));
        assert_eq!(imp.project.item(n).unwrap().name, "Edit 1");
        // markers
        assert_eq!(q.markers.len(), 1);
        assert_eq!((q.markers[0].start, q.markers[0].duration), (rate.tick_of(50), rate.tick_of(10)));
        assert_eq!((q.markers[0].name.as_str(), q.markers[0].comment.as_str(), q.markers[0].color), ("Fix this", "colour", Label::Mango));
        // gains
        assert!(close(level(&imp.project, n, 0, 1).value.as_f64().unwrap(), -6.0));
        let k = level(&imp.project, n, 1, 0);
        let want = level(&p, s, 1, 0);
        assert_eq!(k.keyframes.len(), 3);
        for (a, b) in k.keyframes.iter().zip(&want.keyframes) {
            // control points are stored in samples: keyframes not on a sample move to the nearest one
            assert!((a.time - b.time).abs() <= Tick(filmcraft_time::TICKS_PER_SECOND / 48_000 / 2), "{rate:?}: {a:?} {b:?}");
            assert!(close(a.value.as_f64().unwrap(), b.value.as_f64().unwrap()), "{a:?} {b:?}");
        }
        // media: paths, timecode, markers
        let a = imp
            .project
            .items
            .values()
            .find(|i| matches!(&i.kind, ItemKind::Media(m) if matches!(&m.media, MediaRef::File{path} if path == "/media/A001.mov")))
            .unwrap();
        let ma = a.as_media().unwrap();
        assert_eq!(ma.info.start_timecode, Some(rate.timecode_base() * 3600 + 12));
        assert!(ma.info.video.is_some() && ma.info.has_audio());
        assert_eq!(ma.markers.len(), 1);
        assert_eq!((ma.markers[0].name.as_str(), ma.markers[0].comment.as_str()), ("slate", "take 3"));
        let mus = imp.project.items.values().find(|i| i.name == "music.wav").unwrap();
        assert!(mus.as_media().unwrap().info.video.is_none());
        // the same document again: byte-identical (reproducible)
        assert_eq!(aaf::export(&p, s, &AafOptions::default()).unwrap().0, aaf::export(&p, s, &AafOptions::default()).unwrap().0);
    }
}

#[test]
fn aaf_small_sectors_and_generic_api() {
    let (p, s) = sample(FrameRate::FPS_25, false);
    let (b512, _) = aaf::export(&p, s, &AafOptions { small_sectors: true, ..Default::default() }).unwrap();
    assert_eq!(u16::from_le_bytes([b512[30], b512[31]]), 9);
    let (imp, _) = filmcraft_interchange::import(&b512, Format::Aaf, None).unwrap();
    let n = only_seq(&imp);
    assert_eq!(structure(&imp.project, n, TrackKind::Video), structure(&p, s, TrackKind::Video));
    let (bytes, _) = filmcraft_interchange::export(&p, s, Format::Aaf, &Default::default()).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[30], bytes[31]]), 12);
}

#[test]
fn aaf_breakout_to_mono() {
    let (p, s) = sample(FrameRate::FPS_25, false);
    let opts = AafOptions { media: MediaOptions { breakout_to_mono: true, ..Default::default() }, ..Default::default() };
    let (imp, _, _) = aaf_rt(&p, s, &opts);
    let n = only_seq(&imp);
    let q = imp.project.sequence(n).unwrap();
    assert_eq!(q.audio_tracks.len(), 8, "four stereo tracks → eight mono tracks");
    for (i, t) in q.audio_tracks.iter().enumerate() {
        assert_eq!(t.channels, AudioChannels::Mono);
        let ch = (i % 2) as u16;
        assert!(t.items.iter().all(|c| c.source_channels == vec![ch]), "track {i}");
    }
    // left and right of the same clip use the same media item
    assert_eq!(q.audio_tracks[0].items[0].item, q.audio_tracks[1].items[0].item);
    let orig = structure(&p, s, TrackKind::Audio);
    let got = structure(&imp.project, n, TrackKind::Audio);
    assert_eq!(got[0], orig[0]);
    assert_eq!(got[1], orig[0]);
    assert_eq!(got[2], orig[1]);
}

fn pcm_ramp(frames: usize, channels: usize, seed: i32) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..frames {
        for c in 0..channels {
            let s = ((i as i32 * 37 + c as i32 * 1000 + seed) % 30000) as i16;
            v.extend_from_slice(&s.to_le_bytes());
        }
    }
    v
}

#[test]
fn aaf_embedded_trimmed_audio() {
    let (p, s) = sample(FrameRate::FPS_25, false);
    let handles = FrameRate::FPS_25.tick_of(5);
    let needs = audio_needs(&p, s, &NeedOptions { handles, ..Default::default() });
    assert_eq!(needs.len(), 3);
    let a = needs.iter().find(|n| n.path.as_deref() == Some("/media/A001.mov")).unwrap();
    // A001 is used at source 20..120 and 300..360 (+ 5 frame handles, plus the crossfade's 5 frames)
    assert_eq!(a.start, FrameRate::FPS_25.tick_of(15));
    assert_eq!(a.end, FrameRate::FPS_25.tick_of(365));
    let mut essence = Vec::new();
    for (k, n) in needs.iter().enumerate() {
        let frames = ((n.end - n.start).0 / (filmcraft_time::TICKS_PER_SECOND / 48_000)) as u64;
        essence.push(AudioEssence {
            key: n.key,
            channel: None,
            start: n.start,
            frames,
            sample_rate: 48_000,
            bits: 16,
            channels: 2,
            data: EssenceData::Embedded(pcm_ramp(frames as usize, 2, k as i32)),
            effects_rendered: false,
        });
    }
    let opts = AafOptions { media: MediaOptions { essence: essence.clone(), ..Default::default() }, ..Default::default() };
    let (imp, extracted, rep) = aaf_rt(&p, s, &opts);
    assert_eq!(extracted.len(), 3, "{rep}");
    let n = only_seq(&imp);
    for e in &extracted {
        let (pcm, ch, sr, bits) = filmcraft_interchange::wav::parse_wav(&e.wav).unwrap();
        assert_eq!((ch, sr, bits), (2, 48_000, 16));
        assert!(essence.iter().any(|x| matches!(&x.data, EssenceData::Embedded(d) if *d == pcm)));
        assert!(e.path.starts_with("/proj/rt Media/"), "{}", e.path);
        let it = imp.project.item(e.item).unwrap().as_media().unwrap();
        assert!(matches!(&it.media, MediaRef::File { path } if *path == e.path));
    }
    // the audio clips now point into the trimmed essence: source times shift by the trim start
    let q = imp.project.sequence(n).unwrap();
    let orig = p.sequence(s).unwrap();
    for (t, ot) in q.audio_tracks.iter().zip(&orig.audio_tracks) {
        for (c, oc) in t.items.iter().zip(&ot.items) {
            let need = needs.iter().find(|x| x.item == oc.item).unwrap();
            assert_eq!(c.source_in, oc.source_in - need.start);
            assert_eq!((c.start, c.duration), (oc.start, oc.duration));
        }
    }
    // video still links to the original movies
    assert_eq!(structure(&imp.project, n, TrackKind::Video), structure(&p, s, TrackKind::Video));
}

#[test]
fn aaf_linked_consolidated_audio_and_mixdown() {
    let (p, s) = sample(FrameRate::FPS_25, false);
    let needs = audio_needs(&p, s, &NeedOptions { per_clip: true, ..Default::default() });
    assert_eq!(needs.len(), 4, "one range per audio clip");
    let essence: Vec<AudioEssence> = needs
        .iter()
        .flat_map(|n| {
            (0..2).map(move |c| AudioEssence {
                key: n.key,
                channel: Some(c),
                start: n.start,
                frames: 4800,
                sample_rate: 48_000,
                bits: 24,
                channels: 1,
                data: EssenceData::File { path: format!("/out/{:?}_{c}.wav", n.key).replace(['(', ')'], "") },
                effects_rendered: true,
            })
        })
        .collect();
    let mix = MixdownVideo { path: "/out/mixdown.mxf".into(), start: Tick::ZERO, duration: FrameRate::FPS_25.tick_of(280), width: 1920, height: 1080 };
    let opts = AafOptions { media: MediaOptions { essence, breakout_to_mono: true, mixdown_video: Some(mix), ..Default::default() }, ..Default::default() };
    let (imp, extracted, rep) = aaf_rt(&p, s, &opts);
    assert!(extracted.is_empty());
    let n = only_seq(&imp);
    let q = imp.project.sequence(n).unwrap();
    assert_eq!(q.video_tracks.len(), 1);
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert_eq!(q.video_tracks[0].items[0].duration, FrameRate::FPS_25.tick_of(280));
    assert_eq!(structure(&imp.project, n, TrackKind::Video)[0].clips[0].media, "/out/mixdown.mxf");
    assert_eq!(q.audio_tracks.len(), 8);
    let paths: Vec<String> = q.audio_tracks.iter().flat_map(|t| t.items.iter().map(|c| media_key(&imp.project, c.item))).collect();
    assert!(paths.iter().all(|p| p.starts_with("/out/Clip")), "{paths:?} {rep}");
    // rendered clip audio carries no gain any more
    for t in &q.audio_tracks {
        for c in &t.items {
            assert!(c.effect("volume").and_then(|e| e.param("level")).is_none_or(|p| !p.is_animated() && p.value.as_f64() == Some(0.0)));
        }
    }
}

#[test]
fn omf_round_trip_audio_only() {
    for rate in [FrameRate::FPS_25, FrameRate::FPS_29_97] {
        let (p, s) = sample(rate, rate.is_ntsc());
        let needs = audio_needs(&p, s, &NeedOptions { handles: rate.tick_of(10), ..Default::default() });
        let essence: Vec<AudioEssence> = needs
            .iter()
            .enumerate()
            .map(|(k, n)| {
                let frames = ((n.end - n.start).0 / (filmcraft_time::TICKS_PER_SECOND / 48_000)) as u64;
                AudioEssence {
                    key: n.key,
                    channel: None,
                    start: n.start,
                    frames,
                    sample_rate: 48_000,
                    bits: 16,
                    channels: 2,
                    data: EssenceData::Embedded(pcm_ramp(frames as usize, 2, k as i32 * 7)),
                    effects_rendered: false,
                }
            })
            .collect();
        let opts = OmfOptions { media: MediaOptions { essence: essence.clone(), ..Default::default() }, ..Default::default() };
        let (bytes, rep) = omf::export(&p, s, &opts).unwrap();
        assert!(rep.mentions("markers"), "{rep}");
        assert!(omf::sniff(&bytes));
        assert_eq!(detect(&bytes, Some("omf")), Some(Format::Omf));
        let (imp, extracted, rep) = omf::import(&bytes, &ImportOptions { base_dir: Some("/x".into()), name: Some("o".into()), ..Default::default() }).unwrap();
        assert_eq!(extracted.len(), 3, "{rep}");
        let n = only_seq(&imp);
        let q = imp.project.sequence(n).unwrap();
        assert!(q.video_tracks.iter().all(|t| t.items.is_empty()), "OMF exports are audio only");
        assert_eq!(q.start_timecode, rate.timecode_base() * 3600);
        assert_eq!(q.settings.frame_rate, rate);
        let orig = p.sequence(s).unwrap();
        assert_eq!(q.audio_tracks.len(), orig.audio_tracks.len());
        for (t, ot) in q.audio_tracks.iter().zip(&orig.audio_tracks) {
            assert_eq!(t.items.len(), ot.items.len());
            for (c, oc) in t.items.iter().zip(&ot.items) {
                let need = needs.iter().find(|x| x.item == oc.item).unwrap();
                assert_eq!((c.start, c.duration, c.source_in), (oc.start, oc.duration, oc.source_in - need.start));
            }
            assert_eq!(t.transitions.len(), ot.transitions.len());
            for (x, ox) in t.transitions.iter().zip(&ot.transitions) {
                assert_eq!((x.start, x.duration, &x.effect.effect, x.align), (ox.start, ox.duration, &ox.effect.effect, ox.align));
            }
        }
        assert!(close(level(&imp.project, n, 0, 1).value.as_f64().unwrap(), -6.0));
        assert_eq!(level(&imp.project, n, 1, 0).keyframes.len(), 3);
        for e in &extracted {
            let (pcm, ..) = filmcraft_interchange::wav::parse_wav(&e.wav).unwrap();
            assert!(essence.iter().any(|x| matches!(&x.data, EssenceData::Embedded(d) if *d == pcm)));
        }
        // the media keeps its source timecode
        let a = imp.project.items.values().find(|i| i.name == "A001.mov").expect("master mob name");
        assert_eq!(a.as_media().unwrap().info.start_timecode, Some(rate.timecode_base() * 3600 + 12));
    }
}

#[test]
fn omf_separate_files_and_breakout() {
    let (p, s) = sample(FrameRate::FPS_25, false);
    let needs = audio_needs(&p, s, &NeedOptions::default());
    let essence: Vec<AudioEssence> = needs
        .iter()
        .flat_map(|n| {
            (0..2u32).map(move |c| AudioEssence {
                key: n.key,
                channel: Some(c),
                start: n.start,
                frames: 1000,
                sample_rate: 48_000,
                bits: 24,
                channels: 1,
                data: EssenceData::File { path: format!("/omf/media/{}_{c}.aif", n.item.0) },
                effects_rendered: false,
            })
        })
        .collect();
    let opts = OmfOptions { media: MediaOptions { essence, breakout_to_mono: true, ..Default::default() }, ..Default::default() };
    let (bytes, _) = omf::export(&p, s, &opts).unwrap();
    let (imp, extracted, _) = omf::import(&bytes, &ImportOptions::default()).unwrap();
    assert!(extracted.is_empty());
    let n = only_seq(&imp);
    let q = imp.project.sequence(n).unwrap();
    assert_eq!(q.audio_tracks.len(), 8);
    for t in &q.audio_tracks {
        assert_eq!(t.channels, AudioChannels::Mono);
        for c in &t.items {
            let k = media_key(&imp.project, c.item);
            assert!(k.starts_with("/omf/media/") && k.ends_with(".aif"), "{k}");
            let m = imp.project.item(c.item).unwrap().as_media().unwrap();
            assert_eq!(m.info.audio().map(|a| (a.channels, a.sample_rate)), Some((1, 48_000)));
        }
    }
}

#[test]
fn empty_and_damaged_documents() {
    let mut p = Project::new("e");
    let s = sequence(&mut p, "Empty", FrameRate::FPS_24, false);
    for small in [false, true] {
        let (bytes, _) = aaf::export(&p, s, &AafOptions { small_sectors: small, ..Default::default() }).unwrap();
        let (imp, _, _) = aaf::import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(imp.sequences.len(), 1);
    }
    let (bytes, _) = omf::export(&p, s, &OmfOptions::default()).unwrap();
    assert_eq!(omf::import(&bytes, &ImportOptions::default()).unwrap().0.sequences.len(), 1);
    assert!(aaf::import(b"not an aaf", &ImportOptions::default()).is_err());
    assert!(omf::import(b"not an omf", &ImportOptions::default()).is_err());
    assert!(!aaf::sniff(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1 but short"));

    // truncation and corruption: errors, never panics
    let (p, s) = sample(FrameRate::FPS_25, false);
    let (aaf_bytes, _) = aaf::export(&p, s, &AafOptions { small_sectors: true, ..Default::default() }).unwrap();
    let needs = audio_needs(&p, s, &NeedOptions::default());
    let essence = needs
        .iter()
        .map(|n| AudioEssence {
            key: n.key,
            channel: None,
            start: n.start,
            frames: 100,
            sample_rate: 48_000,
            bits: 16,
            channels: 2,
            data: EssenceData::Embedded(pcm_ramp(100, 2, 1)),
            effects_rendered: false,
        })
        .collect();
    let (omf_bytes, _) = omf::export(&p, s, &OmfOptions { media: MediaOptions { essence, ..Default::default() }, ..Default::default() }).unwrap();
    for (bytes, is_aaf) in [(&aaf_bytes, true), (&omf_bytes, false)] {
        for cut in (0..bytes.len()).step_by(97) {
            let part = &bytes[..cut];
            let _ = if is_aaf { aaf::import(part, &ImportOptions::default()).map(|_| ()) } else { omf::import(part, &ImportOptions::default()).map(|_| ()) };
        }
        let mut seed = 0xA5A5_1234_5678_9ABCu64;
        for _ in 0..400 {
            let mut g = bytes.clone();
            for _ in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed as usize) % g.len();
                g[at] = (seed >> 40) as u8;
            }
            let _ = if is_aaf { aaf::import(&g, &ImportOptions::default()).map(|_| ()) } else { omf::import(&g, &ImportOptions::default()).map(|_| ()) };
        }
    }
}

#[test]
fn unsupported_clips_become_gaps_with_a_report() {
    let mut p = Project::new("g");
    let s = sequence(&mut p, "S", FrameRate::FPS_25, false);
    let g = generator(&mut p, "Bars", filmcraft_media::Generator::BarsAndTone);
    let a = media(&mut p, "/m/a.mov", true, true, FrameRate::FPS_25);
    clip(&mut p, s, TrackKind::Video, 0, g, 0, 25, 0);
    clip(&mut p, s, TrackKind::Video, 0, a, 25, 25, 0);
    let (bytes, rep) = aaf::export(&p, s, &AafOptions::default()).unwrap();
    assert!(rep.mentions("synthetic"), "{rep}");
    let (imp, _, _) = aaf::import(&bytes, &ImportOptions::default()).unwrap();
    let n = only_seq(&imp);
    let v = &imp.project.sequence(n).unwrap().video_tracks[0];
    assert_eq!(v.items.len(), 1);
    assert_eq!(v.items[0].start, FrameRate::FPS_25.tick_of(25));
}

// ---- nested sequences
//
// Premiere Pro 26.5.2 (observed in the files it exports): in AAF a nested sequence is a composition
// of its own that clips point at; in OMF its sound is rendered into the document as one clip named
// after the nested sequence.

/// "Outer": a media clip, then a nest ("Inner", picture and linked sound, trimmed in, -4 dB on
/// its sound clip), then the same nest again. "Inner": two clips with a dissolve, a second audio
/// track, and a nest of its own ("Deep").
fn nested(rate: FrameRate) -> (Project, [ItemId; 3]) {
    let mut p = Project::new("n");
    let a = media(&mut p, "/media/A001.mov", true, true, rate);
    let b = media(&mut p, "/media/B002.mov", true, true, rate);
    let m = media(&mut p, "/media/music.wav", false, true, rate);
    let deep = sequence(&mut p, "Deep", rate, false);
    clip(&mut p, deep, TrackKind::Video, 0, b, 0, 50, 200);
    let inner = sequence(&mut p, "Inner", rate, false);
    let i1 = clip(&mut p, inner, TrackKind::Video, 0, a, 0, 60, 10);
    let i2 = clip(&mut p, inner, TrackKind::Video, 0, b, 60, 60, 100);
    transition(&mut p, inner, TrackKind::Video, 0, "cross_dissolve", Some(i1), Some(i2), 54, 12, TransitionAlign::CenterAtCut);
    clip(&mut p, inner, TrackKind::Video, 1, deep, 20, 30, 5);
    let ia = clip(&mut p, inner, TrackKind::Audio, 0, a, 0, 60, 10);
    clip(&mut p, inner, TrackKind::Audio, 1, m, 0, 120, 0);
    link(&mut p, inner, &[i1, ia]);
    let outer = sequence(&mut p, "Outer", rate, false);
    let v0 = clip(&mut p, outer, TrackKind::Video, 0, a, 0, 40, 0);
    let a0 = clip(&mut p, outer, TrackKind::Audio, 0, a, 0, 40, 0);
    let v1 = clip(&mut p, outer, TrackKind::Video, 0, inner, 40, 70, 15);
    let a1 = clip(&mut p, outer, TrackKind::Audio, 0, inner, 40, 70, 15);
    let v2 = clip(&mut p, outer, TrackKind::Video, 1, inner, 150, 30, 0);
    link(&mut p, outer, &[v0, a0]);
    link(&mut p, outer, &[v1, a1]);
    let (_, c) = p.sequence_mut(outer).unwrap().find_item_mut(a1).unwrap();
    c.effect_mut("volume").unwrap().params.insert("level".into(), Param::new(ParamValue::Float(-4.0)));
    let _ = v2;
    (p, [outer, inner, deep])
}

fn sequence_named(p: &Project, name: &str) -> ItemId {
    let mut found = p.items.values().filter(|i| i.name == name && matches!(i.kind, ItemKind::Sequence(_)));
    let id = found.next().unwrap_or_else(|| panic!("no sequence {name}")).id;
    assert!(found.next().is_none(), "{name} was imported more than once");
    id
}

#[test]
fn aaf_nested_sequences_round_trip_as_compositions() {
    for rate in [FrameRate::FPS_25, FrameRate::FPS_23_976] {
        let (p, [outer, inner, deep]) = nested(rate);
        let (bytes, rep) = aaf::export(&p, outer, &AafOptions::default()).unwrap();
        assert!(!rep.has_warnings(), "{rep}");
        let (imp, _, rep) = aaf::import(&bytes, &ImportOptions { base_dir: Some("/proj".into()), ..Default::default() }).unwrap();
        assert!(!rep.has_warnings(), "{rep}");
        // one sequence is imported as the edit; the nested ones are sequences in the project that
        // it uses (each once, however many clips use it)
        let q = &imp.project;
        let n = only_seq(&imp);
        assert_eq!(q.item(n).unwrap().name, "Outer");
        let (ni, nd) = (sequence_named(q, "Inner"), sequence_named(q, "Deep"));
        for kind in [TrackKind::Video, TrackKind::Audio] {
            assert_eq!(structure(q, n, kind), structure(&p, outer, kind), "{rate:?} Outer {kind:?}");
            assert_eq!(structure(q, ni, kind), structure(&p, inner, kind), "{rate:?} Inner {kind:?}");
            assert_eq!(structure(q, nd, kind), structure(&p, deep, kind), "{rate:?} Deep {kind:?}");
        }
        let top = q.sequence(n).unwrap();
        assert_eq!((top.video_tracks[0].items[1].item, top.video_tracks[1].items[0].item, top.audio_tracks[0].items[1].item), (ni, ni, ni));
        assert_eq!(q.sequence(ni).unwrap().video_tracks[1].items[0].item, nd);
        // the nest's picture and sound are linked again, and the level on its sound clip is kept
        assert_eq!(links(q, n), links(&p, outer));
        assert!(close(level(q, n, 0, 1).value.as_f64().unwrap(), -4.0));
        // the nested sequences keep their own settings
        assert_eq!(q.sequence(ni).unwrap().settings.frame_rate, rate);
        // reproducible
        assert_eq!(bytes, aaf::export(&p, outer, &AafOptions::default()).unwrap().0);
    }
}

#[test]
fn aaf_nested_sequence_of_another_rate_and_broken_out_to_mono() {
    // the nest runs at 25 fps and 44.1 kHz inside a 23.976 fps, 48 kHz sequence: a clip's start in
    // it counts the nested composition's edit units
    let (mut p, [outer, inner, _]) = nested(FrameRate::FPS_23_976);
    {
        let q = p.sequence_mut(inner).unwrap();
        q.settings.frame_rate = FrameRate::FPS_25;
        q.settings.sample_rate = 44_100;
    }
    let before = (structure(&p, outer, TrackKind::Video), structure(&p, outer, TrackKind::Audio));
    for breakout in [false, true] {
        let opts = AafOptions { media: MediaOptions { breakout_to_mono: breakout, ..Default::default() }, ..Default::default() };
        let (bytes, _) = aaf::export(&p, outer, &opts).unwrap();
        let (imp, _, rep) = aaf::import(&bytes, &ImportOptions::default()).unwrap();
        let (q, n) = (&imp.project, only_seq(&imp));
        // picture starts are stored in frames of the nested composition: within half of one
        let picture = structure(q, n, TrackKind::Video);
        assert_eq!(picture.len(), before.0.len(), "{rep}");
        for (t, o) in picture.iter().zip(&before.0) {
            assert_eq!(t.clips.len(), o.clips.len());
            for (c, o) in t.clips.iter().zip(&o.clips) {
                assert_eq!((c.start, c.dur, &c.media), (o.start, o.dur, &o.media));
                let grid = if c.media == "seq:Inner" { FrameRate::FPS_25.frame_duration() } else { Tick(1) };
                assert!((c.src - o.src).abs() <= Tick(grid.0 / 2), "{c:?} {o:?}");
            }
        }
        let ni = sequence_named(q, "Inner");
        assert_eq!((q.sequence(ni).unwrap().settings.frame_rate, q.sequence(ni).unwrap().settings.sample_rate), (FrameRate::FPS_25, 44_100));
        let sound = structure(q, n, TrackKind::Audio);
        if breakout {
            // each stereo track became two mono tracks; the nest is on both, at the same place
            assert_eq!(sound.len(), 2);
            assert_eq!(sound[0], sound[1]);
            assert_eq!(
                sound[0].clips.iter().map(|c| (c.start, c.dur, c.media.as_str())).collect::<Vec<_>>(),
                before.1[0].clips.iter().map(|c| (c.start, c.dur, c.media.as_str())).collect::<Vec<_>>()
            );
        } else {
            // sound starts are stored in samples of the nested composition: within one sample
            assert_eq!(sound.len(), before.1.len());
            for (c, o) in sound[0].clips.iter().zip(&before.1[0].clips) {
                assert_eq!((c.start, c.dur, &c.media), (o.start, o.dur, &o.media));
                assert!((c.src - o.src).abs() <= Tick(filmcraft_time::TICKS_PER_SECOND / 44_100), "{c:?} {o:?}");
            }
        }
    }
}

/// A project that claims a sequence is inside itself (FilmCraft refuses to make one, a damaged
/// file may hold one) exports without looping: the clip is left as a gap and named in the report.
#[test]
fn a_sequence_inside_itself_exports_as_a_gap() {
    let rate = FrameRate::FPS_25;
    let (mut p, [outer, inner, deep]) = nested(rate);
    clip(&mut p, deep, TrackKind::Video, 1, outer, 0, 20, 0);
    clip(&mut p, deep, TrackKind::Audio, 0, outer, 0, 20, 0);
    clip(&mut p, inner, TrackKind::Audio, 2, inner, 0, 20, 0);
    let (bytes, rep) = aaf::export(&p, outer, &AafOptions::default()).unwrap();
    assert!(rep.mentions("inside itself"), "{rep}");
    let (imp, _, _) = aaf::import(&bytes, &ImportOptions::default()).unwrap();
    let q = &imp.project;
    assert!(q.sequence(sequence_named(q, "Deep")).unwrap().video_tracks[1].items.is_empty());
    // the lists of what to prepare end too
    for nests in [NestNeeds::Skip, NestNeeds::Inside, NestNeeds::Render] {
        let needs = audio_needs(&p, outer, &NeedOptions { nests, ..Default::default() });
        assert!(needs.len() < 16, "{nests:?}: {}", needs.len());
    }
    let (_, rep) = omf::export(&p, outer, &OmfOptions::default()).unwrap();
    assert!(rep.mentions("was not rendered"), "{rep}");
}

#[test]
fn audio_needs_of_nested_sequences() {
    let rate = FrameRate::FPS_25;
    let (p, [outer, inner, _]) = nested(rate);
    let media_named = |name: &str| p.items.values().find(|i| i.name == name).unwrap().id;
    let (a, m) = (media_named("A001.mov"), media_named("music.wav"));
    let nest_clip = p.sequence(outer).unwrap().audio_tracks[0].items[1].clone();
    // by default a nest needs nothing (as before)
    let plain = audio_needs(&p, outer, &NeedOptions::default());
    assert_eq!(plain.iter().map(|n| n.key).collect::<Vec<_>>(), [EssenceKey::Media(a)]);
    // inside: the media of the clips in the nested sequence, each item once over all its uses
    let inside = audio_needs(&p, outer, &NeedOptions { nests: NestNeeds::Inside, ..Default::default() });
    assert_eq!(inside.iter().map(|n| n.key).collect::<Vec<_>>(), [EssenceKey::Media(a), EssenceKey::Media(m)]);
    assert_eq!((inside[0].start, inside[0].end), (Tick::ZERO, rate.tick_of(70)), "0..40 in Outer and 10..70 in Inner");
    // render: one range of the nested sequence per clip, with handles, not past the sequence's end
    let handles = rate.tick_of(10);
    let render = audio_needs(&p, outer, &NeedOptions { nests: NestNeeds::Render, handles, ..Default::default() });
    let need = render.iter().find(|n| n.key == EssenceKey::Clip(nest_clip.id)).expect("the nest clip");
    assert_eq!((need.item, need.path.as_deref()), (inner, None));
    assert_eq!((need.start, need.end), (rate.tick_of(5), rate.tick_of(95)));
    assert_eq!((need.channels, need.sample_rate), (2, 48_000));
    let whole = audio_needs(&p, outer, &NeedOptions { nests: NestNeeds::Render, handles: rate.tick_of(100), ..Default::default() });
    let need = whole.iter().find(|n| n.key == EssenceKey::Clip(nest_clip.id)).unwrap();
    assert_eq!((need.start, need.end), (Tick::ZERO, p.sequence(inner).unwrap().duration()));
}

#[test]
fn omf_carries_the_rendered_sound_of_a_nested_sequence() {
    let rate = FrameRate::FPS_25;
    let (p, [outer, _, _]) = nested(rate);
    let nest_clip = p.sequence(outer).unwrap().audio_tracks[0].items[1].clone();
    let needs = audio_needs(&p, outer, &NeedOptions { handles: rate.tick_of(5), nests: NestNeeds::Render, ..Default::default() });
    assert_eq!(needs.len(), 2);
    let essence: Vec<AudioEssence> = needs
        .iter()
        .enumerate()
        .map(|(k, n)| {
            let frames = ((n.end - n.start).0 / (filmcraft_time::TICKS_PER_SECOND / 48_000)) as u64;
            AudioEssence {
                key: n.key,
                channel: None,
                start: n.start,
                frames,
                sample_rate: 48_000,
                bits: 16,
                channels: 2,
                data: EssenceData::Embedded(pcm_ramp(frames as usize, 2, k as i32 * 11)),
                effects_rendered: matches!(n.key, EssenceKey::Clip(_)),
            }
        })
        .collect();
    let opts = OmfOptions { media: MediaOptions { essence: essence.clone(), ..Default::default() }, ..Default::default() };
    let (bytes, rep) = omf::export(&p, outer, &opts).unwrap();
    assert!(!rep.mentions("nested") && !rep.mentions("gap"), "{rep}");
    let (imp, extracted, rep) = omf::import(&bytes, &ImportOptions { base_dir: Some("/x".into()), name: Some("o".into()), ..Default::default() }).unwrap();
    let q = imp.project.sequence(only_seq(&imp)).unwrap();
    let t = &q.audio_tracks[0];
    assert_eq!(t.items.len(), 2, "{rep}");
    // the nest is one clip named after the nested sequence, playing the rendered sound from where
    // the clip starts in it (the render begins 5 frames earlier: the handle)
    let c = &t.items[1];
    assert_eq!((c.name.as_str(), imp.project.item(c.item).unwrap().name.as_str()), ("Inner", "Inner"));
    assert_eq!((c.start, c.duration, c.source_in), (nest_clip.start, nest_clip.duration, rate.tick_of(5)));
    assert!(matches!(imp.project.item(c.item).unwrap().kind, ItemKind::Media(_)), "media in the document, not a sequence");
    // the clip's level is in the rendered sound, not written again
    assert!(close(level(&imp.project, only_seq(&imp), 0, 1).value.as_f64().unwrap(), 0.0));
    let nest_pcm = essence.iter().find(|e| e.key == EssenceKey::Clip(nest_clip.id)).map(|e| match &e.data {
        EssenceData::Embedded(d) => d.clone(),
        EssenceData::File { .. } => unreachable!(),
    });
    assert!(extracted.iter().any(|e| filmcraft_interchange::wav::parse_wav(&e.wav).map(|w| w.0) == nest_pcm));

    // without the rendered sound the nest is a gap, and the report says which sequence
    let (bytes, rep) = omf::export(&p, outer, &OmfOptions::default()).unwrap();
    assert!(rep.mentions("\"Inner\" was not rendered"), "{rep}");
    let (imp, _, _) = omf::import(&bytes, &ImportOptions::default()).unwrap();
    assert_eq!(imp.project.sequence(only_seq(&imp)).unwrap().audio_tracks[0].items.len(), 1);
}
