//! Composition model → AAF objects.

use std::collections::HashMap;

use filmcraft_time::{FrameRate, Tick};

use super::ids::{META_DICTIONARY, ROOT, cls, ddef, def, path, pid};
use super::store::{self, Auid, Obj, Value, rational, utf16z};
use crate::comp::{CItem, CKind, CMarker, Composition, Document, Gain, Source, ticks_to_units};
use crate::{Error, Report, Result};

/// UMID-style mob id (SMPTE 330 basic UMID): label, length 0x13, instance 0, material number.
pub(crate) fn mob_id(seed: u64, n: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..12].copy_from_slice(&[0x06, 0x0A, 0x2B, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x01, 0x0F, 0x20]);
    id[12] = 0x13;
    let a = mix(seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let b = mix(a ^ 0xD6E8_FEB8_6659_FD93);
    id[16..24].copy_from_slice(&a.to_le_bytes());
    id[24..32].copy_from_slice(&b.to_le_bytes());
    // a "random" material number per RFC 4122 v4 layout keeps it a valid UUID
    id[22] = (id[22] & 0x0F) | 0x40;
    id[24] = (id[24] & 0x3F) | 0x80;
    id
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub(crate) fn fnv(s: &[u8], h: u64) -> u64 {
    s.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3))
}

fn timestamp() -> Vec<u8> {
    // 1970-01-01 00:00:00.00: FilmCraft writes reproducible files
    let mut v = 1970i16.to_le_bytes().to_vec();
    v.extend_from_slice(&[1, 1, 0, 0, 0, 0]);
    v
}

fn weak(path: [u16; 3], key: Auid) -> Value {
    Value::Weak(path.to_vec(), pid::IDENTIFICATION, key.to_vec())
}

fn data_def(kind: CKind) -> Auid {
    match kind {
        CKind::Picture => ddef::PICTURE,
        CKind::Sound => ddef::SOUND,
    }
}

/// An indirect value: byte order, type id, value.
pub(crate) fn indirect(ty: Auid, v: &[u8]) -> Vec<u8> {
    let mut d = vec![0x4C];
    d.extend_from_slice(&ty);
    d.extend_from_slice(v);
    d
}

fn tagged(name: &str, value: &str) -> Obj {
    Obj::new(cls::TAGGED_VALUE).data_prop(pid::TAG_NAME, utf16z(name)).data_prop(pid::TAG_VALUE, indirect(def::TYPE_STRING, &utf16z(value)))
}

fn gain_rational(g: f64) -> Vec<u8> {
    rational((g.max(0.0) * 65536.0).round().min(i32::MAX as f64) as i64, 65536)
}

fn definition(class: Auid, id: Auid, name: &str) -> Obj {
    Obj::new(class).data_prop(pid::IDENTIFICATION, id.to_vec()).data_prop(pid::NAME, utf16z(name)).data_prop(pid::DESCRIPTION, utf16z(""))
}

struct W<'a> {
    report: &'a mut Report,
    inexact: bool,
}

struct IdGen {
    seed: u64,
    next: u64,
}

impl IdGen {
    fn for_document(doc: &Document, comp: &Composition) -> Self {
        let mut seed = fnv(comp.name.as_bytes(), 0xCBF2_9CE4_8422_2325);
        for source in &doc.sources {
            seed = fnv(source.path.as_deref().unwrap_or("").as_bytes(), fnv(source.key.as_bytes(), seed));
        }
        seed = fnv(&(comp.tracks.len() as u64).to_le_bytes(), seed);
        Self { seed, next: 0 }
    }

    fn next(&mut self) -> [u8; 32] {
        self.next += 1;
        mob_id(self.seed, self.next)
    }
}

struct MediaOutput {
    mobs: Vec<Obj>,
    essence: Vec<Obj>,
    master_ref: HashMap<usize, ([u8; 32], u32)>,
}

type FileMob = ((Option<String>, usize, CKind), [u8; 32], Vec<Obj>, usize);

struct GroupSlots {
    master_id: [u8; 32],
    tape_id: [u8; 32],
    timecode: Option<(i64, filmcraft_time::FrameRate)>,
    master_slots: Vec<Obj>,
    tape_slots: Vec<Obj>,
    files: Vec<FileMob>,
    master_ref: HashMap<usize, ([u8; 32], u32)>,
}

struct GroupSlotBuilder<'a, 'b> {
    doc: &'a Document,
    master_id: [u8; 32],
    tape_id: [u8; 32],
    ids: &'b mut IdGen,
    files: &'b mut Vec<FileMob>,
    master_slots: &'b mut Vec<Obj>,
    tape_slots: &'b mut Vec<Obj>,
    master_ref: &'b mut HashMap<usize, ([u8; 32], u32)>,
}

struct FileSlotInput<'a> {
    source: &'a Source,
    file_slot: u32,
    length: i64,
    has_timecode: bool,
    tape_id: [u8; 32],
    member_index: usize,
    slots: &'a mut Vec<Obj>,
    rate: (i64, i64),
}

impl W<'_> {
    /// Edit units at `rate` of tick `t`.
    fn u(&mut self, t: Tick, rate: (i64, i64)) -> i64 {
        let (n, exact) = ticks_to_units(t, rate.0, rate.1);
        if !exact {
            self.inexact = true;
        }
        n
    }
}

fn source_rate(s: &Source) -> (i64, i64) {
    match s.kind {
        CKind::Picture => (s.frame_rate.num, s.frame_rate.den),
        CKind::Sound => (s.sample_rate.max(1) as i64, 1),
    }
}

fn write_media(doc: &Document, w: &mut W<'_>, ids: &mut IdGen) -> MediaOutput {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, source) in doc.sources.iter().enumerate() {
        if source.nested.is_some() {
            continue;
        }
        match groups.iter_mut().find(|(key, _)| *key == source.key) {
            Some((_, members)) => members.push(index),
            None => groups.push((source.key.clone(), vec![index])),
        }
    }

    let mut output = MediaOutput { mobs: Vec::new(), essence: Vec::new(), master_ref: HashMap::new() };
    for (_, members) in groups {
        write_source_group(doc, &members, w, ids, &mut output);
    }
    output
}

fn write_source_group(doc: &Document, members: &[usize], w: &mut W<'_>, ids: &mut IdGen, output: &mut MediaOutput) {
    let first = &doc.sources[members[0]];
    let slots = build_group_slots(doc, members, w, ids);
    output.master_ref.extend(slots.master_ref);
    let mut master_slots = slots.master_slots;
    add_master_markers(doc, members, w, &mut master_slots);
    output.mobs.push(
        Obj::new(cls::MASTER_MOB)
            .data_prop(pid::MOB_ID, slots.master_id.to_vec())
            .data_prop(pid::MOB_NAME, utf16z(&first.name))
            .data_prop(pid::MOB_LAST_MODIFIED, timestamp())
            .data_prop(pid::MOB_CREATION_TIME, timestamp())
            .with(pid::SLOTS, Value::StrongVec(master_slots)),
    );
    let tape_id = slots.tape_id;
    let timecode = slots.timecode;
    let tape_slots = slots.tape_slots;
    add_file_mobs(doc, slots.files, w, output);
    add_tape_mob(first, tape_id, timecode, tape_slots, output);
}

fn build_group_slots(doc: &Document, members: &[usize], w: &mut W<'_>, ids: &mut IdGen) -> GroupSlots {
    let master_id = ids.next();
    let tape_id = ids.next();
    let timecode = members.iter().find_map(|&i| doc.sources[i].start_tc.map(|t| (t, doc.sources[i].tc_rate)));
    let mut tape_slots = Vec::new();
    if let Some((start, rate)) = timecode {
        tape_slots.push(timecode_slot(members, doc, w, start, rate));
    }
    let mut files: Vec<FileMob> = Vec::new();
    let mut master_slots = Vec::new();
    let mut master_ref = HashMap::new();
    let mut builder = GroupSlotBuilder {
        doc,
        master_id,
        tape_id,
        ids,
        files: &mut files,
        master_slots: &mut master_slots,
        tape_slots: &mut tape_slots,
        master_ref: &mut master_ref,
    };
    for (member_index, &source_index) in members.iter().enumerate() {
        builder.add_source(source_index, member_index, timecode.is_some(), w);
    }
    GroupSlots { master_id, tape_id, timecode, master_slots, tape_slots, files, master_ref }
}

fn timecode_slot(members: &[usize], doc: &Document, w: &mut W<'_>, start: i64, rate: FrameRate) -> Obj {
    let edit_rate = (rate.num, rate.den);
    let len = members.iter().map(|&i| doc.sources[i].length).max().unwrap_or(Tick::ZERO);
    let units = w.u(len, edit_rate);
    timeline_slot(
        1,
        "TC1",
        0,
        edit_rate,
        Obj::new(cls::TIMECODE)
            .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, ddef::TIMECODE))
            .data_prop(pid::LENGTH, units.to_le_bytes().to_vec())
            .data_prop(pid::TC_START, start.to_le_bytes().to_vec())
            .data_prop(pid::TC_FPS, (rate.timecode_base().clamp(1, u16::MAX as i64) as u16).to_le_bytes().to_vec())
            .data_prop(pid::TC_DROP, vec![0]),
    )
}

impl GroupSlotBuilder<'_, '_> {
    fn add_source(&mut self, source_index: usize, member_index: usize, has_timecode: bool, w: &mut W<'_>) {
        let source = &self.doc.sources[source_index];
        let rate = source_rate(source);
        let embedded_key = if source.embedded.is_some() { source_index } else { usize::MAX };
        let file_key = (source.path.clone(), embedded_key, source.kind);
        let file_slot = source.channel.map_or(1, |channel| channel + 1);
        let len_units = w.u(source.length, rate);
        let file_index = match self.files.iter().position(|file| file.0 == file_key) {
            Some(index) => index,
            None => {
                self.files.push((file_key, self.ids.next(), Vec::new(), source_index));
                self.files.len() - 1
            }
        };
        let file_id = self.files[file_index].1;
        add_file_slot(FileSlotInput {
            source,
            file_slot,
            length: len_units,
            has_timecode,
            tape_id: self.tape_id,
            member_index,
            slots: &mut self.files[file_index].2,
            rate,
        });
        if has_timecode {
            let tape_slot_id = 2 + member_index as u32;
            self.tape_slots.push(timeline_slot(
                tape_slot_id,
                &slot_name(source.kind, member_index as u32 + 1),
                member_index as u32 + 1,
                rate,
                source_clip(source.kind, len_units, [0; 32], 0, 0),
            ));
        }
        let master_slot = member_index as u32 + 1;
        self.master_slots.push(timeline_slot(
            master_slot,
            &slot_name(source.kind, master_slot),
            master_slot,
            rate,
            source_clip(source.kind, len_units, file_id, file_slot, 0),
        ));
        self.master_ref.insert(source_index, (self.master_id, master_slot));
    }
}

fn add_file_slot(input: FileSlotInput<'_>) {
    let FileSlotInput { source, file_slot, length, has_timecode, tape_id, member_index, slots, rate } = input;
    if slots.iter().any(|slot| slot.u32(pid::SLOT_ID) == Some(file_slot)) {
        return;
    }
    let tape_slot_id = if has_timecode { 2 + member_index as u32 } else { 0 };
    let mob_id = if has_timecode { tape_id } else { [0; 32] };
    let clip = source_clip(source.kind, length, mob_id, tape_slot_id, 0);
    slots.push(timeline_slot(file_slot, &slot_name(source.kind, file_slot), file_slot, rate, clip));
}

fn add_master_markers(doc: &Document, members: &[usize], w: &mut W<'_>, master_slots: &mut Vec<Obj>) {
    let first = &doc.sources[members[0]];
    let marker_rate =
        members.iter().map(|&i| &doc.sources[i]).find(|source| source.kind == CKind::Picture).map(source_rate).unwrap_or_else(|| source_rate(first));
    let markers: Vec<&CMarker> = members.iter().flat_map(|&i| doc.sources[i].markers.iter()).collect();
    if markers.is_empty() {
        return;
    }
    let mut unique: Vec<CMarker> = markers.into_iter().cloned().collect();
    unique.sort_by(|a, b| (a.start, &a.name).cmp(&(b.start, &b.name)));
    unique.dedup();
    master_slots.push(event_slot(w, members.len() as u32 + 1, marker_rate, &unique));
}

fn add_file_mobs(doc: &Document, files: Vec<FileMob>, w: &mut W<'_>, output: &mut MediaOutput) {
    for (_, file_id, slots, source_index) in files {
        let source = &doc.sources[source_index];
        let desc = descriptor(w, source);
        output.mobs.push(
            Obj::new(cls::SOURCE_MOB)
                .data_prop(pid::MOB_ID, file_id.to_vec())
                .data_prop(pid::MOB_NAME, utf16z(&source.name))
                .data_prop(pid::MOB_LAST_MODIFIED, timestamp())
                .data_prop(pid::MOB_CREATION_TIME, timestamp())
                .with(pid::SLOTS, Value::StrongVec(slots))
                .with(pid::ESSENCE_DESCRIPTION, Value::Strong(Box::new(desc))),
        );
        if let Some(data) = &source.embedded {
            output
                .essence
                .push(Obj::new(cls::ESSENCE_DATA).data_prop(pid::ESSENCE_MOB_ID, file_id.to_vec()).with(pid::ESSENCE_STREAM, Value::Stream(data.clone())));
        }
    }
}

fn add_tape_mob(first: &Source, tape_id: [u8; 32], timecode: Option<(i64, FrameRate)>, tape_slots: Vec<Obj>, output: &mut MediaOutput) {
    let Some(_) = timecode else { return };
    let reel = first.path.as_deref().map(crate::common::file_stem).unwrap_or(&first.name).to_string();
    output.mobs.push(
        Obj::new(cls::SOURCE_MOB)
            .data_prop(pid::MOB_ID, tape_id.to_vec())
            .data_prop(pid::MOB_NAME, utf16z(&reel))
            .data_prop(pid::MOB_LAST_MODIFIED, timestamp())
            .data_prop(pid::MOB_CREATION_TIME, timestamp())
            .with(pid::SLOTS, Value::StrongVec(tape_slots))
            .with(pid::ESSENCE_DESCRIPTION, Value::Strong(Box::new(Obj::new(cls::TAPE_DESCRIPTOR)))),
    );
}

/// Serialise a composition document.
pub(crate) fn write(doc: &Document, version: filmcraft_cfb::Version, report: &mut Report) -> Result<Vec<u8>> {
    let comp = doc.compositions.first().ok_or(Error::Empty)?;
    let mut w = W { report, inexact: false };
    let mut ids = IdGen::for_document(doc, comp);
    let MediaOutput { mut mobs, essence, master_ref } = write_media(doc, &mut w, &mut ids);
    let mut used_ops: Vec<Auid> = Vec::new();
    let mut uses_varying = false;

    // ---- nested compositions: a clip refers to one as it does to a master mob, through the
    // composition's first track of the clip's kind (a broken-out channel: that channel's track)
    let mut refs = Refs { mobs: master_ref, rates: HashMap::new() };
    let nested_ids: Vec<[u8; 32]> = doc.nested.iter().map(|_| ids.next()).collect();
    for (si, s) in doc.sources.iter().enumerate() {
        let Some((comp, id)) = s.nested.and_then(|n| Some((doc.nested.get(n)?, nested_ids.get(n)?))) else { continue };
        let of_kind: Vec<usize> = comp.tracks.iter().enumerate().filter(|(_, t)| t.kind == s.kind).map(|(i, _)| i).collect();
        let Some(&track) = s.channel.and_then(|c| of_kind.get(c as usize)).or(of_kind.first()) else { continue };
        let rate = match s.kind {
            CKind::Picture => (comp.rate.num, comp.rate.den),
            CKind::Sound => (comp.sample_rate.max(1) as i64, 1),
        };
        refs.mobs.insert(si, (*id, track as u32 + 2));
        refs.rates.insert(si, rate);
    }

    // ---- the compositions: the exported sequence first
    let comp_id = ids.next();
    let composition_mobs = {
        let mut composition_writer = CompositionWriter { w: &mut w, doc, refs: &refs, used_ops: &mut used_ops, uses_varying: &mut uses_varying };
        let mut output = vec![composition_writer.write(comp, comp_id, true)];
        output.extend(doc.nested.iter().zip(&nested_ids).map(|(nested, id)| composition_writer.write(nested, *id, false)));
        output
    };
    mobs.splice(0..0, composition_mobs);
    if w.inexact {
        w.report.info("some times are not on whole edit units and were rounded");
    }

    // ---- dictionary
    let mut ddefs = Vec::new();
    for (id, name) in [(ddef::PICTURE, "Picture"), (ddef::SOUND, "Sound"), (ddef::TIMECODE, "Timecode"), (ddef::DESCRIPTIVE_METADATA, "DescriptiveMetadata")] {
        ddefs.push(definition(cls::DATA_DEFINITION, id, name));
    }
    let mut opdefs = Vec::new();
    for op in &used_ops {
        let (name, dd, inputs) = match *op {
            x if x == def::VIDEO_DISSOLVE => ("Video Dissolve", ddef::PICTURE, 2),
            x if x == def::VIDEO_FADE_TO_BLACK => ("Video Fade To Black", ddef::PICTURE, 1),
            x if x == def::SMPTE_VIDEO_WIPE => ("SMPTE Video Wipe", ddef::PICTURE, 2),
            x if x == def::MONO_AUDIO_DISSOLVE => ("Mono Audio Dissolve", ddef::SOUND, 2),
            _ => ("Mono Audio Gain", ddef::SOUND, 1i32),
        };
        let params: Vec<Vec<u8>> = if *op == def::MONO_AUDIO_GAIN {
            vec![def::PARAM_AMPLITUDE.to_vec()]
        } else if *op == def::VIDEO_DISSOLVE {
            vec![def::PARAM_LEVEL.to_vec()]
        } else {
            Vec::new()
        };
        let mut o = definition(cls::OPERATION_DEFINITION, *op, name)
            .with(pid::OPDEF_DATA_DEFINITION, weak(path::DATA_DEFS, dd))
            .data_prop(pid::OPDEF_IS_TIME_WARP, vec![0])
            .data_prop(pid::OPDEF_NUMBER_INPUTS, inputs.to_le_bytes().to_vec());
        if !params.is_empty() {
            o.set(pid::OPDEF_PARAMETERS_DEFINED, Value::WeakVec(path::PARAMETER_DEFS.to_vec(), pid::IDENTIFICATION, params));
        }
        opdefs.push(o);
    }
    let pdefs = vec![
        definition(cls::PARAMETER_DEFINITION, def::PARAM_LEVEL, "Level").data_prop(pid::PARAMDEF_DISPLAY_UNITS, utf16z("")),
        definition(cls::PARAMETER_DEFINITION, def::PARAM_AMPLITUDE, "Amplitude").data_prop(pid::PARAMDEF_DISPLAY_UNITS, utf16z("")),
    ];
    let idefs = if uses_varying || !used_ops.is_empty() {
        vec![
            definition(cls::INTERPOLATION_DEFINITION, def::INTERP_LINEAR, "Linear"),
            definition(cls::INTERPOLATION_DEFINITION, def::INTERP_CONSTANT, "Constant"),
        ]
    } else {
        Vec::new()
    };
    let cdefs = vec![
        definition(cls::CONTAINER_DEFINITION, def::CONTAINER_EXTERNAL, "External Container"),
        definition(cls::CONTAINER_DEFINITION, def::CONTAINER_AAF, "AAF Container"),
    ];
    let mut dict = Obj::new(cls::DICTIONARY)
        .with(pid::DATA_DEFINITIONS, Value::StrongSet(ddefs, pid::IDENTIFICATION))
        .with(pid::PARAMETER_DEFINITIONS, Value::StrongSet(pdefs, pid::IDENTIFICATION))
        .with(pid::CONTAINER_DEFINITIONS, Value::StrongSet(cdefs, pid::IDENTIFICATION));
    if !opdefs.is_empty() {
        dict.set(pid::OPERATION_DEFINITIONS, Value::StrongSet(opdefs, pid::IDENTIFICATION));
    }
    if !idefs.is_empty() {
        dict.set(pid::INTERPOLATION_DEFINITIONS, Value::StrongSet(idefs, pid::IDENTIFICATION));
    }

    let mut content = Obj::new(cls::CONTENT_STORAGE).with(pid::MOBS, Value::StrongSet(mobs, pid::MOB_ID));
    if !essence.is_empty() {
        content.set(pid::ESSENCE_DATA, Value::StrongSet(essence, pid::ESSENCE_MOB_ID));
    }
    let ident = Obj::new(cls::IDENTIFICATION)
        .data_prop(pid::COMPANY_NAME, utf16z("FilmCraft"))
        .data_prop(pid::PRODUCT_NAME, utf16z("FilmCraft"))
        .data_prop(pid::PRODUCT_VERSION_STRING, utf16z(env!("CARGO_PKG_VERSION")))
        .data_prop(pid::PRODUCT_ID, super::ids::guid(0x46494C4D, 0x4352, 0x4146, *b"T-AAF-EP").to_vec())
        .data_prop(pid::DATE, timestamp())
        .data_prop(pid::PLATFORM, utf16z("FilmCraft"))
        .data_prop(pid::GENERATION_AUID, mob_id(ids.seed, 0)[16..].to_vec());
    let header = Obj::new(cls::HEADER)
        .data_prop(pid::BYTE_ORDER, 0x4949i16.to_le_bytes().to_vec())
        .data_prop(pid::LAST_MODIFIED, timestamp())
        .data_prop(pid::VERSION, vec![1, 1])
        .data_prop(pid::OBJECT_MODEL_VERSION, 1u32.to_le_bytes().to_vec())
        .data_prop(pid::OPERATIONAL_PATTERN, def::OP_EDIT_PROTOCOL.to_vec())
        .with(pid::CONTENT, Value::Strong(Box::new(content)))
        .with(pid::DICTIONARY, Value::Strong(Box::new(dict)))
        .with(pid::IDENTIFICATION_LIST, Value::StrongVec(vec![ident]));
    // The meta-dictionary holds only extensions to the baseline object model, and everything written
    // here is baseline, so its class and type definition sets are empty; readers that load them
    // (pyaaf2) still need the sets to be there (#323).
    let meta = Obj::new(META_DICTIONARY)
        .with(pid::META_CLASS_DEFINITIONS, Value::StrongSet(Vec::new(), pid::META_IDENTIFICATION))
        .with(pid::META_TYPE_DEFINITIONS, Value::StrongSet(Vec::new(), pid::META_IDENTIFICATION));
    let root = Obj::new(ROOT).with(pid::ROOT_META_DICTIONARY, Value::Strong(Box::new(meta))).with(pid::ROOT_HEADER, Value::Strong(Box::new(header)));
    store::write(&root, version).map_err(Error::Other)
}

/// Where clips find their sources: source index → (mob id, slot id), and for nested compositions
/// the edit rate of that slot.
#[derive(Default)]
struct Refs {
    mobs: HashMap<usize, ([u8; 32], u32)>,
    rates: HashMap<usize, (i64, i64)>,
}

/// The composition mob of `comp`. Slot 1 is its timecode; its tracks follow from slot 2 in order.
struct CompositionWriter<'a, 'b, 'c> {
    w: &'a mut W<'c>,
    doc: &'b Document,
    refs: &'b Refs,
    used_ops: &'a mut Vec<Auid>,
    uses_varying: &'a mut bool,
}

impl CompositionWriter<'_, '_, '_> {
    fn write(&mut self, comp: &Composition, id: [u8; 32], top: bool) -> Obj {
        let w = &mut *self.w;
        let doc = self.doc;
        let refs = self.refs;
        let used_ops = &mut *self.used_ops;
        let uses_varying = &mut *self.uses_varying;
        let crate_rate = (comp.rate.num, comp.rate.den);
        let mut slots = Vec::new();
        let total = comp
            .tracks
            .iter()
            .map(|t| {
                let mut c = Tick::ZERO;
                for i in &t.items {
                    match i {
                        CItem::Transition(x) => c -= x.len,
                        other => c += other.len(),
                    }
                }
                c
            })
            .max()
            .unwrap_or(Tick::ZERO);
        let total_units = w.u(total, crate_rate);
        slots.push(timeline_slot(
            1,
            "TC1",
            0,
            crate_rate,
            Obj::new(cls::TIMECODE)
                .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, ddef::TIMECODE))
                .data_prop(pid::LENGTH, total_units.to_le_bytes().to_vec())
                .data_prop(pid::TC_START, comp.start_tc.to_le_bytes().to_vec())
                .data_prop(pid::TC_FPS, (comp.rate.timecode_base().clamp(1, u16::MAX as i64) as u16).to_le_bytes().to_vec())
                .data_prop(pid::TC_DROP, vec![comp.drop as u8]),
        ));
        let mut slot_id = 2u32;
        for t in &comp.tracks {
            let rate = match t.kind {
                CKind::Picture => crate_rate,
                CKind::Sound => (comp.sample_rate.max(1) as i64, 1),
            };
            let dd = data_def(t.kind);
            let mut comps = Vec::new();
            let mut cursor = Tick::ZERO;
            for it in &t.items {
                match it {
                    CItem::Filler(l) => {
                        let n = w.u(cursor + *l, rate) - w.u(cursor, rate);
                        cursor += *l;
                        comps
                            .push(Obj::new(cls::FILLER).with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, dd)).data_prop(pid::LENGTH, n.to_le_bytes().to_vec()));
                    }
                    CItem::Clip(c) => {
                        let n = w.u(cursor + c.len, rate) - w.u(cursor, rate);
                        let s = &doc.sources[c.source];
                        let (mid, mslot) = refs.mobs.get(&c.source).copied().unwrap_or(([0; 32], 0));
                        // (a nested composition counts the start in its own slot's edit units)
                        let start = w.u(c.start, refs.rates.get(&c.source).copied().unwrap_or(rate));
                        let mut sc = source_clip(t.kind, n, mid, mslot, start);
                        if !c.name.is_empty() && c.name != s.name {
                            sc.set(pid::COMPONENT_USER_COMMENTS, Value::StrongVec(vec![tagged("Clip Name", &c.name)]));
                        }
                        let seg = match (&c.gain, t.kind) {
                            (Some(g), CKind::Sound) => {
                                if !used_ops.contains(&def::MONO_AUDIO_GAIN) {
                                    used_ops.push(def::MONO_AUDIO_GAIN);
                                }
                                let param = match g {
                                    Gain::Constant(a) => Obj::new(cls::CONSTANT_VALUE)
                                        .data_prop(pid::PARAMETER_DEFINITION, def::PARAM_AMPLITUDE.to_vec())
                                        .data_prop(pid::CONSTANT_VALUE, indirect(def::TYPE_RATIONAL, &gain_rational(*a))),
                                    Gain::Varying { linear, points } => {
                                        *uses_varying = true;
                                        let pts = points
                                            .iter()
                                            .map(|(off, a)| {
                                                let o = w.u(cursor + *off, rate) - w.u(cursor, rate);
                                                Obj::new(cls::CONTROL_POINT)
                                                    .data_prop(pid::CP_TIME, rational(o, n.max(1)))
                                                    .data_prop(pid::CP_VALUE, indirect(def::TYPE_RATIONAL, &gain_rational(*a)))
                                                    .data_prop(pid::CP_EDIT_HINT, vec![0])
                                            })
                                            .collect();
                                        Obj::new(cls::VARYING_VALUE)
                                            .data_prop(pid::PARAMETER_DEFINITION, def::PARAM_AMPLITUDE.to_vec())
                                            .with(
                                                pid::INTERPOLATION,
                                                weak(path::INTERPOLATION_DEFS, if *linear { def::INTERP_LINEAR } else { def::INTERP_CONSTANT }),
                                            )
                                            .with(pid::POINT_LIST, Value::StrongVec(pts))
                                    }
                                };
                                Obj::new(cls::OPERATION_GROUP)
                                    .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, dd))
                                    .data_prop(pid::LENGTH, n.to_le_bytes().to_vec())
                                    .with(pid::OPERATION, weak(path::OPERATION_DEFS, def::MONO_AUDIO_GAIN))
                                    .with(pid::INPUT_SEGMENTS, Value::StrongVec(vec![sc]))
                                    .with(pid::PARAMETERS, Value::StrongVec(vec![param]))
                            }
                            _ => sc,
                        };
                        comps.push(seg);
                        cursor += c.len;
                    }
                    CItem::Transition(x) => {
                        let ts = cursor - x.len;
                        let n = w.u(cursor, rate) - w.u(ts, rate);
                        let cut = w.u(ts + x.cut, rate) - w.u(ts, rate);
                        cursor = ts;
                        let op = transition_op(t.kind, &x.effect);
                        if !used_ops.contains(&op) {
                            used_ops.push(op);
                        }
                        let mut og = Obj::new(cls::OPERATION_GROUP)
                            .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, dd))
                            .data_prop(pid::LENGTH, n.to_le_bytes().to_vec())
                            .with(pid::OPERATION, weak(path::OPERATION_DEFS, op))
                            .with(pid::COMPONENT_USER_COMMENTS, Value::StrongVec(vec![tagged("FilmCraft Effect", &x.effect)]));
                        if op == def::VIDEO_DISSOLVE {
                            *uses_varying = true;
                            let pts = [(0, 0), (1, 1)]
                                .iter()
                                .map(|&(t, v)| {
                                    Obj::new(cls::CONTROL_POINT)
                                        .data_prop(pid::CP_TIME, rational(t, 1))
                                        .data_prop(pid::CP_VALUE, indirect(def::TYPE_RATIONAL, &rational(v, 1)))
                                        .data_prop(pid::CP_EDIT_HINT, vec![0])
                                })
                                .collect();
                            og.set(
                                pid::PARAMETERS,
                                Value::StrongVec(vec![
                                    Obj::new(cls::VARYING_VALUE)
                                        .data_prop(pid::PARAMETER_DEFINITION, def::PARAM_LEVEL.to_vec())
                                        .with(pid::INTERPOLATION, weak(path::INTERPOLATION_DEFS, def::INTERP_LINEAR))
                                        .with(pid::POINT_LIST, Value::StrongVec(pts)),
                                ]),
                            );
                        }
                        comps.push(
                            Obj::new(cls::TRANSITION)
                                .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, dd))
                                .data_prop(pid::LENGTH, n.to_le_bytes().to_vec())
                                .data_prop(pid::CUT_POINT, cut.to_le_bytes().to_vec())
                                .with(pid::OPERATION_GROUP, Value::Strong(Box::new(og))),
                        );
                    }
                }
            }
            let len: i64 = comps
                .iter()
                .map(|c| {
                    let l = c.i64(pid::LENGTH).unwrap_or(0);
                    if c.class == cls::TRANSITION { -l } else { l }
                })
                .sum();
            let seq = Obj::new(cls::SEQUENCE)
                .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, dd))
                .data_prop(pid::LENGTH, len.to_le_bytes().to_vec())
                .with(pid::COMPONENTS, Value::StrongVec(comps));
            slots.push(timeline_slot(slot_id, &t.name, t.number, rate, seq));
            slot_id += 1;
        }
        if !comp.markers.is_empty() {
            slots.push(event_slot(w, slot_id, crate_rate, &comp.markers));
        }
        let mut mob = Obj::new(cls::COMPOSITION_MOB)
            .data_prop(pid::MOB_ID, id.to_vec())
            .data_prop(pid::MOB_NAME, utf16z(&comp.name))
            .data_prop(pid::MOB_LAST_MODIFIED, timestamp())
            .data_prop(pid::MOB_CREATION_TIME, timestamp());
        // only the exported sequence is a top-level composition; a nested sequence is found through
        // the clips that use it
        if top {
            mob.set(pid::USAGE_CODE, Value::Data(def::USAGE_TOP_LEVEL.to_vec()));
        }
        mob.with(pid::SLOTS, Value::StrongVec(slots)).with(
            pid::MOB_USER_COMMENTS,
            Value::StrongVec(vec![
                tagged("FilmCraft Frame Size", &format!("{}x{}", comp.width, comp.height)),
                tagged("FilmCraft Audio Sample Rate", &comp.sample_rate.to_string()),
            ]),
        )
    }
}

fn slot_name(kind: CKind, n: u32) -> String {
    match kind {
        CKind::Picture => format!("V{n}"),
        CKind::Sound => format!("A{n}"),
    }
}

fn timeline_slot(id: u32, name: &str, number: u32, rate: (i64, i64), segment: Obj) -> Obj {
    let mut o = Obj::new(cls::TIMELINE_MOB_SLOT)
        .data_prop(pid::SLOT_ID, id.to_le_bytes().to_vec())
        .data_prop(pid::SLOT_NAME, utf16z(name))
        .data_prop(pid::EDIT_RATE, rational(rate.0, rate.1))
        .data_prop(pid::ORIGIN, 0i64.to_le_bytes().to_vec())
        .with(pid::SEGMENT, Value::Strong(Box::new(segment)));
    if number > 0 {
        o.set(pid::PHYSICAL_TRACK_NUMBER, Value::Data(number.to_le_bytes().to_vec()));
    }
    o
}

fn source_clip(kind: CKind, len: i64, source: [u8; 32], slot: u32, start: i64) -> Obj {
    Obj::new(cls::SOURCE_CLIP)
        .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, data_def(kind)))
        .data_prop(pid::LENGTH, len.to_le_bytes().to_vec())
        .data_prop(pid::SOURCE_ID, source.to_vec())
        .data_prop(pid::SOURCE_MOB_SLOT_ID, slot.to_le_bytes().to_vec())
        .data_prop(pid::START_TIME, start.to_le_bytes().to_vec())
}

fn event_slot(w: &mut W, id: u32, rate: (i64, i64), markers: &[CMarker]) -> Obj {
    let comps = markers
        .iter()
        .map(|m| {
            let pos = w.u(m.start, rate);
            let len = w.u(m.start + m.duration, rate) - pos;
            let mut tags = vec![tagged("Name", &m.name), tagged("Comment", &m.comment)];
            if let Some(c) = &m.color {
                tags.push(tagged("Color", c));
            }
            Obj::new(cls::COMMENT_MARKER)
                .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, ddef::DESCRIPTIVE_METADATA))
                .data_prop(pid::LENGTH, len.to_le_bytes().to_vec())
                .data_prop(pid::POSITION, pos.to_le_bytes().to_vec())
                .data_prop(pid::COMMENT, utf16z(if m.comment.is_empty() { &m.name } else { &m.comment }))
                .with(pid::COMPONENT_USER_COMMENTS, Value::StrongVec(tags))
        })
        .collect();
    Obj::new(cls::EVENT_MOB_SLOT)
        .data_prop(pid::SLOT_ID, id.to_le_bytes().to_vec())
        .data_prop(pid::SLOT_NAME, utf16z("Markers"))
        .data_prop(pid::EVENT_EDIT_RATE, rational(rate.0, rate.1))
        .with(
            pid::SEGMENT,
            Value::Strong(Box::new(
                Obj::new(cls::SEQUENCE)
                    .with(pid::DATA_DEFINITION, weak(path::DATA_DEFS, ddef::DESCRIPTIVE_METADATA))
                    .with(pid::COMPONENTS, Value::StrongVec(comps)),
            )),
        )
}

fn transition_op(kind: CKind, effect: &str) -> Auid {
    match kind {
        CKind::Sound => def::MONO_AUDIO_DISSOLVE,
        CKind::Picture => match effect {
            "dip_to_black" => def::VIDEO_FADE_TO_BLACK,
            e if e.contains("wipe") => def::SMPTE_VIDEO_WIPE,
            _ => def::VIDEO_DISSOLVE,
        },
    }
}

fn descriptor(w: &mut W, s: &Source) -> Obj {
    let rate = source_rate(s);
    let len = w.u(s.length, rate);
    let container = if s.embedded.is_some() { def::CONTAINER_AAF } else { def::CONTAINER_EXTERNAL };
    let mut d = match s.kind {
        CKind::Picture => Obj::new(cls::CDCI_DESCRIPTOR)
            .data_prop(pid::STORED_WIDTH, s.width.to_le_bytes().to_vec())
            .data_prop(pid::STORED_HEIGHT, s.height.to_le_bytes().to_vec())
            .data_prop(pid::FRAME_LAYOUT, vec![0])
            .data_prop(pid::IMAGE_ASPECT_RATIO, rational(s.width.max(1) as i64, s.height.max(1) as i64))
            .data_prop(pid::VIDEO_LINE_MAP, {
                let mut v = 2u32.to_le_bytes().to_vec();
                v.extend_from_slice(&4u32.to_le_bytes());
                v.extend_from_slice(&0i32.to_le_bytes());
                v.extend_from_slice(&0i32.to_le_bytes());
                v
            })
            .data_prop(pid::COMPONENT_WIDTH, 8u32.to_le_bytes().to_vec())
            .data_prop(pid::HORIZONTAL_SUBSAMPLING, 2u32.to_le_bytes().to_vec()),
        CKind::Sound => {
            let block = s.file_channels.max(1) * (s.bits as u32).div_ceil(8);
            Obj::new(cls::PCM_DESCRIPTOR)
                .data_prop(pid::AUDIO_SAMPLING_RATE, rational(s.sample_rate as i64, 1))
                .data_prop(pid::LOCKED, vec![1])
                .data_prop(pid::CHANNELS, s.file_channels.max(1).to_le_bytes().to_vec())
                .data_prop(pid::QUANTIZATION_BITS, (s.bits as u32).to_le_bytes().to_vec())
                .data_prop(pid::BLOCK_ALIGN, (block as u16).to_le_bytes().to_vec())
                .data_prop(pid::AVERAGE_BPS, (block * s.sample_rate).to_le_bytes().to_vec())
        }
    };
    d.set(pid::SAMPLE_RATE, Value::Data(rational(rate.0, rate.1)));
    d.set(pid::FILE_LENGTH, Value::Data(len.to_le_bytes().to_vec()));
    d.set(pid::CONTAINER_FORMAT, weak(path::CONTAINER_DEFS, container));
    if let Some(p) = &s.path {
        let url = if crate::common::is_absolute(p) { crate::common::path_to_file_url(p, false) } else { p.clone() };
        d.set(pid::LOCATOR, Value::StrongVec(vec![Obj::new(cls::NETWORK_LOCATOR).data_prop(pid::URL_STRING, utf16z(&url))]));
    }
    d
}
