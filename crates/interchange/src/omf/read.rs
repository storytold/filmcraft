//! OMF 2.0 objects (Bento) → composition model.

use std::collections::{HashMap, HashSet};

use filmcraft_time::{FrameRate, Tick};

use super::bento::Container;
use crate::comp::{CClip, CItem, CKind, CTrack, CTransition, Composition, Document, Gain, Source, units_to_ticks};
use crate::{Error, Report, Result};

type Uid = [u8; 12];

const MAX_REFERENCE_VISITS: usize = 65_536;

struct R<'a, 'b> {
    c: &'b Container<'a>,
    report: &'b mut Report,
    /// big-endian values ("MM"); "II" files are little-endian
    be: bool,
    mobs: HashMap<Uid, u32>,
    media: HashMap<Uid, Vec<u8>>,
    doc: Document,
    sources: HashMap<(Uid, u32, String), usize>,
}

pub(crate) fn read(bytes: &[u8], report: &mut Report) -> Result<Document> {
    let perr = |m: String| Error::Parse { format: "OMF", message: m };
    let c = Container::open(bytes).map_err(perr)?;
    let class_of = |o: u32| c.get(o, "OMFI:OOBJ:ObjClass").and_then(|v| v.get(..4).map(|b| [b[0], b[1], b[2], b[3]]));
    let head = c.objects.keys().copied().find(|&o| class_of(o) == Some(*b"HEAD")).ok_or_else(|| perr("no OMF header object".into()))?;
    let be = c.get(head, "OMFI:HEAD:ByteOrder").is_none_or(|v| v.first() != Some(&0x49));
    let mut r = R { c: &c, report, be, mobs: HashMap::new(), media: HashMap::new(), doc: Document::default(), sources: HashMap::new() };
    let mob_list = r.refs(head, "OMFI:HEAD:Mobs");
    let mob_list = if mob_list.is_empty() {
        c.objects.keys().copied().filter(|&o| matches!(class_of(o).as_ref(), Some(b"CMOB" | b"MMOB" | b"SMOB"))).collect()
    } else {
        mob_list
    };
    for &m in &mob_list {
        if let Some(id) = r.uid(m, "OMFI:MOBJ:MobID") {
            r.mobs.insert(id, m);
        }
    }
    let md = r.refs(head, "OMFI:HEAD:MediaData");
    let md = if md.is_empty() { c.objects.keys().copied().filter(|&o| matches!(class_of(o).as_ref(), Some(b"WAVE" | b"AIFC"))).collect() } else { md };
    for m in md {
        let Some(id) = r.uid(m, "OMFI:MDAT:MobID") else { continue };
        if let Some(d) = c.get(m, "OMFI:WAVE:Data") {
            r.media.insert(id, d);
        } else if let Some(d) = c.get(m, "OMFI:AIFC:Data") {
            r.media.insert(id, d);
        }
    }
    let cmobs: Vec<u32> = mob_list.iter().copied().filter(|&m| class_of(m) == Some(*b"CMOB")).collect();
    let mut referenced = HashSet::new();
    let mut visited = HashSet::new();
    let mut visits = 0;
    for &m in &cmobs {
        r.collect_refs(m, &mut referenced, &mut visited, &mut visits, 0);
    }
    let primary: Vec<u32> = r.refs(head, "OMFI:HEAD:PrimaryMobs").into_iter().filter(|m| cmobs.contains(m)).collect();
    let top: Vec<u32> = if !primary.is_empty() {
        primary
    } else {
        cmobs.iter().copied().filter(|m| r.uid(*m, "OMFI:MOBJ:MobID").is_none_or(|id| !referenced.contains(&id))).collect()
    };
    for m in top {
        let comp = r.composition(m);
        r.doc.compositions.push(comp);
    }
    if r.doc.compositions.is_empty() {
        return Err(Error::Empty);
    }
    Ok(r.doc)
}

impl R<'_, '_> {
    fn class(&self, o: u32) -> [u8; 4] {
        self.c.get(o, "OMFI:OOBJ:ObjClass").and_then(|v| v.get(..4).map(|b| [b[0], b[1], b[2], b[3]])).unwrap_or(*b"????")
    }
    fn u32_of(&self, b: &[u8]) -> Option<u32> {
        let a: [u8; 4] = b.get(..4)?.try_into().ok()?;
        Some(if self.be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) })
    }
    fn i32(&self, o: u32, p: &str) -> Option<i64> {
        self.c.get(o, p).and_then(|v| self.u32_of(&v)).map(|x| x as i32 as i64)
    }
    fn u32(&self, o: u32, p: &str) -> Option<u32> {
        self.c.get(o, p).and_then(|v| self.u32_of(&v))
    }
    fn u16(&self, o: u32, p: &str) -> Option<u16> {
        let v = self.c.get(o, p)?;
        let a: [u8; 2] = v.get(..2)?.try_into().ok()?;
        Some(if self.be { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) })
    }
    fn rational(&self, o: u32, p: &str) -> Option<(i64, i64)> {
        let v = self.c.get(o, p)?;
        self.rational_of(&v)
    }
    fn rational_of(&self, v: &[u8]) -> Option<(i64, i64)> {
        Some((self.u32_of(v)? as i32 as i64, self.u32_of(v.get(4..)?)? as i32 as i64))
    }
    fn string(&self, o: u32, p: &str) -> Option<String> {
        self.c.get(o, p).map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string())
    }
    fn uid(&self, o: u32, p: &str) -> Option<Uid> {
        self.c.get(o, p).and_then(|v| v.get(..12).and_then(|b| b.try_into().ok()))
    }
    fn r#ref(&self, o: u32, p: &str) -> Option<u32> {
        let v = self.c.get(o, p)?;
        let key = u32::from_be_bytes(v.get(..4)?.try_into().ok()?);
        Some(self.c.resolve(o, key))
    }
    fn refs(&self, o: u32, p: &str) -> Vec<u32> {
        let Some(v) = self.c.get(o, p) else { return Vec::new() };
        if v.len() < 2 {
            return Vec::new();
        }
        let n = u16::from_be_bytes([v[0], v[1]]) as usize;
        v[2..].as_chunks::<4>().0.iter().take(n).map(|k| self.c.resolve(o, u32::from_be_bytes([k[0], k[1], k[2], k[3]]))).collect()
    }
    fn data_kind(&self, o: u32) -> String {
        self.r#ref(o, "OMFI:CPNT:DataKind").and_then(|d| self.string(d, "OMFI:DDEF:DataKindID")).unwrap_or_default()
    }
    fn len(&self, o: u32) -> i64 {
        self.i32(o, "OMFI:CPNT:Length").unwrap_or(0).max(0)
    }

    fn collect_refs(&self, o: u32, out: &mut HashSet<Uid>, visited: &mut HashSet<u32>, visits: &mut usize, depth: usize) {
        if depth > 64 || !visited.insert(o) || *visits >= MAX_REFERENCE_VISITS {
            return;
        }
        *visits += 1;
        if self.class(o) == *b"SCLP"
            && let Some(id) = self.uid(o, "OMFI:SCLP:SourceID")
        {
            out.insert(id);
        }
        for p in ["OMFI:MOBJ:Slots", "OMFI:SEQU:Components", "OMFI:EFFE:EffectSlots"] {
            for c in self.refs(o, p) {
                self.collect_refs(c, out, visited, visits, depth + 1);
            }
        }
        for p in ["OMFI:MSLT:Segment", "OMFI:ESLT:ArgValue"] {
            if let Some(c) = self.r#ref(o, p) {
                self.collect_refs(c, out, visited, visits, depth + 1);
            }
        }
    }

    fn composition(&mut self, m: u32) -> Composition {
        let name = self.string(m, "OMFI:MOBJ:Name").unwrap_or_else(|| "Sequence".into());
        let mut comp = Composition {
            name,
            rate: FrameRate::FPS_25,
            sample_rate: 48_000,
            width: 1920,
            height: 1080,
            start_tc: 0,
            drop: false,
            tracks: Vec::new(),
            markers: Vec::new(),
        };
        let slots = self.refs(m, "OMFI:MOBJ:Slots");
        let mut na = 0;
        for s in slots {
            let Some(seg) = self.r#ref(s, "OMFI:MSLT:Segment") else { continue };
            let rate = self.rational(s, "OMFI:MSLT:EditRate").filter(|r| r.0 > 0 && r.1 > 0).unwrap_or((25, 1));
            let kind = self.data_kind(seg);
            if self.class(seg) == *b"TCCP" || kind == "omfi:data:Timecode" {
                comp.rate = FrameRate::new(rate.0, rate.1);
                if self.class(seg) == *b"TCCP" {
                    comp.start_tc = self.i32(seg, "OMFI:TCCP:Start").unwrap_or(0);
                    comp.drop = self.c.get(seg, "OMFI:TCCP:Drop").and_then(|v| v.first().copied()).unwrap_or(0) != 0;
                }
                continue;
            }
            let ckind = match kind.as_str() {
                "omfi:data:Sound" => CKind::Sound,
                "omfi:data:Picture" | "omfi:data:PictureWithMatte" => CKind::Picture,
                _ => {
                    self.report.info("an OMF slot of an unsupported data kind was skipped");
                    continue;
                }
            };
            if ckind == CKind::Picture {
                self.report.info("OMF picture slots are not imported (audio only)");
                continue;
            }
            if rate.1 == 1 && rate.0 >= 8000 {
                comp.sample_rate = rate.0 as u32;
            }
            let items = self.segment(seg, rate, 0);
            na += 1;
            let td = self.r#ref(s, "OMFI:MSLT:TrackDesc");
            let tname = td.and_then(|t| self.string(t, "OMFI:TRKD:TrackName")).unwrap_or_else(|| format!("A{na}"));
            let number = td.and_then(|t| self.u32(t, "OMFI:TRKD:PhysicalTrack")).unwrap_or(na);
            let channels = items
                .iter()
                .filter_map(|i| match i {
                    CItem::Clip(c) => Some(self.doc.sources[c.source].channels),
                    _ => None,
                })
                .max()
                .unwrap_or(1)
                .min(6);
            comp.tracks.push(CTrack { kind: CKind::Sound, name: tname, number, channels, items });
        }
        comp
    }

    fn segment(&mut self, seg: u32, rate: (i64, i64), depth: usize) -> Vec<CItem> {
        let len = units_to_ticks(self.len(seg), rate.0, rate.1);
        if depth > 32 {
            return vec![CItem::Filler(len)];
        }
        match &self.class(seg) {
            b"SEQU" => {
                let mut out = Vec::new();
                for c in self.refs(seg, "OMFI:SEQU:Components") {
                    out.extend(self.segment(c, rate, depth + 1));
                }
                out
            }
            b"FILL" => vec![CItem::Filler(len)],
            b"SCLP" => vec![self.clip(seg, rate)],
            b"TRAN" => {
                let cut = units_to_ticks(self.i32(seg, "OMFI:TRAN:CutPoint").unwrap_or(0), rate.0, rate.1);
                let effect =
                    self.r#ref(seg, "OMFI:TRAN:Effect").and_then(|e| self.string(e, "FilmCraft:EFFE:EffectID")).unwrap_or_else(|| "constant_power".into());
                vec![CItem::Transition(CTransition { len, cut: cut.clamp(Tick::ZERO, len), effect })]
            }
            b"EFFE" => {
                let kind = self.r#ref(seg, "OMFI:EFFE:EffectKind").and_then(|k| self.string(k, "OMFI:EDEF:EffectID")).unwrap_or_default();
                let slots = self.refs(seg, "OMFI:EFFE:EffectSlots");
                let arg =
                    |r: &Self, id: i64| slots.iter().copied().find(|&s| r.i32(s, "OMFI:ESLT:ArgID") == Some(id)).and_then(|s| r.r#ref(s, "OMFI:ESLT:ArgValue"));
                let Some(input) = arg(self, -1).or_else(|| arg(self, 1).filter(|_| kind != "omfi:effect:MonoAudioGain")) else {
                    return vec![CItem::Filler(len)];
                };
                let gain = if kind == "omfi:effect:MonoAudioGain" {
                    arg(self, 1).and_then(|v| self.gain(v, len))
                } else {
                    self.report.warn("OMF effects other than audio gain are not imported (their input is used)");
                    None
                };
                let mut items = self.segment(input, rate, depth + 1);
                if let (Some(g), [CItem::Clip(c)]) = (gain, items.as_mut_slice()) {
                    c.gain = Some(g);
                }
                items
            }
            _ => {
                self.report.info("an unsupported OMF component was imported as a gap");
                vec![CItem::Filler(len)]
            }
        }
    }

    fn amp(&self, v: &[u8]) -> Option<f64> {
        self.rational_of(v).filter(|r| r.1 != 0).map(|(n, d)| n as f64 / d as f64)
    }

    fn gain(&mut self, v: u32, len: Tick) -> Option<Gain> {
        match &self.class(v) {
            b"CVAL" => self.c.get(v, "OMFI:CVAL:Value").and_then(|d| self.amp(&d)).map(Gain::Constant),
            b"VVAL" => {
                let linear = self.u16(v, "OMFI:VVAL:Interpolation") != Some(1);
                let points: Vec<(Tick, f64)> = self
                    .refs(v, "OMFI:VVAL:PointList")
                    .into_iter()
                    .filter_map(|p| {
                        let (n, d) = self.rational(p, "OMFI:CTLP:Time").filter(|r| r.1 != 0)?;
                        let a = self.c.get(p, "OMFI:CTLP:Value").and_then(|d| self.amp(&d))?;
                        Some((Tick((len.0 as i128 * n as i128 / d as i128).clamp(-(1i128 << 56), 1i128 << 56) as i64), a))
                    })
                    .collect();
                (!points.is_empty()).then_some(Gain::Varying { linear, points })
            }
            _ => None,
        }
    }

    fn clip(&mut self, c: u32, rate: (i64, i64)) -> CItem {
        let len = units_to_ticks(self.len(c), rate.0, rate.1);
        let start = units_to_ticks(self.i32(c, "OMFI:SCLP:StartTime").unwrap_or(0), rate.0, rate.1);
        let name = self.string(c, "FilmCraft:SCLP:ClipName").unwrap_or_default();
        let (Some(id), Some(track)) = (self.uid(c, "OMFI:SCLP:SourceID"), self.u32(c, "OMFI:SCLP:SourceTrackID")) else { return CItem::Filler(len) };
        if id == [0; 12] {
            return CItem::Filler(len);
        }
        match self.resolve(id, track, start, None, 0) {
            Some((source, t)) => CItem::Clip(CClip { len, source, start: t, gain: None, name }),
            None => CItem::Filler(len),
        }
    }

    fn slot_of(&self, mob: u32, track: u32) -> Option<u32> {
        self.refs(mob, "OMFI:MOBJ:Slots")
            .into_iter()
            .find(|&s| self.r#ref(s, "OMFI:MSLT:TrackDesc").and_then(|t| self.u32(t, "OMFI:TRKD:TrackID")) == Some(track))
    }

    fn first_clip(&self, seg: u32) -> Option<u32> {
        match &self.class(seg) {
            b"SCLP" => Some(seg),
            b"SEQU" => self.refs(seg, "OMFI:SEQU:Components").into_iter().find_map(|c| self.first_clip(c)),
            _ => None,
        }
    }

    fn resolve(&mut self, id: Uid, track: u32, start: Tick, master: Option<String>, depth: usize) -> Option<(usize, Tick)> {
        if depth > 16 {
            return None;
        }
        let Some(&mob) = self.mobs.get(&id) else {
            self.report.warn("a clip references a mob that is not in the file (imported as a gap)");
            return None;
        };
        let slot = self.slot_of(mob, track)?;
        let rate = self.rational(slot, "OMFI:MSLT:EditRate").filter(|r| r.0 > 0 && r.1 > 0).unwrap_or((48_000, 1));
        let seg = self.r#ref(slot, "OMFI:MSLT:Segment")?;
        match &self.class(mob) {
            b"MMOB" => {
                let key = master.unwrap_or_else(|| id.iter().map(|b| format!("{b:02x}")).collect());
                let clip = self.first_clip(seg)?;
                let inner = units_to_ticks(self.i32(clip, "OMFI:SCLP:StartTime").unwrap_or(0), rate.0, rate.1);
                let next = self.uid(clip, "OMFI:SCLP:SourceID")?;
                let next_track = self.u32(clip, "OMFI:SCLP:SourceTrackID")?;
                let name = self.string(mob, "OMFI:MOBJ:Name");
                let r = self.resolve(next, next_track, start + inner, Some(key), depth + 1)?;
                if let Some(n) = name {
                    self.doc.sources[r.0].name = n;
                }
                Some(r)
            }
            b"SMOB" => {
                let desc = self.r#ref(mob, "OMFI:SMOB:MediaDescription")?;
                let sr = self.rational(desc, "OMFI:MDFL:SampleRate");
                if sr.is_none() && !self.media.contains_key(&id) {
                    return None; // a tape / physical source
                }
                let key = master.unwrap_or_else(|| id.iter().map(|b| format!("{b:02x}")).collect());
                let skey = (id, track, key.clone());
                if let Some(&i) = self.sources.get(&skey) {
                    return Some((i, start));
                }
                let file_rate = sr.filter(|r| r.0 > 0 && r.1 > 0).unwrap_or(rate);
                let length = units_to_ticks(self.i32(desc, "OMFI:MDFL:Length").unwrap_or(0), file_rate.0, file_rate.1);
                let summary = self.c.get(desc, "OMFI:WAVD:Summary").or_else(|| self.c.get(desc, "OMFI:AIFD:Summary")).unwrap_or_default();
                let fmt = crate::wav::parse_wav(&summary).or_else(|| crate::wav::parse_aiff(&summary)).map(|(_, ch, sr, bits)| (ch as u32, sr, bits));
                let mut path = None;
                for l in self.refs(desc, "OMFI:MDES:Locator") {
                    let p = match &self.class(l) {
                        b"UNXL" => self.string(l, "OMFI:UNXL:PathName"),
                        b"DOSL" => self.string(l, "OMFI:DOSL:PathName"),
                        b"MACL" => self.string(l, "OMFI:MACL:PathName"),
                        b"TXTL" => self.string(l, "OMFI:TXTL:Name"),
                        b"NETL" => self.string(l, "OMFI:NETL:URLString").map(|u| crate::common::file_url_to_path(&u)),
                        _ => None,
                    };
                    if p.is_some() {
                        path = p;
                        break;
                    }
                }
                let sound_slots = self.refs(mob, "OMFI:MOBJ:Slots").len();
                let (file_channels, sample_rate, bits) = fmt.unwrap_or((1, file_rate.0 as u32, 16));
                let channel = (file_channels > 1 && sound_slots > 1).then(|| track.saturating_sub(1).min(file_channels - 1));
                // timecode of the physical source
                let tc = self.first_clip(seg).and_then(|clip| {
                    let tid = self.uid(clip, "OMFI:SCLP:SourceID")?;
                    let tape = *self.mobs.get(&tid)?;
                    let (start_tc, fps, trate) = self.refs(tape, "OMFI:MOBJ:Slots").into_iter().find_map(|s| {
                        let g = self.r#ref(s, "OMFI:MSLT:Segment")?;
                        (self.class(g) == *b"TCCP").then(|| {
                            (self.i32(g, "OMFI:TCCP:Start").unwrap_or(0), self.u16(g, "OMFI:TCCP:FPS").unwrap_or(25), self.rational(s, "OMFI:MSLT:EditRate"))
                        })
                    })?;
                    let tcr = trate.filter(|r| r.0 > 0 && r.1 > 0).map(|r| FrameRate::new(r.0, r.1)).unwrap_or_else(|| FrameRate::new(fps.max(1) as i64, 1));
                    let off = units_to_ticks(self.i32(clip, "OMFI:SCLP:StartTime").unwrap_or(0), rate.0, rate.1);
                    Some((start_tc + tcr.frame_at(off), tcr))
                });
                let mut s = Source {
                    key,
                    name: self.string(mob, "OMFI:MOBJ:Name").unwrap_or_default(),
                    kind: CKind::Sound,
                    path,
                    channel,
                    channels: if channel.is_some() { 1 } else { file_channels },
                    file_channels,
                    width: 0,
                    height: 0,
                    frame_rate: FrameRate::FPS_25,
                    sample_rate,
                    bits,
                    length,
                    offset: Tick::ZERO,
                    start_tc: tc.map(|t| t.0),
                    tc_rate: tc.map(|t| t.1).unwrap_or(FrameRate::FPS_25),
                    embedded: None,
                    markers: Vec::new(),
                    nested: None,
                };
                if let Some(d) = self.media.get(&id) {
                    if let Some((pcm, ch, sr, b)) = crate::wav::parse_wav(d).or_else(|| crate::wav::parse_aiff(d)) {
                        s.embedded = Some(pcm);
                        s.file_channels = ch as u32;
                        s.channels = if s.channel.is_some() { 1 } else { ch as u32 };
                        s.sample_rate = sr;
                        s.bits = b;
                    } else {
                        self.report.warn("embedded OMF audio in an unsupported format was skipped");
                    }
                }
                let i = self.doc.sources.len();
                self.doc.sources.push(s);
                self.sources.insert(skey, i);
                Some((i, start))
            }
            _ => {
                self.report.warn("nested OMF compositions are not supported (imported as gaps)");
                None
            }
        }
    }
}
