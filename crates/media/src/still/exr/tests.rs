use super::*;

/// Channel `(name, pixel type code)`.
type Chan = (&'static str, u32);

fn attr(h: &mut Vec<u8>, name: &str, ty: &str, value: &[u8]) {
    for s in [name, ty] {
        h.extend_from_slice(s.as_bytes());
        h.push(0);
    }
    h.extend_from_slice(&(value.len() as u32).to_le_bytes());
    h.extend_from_slice(value);
}

fn box2i(w: [i32; 4]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The test-side preprocessing for RLE / ZIP: interleave even and odd bytes, then byte deltas.
fn predict(raw: &[u8]) -> Vec<u8> {
    let mut t: Vec<u8> = raw.iter().step_by(2).chain(raw.iter().skip(1).step_by(2)).copied().collect();
    for i in (1..t.len()).rev() {
        t[i] = t[i].wrapping_sub(t[i - 1]).wrapping_add(128);
    }
    t
}

/// The test-side PXR24 packing of the `lines` of `raw` (plain layout): per line and channel, the
/// differences to the previous sample as byte planes, floats cut to their top 24 bits.
fn pxr24(raw: &[u8], chans: &[Chan], width: usize, lines: usize) -> Vec<u8> {
    let mut at = 0;
    let mut out = Vec::new();
    for _ in 0..lines {
        for (_, t) in chans {
            let size = if *t == 1 { 2 } else { 4 };
            let mut prev = 0u32;
            let mut planes = vec![Vec::new(); size];
            for x in 0..width {
                let b = &raw[at + x * size..at + (x + 1) * size];
                let v = match *t {
                    1 => u32::from(u16::from_le_bytes([b[0], b[1]])),
                    2 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> 8,
                    _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
                let d = v.wrapping_sub(prev);
                prev = v;
                let n = if *t == 2 { 3 } else { size };
                planes[0..n].iter_mut().enumerate().for_each(|(k, p)| p.push((d >> (8 * (n - 1 - k))) as u8));
            }
            at += width * size;
            out.extend(planes.concat());
        }
    }
    out
}

/// OpenEXR RLE: runs of 3+ equal bytes as runs, the rest as literals.
fn rle(t: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let run = t[i..].iter().take(128).take_while(|&&b| b == t[i]).count();
        if run >= 3 {
            out.push((run - 1) as u8);
            out.push(t[i]);
            i += run;
        } else {
            let mut j = i;
            while j < t.len() && j - i < 127 && !(j + 2 < t.len() && t[j] == t[j + 1] && t[j] == t[j + 2]) {
                j += 1;
            }
            let j = j.max(i + 1);
            out.push((-((j - i) as i32)) as u8);
            out.extend_from_slice(&t[i..j]);
            i = j;
        }
    }
    out
}

/// A zlib stream of stored blocks.
fn zlib_stored(t: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = t.chunks(65535).collect();
    for (k, b) in blocks.iter().enumerate() {
        out.push(u8::from(k + 1 == blocks.len()));
        out.extend_from_slice(&(b.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        out.extend_from_slice(b);
    }
    out.extend_from_slice(&[0; 4]); // Adler-32 (not checked)
    out
}

/// Encode one sample of type `ty` (0 uint, 1 half, 2 float).
fn put(out: &mut Vec<u8>, ty: u32, v: f32) {
    match ty {
        0 => out.extend_from_slice(&(v as u32).to_le_bytes()),
        1 => out.extend_from_slice(&f16(v).to_le_bytes()),
        _ => out.extend_from_slice(&v.to_le_bytes()),
    }
}

/// f32 → half for exactly representable test values.
fn f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    if v == 0.0 {
        return sign;
    }
    let exp = ((b >> 23) & 0xFF) as i32 - 127 + 15;
    sign | ((exp as u16) << 10) | ((b >> 13) & 0x3FF) as u16
}

/// An EXR file: `chans` (alphabetical, as EXR stores them), data window `data`, display window
/// `display`, compression `comp`; `value(channel index, x, y)` gives each sample.
fn exr_file(chans: &[Chan], data: [i32; 4], display: [i32; 4], comp: u8, value: impl Fn(usize, i32, i32) -> f32) -> Vec<u8> {
    let mut f = MAGIC.to_vec();
    f.extend_from_slice(&2u32.to_le_bytes());
    let mut cl = Vec::new();
    for (n, t) in chans {
        cl.extend_from_slice(n.as_bytes());
        cl.push(0);
        cl.extend_from_slice(&t.to_le_bytes());
        cl.extend_from_slice(&[0; 4]);
        cl.extend_from_slice(&1u32.to_le_bytes());
        cl.extend_from_slice(&1u32.to_le_bytes());
    }
    cl.push(0);
    attr(&mut f, "channels", "chlist", &cl);
    attr(&mut f, "compression", "compression", &[comp]);
    attr(&mut f, "dataWindow", "box2i", &box2i(data));
    attr(&mut f, "displayWindow", "box2i", &box2i(display));
    attr(&mut f, "lineOrder", "lineOrder", &[0]);
    attr(&mut f, "pixelAspectRatio", "float", &1f32.to_le_bytes());
    f.push(0);
    let lpc = if comp == 3 || comp == 5 { 16 } else { 1 };
    let ys: Vec<i32> = (data[1]..=data[3]).step_by(lpc).collect();
    let table = f.len();
    f.resize(table + ys.len() * 8, 0);
    for (i, &y0) in ys.iter().enumerate() {
        let mut raw = Vec::new();
        for y in y0..=(y0 + lpc as i32 - 1).min(data[3]) {
            for (c, (_, t)) in chans.iter().enumerate() {
                for x in data[0]..=data[2] {
                    put(&mut raw, *t, value(c, x, y));
                }
            }
        }
        let packed = match comp {
            0 => raw,
            1 => rle(&predict(&raw)),
            5 => {
                let lines = (data[3] - y0 + 1).min(16) as usize;
                zlib_stored(&pxr24(&raw, chans, (data[2] - data[0] + 1) as usize, lines))
            }
            _ => zlib_stored(&predict(&raw)),
        };
        let off = f.len() as u64;
        f[table + i * 8..table + i * 8 + 8].copy_from_slice(&off.to_le_bytes());
        f.extend_from_slice(&y0.to_le_bytes());
        f.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        f.extend_from_slice(&packed);
    }
    f
}

fn floats(d: &Decoded) -> &[f32] {
    match &d.frame.data {
        filmcraft_frame::PixelData::RgbaF32(p) => p,
        _ => panic!("EXR frames are float"),
    }
}

const RGBA_HALF: &[Chan] = &[("A", 1), ("B", 1), ("G", 1), ("R", 1)];

/// A recognisable half-exact pattern: channel c of pixel (x, y).
fn pattern(c: usize, x: i32, y: i32) -> f32 {
    match c {
        0 => [1.0, 0.5, 0.25, 0.0][((x + y) & 3) as usize],
        _ => (c as f32) * 0.125 + (x as f32) * 0.0625 + (y as f32) * 2.0,
    }
}

#[test]
fn half_floats() {
    assert_eq!(half(0x3C00), 1.0);
    assert_eq!(half(0x3800), 0.5);
    assert_eq!(half(0xC000), -2.0);
    assert_eq!(half(0x7BFF), 65504.0);
    assert_eq!(half(0x0001), 2f32.powi(-24));
    assert_eq!(half(0x03FF), 1023.0 * 2f32.powi(-24));
    assert_eq!(half(0x8000).to_bits(), (-0.0f32).to_bits());
    assert_eq!(half(0x7C00), f32::INFINITY);
    assert!(half(0x7E00).is_nan());
    for v in [0.0, 1.0, 0.5, 2.0, 0.0625, 1000.0] {
        assert_eq!(half(f16(v)), v);
    }
}

#[test]
fn rgba_half_every_compression() {
    let (w, h) = (7, 21); // two ZIP chunks, the second partial
    let want = exr_file(RGBA_HALF, [0, 0, w - 1, h - 1], [0, 0, w - 1, h - 1], 0, pattern);
    let base = decode("a.exr", &want).unwrap();
    assert_eq!((base.frame.width, base.frame.height), (7, 21));
    assert!(base.has_alpha);
    assert_eq!(base.pixel_format, "RGBA 16-bit float");
    assert_eq!(base.frame.color.transfer, Transfer::Linear);
    let px = floats(&base);
    // pixel (3, 2): R G B A
    let p = &px[(2 * 7 + 3) * 4..][..4];
    assert_eq!(p, &[pattern(3, 3, 2), pattern(2, 3, 2), pattern(1, 3, 2), pattern(0, 3, 2)]);
    for comp in [1, 2, 3, 5] {
        let f = exr_file(RGBA_HALF, [0, 0, w - 1, h - 1], [0, 0, w - 1, h - 1], comp, pattern);
        assert_ne!(f, want, "compression {comp} packs differently");
        let d = decode("a.exr", &f).unwrap_or_else(|e| panic!("compression {comp}: {e}"));
        assert_eq!(floats(&d), px, "compression {comp}");
    }
}

#[test]
fn pxr24_keeps_half_and_uint_exactly_and_floats_to_24_bits() {
    // 40 wide, 35 tall: three chunks, the last of 3 lines; one channel of every type
    let chans: &[Chan] = &[("B", 2), ("G", 1), ("R", 2), ("Z", 0)];
    let value = |c: usize, x: i32, y: i32| match c {
        0 => 1.0 / 3.0 + x as f32 * 0.37 - y as f32 * 1.5, // not exact in 24 bits
        1 => x as f32 * 0.0625 + y as f32,
        2 => 6000.7 - (x * y) as f32 * 1.25,
        _ => (x * 1000 + y) as f32,
    };
    let f = exr_file(chans, [0, 0, 39, 34], [0, 0, 39, 34], 5, value);
    let d = decode("p.exr", &f).unwrap();
    assert_eq!((d.frame.width, d.frame.height), (40, 35));
    assert_eq!(d.pixel_format, "RGB 32-bit float");
    let px = floats(&d);
    for (x, y) in [(0, 0), (1, 0), (39, 0), (0, 16), (7, 17), (39, 34), (13, 33)] {
        let p = &px[(y * 40 + x) * 4..][..4];
        let (x, y) = (x as i32, y as i32);
        let cut = |v: f32| f32::from_bits(v.to_bits() & 0xFFFF_FF00);
        assert_eq!(p, &[cut(value(2, x, y)), half(f16(value(1, x, y))), cut(value(0, x, y)), 1.0], "pixel ({x}, {y})");
    }
}

#[test]
fn damaged_pxr24_is_an_error() {
    let mut f = exr_file(RGBA_HALF, [0, 0, 9, 19], [0, 0, 9, 19], 5, pattern);
    assert!(decode("p.exr", &f).is_ok());
    // cut the file inside the last chunk
    f.truncate(f.len() - 20);
    assert!(decode("p.exr", &f).is_err());
    // a chunk whose deflate data is garbage
    let mut g = exr_file(RGBA_HALF, [0, 0, 9, 19], [0, 0, 9, 19], 5, pattern);
    let at = g.windows(2).rposition(|w| w == [0x78, 0x01]).unwrap();
    g[at] = 0;
    assert!(decode("p.exr", &g).is_err());
}

#[test]
fn luminance_only_is_grey_and_opaque() {
    let f = exr_file(&[("Y", 2)], [0, 0, 3, 1], [0, 0, 3, 1], 2, |_, x, y| x as f32 * 0.25 + y as f32 * 4.0);
    let d = decode("y.exr", &f).unwrap();
    assert!(!d.has_alpha);
    assert_eq!(d.pixel_format, "Y 32-bit float");
    assert_eq!(&floats(&d)[(4 + 2) * 4..][..4], &[4.5, 4.5, 4.5, 1.0]);
}

#[test]
fn rgb_without_alpha_and_extra_channels() {
    // an unknown channel ("Z", uint) is skipped; missing alpha is opaque
    let chans: &[Chan] = &[("B", 1), ("G", 1), ("R", 1), ("Z", 0)];
    let f = exr_file(chans, [0, 0, 1, 0], [0, 0, 1, 0], 1, |c, x, _| [0.25, 0.5, 2.0, 7.0][c] + x as f32);
    let d = decode("rgb.exr", &f).unwrap();
    assert!(!d.has_alpha);
    assert_eq!(d.pixel_format, "RGB 16-bit float");
    assert_eq!(floats(&d), &[2.0, 0.5, 0.25, 1.0, 3.0, 1.5, 1.25, 1.0]);
}

#[test]
fn data_window_is_placed_in_the_display_window() {
    // data window 2×1 at (1, 1) of a 4×3 display window starting at (-1, -1)... and one column outside
    let f = exr_file(&[("B", 2), ("G", 2), ("R", 2)], [2, 0, 3, 0], [-1, -1, 2, 1], 0, |_, x, _| x as f32);
    let d = decode("dw.exr", &f).unwrap();
    assert_eq!((d.frame.width, d.frame.height), (4, 3));
    assert!(d.has_alpha, "uncovered pixels are transparent");
    let px = floats(&d);
    // display (x, y) = data (x - 1, y - 1): data x 2 lands at display column 3; data x 3 is outside
    for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
        let want: &[f32] = if i == 4 + 3 { &[2.0, 2.0, 2.0, 1.0] } else { &[0.0; 4] };
        assert_eq!(&p[..], want, "pixel {i}");
    }
}

#[test]
fn still_source_reports_exr() {
    let f = exr_file(RGBA_HALF, [0, 0, 3, 3], [0, 0, 3, 3], 3, pattern);
    assert!(sniff(&f));
    let s = crate::open_bytes("shot.0001.exr", f.into(), &[]).unwrap();
    let v = s.info().video.clone().unwrap();
    assert_eq!(s.info().kind, MediaKind::Still);
    assert_eq!((v.width, v.height, v.codec.as_str(), v.has_alpha), (4, 4, "EXR", true));
    assert_eq!(v.pixel_format, "RGBA 16-bit float");
    let fr = s.video_frame(crate::FrameRequest::full(filmcraft_time::Tick(0))).unwrap();
    assert!(matches!(fr.data, filmcraft_frame::PixelData::RgbaF32(_)));
}

#[test]
fn unsupported_files_say_why() {
    let f = exr_file(RGBA_HALF, [0, 0, 1, 1], [0, 0, 1, 1], 0, pattern);
    let mut tiled = f.clone();
    tiled[5] |= 0x02;
    assert!(matches!(decode("t.exr", &tiled), Err(MediaError::Unsupported(m)) if m.contains("tiled")));
    let piz = exr_file(RGBA_HALF, [0, 0, 1, 1], [0, 0, 1, 1], 4, pattern);
    assert!(matches!(decode("p.exr", &piz), Err(MediaError::Unsupported(m)) if m.contains("PIZ")));
    let b44 = exr_file(RGBA_HALF, [0, 0, 1, 1], [0, 0, 1, 1], 6, pattern);
    assert!(matches!(decode("b.exr", &b44), Err(MediaError::Unsupported(m)) if m.contains("B44")));
    let dwab = exr_file(RGBA_HALF, [0, 0, 1, 1], [0, 0, 1, 1], 9, pattern);
    assert!(matches!(decode("d.exr", &dwab), Err(MediaError::Unsupported(m)) if m.contains("DWAB")));
    let ry = exr_file(&[("BY", 1), ("RY", 1), ("Y", 1)], [0, 0, 1, 1], [0, 0, 1, 1], 0, pattern);
    assert!(matches!(decode("c.exr", &ry), Err(MediaError::Unsupported(_))));
    let depth = exr_file(&[("Z", 2)], [0, 0, 1, 1], [0, 0, 1, 1], 0, pattern);
    assert!(matches!(decode("z.exr", &depth), Err(MediaError::Unsupported(m)) if m.contains("no R, G, B or Y")));
}

#[test]
fn hostile_files_are_errors_not_panics() {
    // windows too large to allocate, or inverted
    for data in [[0, 0, i32::MAX, i32::MAX], [i32::MIN, 0, i32::MAX, 0], [5, 5, 0, 0], [0, 0, 1 << 14, 1 << 14]] {
        let f = exr_file(&[("R", 1)], [0, 0, 0, 0], [0, 0, 0, 0], 0, pattern);
        // patch the data window in the header
        let at = f.windows(10).position(|w| w == b"dataWindow").unwrap() + 10 + 1 + 6 + 4;
        let mut g = f.clone();
        g[at..at + 16].copy_from_slice(&box2i(data));
        assert!(decode("h.exr", &g).is_err(), "{data:?}");
    }
    // every truncation and many single-byte corruptions of real files
    for comp in [0, 1, 2, 3] {
        let f = exr_file(RGBA_HALF, [0, 0, 5, 17], [0, 0, 5, 17], comp, pattern);
        for n in 0..f.len() {
            assert!(decode("h.exr", &f[..n]).is_err(), "compression {comp}, {n} bytes");
        }
        for n in 0..f.len() {
            for x in [0x01, 0x80, 0xFF] {
                let mut g = f.clone();
                g[n] ^= x;
                let _ = decode("h.exr", &g);
            }
        }
    }
}

/// A tiny header naming many channels on a very wide line used to size each chunk buffer at
/// hundreds of GB (`Vec::with_capacity` aborts the process); it is refused instead.
#[test]
fn huge_scan_lines_are_refused_before_allocating() {
    let names: Vec<String> = (0..MAX_CHANNELS - 1).map(|i| format!("A{i:04}")).collect();
    let mut chans: Vec<Chan> = names.iter().map(|n| (&*n.clone().leak(), 2)).collect();
    chans.push(("R", 2));
    let f = exr_file(&chans, [0, 0, 0, 0], [0, 0, 0, 0], 3, pattern);
    let at = f.windows(10).position(|w| w == b"dataWindow").unwrap() + 10 + 1 + 6 + 4;
    let mut g = f.clone();
    g[at..at + 16].copy_from_slice(&box2i([0, 0, (1 << 26) - 1, 0]));
    assert!(decode("h.exr", &g).is_err());
}
