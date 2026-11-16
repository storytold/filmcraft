//! A still whose header declares more pixels than the RGBA copy may hold is refused before any
//! pixel is decoded. Hand-built PNG headers: the test never allocates the declared size.

use filmcraft_media::still::decode_raster;

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend((body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(body);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

/// An 8-bit grayscale PNG header of `w` x `h` followed by an empty IDAT.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &[]);
    out
}

#[test]
fn a_picture_over_the_pixel_cap_is_refused_from_its_header() {
    // 2^28 + 1 pixels: its RGBA copy alone would be over 1 GiB
    let e = decode_raster("big.png", &png_header(1 << 14, (1 << 14) + 1)).err().expect("refused").to_string();
    assert!(e.contains("too large"), "{e}");
}
