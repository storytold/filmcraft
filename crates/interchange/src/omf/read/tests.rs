use super::*;
use crate::omf::bento::Writer;

const FILE: u32 = 1_000_000;
const TAPE: u32 = FILE + 1;
const SLOT: u32 = FILE + 2;
const TRACK: u32 = FILE + 3;
const CLIP: u32 = FILE + 4;
const DESC: u32 = FILE + 5;
const LOCATOR: u32 = FILE + 6;
const TAPE_SLOT: u32 = FILE + 7;
const TIMECODE: u32 = FILE + 8;
const LATER_LOCATOR: u32 = FILE + 9;
const LATER_TAPE_SLOT: u32 = FILE + 10;
const LATER_TIMECODE: u32 = FILE + 11;
const FILE_UID: Uid = [1; 12];
const TAPE_UID: Uid = [2; 12];

fn reference(w: &mut Writer, object: u32, property: &str, target: u32) {
    w.set(object, property, "omfi:ObjRef", &target.to_be_bytes());
}

fn references(w: &mut Writer, object: u32, property: &str, targets: &[u32]) {
    let mut value = u16::try_from(targets.len()).unwrap().to_be_bytes().to_vec();
    for target in targets {
        value.extend_from_slice(&target.to_be_bytes());
    }
    w.set(object, property, "omfi:ObjRefArray", &value);
}

fn source_fixture(locators: usize, slots: usize, tape_slots: usize) -> Vec<u8> {
    let mut w = Writer::new();
    for (object, class) in [(FILE, b"SMOB"), (CLIP, b"SCLP"), (LOCATOR, b"UNXL"), (TIMECODE, b"TCCP"), (LATER_LOCATOR, b"UNXL"), (LATER_TIMECODE, b"TCCP")] {
        w.set(object, "OMFI:OOBJ:ObjClass", "omfi:ClassID", class);
    }
    references(&mut w, FILE, "OMFI:MOBJ:Slots", &vec![SLOT; slots]);
    reference(&mut w, SLOT, "OMFI:MSLT:TrackDesc", TRACK);
    reference(&mut w, SLOT, "OMFI:MSLT:Segment", CLIP);
    w.set(TRACK, "OMFI:TRKD:TrackID", "omfi:UInt32", &1u32.to_be_bytes());
    w.set(SLOT, "OMFI:MSLT:EditRate", "omfi:Rational", &[25i32.to_be_bytes(), 1i32.to_be_bytes()].concat());
    reference(&mut w, FILE, "OMFI:SMOB:MediaDescription", DESC);
    w.set(DESC, "OMFI:MDFL:SampleRate", "omfi:Rational", &[48_000i32.to_be_bytes(), 1i32.to_be_bytes()].concat());
    w.set(DESC, "OMFI:WAVD:Summary", "omfi:DataValue", &crate::wav::wav_header(0, 2, 48_000, 16));
    let mut locator_list = vec![LOCATOR; locators];
    if locators > 1 {
        locator_list[locators - 1] = LATER_LOCATOR;
    }
    references(&mut w, DESC, "OMFI:MDES:Locator", &locator_list);
    w.set(LOCATOR, "OMFI:UNXL:PathName", "omfi:String", b"first.wav\0");
    w.set(LATER_LOCATOR, "OMFI:UNXL:PathName", "omfi:String", b"later.wav\0");
    w.set(CLIP, "OMFI:SCLP:SourceID", "omfi:UID", &TAPE_UID);
    w.set(CLIP, "OMFI:SCLP:StartTime", "omfi:Position32", &2i32.to_be_bytes());
    let mut tape_list = vec![TAPE_SLOT; tape_slots];
    if tape_slots > 1 {
        tape_list[tape_slots - 1] = LATER_TAPE_SLOT;
    }
    references(&mut w, TAPE, "OMFI:MOBJ:Slots", &tape_list);
    reference(&mut w, TAPE_SLOT, "OMFI:MSLT:Segment", TIMECODE);
    w.set(TIMECODE, "OMFI:TCCP:Start", "omfi:Position32", &90i32.to_be_bytes());
    w.set(TIMECODE, "OMFI:TCCP:FPS", "omfi:UInt16", &25u16.to_be_bytes());
    reference(&mut w, LATER_TAPE_SLOT, "OMFI:MSLT:Segment", LATER_TIMECODE);
    w.set(LATER_TIMECODE, "OMFI:TCCP:Start", "omfi:Position32", &900i32.to_be_bytes());
    w.finish()
}

fn with_reader(bytes: &[u8], budget: usize, test: impl FnOnce(&mut R<'_, '_>)) {
    let c = Container::open(bytes).unwrap();
    let mut report = Report::default();
    let mut r = R {
        c: &c,
        report: &mut report,
        be: true,
        mobs: HashMap::from([(FILE_UID, FILE), (TAPE_UID, TAPE)]),
        media: HashMap::new(),
        doc: Document::default(),
        sources: HashMap::new(),
        remaining_discovery_visits: MAX_DISCOVERY_VISITS,
        remaining_expansion_visits: budget,
    };
    test(&mut r);
}

#[test]
fn source_lookup_lists_exhaust_the_expansion_budget() {
    // Each case exhausts the budget in a different source metadata list. The
    // locator and tape lookup match the first entry but must charge the full list.
    for (locators, slots, tape_slots, budget, object) in [(8, 1, 1, 8, DESC), (0, 4, 0, 7, FILE), (0, 1, 8, 10, TAPE)] {
        let bytes = source_fixture(locators, slots, tape_slots);
        std::panic::catch_unwind(|| {
            with_reader(&bytes, budget, |r| {
                let result = r.resolve(FILE_UID, 1, Tick::ZERO, None, 0);
                assert!(matches!(result, Err(Error::Parse { format: "OMF", message })
                    if message.contains("expansion budget") && message.ends_with(&format!("object {object}"))));
                assert!(r.doc.sources.is_empty());
            });
        })
        .expect("source lookup panicked");
    }
}

#[test]
fn million_object_wide_graph_stops_at_discovery_budget() {
    const OBJECTS: u32 = 1_000_000;
    const ROOT: u32 = 2_000_000;
    const BRANCHES: u32 = 16;
    const FIRST_LEAF: u32 = 10_000;
    const LEAVES: u32 = OBJECTS - BRANCHES - 1;
    let mut w = Writer::new();
    let branches: Vec<u32> = (ROOT + 1..=ROOT + BRANCHES).collect();
    w.set(ROOT, "OMFI:OOBJ:ObjClass", "omfi:ClassID", b"SEQU");
    references(&mut w, ROOT, "OMFI:SEQU:Components", &branches);
    let leaves: Vec<u32> = (FIRST_LEAF..FIRST_LEAF + LEAVES).collect();
    for (&branch, children) in branches.iter().zip(leaves.chunks(u16::MAX as usize)) {
        w.set(branch, "OMFI:OOBJ:ObjClass", "omfi:ClassID", b"SEQU");
        references(&mut w, branch, "OMFI:SEQU:Components", children);
    }
    w.set(FIRST_LEAF, "OMFI:OOBJ:ObjClass", "omfi:ClassID", b"FILL");
    let bytes = w.finish();
    let mut c = Container::open(&bytes).unwrap();
    // Build the decoded graph with distinct object IDs. Cloning the common
    // filler properties avoids serialising a million equivalent Bento records;
    // the reader still receives all one million objects and their real edges.
    let filler = c.objects[&FIRST_LEAF].clone();
    for &leaf in &leaves[1..] {
        c.objects.insert(leaf, filler.clone());
    }
    assert!(c.objects.contains_key(&ROOT));
    assert!(branches.iter().chain(&leaves).all(|id| c.objects.contains_key(id)));
    assert_eq!(1 + branches.len() + leaves.len(), OBJECTS as usize);

    let mut report = Report::default();
    let mut r = R {
        c: &c,
        report: &mut report,
        be: true,
        mobs: HashMap::new(),
        media: HashMap::new(),
        doc: Document::default(),
        sources: HashMap::new(),
        remaining_discovery_visits: MAX_DISCOVERY_VISITS,
        remaining_expansion_visits: MAX_EXPANSION_VISITS,
    };
    let mut visited = HashSet::new();
    let mut referenced = HashSet::new();
    let started = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.collect_refs(ROOT, &mut referenced, &mut visited, 0)))
        .expect("wide graph traversal panicked");
    eprintln!("million-object discovery stopped after {} visits in {:?}", visited.len(), started.elapsed());
    // Root and first branch take two visits. Refuse the next leaf immediately
    // after the allowance is exhausted, without walking the remaining graph.
    let rejected_object = FIRST_LEAF + MAX_DISCOVERY_VISITS as u32 - 2;
    assert!(matches!(result, Err(Error::Parse { format: "OMF", message })
        if message.contains("reference discovery budget") && message.ends_with(&format!("object {rejected_object}"))));
    assert_eq!(visited.len(), MAX_DISCOVERY_VISITS);
    assert_eq!(r.remaining_discovery_visits, 0);
    assert_eq!(r.remaining_expansion_visits, MAX_EXPANSION_VISITS);
    assert!(!visited.contains(&rejected_object));
    assert!(!visited.contains(branches.last().unwrap()));
}

#[test]
fn source_lookup_lists_preserve_path_channels_and_timecode() {
    // Two file slots (read twice), two locators, one segment and two tape slots.
    with_reader(&source_fixture(2, 2, 2), 9, |r| {
        assert_eq!(r.resolve(FILE_UID, 1, Tick(1234), None, 0).unwrap(), Some((0, Tick(1234))));
        assert_eq!(r.remaining_expansion_visits, 0);
        let source = &r.doc.sources[0];
        assert_eq!(source.path.as_deref(), Some("first.wav"));
        assert_eq!((source.file_channels, source.channels, source.channel), (2, 1, Some(0)));
        assert_eq!((source.start_tc, source.tc_rate), (Some(92), FrameRate::FPS_25));
    });
}

#[test]
fn missing_source_metadata_remains_optional() {
    for missing_tape in [false, true] {
        with_reader(&source_fixture(0, 1, if missing_tape { 8 } else { 0 }), 3, |r| {
            if missing_tape {
                r.mobs.remove(&TAPE_UID);
            }
            assert!(r.resolve(FILE_UID, 1, Tick::ZERO, None, 0).unwrap().is_some());
            let source = &r.doc.sources[0];
            assert_eq!(source.path, None);
            assert_eq!(source.start_tc, None);
            assert_eq!((source.channels, source.channel), (2, None));
        });
    }
}

#[test]
fn reference_budget_counts_only_complete_declared_entries() {
    for (value, expected) in [
        (vec![], vec![]),
        (vec![255], vec![]),
        (vec![255, 255, 0, 0, 0], vec![]),
        (vec![255, 255, 0, 0, 0, 7, 0, 0, 0], vec![7]),
        (vec![0, 0, 0, 0, 0, 7], vec![]),
        (vec![0, 1, 0, 0, 0, 7, 0, 0, 0, 8], vec![7]),
        (vec![0, 2, 0, 0, 0, 7, 0, 0, 0, 8], vec![7, 8]),
    ] {
        let mut w = Writer::new();
        w.set(FILE, "OMFI:MOBJ:Slots", "omfi:ObjRefArray", &value);
        with_reader(&w.finish(), expected.len(), |r| {
            assert_eq!(r.refs(FILE, "OMFI:MOBJ:Slots"), expected);
            assert_eq!(r.budgeted_refs(FILE, "OMFI:MOBJ:Slots").unwrap(), expected);
            assert_eq!(r.remaining_expansion_visits, 0);
            assert!(r.budgeted_refs(FILE, "OMFI:MDES:Locator").unwrap().is_empty());
        });
    }
}
