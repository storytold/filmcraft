use super::*;

fn file(res: &str, body: &[u8]) -> Vec<u8> {
    let mut v = format!("#?RADIANCE\n# test\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=1.0\n\n{res}\n").into_bytes();
    v.extend_from_slice(body);
    v
}

fn px(f: &VideoFrame) -> Vec<f32> {
    match &f.data {
        filmcraft_frame::PixelData::RgbaF32(p) => p.to_vec(),
        _ => panic!("not float"),
    }
}

/// New-style RLE of one scan line, as Radiance's own writer does: per channel, runs of 3+ equal bytes.
fn rle_line(line: &[[u8; 4]]) -> Vec<u8> {
    let n = line.len();
    let mut out = vec![2, 2, (n >> 8) as u8, n as u8];
    for c in 0..4 {
        let ch: Vec<u8> = line.iter().map(|p| p[c]).collect();
        let mut i = 0;
        while i < n {
            let run = ch[i..].iter().take(127).take_while(|&&b| b == ch[i]).count();
            if run >= 3 {
                out.extend([128 + run as u8, ch[i]]);
                i += run;
            } else {
                let mut j = i + 1;
                while j < n && j - i < 128 && !(j + 2 < n && ch[j] == ch[j + 1] && ch[j] == ch[j + 2]) {
                    j += 1;
                }
                out.push((j - i) as u8);
                out.extend_from_slice(&ch[i..j]);
                i = j;
            }
        }
    }
    out
}

#[test]
fn rgbe_values() {
    assert_eq!(rgbe([0, 0, 0, 0]), [0.0; 3]);
    assert_eq!(rgbe([128, 128, 128, 129]), [1.0; 3]);
    assert_eq!(rgbe([128, 64, 0, 136]), [128.0, 64.0, 0.0]);
    assert_eq!(rgbe([128, 128, 128, 128])[0], 0.5);
    assert!(rgbe([255, 255, 255, 255])[0].is_finite());
}

#[test]
fn flat_top_to_bottom() {
    let body = [[10u8, 20, 30, 136], [1, 2, 3, 136], [4, 5, 6, 137], [7, 8, 9, 135]].concat();
    let f = decode("a.hdr", &file("-Y 2 +X 2", &body)).unwrap();
    assert_eq!((f.width, f.height), (2, 2));
    let p = px(&f);
    assert_eq!(&p[0..4], &[10.0, 20.0, 30.0, 1.0]);
    assert_eq!(&p[4..8], &[1.0, 2.0, 3.0, 1.0]);
    assert_eq!(&p[8..12], &[8.0, 10.0, 12.0, 1.0]); // exponent 137: scale 2
    assert_eq!(&p[12..16], &[3.5, 4.0, 4.5, 1.0]); // exponent 135: scale 1/2
}

#[test]
fn orientations() {
    // 3 wide, 2 high, pixel value = its index in the file
    let body: Vec<u8> = (0..6u8).flat_map(|i| [i, 0, 0, 136]).collect();
    let red = |res: &str| -> Vec<f32> {
        let f = decode("a.hdr", &file(res, &body)).unwrap();
        px(&f).chunks(4).map(|p| p[0]).collect()
    };
    assert_eq!(red("-Y 2 +X 3"), [0., 1., 2., 3., 4., 5.]);
    assert_eq!(red("+Y 2 +X 3"), [3., 4., 5., 0., 1., 2.]);
    assert_eq!(red("-Y 2 -X 3"), [2., 1., 0., 5., 4., 3.]);
    // scan lines are columns: file index = x * 2 + (row, top first for -Y)
    assert_eq!(red("+X 3 -Y 2"), [0., 2., 4., 1., 3., 5.]);
    assert_eq!(red("-X 3 +Y 2"), [5., 3., 1., 4., 2., 0.]);
}

#[test]
fn run_length_lines() {
    let w = 40usize;
    let rows: Vec<Vec<[u8; 4]>> = (0..3).map(|y| (0..w).map(|x| if x < 20 { [100, 50, 25, 130] } else { [(x * 5 + y) as u8, 7, 9, 131] }).collect()).collect();
    let body: Vec<u8> = rows.iter().flat_map(|r| rle_line(r)).collect();
    let f = decode("a.hdr", &file(&format!("-Y 3 +X {w}"), &body)).unwrap();
    let p = px(&f);
    for (y, r) in rows.iter().enumerate() {
        for (x, q) in r.iter().enumerate() {
            let want = rgbe(*q);
            assert_eq!(&p[(y * w + x) * 4..(y * w + x) * 4 + 3], &want, "pixel {x},{y}");
        }
    }
}

#[test]
fn old_style_runs() {
    // one pixel, then (1,1,1,3) repeats it 3 times, then a (1,1,1,1) pair repeats 1<<0 then 1<<8 -> clipped to the line
    let body = [[5u8, 6, 7, 136], [1, 1, 1, 3], [9, 9, 9, 136]].concat();
    let f = decode("a.hdr", &file("-Y 1 +X 5", &body)).unwrap();
    let p = px(&f);
    assert_eq!(p[0], 5.0);
    assert_eq!(p[12], 5.0); // fourth pixel is still a repeat
    assert_eq!(p[16], 9.0);
}

#[test]
fn hostile_input_is_an_error_not_a_panic() {
    let ok = file("-Y 2 +X 2", &[0; 16]);
    assert!(decode("a.hdr", &ok).is_ok());
    for cut in 0..ok.len() - 1 {
        // a cut anywhere is an error (the header or the pixel data is incomplete), never a panic
        assert!(decode("a.hdr", &ok[..cut]).is_err(), "{cut}");
    }
    assert!(decode("a.hdr", &file("-Y 2 +X 2", &[0; 8])).is_err());
    assert!(decode("a.hdr", &file("-Y 99999999 +X 99999999", &[0; 16])).is_err());
    // Pass the coarse compressed-size check but fail scanline validation. This must be rejected
    // before a 1 GiB float image is initialized.
    assert!(decode("a.hdr", &file("-Y 67108864 +X 1", &[0; 16_384])).is_err());
    assert!(decode("a.hdr", &file("-Y 0 +X 4", &[])).is_err());
    assert!(decode("a.hdr", &file("-Y 2 -Y 2", &[0; 16])).is_err());
    assert!(decode("a.hdr", &file("junk", &[0; 16])).is_err());
    assert!(decode("a.hdr", b"#?RADIANCE\nFORMAT=32-bit_rle_xyze\n\n-Y 1 +X 1\n\0\0\0\0").is_err());
    // an RLE line announcing the wrong width, a run past the end, a run at the start of a flat line
    assert!(decode("a.hdr", &file("-Y 1 +X 10", &[2, 2, 0, 11, 0])).is_err());
    assert!(decode("a.hdr", &file("-Y 1 +X 10", &[2, 2, 0, 10, 200, 1])).is_err());
    assert!(decode("a.hdr", &file("-Y 1 +X 3", &[1, 1, 1, 9, 0, 0, 0, 0])).is_err());
    assert!(decode("a.hdr", b"#?RADIANCE").is_err());
}

#[test]
fn opens_as_a_float_still() {
    let body = [[128u8, 128, 128, 136]; 4].concat();
    let s = still("a.hdr", &file("-Y 2 +X 2", &body)).unwrap();
    let info = crate::MediaSource::info(&s);
    assert_eq!(info.video.as_ref().map(|v| (v.width, v.height, v.codec.as_str())), Some((2, 2, "HDR")));
    let src = crate::open_bytes("a.hdr", file("-Y 2 +X 2", &body).into(), &[]).unwrap();
    assert_eq!(src.info().kind, MediaKind::Still);
}
