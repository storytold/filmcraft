//! The libvpx VP8 test vectors (`vp80-0*.ivf` with their `.md5` files of per-frame I420 MD5s),
//! when present in `target/fixtures/vp8/conformance` or `$VP8_TEST_VECTORS`. They are not
//! downloaded by the tests; fetch them from
//! `https://storage.googleapis.com/downloads.webmproject.org/test_data/libvpx/<name>`.

use crate::common;

/// MD5 (RFC 1321), for comparing with the vectors' checksums.
fn md5(data: &[u8]) -> String {
    let s: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4,
        11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(s[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0].iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
}

#[test]
fn md5_matches_rfc_1321() {
    assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
}

#[test]
fn libvpx_test_vectors() {
    let dir = std::env::var_os("VP8_TEST_VECTORS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/vp8/conformance"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SKIP: no test vectors in {}", dir.display());
        return;
    };
    let mut names: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ivf")).collect();
    names.sort();
    let mut checked = 0;
    for ivf in names {
        let Ok(sums) = std::fs::read_to_string(ivf.with_extension("ivf.md5")) else { continue };
        let expected: Vec<&str> = sums.lines().filter_map(|l| l.split_whitespace().next()).collect();
        let data = std::fs::read(&ivf).unwrap();
        let pictures = common::decode_all(&common::ivf_frames(&data));
        assert_eq!(pictures.len(), expected.len(), "{}: picture count", ivf.display());
        for (i, (p, want)) in pictures.iter().zip(&expected).enumerate() {
            assert_eq!(md5(&common::i420(p)), *want, "{} frame {i}", ivf.display());
        }
        checked += 1;
    }
    eprintln!("{checked} test vectors bit-exact");
}
