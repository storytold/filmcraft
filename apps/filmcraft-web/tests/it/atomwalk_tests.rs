//! The top-level atom walk of an MP4/MOV open: where the `moov` is found, and that hostile sizes
//! end the walk instead of looping or panicking.

use crate::atomwalk::{HEADER_LEN, MAX_HOPS, Step, step};

/// An atom header: 32-bit size (or `big` for the 64-bit form) and a four-character type.
fn header(size: u32, kind: &[u8; 4], big: Option<u64>) -> Vec<u8> {
    let mut v = size.to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    if let Some(b) = big {
        v.extend_from_slice(&b.to_be_bytes());
    }
    v
}

/// Walk like `prewarm` does over a file made of `(header, atom length)` pairs; returns the
/// outcome and the number of header reads.
fn walk(atoms: &[(Vec<u8>, u64)], size: u64) -> (Option<(u64, u64)>, usize) {
    let mut offs = vec![0u64];
    for (_, len) in atoms {
        offs.push(offs.last().unwrap() + len);
    }
    let (mut off, mut hops) = (0u64, 0usize);
    for _ in 0..MAX_HOPS {
        hops += 1;
        let h = offs.iter().position(|o| *o == off).and_then(|i| atoms.get(i)).map(|(h, _)| h.clone()).unwrap_or_default();
        assert!(h.len() as u64 <= HEADER_LEN);
        match step(&h, off, size) {
            Step::Moov { off, len } => return (Some((off, len)), hops),
            Step::Next(n) => {
                assert!(n > off, "the walk must move forward");
                off = n;
            }
            Step::Stop => return (None, hops),
        }
    }
    (None, hops)
}

#[test]
fn moov_at_the_end_is_found_in_a_few_hops() {
    let atoms = vec![
        (header(24, b"ftyp", None), 24),
        (header(8, b"wide", None), 8),
        (header(40_000_000, b"mdat", None), 40_000_000),
        (header(5000, b"moov", None), 5000),
    ];
    let size = 24 + 8 + 40_000_000 + 5000;
    assert_eq!(walk(&atoms, size), (Some((40_000_032, 5000)), 4));
}

#[test]
fn faststart_moov_is_found_early() {
    let atoms = vec![(header(24, b"ftyp", None), 24), (header(900, b"moov", None), 900), (header(1000, b"mdat", None), 1000)];
    assert_eq!(walk(&atoms, 1924), (Some((24, 900)), 2));
}

#[test]
fn a_64_bit_size_mdat_is_skipped() {
    let big = 6_000_000_000u64;
    let atoms = vec![(header(24, b"ftyp", None), 24), (header(1, b"mdat", Some(big)), big), (header(300, b"moov", None), 300)];
    assert_eq!(walk(&atoms, 24 + big + 300), (Some((24 + big, 300)), 3));
}

#[test]
fn size_zero_runs_to_the_end_and_stops() {
    let atoms = vec![(header(24, b"ftyp", None), 24), (header(0, b"mdat", None), 1000)];
    assert_eq!(walk(&atoms, 1024), (None, 2));
}

#[test]
fn hostile_sizes_stop_without_panic() {
    // too small for its own header, 64-bit form truncated, size past the end, overflow
    assert_eq!(step(&header(4, b"free", None), 0, 100), Step::Stop);
    assert_eq!(step(&header(1, b"mdat", None)[..8], 0, 100), Step::Stop);
    assert_eq!(step(&header(1, b"mdat", Some(8)), 0, 100), Step::Stop);
    assert_eq!(step(&header(1, b"mdat", Some(u64::MAX)), 40, 100), Step::Stop);
    assert_eq!(step(&header(1000, b"mdat", None), 0, 100), Step::Stop);
    assert_eq!(step(&header(1000, b"moov", None), 0, 100), Step::Stop, "a truncated moov is not fetched");
    // truncated or empty reads, header beyond the file
    assert_eq!(step(&[], 0, 100), Step::Stop);
    assert_eq!(step(&[0, 0, 0], 0, 100), Step::Stop);
    assert_eq!(step(&header(8, b"free", None), 95, 100), Step::Stop);
    assert_eq!(step(&header(8, b"free", None), u64::MAX, u64::MAX), Step::Stop);
}

#[test]
fn many_tiny_atoms_end_at_the_hop_bound() {
    let atoms: Vec<_> = (0..10_000).map(|_| (header(8, b"free", None), 8u64)).collect();
    let (found, hops) = walk(&atoms, 80_000);
    assert_eq!(found, None);
    assert_eq!(hops, MAX_HOPS);
}
