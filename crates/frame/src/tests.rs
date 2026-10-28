#[test]
fn cropping_cuts_every_plane_at_the_same_place() {
    // 6x4 RGBA8, red = index
    let px: Vec<u8> = (0..24u8).flat_map(|i| [i, 0, 0, 255]).collect();
    let f = VideoFrame { par: (4, 3), ..VideoFrame::rgba8(6, 4, px) };
    let c = f.cropped(Region { x: 1, y: 1, w: 3, h: 2 });
    assert_eq!((c.width, c.height, c.par), (3, 2, (4, 3)));
    assert_eq!(c.to_rgba8().chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [7, 8, 9, 13, 14, 15]);
    // a region reaching past the frame is clipped to it; an empty or full one changes nothing
    let c = f.cropped(Region { x: 4, y: 3, w: 100, h: 100 });
    assert_eq!((c.width, c.height), (2, 1));
    assert_eq!(c.to_rgba8().chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [22, 23]);
    for r in [
        Region { x: 0, y: 0, w: 0, h: 2 },
        Region { x: 9, y: 9, w: 2, h: 2 },
        Region { x: 0, y: 0, w: 6, h: 4 },
        Region { x: 0, y: 0, w: usize::MAX, h: usize::MAX },
    ] {
        let c = f.cropped(r);
        assert_eq!((c.width, c.height), (6, 4), "{r:?}");
    }

    // 4:2:0, odd sizes (7x5: chroma 4x3), with alpha: luma = index, chroma = 100 + index
    let luma: Vec<u8> = (0..35u8).collect();
    let cb: Vec<u8> = (100..112u8).collect();
    let yuv = |chroma| VideoFrame {
        width: 7,
        height: 5,
        data: PixelData::Yuv8 { planes: [Arc::new(luma.clone()), Arc::new(cb.clone()), Arc::new(cb.clone())], chroma, alpha: Some(Arc::new(luma.clone())) },
        color: Default::default(),
        par: (1, 1),
        pts: Default::default(),
    };
    // an odd corner moves onto the chroma grid (1,1 -> 0,0), keeping the size
    let c = yuv(Chroma::C420).cropped(Region { x: 1, y: 1, w: 3, h: 3 });
    assert_eq!((c.width, c.height), (3, 3));
    let PixelData::Yuv8 { planes, alpha, .. } = &c.data else { panic!() };
    assert_eq!(*planes[0], [0, 1, 2, 7, 8, 9, 14, 15, 16]);
    assert_eq!(**alpha.as_ref().unwrap(), *planes[0]);
    assert_eq!(*planes[1], [100, 101, 104, 105], "chroma 2x2 from (0,0)");
    // even corner: chroma starts at (1,1); the last chroma column of an odd width is kept
    let c = yuv(Chroma::C420).cropped(Region { x: 2, y: 2, w: 5, h: 3 });
    let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
    assert_eq!((c.width, c.height), (5, 3));
    assert_eq!(*planes[1], [105, 106, 107, 109, 110, 111], "chroma 3x2 from (1,1)");
    // 4:4:4 needs no alignment
    let c = yuv(Chroma::C444).cropped(Region { x: 1, y: 1, w: 2, h: 1 });
    let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
    assert_eq!(*planes[0], [8, 9]);
    // a plane shorter than its size gives a blank picture, never a panic
    let mut short = yuv(Chroma::C420);
    if let PixelData::Yuv8 { planes, .. } = &mut short.data {
        planes[0] = Arc::new(vec![1; 3]);
    }
    let c = short.cropped(Region { x: 2, y: 2, w: 2, h: 2 });
    let PixelData::Yuv8 { planes, .. } = &c.data else { panic!() };
    assert_eq!(*planes[0], [0, 0, 0, 0]);
}

#[test]
fn rotation_turns_every_plane_clockwise() {
    // 4x2 RGBA8 with distinct pixels 0..8
    let px: Vec<u8> = (0..8u8).flat_map(|i| [i, 0, 0, 255]).collect();
    let f = VideoFrame { par: (4, 3), ..VideoFrame::rgba8(4, 2, px) };
    let r = f.rotated(1);
    assert_eq!((r.width, r.height, r.par), (2, 4, (3, 4)));
    let red: Vec<u8> = r.to_rgba8().chunks(4).map(|p| p[0]).collect();
    // source rows [0 1 2 3] / [4 5 6 7] turned clockwise: the bottom row becomes the left column
    assert_eq!(red, [4, 0, 5, 1, 6, 2, 7, 3]);
    let red180: Vec<u8> = f.rotated(2).to_rgba8().chunks(4).map(|p| p[0]).collect();
    assert_eq!(red180, [7, 6, 5, 4, 3, 2, 1, 0]);
    let red270: Vec<u8> = f.rotated(3).to_rgba8().chunks(4).map(|p| p[0]).collect();
    assert_eq!(red270, [3, 7, 2, 6, 1, 5, 0, 4]);
    assert_eq!(f.rotated(4).width, 4);

    // 4:2:0 4x2: chroma 2x1 -> 1x2; 4:2:2 4x2: chroma 2x2 -> widened 4:4:4 2x4
    let yuv = |chroma: Chroma, c: Vec<u8>| VideoFrame {
        width: 4,
        height: 2,
        data: PixelData::Yuv8 { planes: [Arc::new((0..8).collect()), Arc::new(c.clone()), Arc::new(c)], chroma, alpha: None },
        color: ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    };
    let r = yuv(Chroma::C420, vec![10, 20]).rotated(1);
    let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
    assert_eq!((*chroma, &planes[0][..], &planes[1][..]), (Chroma::C420, &[4, 0, 5, 1, 6, 2, 7, 3][..], &[10, 20][..]));
    let r = yuv(Chroma::C422, vec![10, 20, 30, 40]).rotated(1);
    let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
    assert_eq!((*chroma, &planes[1][..]), (Chroma::C444, &[30, 10, 30, 10, 40, 20, 40, 20][..]));
}

#[test]
fn box_decimation_averages_blocks_per_plane() {
    // 6x4 4:2:0: luma ramp, chroma 3x2
    let y: Vec<u8> = (0..24).map(|i| (i * 10) as u8).collect();
    let u = vec![10, 20, 30, 40, 50, 60];
    let v = vec![200; 6];
    let f = VideoFrame {
        width: 6,
        height: 4,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    };
    let d = f.box_decimated(2).expect("yuv");
    assert_eq!((d.width, d.height), (3, 2));
    let PixelData::Yuv8 { planes, .. } = &d.data else { panic!() };
    // luma block (0,0): 0, 10, 60, 70 -> 35
    assert_eq!(planes[0][0], 35);
    assert_eq!(planes[0].len(), 6);
    // chroma 3x2 -> 2x1: blocks {10,20,40,50} = 30 and the clamped edge block {30,60} = 45
    assert_eq!(&planes[1][..], &[30, 45]);
    assert_eq!(&planes[2][..], &[200, 200]);
    assert!(f.box_decimated(1).is_none());
    assert!(VideoFrame::rgba8(2, 2, vec![0; 16]).box_decimated(2).is_none());
}

#[test]
fn decimation_caps_hostile_factors_to_the_frame_size() {
    let rgba = VideoFrame::rgba8(1, 1, vec![23, 42, 99, 255]);
    let (w, h, pixels) = rgba.to_linear_f32_decimated(usize::MAX);
    assert_eq!((w, h), (1, 1));
    assert_eq!(pixels, rgba.to_linear_f32());

    let yuv = VideoFrame {
        width: 2,
        height: 2,
        data: PixelData::Yuv8 { planes: [Arc::new(vec![1, 2, 3, 4]), Arc::new(vec![128]), Arc::new(vec![128])], chroma: Chroma::C420, alpha: None },
        color: ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    };
    assert_eq!(yuv.box_decimated(usize::MAX).expect("yuv").luma8(), vec![3]);
}
use super::*;

#[test]
fn rgba8_roundtrip_through_linear() {
    let px: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2, 255]).collect();
    let f = VideoFrame::rgba8(256, 1, px.clone());
    let lin = f.to_linear_f32();
    let back = VideoFrame::rgba_f32(256, 1, lin).to_rgba8();
    assert_eq!(back, px);
}

#[test]
fn yuv_grey_is_grey() {
    let (w, h) = (4, 2);
    let y = Arc::new(vec![126u8; w * h]);
    let u = Arc::new(vec![128u8; 2]);
    let v = Arc::new(vec![128u8; 2]);
    let f = VideoFrame {
        width: 4,
        height: 2,
        data: PixelData::Yuv8 { planes: [y, u, v], chroma: Chroma::C420, alpha: None },
        color: ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    };
    let rgb = f.to_rgba8();
    assert_eq!(&rgb[0..4], &[128, 128, 128, 255]);
    // slow path agrees within 1
    let slow = VideoFrame::rgba_f32(4, 2, f.to_linear_f32()).to_rgba8();
    assert!((slow[0] as i32 - 128).abs() <= 1);
}

#[test]
fn audio_mix() {
    let mut a = AudioBuffer::silence(48000, 2, 4);
    let b = AudioBuffer { sample_rate: 48000, channels: vec![vec![0.5; 4]] };
    a.mix_from(&b, &[1.0, 0.5]);
    assert_eq!(a.channels[1][0], 0.25);
    assert_eq!(a.interleaved()[..2], [0.5, 0.25]);
}

/// Frames of every pixel format with partial and zero alpha, at a size that is not a multiple
/// of the decimation factors.
fn sample_frames() -> Vec<(&'static str, VideoFrame)> {
    sample_frames_sized(37, 23)
}

fn sample_frames_sized(w: usize, h: usize) -> Vec<(&'static str, VideoFrame)> {
    let alpha = |x: usize, y: usize| -> u8 {
        if y < 9 || x < 6 {
            0
        } else if y == 9 || x == 6 {
            77
        } else {
            255
        }
    };
    let rgba8: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7 % 251) as u8, (i * 13 % 241) as u8, (i * 29 % 239) as u8, alpha(i % w, i / w)]).collect();
    let f32s: Vec<f32> =
        (0..w * h).flat_map(|i| [(i % 17) as f32 / 17.0, (i % 5) as f32 / 5.0, (i % 11) as f32 / 11.0, alpha(i % w, i / w) as f32 / 255.0]).collect();
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y8: Vec<u8> = (0..w * h).map(|i| (16 + i * 3 % 219) as u8).collect();
    let u8p: Vec<u8> = (0..cw * ch).map(|i| (16 + i * 5 % 224) as u8).collect();
    let v8p: Vec<u8> = (0..cw * ch).map(|i| (16 + i * 11 % 224) as u8).collect();
    let a8: Vec<u8> = (0..w * h).map(|i| alpha(i % w, i / w)).collect();
    let y16: Vec<u16> = (0..w * h).map(|i| (64 + i * 3 % 876) as u16).collect();
    let c16: Vec<u16> = (0..w * h).map(|i| (64 + i * 7 % 896) as u16).collect();
    let a16: Vec<u16> = (0..w * h).map(|i| (alpha(i % w, i / w) as u16) * 4).collect();
    let c422: Vec<u16> = (0..cw * h).map(|i| (64 + i * 7 % 896) as u16).collect();
    let mk = |data| VideoFrame { width: w as u32, height: h as u32, data, color: ColorInfo::REC709, par: (1, 1), pts: Tick::ZERO };
    vec![
        ("rgba8", mk(PixelData::Rgba8(Arc::new(rgba8)))),
        ("rgba f32", mk(PixelData::RgbaF32(Arc::new(f32s)))),
        (
            "yuv8 4:2:0 + alpha",
            mk(PixelData::Yuv8 {
                planes: [Arc::new(y8.clone()), Arc::new(u8p.clone()), Arc::new(v8p.clone())],
                chroma: Chroma::C420,
                alpha: Some(Arc::new(a8)),
            }),
        ),
        ("yuv8 4:2:0", mk(PixelData::Yuv8 { planes: [Arc::new(y8), Arc::new(u8p), Arc::new(v8p)], chroma: Chroma::C420, alpha: None })),
        (
            "yuv16 4:4:4 + alpha",
            mk(PixelData::Yuv16 {
                planes: [Arc::new(y16.clone()), Arc::new(c16.clone()), Arc::new(c16.clone())],
                chroma: Chroma::C444,
                bits: 10,
                alpha: Some(Arc::new(a16.clone())),
            }),
        ),
        (
            "yuv16 4:2:2 + alpha",
            mk(PixelData::Yuv16 {
                planes: [Arc::new(y16.clone()), Arc::new(c422.clone()), Arc::new(c422)],
                chroma: Chroma::C422,
                bits: 10,
                alpha: Some(Arc::new(a16)),
            }),
        ),
        ("yuv16 4:4:4", mk(PixelData::Yuv16 { planes: [Arc::new(y16), Arc::new(c16.clone()), Arc::new(c16)], chroma: Chroma::C444, bits: 12, alpha: None })),
    ]
}

#[test]
fn a_region_is_exactly_the_crop_of_the_full_conversion() {
    for (name, f) in sample_frames() {
        for n in [1usize, 2, 3] {
            let (ow, oh, full) = f.to_linear_f32_decimated_with(n, None);
            for want in [
                Region { x: 0, y: 0, w: ow, h: oh },
                Region { x: 0, y: 0, w: 1, h: 1 },
                Region { x: ow - 1, y: oh - 1, w: 1, h: 1 },
                Region { x: 3, y: 2, w: ow - 5, h: oh - 4 },
                Region { x: 0, y: oh / 2, w: ow, h: 1 },
                Region { x: ow / 2, y: 0, w: 1, h: oh },
                // sticks out of the picture: clipped
                Region { x: ow - 2, y: oh - 2, w: 10, h: 10 },
            ] {
                let (r, px) = f.to_linear_f32_region(n, None, want);
                assert_eq!(px.len(), r.w * r.h * 4, "{name} n={n} {want:?}");
                assert_eq!(r, want.clip(ow, oh));
                for ry in 0..r.h {
                    for rx in 0..r.w {
                        for k in 0..4 {
                            let (a, b) = (px[(ry * r.w + rx) * 4 + k], full[((r.y + ry) * ow + r.x + rx) * 4 + k]);
                            assert_eq!(a.to_bits(), b.to_bits(), "{name} n={n} {r:?} at ({rx},{ry}) channel {k}: {a} vs {b}");
                        }
                    }
                }
            }
            // outside the picture altogether: nothing, and no panic
            let (r, px) = f.to_linear_f32_region(n, None, Region { x: ow + 5, y: 0, w: 4, h: 4 });
            assert!(r.is_empty() && px.is_empty(), "{name}");
        }
    }
}

/// The conversions write every float they hand out: into a buffer that held something else
/// (here NaN, which the pool gives back as it was) the result is the one a fresh buffer gives.
#[test]
fn a_recycled_buffer_never_leaks_its_old_contents() {
    // big enough for the float images to be worth keeping (64 KiB), at sizes no other test of this
    // binary converts, so the poisoned buffer is the one the next conversion takes
    let poison = |len: usize| pool::recycle_f32(vec![f32::NAN; len]);
    let same = |what: &str, fresh: &[f32], reused: &[f32]| {
        assert_eq!(fresh.len(), reused.len(), "{what}");
        if let Some(i) = fresh.iter().zip(reused).position(|(a, b)| a.to_bits() != b.to_bits()) {
            panic!("{what}: element {i} is {} in a fresh buffer and {} in a recycled one", fresh[i], reused[i]);
        }
    };
    for (name, f) in sample_frames_sized(203, 151) {
        for n in [1usize, 2] {
            let (ow, oh) = f.decimated_size(n);
            let (_, _, fresh) = f.to_linear_f32_decimated(n);
            poison(ow * oh * 4);
            let (_, _, reused) = f.to_linear_f32_decimated(n);
            same(&format!("{name} n={n}, whole picture"), &fresh, &reused);

            let want = Region { x: 1, y: 1, w: 70, h: 60 };
            let (r, fresh) = f.to_linear_f32_region(n, None, want);
            assert_eq!(r, want, "{name} n={n}");
            poison(r.w * r.h * 4);
            let (_, reused) = f.to_linear_f32_region(n, None, want);
            same(&format!("{name} n={n}, rectangle"), &fresh, &reused);
        }
    }
    // a frame with less data than its size says (no row of it fits): transparent, never what the buffer held
    let short = VideoFrame { data: PixelData::RgbaF32(Arc::new(vec![0.5; 100])), ..VideoFrame::rgba_f32(203, 151, vec![0.0; 203 * 151 * 4]) };
    poison(203 * 151 * 4);
    let (_, _, px) = short.to_linear_f32_decimated(1);
    assert!(px.iter().all(|v| *v == 0.0), "rows without data are transparent");
}

#[test]
fn alpha_region_is_the_box_around_what_is_not_transparent() {
    for (name, f) in sample_frames() {
        for n in [1usize, 2, 4] {
            let got = f.alpha_region(n);
            let has_alpha = matches!(f.data, PixelData::Rgba8(_) | PixelData::Yuv8 { alpha: Some(_), .. } | PixelData::Yuv16 { alpha: Some(_), .. });
            assert_eq!(got.is_some(), has_alpha, "{name}");
            let Some(got) = got else { continue };
            // the same box measured on the converted picture: pixels whose alpha is not zero
            let (ow, oh, px) = f.to_linear_f32_decimated(n);
            let on: Vec<(usize, usize)> = (0..oh).flat_map(|y| (0..ow).map(move |x| (x, y))).filter(|(x, y)| px[(y * ow + x) * 4 + 3] != 0.0).collect();
            let want = match (on.iter().map(|p| p.0).min(), on.iter().map(|p| p.0).max(), on.iter().map(|p| p.1).min(), on.iter().map(|p| p.1).max()) {
                (Some(x0), Some(x1), Some(y0), Some(y1)) => Region { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 },
                _ => Region::default(),
            };
            assert_eq!(got, want, "{name} n={n}");
            // and everything outside it is exactly transparent black
            for y in 0..oh {
                for x in 0..ow {
                    if !(x >= got.x && x < got.x + got.w && y >= got.y && y < got.y + got.h) {
                        assert_eq!(&px[(y * ow + x) * 4..(y * ow + x) * 4 + 4], &[0.0; 4], "{name} n={n} at ({x},{y})");
                    }
                }
            }
        }
    }
    // a picture that is transparent everywhere
    let clear = VideoFrame {
        data: PixelData::Yuv8 {
            planes: [Arc::new(vec![16; 64]), Arc::new(vec![128; 16]), Arc::new(vec![128; 16])],
            chroma: Chroma::C420,
            alpha: Some(Arc::new(vec![0; 64])),
        },
        ..VideoFrame::rgba8(8, 8, vec![0; 256])
    };
    assert_eq!(clear.alpha_region(1), Some(Region::default()));
    // malformed alpha (too short): nothing can be assumed
    let short = VideoFrame {
        data: PixelData::Yuv8 {
            planes: [Arc::new(vec![16; 64]), Arc::new(vec![128; 16]), Arc::new(vec![128; 16])],
            chroma: Chroma::C420,
            alpha: Some(Arc::new(vec![255; 10])),
        },
        ..VideoFrame::rgba8(8, 8, vec![0; 256])
    };
    assert_eq!(short.alpha_region(1), None);
}

#[test]
fn full_size_yuv8_tables_give_the_per_pixel_values_bit_for_bit() {
    // 640x480 takes the per-code-pair tables; single rows (below the size that builds them) take the
    // per-pixel loop: every value must match to the bit, for each subsampling, range, matrix and curve.
    let (w, h) = (640usize, 480usize);
    let mut seed = 0x2545_f491u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 24) as u8
    };
    let curve = filmcraft_color::DecodeTable::new(0.0, 1.0, 4096, |v| v.max(0.0).powf(2.4));
    for chroma in [Chroma::C420, Chroma::C422, Chroma::C444] {
        let (sx, sy) = chroma.shifts();
        let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        let y: Vec<u8> = (0..w * h).map(|_| next()).collect();
        let u: Vec<u8> = (0..cw * ch).map(|_| next()).collect();
        let v: Vec<u8> = (0..cw * ch).map(|_| next()).collect();
        for range in [Range::Limited, Range::Full] {
            for matrix in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
                let f = VideoFrame {
                    width: w as u32,
                    height: h as u32,
                    data: PixelData::Yuv8 { planes: [Arc::new(y.clone()), Arc::new(u.clone()), Arc::new(v.clone())], chroma, alpha: None },
                    color: ColorInfo { matrix, range, ..ColorInfo::REC709 },
                    par: (1, 1),
                    pts: Tick::ZERO,
                };
                for decode in [None, Some(&curve)] {
                    let (_, _, full) = f.to_linear_f32_decimated_with(1, decode);
                    for row in [0, 1, 2, 239, 478, 479] {
                        let (_, px) = f.to_linear_f32_region(1, decode, Region { x: 0, y: row, w, h: 1 });
                        let want: Vec<u32> = px.iter().map(|x| x.to_bits()).collect();
                        let got: Vec<u32> = full[row * w * 4..(row + 1) * w * 4].iter().map(|x| x.to_bits()).collect();
                        assert!(want == got, "{chroma:?} {range:?} {matrix:?} decode {} row {row}", decode.is_some());
                    }
                }
            }
        }
    }
}
