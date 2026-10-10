//! Hostile object trees: a crafted file must give an error, never a crash, a hang or an
//! allocation sized by a count in the file (AGENTS.md §0).

use filmcraft_cfb::{Version, Writer};

use super::store::{self, SF_WEAK_SET, SF_WEAK_VECTOR, Value, utf16z};

const PID: u16 = 0x0007;

/// A root object whose one property `PID` is a weak reference vector or set (stored form `sf`)
/// named `Keys`, with `index` as its `Keys index` stream.
fn weak_collection_file(sf: u16, index: Vec<u8>) -> Vec<u8> {
    let name = utf16z("Keys");
    // byte order 'L', format version, 1 entry { pid, stored form, value length }, the value
    let mut props = vec![0x4C, 0x20];
    for x in [1, PID, sf, name.len() as u16] {
        props.extend_from_slice(&x.to_le_bytes());
    }
    props.extend_from_slice(&name);
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "properties", props).unwrap();
    w.stream(Writer::ROOT, "Keys index", index).unwrap();
    w.finish()
}

/// A weak collection index: u32 count, u16 tag, u16 key pid, u8 key size, then the keys.
fn index(count: u32, size: u8, keys: &[u8]) -> Vec<u8> {
    let mut v = count.to_le_bytes().to_vec();
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&0x1B01u16.to_le_bytes());
    v.push(size);
    v.extend_from_slice(keys);
    v
}

fn read_keys(sf: u16, index: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    match store::read(&weak_collection_file(sf, index))?.get(PID) {
        Some(Value::WeakVec(_, _, keys)) => Ok(keys.clone()),
        other => panic!("not a weak collection: {other:?}"),
    }
}

/// With a key size of 0 every key fits, so the count alone drove the loop: up to 4.29 billion
/// empty keys (about 100 GB) from a 9-byte index. A million is enough to show it, cheaply.
#[test]
fn zero_size_weak_collection_keys_are_an_error() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        match read_keys(sf, index(1_000_000, 0, &[])) {
            Err(e) => assert!(e.contains("zero-size"), "{e}"),
            Ok(keys) => panic!("read {} empty keys instead of an error", keys.len()),
        }
    }
}

#[test]
fn weak_collection_count_beyond_the_index_is_an_error() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        let e = read_keys(sf, index(u32::MAX, 4, &[1, 2, 3, 4, 5, 6, 7, 8])).unwrap_err();
        assert!(e.contains("short weak collection index"), "{e}");
    }
}

#[test]
fn weak_collections_within_the_index_are_read() {
    for sf in [SF_WEAK_VECTOR, SF_WEAK_SET] {
        assert_eq!(read_keys(sf, index(0, 0, &[])).unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(read_keys(sf, index(2, 4, &[1, 2, 3, 4, 5, 6, 7, 8])).unwrap(), [vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }
}
