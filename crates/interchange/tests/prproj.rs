use filmcraft_interchange::{Error, ExportOptions, Format, ImportOptions, Imported, Report, detect, export, import_with};
use filmcraft_project::{BinEntry, Interpolation, ItemId, ItemKind, MediaRef, ParamValue, Sequence, TrackKind, TransitionAlign};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use proptest::prelude::*;

const SEC: i64 = TICKS_PER_SECOND;
const NTSC: i64 = 8_475_667_200;
const HZ48: i64 = TICKS_PER_SECOND / 48_000;

fn gzip(data: &[u8]) -> Vec<u8> {
    gzip_with(data, filmcraft_bitstream::crc32(data), data.len() as u32)
}

fn gzip_with(data: &[u8], crc: u32, size: u32) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    if data.is_empty() {
        out.extend([1, 0, 0, 0xff, 0xff]);
    }
    let mut blocks = data.chunks(0xffff).peekable();
    while let Some(b) = blocks.next() {
        let len = b.len() as u16;
        out.push(u8::from(blocks.peek().is_none()));
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend(b);
    }
    out.extend(crc.to_le_bytes());
    out.extend(size.to_le_bytes());
    out
}

#[derive(Default)]
struct Track {
    items: Vec<String>,
    transitions: Vec<String>,
    muted: bool,
    fader: Option<String>,
}

fn track(items: &[&String]) -> Track {
    Track { items: items.iter().map(|s| s.to_string()).collect(), ..Default::default() }
}

#[derive(Default)]
struct Doc {
    xml: String,
    next: u32,
}

impl Doc {
    fn id(&mut self) -> String {
        self.next += 1;
        self.next.to_string()
    }

    fn uid(&mut self) -> String {
        self.next += 1;
        format!("00000000-0000-4000-8000-{:012}", self.next)
    }

    fn add(&mut self, s: String) {
        self.xml.push_str(&s);
        self.xml.push('\n');
    }

    fn item(&mut self, name: &str, source: &str, video: bool) -> (String, String) {
        let (clip, master, item) = (self.id(), self.uid(), self.uid());
        let tag = if video { "VideoClip" } else { "AudioClip" };
        self.add(format!(r#"<{tag} ObjectID="{clip}"><Clip><Source ObjectRef="{source}"/></Clip></{tag}>"#));
        self.add(format!(r#"<MasterClip ObjectUID="{master}"><Clips><Clip ObjectRef="{clip}"/></Clips><Name>{name}</Name></MasterClip>"#));
        self.add(format!(
            r#"<ClipProjectItem ObjectUID="{item}"><ProjectItem><Name>{name}</Name></ProjectItem><MasterClip ObjectURef="{master}"/></ClipProjectItem>"#
        ));
        (item, master)
    }

    fn media(&mut self, name: &str, relative: &str, video: bool, secs: i64) -> (String, String) {
        let (media, vs, aus, source) = (self.uid(), self.id(), self.id(), self.id());
        let d = secs * SEC;
        let vref = if video { format!(r#"<VideoStream ObjectRef="{vs}"/>"#) } else { String::new() };
        self.add(format!(r#"<Media ObjectUID="{media}"><RelativePath>{relative}</RelativePath><FilePath>C:\Footage\{name}</FilePath>{vref}<AudioStream ObjectRef="{aus}"/></Media>"#));
        self.add(format!(
            r#"<VideoStream ObjectID="{vs}"><FrameRate>{NTSC}</FrameRate><FrameRect>0,0,1920,1080</FrameRect><Duration>{d}</Duration></VideoStream>"#
        ));
        self.add(format!(
            r#"<AudioStream ObjectID="{aus}"><FrameRate>{HZ48}</FrameRate><AudioChannelLayout>[{{"channellabel":100}},{{"channellabel":101}}]</AudioChannelLayout><Duration>{d}</Duration></AudioStream>"#
        ));
        let tag = if video { "VideoMediaSource" } else { "AudioMediaSource" };
        self.add(format!(r#"<{tag} ObjectID="{source}"><MediaSource><Media ObjectURef="{media}"/></MediaSource></{tag}>"#));
        self.item(name, &source, video)
    }

    fn param(&mut self, audio: bool, name: &str, start: &str, keys: &str, extra: &str) -> String {
        let id = self.id();
        let tag = if audio { "AudioComponentParam" } else { "VideoComponentParam" };
        let keys = if keys.is_empty() { String::new() } else { format!("<Keyframes>{keys}</Keyframes>") };
        self.add(format!(
            r#"<{tag} ObjectID="{id}"><Name>{name}</Name><ParameterControlType>2</ParameterControlType><StartKeyframe>-91445760000000000,{start},0,0,0,0,0,0</StartKeyframe>{keys}{extra}</{tag}>"#
        ));
        id
    }

    fn effect(&mut self, audio: bool, name: &str, native: &str, params: &[String]) -> String {
        let id = self.id();
        let refs: String = params.iter().map(|p| format!(r#"<Param ObjectRef="{p}"/>"#)).collect();
        let shown = if name.is_empty() { String::new() } else { format!("<DisplayName>{name}</DisplayName>") };
        let comp = format!("<Component><Params>{refs}</Params>{shown}</Component>");
        if audio {
            self.add(format!(r#"<AudioFilterComponent ObjectID="{id}"><AudioComponent>{comp}</AudioComponent><FilterMatchName>{native}</FilterMatchName></AudioFilterComponent>"#));
        } else {
            self.add(format!(r#"<VideoFilterComponent ObjectID="{id}">{comp}<MatchName>{native}</MatchName></VideoFilterComponent>"#));
        }
        id
    }

    fn on_clip(&mut self, audio: bool, effects: &[String]) -> String {
        let chain = self.chain(audio, effects);
        format!(r#"<ComponentOwner><Components ObjectRef="{chain}"/></ComponentOwner>"#)
    }

    fn fader(&mut self, params: &[String]) -> String {
        let fader = self.id();
        let refs: String = params.iter().map(|p| format!(r#"<Param ObjectRef="{p}"/>"#)).collect();
        self.add(format!(r#"<AudioFader ObjectID="{fader}"><AudioComponent><Component><Params>{refs}</Params></Component></AudioComponent></AudioFader>"#));
        self.chain(true, &[fader])
    }

    fn chain(&mut self, audio: bool, effects: &[String]) -> String {
        let id = self.id();
        let tag = if audio { "AudioComponentChain" } else { "VideoComponentChain" };
        let refs: String = effects.iter().map(|e| format!(r#"<Component ObjectRef="{e}"/>"#)).collect();
        self.add(format!(r#"<{tag} ObjectID="{id}"><ComponentChain><Components>{refs}</Components></ComponentChain></{tag}>"#));
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn clip(&mut self, video: bool, master: &str, name: &str, start: i64, end: i64, inp: i64, fields: &str, extra: &str) -> String {
        let (clip, sub, item) = (self.id(), self.id(), self.id());
        let (ct, it) = if video { ("VideoClip", "VideoClipTrackItem") } else { ("AudioClip", "AudioClipTrackItem") };
        let (fields, gain) = fields.split_once('|').unwrap_or((fields, ""));
        self.add(format!(r#"<{ct} ObjectID="{clip}"><Clip><InPoint>{}</InPoint>{fields}</Clip>{gain}</{ct}>"#, inp * SEC));
        self.add(format!(r#"<SubClip ObjectID="{sub}"><Clip ObjectRef="{clip}"/><MasterClip ObjectURef="{master}"/><Name>{name}</Name></SubClip>"#));
        self.add(format!(
            r#"<{it} ObjectID="{item}"><ClipTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="{sub}"/>{extra}</ClipTrackItem></{it}>"#,
            start * SEC,
            end * SEC
        ));
        item
    }

    fn transition(&mut self, name: &str, start: i64, end: i64) -> String {
        let id = self.id();
        self.add(format!(
            r#"<VideoTransitionTrackItem ObjectID="{id}"><TransitionTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><DisplayName>{name}</DisplayName></TransitionTrackItem></VideoTransitionTrackItem>"#,
            start * SEC,
            end * SEC
        ));
        id
    }

    fn sequence(&mut self, name: &str, video: Vec<Track>, audio: Vec<Track>, links: &[(&String, &String)]) -> (String, String) {
        let seq = self.uid();
        let mut groups = String::new();
        for (kind, tracks) in [("Video", video), ("Audio", audio)] {
            let mut refs = String::new();
            for (i, t) in tracks.into_iter().enumerate() {
                let uid = self.uid();
                let list = |ids: &[String]| ids.iter().map(|x| format!(r#"<TrackItem ObjectRef="{x}"/>"#)).collect::<String>();
                let fader =
                    t.fader.map(|c| format!(r#"<AudioTrack><ComponentOwner><Components ObjectRef="{c}"/></ComponentOwner></AudioTrack>"#)).unwrap_or_default();
                self.add(format!(
                    r#"<{kind}ClipTrack ObjectUID="{uid}"><ClipTrack><Track><Index>{i}</Index><IsMuted>{}</IsMuted></Track><ClipItems><TrackItems>{}</TrackItems></ClipItems><TransitionItems><TrackItems>{}</TrackItems></TransitionItems></ClipTrack>{fader}</{kind}ClipTrack>"#,
                    t.muted,
                    list(&t.items),
                    list(&t.transitions)
                ));
                refs.push_str(&format!(r#"<Track ObjectURef="{uid}"/>"#));
            }
            let g = self.id();
            let (rate, rect) = if kind == "Video" { (NTSC, "<FrameRect>0,0,1280,720</FrameRect>") } else { (HZ48, "") };
            self.add(format!(
                r#"<{kind}TrackGroup ObjectID="{g}"><TrackGroup><Tracks>{refs}</Tracks><FrameRate>{rate}</FrameRate></TrackGroup>{rect}</{kind}TrackGroup>"#
            ));
            groups.push_str(&format!(r#"<TrackGroup><Second ObjectRef="{g}"/></TrackGroup>"#));
        }
        let mut lrefs = String::new();
        for (a, b) in links {
            let l = self.id();
            self.add(format!(r#"<Link ObjectID="{l}"><TrackItemGroup><TrackItems><TrackItem ObjectRef="{a}"/><TrackItem ObjectRef="{b}"/></TrackItems></TrackItemGroup></Link>"#));
            lrefs.push_str(&format!(r#"<Link ObjectRef="{l}"/>"#));
        }
        self.add(format!(
            r#"<Sequence ObjectUID="{seq}"><TrackGroups>{groups}</TrackGroups><PersistentGroupContainer><LinkContainer><Links>{lrefs}</Links></LinkContainer></PersistentGroupContainer><Name>{name}</Name></Sequence>"#
        ));
        let source = self.id();
        self.add(format!(r#"<VideoSequenceSource ObjectID="{source}"><SequenceSource><Sequence ObjectURef="{seq}"/></SequenceSource></VideoSequenceSource>"#));
        self.item(name, &source, true)
    }

    fn bin(&mut self, name: &str, items: &[&String]) -> String {
        let uid = self.uid();
        let refs: String = items.iter().map(|i| format!(r#"<Item ObjectURef="{i}"/>"#)).collect();
        self.add(format!(r#"<BinProjectItem ObjectUID="{uid}"><ProjectItem><Name>{name}</Name></ProjectItem><ProjectItemContainer><Items>{refs}</Items></ProjectItemContainer></BinProjectItem>"#));
        uid
    }

    fn text(&self, top: &[&String]) -> String {
        let refs: String = top.iter().map(|i| format!(r#"<Item ObjectURef="{i}"/>"#)).collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<PremiereData Version=\"3\">\n<RootProjectItem ObjectUID=\"root\"><ProjectItem><Name>Root Bin</Name></ProjectItem><ProjectItemContainer><Items>{refs}</Items></ProjectItemContainer></RootProjectItem>\n{}</PremiereData>\n",
            self.xml
        )
    }

    fn bytes(&self, top: &[&String]) -> Vec<u8> {
        gzip(self.text(top).as_bytes())
    }
}

fn open(bytes: &[u8]) -> Result<(Imported, Report), Error> {
    let opts = ImportOptions { base_dir: Some("/work/edits".into()), name: Some("Demo".into()), ..Default::default() };
    import_with(bytes, Format::Prproj, &opts)
}

fn id(imp: &Imported, name: &str) -> ItemId {
    imp.project.items.values().find(|i| i.name == name).map(|i| i.id).expect(name)
}

fn seq<'a>(imp: &'a Imported, name: &str) -> &'a Sequence {
    imp.project.sequence(id(imp, name)).expect(name)
}

fn demo() -> Vec<u8> {
    gzip(demo_xml().as_bytes())
}

fn demo_xml() -> String {
    let mut d = Doc::default();
    let (movie_item, movie) = d.media("shot.mp4", r"..\media\shot.mp4", true, 20);
    let (music_item, music) = d.media("music.wav", r"..\media\music.wav", false, 60);
    let scale = d.param(false, "Scale", "50.", "", "");
    let opacity = d.param(false, "Opacity", "100.", &format!("{},100.,0,0,0,0.25,0,0.25;{},0.,5,0,0,0.5,0,0.5;", 2 * SEC, 4 * SEC), "");
    let fx = [d.effect(false, "Motion", "AE.ADBE Motion", &[scale]), d.effect(false, "Opacity", "AE.ADBE Opacity", &[opacity])];
    let chain = d.chain(false, &fx);
    let level = d.param(true, "Level", "0.5", "", "");
    let vol = d.effect(true, "Volume", "Internal Volume Stereo", &[level]);
    let achain = d.chain(true, &[vol]);
    let mute = d.param(true, "Mute", "true", "", "");
    let fader_vol = d.param(true, "Volume", "0.5", "", "");
    let fader_chain = d.fader(&[mute, fader_vol]);
    let dissolve = d.transition("Cross Dissolve", 3, 5);
    let v1 = d.clip(
        true,
        &movie,
        "shot.mp4",
        0,
        4,
        2,
        "",
        &format!(r#"<ComponentOwner><Components ObjectRef="{chain}"/></ComponentOwner><TailTransition ObjectRef="{dissolve}"/>"#),
    );
    let v2 = d.clip(
        true,
        &movie,
        "shot.mp4",
        4,
        8,
        10,
        "<PlaybackSpeed>2</PlaybackSpeed><PlayBackwards>true</PlayBackwards>",
        &format!(r#"<HeadTransition ObjectRef="{dissolve}"/>"#),
    );
    let a1 = d.clip(false, &movie, "shot.mp4", 0, 4, 2, "|<Gain>2</Gain>", &format!(r#"<ComponentOwner><Components ObjectRef="{achain}"/></ComponentOwner>"#));
    let a2 = d.clip(false, &music, "music.wav", 4, 10, 0, "", "");
    let inner_clip = d.clip(true, &movie, "shot.mp4", 0, 3, 0, "", "");
    let (inner_item, inner) = d.sequence("Inner", vec![track(&[&inner_clip])], vec![], &[]);
    let nest = d.clip(true, &inner, "Inner", 8, 11, 0, "", "");
    let v1t = Track { items: vec![v1.clone(), v2, nest], transitions: vec![dissolve], ..Default::default() };
    let a2t = Track { items: vec![a2], muted: true, fader: Some(fader_chain), ..Default::default() };
    let (main, _) = d.sequence("Main", vec![v1t], vec![track(&[&a1]), a2t], &[(&v1, &a1)]);
    let footage = d.bin("Footage", &[&movie_item, &music_item]);
    d.text(&[&footage, &main, &inner_item])
}

#[test]
fn imports_a_known_project() {
    let (imp, report) = open(&demo()).unwrap();
    assert!(report.is_empty(), "{report:?}");

    let footage = imp.project.root.children.iter().find_map(|c| match c {
        BinEntry::Bin(b) if b.name == "Footage" => Some(b),
        _ => None,
    });
    assert_eq!(footage.unwrap().children, vec![BinEntry::Item(id(&imp, "shot.mp4")), BinEntry::Item(id(&imp, "music.wav"))]);

    let movie = imp.project.item(id(&imp, "shot.mp4")).and_then(|i| i.as_media()).unwrap();
    assert_eq!(movie.media, MediaRef::File { path: r"C:\Footage\shot.mp4".into() });
    assert_eq!(imp.alternates, vec![(id(&imp, "shot.mp4"), "/work/media/shot.mp4".into()), (id(&imp, "music.wav"), "/work/media/music.wav".into())]);
    let v = movie.info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.frame_rate), (1920, 1080, FrameRate::FPS_29_97));
    let music = imp.project.item(id(&imp, "music.wav")).and_then(|i| i.as_media()).unwrap();
    assert_eq!(music.info.audio().map(|a| (a.sample_rate, a.channels)), Some((48_000, 2)));

    let main = seq(&imp, "Main");
    assert_eq!((main.settings.width, main.settings.height, main.settings.frame_rate, main.settings.sample_rate), (1280, 720, FrameRate::FPS_29_97, 48_000));
    let v1 = &main.video_tracks[0].items[0];
    let v2 = &main.video_tracks[0].items[1];
    let nest = &main.video_tracks[0].items[2];
    let a1 = &main.audio_tracks[0].items[0];
    assert_eq!((v1.start, v1.duration, v1.source_in, v1.speed), (Tick(0), Tick(4 * SEC), Tick(2 * SEC), 1.0));
    assert_eq!((v2.start, v2.source_in, v2.speed, v2.reverse), (Tick(4 * SEC), Tick(10 * SEC), 2.0, true));
    assert_eq!((nest.item, nest.start), (id(&imp, "Inner"), Tick(8 * SEC)));
    assert!(v1.link.is_some() && v1.link == a1.link);
    assert!((a1.gain_db - 6.0206).abs() < 1e-3, "{}", a1.gain_db);

    let scale = v1.effect("motion").and_then(|e| e.param("scale")).unwrap();
    assert_eq!(scale.value, ParamValue::Float(50.0));
    let opacity = v1.effect("opacity").and_then(|e| e.param("opacity")).unwrap();
    let keys: Vec<_> = opacity.keyframes.iter().map(|k| (k.time, k.value.clone(), k.interp)).collect();
    assert_eq!(keys, vec![(Tick(2 * SEC), ParamValue::Float(100.0), Interpolation::Linear), (Tick(4 * SEC), ParamValue::Float(0.0), Interpolation::Bezier)]);
    assert_eq!((opacity.keyframes[0].out_influence, opacity.keyframes[1].in_influence), (0.25, 0.5));
    let level = a1.effect("volume").and_then(|e| e.param("level")).unwrap();
    assert!(matches!(level.value, ParamValue::Float(db) if (db - (20.0 * 0.5f64.log10() + 15.0)).abs() < 1e-9));

    let tr = &main.video_tracks[0].transitions[0];
    assert_eq!(
        (tr.effect.effect.as_str(), tr.start, tr.duration, tr.from, tr.to, tr.align),
        ("cross_dissolve", Tick(3 * SEC), Tick(2 * SEC), Some(v1.id), Some(v2.id), TransitionAlign::CenterAtCut)
    );

    let a2t = &main.audio_tracks[1];
    assert!(a2t.muted);
    assert!((a2t.volume_db + 6.0206).abs() < 1e-3, "{}", a2t.volume_db);
    assert!(main.video_tracks.iter().all(|t| t.kind == TrackKind::Video) && main.audio_tracks.iter().all(|t| t.kind == TrackKind::Audio));
}

#[test]
fn huge_upper_bound_does_not_overflow() {
    let mut d = Doc::default();
    let (_, movie) = d.media("a.mp4", "a.mp4", true, 10);
    let dims = d.id();
    d.add(format!(r#"<VideoComponentParam ObjectID="{dims}"><Name>Blur Dimensions</Name><ParameterControlType>7</ParameterControlType><StartKeyframe>-91445760000000000,1,0,0,0,0,0,0</StartKeyframe><UpperBound>1e30</UpperBound></VideoComponentParam>"#));
    let blur = d.effect(false, "Gaussian Blur", "AE.ADBE Gaussian Blur 2", &[dims]);
    let chain = d.chain(false, &[blur]);
    let c = d.clip(true, &movie, "a", 0, 2, 0, "", &format!(r#"<ComponentOwner><Components ObjectRef="{chain}"/></ComponentOwner>"#));
    let (main, _) = d.sequence("Main", vec![track(&[&c])], vec![], &[]);
    let (imp, _) = open(&d.bytes(&[&main])).unwrap();
    let blur = seq(&imp, "Main").video_tracks[0].items[0].effect("gaussian_blur").cloned().unwrap();
    assert_eq!(blur.param("dimensions").map(|p| p.value.clone()), Some(ParamValue::Choice(0)));
}

#[test]
fn negative_db_is_reported_not_stored() {
    let mut d = Doc::default();
    let (_, movie) = d.media("a.mp4", "a.mp4", false, 10);
    let level = d.param(true, "Level", "-1", "", "");
    let vol = d.effect(true, "Volume", "Internal Volume Stereo", &[level]);
    let chain = d.chain(true, &[vol]);
    let c = d.clip(false, &movie, "a", 0, 2, 0, "", &format!(r#"<ComponentOwner><Components ObjectRef="{chain}"/></ComponentOwner>"#));
    let (main, _) = d.sequence("Main", vec![], vec![track(&[&c])], &[]);
    let (imp, report) = open(&d.bytes(&[&main])).unwrap();
    let level = seq(&imp, "Main").audio_tracks[0].items[0].effect("volume").and_then(|e| e.param("level")).map(|p| p.value.clone());
    assert_eq!(level, Some(ParamValue::Float(0.0)));
    assert!(report.mentions("\"Level\" of \"Volume\""), "{report:?}");
}

#[test]
fn bins_nested_too_deep_or_repeated() {
    let mut d = Doc::default();
    let mut inner = d.bin("b", &[]);
    for _ in 0..100 {
        inner = d.bin("b", &[&inner]);
    }
    let (a, _) = d.media("a.mp4", "a.mp4", true, 10);
    let twice = d.bin("Twice", &[&a]);
    let (imp, report) = open(&d.bytes(&[&inner, &twice, &twice])).unwrap_or_else(|e| panic!("{e}"));
    assert!(report.mentions("nested too deeply"), "{report:?}");
    assert!(report.mentions("listed more than once"), "{report:?}");
    assert_eq!(imp.project.root.children.iter().filter(|c| matches!(c, BinEntry::Bin(b) if b.name == "Twice")).count(), 1);
}

#[test]
fn broken_gzip_is_refused() {
    let text = demo_xml();
    let crc = filmcraft_bitstream::crc32(text.as_bytes());
    let len = text.len() as u32;
    let good = gzip(text.as_bytes());
    assert!(open(&good).is_ok());
    for (what, bytes) in [
        ("truncated", good[..good.len() / 2].to_vec()),
        ("wrong size", gzip_with(text.as_bytes(), crc, len + 1)),
        ("bad checksum", gzip_with(text.as_bytes(), crc ^ 1, len)),
        ("claims 128 MiB", gzip_with(b"<PremiereData/>", 0, 128 << 20)),
        ("claims more than 200x its size", gzip_with(text.as_bytes(), crc, (good.len() * 201) as u32)),
        ("not gzip", text.clone().into_bytes()),
    ] {
        assert!(matches!(open(&bytes), Err(Error::Parse { .. })), "{what}");
    }
}

#[test]
fn detect_errors_and_import_only() {
    let bytes = demo();
    assert_eq!(detect(&bytes, Some("prproj")), Some(Format::Prproj));
    assert_eq!(Format::from_extension(".PRPROJ"), Some(Format::Prproj));
    assert!(!Format::ALL.contains(&Format::Prproj));
    assert!(matches!(open(&gzip(b"<xmeml/>")), Err(Error::Parse { .. })));
    assert!(matches!(open(&gzip(b"\xff\xfe")), Err(Error::Parse { .. })));
    assert!(matches!(open(&Doc::default().bytes(&[])), Err(Error::Empty)));
    let (imp, _) = open(&bytes).unwrap();
    assert!(export(&imp.project, id(&imp, "Main"), Format::Prproj, &ExportOptions::default()).is_err());
}

#[test]
fn sequence_inside_itself_is_skipped() {
    let mut d = Doc::default();
    let uid = "00000000-0000-4000-8000-000000009999";
    let c = d.clip(true, uid, "Loop", 0, 1, 0, "", "");
    let (item, master) = d.sequence("Loop", vec![track(&[&c])], vec![], &[]);
    let text = d.text(&[&item]).replace(&master, uid);
    let (imp, report) = open(&gzip(text.as_bytes())).unwrap();
    assert!(seq(&imp, "Loop").video_tracks[0].items.is_empty());
    assert!(report.mentions("inside itself"), "{report:?}");
    assert!(matches!(imp.project.item(id(&imp, "Loop")).map(|i| &i.kind), Some(ItemKind::Sequence(_))));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn damaged_projects_never_panic(at in 0usize..1_000_000, byte in any::<u8>(), cut in any::<bool>()) {
        let mut xml = demo_xml().into_bytes();
        let at = at % xml.len();
        if cut { xml.truncate(at) } else { xml[at] = byte }
        let _ = open(&gzip(&xml));
    }
}

fn one_clip(d: &mut Doc, audio: bool, extra: &str) -> Vec<u8> {
    let (_, movie) = d.media("a.mp4", "a.mp4", !audio, 10);
    let c = d.clip(!audio, &movie, "a", 0, 2, 0, "", extra);
    let t = vec![track(&[&c])];
    let (main, _) = if audio { d.sequence("Main", vec![], t, &[]) } else { d.sequence("Main", t, vec![], &[]) };
    d.bytes(&[&main])
}

#[test]
fn plugins_with_adobe_names_are_not_mapped() {
    let mut d = Doc::default();
    let p = d.param(false, "Blurriness", "40.", "", "");
    let fx = d.effect(false, "Gaussian Blur", "AE.SomeVendor Blur", &[p]);
    let on = d.on_clip(false, &[fx]);
    let (imp, report) = open(&one_clip(&mut d, false, &on)).unwrap();
    assert!(seq(&imp, "Main").video_tracks[0].items[0].effect("gaussian_blur").is_none());
    assert!(report.mentions("effect \"Gaussian Blur\" is not supported"), "{report:?}");
}

#[test]
fn audio_settings_use_their_own_scale() {
    let mut d = Doc::default();
    let room = d.param(true, "Room Size", "0.5", "", "");
    let reverb = d.effect(true, "", "7ea99d31-ca8f-454f-acd7-81a258efb336", &[room]);
    let silent = d.param(true, "Level", "0.", "", "");
    let vol = d.effect(true, "Volume", "Internal Volume Stereo", &[silent]);
    let on = d.on_clip(true, &[reverb, vol]);
    let (imp, _) = open(&one_clip(&mut d, true, &on)).unwrap();
    let c = &seq(&imp, "Main").audio_tracks[0].items[0];
    assert_eq!(c.effect("studio_reverb").and_then(|e| e.param("room")).map(|p| p.value.clone()), Some(ParamValue::Float(50.0)));
    let min = filmcraft_project::find_effect("volume").and_then(|d| d.param("level")).map(|p| match p.kind {
        filmcraft_project::effect::ParamKind::Float { min, .. } => min,
        _ => f64::NAN,
    });
    assert_eq!(c.effect("volume").and_then(|e| e.param("level")).map(|p| p.value.clone()), min.map(ParamValue::Float), "0 is silence");
}

#[test]
fn equalizer_gain_is_not_read_as_volume() {
    let mut d = Doc::default();
    let gain = d.param(true, "Master Gain", "0.5", "", "");
    let eq = d.effect(true, "Graphic Equalizer (20 Bands)", "Internal Graphic EQ", &[gain]);
    let on = d.on_clip(true, &[eq]);
    let (imp, _) = open(&one_clip(&mut d, true, &on)).unwrap();
    let c = &seq(&imp, "Main").audio_tracks[0].items[0];
    let e = c.effect("graphic_eq_20").expect("the equalizer is imported");
    assert_eq!(e.param("gain").map(|p| p.value.clone()), Some(ParamValue::Float(0.0)), "left at its default, not +9 dB");
}

#[test]
fn huge_points_and_rates_are_refused() {
    let mut d = Doc::default();
    let pos = d.id();
    d.add(format!(
        r#"<VideoComponentParam ObjectID="{pos}"><Name>Position</Name><ParameterControlType>6</ParameterControlType><StartKeyframe>-91445760000000000,1e308:0.5,0,0,0,0,0,0</StartKeyframe></VideoComponentParam>"#
    ));
    let motion = d.effect(false, "Motion", "AE.ADBE Motion", &[pos]);
    let on = d.on_clip(false, &[motion]);
    let (_, movie) = d.media("a.mp4", "a.mp4", true, 10);
    let c = d.clip(true, &movie, "a", 0, 2, 0, "", &on);
    let (main, _) = d.sequence("Main", vec![track(&[&c])], vec![], &[]);
    let fast = format!("<FrameRate>{NTSC}</FrameRate></TrackGroup><FrameRect>0,0,1280,720");
    let text = d.text(&[&main]);
    assert!(text.contains(&fast));
    let (imp, report) = open(&gzip(text.replace(&fast, "<FrameRate>1</FrameRate></TrackGroup><FrameRect>0,0,1280,720").as_bytes())).unwrap();
    let main = seq(&imp, "Main");
    assert_eq!(main.settings.frame_rate, FrameRate::default());
    let pos = main.video_tracks[0].items[0].effect("motion").and_then(|e| e.param("position")).map(|p| p.value.clone());
    assert!(matches!(pos, Some(ParamValue::Vec2(v)) if !v.x.is_infinite()), "{pos:?}");
    assert!(report.mentions("\"Position\" of \"Motion\""), "{report:?}");
}

#[test]
fn gzip_claiming_more_than_the_limit_is_refused() {
    let pad = "x".repeat(700_000);
    let text = format!("<PremiereData>{pad}</PremiereData>");
    let crc = filmcraft_bitstream::crc32(text.as_bytes());
    assert!(matches!(open(&gzip_with(text.as_bytes(), crc, (128 << 20) + 1)), Err(Error::Parse { .. })));
}

#[test]
fn media_without_a_path_stay_separate() {
    let mut d = Doc::default();
    let mut items = Vec::new();
    for name in ["a.mp4", "b.mp4"] {
        let (media, source) = (d.uid(), d.id());
        d.add(format!(r#"<Media ObjectUID="{media}"><Title>{name}</Title></Media>"#));
        d.add(format!(r#"<VideoMediaSource ObjectID="{source}"><MediaSource><Media ObjectURef="{media}"/></MediaSource></VideoMediaSource>"#));
        items.push(d.item(name, &source, true).0);
    }
    let (imp, report) = open(&d.bytes(&items.iter().collect::<Vec<_>>())).unwrap();
    let (a, b) = (id(&imp, "a.mp4"), id(&imp, "b.mp4"));
    assert_ne!(a, b);
    assert!([a, b].iter().all(|i| imp.project.item(*i).and_then(|i| i.as_media()).is_some_and(|m| m.offline)));
    assert!(report.mentions("no file path"), "{report:?}");
}
