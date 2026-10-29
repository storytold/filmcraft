use filmcraft_media::MediaKind;
use filmcraft_project::{Bin, BinEntry, ItemKind, MediaRef, TrackKind, TransitionAlign};
use filmcraft_sqlite::Value;
use filmcraft_sqlite::write::Writer;
use filmcraft_time::FrameRate;

use super::*;

/// A TEXT value.
fn t(s: &str) -> Value {
    Value::Text(s.into())
}
/// An INTEGER value.
fn n(i: i64) -> Value {
    Value::Integer(i)
}
/// A BLOB value.
fn b(v: Vec<u8>) -> Value {
    Value::Blob(v)
}
const NULL: Value = Value::Null;

/// A frame-rate blob.
fn rate(fps: f64) -> Vec<u8> {
    let mut v = fps.to_le_bytes().to_vec();
    v.extend([0u8; 8]);
    v
}
/// A resolution blob.
fn size(w: u64, h: u64) -> Vec<u8> {
    [w.to_be_bytes(), h.to_be_bytes()].concat()
}
/// A media-extents blob (start and duration in seconds).
fn extents(start: f64, dur: f64) -> Vec<u8> {
    [start.to_le_bytes(), dur.to_le_bytes()].concat()
}

/// A clip record: header, flag, then a zstd frame of one raw block holding the protobuf.
fn clip_record(dir: &str, name: &str) -> Vec<u8> {
    let mut pb = vec![0x0a, dir.len() as u8];
    pb.extend(dir.as_bytes());
    pb.push(0x12);
    pb.push(name.len() as u8);
    pb.extend(name.as_bytes());
    pb.extend([0x2a, 3, b'a', b'v', b'c']); // field 5: codec, ignored
    let mut z = vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, pb.len() as u8];
    let bh = 1u32 | ((pb.len() as u32) << 3); // last block, raw
    z.extend(&bh.to_le_bytes()[..3]);
    z.extend(&pb);
    let mut out = vec![0, 0, 0, 2];
    out.extend((z.len() as u32 + 1).to_be_bytes());
    out.push(0x81);
    out.extend(z);
    out
}

/// A Qt-serialised variant map.
fn qmap_bytes(entries: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1];
    out.extend((entries.len() as u32).to_be_bytes());
    for (k, ty, v) in entries {
        let key: Vec<u8> = k.encode_utf16().flat_map(u16::to_be_bytes).collect();
        out.extend((key.len() as u32).to_be_bytes());
        out.extend(key);
        out.extend(ty.to_be_bytes());
        out.push(0);
        if *ty == 12 {
            out.extend((v.len() as u32).to_be_bytes());
        }
        out.extend(v);
    }
    out
}

/// A row of a many-to-many link table.
fn link(owner: &str, prop: &str, idx: i64, assoc: &str) -> Vec<Value> {
    vec![t(owner), t(assoc), t(prop), n(idx)]
}

const LINK_SQL: &str = "(\"DbOwner\" uuid NOT NULL, \"DbAssociate\" uuid, \"DbPropertyName\" character varying NOT NULL, \"DbIndex\" integer NOT NULL)";

/// One item row: id, type, name, start, duration, in, media ref, path, media rate, timemap, track.
#[allow(clippy::too_many_arguments)]
fn item(id: &str, ty: &str, name: &str, start: &str, dur: i64, inn: Option<i64>, media: Option<&str>, path: Option<&str>, track: &str) -> Vec<Value> {
    vec![
        t(id),
        t(ty),
        t(name),
        t(start),
        t(&dur.to_string()),
        inn.map(|i| t(&i.to_string())).unwrap_or(NULL),
        media.map(t).unwrap_or(NULL),
        path.map(t).unwrap_or(NULL),
        if ty == "Sm2TiTransition" { NULL } else { b(rate(25.0)) },
        b(vec![2, 0x40, 0, 0, 0, 0, 0, 0, 0]),
        Value::Real(0.0),
        t(track),
    ]
}

/// A small project: a Main timeline (01:00:00:00 at 25 fps) with picture, linked sound, music, a
/// dissolve, a crossfade, a nested Sub timeline, a title and a subtitle track; a Sub timeline;
/// a compound clip; a Footage bin.
fn project() -> Vec<u8> {
    let mut w = Writer::new(1024);
    w.table("SM_Project", "CREATE TABLE \"SM_Project\" (\"SM_Project_id\" uuid NOT NULL, \"ProjectName\" character varying)", vec![vec![t("p"), t("My Edit")]]);
    w.table(
        "SM_Project_Sm2Timeline",
        &format!("CREATE TABLE \"SM_Project_Sm2Timeline\" {LINK_SQL}"),
        vec![link("p", "TimelineHandleVec", 0, "tl-main"), link("p", "TimelineHandleVec", 1, "tl-sub")],
    );
    w.table(
        "Sm2MpFolder",
        "CREATE TABLE \"Sm2MpFolder\" (\"Sm2MpFolder_id\" uuid NOT NULL, \"DbType\" character varying(128) NOT NULL, \"Name\" character varying, \"MpFolder\" uuid)",
        vec![vec![t("f-sub"), t("Sm2MpFolder"), t("Footage"), t("f-root")], vec![t("f-root"), t("Sm2MpFolder"), t("Master"), NULL]],
    );
    w.table("Sm2MpFolder_Sm2MpMedia", &format!("CREATE TABLE \"Sm2MpFolder_Sm2MpMedia\" {LINK_SQL}"), vec![link("f-sub", "MediaVec", 0, "m-a")]);
    w.table(
        "Sm2MpMedia",
        "CREATE TABLE \"Sm2MpMedia\" (\"Sm2MpMedia_id\" uuid NOT NULL, \"DbType\" character varying(128) NOT NULL, \"Name\" character varying, \"Sm2MpFolder_id\" uuid, \"Video\" uuid, \"Sequence\" uuid)",
        vec![
            vec![t("m-a"), t("Sm2MpVideoClip"), t("a.mp4"), t("f-sub"), t("v-a"), NULL],
            vec![t("m-music"), t("Sm2MpAudioClip"), t("music.wav"), t("f-root"), NULL, NULL],
            vec![t("m-main"), t("Sm2MpTimelineClip"), t("Main"), t("f-root"), NULL, NULL],
            vec![t("m-sub"), t("Sm2MpTimelineClip"), t("Sub"), t("f-sub"), NULL, NULL],
            vec![t("m-cc"), t("Sm2MpCompoundClip"), t("Compound Clip 1"), t("f-root"), NULL, t("s-cc")],
            vec![t("m-fx"), t("Sm2MpFusionClip"), t("Fusion Composition"), t("f-root"), NULL, NULL],
        ],
    );
    let time = qmap_bytes(&[("StartFrame", 2, 0i32.to_be_bytes().to_vec()), ("NumFrames", 2, 240i32.to_be_bytes().to_vec()), ("FrameRate", 12, rate(25.0))]);
    let geometry = qmap_bytes(&[("Resolution", 12, size(1280, 720))]);
    w.table(
        "BtVideoInfo",
        "CREATE TABLE \"BtVideoInfo\" (\"BtVideoInfo_id\" uuid NOT NULL, \"Clip\" BLOB, \"Time\" BLOB, \"Geometry\" BLOB, \"Sm2MpMedia_id\" uuid)",
        vec![vec![t("v-a"), b(clip_record("C:\\media", "a.mp4")), b(time), b(geometry), t("m-a")]],
    );
    let track0 = qmap_bytes(&[("SampleRate", 3, 48000u32.to_be_bytes().to_vec()), ("NumChannels", 2, 2i32.to_be_bytes().to_vec())]);
    w.table(
        "BtAudioInfo",
        "CREATE TABLE \"BtAudioInfo\" (\"BtAudioInfo_id\" uuid NOT NULL, \"Clip\" BLOB, \"TracksBA\" BLOB, \"Sm2MpMedia_id\" uuid)",
        vec![vec![t("a-a"), b(clip_record("C:\\media", "a.mp4")), b(qmap_bytes(&[("0", 12, track0)])), t("m-a")]],
    );
    w.table(
        "Sm2Timeline",
        "CREATE TABLE \"Sm2Timeline\" (\"Sm2Timeline_id\" uuid NOT NULL, \"Name\" character varying, \"Sequence\" uuid, \"Sm2MpMedia_id\" uuid)",
        // stored out of order: the project's timeline list decides
        vec![vec![t("tl-sub"), t("Sub"), t("s-sub"), t("m-sub")], vec![t("tl-main"), t("Main"), t("s-main"), t("m-main")]],
    );
    w.table(
        "Sm2Sequence",
        "CREATE TABLE \"Sm2Sequence\" (\"Sm2Sequence_id\" uuid NOT NULL, \"FrameRate\" BLOB, \"Resolution\" BLOB, \"MediaExtents\" BLOB)",
        vec![
            vec![t("s-main"), b(rate(25.0)), b(size(1920, 1080)), b(extents(3600.0, 4.0))],
            vec![t("s-sub"), b(rate(25.0)), b(vec![0; 16]), b(extents(0.0, 4.0))],
            vec![t("s-cc"), b(rate(25.0)), b(size(1920, 1080)), b(extents(0.0, 1.0))],
        ],
    );
    w.table(
        "Sm2SequenceContainer",
        "CREATE TABLE \"Sm2SequenceContainer\" (\"Sm2SequenceContainer_id\" uuid NOT NULL, \"Sm2Sequence_id\" uuid)",
        vec![vec![t("c-main"), t("s-main")], vec![t("c-sub"), t("s-sub")]],
    );
    w.table(
        "Sm2SequenceContainer_Sm2TiTrack",
        &format!("CREATE TABLE \"Sm2SequenceContainer_Sm2TiTrack\" {LINK_SQL}"),
        vec![
            link("c-main", "VideoTrackVec", 1, "tr-v2"),
            link("c-main", "VideoTrackVec", 0, "tr-v1"),
            link("c-main", "AudioTrackVec", 0, "tr-a1"),
            link("c-main", "AudioTrackVec", 1, "tr-a2"),
            link("c-main", "SubtitleTrackVec", 0, "tr-sub"),
            link("c-sub", "VideoTrackVec", 0, "tr-sub-v1"),
        ],
    );
    w.table(
        "Sm2TiTrack",
        "CREATE TABLE \"Sm2TiTrack\" (\"Sm2TiTrack_id\" uuid NOT NULL, \"Type\" integer, \"UserDefinedName\" character varying, \"Sequence\" uuid)",
        vec![
            vec![t("tr-v1"), n(0), NULL, t("s-main")],
            vec![t("tr-v2"), n(0), NULL, t("s-main")],
            vec![t("tr-a1"), n(1), t("Dialog"), t("s-main")],
            vec![t("tr-a2"), n(1), t("Music"), t("s-main")],
            vec![t("tr-sub"), n(2), NULL, t("s-main")],
            vec![t("tr-sub-v1"), n(0), NULL, t("s-sub")],
            // the compound clip's track, listed by no container
            vec![t("tr-cc-v1"), n(0), NULL, t("s-cc")],
        ],
    );
    let path = "C:\\media\\a.mp4";
    let items = vec![
        item("i1", "Sm2TiVideoClip", "a.mp4", "90000", 50, Some(10), Some("m-a"), Some(path), "tr-v1"),
        item("i2", "Sm2TiVideoClip", "a.mp4", "90050", 25, Some(100), Some("m-a"), Some(path), "tr-v1"),
        item("i3", "Sm2TiTransition", "Cross Dissolve", "90040|0000000000000000", 20, None, None, None, "tr-v1"),
        item("i4", "Sm2TiVideoClip", "Sub", "90100", 20, Some(5), Some("m-sub"), None, "tr-v2"),
        item("i5", "Sm2TiGenerator", "Text+", "90000", 25, None, None, None, "tr-v2"),
        item("i6", "Sm2TiAudioClip", "a.mp4", "90000", 50, Some(10), Some("m-a"), Some(path), "tr-a1"),
        item("i7", "Sm2TiAudioClip", "a.mp4", "90050", 25, Some(100), Some("m-a"), Some(path), "tr-a1"),
        item("i8", "Sm2TiTransition", "Cross Fade -3 dB", "90040", 20, None, None, None, "tr-a1"),
        item("i9", "Sm2TiAudioClip", "music.wav", "90000", 75, None, Some("m-music"), Some("D:/audio/music.wav"), "tr-a2"),
        item("i10", "Sm2TiSubtitle", "Hello", "90000", 25, None, None, None, "tr-sub"),
        item("i11", "Sm2TiVideoClip", "a.mp4", "0", 100, None, Some("m-a"), Some(path), "tr-sub-v1"),
        item("i12", "Sm2TiVideoClip", "a.mp4", "0", 25, None, Some("m-a"), Some(path), "tr-cc-v1"),
    ];
    w.table(
        "Sm2TiItem",
        "CREATE TABLE \"Sm2TiItem\" (\"Sm2TiItem_id\" uuid NOT NULL, \"DbType\" character varying(128), \"Name\" character varying, \"Start\" character varying, \"Duration\" character varying, \"In\" character varying, \"MediaRef\" uuid, \"MediaFilePath\" character varying, \"MediaFrameRate\" BLOB, \"MediaTimemapBA\" BLOB, \"MediaStartTime\" double precision, \"Sm2TiTrack_id\" uuid)",
        items,
    );
    // the list order is the track order; items it misses follow by start
    w.table(
        "Sm2TiItem_Sm2TiTrack",
        &format!("CREATE TABLE \"Sm2TiItem_Sm2TiTrack\" {LINK_SQL}"),
        vec![link("tr-v1", "ItemVec", 0, "i1"), link("tr-v1", "ItemVec", 1, "i3"), link("tr-v1", "ItemVec", 2, "i2"), link("tr-a1", "ItemVec", 0, "i6")],
    );
    w.finish()
}

/// The bin named `name`, at or under `b`.
fn bin_named<'a>(b: &'a Bin, name: &str) -> Option<&'a Bin> {
    if b.name == name {
        return Some(b);
    }
    b.children.iter().find_map(|c| if let BinEntry::Bin(x) = c { bin_named(x, name) } else { None })
}

/// Whether bin `b` holds item `id` itself.
fn holds(b: &Bin, id: ItemId) -> bool {
    b.children.iter().any(|c| matches!(c, BinEntry::Item(i) if *i == id))
}

/// The item named `name`.
fn by_name(p: &Project, name: &str) -> ItemId {
    p.items.values().find(|i| i.name == name).map(|i| i.id).unwrap_or_else(|| panic!("no item {name}"))
}

/// The synthetic project, imported (after checking it is recognised and named).
fn imported() -> (Imported, Report) {
    let bytes = project();
    assert!(sniff(&bytes));
    assert_eq!(project_name(&bytes).as_deref(), Some("My Edit"));
    import(&bytes, &ImportOptions::default()).unwrap()
}

const FPS25: FrameRate = FrameRate { num: 25, den: 1 };

/// Bins, Media Pool clips with their files and shape, and the timelines in the project's order.
#[test]
fn imports_bins_and_media() {
    let (imp, _) = imported();
    let p = &imp.project;
    assert_eq!(p.name, "My Edit");
    // timelines in the project's order; the compound clip is a sequence but not a top timeline
    let names: Vec<&str> = imp.sequences.iter().map(|id| p.item(*id).unwrap().name.as_str()).collect();
    assert_eq!(names, ["Main", "Sub"]);
    assert!(p.sequence(by_name(p, "Compound Clip 1")).is_some());
    // the Footage bin holds the clip; its file and shape come from the Media Pool records
    let a = by_name(p, "a.mp4");
    let ItemKind::Media(m) = &p.item(a).unwrap().kind else { panic!("a.mp4 is not media") };
    assert_eq!(m.media, MediaRef::File { path: "C:\\media\\a.mp4".into() });
    let v = m.info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.frame_rate), (1280, 720, FPS25));
    assert_eq!(m.info.audio_streams[0].sample_rate, 48000);
    assert_eq!(m.info.duration, FPS25.tick_of(240));
    let footage = bin_named(&p.root, "Footage").unwrap();
    assert!(holds(footage, a));
    assert!(holds(footage, by_name(p, "Sub")));
    assert!(holds(&p.root, by_name(p, "Main")), "the Master folder is the root bin");
    // the music has no Media Pool record: its timeline clip names the file
    let ItemKind::Media(music) = &p.item(by_name(p, "music.wav")).unwrap().kind else { panic!() };
    assert_eq!(music.media, MediaRef::File { path: "D:/audio/music.wav".into() });
    assert_eq!(music.info.kind, MediaKind::AudioOnly);
}

/// Timeline settings, the start timecode and the picture tracks with their clips.
#[test]
fn imports_picture_tracks() {
    let (imp, _) = imported();
    let p = &imp.project;
    let main = p.sequence(by_name(p, "Main")).unwrap();
    assert_eq!(main.settings.frame_rate, FPS25);
    assert_eq!((main.settings.width, main.settings.height), (1920, 1080));
    assert_eq!(main.start_timecode, 90000, "01:00:00:00 at 25 fps");
    // V1: two cuts from 0, the dissolve centred on the cut
    let v1 = &main.video_tracks[0];
    assert_eq!(v1.items.len(), 2);
    assert_eq!((v1.items[0].start, v1.items[0].duration, v1.items[0].source_in), (FPS25.tick_of(0), FPS25.tick_of(50), FPS25.tick_of(10)));
    assert_eq!(v1.items[1].start, FPS25.tick_of(50));
    // the 1280x720 clip fills the 1920x1080 frame, as Resolve fits it by default
    assert!(v1.items.iter().all(|c| c.scale_to_frame));
    assert_eq!(v1.transitions.len(), 1);
    let tr = &v1.transitions[0];
    assert_eq!((tr.from, tr.to, tr.align), (Some(v1.items[0].id), Some(v1.items[1].id), TransitionAlign::CenterAtCut));
    assert_eq!((tr.start, tr.duration), (FPS25.tick_of(40), FPS25.tick_of(20)));
    assert_eq!(tr.effect.effect, "cross_dissolve");
    // V2: the nested Sub timeline (its In counts from Sub's origin); the title is reported
    let v2 = &main.video_tracks[1];
    assert_eq!(v2.items.len(), 1);
    assert_eq!(v2.items[0].item, by_name(p, "Sub"));
    assert_eq!((v2.items[0].start, v2.items[0].source_in), (FPS25.tick_of(100), FPS25.tick_of(5)));
}

/// Sound tracks, links and crossfades, nested timelines and compound clips, and the report of what
/// is skipped.
#[test]
fn imports_sound_nests_and_reports() {
    let (imp, report) = imported();
    let p = &imp.project;
    let main = p.sequence(by_name(p, "Main")).unwrap();
    let v1 = &main.video_tracks[0];
    // A1 keeps its name; its clips are linked to the pictures; the crossfade is constant power
    let a1 = &main.audio_tracks[0];
    assert_eq!(a1.name, "Dialog");
    assert_eq!(main.audio_tracks[1].name, "Music");
    assert_eq!(a1.items.len(), 2);
    assert!(a1.items[0].link.is_some());
    assert_eq!(a1.items[0].link, v1.items[0].link);
    assert_eq!(a1.items[1].link, v1.items[1].link);
    assert_ne!(a1.items[0].link, a1.items[1].link);
    assert_eq!(a1.transitions[0].effect.effect, "constant_power");
    assert!(a1.items.iter().all(|c| !c.scale_to_frame), "sound has no frame to fit");
    assert_eq!(main.audio_tracks[1].items[0].duration, FPS25.tick_of(75));
    assert_eq!(main.audio_tracks.len(), 2, "the subtitle track is not an audio track");

    let sub = p.sequence(by_name(p, "Sub")).unwrap();
    assert_eq!(sub.start_timecode, 0);
    assert_eq!((sub.settings.width, sub.settings.height), (1920, 1080), "a timeline without a size gets the common one");
    assert_eq!(sub.video_tracks[0].items[0].duration, FPS25.tick_of(100));
    let cc = p.sequence(by_name(p, "Compound Clip 1")).unwrap();
    assert_eq!(cc.video_tracks[0].items.len(), 1);

    assert!(report.mentions("generator or title \"Text+\" at 01:00:00:00"), "{report}");
    assert!(report.mentions("subtitle track"), "{report}");
    assert!(report.mentions("Fusion Composition"), "{report}");
    assert_eq!(p.nest_cycle(), None);
}

/// A database that is not a Resolve project, and a file that is not a database, are refused.
#[test]
fn rejects_other_databases() {
    let mut w = Writer::new(4096);
    w.table("notes", "CREATE TABLE notes (a)", vec![vec![t("x")]]);
    let bytes = w.finish();
    assert!(!sniff(&bytes));
    assert_eq!(project_name(&bytes), None);
    assert!(matches!(import(&bytes, &ImportOptions::default()), Err(Error::Parse { .. })));
    assert!(matches!(import(b"plain text", &ImportOptions::default()), Err(Error::Parse { .. })));
}

/// Frame counts as Resolve writes them (integers, text, fractions, damaged values), and timecode in
/// messages.
#[test]
fn frame_fields() {
    assert_eq!(frames(&t("86400")), Some(86400));
    assert_eq!(frames(&t("140909|0080f1155ff1b53f")), Some(140909), "a small fraction rounds down");
    assert_eq!(frames(&t(&format!("10|{}", hex(0.75)))), Some(11));
    assert_eq!(frames(&n(-3)), Some(-3));
    // i64::MIN has no absolute value: rejected, never an overflow
    assert_eq!(frames(&n(i64::MIN)), None);
    assert_eq!(frames(&t(&i64::MIN.to_string())), None);
    assert_eq!(frames(&t("x")), None);
    assert_eq!(frames(&t("99999999999999999")), None);
    assert_eq!(timecode(FrameRate::new(24, 1), 86400 + 24 * 61 + 5), "01:01:01:05");
}

/// `f` as the hex text Resolve writes after `|`.
fn hex(f: f64) -> String {
    f.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Qt variant maps, and clip records with their folder and file name.
#[test]
fn qt_maps_and_clip_records() {
    let m = qmap(&qmap_bytes(&[("A", 2, (-5i32).to_be_bytes().to_vec()), ("B", 12, vec![1, 2, 3]), ("C", 99, vec![])]));
    assert_eq!(m.get("A"), Some(&QVal::Int(-5)));
    assert_eq!(m.get("B"), Some(&QVal::Bytes(vec![1, 2, 3])));
    assert!(!m.contains_key("C"), "unknown types end the map");
    assert_eq!(clip_path(&clip_record("/Volumes/Media/", "x.mov")).as_deref(), Some("/Volumes/Media/x.mov"));
    assert_eq!(clip_path(&clip_record("/Volumes/Media", "x.mov")).as_deref(), Some("/Volumes/Media/x.mov"));
    assert_eq!(clip_path(&clip_record("D:", "x.mov")).as_deref(), Some("D:\\x.mov"));
    assert_eq!(clip_path(&[0; 4]), None);
    // damaged records never panic
    let good = clip_record("C:\\media", "a.mp4");
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            let _ = clip_path(&b);
        }
        let _ = clip_path(&good[..i]);
        let _ = qmap(&good[..i]);
    }
}

/// The sample project with timeline item `id`'s columns set to `values`.
fn with_item(id: &str, values: &[(&str, Value)]) -> Vec<u8> {
    let bytes = project();
    let db = Database::open(&bytes).unwrap();
    let mut w = Writer::new(1024);
    for e in db.schema().iter().filter(|e| e.kind == "table") {
        let (tbl, rows) = db.read_table(&e.name).unwrap();
        let rows = rows
            .into_iter()
            .map(|mut r| {
                if e.name == "Sm2TiItem" && r.values[0] == t(id) {
                    for (col, v) in values {
                        r.values[tbl.column(col).unwrap()] = v.clone();
                    }
                }
                r.values
            })
            .collect();
        w.table(&e.name, &e.sql, rows);
    }
    w.finish()
}

/// Sibling bins come in the Media Pool's order on every import (not a HashMap's, which changes
/// from run to run).
#[test]
fn bins_keep_the_media_pool_order() {
    let names: Vec<String> = (0..8).map(|n| format!("B{n}")).collect();
    let bytes = project();
    let db = Database::open(&bytes).unwrap();
    let mut w = Writer::new(1024);
    for e in db.schema().iter().filter(|e| e.kind == "table") {
        let (_, rows) = db.read_table(&e.name).unwrap();
        let mut rows: Vec<Vec<Value>> = rows.into_iter().map(|r| r.values).collect();
        if e.name == "Sm2MpFolder" {
            rows.extend(names.iter().map(|n| vec![t(&format!("f-{n}")), t("Sm2MpFolder"), t(n), t("f-root")]));
        }
        w.table(&e.name, &e.sql, rows);
    }
    let (imp, _) = import(&w.finish(), &ImportOptions::default()).unwrap();
    let bins: Vec<&str> = imp
        .project
        .root
        .children
        .iter()
        .filter_map(|c| if let BinEntry::Bin(b) = c { Some(b.name.as_str()) } else { None })
        .filter(|n| n.starts_with('B'))
        .collect();
    assert_eq!(bins, names);
}

#[test]
fn a_timeline_nesting_itself_is_cut_loose() {
    // Sub nests Main, which nests Sub
    let bytes = with_item("i11", &[("MediaRef", t("m-main")), ("DbType", t("Sm2TiVideoClip")), ("MediaFilePath", NULL)]);
    let (imp, report) = import(&bytes, &ImportOptions::default()).unwrap();
    assert_eq!(imp.project.nest_cycle(), None);
    assert!(report.mentions("nests itself"), "{report}");
}

#[test]
fn frame_counts_beyond_the_tick_range_are_skipped() {
    // within MAX_FRAMES, but far past what Tick can hold at 25 fps: reported and skipped, not
    // clamped into a clip at a meaningless time
    let huge = (super::MAX_FRAMES - 1).to_string();
    for (col, why) in
        [("Start", "outside the time a sequence can hold"), ("Duration", "outside the time a sequence can hold"), ("In", "outside the time its source")]
    {
        let (imp, report) = import(&with_item("i2", &[(col, t(&huge))]), &ImportOptions::default()).unwrap();
        let p = &imp.project;
        let main = p.sequence(by_name(p, "Main")).unwrap();
        assert_eq!(main.video_tracks[0].items.len(), 1, "{col}: the damaged clip is left out");
        assert!(main.all_tracks().flat_map(|t| &t.items).all(|c| c.end() <= Tick::MAX && c.source_in <= Tick::MAX), "{col}");
        assert!(report.mentions(why), "{col}: {report}");
    }
}

#[test]
fn an_unreadable_in_point_skips_the_clip() {
    // a present In that is not a frame count: reported and skipped, never played from frame 0
    let beyond = (super::MAX_FRAMES + 1).to_string();
    for (what, v) in [("beyond MAX_FRAMES", t(&beyond)), ("unparsable", t("soon")), ("not finite", Value::Real(f64::NAN))] {
        let (imp, report) = import(&with_item("i2", &[("In", v)]), &ImportOptions::default()).unwrap();
        let p = &imp.project;
        let main = p.sequence(by_name(p, "Main")).unwrap();
        assert_eq!(main.video_tracks[0].items.len(), 1, "{what}: the damaged clip is left out");
        assert!(report.mentions("clip \"a.mp4\" has an unreadable in point"), "{what}: {report}");
    }
    // an absent In is the start of the source
    for (what, v) in [("null", NULL), ("empty", t(" "))] {
        let (imp, report) = import(&with_item("i2", &[("In", v)]), &ImportOptions::default()).unwrap();
        let p = &imp.project;
        let main = p.sequence(by_name(p, "Main")).unwrap();
        assert_eq!(main.video_tracks[0].items.len(), 2, "{what}: {report}");
        assert!(main.video_tracks[0].items.iter().any(|c| c.source_in == Tick::ZERO), "{what}");
        assert!(!report.mentions("in point"), "{what}: {report}");
    }
}

#[test]
fn damaged_projects_never_panic() {
    let bytes = project();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for cut in (0..bytes.len()).step_by(211) {
        let b = &bytes[..cut];
        assert!(std::panic::catch_unwind(|| import(b, &ImportOptions::default())).is_ok(), "truncated at {cut}");
    }
    for round in 0..300 {
        let mut b = bytes.clone();
        for _ in 0..1 + round % 6 {
            let i = (rnd() as usize) % b.len();
            b[i] ^= 1 << (rnd() % 8);
        }
        assert!(std::panic::catch_unwind(|| import(&b, &ImportOptions::default())).is_ok(), "bit flips, round {round}");
    }
}

/// The picture tracks of an imported timeline are video tracks.
#[test]
fn track_kinds() {
    let bytes = project();
    let (imp, _) = import(&bytes, &ImportOptions::default()).unwrap();
    let main = imp.project.sequence(by_name(&imp.project, "Main")).unwrap();
    assert!(main.video_tracks.iter().all(|t| t.kind == TrackKind::Video));
}
