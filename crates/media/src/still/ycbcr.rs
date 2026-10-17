//! 8-bit chunky YCbCr TIFF (PhotometricInterpretation 6), uncompressed or PackBits.
//!
//! The `image` crate only decodes YCbCr when the compression is JPEG. ffmpeg writes
//! PackBits or uncompressed YCbCr for any YUV source, including 4:2:0 phone footage.

use super::Raster;
use crate::{MediaError, Result};

/// `Ok(None)` when `bytes` is not an 8-bit YCbCr TIFF this path owns.
pub(super) fn decode(name: &str, bytes: &[u8]) -> Result<Option<Raster>> {
    let Some(tiff) = Tiff::parse(bytes) else { return Ok(None) };
    if tiff.photometric != 6 {
        return Ok(None);
    }
    if tiff.bits != [8, 8, 8] || tiff.samples != 3 || tiff.planar != 1 {
        return Err(MediaError::Decode(format!("{name}: only 8-bit chunky YCbCr TIFF is supported")));
    }
    if tiff.compression != 1 && tiff.compression != 32773 {
        return Err(MediaError::Decode(format!("{name}: YCbCr TIFF compression {} is not supported", tiff.compression)));
    }
    let (w, h) = (tiff.width as usize, tiff.height as usize);
    let (sh, sv) = (tiff.sub_h as usize, tiff.sub_v as usize);
    if sh == 0 || sv == 0 || !matches!((sh, sv), (1, 1) | (2, 1) | (2, 2) | (1, 2) | (4, 1) | (4, 2) | (4, 4)) {
        return Err(MediaError::Decode(format!("{name}: YCbCr subsampling {}x{} is not supported", tiff.sub_h, tiff.sub_v)));
    }
    if w == 0 || h == 0 || w % sh != 0 || h % sv != 0 {
        return Err(MediaError::Decode(format!("{name}: YCbCr size {w}x{h} is not a multiple of the subsampling")));
    }
    let raw = match decompress(&tiff, bytes) {
        Some(v) => v,
        None => return Err(MediaError::Decode(format!("{name}: truncated YCbCr TIFF"))),
    };
    let block = sh * sv + 2;
    let blocks_x = w / sh;
    let expected = (h / sv) * blocks_x * block;
    if raw.len() < expected {
        return Err(MediaError::Decode(format!("{name}: YCbCr TIFF is shorter than its image")));
    }
    let (kr, kg, kb) = tiff.coefs;
    let mut rgba = vec![0u8; w * h * 4];
    let mut i = 0;
    for gy in 0..(h / sv) {
        for gx in 0..blocks_x {
            let y0 = gy * sv;
            let x0 = gx * sh;
            let mut ys = [0u8; 16];
            for k in 0..(sh * sv) {
                ys[k] = raw[i + k];
            }
            i += sh * sv;
            let cb = norm_chroma(raw[i], tiff.cb_black, tiff.cb_white);
            let cr = norm_chroma(raw[i + 1], tiff.cr_black, tiff.cr_white);
            i += 2;
            let (r_add, b_add) = (cr * (2.0 - 2.0 * kr), cb * (2.0 - 2.0 * kb));
            for row in 0..sv {
                for col in 0..sh {
                    let y = norm_luma(ys[row * sh + col], tiff.y_black, tiff.y_white);
                    let r = (y + r_add).clamp(0.0, 1.0);
                    let b = (y + b_add).clamp(0.0, 1.0);
                    let g = ((y - kr * r - kb * b) / kg).clamp(0.0, 1.0);
                    let px = ((y0 + row) * w + (x0 + col)) * 4;
                    rgba[px] = (r * 255.0).round() as u8;
                    rgba[px + 1] = (g * 255.0).round() as u8;
                    rgba[px + 2] = (b * 255.0).round() as u8;
                    rgba[px + 3] = 255;
                }
            }
        }
    }
    Ok(Some(Raster { width: tiff.width, height: tiff.height, rgba, has_alpha: false }))
}

struct Tiff {
    width: u32,
    height: u32,
    bits: [u16; 3],
    compression: u16,
    photometric: u16,
    samples: u16,
    planar: u16,
    sub_h: u16,
    sub_v: u16,
    rows_per_strip: u32,
    strips: Vec<(u64, u64)>,
    y_black: f64,
    y_white: f64,
    cb_black: f64,
    cb_white: f64,
    cr_black: f64,
    cr_white: f64,
    coefs: (f64, f64, f64),
}

impl Tiff {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        let le = match &bytes[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        if u16_at(bytes, 2, le)? != 42 {
            return None;
        }
        let mut ifd = u32_at(bytes, 4, le)? as usize;
        if ifd.saturating_add(2) > bytes.len() {
            return None;
        }
        let n = u16_at(bytes, ifd, le)? as usize;
        ifd += 2;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut bits = [8u16, 8, 8];
        let mut compression = 1u16;
        let mut photometric = 0u16;
        let mut samples = 1u16;
        let mut planar = 1u16;
        let mut sub_h = 2u16;
        let mut sub_v = 2u16;
        let mut rows_per_strip = u32::MAX;
        let mut offsets: Vec<u64> = Vec::new();
        let mut counts: Vec<u64> = Vec::new();
        let mut y_black = 0.0;
        let mut y_white = 255.0;
        let mut cb_black = 128.0;
        let mut cb_white = 255.0;
        let mut cr_black = 128.0;
        let mut cr_white = 255.0;
        let mut coefs = (0.299, 0.587, 0.114);
        for i in 0..n {
            let e = ifd + i * 12;
            if e + 12 > bytes.len() {
                return None;
            }
            let tag = u16_at(bytes, e, le)?;
            let typ = u16_at(bytes, e + 2, le)?;
            let cnt = u32_at(bytes, e + 4, le)? as usize;
            let val = u32_at(bytes, e + 8, le)?;
            match tag {
                256 => width = scalar(bytes, typ, cnt, val, le)?,
                257 => height = scalar(bytes, typ, cnt, val, le)?,
                258 => {
                    let v = shorts(bytes, typ, cnt, val, le)?;
                    if v.len() >= 3 {
                        bits = [v[0], v[1], v[2]];
                    }
                }
                259 => compression = scalar(bytes, typ, cnt, val, le)? as u16,
                262 => photometric = scalar(bytes, typ, cnt, val, le)? as u16,
                273 => offsets = longs(bytes, typ, cnt, val, le)?,
                277 => samples = scalar(bytes, typ, cnt, val, le)? as u16,
                278 => rows_per_strip = scalar(bytes, typ, cnt, val, le)?,
                279 => counts = longs(bytes, typ, cnt, val, le)?,
                284 => planar = scalar(bytes, typ, cnt, val, le)? as u16,
                529 => {
                    let r = rationals(bytes, typ, cnt, val, le)?;
                    if r.len() >= 3 && r[0] + r[1] + r[2] > 0.0 {
                        coefs = (r[0], r[1], r[2]);
                    }
                }
                530 => {
                    let v = shorts(bytes, typ, cnt, val, le)?;
                    if v.len() >= 2 && v[0] > 0 && v[1] > 0 {
                        sub_h = v[0];
                        sub_v = v[1];
                    }
                }
                532 => {
                    let r = rationals(bytes, typ, cnt, val, le)?;
                    if r.len() >= 6 && r[1] > r[0] && r[3] > r[2] && r[5] > r[4] {
                        y_black = r[0];
                        y_white = r[1];
                        cb_black = r[2];
                        cb_white = r[3];
                        cr_black = r[4];
                        cr_white = r[5];
                    }
                }
                _ => {}
            }
        }
        if offsets.len() != counts.len() || offsets.is_empty() {
            return None;
        }
        let strips = offsets.into_iter().zip(counts).collect();
        Some(Self {
            width,
            height,
            bits,
            compression,
            photometric,
            samples,
            planar,
            sub_h,
            sub_v,
            rows_per_strip,
            strips,
            y_black,
            y_white,
            cb_black,
            cb_white,
            cr_black,
            cr_white,
            coefs,
        })
    }
}

fn decompress(tiff: &Tiff, bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut remaining = tiff.height;
    for &(off, len) in &tiff.strips {
        let start = usize::try_from(off).ok()?;
        let len = usize::try_from(len).ok()?;
        let src = bytes.get(start..start + len)?;
        let rows = remaining.min(tiff.rows_per_strip.max(1));
        let n = samples_in_rows(tiff, rows)?;
        let piece = if tiff.compression == 32773 { unpack_packbits(src, n)? } else { src.get(..n)?.to_vec() };
        out.extend_from_slice(&piece);
        remaining = remaining.saturating_sub(rows);
        if remaining == 0 {
            break;
        }
    }
    Some(out)
}

fn samples_in_rows(tiff: &Tiff, rows: u32) -> Option<usize> {
    let sh = tiff.sub_h as usize;
    let sv = tiff.sub_v as usize;
    let rows = rows as usize;
    if rows % sv != 0 {
        return None;
    }
    let blocks_x = tiff.width as usize / sh;
    Some((rows / sv) * blocks_x * (sh * sv + 2))
}

fn unpack_packbits(src: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len);
    let mut i = 0;
    while out.len() < out_len {
        let n = *src.get(i)? as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            let take = count.min(out_len - out.len());
            out.extend_from_slice(src.get(i..i + take)?);
            i += count;
        } else if n != -128 {
            let count = (1 - i32::from(n)) as usize;
            let b = *src.get(i)?;
            i += 1;
            let take = count.min(out_len - out.len());
            out.extend(std::iter::repeat_n(b, take));
        }
    }
    Some(out)
}

fn norm_luma(code: u8, black: f64, white: f64) -> f64 {
    (f64::from(code) - black) / (white - black)
}

/// Reference black is the code for chroma 0 and reference white is the code for +0.5.
fn norm_chroma(code: u8, black: f64, white: f64) -> f64 {
    (f64::from(code) - black) / (2.0 * (white - black))
}

fn u16_at(b: &[u8], i: usize, le: bool) -> Option<u16> {
    let s = b.get(i..i + 2)?;
    Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
}

fn u32_at(b: &[u8], i: usize, le: bool) -> Option<u32> {
    let s = b.get(i..i + 4)?;
    Some(if le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
}

fn data_off(typ: u16, cnt: usize, val: u32) -> Option<usize> {
    let size = match typ {
        3 => 2,
        4 => 4,
        5 => 8,
        _ => return None,
    };
    if cnt.saturating_mul(size) <= 4 {
        Some(usize::MAX)
    } else {
        Some(val as usize)
    }
}

fn scalar(_bytes: &[u8], typ: u16, cnt: usize, val: u32, le: bool) -> Option<u32> {
    if cnt != 1 {
        return None;
    }
    match typ {
        3 => Some(u32::from(if le { val as u16 } else { (val >> 16) as u16 })),
        4 => Some(val),
        _ => None,
    }
}

fn shorts(bytes: &[u8], typ: u16, cnt: usize, val: u32, le: bool) -> Option<Vec<u16>> {
    if typ != 3 {
        return None;
    }
    if cnt == 1 {
        return Some(vec![if le { val as u16 } else { (val >> 16) as u16 }]);
    }
    if cnt == 2 {
        // `val` was decoded with the file endian already, so rebuild the raw 4 bytes.
        let raw = if le { val.to_le_bytes() } else { val.to_be_bytes() };
        return Some(vec![u16_at(&raw, 0, le)?, u16_at(&raw, 2, le)?]);
    }
    let off = val as usize;
    let mut v = Vec::with_capacity(cnt);
    for i in 0..cnt {
        v.push(u16_at(bytes, off + i * 2, le)?);
    }
    Some(v)
}

fn longs(bytes: &[u8], typ: u16, cnt: usize, val: u32, le: bool) -> Option<Vec<u64>> {
    if cnt == 0 {
        return None;
    }
    if cnt == 1 && (typ == 3 || typ == 4) {
        return Some(vec![u64::from(scalar(bytes, typ, 1, val, le)?)]);
    }
    if typ != 4 {
        return None;
    }
    let off = val as usize;
    let mut v = Vec::with_capacity(cnt);
    for i in 0..cnt {
        v.push(u64::from(u32_at(bytes, off + i * 4, le)?));
    }
    Some(v)
}

fn rationals(bytes: &[u8], typ: u16, cnt: usize, val: u32, le: bool) -> Option<Vec<f64>> {
    if typ != 5 {
        return None;
    }
    let off = if cnt == 0 { return None } else { data_off(typ, cnt, val)? };
    let base = if off == usize::MAX { return None } else { off };
    let mut v = Vec::with_capacity(cnt);
    for i in 0..cnt {
        let n = u32_at(bytes, base + i * 8, le)?;
        let d = u32_at(bytes, base + i * 8 + 4, le)?;
        if d == 0 {
            return None;
        }
        v.push(f64::from(n) / f64::from(d));
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::super::decode_raster;

    /// Studio-range red (Y=81, Cb=90, Cr=240), the limited-range triple ffmpeg writes for pure red.
    fn red_block(sub_h: u16, sub_v: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        // IFD starts after a 32-byte strip (4×2 4:4:4 is 24 bytes, 4:2:0 is 12).
        tiff.extend_from_slice(&8u32.to_le_bytes());
        let w = 4u32;
        let h = 2u32;
        let sh = sub_h as usize;
        let sv = sub_v as usize;
        let mut strip = Vec::new();
        for _ in 0..(h as usize / sv) * (w as usize / sh) {
            strip.extend(std::iter::repeat_n(81u8, sh * sv));
            strip.push(90);
            strip.push(240);
        }
        assert!(strip.len() <= 32);
        let strip_len = strip.len() as u32;
        tiff.extend_from_slice(&strip);
        tiff.resize(8 + 32, 0);
        let ifd = tiff.len() as u32;
        tiff[4..8].copy_from_slice(&ifd.to_le_bytes());
        // 10 tags
        tiff.extend_from_slice(&11u16.to_le_bytes());
        let tags: [(u16, u16, u32, u32); 11] = [
            (256, 4, 1, w),
            (257, 4, 1, h),
            (258, 3, 3, 0), // filled after we know the offset
            (259, 3, 1, 1),
            (262, 3, 1, 6),
            (273, 4, 1, 8),
            (277, 3, 1, 3),
            (278, 4, 1, h),
            (279, 4, 1, strip_len),
            (530, 3, 2, u32::from(sub_h) | (u32::from(sub_v) << 16)),
            (532, 5, 6, 0),
        ];
        let bits_at = tiff.len() + 11 * 12 + 4;
        let rbw_at = bits_at + 6;
        for (i, (tag, typ, cnt, val)) in tags.iter().enumerate() {
            let val = if *tag == 258 {
                bits_at as u32
            } else if *tag == 532 {
                rbw_at as u32
            } else {
                *val
            };
            let at = tiff.len();
            tiff.resize(at + 12, 0);
            tiff[at..at + 2].copy_from_slice(&tag.to_le_bytes());
            tiff[at + 2..at + 4].copy_from_slice(&typ.to_le_bytes());
            tiff[at + 4..at + 8].copy_from_slice(&cnt.to_le_bytes());
            tiff[at + 8..at + 12].copy_from_slice(&val.to_le_bytes());
            let _ = i;
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&8u16.to_le_bytes());
        tiff.extend_from_slice(&8u16.to_le_bytes());
        tiff.extend_from_slice(&8u16.to_le_bytes());
        // ReferenceBlackWhite for studio swing: Y 15..235, Cb/Cr 128..240 (ffmpeg's tag).
        for (n, d) in [(15u32, 1u32), (235, 1), (128, 1), (240, 1), (128, 1), (240, 1)] {
            tiff.extend_from_slice(&n.to_le_bytes());
            tiff.extend_from_slice(&d.to_le_bytes());
        }
        tiff
    }

    #[test]
    fn studio_red_ycbcr_444_and_420_decode_to_red() {
        for (h, v) in [(1u16, 1u16), (2, 2)] {
            let bytes = red_block(h, v);
            let r = decode_raster("red.tiff", &bytes).unwrap();
            assert_eq!((r.width, r.height), (4, 2));
            assert!(!r.has_alpha);
            for px in r.rgba.chunks(4) {
                assert!(px[0] >= 250, "{h}x{v} R {}", px[0]);
                assert!(px[1] <= 4, "{h}x{v} G {}", px[1]);
                assert!(px[2] <= 4, "{h}x{v} B {}", px[2]);
                assert_eq!(px[3], 255);
            }
        }
    }

    #[test]
    fn packbits_420_matches_the_uncompressed_block() {
        // The strip ffmpeg wrote for a 4×2 limited-range red, PackBits, subsampling 2×2.
        let packed = [0xfd, 0x51, 0x01, 0x5a, 0xf0, 0xfd, 0x51, 0x01, 0x5a, 0xf0];
        let plain = super::unpack_packbits(&packed, 12).unwrap();
        assert_eq!(plain, [0x51, 0x51, 0x51, 0x51, 0x5a, 0xf0, 0x51, 0x51, 0x51, 0x51, 0x5a, 0xf0]);
    }
}
