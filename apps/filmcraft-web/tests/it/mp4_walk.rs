//! The walk over an MP4's top-level boxes that an import runs before `file.import`. It is pure, so
//! it runs natively even though the rest of the crate is wasm32-only.

#[path = "../../src/mp4_walk.rs"]
mod walk;

use walk::{Step, step};

/// The walk the import runs, over a file held in memory: the `moov` range it loads, if any.
fn find_moov(file: &[u8]) -> Option<(u64, u64)> {
    let size = file.len() as u64;
    let mut off = 0u64;
    for _ in 0..256 {
        if off.saturating_add(8) > size {
            return None;
        }
        let start = off as usize;
        let header = &file[start..file.len().min(start + 16)];
        match step(header, off, size) {
            Step::Moov { end } => return Some((off, end)),
            Step::Next(next) => off = next,
            Step::Stop => return None,
        }
    }
    None
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// A box with a 64-bit length.
fn large_box(kind: &[u8; 4], len: u64) -> Vec<u8> {
    let mut b = 1u32.to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(&len.to_be_bytes());
    b
}

#[test]
fn the_walk_finds_the_moov_after_other_boxes() {
    let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0");
    file.extend(mp4_box(b"free", &[0; 20]));
    let moov_at = file.len() as u64;
    file.extend(mp4_box(b"moov", &[0; 40]));
    assert_eq!(find_moov(&file), Some((moov_at, file.len() as u64)));
}

/// A malformed file whose box lengths run past the end of `u64` stops the walk, and the next file
/// dropped is walked as usual.
#[test]
fn a_box_length_near_u64_max_stops_the_walk_and_the_next_file_is_walked() {
    let mut malformed = mp4_box(b"ftyp", b"isom\0\0\0\0");
    let at = malformed.len() as u64;
    malformed.extend(large_box(b"mdat", u64::MAX - 4));
    malformed.extend([0; 32]);
    assert_eq!(step(&malformed[at as usize..], at, malformed.len() as u64), Step::Stop);
    assert_eq!(find_moov(&malformed), None);

    let mut moov_last = mp4_box(b"ftyp", b"isom\0\0\0\0");
    moov_last.extend(large_box(b"moov", u64::MAX));
    let size = moov_last.len() as u64;
    assert_eq!(find_moov(&moov_last), Some((16, size)), "the moov range ends at the end of the file");

    let mut valid = mp4_box(b"ftyp", b"isom\0\0\0\0");
    let moov_at = valid.len() as u64;
    valid.extend(mp4_box(b"moov", &[0; 40]));
    assert_eq!(find_moov(&valid), Some((moov_at, valid.len() as u64)));
}

#[test]
fn a_short_header_a_zero_length_box_and_a_box_past_the_end_stop_the_walk() {
    assert_eq!(step(&[0, 0, 0], 0, 100), Step::Stop);
    assert_eq!(step(&[0, 0, 0, 4, b'f', b'r', b'e', b'e'], 0, 100), Step::Stop, "a length below 8");
    // length 0: the box runs to the end of the file
    assert_eq!(step(&[0, 0, 0, 0, b'm', b'o', b'o', b'v'], 24, 100), Step::Moov { end: 100 });
    assert_eq!(step(&[0, 0, 0, 0, b'm', b'd', b'a', b't'], 24, 100), Step::Stop);
    assert_eq!(step(&mp4_box(b"free", &[0; 92]), 0, 100), Step::Stop, "no header fits after it");
    assert_eq!(step(&mp4_box(b"free", &[0; 84]), 0, 100), Step::Next(92));
}
