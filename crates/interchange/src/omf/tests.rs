//! Structural checks of written OMF files: the Bento label and TOC, and the required properties of
//! the OMF Interchange 2.0 classes FilmCraft writes.

use std::collections::HashMap;

use filmcraft_project::{Project, SequenceSettings, TrackKind, TransitionAlign, TransitionId, find_effect};
use filmcraft_time::{FrameRate, TimeRange};

use super::bento::{self, Container};
use super::*;
use crate::essence::{AudioEssence, EssenceData, NeedOptions, audio_needs};

fn project() -> (Project, ItemId) {
    let mut p = Project::new("t");
    let info = filmcraft_media::MediaInfo {
        name: "a.wav".into(),
        kind: filmcraft_media::MediaKind::AudioOnly,
        duration: FrameRate::FPS_25.tick_of(2500),
        video: None,
        audio_streams: vec![filmcraft_media::AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: String::new(), bits_per_sample: Some(16) }],
        container: String::new(),
        start_timecode: None,
        file_size: None,
    };
    let a = p.add_item(
        "a.wav",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::File { path: "/m/a.wav".into() },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let r = FrameRate::FPS_25;
    let s = p.new_sequence("S", SequenceSettings { frame_rate: r, ..Default::default() }, 1, 2, None);
    let mut ids = Vec::new();
    for (start, dur, src) in [(0, 50, 10), (50, 50, 300)] {
        let ti = p.make_track_item(a, TrackKind::Audio, r.tick_of(start), TimeRange::new(r.tick_of(src), r.tick_of(dur)), r).unwrap();
        ids.push(ti.id);
        p.sequence_mut(s).unwrap().audio_tracks[0].items.push(ti);
    }
    let id = TransitionId(p.alloc_id());
    let q = p.sequence_mut(s).unwrap();
    q.audio_tracks[0].transitions.push(filmcraft_project::Transition {
        id,
        effect: find_effect("constant_power").unwrap().instance(),
        start: r.tick_of(45),
        duration: r.tick_of(10),
        from: Some(ids[0]),
        to: Some(ids[1]),
        align: TransitionAlign::CenterAtCut,
        reverse: false,
    });
    q.audio_tracks[0].items[0].gain_db = 6.0;
    (p, s)
}

#[test]
fn written_files_have_the_required_structure() {
    let (p, s) = project();
    let needs = audio_needs(&p, s, &NeedOptions::default());
    let essence = needs
        .iter()
        .map(|n| AudioEssence {
            key: n.key,
            channel: None,
            start: n.start,
            frames: 480,
            sample_rate: 48_000,
            bits: 16,
            channels: 2,
            data: EssenceData::Embedded(vec![7; 480 * 4]),
            effects_rendered: false,
        })
        .collect();
    let (bytes, _) = export(&p, s, &OmfOptions { media: MediaOptions { essence, ..Default::default() }, ..Default::default() }).unwrap();
    // label
    let l = &bytes[bytes.len() - bento::LABEL_SIZE..];
    assert_eq!(&l[..8], &bento::MAGIC);
    assert_eq!(u16::from_be_bytes([l[12], l[13]]), 1, "Bento major version");
    let c = Container::open(&bytes).unwrap();
    let class = |o: u32| c.get(o, "OMFI:OOBJ:ObjClass").map(|v| String::from_utf8_lossy(&v).to_string());
    let mut by_class: HashMap<String, Vec<u32>> = HashMap::new();
    for &o in c.objects.keys() {
        if let Some(k) = class(o) {
            by_class.entry(k).or_default().push(o);
        }
    }
    let required: &[(&str, &[&str])] = &[
        (
            "HEAD",
            &["OMFI:HEAD:ByteOrder", "OMFI:HEAD:LastModified", "OMFI:HEAD:Version", "OMFI:HEAD:Mobs", "OMFI:HEAD:MediaData", "OMFI:HEAD:DefinitionObjects"],
        ),
        ("CMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:MOBJ:LastModified", "OMFI:MOBJ:CreationTime"]),
        ("MMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:MOBJ:LastModified", "OMFI:MOBJ:CreationTime"]),
        ("SMOB", &["OMFI:MOBJ:MobID", "OMFI:MOBJ:Slots", "OMFI:SMOB:MediaDescription"]),
        ("MSLT", &["OMFI:MSLT:Segment", "OMFI:MSLT:EditRate", "OMFI:MSLT:TrackDesc"]),
        ("TRKD", &["OMFI:TRKD:Origin", "OMFI:TRKD:TrackID"]),
        ("SEQU", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:SEQU:Components"]),
        ("SCLP", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:SCLP:SourceID", "OMFI:SCLP:SourceTrackID", "OMFI:SCLP:StartTime"]),
        ("FILL", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length"]),
        ("TRAN", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:TRAN:CutPoint", "OMFI:TRAN:Effect"]),
        ("EFFE", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:EFFE:EffectKind"]),
        ("ESLT", &["OMFI:ESLT:ArgID", "OMFI:ESLT:ArgValue"]),
        ("CVAL", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:CVAL:Value"]),
        ("EDEF", &["OMFI:EDEF:EffectID"]),
        ("DDEF", &["OMFI:DDEF:DataKindID"]),
        ("TCCP", &["OMFI:CPNT:DataKind", "OMFI:CPNT:Length", "OMFI:TCCP:Start", "OMFI:TCCP:FPS", "OMFI:TCCP:Drop"]),
        ("WAVD", &["OMFI:MDFL:IsOMFI", "OMFI:MDFL:SampleRate", "OMFI:MDFL:Length", "OMFI:WAVD:Summary"]),
        ("WAVE", &["OMFI:MDAT:MobID", "OMFI:WAVE:Data"]),
        ("IDNT", &["OMFI:IDNT:CompanyName", "OMFI:IDNT:ProductName"]),
    ];
    for (k, props) in required {
        let Some(objs) = by_class.get(*k) else {
            assert_eq!(*k, "FILL", "no {k} object");
            continue;
        };
        for &o in objs {
            for prop in *props {
                assert!(c.get(o, prop).is_some(), "{k} {o} lacks {prop}");
            }
        }
    }
    assert_eq!(by_class["HEAD"].len(), 1);
    assert_eq!(by_class["CMOB"].len(), 1);
    let head = by_class["HEAD"][0];
    assert_eq!(c.get(head, "OMFI:HEAD:Version"), Some(vec![2, 0]));
    assert_eq!(c.get(head, "OMFI:HEAD:ByteOrder"), Some(b"MM".to_vec()));
    // object references resolve to objects of the right class
    let refs = |o: u32, p: &str| -> Vec<u32> {
        let v = c.get(o, p).unwrap();
        let n = u16::from_be_bytes([v[0], v[1]]) as usize;
        v[2..].as_chunks::<4>().0.iter().take(n).map(|k| c.resolve(o, u32::from_be_bytes([k[0], k[1], k[2], k[3]]))).collect()
    };
    let one = |o: u32, p: &str| -> u32 { c.resolve(o, u32::from_be_bytes(c.get(o, p).unwrap()[..4].try_into().unwrap())) };
    for m in refs(head, "OMFI:HEAD:Mobs") {
        assert!(matches!(class(m).as_deref(), Some("CMOB" | "MMOB" | "SMOB")));
        for s in refs(m, "OMFI:MOBJ:Slots") {
            assert_eq!(class(s).as_deref(), Some("MSLT"));
            assert_eq!(class(one(s, "OMFI:MSLT:TrackDesc")).as_deref(), Some("TRKD"));
        }
    }
    // embedded media: WAVE data is a WAVE file of the file mob it names
    for w in &by_class["WAVE"] {
        let data = c.get(*w, "OMFI:WAVE:Data").unwrap();
        let (pcm, ch, sr, bits) = crate::wav::parse_wav(&data).unwrap();
        assert_eq!((pcm.len(), ch, sr, bits), (480 * 4, 2, 48_000, 16));
        let id = c.get(*w, "OMFI:MDAT:MobID").unwrap();
        assert!(by_class["SMOB"].iter().any(|s| c.get(*s, "OMFI:MOBJ:MobID") == Some(id.clone())));
    }
    // the composition's sound sequence: clip, transition, gain effect around a clip
    let cm = by_class["CMOB"][0];
    let slots = refs(cm, "OMFI:MOBJ:Slots");
    assert_eq!(class(one(slots[0], "OMFI:MSLT:Segment")).as_deref(), Some("TCCP"));
    let seq = one(slots[1], "OMFI:MSLT:Segment");
    let comps = refs(seq, "OMFI:SEQU:Components");
    let classes: Vec<String> = comps.iter().map(|&o| class(o).unwrap()).collect();
    assert_eq!(classes, vec!["EFFE", "TRAN", "SCLP"]);
    let len = |o: u32| i32::from_be_bytes(c.get(o, "OMFI:CPNT:Length").unwrap()[..4].try_into().unwrap()) as i64;
    let samples = |f: i64| f * 48_000 / 25;
    assert_eq!(len(comps[0]), samples(55));
    assert_eq!(len(comps[1]), samples(10));
    assert_eq!(len(comps[2]), samples(55));
    assert_eq!(len(seq), samples(100));
    assert_eq!(c.get(one(comps[0], "OMFI:EFFE:EffectKind"), "OMFI:EDEF:EffectID"), Some(b"omfi:effect:MonoAudioGain\0".to_vec()));
}

#[test]
fn bento_values_round_trip() {
    let mut w = bento::Writer::new();
    let a = w.new_object();
    let b = w.new_object();
    w.set(a, "Test:Small", "test:Bytes", &[1, 2]);
    w.set(a, "Test:Empty", "test:Bytes", &[]);
    w.set(b, "Test:Big", "test:Bytes", &vec![9; 100_000]);
    w.set_references(a, &[b]);
    let bytes = w.finish();
    let c = Container::open(&bytes).unwrap();
    assert_eq!(c.get(a, "Test:Small"), Some(vec![1, 2]));
    assert_eq!(c.get(a, "Test:Empty"), Some(vec![]));
    assert_eq!(c.get(b, "Test:Big").map(|v| v.len()), Some(100_000));
    assert_eq!(c.resolve(a, b), b);
    assert_eq!(c.name_of(c.id_of("Test:Big").unwrap()), Some("Test:Big"));
    assert!(Container::open(&bytes[..bytes.len() - 1]).is_err());
}
