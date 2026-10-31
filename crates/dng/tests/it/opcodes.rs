//! Opcode lists in whole files: each list runs at its stage (OpcodeList1 on the stored samples,
//! OpcodeList2 on linear values before white balance, OpcodeList3 after the demosaic), unknown
//! opcodes follow their optional flag, preview-only opcodes are skipped on the reduced path, and
//! hostile lists are errors, never panics.

use filmcraft_dng::opcode::encode;
use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Options, Value, write};
use filmcraft_dng::{Dng, Error, ProcessOptions, decode_with};

/// Develop with the opcode lists applied (they are off by default).
fn decode(bytes: &[u8]) -> Result<filmcraft_dng::Image, Error> {
    decode_with(bytes, &ProcessOptions { apply_opcodes: true, ..Default::default() })
}

fn u(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn d(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn f(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn picture(w: usize, h: usize, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..w * h)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (500 + (i * 13) % 2500 + (s % 64) as usize) as u16
        })
        .collect()
}

/// A 12-bit RGGB file, white balanced by (0.5, 1, 0.8), with `lists` as (tag, list) pairs.
fn bayer(px: &[u16], w: usize, h: usize, lists: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut raw_tags = vec![(tags::BLACK_LEVEL, Value::Short(vec![100])), (tags::WHITE_LEVEL, Value::Short(vec![4095]))];
    raw_tags.extend(lists.iter().map(|(t, l)| (*t, Value::Undefined(l.clone()))));
    let o = Options {
        width: w,
        height: h,
        bits: 12,
        compression: Compression::LosslessJpeg { predictor: 1, components: 1, restart_lines: 0, pt: 0 },
        main_tags: vec![(tags::AS_SHOT_NEUTRAL, Value::rationals(&[0.5, 1.0, 0.8]))],
        raw_tags,
        ..Default::default()
    };
    write(px, &o)
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn opcode_list_1_runs_on_the_stored_samples() {
    let (w, h) = (16, 12);
    let px = picture(w, h, 1);
    // MapTable halving every code over the whole raw image = a file holding the halved codes
    let table: Vec<u8> = (0..4096u32).flat_map(|v| ((v / 2) as u16).to_be_bytes()).collect();
    let list = encode(&[(7, 0, [u(&[0, 0, h as u32, w as u32, 0, 1, 1, 1, 4096]), table].concat())]);
    let halved: Vec<u16> = px.iter().map(|v| v / 2).collect();
    let a = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_1, list)])).unwrap();
    let b = decode(&bayer(&halved, w, h, &[])).unwrap();
    assert!(same_bits(&a.rgba, &b.rgba));
    // raw() stays as stored
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_1, encode(&[(7, 0, vec![])]))]);
    assert_eq!(Dng::parse(&f).unwrap().raw().unwrap().data, px);
    assert!(decode(&f).is_err(), "a mandatory MapTable without parameters is malformed");
}

#[test]
fn opcode_list_2_runs_before_white_balance() {
    let (w, h) = (16, 12);
    let px = picture(w, h, 2);
    // ScalePerRow 0.5 on every row of the linear values = halving (code − black)
    let list = encode(&[(12, 0, [u(&[0, 0, h as u32, w as u32, 0, 1, 1, 1, h as u32]), f(&vec![0.5; h])].concat())]);
    let a = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_2, list)])).unwrap();
    let halved: Vec<u16> = px.iter().map(|&v| ((f64::from(v) - 100.0) / 2.0 + 100.0) as u16).collect();
    let b = decode(&bayer(&halved, w, h, &[])).unwrap();
    for (x, y) in a.rgba.iter().zip(&b.rgba) {
        assert!((x - y).abs() < 2e-3, "{x} {y}");
    }
    // a gain only on the green sites of the mosaic (row / column pitch 2 from (0, 1) and (1, 0))
    let green = encode(&[
        (12, 0, [u(&[0, 1, h as u32, w as u32, 0, 1, 2, 2, (h / 2) as u32]), f(&vec![0.0; h / 2])].concat()),
        (12, 0, [u(&[1, 0, h as u32, w as u32, 0, 1, 2, 2, (h / 2) as u32]), f(&vec![0.0; h / 2])].concat()),
    ]);
    let g = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_2, green)])).unwrap();
    let plain = decode(&bayer(&px, w, h, &[])).unwrap();
    let mean_green = |img: &[f32]| img.chunks(4).map(|p| p[1]).sum::<f32>();
    assert!(mean_green(&g.rgba) < 0.1 * mean_green(&plain.rgba), "green sites zeroed");
    // red and blue sites untouched: the red of a red site is the same
    assert_eq!(g.rgba[0], plain.rgba[0]);
}

#[test]
fn opcode_list_3_runs_after_the_demosaic_and_may_trim() {
    let (w, h) = (16, 12);
    let px = picture(w, h, 3);
    let plain = decode(&bayer(&px, w, h, &[])).unwrap();
    // TrimBounds to (2, 1)–(10, 9) on the demosaiced picture
    let trim = encode(&[(6, 0, u(&[1, 2, 9, 10]))]);
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_3, trim)]);
    let t = decode(&f).unwrap();
    assert_eq!((t.width, t.height), (8, 8));
    assert_eq!(Dng::parse(&f).unwrap().info().unwrap().width, w, "the info keeps the stored crop");
    for y in 0..8 {
        for x in 0..8 {
            let a = &t.rgba[(y * 8 + x) * 4..][..4];
            let b = &plain.rgba[((y + 1) * w + x + 2) * 4..][..4];
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 1e-5, "({x},{y}) {k}: {} {}", a[k], b[k]);
            }
        }
    }
    // a warp on the demosaiced picture is allowed (identity changes nothing beyond clipping)
    let warp = encode(&[(1, 0, [u(&[1]), d(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat())]);
    let wpd = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_3, warp.clone())])).unwrap();
    for (a, b) in wpd.rgba.iter().zip(&plain.rgba) {
        assert!((a - b.min(1.0 / 0.5)).abs() < 1e-4, "{a} {b}");
    }
    // the same warp before the demosaic is refused (mandatory) or skipped (optional)
    let e = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_2, warp)]));
    assert!(matches!(e, Err(Error::Unsupported(_))), "{:?}", e.err());
    let optional = encode(&[(1, 1, [u(&[1]), d(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat())]);
    let o = decode(&bayer(&px, w, h, &[(tags::OPCODE_LIST_2, optional)])).unwrap();
    assert!(same_bits(&o.rgba, &plain.rgba));
}

#[test]
fn unknown_opcodes_and_the_preview_path() {
    let (w, h) = (32, 24);
    let px = picture(w, h, 4);
    let plain = decode(&bayer(&px, w, h, &[])).unwrap();
    // an unknown optional opcode is skipped, a mandatory one names itself
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_1, encode(&[(4242, 1, vec![1, 2])]))]);
    assert!(same_bits(&decode(&f).unwrap().rgba, &plain.rgba));
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_3, encode(&[(4242, 0, vec![])]))]);
    assert!(matches!(decode(&f), Err(Error::Unsupported(m)) if m.contains("4242")));
    // a preview-only vignette in OpcodeList3 is skipped at half size and applied at full size
    let vig = |flags| encode(&[(3, flags, d(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]))]);
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_3, vig(2))]);
    let half = decode_with(&f, &ProcessOptions { target_size: Some((16, 12)), apply_opcodes: true, ..Default::default() }).unwrap();
    assert_eq!((half.width, half.height), (16, 12));
    let half_plain = decode_with(&bayer(&px, w, h, &[]), &ProcessOptions { target_size: Some((16, 12)), apply_opcodes: true, ..Default::default() }).unwrap();
    assert!(same_bits(&half.rgba, &half_plain.rgba));
    let full = decode(&f).unwrap();
    assert!(full.rgba[0] > plain.rgba[0], "the corner brightened");
    // a mandatory OpcodeList3 opcode keeps full resolution
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_3, vig(0))]);
    let img = decode_with(&f, &ProcessOptions { target_size: Some((16, 12)), apply_opcodes: true, ..Default::default() }).unwrap();
    assert_eq!((img.width, img.height), (w, h));
}

#[test]
fn hostile_opcode_lists_are_errors_not_panics() {
    let (w, h) = (16, 12);
    let px = picture(w, h, 5);
    let lists = [
        (tags::OPCODE_LIST_1, encode(&[(5, 0, u(&[0, 2, 1, 3, 3, 0, 0, 4, 4, 6, 6])), (4, 1, u(&[0, 0]))])),
        (tags::OPCODE_LIST_2, encode(&[(9, 0, [u(&[0, 0, 12, 16, 0, 1, 1, 1, 2, 2]), d(&[1.0, 1.0, 0.0, 0.0]), u(&[1]), f(&[1.0, 1.1, 1.2, 1.3])].concat())])),
        (tags::OPCODE_LIST_3, encode(&[(3, 0, d(&[0.1, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])), (6, 0, u(&[1, 1, 11, 15]))])),
    ];
    let f = bayer(&px, w, h, &lists);
    if let Err(e) = decode(&f) {
        panic!("{e}");
    }
    let mut s = 777u32;
    for i in 0..3000 {
        let mut g = f.clone();
        for _ in 0..1 + i % 3 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let at = s as usize % g.len();
            g[at] = if i % 4 == 0 { 0xFF } else { g[at] ^ (1 << (s >> 29)) };
        }
        assert!(std::panic::catch_unwind(|| decode(&g).map(|_| ())).is_ok(), "mutation {i}");
    }
}

#[test]
fn opcode_lists_are_ignored_unless_asked_for() {
    let (w, h) = (16, 12);
    let px = picture(w, h, 7);
    let table: Vec<u8> = (0..4096u32).flat_map(|v| ((v / 2) as u16).to_be_bytes()).collect();
    let list = encode(&[(7, 0, [u(&[0, 0, h as u32, w as u32, 0, 1, 1, 1, 4096]), table].concat())]);
    let f = bayer(&px, w, h, &[(tags::OPCODE_LIST_1, list)]);
    let plain = filmcraft_dng::decode(&bayer(&px, w, h, &[])).unwrap();
    assert!(same_bits(&filmcraft_dng::decode(&f).unwrap().rgba, &plain.rgba), "default ignores the lists");
    assert!(!same_bits(&decode(&f).unwrap().rgba, &plain.rgba), "apply_opcodes uses them");
}
