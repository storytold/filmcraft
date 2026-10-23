use super::*;

fn common(kind: u16) -> Vec<u8> {
    let mut p = vec![2, 3];
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&kind.to_le_bytes());
    p.extend_from_slice(&24u32.to_le_bytes());
    p.extend_from_slice(&[0; 2]);
    p.extend_from_slice(&[255; 4]);
    p.extend_from_slice(&[0; 12]);
    p
}
fn selector(refs: &[u32]) -> Vec<u8> {
    let mut p = common(2);
    p.extend_from_slice(&[2, 8, 0]);
    p.extend_from_slice(&48u32.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.extend_from_slice(&(refs.len() as u32).to_le_bytes());
    for r in refs {
        p.extend_from_slice(&4u16.to_le_bytes());
        p.extend_from_slice(&r.to_le_bytes());
    }
    // Deliberately non-default raw fields: preserve them without treating them as an angle.
    p.extend_from_slice(&[2, 1, 7, 9, 0, 3]);
    p
}
fn source(track: u16, start: i32) -> Vec<u8> {
    let mut p = common(2);
    p.extend_from_slice(&[2, 1]);
    p.extend_from_slice(&48u32.to_le_bytes());
    p.extend_from_slice(&[2, 3]);
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&track.to_le_bytes());
    p.extend_from_slice(&start.to_le_bytes());
    p.push(3);
    p
}
fn decode(refs: &[u32], budget: usize, checkpoint: &dyn Fn() -> Result<()>) -> Result<Value> {
    let a = selector(refs);
    let b = source(2, 13);
    let c = source(4, 29);
    let bin = Bin {
        byte_order: avid_bin::ByteOrder::Little,
        root_index: 1,
        saved_at_raw: &[],
        creator_raw: &[],
        save_fields_raw: &[],
        reserved_raw: &[],
        objects: vec![
            avid_bin::Object { index: 1, class: *b"SLCT", offset: 0, payload: &a },
            avid_bin::Object { index: 2, class: *b"SCLP", offset: 100, payload: &b },
            avid_bin::Object { index: 3, class: *b"SCLP", offset: 200, payload: &c },
        ],
    };
    Graph {
        evidence: Evidence { bin: &bin, remaining: 100, bytes_left: 4096, visiting: BTreeSet::new(), checkpoint },
        nodes: 0,
        bytes_left: budget,
        visiting: BTreeSet::new(),
    }
    .component(1, 0)
}

#[test]
fn selector_alternatives_preserve_order_tracks_offsets_and_raw_selection() {
    let v = decode(&[3, 2], 16384, &|| Ok(())).unwrap();
    assert_eq!(v["class"], "SLCT");
    assert_eq!(v["length"], 48, "alternative durations must not be added");
    assert_eq!(v["properties"]["selection_raw"], 7);
    assert_eq!(v["properties"]["trailing_raw"], 9);
    assert_eq!(v["alternatives"][0]["component"]["source_track"], 4);
    assert_eq!(v["alternatives"][0]["component"]["start"], 29);
    assert_eq!(v["alternatives"][1]["component"]["source_track"], 2);
    assert_eq!(v["alternatives"][1]["component"]["start"], 13);
    assert!(v.get("input_tracks").is_none());
    assert!(v.get("components").is_none());
    assert_eq!(v["unsupported"], "selector_selection");
    // Even one alternative must not enter the generic effect-bypass path.
    assert!(decode(&[2], 16384, &|| Ok(())).unwrap().get("input_tracks").is_none());
}

#[test]
fn selector_cycles_missing_references_budgets_and_cancellation_are_errors() {
    for refs in [&[1][..], &[0][..], &[4][..]] {
        assert!(std::panic::catch_unwind(|| decode(refs, 16384, &|| Ok(()))).unwrap().is_err());
    }
    assert!(decode(&[2, 3], 1024, &|| Ok(())).unwrap_err().to_string().contains("budget"));
    let calls = std::cell::Cell::new(0);
    let error = decode(&[2, 3], 16384, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 4 { Err(bad("cancelled selector test")) } else { Ok(()) }
    })
    .unwrap_err();
    assert!(error.to_string().contains("cancelled selector test"));
}
