#[cfg(test)]
mod tests {
    use craft_exr::zlib::inflate_exact_without_checksum as zlib;

    // Vectors made with Python's zlib (an external tool; the bytes are data).
    /// `zlib.compress(b"hello hello hello hello", 9)`: one fixed-Huffman block with a back reference.
    const FIXED: &[u8] = &[0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08, 0xb1];

    #[test]
    fn fixed_block() {
        assert_eq!(zlib(FIXED, 23).unwrap(), b"hello hello hello hello");
    }

    #[test]
    fn stored_block() {
        // zlib header, one final stored block of 3 bytes, Adler-32
        let s = [0x78, 0x01, 0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c', 0x02, 0x4d, 0x01, 0x27];
        assert_eq!(zlib(&s, 3).unwrap(), b"abc");
    }

    /// `zlib.compressobj(9)` of the 300 bytes `(i*i*7 + i/3) % 23 + 65`: one dynamic-Huffman block.
    const DYNAMIC: &[u8] = &[
        0x78, 0xda, 0xe5, 0xcc, 0x01, 0x11, 0xc0, 0x20, 0x08, 0x00, 0xc0, 0x6c, 0x6c, 0x22, 0x70, 0x0a, 0xe2, 0xa1, 0xd0, 0xbf, 0xc9, 0x82, 0xec, 0x03, 0x3c,
        0x70, 0x8f, 0xf4, 0x07, 0x2c, 0xac, 0x60, 0x9d, 0xdd, 0xf0, 0x36, 0xd0, 0xc5, 0x6e, 0x2f, 0x81, 0x48, 0xbe, 0xc9, 0x5c, 0x08, 0x53, 0x71, 0xca, 0x29,
        0xaf, 0x1c, 0x26, 0xdb, 0x69, 0x74, 0x77, 0x52, 0x89, 0xab, 0xb1, 0x91, 0xe0, 0x1f, 0xc9, 0x07, 0x4a, 0xb8, 0x58, 0xfe,
    ];

    #[test]
    fn dynamic_block() {
        let want: Vec<u8> = (0..300usize).map(|i| ((i * i * 7 + i / 3) % 23 + 65) as u8).collect();
        assert_eq!(zlib(DYNAMIC, 300).unwrap(), want);
    }

    #[test]
    fn garbage_never_panics() {
        for seed in 1u32..3000 {
            let mut x = seed.wrapping_mul(2_654_435_761);
            let v: Vec<u8> = (0..64)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = zlib(&[[0x78, 0x01].as_slice(), &v].concat(), 4096);
        }
        // every truncation and single-byte corruption of a real stream
        for n in 0..DYNAMIC.len() {
            let _ = zlib(&DYNAMIC[..n], 300);
            let mut v = DYNAMIC.to_vec();
            v[n] ^= 0x5A;
            let _ = zlib(&v, 300);
        }
    }

    #[test]
    fn hostile_streams_are_errors() {
        assert!(zlib(&[], 1).is_err());
        assert!(zlib(&[0x78, 0x9c], 1).is_err()); // header only
        assert!(zlib(&FIXED[..8], 23).is_err()); // truncated
        assert!(zlib(FIXED, 5).is_err()); // more output than allowed
        assert!(zlib(FIXED, 30).is_err()); // less output than expected
        assert!(zlib(&[0x78, 0x01, 0x07], 1).is_err()); // reserved block type
        // distance 1 with no output yet: fixed block, length code 257, distance code 0
        assert!(zlib(&[0x78, 0x01, 0x03, 0x02], 16).is_err());
    }
}
