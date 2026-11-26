use super::*;

fn number(out: &mut Vec<u8>, n: u64, size: usize, order: ByteOrder) {
    match order {
        ByteOrder::Little => out.extend_from_slice(&n.to_le_bytes()[..size]),
        ByteOrder::Big => out.extend_from_slice(&n.to_be_bytes()[8 - size..]),
    }
}
fn tag(out: &mut Vec<u8>, mut class: [u8; 4], order: ByteOrder) {
    if order == ByteOrder::Little {
        class.reverse();
    }
    out.extend_from_slice(&class);
}
fn text(out: &mut Vec<u8>, value: &[u8], order: ByteOrder) {
    number(out, value.len() as u64, 2, order);
    out.extend_from_slice(value);
}
fn fixture(order: ByteOrder) -> Vec<u8> {
    let mut b = Vec::new();
    text(&mut b, b"Domain", order);
    tag(&mut b, *b"OBJD", order);
    text(&mut b, b"AObjDoc", order);
    b.push(4);
    text(&mut b, b"2026/10/11 00:00:00", order);
    number(&mut b, 4, 4, order);
    number(&mut b, 4, 4, order);
    b.extend_from_slice(&[0x55; 20]); // deliberately opaque metadata
    text(&mut b, "Synthetic \u{00e9}".as_bytes(), order);
    b.extend_from_slice(&[0xab; 16]);
    let mut root = vec![2, 0x0e];
    number(&mut root, 0, 4, order);
    number(&mut root, 0x0123456789abcdef, 8, order);
    number(&mut root, 2, 2, order);
    for (id, placed) in [(1, 1), (2, 0)] {
        number(&mut root, id, 4, order);
        root.extend_from_slice(&[0x88; 8]);
        root.push(placed);
    }
    root.extend_from_slice(&[0xcd; 103]);
    for (class, payload) in [(*b"CMPO", vec![1]), (*b"CMPO", vec![2]), (*b"Zzzz", vec![0xff, 0, 1, 2]), (*b"ABIN", root)] {
        tag(&mut b, class, order);
        number(&mut b, payload.len() as u64, 4, order);
        b.extend_from_slice(&payload);
    }
    b
}

#[test]
fn endian_membership_and_opaque_fields_are_preserved() {
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let bytes = fixture(order);
        let bin = index(&bytes).unwrap();
        assert_eq!(bin.byte_order, order);
        assert_eq!(bin.creator_raw, "Synthetic \u{00e9}".as_bytes());
        assert_eq!(bin.save_fields_raw, [0x55; 20]);
        assert_eq!(bin.reserved_raw, [0xab; 16]);
        assert!(bin.object(0).unwrap().is_none());
        assert!(bin.object(5).is_err());
        let unknown = bin.object(3).unwrap().unwrap();
        assert_eq!(unknown.class, *b"Zzzz");
        assert_eq!(unknown.payload, [0xff, 0, 1, 2]);
        assert_eq!(unknown.payload.as_ptr(), bytes[unknown.offset + 8..].as_ptr());
        let members = bin.membership().unwrap();
        assert_eq!(members.uid, 0x0123456789abcdef);
        assert_eq!(members.view_index, 0);
        assert_eq!(members.members.len(), 2);
        assert!(members.members[0].user_placed);
        assert!(!members.members[1].user_placed);
        assert_eq!(members.members[1].object_index, 2);
        assert_eq!(members.members[0].placement_raw, [0x88; 8]);
        assert_eq!(members.tail_raw, [0xcd; 103]);
    }
}

#[test]
fn truncation_counts_lengths_roots_and_trailing_data_fail_without_panics() {
    let bytes = fixture(ByteOrder::Little);
    for end in 0..bytes.len() {
        assert!(std::panic::catch_unwind(|| index(&bytes[..end])).unwrap().is_err(), "accepted truncation {end}");
    }
    let first_length = index(&bytes).unwrap().objects[0].offset + 4;
    for (at, value) in [(43, u32::MAX), (43, 0), (47, 0), (47, 5), (first_length, u32::MAX)] {
        let mut mutated = bytes.clone();
        mutated[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(std::panic::catch_unwind(|| index(&mutated)).unwrap().is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(index(&trailing).unwrap_err().to_string().contains("trailing"));
}

#[test]
fn unsupported_root_versions_flags_and_dangling_members_are_explicit() {
    let bytes = fixture(ByteOrder::Little);
    let start = index(&bytes).unwrap().object(4).unwrap().unwrap().offset + 8;
    for (offset, value) in [(1, 0x0f), (14, 255), (16, 0), (16, 5), (28, 2)] {
        let mut mutated = bytes.clone();
        mutated[start + offset] = value;
        assert!(index(&mutated).unwrap().membership().is_err(), "offset {offset}");
    }
    let mut unsupported = bytes.clone();
    unsupported[start - 8..start - 4].copy_from_slice(b"FNIB");
    assert!(index(&unsupported).unwrap().membership().unwrap_err().to_string().contains("root class"));
}

#[test]
fn bit_mutations_never_escape_object_bounds() {
    let original = fixture(ByteOrder::Little);
    for offset in 0..original.len() {
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[offset] ^= 1 << bit;
            std::panic::catch_unwind(|| {
                if let Ok(bin) = index(&bytes) {
                    for object in &bin.objects {
                        assert_eq!(bytes.get(object.offset + 8..object.offset + 8 + object.payload.len()), Some(object.payload));
                    }
                    let _ = bin.membership();
                }
            })
            .unwrap();
        }
    }
}

proptest::proptest! {
    #[test]
    fn arbitrary_bytes_do_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        if let Ok(bin) = index(&bytes) {
            let _ = bin.membership();
        }
    }
}

#[test]
#[ignore = "set FILMCRAFT_AVB_CORPUS_MANIFEST to private oracle counts/paths; no fixture contents enter Git"]
fn local_corpus_matches_oracle_index_and_membership() {
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(std::env::var("FILMCRAFT_AVB_CORPUS_MANIFEST").unwrap()).unwrap()).unwrap();
    let mut objects = 0;
    for file in manifest.as_array().unwrap() {
        let bytes = std::fs::read(file["path"].as_str().unwrap()).unwrap();
        let bin = index(&bytes).unwrap();
        assert_eq!(bin.objects.len(), file["objects"].as_u64().unwrap() as usize);
        let members = bin.membership().unwrap();
        assert_eq!(members.uid, file["uid"].as_u64().unwrap());
        assert_eq!(members.members.len(), file["members"].as_u64().unwrap() as usize);
        assert_eq!(members.members.iter().filter(|m| m.user_placed).count(), file["placed"].as_u64().unwrap() as usize);
        assert_eq!(members.members.iter().map(|m| m.object_index).collect::<Vec<_>>(), serde_json::from_value::<Vec<u32>>(file["references"].clone()).unwrap());
        objects += bin.objects.len();
    }
    eprintln!("{} bins / {objects} objects: native index, bin UID and membership match oracle", manifest.as_array().unwrap().len());
}

#[test]
fn large_object_indexes_are_bounded_and_cancellable() {
    let source = fixture(ByteOrder::Little);
    let header = index(&source).unwrap().objects[0].offset;
    let make = |count: usize| {
        let mut bytes = source[..header].to_vec();
        bytes[43..47].copy_from_slice(&(count as u32).to_le_bytes());
        bytes[47..51].copy_from_slice(&1u32.to_le_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(b"zzzz\0\0\0\0");
        }
        bytes
    };
    // An actual object table above the former million-object cap, not just a forged count.
    let bytes = make(1_000_001);
    let bin = index(&bytes).unwrap();
    assert_eq!(bin.objects.len(), 1_000_001);
    assert!(bin.object(1_000_002).is_err());
    drop(bin);
    let calls = std::cell::Cell::new(0);
    let result = index_with_checkpoint(&bytes, &|| {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 { Err(bad("cancelled index")) } else { Ok(()) }
    });
    assert!(result.unwrap_err().to_string().contains("cancelled index"));
    assert_eq!(calls.get(), 3);
    let mut truncated = bytes.clone();
    truncated.pop();
    assert!(index(&truncated).is_err());
    assert!(index(&make(MAX_OBJECTS + 1)).unwrap_err().to_string().contains("excessive object count"));
}
