//! Composition model → OMF 2.0 objects in a Bento container.

use std::collections::HashMap;

use filmcraft_time::Tick;

use super::bento;
use crate::comp::{CItem, CKind, Document, Gain, Source, ticks_to_units};
use crate::{Error, Report, Result};

/// An OMF mob id (`omfi:UID`): prefix, major, minor.
pub(crate) type Uid = [u8; 12];

pub(crate) fn uid(prefix: u32, major: u32, minor: u32) -> Uid {
    let mut u = [0u8; 12];
    u[..4].copy_from_slice(&prefix.to_be_bytes());
    u[4..8].copy_from_slice(&major.to_be_bytes());
    u[8..].copy_from_slice(&minor.to_be_bytes());
    u
}

struct W<'a> {
    b: bento::Writer,
    refs: HashMap<u32, Vec<u32>>,
    ddefs: HashMap<&'static str, u32>,
    edefs: HashMap<&'static str, u32>,
    defs: Vec<u32>,
    report: &'a mut Report,
    inexact: bool,
}

fn rational(n: i64, d: i64) -> Vec<u8> {
    let mut v = (n.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_be_bytes().to_vec();
    v.extend_from_slice(&(d.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_be_bytes());
    v
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

impl W<'_> {
    fn obj(&mut self, class: &[u8; 4]) -> u32 {
        let id = self.b.new_object();
        self.b.set(id, "OMFI:OOBJ:ObjClass", "omfi:ClassID", class);
        id
    }
    fn str(&mut self, o: u32, p: &str, v: &str) {
        self.b.set(o, p, "omfi:String", &cstr(v));
    }
    fn i32(&mut self, o: u32, p: &str, ty: &str, v: i64) {
        if v > i32::MAX as i64 || v < i32::MIN as i64 {
            self.report.warn("a time does not fit OMF's 32-bit positions and was clamped");
        }
        self.b.set(o, p, ty, &(v.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_be_bytes());
    }
    fn r#ref(&mut self, o: u32, p: &str, target: u32) {
        self.b.set(o, p, "omfi:ObjRef", &target.to_be_bytes());
        self.refs.entry(o).or_default().push(target);
    }
    fn refs(&mut self, o: u32, p: &str, targets: &[u32]) {
        let mut v = (targets.len() as u16).to_be_bytes().to_vec();
        for t in targets {
            v.extend_from_slice(&t.to_be_bytes());
        }
        self.b.set(o, p, "omfi:ObjRefArray", &v);
        self.refs.entry(o).or_default().extend_from_slice(targets);
    }
    fn u(&mut self, t: Tick, rate: (i64, i64)) -> i64 {
        let (n, exact) = ticks_to_units(t, rate.0, rate.1);
        if !exact {
            self.inexact = true;
        }
        n
    }
    fn ddef(&mut self, kind: &'static str) -> u32 {
        if let Some(&d) = self.ddefs.get(kind) {
            return d;
        }
        let d = self.obj(b"DDEF");
        self.b.set(d, "OMFI:DDEF:DataKindID", "omfi:UniqueName", &cstr(kind));
        self.ddefs.insert(kind, d);
        self.defs.push(d);
        d
    }
    fn edef(&mut self, id: &'static str, name: &str) -> u32 {
        if let Some(&d) = self.edefs.get(id) {
            return d;
        }
        let d = self.obj(b"EDEF");
        self.b.set(d, "OMFI:EDEF:EffectID", "omfi:UniqueName", &cstr(id));
        self.str(d, "OMFI:EDEF:EffectName", name);
        self.b.set(d, "OMFI:EDEF:Bypass", "omfi:ArgIDType", &(-1i32).to_be_bytes());
        self.edefs.insert(id, d);
        self.defs.push(d);
        d
    }
    fn cpnt(&mut self, o: u32, kind: &'static str, len: i64) {
        let d = self.ddef(kind);
        self.r#ref(o, "OMFI:CPNT:DataKind", d);
        self.i32(o, "OMFI:CPNT:Length", "omfi:Length32", len);
    }
    fn mob(&mut self, class: &[u8; 4], id: Uid, name: &str, slots: &[u32]) -> u32 {
        let m = self.obj(class);
        self.b.set(m, "OMFI:MOBJ:MobID", "omfi:UID", &id);
        self.str(m, "OMFI:MOBJ:Name", name);
        self.refs(m, "OMFI:MOBJ:Slots", slots);
        self.b.set(m, "OMFI:MOBJ:LastModified", "omfi:TimeStamp", &[0, 0, 0, 0, 1]);
        self.b.set(m, "OMFI:MOBJ:CreationTime", "omfi:TimeStamp", &[0, 0, 0, 0, 1]);
        m
    }
    fn slot(&mut self, rate: (i64, i64), segment: u32, track_id: u32, name: &str, physical: u32) -> u32 {
        let t = self.obj(b"TRKD");
        self.i32(t, "OMFI:TRKD:Origin", "omfi:Position32", 0);
        self.b.set(t, "OMFI:TRKD:TrackID", "omfi:UInt32", &track_id.to_be_bytes());
        self.str(t, "OMFI:TRKD:TrackName", name);
        if physical > 0 {
            self.b.set(t, "OMFI:TRKD:PhysicalTrack", "omfi:UInt32", &physical.to_be_bytes());
        }
        let s = self.obj(b"MSLT");
        self.b.set(s, "OMFI:MSLT:EditRate", "omfi:Rational", &rational(rate.0, rate.1));
        self.r#ref(s, "OMFI:MSLT:Segment", segment);
        self.r#ref(s, "OMFI:MSLT:TrackDesc", t);
        s
    }
    fn sclp(&mut self, kind: &'static str, len: i64, source: Uid, track: u32, start: i64) -> u32 {
        let c = self.obj(b"SCLP");
        self.cpnt(c, kind, len);
        self.b.set(c, "OMFI:SCLP:SourceID", "omfi:UID", &source);
        self.b.set(c, "OMFI:SCLP:SourceTrackID", "omfi:UInt32", &track.to_be_bytes());
        self.i32(c, "OMFI:SCLP:StartTime", "omfi:Position32", start);
        c
    }
    fn tccp(&mut self, len: i64, start: i64, fps: i64, drop: bool) -> u32 {
        let t = self.obj(b"TCCP");
        self.cpnt(t, "omfi:data:Timecode", len);
        self.i32(t, "OMFI:TCCP:Start", "omfi:Position32", start);
        self.b.set(t, "OMFI:TCCP:FPS", "omfi:UInt16", &(fps.clamp(1, u16::MAX as i64) as u16).to_be_bytes());
        self.b.set(t, "OMFI:TCCP:Drop", "omfi:Boolean", &[drop as u8]);
        t
    }
    fn value(&mut self, len: i64, g: &Gain, seg_len: i64, rate: (i64, i64), base: Tick) -> u32 {
        match g {
            Gain::Constant(a) => {
                let v = self.obj(b"CVAL");
                self.cpnt(v, "omfi:data:Rational", len);
                self.b.set(v, "OMFI:CVAL:Value", "omfi:DataValue", &amp(*a));
                v
            }
            Gain::Varying { linear, points } => {
                let mut pts = Vec::new();
                for (off, a) in points {
                    let o = self.u(base + *off, rate) - self.u(base, rate);
                    let p = self.obj(b"CTLP");
                    self.b.set(p, "OMFI:CTLP:Time", "omfi:Rational", &rational(o, seg_len.max(1)));
                    self.b.set(p, "OMFI:CTLP:Value", "omfi:DataValue", &amp(*a));
                    self.b.set(p, "OMFI:CTLP:EditHint", "omfi:EditHintType", &[0]);
                    pts.push(p);
                }
                let v = self.obj(b"VVAL");
                self.cpnt(v, "omfi:data:Rational", len);
                self.b.set(v, "OMFI:VVAL:Interpolation", "omfi:InterpKind", &(if *linear { 2u16 } else { 1u16 }).to_be_bytes());
                self.refs(v, "OMFI:VVAL:PointList", &pts);
                v
            }
        }
    }
}

fn fnv(s: &[u8], h: u64) -> u64 {
    s.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3))
}

fn amp(a: f64) -> Vec<u8> {
    rational((a.max(0.0) * 65536.0).round().min(i32::MAX as f64) as i64, 65536)
}

fn rate_of(s: &Source) -> (i64, i64) {
    match s.kind {
        CKind::Picture => (s.frame_rate.num, s.frame_rate.den),
        CKind::Sound => (s.sample_rate.max(1) as i64, 1),
    }
}

pub(crate) fn write(doc: &Document, report: &mut Report) -> Result<Vec<u8>> {
    let comp = doc.compositions.first().ok_or(Error::Empty)?;
    let mut w = W { b: bento::Writer::new(), refs: HashMap::new(), ddefs: HashMap::new(), edefs: HashMap::new(), defs: Vec::new(), report, inexact: false };
    let mut seed = fnv(comp.name.as_bytes(), 0xCBF2_9CE4_8422_2325);
    for s in &doc.sources {
        seed = fnv(s.path.as_deref().unwrap_or("").as_bytes(), fnv(s.key.as_bytes(), seed));
    }
    let major = (seed >> 32) as u32 ^ seed as u32;
    let mut minor = 0u32;
    let mut new_uid = || {
        minor += 1;
        uid(0x464C_4D43, major, minor)
    };
    let mut mobs = Vec::new();
    let mut media_data = Vec::new();
    let mut master_ref: HashMap<usize, (Uid, u32)> = HashMap::new();
    // media
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in doc.sources.iter().enumerate() {
        if s.kind != CKind::Sound {
            continue;
        }
        match groups.iter_mut().find(|(k, _)| *k == s.key) {
            Some((_, v)) => v.push(i),
            None => groups.push((s.key.clone(), vec![i])),
        }
    }
    for (_, members) in &groups {
        let first = &doc.sources[members[0]];
        let master = new_uid();
        let tc = members.iter().find_map(|&i| doc.sources[i].start_tc.map(|t| (t, doc.sources[i].tc_rate)));
        let tape = new_uid();
        let mut tape_slots = Vec::new();
        if let Some((start, r)) = tc {
            let len = members.iter().map(|&i| doc.sources[i].length).max().unwrap_or(Tick::ZERO);
            let l = w.u(len, (r.num, r.den));
            let t = w.tccp(l, start, r.timecode_base(), false);
            tape_slots.push(w.slot((r.num, r.den), t, 1, "TC1", 0));
        }
        let mut master_slots = Vec::new();
        for (k, &si) in members.iter().enumerate() {
            let s = &doc.sources[si];
            let rate = rate_of(s);
            let len = w.u(s.length, rate);
            // file mob
            let file = new_uid();
            let file_track = s.channel.map_or(1, |c| c + 1);
            let tape_track = 2 + k as u32;
            let to_tape = if tc.is_some() { w.sclp("omfi:data:Sound", len, tape, tape_track, 0) } else { w.sclp("omfi:data:Sound", len, uid(0, 0, 0), 0, 0) };
            let fslot = w.slot(rate, to_tape, file_track, &format!("A{file_track}"), file_track);
            let desc = descriptor(&mut w, s, len);
            let fm = w.mob(b"SMOB", file, &s.name, &[fslot]);
            w.r#ref(fm, "OMFI:SMOB:MediaDescription", desc);
            mobs.push(fm);
            if let Some(pcm) = &s.embedded {
                let md = w.obj(b"WAVE");
                w.b.set(md, "OMFI:MDAT:MobID", "omfi:UID", &file);
                w.b.set(
                    md,
                    "OMFI:WAVE:Data",
                    "omfi:DataValue",
                    &crate::wav::wav_file(pcm, u16::try_from(s.file_channels.max(1)).unwrap_or(u16::MAX), s.sample_rate, s.bits),
                );
                media_data.push(md);
            }
            if tc.is_some() {
                let c = w.sclp("omfi:data:Sound", len, uid(0, 0, 0), 0, 0);
                tape_slots.push(w.slot(rate, c, tape_track, &format!("A{}", k + 1), k as u32 + 1));
            }
            let mclip = w.sclp("omfi:data:Sound", len, file, file_track, 0);
            master_slots.push(w.slot(rate, mclip, k as u32 + 1, &format!("A{}", k + 1), k as u32 + 1));
            master_ref.insert(si, (master, k as u32 + 1));
        }
        mobs.push(w.mob(b"MMOB", master, &first.name, &master_slots));
        if tc.is_some() {
            let d = w.obj(b"MDTP");
            let reel = first.path.as_deref().map(crate::common::file_stem).unwrap_or(&first.name).to_string();
            let tm = w.mob(b"SMOB", tape, &reel, &tape_slots);
            w.r#ref(tm, "OMFI:SMOB:MediaDescription", d);
            mobs.push(tm);
        }
    }
    // composition
    let crate_rate = (comp.rate.num, comp.rate.den);
    let mut slots = Vec::new();
    let total = comp
        .tracks
        .iter()
        .map(|t| t.items.iter().fold(Tick::ZERO, |a, i| if matches!(i, CItem::Transition(_)) { a - i.len() } else { a + i.len() }))
        .max()
        .unwrap_or(Tick::ZERO);
    let tl = w.u(total, crate_rate);
    let tcc = w.tccp(tl, comp.start_tc, comp.rate.timecode_base(), comp.drop);
    slots.push(w.slot(crate_rate, tcc, 1, "TC1", 0));
    for (track_id, t) in (2..).zip(comp.tracks.iter().filter(|t| t.kind == CKind::Sound)) {
        let rate = (comp.sample_rate.max(1) as i64, 1);
        let mut comps = Vec::new();
        let mut cursor = Tick::ZERO;
        for it in &t.items {
            match it {
                CItem::Filler(l) => {
                    let n = w.u(cursor + *l, rate) - w.u(cursor, rate);
                    cursor += *l;
                    let f = w.obj(b"FILL");
                    w.cpnt(f, "omfi:data:Sound", n);
                    comps.push(f);
                }
                CItem::Clip(c) => {
                    let n = w.u(cursor + c.len, rate) - w.u(cursor, rate);
                    let (mid, mtrack) = master_ref.get(&c.source).copied().unwrap_or((uid(0, 0, 0), 0));
                    let start = w.u(c.start, rate);
                    let sc = w.sclp("omfi:data:Sound", n, mid, mtrack, start);
                    if !c.name.is_empty() {
                        w.str(sc, "FilmCraft:SCLP:ClipName", &c.name);
                    }
                    let seg = match &c.gain {
                        Some(g) => {
                            let v = w.value(n, g, n, rate, cursor);
                            let input = w.obj(b"ESLT");
                            w.b.set(input, "OMFI:ESLT:ArgID", "omfi:ArgIDType", &(-1i32).to_be_bytes());
                            w.r#ref(input, "OMFI:ESLT:ArgValue", sc);
                            let level = w.obj(b"ESLT");
                            w.b.set(level, "OMFI:ESLT:ArgID", "omfi:ArgIDType", &1i32.to_be_bytes());
                            w.r#ref(level, "OMFI:ESLT:ArgValue", v);
                            let kind = w.edef("omfi:effect:MonoAudioGain", "Mono Audio Gain");
                            let e = w.obj(b"EFFE");
                            w.cpnt(e, "omfi:data:Sound", n);
                            w.r#ref(e, "OMFI:EFFE:EffectKind", kind);
                            w.refs(e, "OMFI:EFFE:EffectSlots", &[input, level]);
                            e
                        }
                        None => sc,
                    };
                    comps.push(seg);
                    cursor += c.len;
                }
                CItem::Transition(x) => {
                    let ts = cursor - x.len;
                    let n = w.u(cursor, rate) - w.u(ts, rate);
                    let cut = w.u(ts + x.cut, rate) - w.u(ts, rate);
                    cursor = ts;
                    let kind = w.edef("omfi:effect:SimpleMonoAudioDissolve", "Simple Mono Audio Dissolve");
                    let e = w.obj(b"EFFE");
                    w.cpnt(e, "omfi:data:Sound", n);
                    w.r#ref(e, "OMFI:EFFE:EffectKind", kind);
                    w.refs(e, "OMFI:EFFE:EffectSlots", &[]);
                    w.str(e, "FilmCraft:EFFE:EffectID", &x.effect);
                    let tr = w.obj(b"TRAN");
                    w.cpnt(tr, "omfi:data:Sound", n);
                    w.i32(tr, "OMFI:TRAN:CutPoint", "omfi:Position32", cut);
                    w.r#ref(tr, "OMFI:TRAN:Effect", e);
                    comps.push(tr);
                }
            }
        }
        let len = w.u(cursor.max(Tick::ZERO), rate);
        let seq = w.obj(b"SEQU");
        w.cpnt(seq, "omfi:data:Sound", len);
        w.refs(seq, "OMFI:SEQU:Components", &comps);
        slots.push(w.slot(rate, seq, track_id, &t.name, t.number));
    }
    let cmob = w.mob(b"CMOB", new_uid(), &comp.name, &slots);
    mobs.insert(0, cmob);
    if w.inexact {
        w.report.info("some times are not on whole samples / frames and were rounded");
    }
    let ident = w.obj(b"IDNT");
    w.str(ident, "OMFI:IDNT:CompanyName", "FilmCraft");
    w.str(ident, "OMFI:IDNT:ProductName", "FilmCraft");
    w.str(ident, "OMFI:IDNT:ProductVersionString", env!("CARGO_PKG_VERSION"));
    w.str(ident, "OMFI:IDNT:Platform", "FilmCraft");
    w.b.set(ident, "OMFI:IDNT:Date", "omfi:TimeStamp", &[0, 0, 0, 0, 1]);
    let head = w.obj(b"HEAD");
    w.b.set(head, "OMFI:HEAD:ByteOrder", "omfi:Int16", &0x4D4Di16.to_be_bytes());
    w.b.set(head, "OMFI:HEAD:LastModified", "omfi:TimeStamp", &[0, 0, 0, 0, 1]);
    w.b.set(head, "OMFI:HEAD:Version", "omfi:VersionType", &[2, 0]);
    w.refs(head, "OMFI:HEAD:Mobs", &mobs);
    w.refs(head, "OMFI:HEAD:MediaData", &media_data);
    w.refs(head, "OMFI:HEAD:PrimaryMobs", &[cmob]);
    let defs = w.defs.clone();
    w.refs(head, "OMFI:HEAD:DefinitionObjects", &defs);
    w.refs(head, "OMFI:HEAD:IdentificationList", &[ident]);
    let refs = std::mem::take(&mut w.refs);
    let mut keys: Vec<_> = refs.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        let mut t = refs[&k].clone();
        t.sort_unstable();
        t.dedup();
        w.b.set_references(k, &t);
    }
    Ok(w.b.finish())
}

fn descriptor(w: &mut W, s: &Source, len: i64) -> u32 {
    let aiff = s.embedded.is_none() && s.path.as_deref().is_some_and(|p| matches!(crate::common::extension(p).as_str(), "aif" | "aiff" | "aifc"));
    let d = w.obj(if aiff { b"AIFD" } else { b"WAVD" });
    w.b.set(d, "OMFI:MDFL:IsOMFI", "omfi:Boolean", &[s.embedded.is_some() as u8]);
    let r = rate_of(s);
    w.b.set(d, "OMFI:MDFL:SampleRate", "omfi:Rational", &rational(r.0, r.1));
    w.i32(d, "OMFI:MDFL:Length", "omfi:Length32", len);
    let frames = u32::try_from(len.max(0)).unwrap_or(u32::MAX);
    // Channel counts come from the project: saturate, as the headers can't hold more anyway.
    let bytes_per_frame = s.file_channels.max(1).saturating_mul((s.bits as u32).div_ceil(8));
    let channels = u16::try_from(s.file_channels.max(1)).unwrap_or(u16::MAX);
    if aiff {
        w.b.set(d, "OMFI:AIFD:Summary", "omfi:DataValue", &crate::wav::aiff_header(frames, channels, s.sample_rate, s.bits));
    } else {
        w.b.set(d, "OMFI:WAVD:Summary", "omfi:DataValue", &crate::wav::wav_header(frames.saturating_mul(bytes_per_frame), channels, s.sample_rate, s.bits));
    }
    if let Some(p) = &s.path {
        let mut locs = Vec::new();
        let dos = p.len() > 2 && p.as_bytes()[1] == b':';
        let l = w.obj(if dos { b"DOSL" } else { b"UNXL" });
        w.str(l, if dos { "OMFI:DOSL:PathName" } else { "OMFI:UNXL:PathName" }, p);
        locs.push(l);
        if crate::common::is_absolute(p) {
            let n = w.obj(b"NETL");
            w.str(n, "OMFI:NETL:URLString", &crate::common::path_to_file_url(p, false));
            locs.push(n);
        }
        w.refs(d, "OMFI:MDES:Locator", &locs);
    }
    d
}
