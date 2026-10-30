//! Small synthetic Bento graphs; no external media or binary fixtures.
use super::{ImportOptions, bento::Writer, import, read};
use crate::{
    Error, Report,
    comp::{CItem, Gain},
};
use filmcraft_time::Tick;

struct Graph {
    w: Writer,
    head: u32,
    mob: u32,
    sound: u32,
    mobs: Vec<u32>,
}

impl Graph {
    fn new() -> Self {
        let mut g = Self { w: Writer::new(), head: 0, mob: 0, sound: 0, mobs: Vec::new() };
        g.head = g.obj(b"HEAD");
        g.mob = g.obj(b"CMOB");
        g.sound = g.obj(b"DDEF");
        g.w.set(g.sound, "OMFI:DDEF:DataKindID", "omfi:UniqueName", b"omfi:data:Sound\0");
        g.mobs.push(g.mob);
        g
    }
    fn obj(&mut self, class: &[u8; 4]) -> u32 {
        let o = self.w.new_object();
        self.w.set(o, "OMFI:OOBJ:ObjClass", "omfi:ClassID", class);
        o
    }
    fn component(&mut self, class: &[u8; 4]) -> u32 {
        let o = self.obj(class);
        self.one(o, "OMFI:CPNT:DataKind", self.sound);
        self.w.set(o, "OMFI:CPNT:Length", "omfi:Length32", &1i32.to_be_bytes());
        o
    }
    fn one(&mut self, o: u32, p: &str, target: u32) {
        self.w.set(o, p, "omfi:ObjRef", &target.to_be_bytes());
    }
    fn refs(&mut self, o: u32, p: &str, targets: &[u32]) {
        let mut bytes = u16::try_from(targets.len()).unwrap().to_be_bytes().to_vec();
        for t in targets {
            bytes.extend_from_slice(&t.to_be_bytes());
        }
        self.w.set(o, p, "omfi:ObjRefArray", &bytes);
    }
    fn sequence(&mut self, targets: &[u32]) -> u32 {
        let o = self.component(b"SEQU");
        self.refs(o, "OMFI:SEQU:Components", targets);
        o
    }
    fn slot(&mut self, seg: u32) -> u32 {
        let s = self.obj(b"MSLT");
        self.one(s, "OMFI:MSLT:Segment", seg);
        let t = self.obj(b"TRKD");
        self.w.set(t, "OMFI:TRKD:TrackID", "omfi:UInt32", &1u32.to_be_bytes());
        self.one(s, "OMFI:MSLT:TrackDesc", t);
        s
    }
    fn source_clip(&mut self, class: &[u8; 4], seg: u32) -> u32 {
        let mob = self.obj(class);
        self.mobs.push(mob);
        let mut uid = [0; 12];
        uid[..4].copy_from_slice(&mob.to_be_bytes());
        self.w.set(mob, "OMFI:MOBJ:MobID", "omfi:UID", &uid);
        let slot = self.slot(seg);
        self.refs(mob, "OMFI:MOBJ:Slots", &[slot]);
        if class == b"SMOB" {
            let desc = self.obj(b"WAVD");
            self.one(mob, "OMFI:SMOB:MediaDescription", desc);
            let mut rate = 48_000i32.to_be_bytes().to_vec();
            rate.extend_from_slice(&1i32.to_be_bytes());
            self.w.set(desc, "OMFI:MDFL:SampleRate", "omfi:Rational", &rate);
        }
        let clip = self.component(b"SCLP");
        self.w.set(clip, "OMFI:SCLP:SourceID", "omfi:UID", &uid);
        self.w.set(clip, "OMFI:SCLP:SourceTrackID", "omfi:UInt32", &1u32.to_be_bytes());
        clip
    }
    fn finish(mut self, seg: u32) -> Vec<u8> {
        let s = self.slot(seg);
        self.refs(self.mob, "OMFI:MOBJ:Slots", &[s]);
        self.refs(self.head, "OMFI:HEAD:PrimaryMobs", &[self.mob]);
        let mobs = self.mobs.clone();
        self.refs(self.head, "OMFI:HEAD:Mobs", &mobs);
        self.w.finish()
    }
}

fn rejected(bytes: &[u8], reason: &str) {
    let result = std::panic::catch_unwind(|| import(bytes, &ImportOptions::default())).expect("OMF import panicked");
    let Err(Error::Parse { format: "OMF", message }) = result else { panic!("expected OMF parse error") };
    assert!(message.contains(reason), "{message}");
}

#[test]
fn sequence_cycle_is_rejected() {
    for fanout in [1, 2] {
        let mut g = Graph::new();
        let seq = g.component(b"SEQU");
        g.refs(seq, "OMFI:SEQU:Components", &vec![seq; fanout]);
        rejected(&g.finish(seq), "cycle");
    }
}

#[test]
fn repeated_dag_expansion_is_bounded() {
    for empty in [false, true] {
        let mut g = Graph::new();
        let mut seg = if empty { g.sequence(&[]) } else { g.component(b"FILL") };
        // Only 21 segment objects, but 2,097,151 visits even with no output items.
        for _ in 0..20 {
            seg = g.sequence(&[seg, seg]);
        }
        rejected(&g.finish(seg), "budget");
    }
}

#[test]
fn shared_segments_preserve_every_occurrence() {
    let mut g = Graph::new();
    let a = g.component(b"FILL");
    let b = g.obj(b"FILL");
    g.w.set(b, "OMFI:CPNT:Length", "omfi:Length32", &2i32.to_be_bytes());
    let seq = g.sequence(&[a, b]);
    let top = g.sequence(&[seq, a, seq]);
    let bytes = g.finish(top);
    let doc = read::read(&bytes, &mut Report::default()).unwrap();
    let items = &doc.compositions[0].tracks[0].items;
    let lengths: Vec<Tick> = items
        .iter()
        .map(|i| match i {
            CItem::Filler(t) => *t,
            _ => panic!("expected filler"),
        })
        .collect();
    assert_eq!(lengths, [1, 2, 1, 1, 2].map(|n| Tick(n * filmcraft_time::TICKS_PER_SECOND / 25)));
}

#[test]
fn effect_input_cycle_is_rejected() {
    let mut g = Graph::new();
    let effect = g.component(b"EFFE");
    let seq = g.sequence(&[effect]);
    let slot = g.obj(b"ESLT");
    g.w.set(slot, "OMFI:ESLT:ArgID", "omfi:ArgIDType", &(-1i32).to_be_bytes());
    g.one(slot, "OMFI:ESLT:ArgValue", seq);
    g.refs(effect, "OMFI:EFFE:EffectSlots", &[slot]);
    rejected(&g.finish(effect), "cycle");
}

#[test]
fn segment_depth_limit_is_explicit() {
    for levels in [32, 33] {
        let mut g = Graph::new();
        let mut seg = g.component(b"FILL");
        for _ in 0..levels {
            seg = g.sequence(&[seg]);
        }
        let bytes = g.finish(seg);
        if levels == 32 {
            assert!(import(&bytes, &ImportOptions::default()).is_ok());
        } else {
            rejected(&bytes, "depth limit");
        }
    }
}

#[test]
fn master_sequence_cycle_is_rejected() {
    let mut g = Graph::new();
    let seq = g.component(b"SEQU");
    g.refs(seq, "OMFI:SEQU:Components", &[seq]);
    let clip = g.source_clip(b"MMOB", seq);
    rejected(&g.finish(clip), "cycle");
}

#[test]
fn source_timecode_sequence_cycle_is_rejected() {
    let mut g = Graph::new();
    let seq = g.component(b"SEQU");
    g.refs(seq, "OMFI:SEQU:Components", &[seq]);
    let clip = g.source_clip(b"SMOB", seq);
    rejected(&g.finish(clip), "cycle");
}

#[test]
fn source_lookup_expansion_is_bounded() {
    let mut g = Graph::new();
    let mut seg = g.component(b"FILL");
    for _ in 0..20 {
        seg = g.sequence(&[seg, seg]);
    }
    let clip = g.source_clip(b"MMOB", seg);
    rejected(&g.finish(clip), "budget");
}

#[test]
fn source_chain_depth_limit_preserves_audio_or_returns_an_error() {
    for depth in [16, 17] {
        let mut g = Graph::new();
        let filler = g.component(b"FILL");
        let mut clip = g.source_clip(b"SMOB", filler);
        for _ in 0..depth {
            clip = g.source_clip(b"MMOB", clip);
        }
        let bytes = g.finish(clip);
        if depth == 16 {
            let doc = read::read(&bytes, &mut Report::default()).unwrap();
            assert_eq!(doc.sources.len(), 1);
            assert!(matches!(doc.compositions[0].tracks[0].items.as_slice(), [CItem::Clip(c)] if c.source == 0));
        } else {
            rejected(&bytes, "source chain exceeds depth limit");
        }
    }
}

#[test]
fn source_chain_cycles_return_an_error_instead_of_filler() {
    for length in [1, 2] {
        let mut g = Graph::new();
        let start = g.component(b"SCLP");
        let mut clip = start;
        for _ in 0..length {
            clip = g.source_clip(b"MMOB", clip);
        }
        // Close the chain by pointing its innermost clip back to the outermost master mob.
        let mut uid = [0; 12];
        uid[..4].copy_from_slice(&g.mobs.last().unwrap().to_be_bytes());
        g.w.set(start, "OMFI:SCLP:SourceID", "omfi:UID", &uid);
        g.w.set(start, "OMFI:SCLP:SourceTrackID", "omfi:UInt32", &1u32.to_be_bytes());
        rejected(&g.finish(clip), "source chain exceeds depth limit");
    }
}

#[test]
fn budget_is_shared_across_tracks_and_compositions() {
    for compositions in [false, true] {
        let mut g = Graph::new();
        let mut seg = g.component(b"FILL");
        // Each expansion fits individually, but the whole document does not.
        for _ in 0..18 {
            seg = g.sequence(&[seg, seg]);
        }
        let slot = g.slot(seg);
        if compositions {
            g.refs(g.mob, "OMFI:MOBJ:Slots", &[slot]);
            let second = g.obj(b"CMOB");
            g.mobs.push(second);
            g.mob = second;
        } else {
            g.refs(g.mob, "OMFI:MOBJ:Slots", &[slot, slot, slot]);
        }
        // Write the fixture directly so every property has only one value.
        let mobs = g.mobs.clone();
        g.refs(g.head, "OMFI:HEAD:Mobs", &mobs);
        if compositions {
            g.refs(g.mob, "OMFI:MOBJ:Slots", &[slot, slot]);
        }
        rejected(&g.w.finish(), "budget");
    }
}

#[test]
fn shared_source_clips_are_not_dropped() {
    let mut g = Graph::new();
    let filler = g.component(b"FILL");
    let clip = g.source_clip(b"SMOB", filler);
    let seq = g.sequence(&[clip, clip]);
    let top = g.sequence(&[seq, clip, seq]);
    let doc = read::read(&g.finish(top), &mut Report::default()).unwrap();
    assert_eq!(doc.sources.len(), 1);
    let items = &doc.compositions[0].tracks[0].items;
    assert_eq!(items.len(), 5);
    assert!(items.iter().all(|i| matches!(i, CItem::Clip(c) if c.source == 0)));
}

#[test]
fn mutated_reference_graphs_never_panic() {
    let mut g = Graph::new();
    let filler = g.component(b"FILL");
    let seq = g.sequence(&[filler, filler]);
    let bytes = g.finish(seq);
    for at in (0..bytes.len()).step_by(17) {
        let result = std::panic::catch_unwind(|| import(&bytes[..at], &ImportOptions::default()));
        assert!(result.is_ok(), "truncation at {at} panicked");
        for mask in [1, 0x80, 0xff] {
            let mut changed = bytes.clone();
            changed[at] ^= mask;
            let result = std::panic::catch_unwind(|| import(&changed, &ImportOptions::default()));
            assert!(result.is_ok(), "mutation at {at} panicked");
        }
    }
}

#[test]
fn discovery_budget_is_independent_of_expansion_and_counts_repeated_references() {
    // Discovery also visits the composition, slot and sequence: the first case
    // uses exactly 65,536 visits and must leave expansion its own full allowance.
    for count in [65_533, 65_534] {
        let mut g = Graph::new();
        let filler = g.component(b"FILL");
        let seq = g.sequence(&vec![filler; count]);
        let bytes = g.finish(seq);
        if count == 65_533 {
            let doc = read::read(&bytes, &mut Report::default()).unwrap();
            assert_eq!(doc.compositions[0].tracks[0].items.len(), count);
        } else {
            rejected(&bytes, "reference discovery budget");
        }
    }
}

#[test]
fn early_source_lookup_still_charges_the_reference_list() {
    for count in [128, 1024] {
        let mut g = Graph::new();
        let filler = g.component(b"FILL");
        let source = g.source_clip(b"SMOB", filler);
        let master_seq = g.sequence(&vec![source; 1024]);
        let clip = g.source_clip(b"MMOB", master_seq);
        let top = g.sequence(&vec![clip; count]);
        let bytes = g.finish(top);
        if count == 128 {
            let doc = read::read(&bytes, &mut Report::default()).unwrap();
            let items = &doc.compositions[0].tracks[0].items;
            assert_eq!(items.len(), count);
            assert!(items.iter().all(|i| matches!(i, CItem::Clip(c) if c.source == 0)));
        } else {
            rejected(&bytes, "expansion budget");
        }
    }
}

fn automated_clips(count: usize) -> Vec<u8> {
    let mut g = Graph::new();
    let filler = g.component(b"FILL");
    let clip = g.source_clip(b"SMOB", filler);
    let mut points = Vec::new();
    for n in 0..1024i32 {
        let point = g.obj(b"CTLP");
        let rational = [n.to_be_bytes(), 1024i32.to_be_bytes()].concat();
        g.w.set(point, "OMFI:CTLP:Time", "omfi:Rational", &rational);
        g.w.set(point, "OMFI:CTLP:Value", "omfi:DataValue", &rational);
        points.push(point);
    }
    let value = g.component(b"VVAL");
    g.refs(value, "OMFI:VVAL:PointList", &points);
    let effect = g.component(b"EFFE");
    let kind = g.obj(b"EDEF");
    g.w.set(kind, "OMFI:EDEF:EffectID", "omfi:UniqueName", b"omfi:effect:MonoAudioGain\0");
    g.one(effect, "OMFI:EFFE:EffectKind", kind);
    let mut slots = Vec::new();
    for (id, arg) in [(-1i32, clip), (1, value)] {
        let slot = g.obj(b"ESLT");
        g.w.set(slot, "OMFI:ESLT:ArgID", "omfi:ArgIDType", &id.to_be_bytes());
        g.one(slot, "OMFI:ESLT:ArgValue", arg);
        slots.push(slot);
    }
    g.refs(effect, "OMFI:EFFE:EffectSlots", &slots);
    let top = g.sequence(&vec![effect; count]);
    g.finish(top)
}

#[test]
fn dense_automation_above_the_discovery_budget_preserves_every_point() {
    // 131,072 expanded gain points, plus composition and source lookup work.
    let bytes = automated_clips(128);
    assert!(import(&bytes, &ImportOptions::default()).is_ok());
    let doc = read::read(&bytes, &mut Report::default()).unwrap();
    let items = &doc.compositions[0].tracks[0].items;
    assert_eq!(items.len(), 128);
    for item in items {
        let CItem::Clip(c) = item else { panic!("expected clip") };
        assert_eq!(c.source, 0);
        let Some(Gain::Varying { linear, points }) = &c.gain else { panic!("expected automation") };
        assert!(linear);
        assert_eq!(points.len(), 1024);
        for (n, &(time, value)) in points.iter().enumerate() {
            assert_eq!(time, Tick(c.len.0 * n as i64 / 1024));
            assert_eq!(value, n as f64 / 1024.0);
        }
    }
}

#[test]
fn repeated_gain_points_consume_the_expansion_budget() {
    rejected(&automated_clips(1024), "expansion budget");
}
