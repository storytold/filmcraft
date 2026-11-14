use filmcraft_interchange::wav::parse_wav;

const TAIL: [u8; 12] = [0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71];

fn extensible(sub_format: u16, fmt_len: usize) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&48_000u32.to_le_bytes());
    fmt.extend_from_slice(&192_000u32.to_le_bytes());
    fmt.extend_from_slice(&4u16.to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());
    fmt.extend_from_slice(&22u16.to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());
    fmt.extend_from_slice(&4u32.to_le_bytes());
    fmt.extend_from_slice(&sub_format.to_le_bytes());
    fmt.extend_from_slice(&[0, 0]);
    fmt.extend_from_slice(&TAIL);
    fmt.truncate(fmt_len);
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&((4 + 8 + fmt.len() + 8 + 4) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    v.extend_from_slice(&fmt);
    v.extend_from_slice(b"data");
    v.extend_from_slice(&4u32.to_le_bytes());
    v.extend_from_slice(&[1, 2, 3, 4]);
    v
}

#[test]
fn extensible_float_is_rejected() {
    assert!(parse_wav(&extensible(3, 40)).is_none());
}

#[test]
fn extensible_pcm_is_accepted() {
    let (pcm, ch, sr, bits) = parse_wav(&extensible(1, 40)).expect("pcm");
    assert_eq!((pcm, ch, sr, bits), (vec![1, 2, 3, 4], 1, 48_000, 32));
}

#[test]
fn extensible_truncated_does_not_panic() {
    for n in 16..40 {
        let _ = parse_wav(&extensible(3, n));
    }
}
